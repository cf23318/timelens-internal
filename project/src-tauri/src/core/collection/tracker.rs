use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
    Arc, RwLock,
};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use crc32fast::Hasher as Crc32;
use log::warn;
use serde_json::json;
use std::hash::{Hash, Hasher};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

use crate::core::acquisition::{self, FrontWindowState};
use crate::core::models::{
    AggregationCmd, AppSwitch, AppSwitchRow, CapturePriority, CaptureSignal, NotificationRow,
    RawEventRow, WriteEvent,
};
use crate::core::privacy;
use crate::core::settings::app_name_blacklisted;
use crate::core::writer::WriterHandle;
use crate::core::{browser, internal::InternalControl};

const AFK_SECS: f64 = 300.0;
const POLL: Duration = Duration::from_secs(2);

fn sampling_interrupted(monotonic_gap: Duration, previous_ms: i64, now_ms: i64) -> bool {
    let wall_gap = now_ms.saturating_sub(previous_ms);
    monotonic_gap > Duration::from_secs(5) || wall_gap > 5000 || wall_gap < 0
}

#[cfg(test)]
mod gap_tests {
    use super::*;
    #[test]
    fn detects_sleep_even_if_monotonic_clock_stops_and_handles_clock_changes() {
        assert!(!sampling_interrupted(Duration::from_secs(2), 1000, 3000));
        assert!(sampling_interrupted(
            Duration::from_secs(2),
            1000,
            28_801_000
        ));
        assert!(sampling_interrupted(Duration::from_secs(6), 1000, 3000));
        assert!(sampling_interrupted(Duration::from_secs(2), 3000, 1000));
    }
}

fn compute_hash(app: &str, bundle: &Option<String>, title: &str, fs: bool) -> i64 {
    let mut h = Crc32::new();
    format!(
        "{}|{}|{}|{}",
        app,
        bundle.as_deref().unwrap_or(""),
        title,
        fs as u8
    )
    .hash(&mut h);
    h.finish() as u32 as i64
}

