//! Encrypted export and the internal server upload contract. Never invoked by tests with user data.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use parking_lot::Mutex;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Emitter, Manager};

use super::internal::{InternalConfig, TaskProgress, TaskStatus};
use super::storage::DataPaths;

const MAX_BUNDLE_BYTES: u64 = 2_000_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadReceipt {
    pub task_id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub stored: bool,
}

impl UploadReceipt {
    fn verify(&self, task_id: &str, bundle: &Bundle) -> Result<(), String> {
        if !self.stored
            || self.task_id != task_id
            || self.sha256 != bundle.sha256
            || self.size_bytes != bundle.size_bytes
        {
            return Err("服务器回执与上传文件不符，未清理本地数据".into());
        }
        Ok(())
    }
}

pub struct Bundle {
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredBundle {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub recipient: String,
}

impl Bundle {
    fn stored(&self, recipient: &str) -> StoredBundle {
        StoredBundle {
            path: self.path.to_string_lossy().into_owned(),
            sha256: self.sha256.clone(),
            size_bytes: self.size_bytes,
            recipient: recipient.trim().into(),
        }
    }
}

fn reuse_bundle(
    paths: &DataPaths,
    record: &StoredBundle,
    config: &InternalConfig,
) -> Result<Bundle, String> {
    if record.recipient != config.encryption_recipient.trim() {
        return Err("待回收数据包的加密公钥与配置不符，请人工审核".into());
    }
    let path = PathBuf::from(&record.path);
    if fs::symlink_metadata(&path)
        .map_err(|_| "待回收数据包缺失，原始数据已保留，请人工核对服务器")?
        .file_type()
        .is_symlink()
    {
        return Err("待回收数据包不允许符号链接".into());
    }
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    if !path.starts_with(
        paths
            .exports_dir
            .canonicalize()
            .map_err(|e| e.to_string())?,
    ) || !path.is_file()
    {
        return Err("待回收数据包路径无效".into());
    }
    if fs::metadata(&path).map_err(|e| e.to_string())?.len() != record.size_bytes
        || sha256_file(&path)? != record.sha256
    {
        return Err("待回收数据包校验失败，原始数据已保留，请人工核对服务器".into());
    }
    Ok(Bundle {
        path,
        sha256: record.sha256.clone(),
        size_bytes: record.size_bytes,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShotEntry {
    snapshot_id: Option<String>,
    path: String,
    sha256: String,
    size_bytes: u64,
    #[serde(skip)]
    source: PathBuf,
}

fn collect_unindexed(
    dir: &Path,
    root: &Path,
    known: &std::collections::HashSet<PathBuf>,
    shots: &mut Vec<ShotEntry>,
) -> Result<(), String> {
    for item in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = item.map_err(|e| e.to_string())?.path();
        let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err("截图目录存在符号链接，已拒绝打包和自动清理".into());
        }
        if meta.is_dir() {
            collect_unindexed(&path, root, known, shots)?;
            continue;
        }
        if !meta.is_file() {
            return Err("截图目录存在非普通文件，需人工审核".into());
        }
        if path.file_name().is_some_and(|name| name == ".DS_Store") {
            continue;
        }
        let canonical = path.canonicalize().map_err(|e| e.to_string())?;
        if known.contains(&canonical) {
            continue;
        }
        if path
            .extension()
            .is_none_or(|ext| !ext.eq_ignore_ascii_case("webp"))
        {
            return Err("截图目录存在未识别文件，未自动清理，请人工审核".into());
        }
        let relative = canonical
            .strip_prefix(root)
            .map_err(|_| "截图路径超出任务目录")?;
        // Preserve original bytes even when a crash left a partial WebP; never invent a session/id.
        shots.push(ShotEntry {
            snapshot_id: None,
            path: format!("shots/{}", relative.to_string_lossy().replace('\\', "/")),
            sha256: sha256_file(&canonical)?,
            size_bytes: meta.len(),
            source: canonical,
        });
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn append_file<W: Write>(tar: &mut tar::Builder<W>, name: &str, path: &Path) -> Result<(), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut header = tar::Header::new_gnu();
    header.set_size(f.metadata().map_err(|e| e.to_string())?.len());
    header.set_mode(0o600);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
    tar.append_data(&mut header, name, &mut f)
        .map_err(|e| e.to_string())
}

pub fn build_bundle(
    conn: &Mutex<Connection>,
    paths: &DataPaths,
    progress: &TaskProgress,
    config: &InternalConfig,
) -> Result<Bundle, String> {
    let recipient: age::x25519::Recipient = config
        .encryption_recipient
        .trim()
        .parse()
        .map_err(|_| "需要有效的 age X25519 接收方公钥；未生成未加密的数据包")?;
    let scratch = tempfile::Builder::new()
        .prefix("package-")
        .tempdir_in(&paths.exports_dir)
        .map_err(|e| e.to_string())?;
    let db_path = scratch.path().join("db.sqlite");
    // SQLite online backup includes committed WAL pages; a raw copy of db.sqlite would not.
    conn.lock()
        .backup(rusqlite::MAIN_DB, &db_path, None)
        .map_err(|e| e.to_string())?;
    let db = Connection::open(&db_path).map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA journal_mode=DELETE; DELETE FROM settings;")
        .map_err(|e| e.to_string())?;
    let rows = {
        let mut s = db
            .prepare("SELECT id, file_path FROM snapshots ORDER BY captured_at_ms, id")
            .map_err(|e| e.to_string())?;
        let rows = s
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?
    };
    let shots_root = paths.shots_dir.canonicalize().map_err(|e| e.to_string())?;
    let mut shots = Vec::new();
    let mut known = std::collections::HashSet::new();
    for (id, source) in rows {
        let source = PathBuf::from(source);
        if source.as_os_str().is_empty()
            || fs::symlink_metadata(&source)
                .map_err(|_| "截图文件缺失，未生成不完整的数据包")?
                .file_type()
                .is_symlink()
        {
            return Err("截图路径无效或存在符号链接".into());
        }
        let canonical = source.canonicalize().map_err(|e| e.to_string())?;
        let relative = canonical
            .strip_prefix(&shots_root)
            .map_err(|_| "截图位于任务目录之外，已拒绝打包")?;
        if !canonical.is_file() {
            return Err("截图不是普通文件".into());
        }
        if !known.insert(canonical.clone()) {
            return Err("多个截图记录指向同一文件，请人工审核".into());
        }
        let archive_name = format!("shots/{}", relative.to_string_lossy().replace('\\', "/"));
        let size_bytes = fs::metadata(&canonical).map_err(|e| e.to_string())?.len();
        shots.push(ShotEntry {
            snapshot_id: Some(id.clone()),
            path: archive_name.clone(),
            sha256: sha256_file(&canonical)?,
            size_bytes,
            source: canonical,
        });
        db.execute(
            "UPDATE snapshots SET file_path=?1 WHERE id=?2",
            rusqlite::params![archive_name, id],
        )
        .map_err(|e| e.to_string())?;
    }
    collect_unindexed(&paths.shots_dir, &shots_root, &known, &mut shots)?;
    db.execute_batch("VACUUM;").map_err(|e| e.to_string())?;
    drop(db);
    let manifest = serde_json::json!({
        "formatVersion": 1, "taskId": progress.task_id, "elapsedMs": progress.elapsed_ms,
        "targetMs": progress.target_ms, "database": { "path": "db.sqlite", "sha256": sha256_file(&db_path)? },
        "screenshots": shots, "clipboardFlowIsInferred": true, "screenshotPixelsRedacted": false, "imageBytesValidated": false,
    });
    let manifest_path = scratch.path().join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let out = paths.exports_dir.join(format!(
        "{}-{}.tar.gz.age",
        progress.task_id,
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        let mut opts = OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&out).map_err(|e| e.to_string())?;
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
                .map_err(|e| e.to_string())?;
        let encrypted = encryptor.wrap_output(file).map_err(|e| e.to_string())?;
        let gzip = flate2::write::GzEncoder::new(encrypted, flate2::Compression::default());
        let mut tar = tar::Builder::new(gzip);
        append_file(&mut tar, "manifest.json", &manifest_path)?;
        append_file(&mut tar, "db.sqlite", &db_path)?;
        for shot in &shots {
            append_file(&mut tar, &shot.path, &shot.source)?;
        }
        let gzip = tar.into_inner().map_err(|e| e.to_string())?;
        let encrypted = gzip.finish().map_err(|e| e.to_string())?;
        let file = encrypted.finish().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let size_bytes = file.metadata().map_err(|e| e.to_string())?.len();
        if size_bytes >= MAX_BUNDLE_BYTES {
            return Err("加密包达到 2GB 上限，需要审核采集策略；本地原始数据已保留".into());
        }
        Ok(Bundle {
            sha256: sha256_file(&out)?,
            path: out.clone(),
            size_bytes,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&out);
    }
    result
}

fn upload(
    config: &InternalConfig,
    task_id: &str,
    bundle: &Bundle,
) -> Result<UploadReceipt, String> {
    config.validate()?;
    if config.upload_url.is_empty() || !config.domestic_server_confirmed {
        return Err("尚未配置并确认国内内部上传服务器，原始数据已保留".into());
    }
    let token =
        std::env::var("TIMELENS_UPLOAD_TOKEN").map_err(|_| "缺少上传凭证，原始数据已保留")?;
    if token.trim().is_empty() {
        return Err("上传凭证为空".into());
    }
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|_| "无法创建内部上传连接")?;
    upload_with_client(&client, config, task_id, bundle, &token)
}

fn upload_with_client(
    client: &reqwest::blocking::Client,
    config: &InternalConfig,
    task_id: &str,
    bundle: &Bundle,
    token: &str,
) -> Result<UploadReceipt, String> {
    config.validate()?;
    if !config.domestic_server_confirmed || config.upload_url.is_empty() {
        return Err("尚未确认内部上传服务器".into());
    }
    let file = File::open(&bundle.path).map_err(|e| e.to_string())?;
    let response = client
        .post(&config.upload_url)
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .header("Content-Length", bundle.size_bytes)
        .header("X-TimeLens-Task-Id", task_id)
        .header("X-TimeLens-SHA256", &bundle.sha256)
        .body(reqwest::blocking::Body::new(file))
        .send()
        .map_err(|_| "内部服务器上传失败，原始数据已保留")?;
    if !response.status().is_success() {
        return Err(format!(
            "内部服务器未确认接收（HTTP {}），原始数据已保留",
            response.status().as_u16()
        ));
    }
    let receipt: UploadReceipt = serde_json::from_reader(response.take(65_536))
        .map_err(|_| "服务器回执格式无效，原始数据已保留")?;
    receipt.verify(task_id, bundle)?;
    Ok(receipt)
}

fn cleanup(state: &crate::AppState) -> Result<(), String> {
    let _exclusive = state.0.internal.gate.write();
    let p = state.0.internal.progress.lock().clone();
    let receipt = p
        .receipt
        .as_ref()
        .ok_or("缺少已校验的服务器回执，拒绝清理")?;
    let record = p
        .stored_bundle
        .as_ref()
        .ok_or("缺少已上传数据包的校验记录，拒绝清理")?;
    receipt.verify(
        &p.task_id,
        &Bundle {
            path: PathBuf::from(&record.path),
            sha256: record.sha256.clone(),
            size_bytes: record.size_bytes,
        },
    )?;
    state.0.writer.erase_collected_data()?;
    let ocr_tmp = state.0.internal.ocr_temp_dir();
    for dir in [
        &state.0.paths.shots_dir,
        &state.0.paths.exports_dir,
        &ocr_tmp,
    ] {
        if dir.exists() {
            if fs::symlink_metadata(dir)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err("清理目录存在符号链接，已停止".into());
            }
            fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
        }
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn recover(state: &crate::AppState) -> Result<(), String> {
    let progress = state.0.internal.progress.lock().clone();
    if progress.receipt.is_none() {
        state.0.writer.flush()?;
        let cfg = state.0.internal.config.read().clone();
        if cfg.upload_url.is_empty() || !cfg.domestic_server_confirmed {
            return Err("尚未配置并确认国内内部上传服务器，原始数据已保留".into());
        }
        let bundle = if let Some(ref record) = progress.stored_bundle {
            reuse_bundle(&state.0.paths, record, &cfg)?
        } else {
            let bundle = build_bundle(&state.0.read_conn, &state.0.paths, &progress, &cfg)?;
            if let Err(e) = state.0.internal.edit_progress(|p| {
                p.bundle_path = Some(bundle.path.to_string_lossy().into_owned());
                p.stored_bundle = Some(bundle.stored(&cfg.encryption_recipient));
                Ok(())
            }) {
                let _ = fs::remove_file(&bundle.path);
                return Err(e);
            }
            bundle
        };
        let receipt = upload(&cfg, &progress.task_id, &bundle)?;
        // Save acknowledgement before cleanup so a crash can safely resume cleanup, without repackaging.
        state.0.internal.edit_progress(|p| {
            p.receipt = Some(receipt);
            Ok(())
        })?;
    }
    cleanup(state)?;
    state.0.internal.edit_progress(|p| {
        p.status = TaskStatus::Uploaded;
        p.last_error = None;
        p.bundle_path = None;
        p.stored_bundle = None;
        Ok(())
    })
}

pub fn begin_recovery(app: &tauri::AppHandle) -> Result<(), String> {
    let state = app.state::<crate::AppState>().inner().clone();
    let exclusive = state.0.internal.gate.write();
    state.0.internal.edit_progress(|p| {
        if p.status != TaskStatus::Completed {
            return Err("请先结束采集；回收过程中不能重复发起".into());
        }
        if p.needs_flush || p.integrity_error.is_some() {
            return Err("停止落盘尚未完成或数据完整性需要人工审核，未开始回收".into());
        }
        p.status = TaskStatus::Uploading;
        p.last_error = None;
        Ok(())
    })?;
    drop(exclusive);
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = recover(&state) {
            if state
                .0
                .internal
                .edit_progress(|p| {
                    p.status = TaskStatus::Completed;
                    p.last_error = Some(e.clone());
                    Ok(())
                })
                .is_err()
            {
                let mut p = state.0.internal.progress.lock();
                p.status = TaskStatus::Completed;
                p.last_error = Some(e);
            }
        }
        let _ = app.emit(
            "internal_task_changed",
            state.0.internal.progress.lock().clone(),
        );
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Clone, Copy)]
    enum TestResponse {
        Valid,
        WrongHash,
        Failure,
        Redirect,
        LostResponse,
    }

    /// Real TLS on loopback, using an ephemeral test-only certificate and synthetic capture bytes.
    fn tls_fixture(
        response: TestResponse,
        task: &str,
        expected: &Bundle,
    ) -> (
        String,
        reqwest::blocking::Client,
        std::thread::JoinHandle<()>,
    ) {
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                .unwrap();
        let client_cert = reqwest::Certificate::from_der(cert.der()).unwrap();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der());
        let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key.into())
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let task = task.to_owned();
        let sha = expected.sha256.clone();
        let size = expected.size_bytes;
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("test TLS accept failed: {e}"),
                }
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let connection = rustls::ServerConnection::new(std::sync::Arc::new(config)).unwrap();
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut received = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                received.extend_from_slice(&chunk[..n]);
                if let Some(i) = received.windows(4).position(|p| p == b"\r\n\r\n") {
                    break i + 4;
                }
                assert!(received.len() < 32_768);
            };
            let headers = String::from_utf8(received[..header_end].to_vec())
                .unwrap()
                .to_ascii_lowercase();
            assert!(headers.starts_with("post /upload http/1.1\r\n"));
            assert!(headers.contains("authorization: bearer fixture-only-token\r\n"));
            assert!(headers.contains(&format!("x-timelens-task-id: {}\r\n", task.to_lowercase())));
            assert!(headers.contains(&format!("x-timelens-sha256: {sha}\r\n")));
            let mut body = received[header_end..].to_vec();
            while body.len() < size as usize {
                let limit = (size as usize - body.len()).min(chunk.len());
                let n = stream.read(&mut chunk[..limit]).unwrap();
                assert!(n > 0);
                body.extend_from_slice(&chunk[..n]);
            }
            assert_eq!(body.len() as u64, size);
            assert_eq!(format!("{:x}", Sha256::digest(&body)), sha);
            if matches!(response, TestResponse::LostResponse) {
                return;
            }
            let body = serde_json::to_vec(&UploadReceipt {
                task_id: task,
                sha256: if matches!(response, TestResponse::WrongHash) {
                    "invalid".into()
                } else {
                    sha
                },
                size_bytes: size,
                stored: true,
            })
            .unwrap();
            let (status, extra) = match response {
                TestResponse::Failure => ("500 Internal Server Error", ""),
                TestResponse::Redirect => (
                    "307 Temporary Redirect",
                    "Location: https://example.invalid/upload\r\n",
                ),
                _ => ("200 OK", ""),
            };
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(&body).unwrap();
            stream.flush().unwrap();
        });
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .add_root_certificate(client_cert)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        (format!("https://{address}/upload"), client, handle)
    }

    #[test]
    fn https_upload_requires_matching_receipt_and_preserves_raw_data_on_failures() {
        let (_dir, paths, db, progress, mut config, _key) = fixture();
        let bundle = build_bundle(&db, &paths, &progress, &config).unwrap();
        config.domestic_server_confirmed = true;
        for response in [
            TestResponse::Valid,
            TestResponse::WrongHash,
            TestResponse::Failure,
            TestResponse::Redirect,
            TestResponse::LostResponse,
        ] {
            let (url, client, server) = tls_fixture(response, &progress.task_id, &bundle);
            config.upload_url = url;
            let result = upload_with_client(
                &client,
                &config,
                &progress.task_id,
                &bundle,
                "fixture-only-token",
            );
            assert_eq!(result.is_ok(), matches!(response, TestResponse::Valid));
            server.join().unwrap();
            assert!(paths.shots_dir.join("fixture.webp").exists());
            assert_eq!(
                db.lock()
                    .query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }
    #[test]
    fn retry_reuses_same_ciphertext_and_detects_local_corruption() {
        let (_dir, paths, db, progress, config, _key) = fixture();
        let first = build_bundle(&db, &paths, &progress, &config).unwrap();
        let record = first.stored(&config.encryption_recipient);
        let second = reuse_bundle(&paths, &record, &config).unwrap();
        assert_eq!(first.path.canonicalize().unwrap(), second.path);
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(fs::read_dir(&paths.exports_dir).unwrap().count(), 1);
        fs::write(&first.path, b"damaged ciphertext").unwrap();
        assert!(reuse_bundle(&paths, &record, &config).is_err());
        assert!(paths.shots_dir.join("fixture.webp").exists());
    }

    #[test]
    fn archive_preserves_unindexed_partial_images_and_rejects_unknown_files() {
        let (_dir, paths, db, progress, config, key) = fixture();
        fs::write(
            paths.shots_dir.join("unindexed.webp"),
            b"partial image bytes",
        )
        .unwrap();
        let bundle = build_bundle(&db, &paths, &progress, &config).unwrap();
        let reader = age::Decryptor::new(File::open(bundle.path).unwrap())
            .unwrap()
            .decrypt(std::iter::once(&key as &dyn age::Identity))
            .unwrap();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(reader));
        let mut found = false;
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().as_ref() == Path::new("manifest.json") {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                found = manifest["screenshots"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["snapshotId"].is_null() && s["path"] == "shots/unindexed.webp");
            }
        }
        assert!(found);
        fs::write(paths.shots_dir.join("user-note.txt"), b"not capture data").unwrap();
        assert!(build_bundle(&db, &paths, &progress, &config).is_err());
        assert!(paths.shots_dir.join("user-note.txt").exists());
    }
    fn fixture() -> (
        tempfile::TempDir,
        DataPaths,
        Mutex<Connection>,
        TaskProgress,
        InternalConfig,
        age::x25519::Identity,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let paths = DataPaths {
            root: dir.path().to_owned(),
            data_dir: dir.path().join("data"),
            db_path: dir.path().join("data/db.sqlite"),
            shots_dir: dir.path().join("data/shots"),
            exports_dir: dir.path().join("exports"),
        };
        paths.ensure_dirs().unwrap();
        let db = super::super::storage::db::open_write(&paths.db_path).unwrap();
        db.execute("INSERT INTO window_sessions(id,start_ms,end_ms,duration_ms,app_name,window_title) VALUES('s',1,2,1,'Excel','fixture')", []).unwrap();
        let shot = paths.shots_dir.join("fixture.webp");
        fs::write(&shot, b"synthetic screenshot fixture, no screen capture").unwrap();
        db.execute("INSERT INTO snapshots(id,session_id,file_path,captured_at_ms,file_size_bytes,trigger_type) VALUES('shot','s',?1,1,47,'poll')", [shot.to_str().unwrap()]).unwrap();
        db.execute("INSERT INTO settings(key,value,updated_at) VALUES('test_credential','fixture-secret',1)", []).unwrap();
        let progress = TaskProgress {
            task_id: uuid::Uuid::new_v4().to_string(),
            elapsed_ms: 1000,
            target_ms: 1000,
            status: TaskStatus::Completed,
            last_error: None,
            bundle_path: None,
            receipt: None,
            stored_bundle: None,
            integrity_error: None,
            needs_flush: false,
        };
        let key = age::x25519::Identity::generate();
        let mut config = InternalConfig::default();
        config.encryption_recipient = key.to_public().to_string();
        (dir, paths, Mutex::new(db), progress, config, key)
    }

    #[test]
    fn encrypted_archive_roundtrip_includes_wal_and_relative_screenshot_paths() {
        let (_dir, paths, db, progress, config, key) = fixture();
        let bundle = build_bundle(&db, &paths, &progress, &config).unwrap();
        let wrong_key = age::x25519::Identity::generate();
        assert!(age::Decryptor::new(File::open(&bundle.path).unwrap())
            .unwrap()
            .decrypt(std::iter::once(&wrong_key as &dyn age::Identity))
            .is_err());
        let decrypted = age::Decryptor::new(File::open(&bundle.path).unwrap())
            .unwrap()
            .decrypt(std::iter::once(&key as &dyn age::Identity))
            .unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(decrypted));
        let mut files = std::collections::HashMap::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            files.insert(path, bytes);
        }
        assert_eq!(files.len(), 3);
        assert_eq!(
            &files["shots/fixture.webp"],
            b"synthetic screenshot fixture, no screen capture"
        );
        let extracted_path = paths.root.join("extracted.sqlite");
        fs::write(&extracted_path, &files["db.sqlite"]).unwrap();
        let restored = Connection::open(&extracted_path).unwrap();
        assert_eq!(
            restored
                .query_row("SELECT COUNT(*) FROM window_sessions", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            restored
                .query_row("SELECT COUNT(*) FROM settings", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            restored
                .query_row("SELECT file_path FROM snapshots", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "shots/fixture.webp"
        );
        let manifest: serde_json::Value = serde_json::from_slice(&files["manifest.json"]).unwrap();
        assert_eq!(
            manifest["database"]["sha256"],
            sha256_file(&extracted_path).unwrap()
        );
        assert!(!String::from_utf8(files["manifest.json"].clone())
            .unwrap()
            .contains(paths.root.to_str().unwrap()));
        // Building an archive must leave the source settings and captures intact.
        assert_eq!(
            db.lock()
                .query_row(
                    "SELECT COUNT(*) FROM settings WHERE key='test_credential'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            1
        );
        assert!(paths.shots_dir.join("fixture.webp").exists());
    }

    #[test]
    fn missing_screenshot_prevents_incomplete_archive_without_changing_source_database() {
        let (_dir, paths, db, progress, config, _key) = fixture();
        fs::remove_file(paths.shots_dir.join("fixture.webp")).unwrap();
        assert!(build_bundle(&db, &paths, &progress, &config).is_err());
        assert_eq!(
            db.lock()
                .query_row("SELECT COUNT(*) FROM snapshots", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(fs::read_dir(&paths.exports_dir).unwrap().count(), 0);
    }

    #[test]
    fn acknowledgement_must_match_complete_ciphertext() {
        let bundle = Bundle {
            path: PathBuf::new(),
            sha256: "abc".into(),
            size_bytes: 123,
        };
        let mut r = UploadReceipt {
            task_id: "task".into(),
            sha256: "abc".into(),
            size_bytes: 123,
            stored: true,
        };
        assert!(r.verify("task", &bundle).is_ok());
        r.size_bytes = 122;
        assert!(r.verify("task", &bundle).is_err());
        r.size_bytes = 123;
        r.task_id = "other-task".into();
        assert!(r.verify("task", &bundle).is_err());
        r.task_id = "task".into();
        r.sha256 = "different".into();
        assert!(r.verify("task", &bundle).is_err());
        r.sha256 = "abc".into();
        r.stored = false;
        assert!(r.verify("task", &bundle).is_err());
    }
}
