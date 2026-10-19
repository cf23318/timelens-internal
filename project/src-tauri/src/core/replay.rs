//! Read-only screenshot playback. OCR enriches a frame but never determines its visibility.

use rusqlite::{params, Connection};
use serde::Serialize;

use super::{settings, time_range::local_day_bounds_ms};

const PAGE_SIZE: u32 = 100;
const PREVIEW_CHARS: usize = 240;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayFrame {
    pub snapshot_id: String,
    pub captured_at_ms: i64,
    pub app_name: String,
    pub window_title: String,
    pub ocr_preview: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayPage {
    pub date: String,
    pub offset: i64,
    pub total: i64,
    pub ocr_enabled: bool,
    pub items: Vec<ReplayFrame>,
}

pub fn read_page(
    conn: &Connection,
    date: &str,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<ReplayPage, String> {
    let (start, end) = local_day_bounds_ms(date)?;
    // Keep the count and page on the same read snapshot if collection or cleanup is concurrent.
    let read = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let conn = &read;
    let limit = i64::from(limit.unwrap_or(PAGE_SIZE).clamp(1, PAGE_SIZE));
    let total: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM snapshots WHERE captured_at_ms BETWEEN ?1 AND ?2",
            params![start, end],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    // A date change or concurrent cleanup can make an old offset exceed the new count.
    let last_page = (total - 1).max(0) / limit * limit;
    let offset = i64::from(offset.unwrap_or(0)).min(last_page);
    let ocr_enabled = settings::get_ocr_enabled(conn);
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.captured_at_ms,
                    COALESCE(ws.app_name, ''), COALESCE(ws.window_title, ''),
                    CASE WHEN ?3 AND o.status = 'ok' AND o.redacted = 0
                         THEN substr(o.ocr_text, 1, ?4) ELSE NULL END
             FROM snapshots s
             LEFT JOIN window_sessions ws ON ws.id = s.session_id
             LEFT JOIN snapshot_ocr o ON o.snapshot_id = s.id
             WHERE s.captured_at_ms BETWEEN ?1 AND ?2
             ORDER BY s.captured_at_ms ASC, s.id ASC
             LIMIT ?5 OFFSET ?6",
        )
        .map_err(|e| e.to_string())?;
    let items = stmt
        .query_map(
            params![
                start,
                end,
                ocr_enabled,
                PREVIEW_CHARS as i64 + 1,
                limit,
                offset
            ],
            |row| {
                let text: Option<String> = row.get(4)?;
                let ocr_preview = text.and_then(|text| {
                    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if collapsed.is_empty() {
                        return None;
                    }
                    let mut preview: String = collapsed.chars().take(PREVIEW_CHARS).collect();
                    if collapsed.chars().count() > PREVIEW_CHARS {
                        preview.push('…');
                    }
                    Some(preview)
                });
                Ok(ReplayFrame {
                    snapshot_id: row.get(0)?,
                    captured_at_ms: row.get(1)?,
                    app_name: row.get(2)?,
                    window_title: row.get(3)?,
                    ocr_preview,
                })
            },
        )
        .map_err(|e| e.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())?;
    Ok(ReplayPage {
        date: date.into(),
        offset,
        total,
        ocr_enabled,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::storage::migrations::run_migrations;

    fn fixture() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        run_migrations(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO window_sessions
             (id,start_ms,end_ms,duration_ms,app_name,window_title)
             VALUES ('short',0,1000,1000,'Fixture App','Fixture Window')",
            [],
        )
        .unwrap();
        conn
    }

    fn shot(conn: &Connection, id: &str, at: i64) {
        conn.execute(
            "INSERT INTO snapshots (id,session_id,file_path,captured_at_ms,perceptual_hash,trigger_type)
             VALUES (?1,'short','synthetic.webp',?2,'same-hash','poll')",
            params![id, at],
        )
        .unwrap();
    }

    fn ocr(conn: &Connection, id: &str, text: &str, status: &str, redacted: i64) {
        conn.execute(
            "INSERT INTO snapshot_ocr
             (snapshot_id,session_id,captured_at_ms,ocr_text,status,redacted,processed_at_ms)
             VALUES (?1,'short',0,?2,?3,?4,0)",
            params![id, text, status, redacted],
        )
        .unwrap();
    }

    #[test]
    fn ocr_off_keeps_short_sessions_and_same_hash_frames_beyond_highlight_limit() {
        let conn = fixture();
        let (start, _) = local_day_bounds_ms("2026-10-19").unwrap();
        for i in 0..130 {
            shot(&conn, &format!("frame-{i:03}"), start + i);
        }
        ocr(
            &conn,
            "frame-000",
            "old OCR must stay hidden when disabled",
            "ok",
            0,
        );
        let page = read_page(&conn, "2026-10-19", None, None).unwrap();
        assert!(!page.ocr_enabled);
        assert_eq!(page.total, 130);
        assert_eq!(page.items.len(), 100);
        assert!(page.items.iter().all(|f| f.ocr_preview.is_none()));
        let next = read_page(&conn, "2026-10-19", Some(100), None).unwrap();
        assert_eq!(next.items.len(), 30);
        assert_eq!(next.items[0].snapshot_id, "frame-100");
    }

    #[test]
    fn ocr_is_optional_and_failed_or_redacted_text_is_never_exposed() {
        let mut conn = fixture();
        settings::set_ocr_enabled(&mut conn, true).unwrap();
        let (start, _) = local_day_bounds_ms("2026-10-19").unwrap();
        for id in ["good", "redacted", "failed", "pending", "empty", "long"] {
            shot(&conn, id, start);
        }
        ocr(&conn, "good", "  visible\n  text  ", "ok", 0);
        ocr(&conn, "redacted", "sensitive text", "ok", 1);
        ocr(&conn, "failed", "incomplete text", "error", 0);
        ocr(&conn, "empty", "  \n  ", "ok", 0);
        ocr(&conn, "long", &"字".repeat(300), "ok", 0);
        let page = read_page(&conn, "2026-10-19", None, None).unwrap();
        assert_eq!(page.total, 6);
        assert!(page.ocr_enabled);
        for frame in page.items {
            match frame.snapshot_id.as_str() {
                "good" => assert_eq!(frame.ocr_preview.as_deref(), Some("visible text")),
                "long" => assert_eq!(frame.ocr_preview.unwrap(), format!("{}…", "字".repeat(240))),
                _ => assert!(frame.ocr_preview.is_none()),
            }
        }
    }

    #[test]
    fn pages_are_bounded_and_stably_ordered_for_equal_timestamps() {
        let conn = fixture();
        let (start, _) = local_day_bounds_ms("2026-10-19").unwrap();
        for i in (0..205).rev() {
            shot(&conn, &format!("{i:03}"), start);
        }
        let page = read_page(&conn, "2026-10-19", None, Some(u32::MAX)).unwrap();
        assert_eq!(page.items.len(), 100);
        assert_eq!(page.items[0].snapshot_id, "000");
        assert_eq!(page.items[99].snapshot_id, "099");
        let last = read_page(&conn, "2026-10-19", Some(u32::MAX), None).unwrap();
        assert_eq!(last.offset, 200);
        assert_eq!(last.items.len(), 5);
        assert_eq!(last.items[4].snapshot_id, "204");
        assert_eq!(
            read_page(&conn, "2026-10-19", None, Some(0))
                .unwrap()
                .items
                .len(),
            1
        );
    }

    #[test]
    fn respects_local_date_boundaries_and_does_not_turn_errors_into_empty_results() {
        let conn = fixture();
        let (start, end) = local_day_bounds_ms("2026-10-19").unwrap();
        for (id, at) in [
            ("before", start - 1),
            ("first", start),
            ("last", end),
            ("after", end + 1),
        ] {
            shot(&conn, id, at);
        }
        let page = read_page(&conn, "2026-10-19", None, None).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.items[0].snapshot_id, "first");
        assert_eq!(page.items[1].snapshot_id, "last");
        let empty = read_page(&conn, "2026-10-17", Some(100), None).unwrap();
        assert_eq!(empty.offset, 0);
        assert!(empty.items.is_empty());
        assert_eq!(
            read_page(&conn, "2026-10-17", Some(100), Some(0))
                .unwrap()
                .offset,
            0
        );
        assert!(read_page(&conn, "invalid-date", None, None).is_err());
        let broken = Connection::open_in_memory().unwrap();
        assert!(read_page(&broken, "2026-10-19", None, None).is_err());
    }
}
