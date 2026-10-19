use std::sync::atomic::{AtomicU32, AtomicU64, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use log::{error, warn};
use rusqlite::{params, Connection};

use super::models::{SessionUpdateOp, WriteEvent};

const QUEUE_CAP: usize = 256;
const BATCH_MAX: usize = 64;

#[derive(Debug, Default)]
pub struct WriterMetrics {
    pub last_error: parking_lot::Mutex<Option<String>>,
    error_marker: parking_lot::Mutex<Option<std::path::PathBuf>>,
    pub total_batches: AtomicU64,
    pub total_events: AtomicU64,
    pub batch_latency_sum_ms: AtomicU64,
    pub last_batch_events: AtomicU32,
    pub last_batch_ms: AtomicU64,
}

impl WriterMetrics {
    fn record_error(&self, message: String) {
        let mut error = self.last_error.lock();
        if error.is_some() {
            return;
        }
        *error = Some(message.clone());
        if let Some(path) = self.error_marker.lock().as_ref() {
            if crate::core::internal::write_json_atomic(path, &message).is_err() {
                log::error!("无法持久保存数据库错误标记；任务保留未完成落盘状态");
            }
        }
    }
    pub fn snapshot(&self, pending_estimate: u32) -> crate::core::models::WriterStats {
        let batches = self.total_batches.load(AtomicOrdering::Relaxed);
        let events = self.total_events.load(AtomicOrdering::Relaxed);
        let sum = self.batch_latency_sum_ms.load(AtomicOrdering::Relaxed);
        let avg_batch = if batches > 0 {
            events as f64 / batches as f64
        } else {
            0.0
        };
        let avg_lat = if batches > 0 {
            sum as f64 / batches as f64
        } else {
            0.0
        };
        crate::core::models::WriterStats {
            total_batches: batches,
            total_events: events,
            avg_batch_size: avg_batch,
            avg_latency_ms: avg_lat,
            last_batch_events: self.last_batch_events.load(AtomicOrdering::Relaxed),
            last_batch_ms: self.last_batch_ms.load(AtomicOrdering::Relaxed),
            channel_pending_estimate: pending_estimate,
        }
    }
}

fn event_sort_key(e: &WriteEvent) -> u8 {
    match e {
        WriteEvent::SessionUpdate(_) => 0,
        WriteEvent::AppSwitch(_) => 1,
        WriteEvent::RawEvent(_) => 2,
        WriteEvent::Snapshot(_) => 3,
        WriteEvent::SnapshotOcr(_) => 4,
        WriteEvent::InputMetric(_) => 5,
        WriteEvent::ClipboardFlow(_) => 6,
        WriteEvent::Notification(_) => 7,
        WriteEvent::AmbientContext(_) => 8,
        WriteEvent::NudgeLog(_) => 8,
        WriteEvent::Retention { .. } => 9,
        WriteEvent::WalCheckpoint => 10,
        WriteEvent::Shutdown | WriteEvent::Flush(_) | WriteEvent::EraseCollectedData(_) => 0,
    }
}

#[derive(Clone)]
pub struct WriterHandle {
    pub tx: Sender<WriteEvent>,
    pub metrics: Arc<WriterMetrics>,
}

impl WriterHandle {
    pub fn check_health(&self) -> Result<(), String> {
        self.metrics.last_error.lock().clone().map_or(Ok(()), Err)
    }
    pub fn erase_collected_data(&self) -> Result<(), String> {
        self.check_health()?;
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send_timeout(
                WriteEvent::EraseCollectedData(tx),
                std::time::Duration::from_secs(5),
            )
            .map_err(|_| "数据库写入队列不可用")?;
        rx.recv_timeout(std::time::Duration::from_secs(120))
            .map_err(|_| "等待采集数据清理超时")?
    }
    pub fn flush(&self) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send_timeout(WriteEvent::Flush(tx), std::time::Duration::from_secs(5))
            .map_err(|_| "数据库写入队列不可用")?;
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|_| "等待数据库写入完成超时")?
    }
    pub fn try_send(&self, ev: WriteEvent) -> Result<(), WriteEvent> {
        match self.tx.try_send(ev) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(ev)) => {
                self.metrics
                    .record_error("数据库队列已满，存在未写入的数据；已禁止自动清理".into());
                warn!("writer queue full, dropping event");
                Err(ev)
            }
            Err(TrySendError::Disconnected(ev)) => {
                self.metrics
                    .record_error("数据库写入线程已中断，已禁止自动清理".into());
                Err(ev)
            }
        }
    }
}

