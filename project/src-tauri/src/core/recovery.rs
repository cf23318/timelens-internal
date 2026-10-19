//! Crash journal for newly captured images only. Existing screenshots are never auto-enrolled.
use super::{
    frame_upload::{frame_bytes, Frame, UploadDraft, MAX_FRAME_BYTES},
    models::SnapshotRow,
    storage::DataPaths,
    writer::WriterHandle,
};
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
pub fn recover(paths: &DataPaths, writer: &WriterHandle, task_id: &str) -> Result<usize, String> {
    let root = journal_dir(paths)?;
    let mut count = 0;
    // Interrupted atomic JSON writes cannot masquerade as committed recovery entries.
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())? {
        let p = entry.map_err(|e| e.to_string())?.path();
        if let Some(id) = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json.tmp"))
        {
            uuid::Uuid::parse_str(id).map_err(|_| "恢复临时清单名称无效")?;
            let meta = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 32768 {
                return Err("恢复临时清单无效".into());
            }
            let quarantine = super::storage::private_subdir(&paths.data_dir, "recovery-quarantine")
                .map_err(|e| e.to_string())?;
            let q = quarantine.join(format!("{}-{}.json.tmp", id, uuid::Uuid::new_v4()));
            fs::rename(&p, q).map_err(|e| e.to_string())?;
            sync_dir(&root)?;
            sync_dir(&quarantine)?;
        }
    }
    for entry in fs::read_dir(&root).map_err(|e| e.to_string())? {
        let p = entry.map_err(|e| e.to_string())?.path();
        let meta = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() > 32768
            || p.extension().and_then(|s| s.to_str()) != Some("json")
        {
            return Err("恢复清单类型无效，已保留现场".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(&p)
            .map_err(|e| e.to_string())?
            .take(32769)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 32768 {
            return Err("恢复清单过大".into());
        }
        let j: Journal = serde_json::from_slice(&bytes).map_err(|_| "恢复清单损坏，已保留现场")?;
        if j.version != 1
            || j.upload.task_id != task_id
            || p != manifest(paths, &j.row.id)?
            || j.row.file_size_bytes < 12
            || j.row.file_size_bytes as u64 != j.upload.size_bytes
            || j.upload.size_bytes > MAX_FRAME_BYTES
        {
            return Err("恢复清单身份不匹配，已保留现场".into());
        }
        let target = Path::new(&j.row.file_path);
        let parent = target.parent().ok_or("恢复路径无效")?;
        if parent
            .canonicalize()
            .map_err(|_| "恢复目录不可用")?
            .parent()
            != Some(
                paths
                    .shots_dir
                    .canonicalize()
                    .map_err(|_| "恢复根目录不可用")?
                    .as_path(),
            )
        {
            return Err("恢复图片路径越界".into());
        }
        if !target
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with(&format!("{}_", j.row.id)))
        {
            return Err("恢复文件名无效".into());
        }
        let part = target.with_extension("webp.part");
        if !target.exists() {
            if !part.exists() {
                committed(paths, &j.row.id)?;
                continue;
            } // Crash before the image write started.
            let pm = fs::symlink_metadata(&part).map_err(|e| e.to_string())?;
            if pm.file_type().is_symlink()
                || !pm.is_file()
                || pm.len() != j.upload.size_bytes
                || j.upload.size_bytes > MAX_FRAME_BYTES
            {
                return Err("恢复中的图片不完整，已保留现场".into());
            }
            use sha2::{Digest, Sha256};
            let data = fs::read(&part).map_err(|e| e.to_string())?;
            if format!("{:x}", Sha256::digest(&data)) != j.upload.sha256 {
                return Err("恢复中的图片校验失败".into());
            }
            fs::hard_link(&part, target).map_err(|e| e.to_string())?;
            fs::remove_file(&part).map_err(|e| e.to_string())?;
            sync_dir(parent)?;
        }
        let f = Frame {
            id: j.row.id.clone(),
            path: j.row.file_path.clone(),
            captured_at_ms: j.row.captured_at_ms,
            display_id: j.upload.display_id,
            sha256: j.upload.sha256.clone(),
            size_bytes: j.upload.size_bytes,
            attempts: 0,
        };
        frame_bytes(paths, &f)?;
        if part.exists() {
            let pm = fs::symlink_metadata(&part).map_err(|e| e.to_string())?;
            if !pm.is_file() || pm.file_type().is_symlink() || pm.len() != j.upload.size_bytes {
                return Err("恢复临时图片不匹配，已保留现场".into());
            }
            use sha2::{Digest, Sha256};
            let data = fs::read(&part).map_err(|e| e.to_string())?;
            if format!("{:x}", Sha256::digest(&data)) != j.upload.sha256 {
                return Err("恢复临时图片校验失败".into());
            }
            fs::remove_file(&part).map_err(|e| e.to_string())?;
            sync_dir(parent)?;
        }
        writer.send_snapshot_confirmed(j.row.clone(), j.upload)?;
        committed(paths, &j.row.id)?;
        count += 1;
    }
    Ok(count)
}

