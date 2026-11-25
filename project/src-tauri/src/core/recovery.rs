//! Crash journal for newly captured images only. Existing screenshots are never auto-enrolled.
use super::{
    frame_upload::{frame_bytes, Frame, UploadDraft, MAX_FRAME_BYTES},
    internal::{InternalControl, LEGACY_UNCLEAN_EXIT_ERROR},
    models::SnapshotRow,
    storage::DataPaths,
};
pub(super) const RECOVERY_ERROR: &str = "本地截图恢复未完成：";
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    row: SnapshotRow,
    upload: UploadDraft,
}
fn journal_dir(paths: &DataPaths) -> Result<PathBuf, String> {
    super::storage::private_subdir(&paths.data_dir, "pending-snapshots").map_err(|e| e.to_string())
}
fn manifest(paths: &DataPaths, id: &str) -> Result<PathBuf, String> {
    uuid::Uuid::parse_str(id).map_err(|_| "恢复清单编号无效")?;
    Ok(journal_dir(paths)?.join(format!("{id}.json")))
}
pub fn check_capture_storage(paths: &DataPaths) -> Result<(), String> {
    let journal = journal_dir(paths).map_err(|e| format!("截图暂存目录不可用：{e}"))?;
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let shots = super::storage::private_subdir(&paths.shots_dir, &day)
        .map_err(|e| format!("截图目录不可用：{e}"))?;
    for directory in [journal, shots] {
        let mut probe = tempfile::Builder::new().prefix(".timelens-write-probe-")
            .tempfile_in(directory).map_err(|e| format!("本机截图目录暂时无法写入：{e}"))?;
        probe.write_all(&[0]).and_then(|_| probe.as_file().sync_all())
            .map_err(|e| format!("本机截图目录暂时无法写入：{e}"))?;
    }
    Ok(())
}

