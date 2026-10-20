//! Raw WebP outbox and the adapted /v1/frames contract. No legacy bundle cleanup.
//! Queue enrollment is atomic with the snapshot. Network authorization never survives restart.
use super::{
    internal::{InternalConfig, TaskStatus},
    models::SnapshotRow,
    storage::DataPaths,
};
use chrono::{TimeZone, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path, sync::atomic::Ordering, time::Duration};
use tauri::{Emitter, Manager};
pub const MAX_FRAME_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadDraft {
    pub task_id: String,
    pub display_id: u32,
    pub sha256: String,
    pub size_bytes: u64,
}
impl UploadDraft {
    pub fn new(task_id: &str, display: &str, bytes: &[u8]) -> Result<Self, String> {
        uuid::Uuid::parse_str(task_id).map_err(|_| "任务编号无效")?;
        let display_id = display
            .strip_prefix("display-")
            .and_then(|s| s.parse().ok())
            .ok_or("显示器标识无效")?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_FRAME_BYTES {
            return Err("截图大小超出上传限制".into());
        }
        Ok(Self {
            task_id: task_id.into(),
            display_id,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size_bytes: bytes.len() as u64,
        })
    }
}

pub fn insert_draft(
    conn: &Connection,
    row: &SnapshotRow,
    draft: &UploadDraft,
) -> rusqlite::Result<()> {
    // INSERT OR IGNORE for recovery must never hide a different snapshot or queue identity.
    let stored: String = conn.query_row("SELECT session_id || char(10) || file_path || char(10) || captured_at_ms || char(10) || file_size_bytes FROM snapshots WHERE id=?", [&row.id], |r|r.get(0))?;
    if stored
        != format!(
            "{}\n{}\n{}\n{}",
            row.session_id, row.file_path, row.captured_at_ms, row.file_size_bytes
        )
        || draft.size_bytes != row.file_size_bytes as u64
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    conn.execute("INSERT OR IGNORE INTO frame_upload_outbox(snapshot_id,task_id,display_id,sha256,size_bytes) VALUES(?1,?2,?3,?4,?5)",params![row.id,draft.task_id,draft.display_id,draft.sha256,draft.size_bytes as i64])?;
    let matches: bool = conn.query_row("SELECT task_id=?2 AND display_id=?3 AND sha256=?4 AND size_bytes=?5 FROM frame_upload_outbox WHERE snapshot_id=?1", params![row.id,draft.task_id,draft.display_id,draft.sha256,draft.size_bytes as i64],|r|r.get(0))?;
    if !matches {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FrameReceipt {
    pub snapshot_id: String,
    pub object_key: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub verified: bool,
}
#[derive(Debug, Clone)]
pub struct Frame {
    pub task_id: String,
    pub id: String,
    pub path: String,
    pub captured_at_ms: i64,
    pub display_id: u32,
    pub sha256: String,
    pub size_bytes: u64,
    pub attempts: u32,
}
impl Frame {
    fn object_key(&self, cfg: &InternalConfig) -> Result<String, String> {
        let date = Utc
            .timestamp_millis_opt(self.captured_at_ms)
            .single()
            .ok_or("截图时间无效")?
            .format("%Y/%m/%d");
        Ok(format!(
            "timelens/v2/surveys/{}/devices/{}/tasks/{}/{}/{}.webp",
            cfg.upload_survey_id, cfg.upload_device_id, self.task_id, date, self.id
        ))
    }
    fn verify(&self, cfg: &InternalConfig, receipt: &FrameReceipt) -> Result<(), String> {
        if !receipt.verified
            || receipt.snapshot_id != self.id
            || receipt.sha256 != self.sha256
            || receipt.size_bytes != self.size_bytes
            || receipt.object_key != self.object_key(cfg)?
        {
            return Err("服务器截图回执不匹配，本地图片已保留".into());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadStats {
    pub pending: i64,
    pub verified: i64,
    pub oss_accepted: i64,
    pub blocked: i64,
    pub legacy: i64,
    pub last_error: Option<String>,
}
pub fn stats(conn: &Connection, task_id: &str) -> Result<UploadStats, String> {
    let (pending,verified,blocked)=conn.query_row("SELECT COALESCE(SUM(status='pending' AND NOT EXISTS(SELECT 1 FROM oss_direct_receipts d WHERE d.snapshot_id=frame_upload_outbox.snapshot_id)),0), COALESCE(SUM(status='verified'),0), COALESCE(SUM(status='blocked'),0) FROM frame_upload_outbox WHERE task_id=?",[task_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|e|e.to_string())?;
    let legacy=conn.query_row("SELECT COUNT(*) FROM snapshots s LEFT JOIN frame_upload_outbox q ON q.snapshot_id=s.id WHERE q.snapshot_id IS NULL",[],|r|r.get(0)).map_err(|e|e.to_string())?;
    let last_error=conn.query_row("SELECT last_error FROM frame_upload_outbox WHERE task_id=? AND last_error IS NOT NULL ORDER BY next_retry_ms DESC LIMIT 1",[task_id],|r|r.get(0)).unwrap_or(None);
    Ok(UploadStats {
        pending,
        verified,
        oss_accepted: conn.query_row("SELECT COUNT(*) FROM oss_direct_receipts WHERE task_id=?",[task_id],|r|r.get(0)).map_err(|e|e.to_string())?,
        blocked,
        legacy,
        last_error,
    })
}
pub fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
pub fn destination(cfg: &InternalConfig) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(&cfg.upload_url).map_err(|_| "上传地址无效")?;
    if u.scheme() != "https"
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err("逐帧服务器地址须为 HTTPS 根地址，不包含路径、账号或查询参数".into());
    }
    if !valid_id(&cfg.upload_survey_id) || !valid_id(&cfg.upload_device_id) {
        return Err("请配置有效的调研编号和设备编号".into());
    }
    if !cfg.domestic_server_confirmed {
        return Err("请先确认国内服务器目的地".into());
    }
    Ok(u)
}
pub fn read_token(root: &Path) -> Result<String, String> {
    let value = if let Ok(value) = std::env::var("TIMELENS_UPLOAD_TOKEN") {
        value
    } else {
        let p = root.join("upload-token");
        let meta = fs::symlink_metadata(&p).map_err(|_| "尚未配置上传凭证")?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 {
            return Err("上传凭证文件无效".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
                return Err("上传凭证文件须仅当前用户可读写（0600）".into());
            }
        }
        fs::read_to_string(p).map_err(|_| "无法读取上传凭证")?
    };
    let value = value.trim();
    if value.len() < 32 || value.len() > 4096 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("上传凭证须为至少32字符且无空白的令牌".into());
    }
    Ok(value.into())
}
pub fn frame_bytes(paths: &DataPaths, frame: &Frame) -> Result<Vec<u8>, String> {
    uuid::Uuid::parse_str(&frame.id).map_err(|_| "截图编号无效")?;
    let p = Path::new(&frame.path);
    let meta = fs::symlink_metadata(p).map_err(|_| "待传截图缺失")?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() != frame.size_bytes
        || frame.size_bytes > MAX_FRAME_BYTES
        || frame.size_bytes < 12
    {
        return Err("待传截图大小或类型无效".into());
    }
    let base = paths.shots_dir.canonicalize().map_err(|_| "截图库不可用")?;
    let actual = p.canonicalize().map_err(|_| "截图路径无效")?;
    if !actual.starts_with(&base)
        || actual.extension().and_then(|s| s.to_str()) != Some("webp")
        || !actual
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with(&format!("{}_", frame.id)))
    {
        return Err("截图路径不在本任务管理目录内".into());
    }
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut bytes = Vec::new();
    opts.open(p)
        .map_err(|_| "无法读取截图")?
        .take(MAX_FRAME_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取截图")?;
    if bytes.len() as u64 != frame.size_bytes
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WEBP"
        || format!("{:x}", Sha256::digest(&bytes)) != frame.sha256
    {
        return Err("截图校验失败，本地数据已保留".into());
    }
    Ok(bytes)
}
fn send_with_client(
    client: &reqwest::blocking::Client,
    base: &reqwest::Url,
    cfg: &InternalConfig,
    token: &str,
    frame: &Frame,
    bytes: Vec<u8>,
) -> Result<FrameReceipt, String> {
    let url = base
        .join(&format!("v1/frames/{}", frame.id))
        .map_err(|_| "上传地址无效")?;
    let captured = Utc
        .timestamp_millis_opt(frame.captured_at_ms)
        .single()
        .ok_or("截图时间无效")?
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut response = client
        .post(url)
        .bearer_auth(token)
        .header("Content-Type", "image/webp")
        .header("X-TimeLens-Task",&frame.task_id)
        .header("X-TimeLens-Archive-End",&cfg.archive_end_at)
        .header("X-TimeLens-Survey", &cfg.upload_survey_id)
        .header("X-TimeLens-Device", &cfg.upload_device_id)
        .header("X-TimeLens-Captured-At", captured)
        .header("X-TimeLens-Sha256", &frame.sha256)
        .header("X-TimeLens-Size-Bytes", frame.size_bytes)
        .header("X-TimeLens-Capture-Group-ID", &frame.id)
        .header("X-TimeLens-Group-Elapsed-Ms", 0)
        .header("X-TimeLens-Expected-Displays", 1)
        .header("X-TimeLens-Display-ID", frame.display_id)
        .body(bytes)
        .send()
        .map_err(|_| "逐帧上传连接失败，可重试；本地图片已保留")?;
    if !response.status().is_success() {
        return Err(format!(
            "逐帧服务返回 HTTP {}，本地图片已保留",
            response.status().as_u16()
        ));
    }
    let mut body = Vec::new();
    response
        .by_ref()
        .take(8193)
        .read_to_end(&mut body)
        .map_err(|_| "无法读取上传回执")?;
    if body.len() > 8192 {
        return Err("上传回执过大".into());
    }
    let receipt: FrameReceipt =
        serde_json::from_slice(&body).map_err(|_| "上传回执格式无效，本地图片已保留")?;
    frame.verify(cfg, &receipt)?;
    Ok(receipt)
}
pub(super) fn db(paths: &DataPaths) -> Result<Connection, String> {
    let c = Connection::open(&paths.db_path).map_err(|e| e.to_string())?;
    c.busy_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    c.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
        .map_err(|e| e.to_string())?;
    Ok(c)
}
pub fn retry_delay_ms(attempts: u32) -> i64 {
    (2000_i64.saturating_mul(1_i64 << attempts.min(9))).min(900_000)
}
fn next(conn: &Connection, task: &str) -> Result<Option<Frame>, String> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT s.id,s.file_path,s.captured_at_ms,q.display_id,q.sha256,q.size_bytes,q.attempts,q.task_id FROM frame_upload_outbox q JOIN snapshots s ON s.id=q.snapshot_id WHERE q.task_id=?1 AND q.status='pending' AND q.next_retry_ms<=?2 ORDER BY s.captured_at_ms LIMIT 1",params![task,Utc::now().timestamp_millis()],|r|Ok(Frame{task_id:r.get(7)?,id:r.get(0)?,path:r.get(1)?,captured_at_ms:r.get(2)?,display_id:r.get(3)?,sha256:r.get(4)?,size_bytes:r.get::<_,i64>(5)? as u64,attempts:r.get(6)?})).optional().map_err(|e|e.to_string())
}
pub(super) fn bind(conn: &Connection, frame: &Frame, cfg: &InternalConfig) -> Result<(), String> {
    conn.execute("UPDATE frame_upload_outbox SET destination=?2,survey_id=?3,device_id=?4 WHERE snapshot_id=?1 AND destination IS NULL",params![frame.id,cfg.upload_url,cfg.upload_survey_id,cfg.upload_device_id]).map_err(|e|e.to_string())?;
    let same:bool=conn.query_row("SELECT destination=?2 AND survey_id=?3 AND device_id=?4 FROM frame_upload_outbox WHERE snapshot_id=?1",params![frame.id,cfg.upload_url,cfg.upload_survey_id,cfg.upload_device_id],|r|r.get(0)).map_err(|e|e.to_string())?;
    if !same {
        return Err("截图已绑定其他上传目的地或设备，请人工核对；未发送图片".into());
    }
    Ok(())
}

#[derive(Deserialize)]
struct RemoteArchive {
    task_id:String,survey:String,device:String,planned_end_ms:i64,
    effective_end_ms:i64,deadline_ms:i64,deleted:i64,
}
fn archive_request(client:&reqwest::blocking::Client,base:&reqwest::Url,cfg:&InternalConfig,token:&str,task:&str,suffix:&str,post:bool)->Result<reqwest::blocking::RequestBuilder,String>{
    uuid::Uuid::parse_str(task).map_err(|_|"归档任务无效")?;
    let url=base.join(&format!("v1/archives/{task}{suffix}")).map_err(|_|"归档地址无效")?;
    let r=if post {client.post(url)} else {client.get(url)};
    Ok(r.bearer_auth(token).header("X-TimeLens-Survey",&cfg.upload_survey_id).header("X-TimeLens-Device",&cfg.upload_device_id).header("X-TimeLens-Archive-End",&cfg.archive_end_at))
}
fn archive_json(mut response:reqwest::blocking::Response)->Result<serde_json::Value,String>{
    if !response.status().is_success(){return Err(format!("归档服务返回 HTTP {}",response.status().as_u16()))}
    let mut bytes=Vec::new();response.by_ref().take(524289).read_to_end(&mut bytes).map_err(|_|"归档响应读取失败")?;
    if bytes.len()>524288{return Err("归档清单响应过大".into())}
    serde_json::from_slice(&bytes).map_err(|_|"归档响应无效".into())
}
fn validate_remote(value:&serde_json::Value,cfg:&InternalConfig,task:&str)->Result<RemoteArchive,String>{
    let r:RemoteArchive=serde_json::from_value(value.clone()).map_err(|_|"归档状态格式无效")?;
    if r.task_id!=task||r.survey!=cfg.upload_survey_id||r.device!=cfg.upload_device_id||r.planned_end_ms!=super::archive::end_ms(cfg)?||r.effective_end_ms<=0||r.effective_end_ms>r.planned_end_ms||r.deadline_ms!=r.effective_end_ms.checked_add(super::archive::RETENTION_MS).ok_or("删除截止无效")?||r.deleted!=0||Utc::now().timestamp_millis()>=r.deadline_ms{
        return Err("服务器归档身份或截止不匹配".into())
    }Ok(r)
}
fn register_archive(client:&reqwest::blocking::Client,base:&reqwest::Url,cfg:&InternalConfig,token:&str,task:&str)->Result<(),String>{
    let v=archive_json(archive_request(client,base,cfg,token,task,"/register",true)?.send().map_err(|_|"归档登记连接失败")?)?;
    validate_remote(&v,cfg,task)?;Ok(())
}
fn finish_archive(client:&reqwest::blocking::Client,base:&reqwest::Url,cfg:&InternalConfig,token:&str,task:&str,ended:i64,root:&Path)->Result<RemoteArchive,String>{
    let ended=ended.min(super::archive::end_ms(cfg)?);
    let ended=Utc.timestamp_millis_opt(ended).single().ok_or("结束时间无效")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    let v=archive_json(archive_request(client,base,cfg,token,task,"/finish",true)?.json(&serde_json::json!({"ended_at":ended})).send().map_err(|_|"归档结束时间同步失败，将重试")?)?;
    let state=validate_remote(&v,cfg,task)?;
    super::archive::accept_end(root,task,state.effective_end_ms,state.deadline_ms)?;
    Ok(state)
}
/// Only lifecycle metadata for an already explicitly enrolled task; never drains raw frames.
pub fn sync_end_metadata(paths:&super::storage::DataPaths,cfg:&InternalConfig,p:&super::internal::TaskProgress)->Result<(),String>{
    let Some(b)=super::archive::load(&paths.root)? else{return Ok(())};
    if b.task_id!=p.task_id || b.upload_url!=cfg.upload_url || b.survey_id!=cfg.upload_survey_id || b.device_id!=cfg.upload_device_id{return Err("归档结束同步身份不匹配".into())}
    super::archive::ensure_live(&paths.root,&p.task_id,Utc::now().timestamp_millis())?;
    let base=destination(cfg)?;
    let client=reqwest::blocking::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(30)).build().map_err(|_|"无法创建结束同步连接")?;
    finish_archive(&client,&base,cfg,&read_token(&paths.root)?,&p.task_id,p.ended_at_ms.ok_or("任务未结束")?,&paths.root)?;
    Ok(())
}
fn reconcile_archive(client:&reqwest::blocking::Client,base:&reqwest::Url,cfg:&InternalConfig,token:&str,task:&str,ended:i64,conn:&Connection,root:&Path)->Result<(),String>{
    let ended=ended.min(super::archive::end_ms(cfg)?);
    let ended=Utc.timestamp_millis_opt(ended).single().ok_or("结束时间无效")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    let finish=archive_json(archive_request(client,base,cfg,token,task,"/finish",true)?.json(&serde_json::json!({"ended_at":ended})).send().map_err(|_|"归档结案连接失败")?)?;
    let final_state=validate_remote(&finish,cfg,task)?;
    let mut local:std::collections::HashMap<String,(String,i64)>=conn.prepare("SELECT snapshot_id,sha256,size_bytes FROM frame_upload_outbox WHERE task_id=? AND status='verified'").map_err(|e|e.to_string())?
        .query_map([task],|r|Ok((r.get::<_,String>(0)?,(r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))).map_err(|e|e.to_string())?.collect::<rusqlite::Result<_>>().map_err(|e|e.to_string())?;
    let summary=stats(conn,task)?;
    if summary.pending!=0||summary.blocked!=0{return Err("仍有未确认图片，不能完成对账".into())}
    let remote=archive_json(archive_request(client,base,cfg,token,task,"/reconcile",false)?.send().map_err(|_|"云端对象对账连接失败")?)?;
    if remote["missing"]!=0 || remote["unexpected"]!=0 || remote["frame_count"].as_u64()!=Some(local.len() as u64){return Err("OSS对象数量不匹配，不能完成归档".into())}
    let mut after=String::new();let mut seen=std::collections::HashSet::new();
    loop {
        super::archive::ensure_live(root,task,Utc::now().timestamp_millis())?;
        let page=archive_json(archive_request(client,base,cfg,token,task,"/manifest",false)?.query(&[("after",&after)]).send().map_err(|_|"云端清单读取失败")?)?;
        let state=validate_remote(&page["archive"],cfg,task)?;
        if state.deadline_ms!=final_state.deadline_ms{return Err("对账过程中截止发生变化，请重新核对".into())}
        let frames=page["frames"].as_array().ok_or("云端清单无效")?;
        if frames.len()>500{return Err("云端清单分页过大".into())}
        for item in frames{
            let id=item["snapshot_id"].as_str().ok_or("云端截图编号无效")?;
            if !seen.insert(id.to_string()){return Err("云端清单重复截图".into())}
            let (sha,size)=local.remove(id).ok_or("云端包含本地任务之外的图片")?;
            if item["task_id"]!=task||item["survey"]!=cfg.upload_survey_id||item["device"]!=cfg.upload_device_id||item["sha256"]!=sha||item["size_bytes"]!=size{return Err("云端图片对账不匹配".into())}
        }
        match page["next_after"].as_str(){
            None if page["next_after"].is_null()=>break,
            Some(next) if next>after.as_str() && uuid::Uuid::parse_str(next).is_ok()=>after=next.to_string(),
            _=>return Err("云端清单游标无效".into())
        }
    }
    if !local.is_empty(){return Err("云端缺少部分本地图片，请检查后重试".into())}
    super::archive::mark_reconciled(root,task,final_state.effective_end_ms,final_state.deadline_ms)
}

fn permanent_failure(error: &str) -> bool {
    let status = error
        .split("HTTP ")
        .nth(1)
        .and_then(|v| v.get(..3))
        .and_then(|v| v.parse::<u16>().ok());
    error.contains("不匹配")
        || error.contains("校验失败")
        || error.contains("绑定其他")
        || status.is_some_and(|code| (400..500).contains(&code) && ![408, 429].contains(&code))
}
fn worker(app: tauri::AppHandle, state: crate::AppState, cfg: InternalConfig, task: String) {
    let result = (|| -> Result<(), String> {
        let base = destination(&cfg)?;
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "无法创建上传连接")?;
        let conn = db(&state.0.paths)?;
        register_archive(&client,&base,&cfg,&read_token(&state.0.paths.root)?,&task)?;
        while state.0.internal.frame_upload_active.load(Ordering::SeqCst) {
            super::archive::ensure_live(&state.0.paths.root,&task,Utc::now().timestamp_millis())?;
            let progress=state.0.internal.progress.lock().clone();
            super::archive::shorten(&state.0.paths.root,&progress)?;
            if !progress.needs_flush && progress.ended_at_ms.is_some() && super::archive::load(&state.0.paths.root)?.is_some_and(|b| !b.end_synced) {
                finish_archive(&client,&base,&cfg,&read_token(&state.0.paths.root)?,&task,progress.ended_at_ms.unwrap(),&state.0.paths.root)?;
            }
            state.0.writer.check_health()?;
            if state.0.internal.progress.lock().integrity_error.is_some() {
                return Err("数据完整性待审核，已停止上传".into());
            }
            if let Some(frame) = next(&conn, &task)? {
                let result = bind(&conn, &frame, &cfg)
                    .and_then(|_| frame_bytes(&state.0.paths, &frame))
                    .and_then(|bytes| {
                        if !state.0.internal.frame_upload_active.load(Ordering::SeqCst) {
                            return Err("后台上传已停止".into());
                        }
                        let token = read_token(&state.0.paths.root)?;
                        send_with_client(&client, &base, &cfg, &token, &frame, bytes)
                    });
                match result {
                    Ok(receipt) => {
                        super::archive::ensure_live(&state.0.paths.root,&task,Utc::now().timestamp_millis())?;
                        conn.execute("UPDATE frame_upload_outbox SET status='verified',receipt_json=?2,last_error=NULL,next_retry_ms=0 WHERE snapshot_id=?1",params![frame.id,serde_json::to_string(&receipt).map_err(|e|e.to_string())?]).map_err(|e|e.to_string())?;
                    }
                    Err(error) => {
                        let blocked = permanent_failure(&error);
                        conn.execute("UPDATE frame_upload_outbox SET status=?2,attempts=attempts+1,next_retry_ms=?3,last_error=?4 WHERE snapshot_id=?1",params![frame.id,if blocked{"blocked"}else{"pending"},Utc::now().timestamp_millis()+retry_delay_ms(frame.attempts),error]).map_err(|e|e.to_string())?;
                        let revoked = error.contains("HTTP 401") || error.contains("HTTP 403");
                        state.0.internal.edit_progress(|p| {
                            p.last_error = Some(error);
                            Ok(())
                        })?;
                        if revoked {
                            return Err("上传凭证被拒绝，已停止后台上传；请核对后重新确认".into());
                        }
                    }
                }
            } else {
                let summary = stats(&conn, &task)?;
                let progress = state.0.internal.progress.lock().clone();
                let ended = !progress.needs_flush
                    && matches!(
                        progress.status,
                        TaskStatus::Completed | TaskStatus::Uploaded
                    );
                if ended {
                    if summary.pending == 0 && summary.blocked == 0 {
                        super::archive::shorten(&state.0.paths.root,&progress)?;
                        let end=progress.ended_at_ms.ok_or("旧任务没有明确结束时间，需人工核对")?;
                        reconcile_archive(&client,&base,&cfg,&read_token(&state.0.paths.root)?,&task,end,&conn,&state.0.paths.root)?;
                        state.0.internal.edit_progress(|p| {
                            p.status = TaskStatus::Uploaded;
                            p.last_error = None;
                            Ok(())
                        })?;
                        break;
                    }
                    if summary.pending == 0 {
                        return Err("部分截图上传被拒绝，请核对配置后重试；本地数据已保留".into());
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            let _ = app.emit(
                "internal_task_changed",
                state.0.internal.progress.lock().clone(),
            );
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = state.0.internal.edit_progress(|p| {
            if p.status == TaskStatus::Uploading {
                p.status = TaskStatus::Completed;
            }
            p.last_error = Some(error);
            Ok(())
        });
    }
    finish_run(&state.0.internal);
    let _ = app.emit(
        "internal_task_changed",
        state.0.internal.progress.lock().clone(),
    );
}
pub(super) fn start_run(control: &super::internal::InternalControl) -> Result<(), String> {
    if control
        .frame_upload_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("上一轮后台上传仍在结束，请稍后再试".into());
    }
    control.frame_upload_active.store(true, Ordering::SeqCst);
    Ok(())
}
pub(super) fn finish_run(control: &super::internal::InternalControl) {
    control.frame_upload_active.store(false, Ordering::SeqCst);
    // Release last, so an old worker can never switch off its successor.
    control.frame_upload_running.store(false, Ordering::SeqCst);
}
pub fn stop(control: &super::internal::InternalControl) {
    let _gate = control.gate.write();
    control.frame_upload_active.store(false, Ordering::SeqCst);
}
pub fn begin(
    app: &tauri::AppHandle,
    expected: &super::desktop::UploadConfirmation,
) -> Result<(), String> {
    let state = app.state::<crate::AppState>().inner().clone();
    if state.0.internal.config.read().upload_mode == "ossDirect" { return super::oss_direct::begin(app,expected); }
    let _gate = state.0.internal.gate.read();
    let cfg = state.0.internal.config.read().clone();
    let p = state.0.internal.progress.lock().clone();
    super::desktop::validate_confirmation(
        expected,
        &cfg,
        &p,
        read_token(&state.0.paths.root).is_ok(),
    )?;
    if p.receipt.is_some() || p.stored_bundle.is_some() || p.bundle_path.is_some() {
        return Err("此任务已有旧版加密包或回执，请先人工核对；未转换或删除旧数据".into());
    }
    if p.integrity_error.is_some() || (p.needs_flush && p.status != TaskStatus::Recording) {
        return Err("数据保存或完整性检查未完成".into());
    }
    destination(&cfg)?;
    super::archive::bind(&state.0.paths,&cfg,&p)?;
    start_run(&state.0.internal)?;
    let reset=db(&state.0.paths).and_then(|c|c.execute("UPDATE frame_upload_outbox SET status='pending',next_retry_ms=0,last_error=NULL WHERE task_id=? AND status!='verified'",[&p.task_id]).map(|_|()).map_err(|e|e.to_string()));
    if let Err(e) = reset {
        finish_run(&state.0.internal);
        return Err(e);
    }
    drop(_gate);
    let handle = app.clone();
    std::thread::spawn(move || worker(handle, state, cfg, p.task_id));
    Ok(())
}
pub fn on_task_finished(_app: &tauri::AppHandle) -> Result<(), String> {
    Ok(())
} // Only an explicitly authorized worker may drain the queue.

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_does_not_release_worker_lease_until_request_returns() {
        use std::sync::{atomic::AtomicBool, Arc};
        let t = tempfile::tempdir().unwrap();
        let c = super::super::internal::InternalControl::load(
            t.path(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        start_run(&c).unwrap();
        c.frame_upload_active.store(false, Ordering::SeqCst);
        assert!(start_run(&c).is_err());
        assert!(c.save_config(InternalConfig::default()).is_err());
        finish_run(&c);
        start_run(&c).unwrap();
        assert!(c.frame_upload_active.load(Ordering::SeqCst));
        finish_run(&c);
        let restored = super::super::internal::InternalControl::load(
            t.path(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(!restored.frame_upload_running.load(Ordering::SeqCst));
        assert!(!restored.frame_upload_active.load(Ordering::SeqCst));
    }
    #[test]
    fn stop_serializes_with_the_middle_of_starting_a_worker() {
        use std::sync::{atomic::AtomicBool, mpsc, Arc};
        let t = tempfile::tempdir().unwrap();
        let c = super::super::internal::InternalControl::load(
            t.path(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let gate = c.gate.read();
        c.frame_upload_running.store(true, Ordering::SeqCst);
        let copy = c.clone();
        let (tx, rx) = mpsc::channel();
        let done = std::thread::spawn(move || {
            tx.send(()).unwrap();
            stop(&copy);
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        c.frame_upload_active.store(true, Ordering::SeqCst);
        drop(gate);
        done.join().unwrap();
        assert!(!c.frame_upload_active.load(Ordering::SeqCst));
        assert!(c.frame_upload_running.load(Ordering::SeqCst));
        finish_run(&c);
    }
    #[test]
    fn temporary_http_failures_retry_but_rejections_require_review() {
        for code in [408, 429, 500, 502, 503] {
            assert!(!permanent_failure(&format!(
                "逐帧服务返回 HTTP {code}，本地图片已保留"
            )));
        }
        for code in [400, 401, 403, 409, 413, 415, 422] {
            assert!(permanent_failure(&format!(
                "逐帧服务返回 HTTP {code}，本地图片已保留"
            )));
        }
    }

    #[cfg(feature = "frame-contract-tests")]
    #[test]
    fn rust_to_python_receiver_to_fake_oss_roundtrip() {
        use std::{
            io::BufRead,
            process::{Command, Stdio},
        };
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let server = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../upload-server");
        let python = std::env::var("TIMELENS_TEST_PYTHON")
            .expect("Set TIMELENS_TEST_PYTHON to the isolated test venv's Python");
        let mut child = ChildGuard(
            Command::new(python)
                .arg("tests/contract_server.py")
                .current_dir(&server)
                .env("PYTHONPATH", &server)
                .env_remove("OSS_ACCESS_KEY_ID").env_remove("OSS_ACCESS_KEY_SECRET").env_remove("OSS_SECURITY_TOKEN")
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let output = child.0.stdout.take().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(output)
            .read_line(&mut line)
            .unwrap();
        let base = reqwest::Url::parse(line.trim()).unwrap();
        assert_eq!(base.host_str(), Some("127.0.0.1"));
        let bytes =
            webp::Encoder::from_rgb(&[0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255], 2, 2)
                .encode(85.)
                .to_vec();
        let mut cfg = InternalConfig::default();
        cfg.upload_survey_id = "fixture-survey".into();
        cfg.upload_device_id = "fixture-device".into();
        cfg.archive_end_at="2099-12-31T17:00:00Z".into();
        let frame = Frame {
            task_id:"11111111-1111-4111-8111-111111111111".into(),
            id: uuid::Uuid::new_v4().to_string(),
            path: String::new(),
            captured_at_ms: 1000,
            display_id: 7,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            size_bytes: bytes.len() as u64,
            attempts: 0,
        };
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let token = "fixture-only-token-not-real-credentials-12345";
        let first = send_with_client(&client, &base, &cfg, token, &frame, bytes.clone()).unwrap();
        assert_eq!(
            first,
            send_with_client(&client, &base, &cfg, token, &frame, bytes.clone()).unwrap()
        );
        let state: serde_json::Value = client
            .get(base.join("fixture-state").unwrap())
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(state["put_count"], 1);
        let readback=archive_request(&client,&base,&cfg,token,&frame.task_id,&format!("/frames/{}",frame.id),false).unwrap().send().unwrap();
        assert_eq!(readback.status().as_u16(),200);
        assert_eq!(readback.bytes().unwrap().as_ref(),bytes.as_slice());
        let t=tempfile::tempdir().unwrap();
        let mut conn=Connection::open_in_memory().unwrap();
        super::super::storage::migrations::run_migrations(&mut conn).unwrap();
        conn.execute("INSERT INTO window_sessions(id,app_name,window_title,start_ms,end_ms,duration_ms,is_active) VALUES('s','fixture','synthetic',1,2,1,0)",[]).unwrap();
        conn.execute("INSERT INTO snapshots(id,session_id,file_path,captured_at_ms,file_size_bytes,trigger_type) VALUES(?1,'s','fixture',1000,?2,'poll')",params![frame.id,frame.size_bytes as i64]).unwrap();
        conn.execute("INSERT INTO frame_upload_outbox(snapshot_id,task_id,display_id,sha256,size_bytes,status) VALUES(?1,?2,?3,?4,?5,'verified')",params![frame.id,frame.task_id,frame.display_id,frame.sha256,frame.size_bytes as i64]).unwrap();
        let end=Utc::now().timestamp_millis();
        let binding=super::super::archive::ArchiveBinding{upload_mode:"receiver".into(),task_id:frame.task_id.clone(),upload_url:cfg.upload_url.clone(),survey_id:cfg.upload_survey_id.clone(),device_id:cfg.upload_device_id.clone(),planned_end_ms:super::super::archive::end_ms(&cfg).unwrap(),effective_end_ms:end,deadline_ms:end+super::super::archive::RETENTION_MS,reconciled:false,deleted:false,end_synced:false,target_ms:None};
        super::super::internal::write_json_atomic(&t.path().join("archive-binding.json"),&binding).unwrap();
        reconcile_archive(&client,&base,&cfg,token,&frame.task_id,end,&conn,t.path()).unwrap();
        assert!(super::super::archive::load(t.path()).unwrap().unwrap().reconciled);
        let mut changed = frame.clone();
        changed.captured_at_ms += 86_400_000;
        assert!(
            send_with_client(&client, &base, &cfg, token, &changed, bytes.clone())
                .unwrap_err()
                .contains("HTTP 409")
        );
        assert!(
            send_with_client(&client, &base, &cfg, "wrong-token", &frame, bytes)
                .unwrap_err()
                .contains("HTTP 401")
        );
    }
    #[test]
    fn endpoint_and_backoff_boundaries() {
        let mut c = InternalConfig::default();
        c.domestic_server_confirmed = true;
        c.upload_survey_id = "fixture-survey".into();
        c.upload_device_id = "fixture-device".into();
        for bad in [
            "http://example.org/",
            "https://u:p@example.org/",
            "https://example.org/upload",
            "https://example.org/?q=1",
            "https://example.org/#x",
        ] {
            c.upload_url = bad.into();
            assert!(destination(&c).is_err());
        }
        c.upload_url = "https://internal.example/".into();
        assert!(destination(&c).is_ok());
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"x".repeat(65)));
        assert!(valid_id("a-._01"));
        assert_eq!(retry_delay_ms(0), 2000);
        assert_eq!(retry_delay_ms(20), 900000);
    }
    #[test]
    fn receipts_bind_every_field() {
        let mut c = InternalConfig::default();
        c.upload_survey_id = "survey".into();
        c.upload_device_id = "device".into();
        let f = Frame {
            task_id:"11111111-1111-4111-8111-111111111111".into(),
            id: uuid::Uuid::new_v4().to_string(),
            path: String::new(),
            captured_at_ms: 1000,
            display_id: 1,
            sha256: "a".repeat(64),
            size_bytes: 42,
            attempts: 0,
        };
        let r = FrameReceipt {
            snapshot_id: f.id.clone(),
            object_key: f.object_key(&c).unwrap(),
            sha256: f.sha256.clone(),
            size_bytes: 42,
            verified: true,
        };
        assert!(f.verify(&c, &r).is_ok());
        let mut x = r.clone();
        x.size_bytes += 1;
        assert!(f.verify(&c, &x).is_err());
        let mut x = r.clone();
        x.verified = false;
        assert!(f.verify(&c, &x).is_err());
        let mut x = r.clone();
        x.object_key.push('x');
        assert!(f.verify(&c, &x).is_err());
        let mut x = r.clone();
        x.sha256 = "b".repeat(64);
        assert!(f.verify(&c, &x).is_err());
        let mut x = r;
        x.snapshot_id = "other".into();
        assert!(f.verify(&c, &x).is_err());
    }
    #[test]
    fn fixture_http_contract_and_mismatch_preserves_local_file() {
        use std::io::Write;
        use std::net::TcpListener;
        let t = tempfile::tempdir().unwrap();
        let p = DataPaths {
            root: t.path().into(),
            data_dir: t.path().join("data"),
            shots_dir: t.path().join("data/shots"),
            db_path: t.path().join("data/db.sqlite"),
            exports_dir: t.path().join("exports"),
        };
        p.ensure_dirs().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = p.shots_dir.join(format!("{id}_display-1_poll.webp"));
        let enc = webp::Encoder::from_rgb(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255], 2, 2);
        let bytes = enc.encode(85.0).to_vec();
        fs::write(&path, &bytes).unwrap();
        let draft =
            UploadDraft::new(&uuid::Uuid::new_v4().to_string(), "display-1", &bytes).unwrap();
        let f = Frame {
            task_id:"11111111-1111-4111-8111-111111111111".into(),
            id: id.clone(),
            path: path.to_string_lossy().into(),
            captured_at_ms: 1000,
            display_id: 1,
            sha256: draft.sha256,
            size_bytes: bytes.len() as u64,
            attempts: 0,
        };
        assert_eq!(frame_bytes(&p, &f).unwrap(), bytes);
        let mut c = InternalConfig::default();
        c.upload_survey_id = "fixture-survey".into();
        c.upload_device_id = "fixture-device".into();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base =
            reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let receipt = FrameReceipt {
            snapshot_id: id,
            object_key: f.object_key(&c).unwrap(),
            sha256: f.sha256.clone(),
            size_bytes: f.size_bytes,
            verified: true,
        };
        let body = serde_json::to_string(&receipt).unwrap();
        let expected = bytes.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut received = Vec::new();
            let mut block = [0; 4096];
            let boundary = loop {
                let n = stream.read(&mut block).unwrap();
                assert!(n > 0);
                received.extend_from_slice(&block[..n]);
                if let Some(i) = received.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&received[..boundary]).to_ascii_lowercase();
            assert!(head.contains("x-timelens-survey: fixture-survey"));
            assert!(head.contains("x-timelens-expected-displays: 1"));
            while received.len() - boundary < expected.len() {
                let n = stream.read(&mut block).unwrap();
                assert!(n > 0);
                received.extend_from_slice(&block[..n]);
            }
            assert_eq!(&received[boundary..], expected);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        assert_eq!(
            send_with_client(
                &client,
                &base,
                &c,
                "fixture-only-token-not-real-credentials",
                &f,
                bytes
            )
            .unwrap(),
            receipt
        );
        server.join().unwrap();
        assert!(path.exists());
        fs::write(&path, b"tampered").unwrap();
        assert!(frame_bytes(&p, &f).is_err());
        assert!(path.exists());
    }
}