/// Restore verifiable image journals before applying the conservative unclean-exit audit gate.
/// A real writer integrity marker still blocks recovery; this never clears unknown event loss.
pub fn startup(
    paths: &DataPaths,
    writer: &WriterHandle,
    control: &super::internal::InternalControl,
) -> Result<(), String> {
    let task = control.progress.lock().task_id.clone();
    if let Err(error) = recover(paths, writer, &task) {
        control.edit_progress(|p| {
            p.integrity_error = Some(error.clone());
            p.last_error = Some(error);
            Ok(())
        })?;
    }
    let audit = control.progress.lock().integrity_error.clone();
    if let Some(error) = audit {
        *writer.metrics.last_error.lock() = Some(error);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::super::{
        models::{SessionUpdateOp, WriteEvent},
        storage::db,
    };
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    fn fixture() -> (
        tempfile::TempDir,
        DataPaths,
        WriterHandle,
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
        let w = super::super::writer::spawn_writer_thread(
            db::open_write(&paths.db_path).unwrap(),
            Arc::new(Default::default()),
        );
        w.try_send(WriteEvent::SessionUpdate(SessionUpdateOp::Insert {
            id: "session".into(),
            app_name: "fixture".into(),
            bundle_id: None,
            window_title: "synthetic".into(),
            start_ms: 0,
            end_ms: 1000,
            duration_ms: 1000,
            intent: None,
            raw_event_count: 0,
            is_active: 0,
            extracted_url: None,
            extracted_file_path: None,
        }))
        .unwrap();
        w.flush().unwrap();
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
        let draft = UploadDraft::new(&task, "display-1", &bytes).unwrap();
        (temp, paths, w, row, draft, bytes)
    }
    #[test]
    fn crash_after_image_commit_recovers_once_without_enrolling_legacy() {
        let (_t, p, w, row, draft, bytes) = fixture();
        stage(&p, &row, &draft, &bytes).unwrap();
        assert!(recover(&p, &w, &uuid::Uuid::new_v4().to_string()).is_err());
        assert!(Path::new(&row.file_path).exists());
        assert_eq!(recover(&p, &w, &draft.task_id).unwrap(), 1);
        assert_eq!(recover(&p, &w, &draft.task_id).unwrap(), 0);
        let c = rusqlite::Connection::open(&p.db_path).unwrap();
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
            let (_t, p, w, row, draft, bytes) = fixture();
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
                    w.send_snapshot_confirmed(row.clone(), draft.clone())
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let result = recover(&p, &w, &draft.task_id);
            if phase == "partial-part" {
                assert!(result.is_err());
                assert!(m.exists());
                assert!(part.exists());
                continue;
            }
            assert!(result.is_ok(), "{phase}: {result:?}");
            assert!(!m.exists());
            assert!(!part.exists());
            if ["complete-part", "final-and-part", "db-committed"].contains(&phase) {
                assert_eq!(fs::read(target).unwrap(), bytes);
            }
            assert_eq!(recover(&p, &w, &draft.task_id).unwrap(), 0);
        }
    }
    #[test]
    fn real_unclean_startup_recovers_images_but_keeps_pause_and_audit() {
        let (_t, p, w, row, mut draft, bytes) = fixture();
        let c = super::super::internal::InternalControl::load(
            &p.root,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        c.start().unwrap();
        draft.task_id = c.progress.lock().task_id.clone();
        stage(&p, &row, &draft, &bytes).unwrap();
        drop(c);
        let restored = super::super::internal::InternalControl::load(
            &p.root,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(restored.progress.lock().integrity_error.is_some());
        assert!(w.check_health().is_ok());
        startup(&p, &w, &restored).unwrap();
        assert!(!restored.tracking.load(Ordering::SeqCst));
        assert!(restored.progress.lock().integrity_error.is_some());
        assert!(w.check_health().is_err());
        let c = rusqlite::Connection::open(&p.db_path).unwrap();
        assert_eq!(
            super::super::frame_upload::stats(&c, &draft.task_id)
                .unwrap()
                .pending,
            1
        );
        assert!(Path::new(&row.file_path).exists());
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
    fn database_failure_returns_error_and_keeps_image_and_manifest() {
        let (_t, p, w, mut row, draft, bytes) = fixture();
        row.session_id = "missing-session".into();
        stage(&p, &row, &draft, &bytes).unwrap();
        assert!(w.send_snapshot_confirmed(row.clone(), draft).is_err());
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
    }
}