fn confirmed_session(tx: &Sender<AggregationCmd>) -> Option<String> {
    let (done, rx) = std::sync::mpsc::channel();
    tx.send(AggregationCmd::SessionReady(done)).ok()?;
    rx.recv_timeout(Duration::from_secs(2)).ok().flatten()
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_tracker_thread(
    app: AppHandle,
    agg_tx: Sender<AggregationCmd>,
    capture_tx: crossbeam_channel::Sender<CaptureSignal>,
    writer: WriterHandle,
    tracking: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    is_afk: Arc<AtomicBool>,
    current_session: Arc<RwLock<Option<String>>>,
    notification_heuristic_enabled: Arc<AtomicBool>,
    app_blacklist: Arc<RwLock<Vec<String>>>,
    control: Arc<InternalControl>,
) {
    thread::spawn(move || {
        let mut last_hash: Option<i64> = None;
        let mut last_app = String::new();
        let mut last_bundle: Option<String> = None;
        let mut last_title = String::new();
        let mut last_session_start_ms: i64 = 0;
        let mut local_afk = false;
        let mut in_blackout = false;
        let mut last_low_cap = std::time::Instant::now()
            .checked_sub(Duration::from_secs(15))
            .unwrap_or_else(std::time::Instant::now);
        let mut last_epoch = u64::MAX;
        let mut last_url: Option<String> = None;
        let mut last_poll = std::time::Instant::now();
        let mut last_poll_wall = Utc::now().timestamp_millis();

        while running.load(Ordering::Relaxed) {
            thread::sleep(POLL);
            let previous_poll_wall = last_poll_wall;
            let now_ms = Utc::now().timestamp_millis();
            let interrupted = sampling_interrupted(last_poll.elapsed(), previous_poll_wall, now_ms);
            last_poll = std::time::Instant::now();
            last_poll_wall = now_ms;
            if !tracking.load(Ordering::Relaxed) {
                continue;
            }
            let epoch = control.epoch.load(Ordering::SeqCst);
            let Some(_active) = control.active(epoch) else {
                continue;
            };
            if epoch != last_epoch {
                last_epoch = epoch;
                last_hash = None;
                last_app.clear();
                last_bundle = None;
                last_title.clear();
                last_url = None;
                last_session_start_ms = 0;
                local_afk = false;
                in_blackout = false;
                is_afk.store(false, Ordering::Relaxed);
            }
            if interrupted {
                // Suspend or a stalled native call must not turn an unobserved gap into app time.
                let _ = agg_tx.send(AggregationCmd::EnterRecordingBlackout {
                    timestamp_ms: previous_poll_wall,
                });
                last_hash = None;
                last_app.clear();
                last_bundle = None;
                last_title.clear();
                last_url = None;
                last_session_start_ms = 0;
                local_afk = false;
                in_blackout = false;
            }
            let idle = acquisition::idle_seconds();
            if idle >= AFK_SECS {
                if !local_afk {
                    local_afk = true;
                    is_afk.store(true, Ordering::Relaxed);
                    let ts = Utc::now().timestamp_millis();
                    let _ = agg_tx.send(AggregationCmd::EnterAfk {
                        timestamp_ms: ts,
                        idle_seconds: idle,
                    });
                    let id = Uuid::new_v4().to_string();
                    let _ = writer.try_send(WriteEvent::RawEvent(RawEventRow {
                        id,
                        timestamp_ms: ts,
                        app_name: "system".into(),
                        bundle_id: None,
                        window_title: "afk".into(),
                        extracted_url: None,
                        extracted_file_path: None,
                        idle_seconds: idle,
                        is_fullscreen: 0,
                        is_audio_playing: 0,
                        state_hash: 0,
                        trigger_type: "afk_enter".into(),
                        created_at: ts,
                    }));
                }
                continue;
            }
            if local_afk {
                local_afk = false;
                is_afk.store(false, Ordering::Relaxed);
                let ts = Utc::now().timestamp_millis();
                let front = match acquisition::sample_front_window() {
                    Ok(f) => f,
                    Err(_) => FrontWindowState {
                        app_name: "Unknown".into(),
                        bundle_id: None,
                        window_title: String::new(),
                        is_fullscreen: false,
                    },
                };
                let list = app_blacklist
                    .read()
                    .ok()
                    .map(|g| g.clone())
                    .unwrap_or_default();
                if app_name_blacklisted(&front.app_name, &list) {
                    local_afk = false;
                    is_afk.store(false, Ordering::Relaxed);
                    let ts = Utc::now().timestamp_millis();
                    let _ =
                        agg_tx.send(AggregationCmd::EnterRecordingBlackout { timestamp_ms: ts });
                    let _ = app.emit(
                        "afk_state_changed",
                        json!({ "isAfk": false, "idleSeconds": 0.0 }),
                    );
                    in_blackout = true;
                    last_hash = None;
                    last_app.clear();
                    last_bundle = None;
                    last_title.clear();
                    last_session_start_ms = 0;
                    continue;
                }
                in_blackout = false;
                let (title_url, ep) = privacy::extract_url_and_path(&front.window_title);
                let eu = if browser::is_browser(&front) {
                    browser::current_url(&front)
                } else {
                    title_url.and_then(|s| browser::sanitized_url(&s))
                };
                let title = if front
                    .bundle_id
                    .as_deref()
                    .is_some_and(privacy::should_redact_bundle)
                {
                    "[redacted]".into()
                } else {
                    privacy::redact_title(&front.window_title)
                };
                let _ = agg_tx.send(AggregationCmd::ExitAfk {
                    timestamp_ms: ts,
                    app_name: front.app_name.clone(),
                    bundle_id: front.bundle_id.clone(),
                    window_title: title.clone(),
                    extracted_url: eu.clone(),
                    extracted_file_path: ep.clone(),
                    state_hash: compute_hash(
                        &front.app_name,
                        &front.bundle_id,
                        &front.window_title,
                        front.is_fullscreen,
                    ),
                });
                let id = Uuid::new_v4().to_string();
                let sh = compute_hash(
                    &front.app_name,
                    &front.bundle_id,
                    &front.window_title,
                    front.is_fullscreen,
                );
                let _ = writer.try_send(WriteEvent::RawEvent(RawEventRow {
                    id,
                    timestamp_ms: ts,
                    app_name: front.app_name.clone(),
                    bundle_id: front.bundle_id.clone(),
                    window_title: privacy::redact_title(&front.window_title),
                    extracted_url: eu.clone(),
                    extracted_file_path: ep,
                    idle_seconds: idle,
                    is_fullscreen: if front.is_fullscreen { 1 } else { 0 },
                    is_audio_playing: 0,
                    state_hash: sh,
                    trigger_type: "afk_exit".into(),
                    created_at: ts,
                }));
                last_hash = Some(sh);
                last_app = front.app_name.clone();
                last_bundle = front.bundle_id.clone();
                last_title = front.window_title.clone();
                last_session_start_ms = ts;
                let _ = capture_tx.try_send(CaptureSignal {
                    epoch,
                    expected_app: front.app_name.clone(),
                    expected_title: front.window_title.clone(),
                    expected_bundle: front.bundle_id.clone(),
                    expected_url: eu.clone(),
                    priority: CapturePriority::High,
                    session_id: confirmed_session(&agg_tx),
                    trigger_type: "afk_exit".into(),
                });
                continue;
            }

            let front = match acquisition::sample_front_window() {
                Ok(f) => f,
                Err(e) => {
                    warn!("front window: {e}");
                    continue;
                }
            };
            let list = app_blacklist
                .read()
                .ok()
                .map(|g| g.clone())
                .unwrap_or_default();
            let ts_blk = Utc::now().timestamp_millis();
            if app_name_blacklisted(&front.app_name, &list) {
                if !in_blackout {
                    in_blackout = true;
                    let _ = agg_tx.send(AggregationCmd::EnterRecordingBlackout {
                        timestamp_ms: ts_blk,
                    });
                    last_hash = None;
                    last_app.clear();
                    last_bundle = None;
                    last_title.clear();
                    last_session_start_ms = 0;
                }
                continue;
            }
            if in_blackout {
                in_blackout = false;
            }
            let mut title = front.window_title.clone();
            if let Some(ref b) = front.bundle_id {
                if privacy::should_redact_bundle(b) {
                    title = "[redacted]".into();
                }
            }
            title = privacy::redact_title(&title);
            let (title_url, ex_path) = privacy::extract_url_and_path(&front.window_title);
            let ex_url = if browser::is_browser(&front) {
                browser::current_url(&front)
            } else {
                title_url.and_then(|s| browser::sanitized_url(&s))
            };
            // Browser provider calls can take time; never associate an address with a different front app.
            if !acquisition::sample_front_window().is_ok_and(|now| {
                now.app_name == front.app_name
                    && now.bundle_id == front.bundle_id
                    && now.window_title == front.window_title
            }) {
                continue;
            }
            let ex_path = ex_path.map(|p| privacy::redact_path(&p));
            let hash = compute_hash(
                &front.app_name,
                &front.bundle_id,
                &title,
                front.is_fullscreen,
            );
            let ts = Utc::now().timestamp_millis();
            let trigger = if last_url != ex_url {
                "window_change"
            } else {
                match last_hash {
                    None => "window_change",
                    Some(lh) if lh != hash => {
                        if front.app_name != last_app
                            || front.bundle_id.as_ref() != last_bundle.as_ref()
                        {
                            "window_change"
                        } else {
                            "window_change"
                        }
                    }
                    Some(_) => "poll",
                }
            };
            last_url = ex_url.clone();

            if trigger == "window_change"
                && (front.app_name != last_app || front.bundle_id != last_bundle)
            {
                let dur = (ts - last_session_start_ms).max(0);
                let short_bounce = notification_heuristic_enabled.load(Ordering::Relaxed)
                    && !last_app.is_empty()
                    && front.app_name != last_app
                    && dur > 0
                    && dur < 2_500;
                let switch_type: String = if short_bounce {
                    "notification".into()
                } else {
                    "voluntary".into()
                };
                let sw_id = Uuid::new_v4().to_string();
                let sw = AppSwitchRow {
                    id: sw_id.clone(),
                    timestamp_ms: ts,
                    from_app: last_app.clone(),
                    from_bundle_id: last_bundle.clone(),
                    from_window_title: Some(privacy::redact_title(&last_title)),
                    to_app: front.app_name.clone(),
                    to_bundle_id: front.bundle_id.clone(),
                    to_window_title: Some(title.clone()),
                    from_session_duration_ms: dur,
                    switch_type: switch_type.clone(),
                };
                if !last_app.is_empty() {
                    let ev = AppSwitch {
                        id: sw_id.clone(),
                        timestamp_ms: ts,
                        from_app: sw.from_app.clone(),
                        from_bundle_id: sw.from_bundle_id.clone(),
                        from_window_title: sw.from_window_title.clone(),
                        to_app: sw.to_app.clone(),
                        to_bundle_id: sw.to_bundle_id.clone(),
                        to_window_title: sw.to_window_title.clone(),
                        from_session_duration_ms: sw.from_session_duration_ms,
                        switch_type: sw.switch_type.clone(),
                    };
                    let _ = writer.try_send(WriteEvent::AppSwitch(sw));
                    let _ = app.emit("app_switch_recorded", ev);
                    if short_bounce {
                        let nid = Uuid::new_v4().to_string();
                        let _ = writer.try_send(WriteEvent::Notification(NotificationRow {
                            id: nid,
                            timestamp_ms: ts,
                            source_app: front.app_name.clone(),
                            source_bundle_id: front.bundle_id.clone(),
                            current_foreground_app: Some(last_app.clone()),
                            user_responded: 0,
                            response_delay_ms: None,
                            caused_switch: 1,
                        }));
                    }
                }
            }

            let _ = agg_tx.send(AggregationCmd::Tick {
                timestamp_ms: ts,
                app_name: front.app_name.clone(),
                bundle_id: front.bundle_id.clone(),
                window_title: title.clone(),
                extracted_url: ex_url.clone(),
                extracted_file_path: ex_path.clone(),
                trigger_type: trigger.to_string(),
                state_hash: hash,
            });

            let confirmed_sid = confirmed_session(&agg_tx);

            let id = Uuid::new_v4().to_string();
            let _ = writer.try_send(WriteEvent::RawEvent(RawEventRow {
                id,
                timestamp_ms: ts,
                app_name: front.app_name.clone(),
                bundle_id: front.bundle_id.clone(),
                window_title: title.clone(),
                extracted_url: ex_url.clone(),
                extracted_file_path: ex_path,
                idle_seconds: idle,
                is_fullscreen: if front.is_fullscreen { 1 } else { 0 },
                is_audio_playing: 0,
                state_hash: hash,
                trigger_type: trigger.to_string(),
                created_at: ts,
            }));

            if trigger == "window_change" {
                last_session_start_ms = ts;
            }

            if trigger == "window_change" || trigger == "title_change" {
                let sid = confirmed_sid.clone();
                let _ = capture_tx.try_send(CaptureSignal {
                    epoch,
                    expected_app: front.app_name.clone(),
                    expected_title: front.window_title.clone(),
                    expected_bundle: front.bundle_id.clone(),
                    expected_url: ex_url.clone(),
                    priority: CapturePriority::High,
                    session_id: sid,
                    trigger_type: trigger.to_string(),
                });
            } else if last_low_cap.elapsed()
                >= Duration::from_secs(control.config.read().capture_interval_secs)
            {
                last_low_cap = std::time::Instant::now();
                let sid = confirmed_sid;
                let _ = capture_tx.try_send(CaptureSignal {
                    epoch,
                    expected_app: front.app_name.clone(),
                    expected_title: front.window_title.clone(),
                    expected_bundle: front.bundle_id.clone(),
                    expected_url: ex_url.clone(),
                    priority: CapturePriority::Low,
                    session_id: sid,
                    trigger_type: "poll".into(),
                });
            }

            last_hash = Some(hash);
            last_app = front.app_name;
            last_bundle = front.bundle_id;
            last_title = front.window_title;
        }
    });
}