pub fn replay_after_pause(paths: &DataPaths, task: &str) -> Result<(), String> {
    if super::archive::load(&paths.root)?.is_some_and(|binding|
        binding.task_id != task || binding.deleted || chrono::Utc::now().timestamp_millis() >= binding.deadline_ms)
    {
        return Ok(());
    }
    if fs::read_dir(journal_dir(paths)?).map_err(|e| e.to_string())?.next().is_none() {
        return Ok(());
    }
    let mut conn = rusqlite::Connection::open_with_flags(&paths.db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;").map_err(|e| e.to_string())?;
    recover(paths, &mut conn, task)?;
    Ok(())
}
fn sync_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub fn stage(
    paths: &DataPaths,
    row: &SnapshotRow,
    upload: &UploadDraft,
    bytes: &[u8],
) -> Result<(), String> {
    if bytes.len() as u64 != upload.size_bytes || upload.size_bytes > MAX_FRAME_BYTES {
        return Err("截图恢复记录大小无效".into());
    }
    let target = Path::new(&row.file_path);
    let parent = target.parent().ok_or("截图目录无效")?;
    if parent.canonicalize().map_err(|e| e.to_string())?.parent()
        != Some(
            paths
                .shots_dir
                .canonicalize()
                .map_err(|e| e.to_string())?
                .as_path(),
        )
    {
        return Err("截图恢复目录越界".into());
    }
    let journal = Journal {
        version: 1,
        row: row.clone(),
        upload: upload.clone(),
    };
    let m = manifest(paths, &row.id)?;
    if m.exists() {
        return Err("截图恢复清单已存在".into());
    }
    let temporary = m.with_extension("json.tmp");
    let mut json_options = fs::OpenOptions::new();
    json_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        json_options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut json_file = json_options.open(&temporary).map_err(|e| e.to_string())?;
    json_file
        .write_all(&serde_json::to_vec(&journal).map_err(|e| e.to_string())?)
        .and_then(|_| json_file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(json_file);
    fs::hard_link(&temporary, &m).map_err(|e| e.to_string())?;
    fs::remove_file(&temporary).map_err(|e| e.to_string())?;
    sync_dir(m.parent().unwrap())?;
    let part = target.with_extension("webp.part");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(&part).map_err(|e| e.to_string())?;
    f.write_all(bytes)
        .and_then(|_| f.sync_all())
        .map_err(|e| e.to_string())?;
    drop(f);
    // hard_link is create-only; unlike rename it cannot replace an existing screenshot.
    fs::hard_link(&part, target).map_err(|e| e.to_string())?;
    fs::remove_file(part).map_err(|e| e.to_string())?;
    sync_dir(parent)
}
pub fn committed(paths: &DataPaths, id: &str) -> Result<(), String> {
    let p = manifest(paths, id)?;
    fs::remove_file(&p).map_err(|e| e.to_string())?;
    sync_dir(p.parent().unwrap())
}
fn quarantine(paths: &DataPaths, file: &Path, name: &str) -> Result<(), String> {
    let dir = super::storage::private_subdir(&paths.data_dir, "recovery-quarantine")
        .map_err(|e| e.to_string())?;
    fs::rename(file, dir.join(format!("{}-{name}", uuid::Uuid::new_v4())))
        .map_err(|e| e.to_string())?;
    sync_dir(&dir)?;
    sync_dir(file.parent().unwrap())
}

#[derive(Debug, Default)]
struct RecoverySummary {
    restored: usize,
    incomplete: usize,
    deferred: usize,
}

enum EntryOutcome {
    Ready(Journal),
    Incomplete,
    Ignored,
}

fn file_metadata(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn recover_entry(
    paths: &DataPaths,
    task_id: &str,
    path: &Path,
) -> Result<EntryOutcome, String> {
    let name = path.file_name().and_then(|n| n.to_str()).ok_or("恢复清单名称无效")?;
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("恢复清单类型无效，已保留现场".into());
    }
    if let Some(id) = name.strip_suffix(".json.tmp") {
        uuid::Uuid::parse_str(id).map_err(|_| "恢复临时清单名称无效")?;
        quarantine(paths, path, name)?;
        return Ok(EntryOutcome::Ignored);
    }
    let id = name.strip_suffix(".json").ok_or("恢复清单扩展名无效")?;
    uuid::Uuid::parse_str(id).map_err(|_| "恢复清单编号无效")?;
    let mut bytes = Vec::new();
    fs::File::open(path).map_err(|e| e.to_string())?
        .take(32769)
        .read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let journal = if bytes.len() <= 32768 {
        serde_json::from_slice::<Journal>(&bytes).ok()
    } else {
        None
    };
    let Some(j) = journal else {
        quarantine(paths, path, name)?;
        return Ok(EntryOutcome::Incomplete);
    };
    if j.version != 1
        || j.upload.task_id != task_id
        || id != j.row.id
        || j.row.file_size_bytes < 12
        || j.row.file_size_bytes as u64 != j.upload.size_bytes
        || j.upload.size_bytes > MAX_FRAME_BYTES
    {
        return Err("恢复清单身份不匹配，已保留现场".into());
    }
    let target = Path::new(&j.row.file_path);
    let parent = target.parent().ok_or("恢复路径无效")?;
    if parent.canonicalize().map_err(|_| "恢复目录不可用")?.parent()
        != Some(paths.shots_dir.canonicalize().map_err(|_| "恢复根目录不可用")?.as_path())
        || target.extension().and_then(|s| s.to_str()) != Some("webp")
        || !target.file_name().and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with(&format!("{id}_")))
    {
        return Err("恢复图片路径越界或文件名无效".into());
    }

    let part = target.with_extension("webp.part");
    let part_meta = file_metadata(&part)?;
    let has_target = file_metadata(target)?.is_some();
    if !has_target {
        let Some(meta) = part_meta.as_ref() else {
            committed(paths, id)?;
            return Ok(EntryOutcome::Ignored);
        };
        if !valid_part(&part, meta, &j.upload)? {
            quarantine(paths, &part, &format!("{id}.webp.part"))?;
            quarantine(paths, path, name)?;
            return Ok(EntryOutcome::Incomplete);
        }
        fs::hard_link(&part, target).map_err(|e| e.to_string())?;
        sync_dir(parent)?;
    }

    let frame = Frame {
        participant_name: j.upload.participant_name.clone(),
        task_id: j.upload.task_id.clone(),
        id: j.row.id.clone(),
        path: j.row.file_path.clone(),
        captured_at_ms: j.row.captured_at_ms,
        display_id: j.upload.display_id,
        sha256: j.upload.sha256.clone(),
        size_bytes: j.upload.size_bytes,
        attempts: 0,
    };
    frame_bytes(paths, &frame)?;
    if let Some(meta) = part_meta {
        if valid_part(&part, &meta, &j.upload)? {
            fs::remove_file(&part).map_err(|e| e.to_string())?;
            sync_dir(parent)?;
        } else {
            quarantine(paths, &part, &format!("{id}.webp.part"))?;
        }
    }

    Ok(EntryOutcome::Ready(j))
}

fn valid_part(path: &Path, meta: &fs::Metadata, upload: &UploadDraft) -> Result<bool, String> {
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("恢复中的图片文件类型无效，已保留现场".into());
    }
    if meta.len() != upload.size_bytes {
        return Ok(false);
    }
    use sha2::{Digest, Sha256};
    let mut data = Vec::new();
    fs::File::open(path).map_err(|e| e.to_string())?
        .take(upload.size_bytes + 1)
        .read_to_end(&mut data).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(&data)) == upload.sha256
        && data.get(..4) == Some(b"RIFF") && data.get(8..12) == Some(b"WEBP"))
}

