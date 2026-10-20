//! Immutable, paged inventory. The index is posted last, so partial uploads are not a complete task.
use super::{
    frame_upload::{self, Frame},
    internal::{InternalConfig, TaskProgress},
    oss_direct::{self, Grant},
};
use chrono::Utc;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::Path, time::Duration};
static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
pub(super) fn retryable(error: &str) -> bool {
    if error == "清单上传连接失败，将重试" {
        return true;
    }
    error
        .split("HTTP ")
        .nth(1)
        .and_then(|s| s.get(..3))
        .and_then(|s| s.parse::<u16>().ok())
        .is_some_and(|status| status == 408 || status == 429 || (500..600).contains(&status))
}
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Cache {
    task: String,
    source_hash: String,
    pages: BTreeMap<String, String>,
    #[serde(default)]
    last_published_ms: i64,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "无法创建清单连接".into())
}
fn post(
    client: &reqwest::blocking::Client,
    g: &Grant,
    key: &str,
    task: &str,
    bytes: &[u8],
) -> Result<(), String> {
    post_to(client, &g.endpoint, g, key, task, bytes)
}
pub(super) fn post_to(
    client: &reqwest::blocking::Client,
    url: &str,
    g: &Grant,
    key: &str,
    task: &str,
    bytes: &[u8],
) -> Result<(), String> {
    g.validate(&g.device_id, Utc::now().timestamp_millis())?;
    if bytes.is_empty()
        || bytes.len() > 20 * 1024 * 1024
        || !key.starts_with(&format!("{}manifests/{task}/", g.prefix))
    {
        return Err("清单范围或大小无效".into());
    }
    let mut fields = g
        .manifest_fields
        .clone()
        .ok_or("旧授权缺少清单权限，请管理员重新签发")?;
    fields.insert("key".into(), key.into());
    for (k, v) in [
        ("sha256", hash(bytes)),
        ("task", task.into()),
        ("captured-at", Utc::now().timestamp_millis().to_string()),
        ("display", "0".into()),
    ] {
        fields.insert(format!("x-oss-meta-timelens-{k}"), v);
    }
    let boundary = format!("timelens-{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"manifest.json\"\r\nContent-Type: application/json\r\n\r\n").as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let r = client
        .post(url)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .map_err(|_| "清单上传连接失败，将重试")?;
    if r.status().as_u16() != 204 {
        return Err(format!("清单上传 HTTP {}，未确认成功", r.status().as_u16()));
    }
    if r.headers()
        .get("x-oss-hash-crc64ecma")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        != Some(oss_direct::crc64(bytes))
        || r.headers().contains_key("x-oss-version-id")
        || r.headers().get("x-oss-request-id").is_none()
    {
        return Err("清单回执校验失败，将重试".into());
    }
    Ok(())
}
fn inventory(conn: &Connection, g: &Grant, p: &TaskProgress) -> Result<Vec<Value>, String> {
    let mut stmt=conn.prepare("SELECT q.snapshot_id,s.captured_at_ms,q.display_id,q.sha256,q.size_bytes FROM frame_upload_outbox q LEFT JOIN snapshots s ON s.id=q.snapshot_id WHERE q.task_id=? ORDER BY q.rowid").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map([&p.task_id], |r| {
            Ok(Frame {
                id: r.get(0)?,
                captured_at_ms: r.get(1)?,
                display_id: r.get(2)?,
                sha256: r.get(3)?,
                size_bytes: r.get::<_, i64>(4)? as u64,
                task_id: p.task_id.clone(),
                path: String::new(),
                attempts: 0,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    for row in rows {
        let f = row.map_err(|e| e.to_string())?;
        frames.push(json!({"snapshotId":f.id,"capturedAtMs":f.captured_at_ms,"displayId":f.display_id,"sha256":f.sha256,"sizeBytes":f.size_bytes,"objectKey":oss_direct::object_key(g,&f)?}));
        if frames.len() > 200_000 {
            return Err("清单截图数量超过上限".into());
        }
    }
    Ok(frames)
}
pub fn publish(
    client: &reqwest::blocking::Client,
    g: &Grant,
    conn: &Connection,
    root: &Path,
    p: &TaskProgress,
) -> Result<(), String> {
    publish_using(g, conn, root, p, |key, body| {
        post(client, g, key, &p.task_id, body)
    })
}
fn publish_using(
    g: &Grant,
    conn: &Connection,
    root: &Path,
    p: &TaskProgress,
    mut send: impl FnMut(&str, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let _lock = LOCK.lock();
    // Legacy authorizations continue to send images; cannot claim an administrator inventory.
    if g.manifest_fields.is_none() {
        return Ok(());
    }
    super::archive::ensure_live(root, &p.task_id, Utc::now().timestamp_millis())?;
    let frames = inventory(conn, g, p)?;
    let end = if p.needs_flush { None } else { p.ended_at_ms };
    let source = json!({"format":"timelens-direct-index-v1","taskId":p.task_id,"userId":g.user_id,"deviceId":g.device_id,"archiveEndAt":g.archive_end_at,"endedAtMs":end,"targetMs":p.target_ms,"elapsedMs":p.elapsed_ms,"frameCount":frames.len(),"sizeBytes":frames.iter().map(|f|f["sizeBytes"].as_u64().unwrap_or(0)).sum::<u64>()});
    let source_hash =
        hash(&serde_json::to_vec(&json!([source, frames])).map_err(|e| e.to_string())?);
    let path = root.join("data/oss-manifest-state.json");
    let mut cache: Cache = match fs::symlink_metadata(&path) {
        Ok(m) => {
            if !m.is_file() || m.file_type().is_symlink() || m.len() > 2 * 1024 * 1024 {
                return Err("清单缓存无效".into());
            }
            serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|_| "清单缓存损坏")?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Cache {
            task: p.task_id.clone(),
            ..Default::default()
        },
        Err(e) => return Err(e.to_string()),
    };
    if cache.task != p.task_id {
        return Err("清单缓存属于其他任务".into());
    }
    if cache.source_hash == source_hash {
        return Ok(());
    }
    let prefix = format!("{}manifests/{}/", g.prefix, p.task_id);
    let mut pages = Vec::new();
    let mut used = BTreeMap::new();
    for chunk in frames.chunks(500) {
        let body = serde_json::to_vec(
            &json!({"format":"timelens-direct-page-v1","taskId":p.task_id,"frames":chunk}),
        )
        .map_err(|e| e.to_string())?;
        let digest = hash(&body);
        let key = if let Some(k) = cache.pages.get(&digest) {
            if !k.starts_with(&format!("{prefix}pages/")) {
                return Err("清单缓存目录不匹配".into());
            }
            k.clone()
        } else {
            let key = format!("{prefix}pages/{}-{}.json", digest, uuid::Uuid::new_v4());
            send(&key, &body)?;
            cache.pages.insert(digest.clone(), key.clone());
            super::internal::write_json_atomic(&path, &cache)?;
            key
        };
        used.insert(digest.clone(), key.clone());
        pages.push(json!({"objectKey":key,"sha256":digest,"sizeBytes":body.len(),"frameCount":chunk.len()}));
    }
    let mut index = source;
    index["pages"] = json!(pages);
    let now = Utc::now()
        .timestamp_millis()
        .max(cache.last_published_ms + 1);
    index["publishedAtMs"] = json!(now);
    cache.last_published_ms = now;
    super::internal::write_json_atomic(&path, &cache)?;
    let key = format!("{prefix}indexes/{now:020}-{}.json", uuid::Uuid::new_v4());
    // Publish only after every referenced page has a checked receipt. Lost response -> new immutable key on retry.
    send(
        &key,
        &serde_json::to_vec(&index).map_err(|e| e.to_string())?,
    )?;
    cache.source_hash = source_hash;
    cache.pages = used;
    super::internal::write_json_atomic(&path, &cache)
}
pub fn sync_end(
    paths: &super::storage::DataPaths,
    cfg: &InternalConfig,
    p: &TaskProgress,
) -> Result<(), String> {
    let g = oss_direct::load(&paths.root, cfg)?;
    if g.manifest_fields.is_none() {
        return Err("旧授权未包含清单权限，结束信息尚未同步；请重新签发授权".into());
    }
    publish(&client()?, &g, &frame_upload::db(paths)?, &paths.root, p)?;
    let b = super::archive::load(&paths.root)?.ok_or("归档未绑定")?;
    super::archive::accept_end(&paths.root, &p.task_id, b.effective_end_ms, b.deadline_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventory_includes_pending_blocked_and_receipt_lost_frames() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE snapshots(id TEXT,captured_at_ms INTEGER);CREATE TABLE frame_upload_outbox(snapshot_id TEXT,task_id TEXT,display_id INTEGER,sha256 TEXT,size_bytes INTEGER,status TEXT);").unwrap();
        // The inventory intentionally reads the entire outbox, never the acceptance receipts.
        let task = uuid::Uuid::new_v4().to_string();
        for status in ["pending", "blocked", "verified"] {
            let id = uuid::Uuid::new_v4().to_string();
            c.execute("INSERT INTO snapshots VALUES(?1,1700000000000)", [&id])
                .unwrap();
            c.execute(
                "INSERT INTO frame_upload_outbox VALUES(?1,?2,1,?3,3,?4)",
                rusqlite::params![id, task, "a".repeat(64), status],
            )
            .unwrap();
        }
        let g:Grant=serde_json::from_value(json!({"format":"timelens-oss-post-v1","deviceId":uuid::Uuid::new_v4().to_string(),"userId":"test","bucket":"test-bucket","region":"cn-beijing","endpoint":"https://test-bucket.oss-cn-beijing.aliyuncs.com/","prefix":"test/","issuedAt":"","expiresAt":"","archiveEndAt":"","fields":{},"manifestFields":{}})).unwrap();
        let p:TaskProgress=serde_json::from_value(json!({"taskId":task,"elapsedMs":0,"targetMs":172800000,"status":"ready","lastError":null,"bundlePath":null})).unwrap();
        assert_eq!(inventory(&c, &g, &p).unwrap().len(), 3);
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("data")).unwrap();
        let mut posted = Vec::new();
        assert!(publish_using(&g, &c, temp.path(), &p, |key, body| {
            posted.push((key.to_owned(), body.to_vec()));
            if key.contains("/indexes/") {
                Err("lost index response".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(posted.len(), 2);
        assert!(posted[0].0.contains("/pages/"));
        assert!(posted[1].0.contains("/indexes/"));
        let cache_path = temp.path().join("data/oss-manifest-state.json");
        let mut cache: Cache = serde_json::from_slice(&fs::read(&cache_path).unwrap()).unwrap();
        let previous = cache.last_published_ms + 60000;
        cache.last_published_ms = previous;
        super::super::internal::write_json_atomic(&cache_path, &cache).unwrap();
        posted.clear();
        publish_using(&g, &c, temp.path(), &p, |k, b| {
            posted.push((k.to_owned(), b.to_vec()));
            Ok(())
        })
        .unwrap();
        assert_eq!(posted.len(), 1);
        let index: Value = serde_json::from_slice(&posted[0].1).unwrap();
        assert!(index["publishedAtMs"].as_i64().unwrap() > previous);
        assert_eq!(index["frameCount"], 3);
        assert!(index["endedAtMs"].is_null());
        publish_using(&g, &c, temp.path(), &p, |_, _| {
            panic!("unchanged manifest must not upload")
        })
        .unwrap();
        // Appending a lexically earlier UUID must not invalidate a full historical page.
        for _ in 0..497 {
            let id = uuid::Uuid::new_v4().to_string();
            c.execute("INSERT INTO snapshots VALUES(?1,1700000000001)", [&id])
                .unwrap();
            c.execute(
                "INSERT INTO frame_upload_outbox VALUES(?1,?2,1,?3,3,'pending')",
                rusqlite::params![id, task, "b".repeat(64)],
            )
            .unwrap();
        }
        let before = inventory(&c, &g, &p).unwrap();
        assert_eq!(before.len(), 500);
        let id = "00000000-0000-0000-0000-000000000000";
        c.execute("INSERT INTO snapshots VALUES(?1,1700000000002)", [id])
            .unwrap();
        c.execute(
            "INSERT INTO frame_upload_outbox VALUES(?1,?2,1,?3,3,'pending')",
            rusqlite::params![id, task, "c".repeat(64)],
        )
        .unwrap();
        let after = inventory(&c, &g, &p).unwrap();
        assert_eq!(&after[..500], before.as_slice());
        c.execute("DELETE FROM snapshots", []).unwrap();
        assert!(inventory(&c, &g, &p).is_err());
    }
    #[test]
    fn only_transient_manifest_errors_retry() {
        for e in [
            "清单上传连接失败，将重试",
            "清单上传 HTTP 503，未确认成功",
            "清单上传 HTTP 429，未确认成功",
        ] {
            assert!(retryable(e));
        }
        for e in [
            "清单上传 HTTP 403，未确认成功",
            "清单上传 HTTP 409，未确认成功",
            "清单回执校验失败，将重试",
            "上传授权已过期",
        ] {
            assert!(!retryable(e));
        }
    }
}
