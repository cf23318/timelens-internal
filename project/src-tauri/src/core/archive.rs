//! Explicitly enrolled task retention. Existing profiles are never adopted for deletion.
use super::{
    internal::{InternalConfig, TaskProgress},
    storage::DataPaths,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path, sync::atomic::Ordering, time::Duration};
pub const RETENTION_MS: i64 = 30 * 86_400_000;
static STATE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveBinding {
    #[serde(default = "receiver_mode")]
    pub upload_mode: String,
    pub task_id: String,
    pub upload_url: String,
    pub survey_id: String,
    pub device_id: String,
    pub planned_end_ms: i64,
    pub effective_end_ms: i64,
    pub deadline_ms: i64,
    pub reconciled: bool,
    pub deleted: bool,
    #[serde(default)]
    pub end_synced: bool,
    #[serde(default)]
    pub target_ms: Option<u64>,
}
fn receiver_mode() -> String {
    "receiver".into()
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let m = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if m.file_type().is_symlink() || !m.is_file() || m.len() > 16384 {
        return Err("归档状态文件无效".into());
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|_| "归档状态损坏，需管理员核对".into())
}
pub fn end_ms(config: &InternalConfig) -> Result<i64, String> {
    let value = chrono::DateTime::parse_from_rfc3339(&config.archive_end_at)
        .map_err(|_| "请配置含时区的批准调研结束时间")?
        .timestamp_millis();
    if !(1..=4_102_444_800_000).contains(&value) {
        return Err("调研结束时间超出范围".into());
    }
    Ok(value)
}
pub fn load(root: &Path) -> Result<Option<ArchiveBinding>, String> {
    let p = root.join("archive-binding.json");
    match fs::symlink_metadata(&p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(_) => {}
    }
    let b: ArchiveBinding = read(&p)?;
    uuid::Uuid::parse_str(&b.task_id).map_err(|_| "归档任务编号无效")?;
    if b.effective_end_ms > b.planned_end_ms
        || b.effective_end_ms <= 0
        || b.deadline_ms
            != b.effective_end_ms
                .checked_add(RETENTION_MS)
                .ok_or("删除截止溢出")?
    {
        return Err("归档删除截止无效".into());
    }
    if b.target_ms
        .is_some_and(|target| target == 0 || target > 48 * 3_600_000)
    {
        return Err("归档采集目标无效".into());
    }
    Ok(Some(b))
}
fn save(root: &Path, b: &ArchiveBinding) -> Result<(), String> {
    super::internal::write_json_atomic(&root.join("archive-binding.json"), b)
}
/// Establish ownership before any collector starts. Existing data without a proof stays local.
pub fn initialize_owner(
    paths: &DataPaths,
    conn: &rusqlite::Connection,
    task: &str,
) -> Result<(), String> {
    let owner = paths.root.join("archive-profile-owner.json");
    if owner.exists() {
        let _: String = read(&owner)?;
        return Ok(());
    }
    let tables=conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT IN ('settings','schema_migrations') AND name NOT LIKE 'snapshot_ocr_fts_%'").map_err(|e|e.to_string())?
        .query_map([],|r|r.get::<_,String>(0)).map_err(|e|e.to_string())?.collect::<rusqlite::Result<Vec<_>>>().map_err(|e|e.to_string())?;
    for table in tables {
        let filter = if table == "intent_mapping" {
            " WHERE is_builtin=0"
        } else {
            ""
        };
        let n: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM \"{}\"{}",
                    table.replace('"', "\"\""),
                    filter
                ),
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if n > 0 {
            return Ok(());
        }
    }
    for dir in [&paths.shots_dir, &paths.exports_dir] {
        if fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
        {
            return Ok(());
        }
    }
    for name in ["pending-snapshots", "recovery-quarantine", "tmp"] {
        let p = paths.data_dir.join(name);
        if p.exists() && fs::read_dir(p).map_err(|e| e.to_string())?.next().is_some() {
            return Ok(());
        }
    }
    super::internal::write_json_atomic(&owner, &task)
}
pub fn owner_matches(paths: &DataPaths, task: &str) -> bool {
    owned(paths, task).is_ok()
}
fn owned(paths: &DataPaths, task: &str) -> Result<(), String> {
    let owner: String = read(&paths.root.join("archive-profile-owner.json")).map_err(|_| {
        "此数据目录含旧记录或归属不明；请管理员使用全新调研 profile，不自动绑定或删除旧数据"
    })?;
    if owner != task {
        return Err("此 profile 不属于当前归档任务".into());
    }
    Ok(())
}
pub fn bind(
    paths: &DataPaths,
    cfg: &InternalConfig,
    p: &TaskProgress,
) -> Result<ArchiveBinding, String> {
    let _state = STATE_LOCK.lock();
    owned(paths, &p.task_id)?;
    let end = end_ms(cfg)?;
    if let Some(b) = load(&paths.root)? {
        if b.upload_mode != cfg.upload_mode
            || b.task_id != p.task_id
            || b.upload_url != cfg.upload_url
            || b.survey_id != cfg.upload_survey_id
            || b.device_id != cfg.upload_device_id
            || b.planned_end_ms != end
        {
            return Err("任务已经绑定其他归档配置，不能更换目的地或延后截止".into());
        }
        ensure_live(
            &paths.root,
            &p.task_id,
            chrono::Utc::now().timestamp_millis(),
        )?;
        return Ok(b);
    }
    let effective = end.min(p.ended_at_ms.unwrap_or(end));
    let b = ArchiveBinding {
        upload_mode: cfg.upload_mode.clone(),
        task_id: p.task_id.clone(),
        upload_url: cfg.upload_url.clone(),
        survey_id: cfg.upload_survey_id.clone(),
        device_id: cfg.upload_device_id.clone(),
        planned_end_ms: end,
        effective_end_ms: effective,
        deadline_ms: effective + RETENTION_MS,
        reconciled: false,
        deleted: false,
        end_synced: false,
        target_ms: Some(p.target_ms),
    };
    if chrono::Utc::now().timestamp_millis() >= b.deadline_ms {
        return Err("调研原始记录已经到删除截止，不能启用上传".into());
    }
    save(&paths.root, &b)?;
    Ok(b)
}
pub fn ensure_live(root: &Path, task: &str, now: i64) -> Result<(), String> {
    if let Some(b) = load(root)? {
        if b.task_id != task || b.deleted || now >= b.deadline_ms {
            return Err("归档任务已到删除截止，不能继续采集、上传或读取原图".into());
        }
    }
    Ok(())
}
pub fn shorten(root: &Path, p: &TaskProgress) -> Result<(), String> {
    let _state = STATE_LOCK.lock();
    if let Some(mut b) = load(root)? {
        if b.task_id != p.task_id {
            return Err("归档任务不匹配".into());
        }
        if b.deleted {
            return Ok(());
        }
        if let Some(end) = p.ended_at_ms {
            if end < b.effective_end_ms {
                b.effective_end_ms = end;
                b.deadline_ms = end + RETENTION_MS;
                b.reconciled = false;
                b.end_synced = false;
                save(root, &b)?
            }
        }
    }
    Ok(())
}
pub fn mark_reconciled(
    root: &Path,
    task: &str,
    server_end: i64,
    server_deadline: i64,
) -> Result<(), String> {
    let _state = STATE_LOCK.lock();
    let mut b = load(root)?.ok_or("归档未绑定")?;
    if b.deleted
        || b.task_id != task
        || server_end > b.effective_end_ms
        || server_deadline != server_end + RETENTION_MS
    {
        return Err("服务器归档截止不匹配".into());
    }
    b.effective_end_ms = server_end;
    b.deadline_ms = server_deadline;
    b.reconciled = true;
    b.end_synced = true;
    save(root, &b)
}
pub fn accept_end(root: &Path, task: &str, end: i64, deadline: i64) -> Result<(), String> {
    let _state = STATE_LOCK.lock();
    let mut b = load(root)?.ok_or("归档未绑定")?;
    if b.deleted || b.task_id != task || end > b.effective_end_ms || deadline != end + RETENTION_MS
    {
        return Err("归档结束回执不匹配".into());
    }
    b.effective_end_ms = end;
    b.deadline_ms = deadline;
    b.end_synced = true;
    save(root, &b)
}
fn remove_tree(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
        Ok(m) if m.file_type().is_symlink() => Err("受管清理目录为链接，已停止并保留告警".into()),
        Ok(m) if m.is_dir() => fs::remove_dir_all(path).map_err(|e| e.to_string()),
        _ => Err("受管清理目录类型无效".into()),
    }
}
/// Caller must stop producers and drain the upload worker before calling this.
pub fn purge_owned(
    paths: &DataPaths,
    writer: &super::writer::WriterHandle,
    task: &str,
    now: i64,
) -> Result<bool, String> {
    let _state = STATE_LOCK.lock();
    let Some(mut b) = load(&paths.root)? else {
        return Ok(false);
    };
    owned(paths, task)?;
    if b.task_id != task {
        return Err("清理任务不匹配".into());
    }
    if now < b.deadline_ms {
        return Ok(false);
    }
    paths.ensure_dirs().map_err(|e| e.to_string())?;
    // Validate fixed roots before deleting anything; do not use paths from screenshot rows.
    for dir in [
        &paths.root,
        &paths.data_dir,
        &paths.shots_dir,
        &paths.exports_dir,
    ] {
        if fs::symlink_metadata(dir)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("数据目录链接不允许清理".into());
        }
    }
    for name in ["shots", "pending-snapshots", "recovery-quarantine", "tmp"] {
        remove_tree(&paths.data_dir.join(name))?
    }
    let manifest_cache = paths.data_dir.join("oss-manifest-state.json");
    match fs::symlink_metadata(&manifest_cache) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
            fs::remove_file(manifest_cache).map_err(|e| e.to_string())?
        }
        Ok(_) => return Err("清单缓存类型无效".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    remove_tree(&paths.exports_dir)?;
    writer.erase_expired_data()?;
    paths.ensure_dirs().map_err(|e| e.to_string())?;
    b.deleted = true;
    b.reconciled = false;
    save(&paths.root, &b)?;
    Ok(true)
}
pub fn spawn(app: tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    std::thread::spawn(move || {
        let mut metadata_retry = std::time::Instant::now() - Duration::from_secs(31);
        loop {
            let Some(state) = app.try_state::<crate::AppState>() else {
                break;
            };
            if !state.0.running.load(Ordering::SeqCst) {
                break;
            }
            let result = (|| -> Result<(), String> {
                let p = state.0.internal.progress.lock().clone();
                shorten(&state.0.paths.root, &p)?;
                let Some(b) = load(&state.0.paths.root)? else {
                    return Ok(());
                };
                if b.deleted {
                    return Ok(());
                }
                let now = chrono::Utc::now().timestamp_millis();
                if now < b.deadline_ms {
                    if b.upload_mode == "receiver"
                        && !b.end_synced
                        && p.ended_at_ms.is_some()
                        && !p.needs_flush
                        && !state.0.internal.frame_upload_running.load(Ordering::SeqCst)
                        && metadata_retry.elapsed() >= Duration::from_secs(30)
                        && now + 30_000 < b.deadline_ms
                    {
                        metadata_retry = std::time::Instant::now();
                        super::frame_upload::sync_end_metadata(
                            &state.0.paths,
                            &state.0.internal.config.read(),
                            &p,
                        )?;
                    }
                    if b.upload_mode == "ossDirect"
                        && !b.end_synced
                        && p.ended_at_ms.is_some()
                        && !p.needs_flush
                        && !state.0.internal.frame_upload_running.load(Ordering::SeqCst)
                        && metadata_retry.elapsed() >= Duration::from_secs(30)
                        && now + 65_000 < b.deadline_ms
                    {
                        metadata_retry = std::time::Instant::now();
                        super::oss_manifest::sync_end(
                            &state.0.paths,
                            &state.0.internal.config.read(),
                            &p,
                        )?;
                    }
                    return Ok(());
                }
                super::frame_upload::stop(&state.0.internal);
                super::internal::pause_for_expiry(&app, &state)?;
                if state.0.internal.frame_upload_running.load(Ordering::SeqCst) {
                    return Err("到期清理等待上传请求退出，将自动重试".into());
                }
                let _gate = state.0.internal.gate.write();
                if state.0.ocr_pending.load(Ordering::SeqCst) > 0 {
                    return Err("到期清理等待文字识别退出，将自动重试".into());
                }
                purge_owned(
                    &state.0.paths,
                    &state.0.writer,
                    &p.task_id,
                    chrono::Utc::now().timestamp_millis(),
                )?;
                state.0.internal.edit_progress(|p| {
                    p.status = super::internal::TaskStatus::Completed;
                    p.last_error = None;
                    p.needs_flush = false;
                    Ok(())
                })?;
                let _ = app.emit(
                    "internal_task_changed",
                    state.0.internal.progress.lock().clone(),
                );
                Ok(())
            })();
            if let Err(e) = result {
                let _ = state.0.internal.edit_progress(|p| {
                    p.last_error = Some(e);
                    Ok(())
                });
            }
            drop(state);
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        internal::{write_json_atomic, InternalControl},
        storage::db,
        writer,
    };
    use std::sync::{atomic::AtomicBool, Arc};
    fn fixture() -> (
        tempfile::TempDir,
        DataPaths,
        rusqlite::Connection,
        Arc<InternalControl>,
        writer::WriterHandle,
    ) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let paths = DataPaths {
            root: root.clone(),
            data_dir: root.join("data"),
            shots_dir: root.join("data/shots"),
            db_path: root.join("data/db.sqlite"),
            exports_dir: root.join("exports"),
        };
        paths.ensure_dirs().unwrap();
        let conn = db::open_write(&paths.db_path).unwrap();
        let control = InternalControl::load(&root, Arc::new(AtomicBool::new(false))).unwrap();
        let writer = writer::spawn_writer_thread(
            db::open_write(&paths.db_path).unwrap(),
            Arc::new(Default::default()),
        );
        (t, paths, conn, control, writer)
    }
    fn config() -> InternalConfig {
        let mut c = InternalConfig::default();
        c.upload_url = "https://fixture.internal".into();
        c.upload_survey_id = "study".into();
        c.upload_device_id = "device".into();
        c.archive_end_at = "2099-12-31T17:00:00Z".into();
        c
    }
    fn insert_raw(conn: &rusqlite::Connection) {
        conn.execute("INSERT INTO window_sessions(id,app_name,window_title,start_ms,end_ms,duration_ms,is_active) VALUES('s','fixture','synthetic',1,2,1,0)",[]).unwrap();
    }
    #[test]
    fn legacy_profile_is_not_adopted_and_capture_policy_stays_local() {
        let (_t, p, c, control, _w) = fixture();
        insert_raw(&c);
        let task = control.progress.lock().task_id.clone();
        initialize_owner(&p, &c, &task).unwrap();
        assert!(!owner_matches(&p, &task));
        assert!(bind(&p, &config(), &control.progress.lock()).is_err());
        crate::core::internal::fill_test_participant_name(&control);
        assert!(control.start().is_ok());
        assert_eq!(control.progress.lock().target_ms, 48 * 3_600_000);
        assert!(!control.config.read().all_displays);
        assert!(!p.root.join("archive-binding.json").exists());
    }
    #[test]
    fn zero_duration_enrolled_task_keeps_target_and_retention_deadline() {
        let (_t, p, c, control, _w) = fixture();
        let mut cfg = config();
        cfg.target_hours = 16;
        control.save_config(cfg.clone()).unwrap();
        let task = control.progress.lock().task_id.clone();
        initialize_owner(&p, &c, &task).unwrap();
        let binding = bind(&p, &cfg, &control.progress.lock()).unwrap();
        let mut changed = cfg.clone();
        changed.target_hours = 48;
        assert!(control.save_config(changed).is_err());
        drop(control);
        let restored = InternalControl::load(&p.root, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(restored.progress.lock().target_ms, 16 * 3_600_000);
        assert_eq!(restored.config.read().target_hours, 16);
        let after = load(&p.root).unwrap().unwrap();
        assert_eq!(after.effective_end_ms, binding.effective_end_ms);
        assert_eq!(after.deadline_ms, binding.deadline_ms);
    }

    #[test]
    fn expiry_boundary_cleans_owned_rows_files_and_wal_but_keeps_settings() {
        let (_t, p, c, control, w) = fixture();
        let task = control.progress.lock().task_id.clone();
        initialize_owner(&p, &c, &task).unwrap();
        insert_raw(&c);
        fs::write(p.shots_dir.join("synthetic.webp"), b"fixture").unwrap();
        fs::write(p.exports_dir.join("synthetic.json"), b"fixture").unwrap();
        c.execute(
            "INSERT OR REPLACE INTO settings(key,value,updated_at) VALUES('language','zh-CN',1)",
            [],
        )
        .unwrap();
        control
            .edit_progress(|p| {
                p.ended_at_ms = Some(chrono::Utc::now().timestamp_millis());
                Ok(())
            })
            .unwrap();
        let b = bind(&p, &config(), &control.progress.lock()).unwrap();
        assert!(!purge_owned(&p, &w, &task, b.deadline_ms - 1).unwrap());
        assert!(p.shots_dir.join("synthetic.webp").exists());
        assert!(purge_owned(&p, &w, &task, b.deadline_ms).unwrap());
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM window_sessions", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            c.query_row("SELECT value FROM settings WHERE key='language'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "zh-CN"
        );
        assert_eq!(
            c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(!p.shots_dir.join("synthetic.webp").exists());
        assert!(load(&p.root).unwrap().unwrap().deleted);
        assert!(purge_owned(&p, &w, &task, b.deadline_ms + 1).unwrap());
        assert!(control.start().is_err());
    }
    #[test]
    fn binding_and_completion_never_extend_deadline() {
        let (_t, p, c, control, _w) = fixture();
        let task = control.progress.lock().task_id.clone();
        initialize_owner(&p, &c, &task).unwrap();
        let b = bind(&p, &config(), &control.progress.lock()).unwrap();
        control
            .edit_progress(|p| {
                p.ended_at_ms = Some(chrono::Utc::now().timestamp_millis());
                Ok(())
            })
            .unwrap();
        shorten(&p.root, &control.progress.lock()).unwrap();
        let early = load(&p.root).unwrap().unwrap();
        assert!(early.deadline_ms < b.deadline_ms);
        control
            .edit_progress(|p| {
                p.ended_at_ms = Some(early.effective_end_ms + 1000);
                Ok(())
            })
            .unwrap();
        shorten(&p.root, &control.progress.lock()).unwrap();
        assert_eq!(
            load(&p.root).unwrap().unwrap().deadline_ms,
            early.deadline_ms
        );
        assert!(mark_reconciled(
            &p.root,
            &task,
            early.effective_end_ms + 1,
            early.deadline_ms + 1
        )
        .is_err());
        let mut changed = config();
        changed.upload_device_id = "other".into();
        assert!(bind(&p, &changed, &control.progress.lock()).is_err());
    }
    #[test]
    fn corrupt_state_and_wrong_owner_do_not_delete_data() {
        let (_t, p, c, control, w) = fixture();
        let task = control.progress.lock().task_id.clone();
        initialize_owner(&p, &c, &task).unwrap();
        let b = bind(&p, &config(), &control.progress.lock()).unwrap();
        fs::write(p.shots_dir.join("fixture"), b"preserve").unwrap();
        write_json_atomic(&p.root.join("archive-profile-owner.json"), &"other").unwrap();
        assert!(purge_owned(&p, &w, &task, b.deadline_ms).is_err());
        assert!(p.shots_dir.join("fixture").exists());
        fs::write(p.root.join("archive-binding.json"), b"bad-json").unwrap();
        assert!(ensure_live(&p.root, &task, i64::MAX).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn managed_symlink_never_deletes_outside_files() {
        let (_t, p, c, control, w) = fixture();
        let task = control.progress.lock().task_id.clone();
        initialize_owner(&p, &c, &task).unwrap();
        let b = bind(&p, &config(), &control.progress.lock()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), p.data_dir.join("recovery-quarantine")).unwrap();
        assert!(purge_owned(&p, &w, &task, b.deadline_ms).is_err());
        assert!(outside.path().join("keep").exists());
    }
}

#[cfg(test)]
mod concurrent_tests {
    use super::*;
    #[test]
    fn concurrent_deadline_updates_never_corrupt_or_extend_state() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let end = chrono::Utc::now().timestamp_millis();
        let task = uuid::Uuid::new_v4().to_string();
        let b = ArchiveBinding {
            upload_mode: "receiver".into(),
            task_id: task.clone(),
            upload_url: "https://fixture.internal".into(),
            survey_id: "study".into(),
            device_id: "device".into(),
            planned_end_ms: end + 10000,
            effective_end_ms: end + 10000,
            deadline_ms: end + 10000 + RETENTION_MS,
            reconciled: false,
            deleted: false,
            end_synced: false,
            target_ms: None,
        };
        save(&root, &b).unwrap();
        let threads = (0..16)
            .map(|i| {
                let root = root.clone();
                let task = task.clone();
                std::thread::spawn(move || {
                    for _ in 0..30 {
                        let value = end + i;
                        let _ = accept_end(&root, &task, value, value + RETENTION_MS);
                        let current = load(&root).unwrap().unwrap();
                        assert!(!current.deleted);
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        let current = load(&root).unwrap().unwrap();
        assert_eq!(current.deadline_ms, end + RETENTION_MS);
        let mut deleted = current.clone();
        deleted.deleted = true;
        save(&root, &deleted).unwrap();
        assert!(accept_end(&root, &task, end, end + RETENTION_MS).is_err());
        assert!(mark_reconciled(&root, &task, end, end + RETENTION_MS).is_err());
        assert!(load(&root).unwrap().unwrap().deleted);
    }
}
