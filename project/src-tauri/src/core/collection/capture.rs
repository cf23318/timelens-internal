use std::collections::HashMap;
use std::fs;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView};
use image_hasher::{HashAlg, HasherConfig};
use log::{info, warn};
use parking_lot::Mutex;
use rusqlite::Connection;
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::core::models::{
    CapturePriority, CaptureSignal, SnapshotPayload, SnapshotRow, WriteEvent,
};
use crate::core::ocr::OcrJob;
use crate::core::storage::DataPaths;
use crate::core::writer::WriterHandle;
use crate::core::{acquisition, browser, internal::InternalControl};

/// 定时 poll 截图：较小尺寸 + 略低质量，控制体积与哈希成本。
const MAX_WIDTH_POLL: u32 = 1600;
const WEBP_QUALITY_POLL: f32 = 85.0;
/// 切换窗口 / 手动 / 标题变化等：更高清晰度。
const MAX_WIDTH_IMPORTANT: u32 = u32::MAX;
const WEBP_QUALITY_IMPORTANT: f32 = 90.0;

fn capture_scale_and_quality(sig: &CaptureSignal) -> (u32, f32) {
    if sig.priority == CapturePriority::Low && sig.trigger_type == "poll" {
        (MAX_WIDTH_POLL, WEBP_QUALITY_POLL)
    } else {
        (MAX_WIDTH_IMPORTANT, WEBP_QUALITY_IMPORTANT)
    }
}

fn scale_to_max_width(img: DynamicImage, max_w: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w <= max_w {
        img
    } else {
        let nh = ((h as f64) * (max_w as f64 / w as f64)).round() as u32;
        img.resize_exact(max_w, nh, FilterType::Lanczos3)
    }
}

fn scale_poll(img: DynamicImage, max_w: u32) -> DynamicImage {
    let (width, height) = img.dimensions();
    if width > 0 && width < 1600 {
        let height = ((height as f64) * (1600.0 / width as f64)).round().max(1.0) as u32;
        img.resize_exact(1600, height, FilterType::Lanczos3)
    } else {
        scale_to_max_width(img, max_w)
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn periodic_images_meet_width_floor_and_cap_while_important_images_keep_native_size() {
        for (source_width, expected_width) in [(800, 1600), (1600, 1600), (3200, 1600)] {
            let result = scale_poll(DynamicImage::new_rgb8(source_width, source_width / 2), 1600);
            assert_eq!(result.dimensions(), (expected_width, expected_width / 2));
        }
        let result = scale_poll(DynamicImage::new_rgb8(3200, 1600), 2560);
        assert_eq!(result.dimensions(), (2560, 1280));
        let important = scale_to_max_width(DynamicImage::new_rgb8(3200, 1600), MAX_WIDTH_IMPORTANT);
        assert_eq!(important.dimensions(), (3200, 1600));
    }

    #[test]
    fn screenshot_writer_refuses_to_replace_existing_image() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("existing.webp");
        fs::write(&path, b"original image").unwrap();
        assert!(write_webp_lossy(&DynamicImage::new_rgb8(4, 4), &path, 85.0).is_err());
        assert_eq!(fs::read(path).unwrap(), b"original image");
    }
}

fn write_webp_lossy(
    scaled: &DynamicImage,
    path: &std::path::Path,
    quality: f32,
) -> Result<(), String> {
    let rgba = scaled.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return Err("empty image".into());
    }
    let enc = webp::Encoder::from_rgba(rgba.as_raw(), w, h);
    let mem = enc.encode(quality.clamp(0.0, 100.0));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut file = opts.open(path).map_err(|e| e.to_string())?;
    file.write_all(&mem)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