fn recover(paths: &DataPaths, conn: &mut rusqlite::Connection, task_id: &str) -> Result<RecoverySummary, String> {
    let root = journal_dir(paths)?;
    let mut summary = RecoverySummary::default();
    let entries = fs::read_dir(root).and_then(|entries| entries.collect::<Result<Vec<_>, _>>())
        .map_err(|e| e.to_string())?;
    for entry in entries {
        match recover_entry(paths, task_id, &entry.path()) {
            Ok(EntryOutcome::Ready(journal)) => {
                match super::writer::restore_snapshot(conn, &journal.row, &journal.upload) {
                    Ok(()) => {
                        summary.restored += 1;
                        if let Err(error) = committed(paths, &journal.row.id) {
                            log::warn!("截图已恢复，暂时无法清理恢复清单：{error}");
                            summary.deferred += 1;
                        }
                    }
                    Err(error) if matches!(error, rusqlite::Error::InvalidQuery)
                        || error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
                    {
                        log::warn!("截图恢复记录与数据库不一致，已保留：{error}");
                        summary.deferred += 1;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(EntryOutcome::Incomplete) => summary.incomplete += 1,
            Ok(EntryOutcome::Ignored) => {}
            Err(error) => {
                log::warn!("跳过单条截图恢复记录：{error}");
                summary.deferred += 1;
            }
        }
    }
    Ok(summary)
}

pub(super) fn is_legacy_writer_fault(message: &str) -> bool {
    matches!(message,
        "数据库写入失败，已禁止自动清理：FOREIGN KEY constraint failed"
        | "数据库写入失败，已禁止自动清理：database is locked"
        | "数据库写入失败，已禁止自动清理：database table is locked"
        | "数据库写入失败，已禁止自动清理：database or disk is full"
        | "数据库队列已满，存在未写入的数据；已禁止自动清理"
        | "数据库写入线程已中断，已禁止自动清理"
    )
}

fn is_legacy_recovery_error(message: &str) -> bool {
    message == LEGACY_UNCLEAN_EXIT_ERROR
        || message.starts_with(RECOVERY_ERROR)
        || matches!(message,
            "恢复临时清单无效"
            | "恢复临时清单名称无效"
            | "恢复清单类型无效，已保留现场"
            | "恢复清单损坏，已保留现场"
            | "恢复清单过大"
            | "恢复清单身份不匹配，已保留现场"
            | "恢复中的图片不完整，已保留现场"
            | "恢复中的图片校验失败"
            | "恢复中的图片文件类型无效，已保留现场"
            | "恢复临时图片不匹配，已保留现场"
            | "恢复临时图片校验失败"
            | "等待截图数据库提交超时，恢复清单已保留"
        )
}

pub(super) fn database_passes_check(conn: &rusqlite::Connection) -> bool {
    conn.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .is_ok_and(|check| check == "ok")
        && conn.prepare("PRAGMA foreign_key_check").and_then(|mut stmt| {
            stmt.query([])?.next().map(|row| row.is_none())
        }).unwrap_or(false)
}

/// Replay in SQLite transactions before the writer and upload worker start.
pub fn startup(
    paths: &DataPaths,
    conn: &mut rusqlite::Connection,
    control: &InternalControl,
) -> Result<(), String> {
    let progress = control.progress.lock().clone();
    if super::archive::ensure_live(&paths.root, &progress.task_id, chrono::Utc::now().timestamp_millis()).is_err() {
        return Ok(());
    }
    let marker_path = paths.data_dir.join("integrity-error.json");
    if let Some(error) = super::writer::read_integrity_marker(&marker_path) {
        if !is_legacy_writer_fault(&error) || !database_passes_check(conn) {
            return control.edit_progress(|p| {
                p.last_error = Some(error.clone());
                p.integrity_error = Some(error);
                Ok(())
            });
        }
        // Keep the old diagnostic, but let a healthy database process the pending journal.
        // Clear the matching task message first so an interruption here is retryable.
        control.edit_progress(|p| {
            for slot in [&mut p.integrity_error, &mut p.last_error] {
                if slot.as_ref() == Some(&error) {
                    *slot = None;
                }
            }
            Ok(())
        })?;
        if let Err(error) = quarantine(paths, &marker_path, "writer-error.json") {
            log::warn!("已确认数据库可恢复，旧错误标记暂未移动：{error}");
        }
    }

    match recover(paths, conn, &progress.task_id) {
        Ok(summary) => {
            // Older capture code could persist a single file error without a journal
            // or leave it behind after committing the last journal.
            let old_file_error = progress.integrity_error.as_deref()
                .is_some_and(|error| error.contains("(os error "))
                && database_passes_check(conn);
            let obsolete = |error: &str| is_legacy_recovery_error(error)
                || (old_file_error && error.contains("(os error "))
                || (is_legacy_writer_fault(error) && database_passes_check(conn));
            if !progress.needs_flush && summary.incomplete + summary.deferred == 0
                && !progress.integrity_error.as_deref().is_some_and(obsolete)
                && !progress.last_error.as_deref().is_some_and(obsolete)
            {
                return Ok(());
            }
            control.edit_progress(|p| {
                if p.integrity_error.as_deref().is_some_and(obsolete) {
                    p.integrity_error = None;
                }
                if p.last_error.as_deref().is_some_and(obsolete) {
                    p.last_error = None;
                }
                p.needs_flush = false;
                if summary.incomplete + summary.deferred > 0 {
                    p.last_error = Some(format!(
                        "截图恢复时隔离了 {} 条不完整记录，保留了 {} 条待核对记录。",
                        summary.incomplete, summary.deferred
                    ));
                }
                Ok(())
            })
        }
        Err(error) => control.edit_progress(|p| {
            let error = format!("{RECOVERY_ERROR}{error}");
            p.last_error = Some(error.clone());
            p.integrity_error.get_or_insert(error);
            // The previous process has exited. A recovery-folder failure need not block capture.
            p.needs_flush = false;
            Ok(())
        }),
    }
}
#[cfg(test)]
mod tests {
    use super::super::storage::db;
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    fn fixture() -> (
        tempfile::TempDir,
        DataPaths,
        rusqlite::Connection,
        SnapshotRow,
        UploadDraft,
        Vec<u8>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let paths = DataPaths {
            data_dir: root.join("data"),
            shots_dir: root.join("data/shots"),
            db_path: root.join("data/db.sqlite"),
            exports_dir: root.join("exports"),
            root,
        };
        paths.ensure_dirs().unwrap();
        let conn = db::open_write(&paths.db_path).unwrap();
        conn.execute(
            "INSERT INTO window_sessions(id, start_ms, end_ms, duration_ms, app_name, window_title, is_active)
             VALUES ('session', 0, 1000, 1000, 'fixture', 'synthetic', 0)",
            [],
        ).unwrap();
        let task = uuid::Uuid::new_v4().to_string();
        let id = uuid::Uuid::new_v4().to_string();
        let dir = super::super::storage::private_subdir(&paths.shots_dir, "1970-01-01").unwrap();
        let bytes = webp::Encoder::from_rgb(&[0; 12], 2, 2).encode(85.).to_vec();
        let row = SnapshotRow {
            id: id.clone(),
            session_id: "session".into(),
            file_path: dir
                .join(format!("{id}_display-1_poll.webp"))
                .to_string_lossy()
                .into(),
            captured_at_ms: 1000,
            file_size_bytes: bytes.len() as i64,
            trigger_type: "poll".into(),
            resolution: Some("2x2".into()),
            format: "webp".into(),
            perceptual_hash: None,
        };
        let draft = UploadDraft::new(&task, "display-1", &bytes, "测试 人员").unwrap();
        (temp, paths, conn, row, draft, bytes)
    }
    #[test]
    fn crash_after_image_commit_recovers_once_without_enrolling_legacy() {
        let (_t, p, mut conn, row, draft, bytes) = fixture();
        stage(&p, &row, &draft, &bytes).unwrap();
        assert_eq!(recover(&p, &mut conn, &uuid::Uuid::new_v4().to_string()).unwrap().deferred, 1);
        assert!(Path::new(&row.file_path).exists());
        assert_eq!(recover(&p, &mut conn, &draft.task_id).unwrap().restored, 1);
        assert_eq!(recover(&p, &mut conn, &draft.task_id).unwrap().restored, 0);
        let c = rusqlite::Connection::open(&p.db_path).unwrap();
        assert_eq!(c.query_row("SELECT participant_name FROM frame_upload_outbox WHERE snapshot_id=?",[&row.id],|r|r.get::<_,String>(0)).unwrap(),draft.participant_name);
        assert_eq!(
            super::super::frame_upload::stats(&c, &draft.task_id)
                .unwrap()
                .pending,
            1
        );
        assert!(stage(&p, &row, &draft, &bytes).is_err());
        assert!(Path::new(&row.file_path).exists());
    }
    #[test]
    fn crash_phase_matrix_preserves_and_recovers_managed_files() {
        for phase in [
            "json-tmp",
            "manifest-only",
            "partial-part",
            "complete-part",
            "final-and-part",
            "db-committed",
        ] {
            let (_t, p, mut conn, row, draft, bytes) = fixture();
            let target = Path::new(&row.file_path);
            let part = target.with_extension("webp.part");
            stage(&p, &row, &draft, &bytes).unwrap();
            let m = manifest(&p, &row.id).unwrap();
            match phase {
                "json-tmp" => {
                    fs::remove_file(target).unwrap();
                    fs::rename(&m, m.with_extension("json.tmp")).unwrap();
                }
                "manifest-only" => {
                    fs::remove_file(target).unwrap();
                }
                "partial-part" => {
                    fs::rename(target, &part).unwrap();
                    fs::write(&part, b"partial").unwrap();
                }
                "complete-part" => {
                    fs::rename(target, &part).unwrap();
                }
                "final-and-part" => {
                    fs::hard_link(target, &part).unwrap();
                }
                "db-committed" => {
                    super::super::writer::restore_snapshot(&mut conn, &row, &draft).unwrap();
                    conn.execute("UPDATE frame_upload_outbox SET status='verified' WHERE snapshot_id=?", [&row.id]).unwrap();
                }
                _ => unreachable!(),
            }
            let result = recover(&p, &mut conn, &draft.task_id);
            let summary = result.unwrap_or_else(|e| panic!("{phase}: {e}"));
            assert!(!m.exists());
            assert!(!part.exists());
            let complete = ["complete-part", "final-and-part", "db-committed"].contains(&phase);
            assert_eq!(summary.restored, usize::from(complete));
            assert_eq!(summary.incomplete, usize::from(phase == "partial-part"));
            if complete {
                assert_eq!(fs::read(target).unwrap(), bytes);
            }
            if phase == "partial-part" {
                let quarantine = p.data_dir.join("recovery-quarantine");
                assert_eq!(fs::read_dir(quarantine).unwrap().count(), 2);
                let conn = rusqlite::Connection::open(&p.db_path).unwrap();
                assert_eq!(super::super::frame_upload::stats(&conn, &draft.task_id).unwrap().pending, 0);
            }
            if phase == "db-committed" {
                let status: String = conn.query_row("SELECT status FROM frame_upload_outbox WHERE snapshot_id=?", [&row.id], |r| r.get(0)).unwrap();
                assert_eq!(status, "verified");
            }
            assert_eq!(recover(&p, &mut conn, &draft.task_id).unwrap().restored, 0);
        }
    }
    #[test]
    fn unclean_startup_recovers_images_and_allows_explicit_resume() {
        let (_t, p, mut conn, mut row, mut draft, bytes) = fixture();
        let c = super::super::internal::InternalControl::load(
            &p.root,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        crate::core::internal::fill_test_participant_name(&c);
        c.start().unwrap();
        c.advance(2000).unwrap();
        draft.task_id = c.progress.lock().task_id.clone();
        row.session_id = "not-committed-before-exit".into();
        stage(&p, &row, &draft, &bytes).unwrap();
        let mut incomplete = row.clone();
        incomplete.id = uuid::Uuid::new_v4().to_string();
        let target = Path::new(&row.file_path).with_file_name(format!("{}_display-1_poll.webp", incomplete.id));
        incomplete.file_path = target.to_string_lossy().into_owned();
        stage(&p, &incomplete, &draft, &bytes).unwrap();
        fs::rename(&target, target.with_extension("webp.part")).unwrap();
        fs::write(target.with_extension("webp.part"), b"interrupted").unwrap();
        fs::write(manifest(&p, &uuid::Uuid::new_v4().to_string()).unwrap(), b"{").unwrap();
        drop(c);
        let restored = super::super::internal::InternalControl::load(
            &p.root,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(restored.progress.lock().needs_flush);
        startup(&p, &mut conn, &restored).unwrap();
        assert!(!restored.tracking.load(Ordering::SeqCst));
        let progress = restored.progress.lock().clone();
        assert_eq!(progress.task_id, draft.task_id);
        assert_eq!(progress.elapsed_ms, 2000);
        assert!(!progress.needs_flush);
        assert!(progress.integrity_error.is_none());
        assert!(progress.last_error.as_deref().unwrap().contains("2 条不完整记录"));
        assert!(super::super::cloud_auth::auto_ready(&restored.config.read(), &progress));
        assert!(database_passes_check(&conn));
        let session: (String, i64) = conn.query_row(
            "SELECT window_title, is_active FROM window_sessions WHERE id=?",
            [&row.session_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(session, ("恢复的截图".into(), 0));
        let c = rusqlite::Connection::open(&p.db_path).unwrap();
        assert_eq!(
            super::super::frame_upload::stats(&c, &draft.task_id)
                .unwrap()
                .pending,
            1
        );
        assert!(Path::new(&row.file_path).exists());
        startup(&p, &mut conn, &restored).unwrap();
        assert_eq!(super::super::frame_upload::stats(&c, &draft.task_id).unwrap().pending, 1);
        restored.start().unwrap();
        assert!(restored.tracking.load(Ordering::SeqCst));
    }
    #[test]
    fn old_blocked_task_recovers_even_if_pause_retry_already_cleared_needs_flush() {
        let (_t, paths, mut conn, _, _, _) = fixture();
        let control = InternalControl::load(&paths.root, Arc::new(AtomicBool::new(false))).unwrap();
        crate::core::internal::fill_test_participant_name(&control);
        control.start().unwrap();
        control.pause(false).unwrap();
        control.edit_progress(|p| {
            p.needs_flush = false;
            p.integrity_error = Some(LEGACY_UNCLEAN_EXIT_ERROR.into());
            p.last_error = p.integrity_error.clone();
            Ok(())
        }).unwrap();
        drop(control);
        let reopened = InternalControl::load(&paths.root, Arc::new(AtomicBool::new(false))).unwrap();
        startup(&paths, &mut conn, &reopened).unwrap();
        assert!(!reopened.tracking.load(Ordering::SeqCst));
        assert!(reopened.progress.lock().integrity_error.is_none());
        reopened.start().unwrap();
    }
    #[test]
    fn failed_writer_is_not_reset_by_legacy_task_recovery() {
        let (_t, paths, mut conn, _, _, _) = fixture();
        let control = InternalControl::load(&paths.root, Arc::new(AtomicBool::new(false))).unwrap();
        let error = "数据库写入失败";
        let marker = paths.data_dir.join("integrity-error.json");
        super::super::internal::write_json_atomic(&marker, &error).unwrap();
        startup(&paths, &mut conn, &control).unwrap();
        assert!(marker.exists());
        assert_eq!(control.progress.lock().integrity_error.as_deref(), Some(error));
    }
    #[test]
    fn legacy_database_and_capture_errors_unblock_after_verified_recovery() {
        for error in [
            "数据库写入失败，已禁止自动清理：FOREIGN KEY constraint failed",
            "数据库写入失败，已禁止自动清理：database is locked",
            "数据库队列已满，存在未写入的数据；已禁止自动清理",
            "恢复中的图片不完整，已保留现场",
            "Permission denied (os error 13)",
        ] {
            let (_t, paths, mut conn, mut row, mut draft, bytes) = fixture();
            let control = InternalControl::load(&paths.root, Arc::new(AtomicBool::new(false))).unwrap();
            crate::core::internal::fill_test_participant_name(&control);
            control.start().unwrap();
            draft.task_id = control.progress.lock().task_id.clone();
            row.session_id = "uncommitted".into();
            stage(&paths, &row, &draft, &bytes).unwrap();
            control.edit_progress(|p| {
                p.integrity_error = Some(error.into());
                p.last_error = Some(error.into());
                Ok(())
            }).unwrap();
            let marker = paths.data_dir.join("integrity-error.json");
            if is_legacy_writer_fault(error) {
                super::super::internal::write_json_atomic(&marker, &error).unwrap();
            }
            let reopened = InternalControl::load(&paths.root, Arc::new(AtomicBool::new(false))).unwrap();
            startup(&paths, &mut conn, &reopened).unwrap();
            assert!(!marker.exists());
            assert!(reopened.progress.lock().integrity_error.is_none(), "{error}");
            assert_eq!(super::super::frame_upload::stats(&conn, &draft.task_id).unwrap().pending, 1);
            reopened.start().unwrap();
        }
    }
    #[test]
    fn blocked_quarantine_keeps_the_old_marker_without_blocking_a_healthy_writer() {
        let (_t, paths, mut conn, _row, _draft, _bytes) = fixture();
        let control = InternalControl::load(&paths.root, Arc::new(AtomicBool::new(false))).unwrap();
        crate::core::internal::fill_test_participant_name(&control);
        let marker = paths.data_dir.join("integrity-error.json");
        let error = "数据库写入失败，已禁止自动清理：database is locked";
        super::super::internal::write_json_atomic(&marker, &error).unwrap();
        fs::write(paths.data_dir.join("recovery-quarantine"), b"blocked").unwrap();
        startup(&paths, &mut conn, &control).unwrap();
        assert!(marker.exists());
        let writer = super::super::writer::spawn_recovered_writer_thread(conn, Arc::new(Default::default()));
        writer.check_health().unwrap();
        control.start().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn journal_temporary_symlink_cannot_truncate_another_file() {
        use std::os::unix::fs::symlink;
        let (_t, p, _w, row, draft, bytes) = fixture();
        let sentinel = p.root.join("sentinel");
        fs::write(&sentinel, b"preserve").unwrap();
        let temporary = manifest(&p, &row.id).unwrap().with_extension("json.tmp");
        symlink(&sentinel, &temporary).unwrap();
        assert!(stage(&p, &row, &draft, &bytes).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
        assert!(!Path::new(&row.file_path).exists());
    }
    #[test]
    fn database_busy_preserves_the_image_and_retry_restores_the_outbox() {
        let (_t, p, conn, mut row, draft, bytes) = fixture();
        conn.busy_timeout(std::time::Duration::from_millis(25)).unwrap();
        let blocker = rusqlite::Connection::open(&p.db_path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
        let w = super::super::writer::spawn_writer_thread(conn, Arc::new(Default::default()));
        row.session_id = "missing-session".into();
        stage(&p, &row, &draft, &bytes).unwrap();
        assert!(w.send_snapshot_confirmed(row.clone(), draft.clone()).is_err());
        assert!(w.check_health().is_ok());
        assert!(w.flush().is_err());
        assert!(!p.data_dir.join("integrity-error.json").exists());
        let c = rusqlite::Connection::open(&p.db_path).unwrap();
        for table in ["snapshots", "frame_upload_outbox"] {
            assert_eq!(
                c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert!(Path::new(&row.file_path).exists());
        assert!(manifest(&p, &row.id).unwrap().exists());
        blocker.execute_batch("ROLLBACK;").unwrap();
        w.flush().unwrap();
        replay_after_pause(&p, &draft.task_id).unwrap();
        assert!(!manifest(&p, &row.id).unwrap().exists());
        assert_eq!(super::super::frame_upload::stats(&c, &draft.task_id).unwrap().pending, 1);
    }

    #[test]
    fn screenshot_commits_when_the_session_insert_was_lost() {
        let (_t, p, conn, mut row, draft, bytes) = fixture();
        row.session_id = "dropped-session".into();
        stage(&p, &row, &draft, &bytes).unwrap();
        let w = super::super::writer::spawn_writer_thread(conn, Arc::new(Default::default()));
        w.send_snapshot_confirmed(row, draft.clone()).unwrap();
        let conn = db::open_read(&p.db_path).unwrap();
        assert_eq!(super::super::frame_upload::stats(&conn.lock(), &draft.task_id).unwrap().pending, 1);
    }
}