pub fn spawn_writer_thread(mut conn: Connection, metrics: Arc<WriterMetrics>) -> WriterHandle {
    let marker = conn
        .path()
        .filter(|p| !p.is_empty())
        .and_then(|p| std::path::Path::new(p).parent())
        .map(|p| p.join("integrity-error.json"));
    if let Some(path) = marker.as_ref().filter(|p| p.exists()) {
        let message = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<String>(&bytes).ok())
            .unwrap_or_else(|| "已有数据库完整性错误标记，请人工审核".into());
        *metrics.last_error.lock() = Some(message);
    }
    *metrics.error_marker.lock() = marker;
    let (tx, rx) = bounded(QUEUE_CAP);
    let metrics_thread = metrics.clone();
    std::thread::spawn(move || writer_loop(&mut conn, rx, metrics_thread));
    WriterHandle { tx, metrics }
}

fn writer_loop(conn: &mut Connection, rx: Receiver<WriteEvent>, metrics: Arc<WriterMetrics>) {
    loop {
        let first = match rx.recv() {
            Ok(e) => e,
            Err(_) => break,
        };
        if matches!(first, WriteEvent::Shutdown) {
            let _ = super::storage::db::wal_checkpoint(conn);
            break;
        }
        if let WriteEvent::Flush(done) = first {
            let _ = done.send(metrics.last_error.lock().clone().map_or(Ok(()), Err));
            continue;
        }
        if let WriteEvent::EraseCollectedData(done) = first {
            let health = metrics.last_error.lock().clone().map_or(Ok(()), Err);
            let _ = done.send(health.and_then(|_| erase_data(conn)));
            continue;
        }
        let mut batch = vec![first];
        while batch.len() < BATCH_MAX {
            match rx.try_recv() {
                Ok(e) => {
                    if matches!(
                        e,
                        WriteEvent::Shutdown
                            | WriteEvent::Flush(_)
                            | WriteEvent::EraseCollectedData(_)
                    ) {
                        batch.push(e);
                        break;
                    }
                    batch.push(e);
                }
                Err(_) => break,
            }
        }
        let has_shutdown = batch.iter().any(|e| matches!(e, WriteEvent::Shutdown));
        let mut flushes = Vec::new();
        let mut erasures = Vec::new();
        batch.retain(|e| match e {
            WriteEvent::Flush(done) => {
                flushes.push(done.clone());
                false
            }
            WriteEvent::EraseCollectedData(done) => {
                erasures.push(done.clone());
                false
            }
            _ => true,
        });
        batch.sort_by(|a, b| event_sort_key(a).cmp(&event_sort_key(b)));
        let started = Instant::now();
        let n = batch.len() as u64;
        let result = apply_batch(conn, &batch).map_err(|e| e.to_string());
        if let Err(ref e) = result {
            error!("writer batch failed: {e}");
            metrics.record_error(format!("数据库写入失败，已禁止自动清理：{e}"));
        }
        let health = metrics.last_error.lock().clone().map_or(Ok(()), Err);
        for done in flushes {
            let _ = done.send(health.clone());
        }
        for done in erasures {
            let _ = done.send(health.clone().and_then(|_| erase_data(conn)));
        }
        let ms = started.elapsed().as_millis() as u64;
        metrics.total_batches.fetch_add(1, AtomicOrdering::Relaxed);
        metrics.total_events.fetch_add(n, AtomicOrdering::Relaxed);
        metrics
            .batch_latency_sum_ms
            .fetch_add(ms, AtomicOrdering::Relaxed);
        metrics
            .last_batch_events
            .store(n as u32, AtomicOrdering::Relaxed);
        metrics.last_batch_ms.store(ms, AtomicOrdering::Relaxed);
        if has_shutdown {
            let _ = super::storage::db::wal_checkpoint(conn);
            break;
        }
    }
}

pub(crate) fn erase_data(conn: &mut Connection) -> Result<(), String> {
    conn.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA secure_delete=ON;")
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let tables = {
            let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name <> 'schema_migrations' AND name NOT LIKE 'snapshot_ocr_fts_%'").map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| e.to_string())?
        };
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for table in tables {
            tx.execute(
                &format!("DELETE FROM \"{}\"", table.replace('"', "\"\"")),
                [],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        conn.execute_batch("VACUUM;").map_err(|e| e.to_string())?;
        let busy: i32 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if busy != 0 {
            return Err("数据库仍被占用，清理需要重试".into());
        }
        Ok(())
    })();
    let _ = conn.execute_batch("PRAGMA foreign_keys=ON;");
    result
}