fn wait_session(conn: &Mutex<Connection>, sig: &CaptureSignal, id: &str) -> bool {
    for _ in 0..80 {
        let ok = conn.lock().query_row(
            "SELECT app_name,bundle_id,window_title,extracted_url,is_active FROM window_sessions WHERE id = ?1 LIMIT 1",
            [id],
            |r| Ok((r.get::<_, String>(0)?,r.get::<_, Option<String>>(1)?,r.get::<_, String>(2)?,r.get::<_, Option<String>>(3)?,r.get::<_, i64>(4)?)),
        );
        if let Ok((app, bundle, title, url, active)) = ok {
            let expected_title = if sig
                .expected_bundle
                .as_deref()
                .is_some_and(crate::core::privacy::should_redact_bundle)
            {
                "[redacted]".into()
            } else {
                crate::core::privacy::redact_title(&sig.expected_title)
            };
            return active == 1
                && app == sig.expected_app
                && bundle == sig.expected_bundle
                && title == expected_title
                && url == sig.expected_url;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

pub fn spawn_capture_thread(
    rx: crossbeam_channel::Receiver<CaptureSignal>,
    writer: WriterHandle,
    read_conn: Arc<Mutex<Connection>>,
    paths: DataPaths,
    is_afk: Arc<AtomicBool>,
    screen_ok: Arc<AtomicBool>,
    app: AppHandle,
    control: Arc<InternalControl>,
    ocr_hook: Option<(
        crossbeam_channel::Sender<OcrJob>,
        Arc<AtomicBool>,
        Arc<AtomicUsize>,
    )>,
) {
    let hasher = HasherConfig::new()
        .hash_alg(HashAlg::Mean)
        .hash_size(8, 8)
        .to_hasher();
    thread::spawn(move || {
        let mut last_phash: HashMap<String, (image_hasher::ImageHash, std::time::Instant)> =
            HashMap::new();
        while let Ok(sig) = rx.recv() {
            if !control.is_current(sig.epoch) {
                continue;
            }
            let Ok(front) = acquisition::sample_front_window() else {
                continue;
            };
            if front.app_name != sig.expected_app
                || front.bundle_id != sig.expected_bundle
                || front.window_title != sig.expected_title
            {
                continue;
            }
            let cfg = control.config.read().clone();
            let url = browser::url_for_front(&front);
            if url != sig.expected_url {
                continue;
            }
            if cfg.blocks_screenshot(
                &front.app_name,
                front.bundle_id.as_deref(),
                &front.window_title,
                url.as_deref(),
                browser::is_browser(&front),
            ) {
                continue;
            }
            if is_afk.load(Ordering::Relaxed) {
                warn!("capture skipped (afk): trigger={}", sig.trigger_type);
                continue;
            }
            if !screen_ok.load(Ordering::Relaxed) {
                warn!(
                    "capture skipped (screen recording not allowed / not refreshed): trigger={}",
                    sig.trigger_type
                );
                continue;
            }
            let Some(sid) = sig.session_id.clone() else {
                warn!(
                    "capture skipped (no active session_id): trigger={}",
                    sig.trigger_type
                );
                continue;
            };
            if !wait_session(read_conn.as_ref(), &sig, &sid) || !control.is_current(sig.epoch) {
                continue;
            }
            let captured = {
                let Some(_job) = control.begin_native(sig.epoch) else {
                    continue;
                };
                super::screens::grab(cfg.all_displays, &|| control.is_current(sig.epoch))
            };
            let images = match captured {
                Ok(i) => i,
                Err(e) => {
                    warn!("screen grab: {e}");
                    continue;
                }
            };
            if !control.is_current(sig.epoch) {
                continue;
            }
            // Drop an image if focus changed during the OS capture operation.
            if !acquisition::sample_front_window().is_ok_and(|now| {
                now.app_name == front.app_name
                    && now.bundle_id == front.bundle_id
                    && now.window_title == front.window_title
            }) {
                continue;
            }
            if browser::url_for_front(&front) != url {
                continue;
            }
            if !wait_session(read_conn.as_ref(), &sig, &sid) {
                continue;
            }
            let Some(_active) = control.active(sig.epoch) else {
                continue;
            };
            for (display, img) in images {
                let (mut max_w, mut webp_q) = capture_scale_and_quality(&sig);
                if sig.priority == CapturePriority::Low && sig.trigger_type == "poll" {
                    max_w = cfg.poll_width;
                    webp_q = cfg.poll_quality;
                }
                let scaled = if sig.priority == CapturePriority::Low && sig.trigger_type == "poll" {
                    scale_poll(img, max_w)
                } else {
                    scale_to_max_width(img, max_w)
                };
                let (sw, sh) = scaled.dimensions();
                if sig.priority == CapturePriority::Low && sig.trigger_type == "poll" {
                    let hnow = hasher.hash_image(&scaled);
                    let key = format!("{}:{display}", sid);
                    if let Some((prev, saved)) = last_phash.get(&key) {
                        if hnow.dist(prev) < 5 && saved.elapsed() < Duration::from_secs(60) {
                            continue;
                        }
                    }
                    last_phash.insert(key, (hnow, std::time::Instant::now()));
                    if last_phash.len() > 128 {
                        last_phash.retain(|_, (_, ts)| ts.elapsed() < Duration::from_secs(300));
                    }
                }
                let day = Utc::now().format("%Y-%m-%d").to_string();
                let shot_dir = paths.shots_dir.join(&day);
                if let Err(e) = fs::create_dir_all(&shot_dir) {
                    warn!("shots dir: {e}");
                    continue;
                }
                let id = Uuid::new_v4().to_string();
                let fname = format!(
                    "{}_{}_{}.webp",
                    id,
                    display,
                    sig.trigger_type.replace('/', "_")
                );
                let out_path = shot_dir.join(&fname);
                if let Err(e) = write_webp_lossy(&scaled, &out_path, webp_q) {
                    warn!("save webp: {e}");
                    continue;
                }
                let size = fs::metadata(&out_path).map(|m| m.len() as i64).unwrap_or(0);
                let fp = out_path.to_string_lossy().to_string();
                let h = hasher.hash_image(&scaled);
                let perceptual_hash = Some(format!("{h:?}"));
                let captured_at_ms = Utc::now().timestamp_millis();
                let row = SnapshotRow {
                    id: id.clone(),
                    session_id: sid.clone(),
                    file_path: fp.clone(),
                    captured_at_ms,
                    file_size_bytes: size,
                    trigger_type: sig.trigger_type.clone(),
                    resolution: Some(format!("{sw}x{sh}")),
                    format: "webp".into(),
                    perceptual_hash: perceptual_hash.clone(),
                };
                if writer.try_send(WriteEvent::Snapshot(row)).is_err() {
                    let _ = fs::remove_file(&out_path);
                    continue;
                }
                let path_for_ocr = fp.clone();
                let ocr_snapshot_id = id.clone();
                let ocr_session_id = sid.clone();
                let ocr_trigger = sig.trigger_type.clone();
                let ocr_phash = perceptual_hash.clone();
                let snap = crate::core::models::Snapshot {
                    id,
                    session_id: sid.clone(),
                    file_path: fp,
                    captured_at_ms,
                    file_size_bytes: size,
                    trigger_type: sig.trigger_type.clone(),
                    resolution: Some(format!("{sw}x{sh}")),
                    format: "webp".into(),
                    perceptual_hash,
                };
                let _ = app.emit("new_snapshot_saved", SnapshotPayload { snapshot: snap });
                if let Some((ref ocr_tx, ref ocr_on, ref pend)) = ocr_hook {
                    if ocr_on.load(Ordering::Relaxed) {
                        let job = OcrJob {
                            snapshot_id: ocr_snapshot_id,
                            session_id: ocr_session_id,
                            file_path: path_for_ocr,
                            captured_at_ms,
                            trigger_type: ocr_trigger,
                            perceptual_hash: ocr_phash,
                            epoch: sig.epoch,
                        };
                        pend.fetch_add(1, Ordering::Relaxed);
                        match ocr_tx.try_send(job) {
                            Ok(()) => {}
                            Err(e) => {
                                pend.fetch_sub(1, Ordering::Relaxed);
                                warn!("ocr queue full or closed, skip: {e}");
                            }
                        }
                    }
                }
                info!("snapshot saved {out_path:?}");
            }
        }
    });
}
