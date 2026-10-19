//! Read-only desktop status and an explicit, destination-bound manual upload confirmation.
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::internal::{InternalConfig, TaskProgress};

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureNotice {
    pub code: String,
    pub at_ms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotStats {
    pub count: i64,
    pub bytes: i64,
    pub latest_at_ms: Option<i64>,
}

pub fn snapshot_stats(conn: &Connection) -> Result<SnapshotStats, String> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(file_size_bytes), 0), MAX(captured_at_ms) FROM snapshots",
        [],
        |r| Ok(SnapshotStats { count: r.get(0)?, bytes: r.get(1)?, latest_at_ms: r.get(2)? }),
    ).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UploadConfirmation {
    pub task_id: String,
    pub upload_url: String,
    pub encryption_recipient: String,
    pub domestic_server_confirmed: bool,
    pub cleanup_only: bool,
}

pub fn upload_missing(config: &InternalConfig, token_present: bool) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if config.upload_url.is_empty() { missing.push("server"); }
    if config.encryption_recipient.trim().is_empty() { missing.push("key"); }
    if !config.domestic_server_confirmed { missing.push("domestic"); }
    if !token_present { missing.push("credential"); }
    missing
}

pub fn validate_confirmation(
    expected: &UploadConfirmation,
    config: &InternalConfig,
    progress: &TaskProgress,
    token_present: bool,
) -> Result<(), String> {
    if expected.task_id != progress.task_id || expected.cleanup_only != progress.receipt.is_some() {
        return Err("任务状态已变化，请重新查看并确认".into());
    }
    // Once a persisted receipt exists, retrying only performs the existing cleanup path.
    if expected.cleanup_only { return Ok(()); }
    if expected.upload_url != config.upload_url
        || expected.encryption_recipient != config.encryption_recipient
        || expected.domestic_server_confirmed != config.domestic_server_confirmed
    {
        return Err("上传配置已变化，请重新查看目的地并确认".into());
    }
    config.validate()?;
    if !upload_missing(config, token_present).is_empty() {
        return Err("上传准备尚未完成，请先检查管理员配置；采集数据仍保留在本机".into());
    }
    Ok(())
}

pub fn token_present() -> bool {
    std::env::var("TIMELENS_UPLOAD_TOKEN").is_ok_and(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{internal::InternalControl, storage::migrations::run_migrations};
    use std::sync::{Arc, atomic::AtomicBool};

    #[test]
    fn stats_handle_empty_and_committed_rows_without_ocr() {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        let empty = snapshot_stats(&conn).unwrap();
        assert_eq!((empty.count, empty.bytes, empty.latest_at_ms), (0, 0, None));
        conn.execute("INSERT INTO window_sessions (id,app_name,window_title,start_ms,end_ms,duration_ms,is_active) VALUES ('s','demo','demo',1,2,1,0)", []).unwrap();
        conn.execute("INSERT INTO snapshots (id,session_id,file_path,captured_at_ms,file_size_bytes,trigger_type) VALUES ('a','s','a.webp',123,10,'poll'), ('b','s','b.webp',456,20,'poll')", []).unwrap();
        let stats = snapshot_stats(&conn).unwrap();
        assert_eq!((stats.count, stats.bytes, stats.latest_at_ms), (2, 30, Some(456)));
    }

    #[test]
    fn confirmation_rejects_stale_destination_task_and_missing_setup_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        let control = InternalControl::load(root.path(), Arc::new(AtomicBool::new(false))).unwrap();
        let progress = control.progress.lock().clone();
        let mut config = control.config.read().clone();
        config.upload_url = "https://internal.example/upload".into();
        config.encryption_recipient = age::x25519::Identity::generate().to_public().to_string();
        config.domestic_server_confirmed = true;
        let expected = UploadConfirmation { task_id: progress.task_id.clone(), upload_url: config.upload_url.clone(), encryption_recipient: config.encryption_recipient.clone(), domestic_server_confirmed: true, cleanup_only: false };
        assert!(validate_confirmation(&expected, &config, &progress, true).is_ok());
        assert!(validate_confirmation(&expected, &config, &progress, false).is_err());
        let mut changed = expected.clone(); changed.task_id = "stale".into();
        assert!(validate_confirmation(&changed, &config, &progress, true).is_err());
        let mut changed = config.clone(); changed.upload_url.push_str("/other");
        assert!(validate_confirmation(&expected, &changed, &progress, true).is_err());
        let mut changed = config.clone(); changed.encryption_recipient = age::x25519::Identity::generate().to_public().to_string();
        assert!(validate_confirmation(&expected, &changed, &progress, true).is_err());
        let mut changed = config.clone(); changed.domestic_server_confirmed = false;
        assert!(validate_confirmation(&expected, &changed, &progress, true).is_err());
        assert!(upload_missing(&InternalConfig::default(), false).len() == 4);
        assert_eq!(control.progress.lock().task_id, progress.task_id);
        assert!(!control.tracking.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn receipt_cleanup_does_not_require_upload_configuration_but_cannot_be_spoofed() {
        let root = tempfile::tempdir().unwrap();
        let control = InternalControl::load(root.path(), Arc::new(AtomicBool::new(false))).unwrap();
        let mut progress = control.progress.lock().clone();
        let config = InternalConfig::default();
        let expected = UploadConfirmation { task_id: progress.task_id.clone(), upload_url: String::new(), encryption_recipient: String::new(), domestic_server_confirmed: false, cleanup_only: true };
        assert!(validate_confirmation(&expected, &config, &progress, false).is_err());
        progress.receipt = Some(crate::core::transfer::UploadReceipt { task_id: progress.task_id.clone(), sha256: "verified".into(), size_bytes: 123, stored: true });
        assert!(validate_confirmation(&expected, &config, &progress, false).is_ok());
        let mut uploading = expected.clone(); uploading.cleanup_only = false;
        assert!(validate_confirmation(&uploading, &config, &progress, true).is_err());
    }

    #[test]
    fn capture_notices_ignore_old_jobs_and_clear_on_pause_and_resume() {
        use std::sync::atomic::Ordering;
        let root = tempfile::tempdir().unwrap();
        let control = InternalControl::load(root.path(), Arc::new(AtomicBool::new(false))).unwrap();
        control.note_capture(0, "privacy");
        assert!(control.capture_notice.lock().code.is_empty());
        control.start().unwrap();
        let epoch = control.epoch.load(Ordering::SeqCst);
        control.note_capture(epoch, "privacy");
        assert_eq!(control.capture_notice.lock().code, "privacy");
        control.pause(false).unwrap();
        control.note_capture(epoch, "queued");
        assert!(control.capture_notice.lock().code.is_empty());
        control.edit_progress(|p| { p.needs_flush = false; Ok(()) }).unwrap();
        control.start().unwrap();
        control.note_capture(epoch, "privacy");
        assert!(control.capture_notice.lock().code.is_empty());
    }
}