fn apply_batch(conn: &mut Connection, batch: &[WriteEvent]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    for ev in batch {
        match ev {
            WriteEvent::RawEvent(r) => {
                tx.execute(
                    r#"INSERT INTO raw_events (
                        id, timestamp_ms, app_name, bundle_id, window_title,
                        extracted_url, extracted_file_path, idle_seconds,
                        is_fullscreen, is_audio_playing, state_hash, trigger_type, created_at
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
                    params![
                        r.id,
                        r.timestamp_ms,
                        r.app_name,
                        r.bundle_id,
                        r.window_title,
                        r.extracted_url,
                        r.extracted_file_path,
                        r.idle_seconds,
                        r.is_fullscreen,
                        r.is_audio_playing,
                        r.state_hash,
                        r.trigger_type,
                        r.created_at,
                    ],
                )?;
                let now = r.timestamp_ms;
                tx.execute(
                    r#"INSERT INTO app_meta (app_name, bundle_id, first_seen_ms, last_seen_ms)
                    VALUES (?1, ?2, ?3, ?4)
                    ON CONFLICT(app_name) DO UPDATE SET
                        bundle_id = COALESCE(excluded.bundle_id, app_meta.bundle_id),
                        last_seen_ms = excluded.last_seen_ms"#,
                    params![r.app_name, r.bundle_id, now, now],
                )?;
            }
            WriteEvent::AppSwitch(s) => {
                tx.execute(
                    r#"INSERT INTO app_switches (
                        id, timestamp_ms, from_app, from_bundle_id, from_window_title,
                        to_app, to_bundle_id, to_window_title, from_session_duration_ms, switch_type
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"#,
                    params![
                        s.id,
                        s.timestamp_ms,
                        s.from_app,
                        s.from_bundle_id,
                        s.from_window_title,
                        s.to_app,
                        s.to_bundle_id,
                        s.to_window_title,
                        s.from_session_duration_ms,
                        s.switch_type,
                    ],
                )?;
            }
            WriteEvent::Snapshot(s) => {
                tx.execute(
                    r#"INSERT INTO snapshots (
                        id, session_id, file_path, captured_at_ms, file_size_bytes,
                        trigger_type, resolution, format, perceptual_hash
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
                    params![
                        s.id,
                        s.session_id,
                        s.file_path,
                        s.captured_at_ms,
                        s.file_size_bytes,
                        s.trigger_type,
                        s.resolution,
                        s.format,
                        s.perceptual_hash,
                    ],
                )?;
            }
            WriteEvent::SnapshotOcr(r) => {
                tx.execute(
                    r#"INSERT INTO snapshot_ocr (
                        snapshot_id, session_id, captured_at_ms, ocr_text, ocr_meta, redacted,
                        status, error_hint, processed_at_ms
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    ON CONFLICT(snapshot_id) DO UPDATE SET
                        ocr_text = excluded.ocr_text,
                        ocr_meta = excluded.ocr_meta,
                        redacted = excluded.redacted,
                        status = excluded.status,
                        error_hint = excluded.error_hint,
                        processed_at_ms = excluded.processed_at_ms"#,
                    params![
                        r.snapshot_id,
                        r.session_id,
                        r.captured_at_ms,
                        r.ocr_text,
                        r.ocr_meta,
                        r.redacted,
                        r.status,
                        r.error_hint,
                        r.processed_at_ms,
                    ],
                )?;
                tx.execute(
                    "DELETE FROM snapshot_ocr_fts WHERE snapshot_id = ?1",
                    [&r.snapshot_id],
                )?;
                if let Some(ref body) = r.fts_body {
                    if !body.trim().is_empty() {
                        tx.execute(
                            r#"INSERT INTO snapshot_ocr_fts (snapshot_id, session_id, captured_at_ms, body)
                               VALUES (?1, ?2, ?3, ?4)"#,
                            params![r.snapshot_id, r.session_id, r.captured_at_ms, body],
                        )?;
                    }
                }
                if r.update_session_context {
                    tx.execute(
                        r#"INSERT INTO session_ocr_context (
                            session_id, summary_line, summary_source, updated_at_ms, empty_reason
                        ) VALUES (?1, ?2, ?3, ?4, ?5)
                        ON CONFLICT(session_id) DO UPDATE SET
                            summary_line = excluded.summary_line,
                            summary_source = excluded.summary_source,
                            updated_at_ms = excluded.updated_at_ms,
                            empty_reason = excluded.empty_reason"#,
                        params![
                            r.session_id,
                            r.session_summary_line,
                            r.session_summary_source,
                            r.processed_at_ms,
                            r.session_empty_reason,
                        ],
                    )?;
                }
            }
            WriteEvent::InputMetric(r) => {
                tx.execute(
                    r#"INSERT INTO input_metrics (
                        id, timestamp_ms, session_id, window_interval_secs,
                        keystrokes_count, kpm, delete_count, delete_ratio, shortcut_count,
                        copy_count, paste_count, undo_count, mouse_click_count, mouse_distance_px,
                        scroll_delta_total, scroll_direction_changes, typing_burst_count, longest_pause_ms
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"#,
                    params![
                        r.id,
                        r.timestamp_ms,
                        r.session_id,
                        r.window_interval_secs,
                        r.keystrokes_count,
                        r.kpm,
                        r.delete_count,
                        r.delete_ratio,
                        r.shortcut_count,
                        r.copy_count,
                        r.paste_count,
                        r.undo_count,
                        r.mouse_click_count,
                        r.mouse_distance_px,
                        r.scroll_delta_total,
                        r.scroll_direction_changes,
                        r.typing_burst_count,
                        r.longest_pause_ms,
                    ],
                )?;
            }
            WriteEvent::ClipboardFlow(r) => {
                tx.execute(
                    r#"INSERT INTO clipboard_flows (
                        id, timestamp_ms, action, app_name, bundle_id, content_type, content_length, flow_pair_id
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
                    params![
                        r.id,
                        r.timestamp_ms,
                        r.action,
                        r.app_name,
                        r.bundle_id,
                        r.content_type,
                        r.content_length,
                        r.flow_pair_id,
                    ],
                )?;
            }
            WriteEvent::Notification(r) => {
                tx.execute(
                    r#"INSERT INTO notifications (
                        id, timestamp_ms, source_app, source_bundle_id, current_foreground_app,
                        user_responded, response_delay_ms, caused_switch
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
                    params![
                        r.id,
                        r.timestamp_ms,
                        r.source_app,
                        r.source_bundle_id,
                        r.current_foreground_app,
                        r.user_responded,
                        r.response_delay_ms,
                        r.caused_switch,
                    ],
                )?;
            }
            WriteEvent::AmbientContext(r) => {
                tx.execute(
                    r#"INSERT INTO ambient_context (
                        id, timestamp_ms, wifi_ssid, display_count, is_external_display,
                        battery_level, is_charging, is_camera_active, is_audio_input_active,
                        is_dnd_enabled, screen_brightness, active_space_index
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
                    params![
                        r.id,
                        r.timestamp_ms,
                        r.wifi_ssid,
                        r.display_count,
                        r.is_external_display,
                        r.battery_level,
                        r.is_charging,
                        r.is_camera_active,
                        r.is_audio_input_active,
                        r.is_dnd_enabled,
                        r.screen_brightness,
                        r.active_space_index,
                    ],
                )?;
            }
            WriteEvent::NudgeLog(r) => {
                tx.execute(
                    r#"INSERT INTO nudge_log (
                        id, timestamp_ms, nudge_type, payload_json, dismissed
                    ) VALUES (?1, ?2, ?3, ?4, ?5)"#,
                    params![
                        r.id,
                        r.timestamp_ms,
                        r.nudge_type,
                        r.payload_json,
                        r.dismissed,
                    ],
                )?;
            }
            WriteEvent::WalCheckpoint => {
                let _ = super::storage::db::wal_checkpoint(&*tx);
            }
            WriteEvent::Retention {
                raw_cutoff_ms,
                snapshot_cutoff_ms,
            } => {
                tx.execute(
                    "DELETE FROM raw_events WHERE timestamp_ms < ?1",
                    [raw_cutoff_ms],
                )?;
                let mut stmt = tx.prepare(
                    "SELECT id, file_path FROM snapshots WHERE captured_at_ms < ?1 AND file_path != ''",
                )?;
                let paths: Vec<(String, String)> = stmt
                    .query_map([snapshot_cutoff_ms], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .filter_map(|x| x.ok())
                    .collect();
                drop(stmt);
                for (_, fp) in &paths {
                    let _ = std::fs::remove_file(fp);
                }
                tx.execute(
                    "UPDATE snapshots SET file_path = '' WHERE captured_at_ms < ?1",
                    [snapshot_cutoff_ms],
                )?;
            }
            WriteEvent::SessionUpdate(op) => match op {
                SessionUpdateOp::DeactivateAll => {
                    tx.execute("UPDATE window_sessions SET is_active = 0", [])?;
                }
                SessionUpdateOp::Insert {
                    id,
                    start_ms,
                    end_ms,
                    duration_ms,
                    app_name,
                    bundle_id,
                    window_title,
                    extracted_url,
                    extracted_file_path,
                    intent,
                    raw_event_count,
                    is_active,
                } => {
                    tx.execute(
                        r#"INSERT INTO window_sessions (
                            id, start_ms, end_ms, duration_ms, app_name, bundle_id, window_title,
                            extracted_url, extracted_file_path, intent, raw_event_count, is_active
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
                        params![
                            id,
                            start_ms,
                            end_ms,
                            duration_ms,
                            app_name,
                            bundle_id,
                            window_title,
                            extracted_url,
                            extracted_file_path,
                            intent,
                            raw_event_count,
                            is_active,
                        ],
                    )?;
                }
                SessionUpdateOp::Close {
                    id,
                    end_ms,
                    duration_ms,
                } => {
                    tx.execute(
                        "UPDATE window_sessions SET end_ms = ?2, duration_ms = ?3, is_active = 0 WHERE id = ?1",
                        params![id, end_ms, duration_ms],
                    )?;
                }
                SessionUpdateOp::BumpRawCount {
                    id,
                    end_ms,
                    duration_ms,
                    delta,
                } => {
                    tx.execute(
                        "UPDATE window_sessions SET end_ms = ?2, duration_ms = ?3, raw_event_count = raw_event_count + ?4 WHERE id = ?1",
                        params![id, end_ms, duration_ms, delta],
                    )?;
                }
            },
            WriteEvent::Shutdown | WriteEvent::Flush(_) | WriteEvent::EraseCollectedData(_) => {}
        }
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{RawEventRow, SessionUpdateOp, SnapshotRow, WriteEvent};
    use crate::core::storage::migrations::run_migrations;
    use rusqlite::Connection;

    fn conn_with_schema() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        run_migrations(&mut c).unwrap();
        c
    }

    fn sample_raw(id: &str, ts: i64) -> RawEventRow {
        RawEventRow {
            id: id.into(),
            timestamp_ms: ts,
            app_name: "TestApp".into(),
            bundle_id: Some("com.test.app".into()),
            window_title: "Window".into(),
            extracted_url: None,
            extracted_file_path: None,
            idle_seconds: 0.0,
            is_fullscreen: 0,
            is_audio_playing: 0,
            state_hash: 42,
            trigger_type: "poll".into(),
            created_at: ts,
        }
    }

    #[test]
    fn apply_batch_raw_inserts_row_and_app_meta() {
        let mut c = conn_with_schema();
        apply_batch(
            &mut c,
            &[WriteEvent::RawEvent(sample_raw("r1", 1_700_000_000_000))],
        )
        .unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM raw_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let meta: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM app_meta WHERE app_name = 'TestApp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(meta, 1);
    }

    #[test]
    fn apply_batch_session_insert_and_close() {
        let mut c = conn_with_schema();
        apply_batch(
            &mut c,
            &[WriteEvent::SessionUpdate(SessionUpdateOp::Insert {
                id: "sess1".into(),
                start_ms: 100,
                end_ms: 100,
                duration_ms: 0,
                app_name: "A".into(),
                bundle_id: None,
                window_title: "t".into(),
                extracted_url: None,
                extracted_file_path: None,
                intent: None,
                raw_event_count: 1,
                is_active: 1,
            })],
        )
        .unwrap();
        apply_batch(
            &mut c,
            &[WriteEvent::SessionUpdate(SessionUpdateOp::Close {
                id: "sess1".into(),
                end_ms: 200,
                duration_ms: 100,
            })],
        )
        .unwrap();
        let active: i64 = c
            .query_row(
                "SELECT is_active FROM window_sessions WHERE id = 'sess1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(active, 0);
        let dur: i64 = c
            .query_row(
                "SELECT duration_ms FROM window_sessions WHERE id = 'sess1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dur, 100);
    }

    #[test]
    fn retention_removes_old_snapshot_file_and_clears_path() {
        let mut c = conn_with_schema();
        apply_batch(
            &mut c,
            &[WriteEvent::SessionUpdate(SessionUpdateOp::Insert {
                id: "sess_r".into(),
                start_ms: 1,
                end_ms: 1,
                duration_ms: 0,
                app_name: "A".into(),
                bundle_id: None,
                window_title: "t".into(),
                extracted_url: None,
                extracted_file_path: None,
                intent: None,
                raw_event_count: 0,
                is_active: 0,
            })],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let fp = dir.path().join("shot.webp");
        std::fs::write(&fp, b"webp").unwrap();
        let fp_str = fp.to_string_lossy().into_owned();
        apply_batch(
            &mut c,
            &[WriteEvent::Snapshot(SnapshotRow {
                id: "snap_r".into(),
                session_id: "sess_r".into(),
                file_path: fp_str,
                captured_at_ms: 1_000,
                file_size_bytes: 4,
                trigger_type: "poll".into(),
                resolution: None,
                format: "webp".into(),
                perceptual_hash: None,
            })],
        )
        .unwrap();
        assert!(fp.exists());
        apply_batch(
            &mut c,
            &[WriteEvent::Retention {
                raw_cutoff_ms: 10_000,
                snapshot_cutoff_ms: 10_000,
            }],
        )
        .unwrap();
        assert!(!fp.exists(), "snapshot file should be deleted");
        let path: String = c
            .query_row(
                "SELECT file_path FROM snapshots WHERE id = 'snap_r'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(path.is_empty());
    }

    #[test]
    fn writer_thread_shutdown_persists_queued_raw() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("w.sqlite");
        let conn = crate::core::storage::db::open_write(&db_path).unwrap();
        let metrics = Arc::new(WriterMetrics::default());
        let h = spawn_writer_thread(conn, metrics);
        h.try_send(WriteEvent::RawEvent(sample_raw("thr1", 2_000_000_000_000)))
            .expect("send raw");
        h.try_send(WriteEvent::Shutdown).expect("send shutdown");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let read = rusqlite::Connection::open(&db_path).unwrap();
        let n: i64 = read
            .query_row(
                "SELECT COUNT(*) FROM raw_events WHERE id = 'thr1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn flush_waits_for_commit_and_erasure_cleans_rows_and_fts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.sqlite");
        let conn = crate::core::storage::db::open_write(&path).unwrap();
        conn.execute("INSERT INTO snapshot_ocr_fts(snapshot_id,session_id,captured_at_ms,body) VALUES('a','s',1,'private-fixture')", []).unwrap();
        let h = spawn_writer_thread(conn, Arc::new(WriterMetrics::default()));
        h.try_send(WriteEvent::RawEvent(sample_raw(
            "flush-test",
            2_000_000_000_000,
        )))
        .unwrap();
        h.flush().unwrap();
        let read = Connection::open(&path).unwrap();
        assert_eq!(
            read.query_row("SELECT COUNT(*) FROM raw_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        h.erase_collected_data().unwrap();
        for table in ["raw_events", "app_meta", "snapshot_ocr_fts", "settings"] {
            assert_eq!(
                read.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert!(
            read.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap()
                > 0
        );
        h.try_send(WriteEvent::Shutdown).unwrap();
    }

    #[test]
    fn prior_write_failure_stays_visible_to_later_flush_and_prevents_erasure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task.sqlite");
        let conn = crate::core::storage::db::open_write(&path).unwrap();
        let h = spawn_writer_thread(conn, Arc::new(WriterMetrics::default()));
        h.try_send(WriteEvent::Snapshot(crate::core::models::SnapshotRow {
            id: "bad".into(),
            session_id: "nonexistent".into(),
            file_path: "fixture.webp".into(),
            captured_at_ms: 1,
            file_size_bytes: 1,
            trigger_type: "poll".into(),
            resolution: None,
            format: "webp".into(),
            perceptual_hash: None,
        }))
        .unwrap();
        assert!(h.flush().is_err());
        h.try_send(WriteEvent::RawEvent(sample_raw("later", 2_000_000_000_000)))
            .unwrap();
        assert!(h.flush().is_err());
        assert!(h.erase_collected_data().is_err());
        let read = Connection::open(&path).unwrap();
        assert_eq!(
            read.query_row("SELECT COUNT(*) FROM raw_events", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        h.try_send(WriteEvent::Shutdown).unwrap();
        let restored = spawn_writer_thread(
            crate::core::storage::db::open_write(&path).unwrap(),
            Arc::new(WriterMetrics::default()),
        );
        assert!(restored.flush().is_err());
        assert!(restored.erase_collected_data().is_err());
        restored.try_send(WriteEvent::Shutdown).unwrap();
    }
}
