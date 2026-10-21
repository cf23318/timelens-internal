//! Offline-issued, device-scoped POST grants. No administrator Secret reaches the client.
use super::{
    frame_upload::{self, Frame, MAX_FRAME_BYTES},
    internal::{InternalConfig, TaskStatus},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::Path,
    sync::{atomic::Ordering, OnceLock},
    time::Duration,
};
use tauri::{Emitter, Manager};

const GRANT_FILE: &str = "oss-upload-grant.json";
const MAX_GRANT: usize = 65536;
static ID_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn private_read(path: &Path) -> Result<Vec<u8>, String> {
    let m = fs::symlink_metadata(path).map_err(|_| "尚未导入上传授权")?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_GRANT as u64 {
        return Err("授权文件类型或大小无效".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        if m.mode() & 0o077 != 0 || m.uid() != unsafe { libc::geteuid() } {
            return Err("授权文件须仅当前用户可读写".into());
        }
        let mut out = Vec::new();
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|_| "无法读取授权")?
            .take(MAX_GRANT as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|_| "无法读取授权")?;
        if out.len() > MAX_GRANT {
            return Err("授权过大".into());
        }
        Ok(out)
    }
    #[cfg(not(unix))]
    {
        fs::read(path).map_err(|_| "无法读取授权".into())
    }
}

/// Stable for this local profile, independent of study/task and upload credentials.
pub fn device_id(root: &Path) -> Result<String, String> {
    let _lock = ID_LOCK.lock();
    let path = root.join("device-identity.json");
    if fs::symlink_metadata(&path).is_ok() {
        let id: String = serde_json::from_slice(&private_read(&path)?)
            .map_err(|_| "固定设备ID文件损坏，请管理员核对")?;
        if uuid::Uuid::parse_str(&id).is_err() || id.len() != 36 {
            return Err("固定设备ID无效".into());
        }
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    super::internal::write_json_atomic(&path, &id)?;
    Ok(id)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub format: String,
    pub device_id: String,
    pub user_id: String,
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
    pub prefix: String,
    pub issued_at: String,
    pub expires_at: String,
    pub archive_end_at: String,
    pub fields: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_fields: Option<BTreeMap<String, String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    expiration: String,
    conditions: Vec<Value>,
}

fn millis(value: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|d| d.timestamp_millis())
        .map_err(|_| "授权时间无效".into())
}
pub fn prefix(user: &str, device: &str) -> String {
    format!("{}{device}/", shared_prefix(user))
}
fn shared_prefix(user: &str) -> String {
    format!("timelens/direct/v1/users/{user}/devices/")
}
impl Grant {
    pub fn is_shared(&self) -> bool { self.format == "timelens-oss-shared-post-v1" }
    fn bind_device(&mut self, device: &str) -> Result<(), String> {
        if !self.is_shared() { return Ok(()); }
        if (self.device_id != "*" && self.device_id != device)
            || (self.prefix != shared_prefix(&self.user_id) && self.prefix != prefix(&self.user_id, device)) {
            return Err("公用授权的本机绑定无效".into());
        }
        for fields in std::iter::once(&mut self.fields).chain(self.manifest_fields.iter_mut()) {
            let value = fields.get_mut("x-oss-meta-timelens-device").ok_or("授权字段缺失")?;
            if !value.is_empty() && value != device { return Err("公用授权已绑定其他设备".into()); }
            *value = device.into();
        }
        self.device_id = device.into();
        self.prefix = prefix(&self.user_id, device);
        Ok(())
    }
    pub fn parse(raw: &str, device: &str, now: i64) -> Result<Self, String> {
        if raw.len() > MAX_GRANT {
            return Err("授权文件超过64KB".into());
        }
        let mut g: Self = serde_json::from_str(raw)
            .map_err(|_| "上传授权格式无效，不能导入长期密钥或普通OSS配置")?;
        g.bind_device(device)?;
        g.validate(device, now)?;
        Ok(g)
    }
    pub fn fingerprint(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).unwrap_or_default())
        )
    }
    pub fn validate(&self, device: &str, now: i64) -> Result<(), String> {
        self.validate_policy(device, now, false)?;
        if self.manifest_fields.is_some() {
            self.validate_policy(device, now, true)?;
        }
        Ok(())
    }
    fn validate_policy(&self, device: &str, now: i64, manifest: bool) -> Result<(), String> {
        let fields = if manifest {
            self.manifest_fields.as_ref().ok_or("缺少清单授权")?
        } else {
            &self.fields
        };
        if (self.format != "timelens-oss-post-v1" && !self.is_shared())
            || self.device_id != device
            || uuid::Uuid::parse_str(device).is_err()
        {
            return Err("授权不属于本机固定设备ID".into());
        }
        if !frame_upload::valid_id(&self.user_id) || self.prefix != prefix(&self.user_id, device) {
            return Err("授权用户目录不匹配".into());
        }
        if self.bucket.len() < 3
            || self.bucket.len() > 63
            || self.bucket.starts_with('-')
            || self.bucket.ends_with('-')
            || !self
                .bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || self.region != "cn-beijing"
            || self.endpoint != format!("https://{}.oss-cn-beijing.aliyuncs.com/", self.bucket)
        {
            return Err("授权目的地须为北京OSS的HTTPS Bucket根地址".into());
        }
        let issued = millis(&self.issued_at)?;
        let expires = millis(&self.expires_at)?;
        if issued <= 0
            || expires <= issued
            || expires - issued > 7 * 86_400_000
            || issued > now + 300_000
        {
            return Err("授权有效期须在签发后7天内且已生效".into());
        }
        if now >= expires {
            return Err("上传授权已过期，请管理员续签；待传图片保留".into());
        }
        let end = millis(&self.archive_end_at)?;
        if !(1..=4_102_444_800_000).contains(&end) {
            return Err("授权中的批准调研结束时间无效".into());
        }
        let field = |name: &str| fields.get(name).map(String::as_str).ok_or("授权字段缺失");
        let date = Utc
            .timestamp_millis_opt(issued)
            .single()
            .ok_or("签发时间无效")?;
        let credential = field("x-oss-credential")?;
        let suffix = format!(
            "/{}/cn-beijing/oss/aliyun_v4_request",
            date.format("%Y%m%d")
        );
        let id = credential.strip_suffix(&suffix).ok_or("授权凭据范围无效")?;
        if !(8..=128).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err("授权凭据范围无效".into());
        }
        let expected = [
            ("x-oss-signature-version", "OSS4-HMAC-SHA256"),
            ("x-oss-object-acl", "private"),
            ("x-oss-server-side-encryption", "AES256"),
            ("x-oss-forbid-overwrite", "true"),
            ("success_action_status", "204"),
            (
                "Content-Type",
                if manifest {
                    "application/json"
                } else {
                    "image/webp"
                },
            ),
            ("x-oss-meta-timelens-user", self.user_id.as_str()),
            ("x-oss-meta-timelens-device", device),
            (
                "x-oss-meta-timelens-study-end",
                self.archive_end_at.as_str(),
            ),
        ];
        for (key, value) in expected {
            if field(key)? != value {
                return Err("授权上传约束不匹配".into());
            }
        }
        if field("x-oss-date")? != date.format("%Y%m%dT%H%M%SZ").to_string() {
            return Err("授权签发时间不匹配".into());
        }
        let sig = field("x-oss-signature")?;
        if sig.len() != 64 || !sig.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("授权签名无效".into());
        }
        // Exact allowlist: no extra redirect/callback/ACL/Secret fields or duplicate policy conditions.
        if fields.len() != 13 {
            return Err("授权包含额外字段".into());
        }
        let decoded = STANDARD
            .decode(field("policy")?)
            .map_err(|_| "授权策略编码无效")?;
        let policy: Policy = serde_json::from_slice(&decoded).map_err(|_| "授权策略无效")?;
        if millis(&policy.expiration)? != expires {
            return Err("授权截止与签名策略不匹配".into());
        }
        let mut conditions = vec![
            json!({"bucket":self.bucket}),
            json!([
                "starts-with",
                "$key",
                if self.is_shared() {
                    shared_prefix(&self.user_id)
                } else if manifest {
                    format!("{}manifests/", self.prefix)
                } else {
                    self.prefix.clone()
                }
            ]),
            json!(["content-length-range", 1, MAX_FRAME_BYTES]),
        ];
        for (key, value) in fields {
            if key != "policy" && key != "x-oss-signature" {
                if self.is_shared() && key == "x-oss-meta-timelens-device" {
                    conditions.push(json!(["starts-with", "$x-oss-meta-timelens-device", ""]));
                } else { conditions.push(json!({key:value})); }
            }
        }
        for name in ["sha256", "task", "captured-at", "display"] {
            conditions.push(json!([
                "starts-with",
                format!("$x-oss-meta-timelens-{name}"),
                ""
            ]));
        }
        if policy.conditions.len() != conditions.len() {
            return Err("授权策略限制不完整".into());
        }
        let mut actual = policy.conditions;
        for expected in conditions {
            let i = actual
                .iter()
                .position(|v| *v == expected)
                .ok_or("授权策略与设备、目录或文件限制不匹配")?;
            actual.remove(i);
        }
        Ok(())
    }
    pub fn matches_config(&self, cfg: &InternalConfig) -> Result<(), String> {
        if cfg.upload_mode != "ossDirect"
            || cfg.upload_url != self.endpoint
            || cfg.upload_device_id != self.device_id
            || cfg.upload_survey_id != self.user_id
            || cfg.archive_end_at != self.archive_end_at
        {
            return Err("授权与已保存的上传配置不匹配".into());
        }
        Ok(())
    }
}
pub fn load(root: &Path, cfg: &InternalConfig) -> Result<Grant, String> {
    let bytes = private_read(&root.join(GRANT_FILE))?;
    let raw = std::str::from_utf8(&bytes).map_err(|_| "授权编码无效")?;
    let g = Grant::parse(raw, &device_id(root)?, Utc::now().timestamp_millis())?;
    g.matches_config(cfg)?;
    Ok(g)
}
pub fn summary(root: &Path, cfg: &InternalConfig) -> Value {
    match load(root, cfg) {
        Ok(g) => {
            json!({"valid":true,"userId":g.user_id,"deviceId":g.device_id,"prefix":g.prefix,"endpoint":g.endpoint,"expiresAt":g.expires_at,"archiveEndAt":g.archive_end_at,"fingerprint":g.fingerprint(),"scope":if g.is_shared() {"shared"} else {"device"}})
        }
        Err(e) => json!({"valid":false,"error":e}),
    }
}
pub fn import(
    root: &Path,
    control: &super::internal::InternalControl,
    raw: &str,
) -> Result<(), String> {
    let g = Grant::parse(raw, &device_id(root)?, Utc::now().timestamp_millis())?;
    let _gate = control.gate.write();
    let p = control.progress.lock().clone();
    if control.frame_upload_running.load(Ordering::SeqCst)
        || matches!(
            p.status,
            TaskStatus::Recording | TaskStatus::Uploading | TaskStatus::Uploaded
        )
        || p.receipt.is_some()
        || p.stored_bundle.is_some()
        || p.bundle_path.is_some()
    {
        return Err("请先暂停采集并停止上传，再导入授权".into());
    }
    let old = control.config.read().clone();
    let mut cfg = old.clone();
    cfg.upload_mode = "ossDirect".into();
    cfg.upload_url = g.endpoint.clone();
    cfg.upload_survey_id = g.user_id.clone();
    cfg.upload_device_id = g.device_id.clone();
    cfg.archive_end_at = g.archive_end_at.clone();
    cfg.domestic_server_confirmed = true;
    cfg.validate()?;
    if let Some(b) = super::archive::load(root)? {
        if b.upload_mode == "ossDirect" {
            let bytes = private_read(&root.join(GRANT_FILE)).map_err(|_| "已绑定任务的原授权不可读，请管理员修复，不能切换范围")?;
            let previous: Grant = serde_json::from_slice(&bytes).map_err(|_| "原上传授权损坏，不能切换授权范围")?;
            if previous.format != g.format { return Err("任务已绑定，不能在设备专属与公用授权之间切换".into()); }
        }
        if b.upload_mode != cfg.upload_mode
            || b.upload_url != cfg.upload_url
            || b.survey_id != cfg.upload_survey_id
            || b.device_id != cfg.upload_device_id
            || b.planned_end_ms != super::archive::end_ms(&cfg)?
        {
            return Err(
                "任务已绑定目的地或调研截止；续签须保留原用户、设备、Bucket和批准结束时间".into(),
            );
        }
        super::archive::ensure_live(root, &p.task_id, Utc::now().timestamp_millis())?;
    }
    super::internal::write_json_atomic(&root.join("internal-config.json"), &cfg)?;
    if let Err(e) = super::internal::write_json_atomic(&root.join(GRANT_FILE), &g) {
        super::internal::write_json_atomic(&root.join("internal-config.json"), &old)
            .map_err(|_| "导入失败且配置恢复失败，请停止上传并由管理员检查")?;
        return Err(e);
    }
    *control.config.write() = cfg;
    Ok(())
}

/// OSS uses reflected ECMA CRC64 with all-one initialization/final xor.
pub fn crc64(bytes: &[u8]) -> u64 {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut t = [0; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let mut x = i as u64;
            for _ in 0..8 {
                x = if x & 1 != 0 {
                    (x >> 1) ^ 0xc96c5795d7870f42
                } else {
                    x >> 1
                };
            }
            *v = x;
        }
        t
    });
    let mut crc = u64::MAX;
    for byte in bytes {
        crc = table[((crc as u8) ^ byte) as usize] ^ (crc >> 8);
    }
    crc ^ u64::MAX
}
#[derive(Debug, Serialize)]
pub struct Accepted {
    pub object_key: String,
    pub crc64: String,
    pub request_id: String,
}
pub(super) fn object_key(g: &Grant, f: &Frame) -> Result<String, String> {
    uuid::Uuid::parse_str(&f.task_id).map_err(|_| "任务ID无效")?;
    uuid::Uuid::parse_str(&f.id).map_err(|_| "截图ID无效")?;
    let date = Utc
        .timestamp_millis_opt(f.captured_at_ms)
        .single()
        .ok_or("截图时间无效")?
        .format("%Y/%m/%d");
    Ok(format!(
        "{}tasks/{}/{}/{}.webp",
        g.prefix, f.task_id, date, f.id
    ))
}
fn multipart(g: &Grant, f: &Frame, bytes: &[u8]) -> Result<(String, Vec<u8>), String> {
    if bytes.is_empty()
        || bytes.len() as u64 > MAX_FRAME_BYTES
        || bytes.len() as u64 != f.size_bytes
        || format!("{:x}", Sha256::digest(bytes)) != f.sha256
    {
        return Err("截图校验失败，未发送".into());
    }
    if f.captured_at_ms <= 0 || f.captured_at_ms > millis(&g.archive_end_at)? {
        return Err("截图时间超过批准结束范围，本地图片保留，需管理员核对".into());
    }
    let key = object_key(g, f)?;
    let boundary = format!("timelens-{}", uuid::Uuid::new_v4().simple());
    let mut fields = g.fields.clone();
    fields.insert("key".into(), key);
    for (name, value) in [
        ("sha256", f.sha256.clone()),
        ("task", f.task_id.clone()),
        ("captured-at", f.captured_at_ms.to_string()),
        ("display", f.display_id.to_string()),
    ] {
        fields.insert(format!("x-oss-meta-timelens-{name}"), value);
    }
    let mut out = Vec::with_capacity(bytes.len() + 10000);
    for (name, value) in fields {
        out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}.webp\"\r\nContent-Type: image/webp\r\n\r\n",f.id).as_bytes());
    out.extend_from_slice(bytes);
    out.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((boundary, out))
}
fn send_to(
    client: &reqwest::blocking::Client,
    url: &str,
    g: &Grant,
    f: &Frame,
    bytes: &[u8],
) -> Result<Accepted, String> {
    g.validate(&g.device_id, Utc::now().timestamp_millis())?;
    let (boundary, body) = multipart(g, f, bytes)?;
    let response = client
        .post(url)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .map_err(|_| "OSS上传连接失败；待传图片保留，若已接收但回执丢失需管理员核验")?;
    if response.status().as_u16() != 204 {
        return Err(format!(
            "OSS返回 HTTP {}；本地图片保留，409不能视为上传成功",
            response.status().as_u16()
        ));
    }
    let remote_crc = response
        .headers()
        .get("x-oss-hash-crc64ecma")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if remote_crc != Some(crc64(bytes)) {
        return Err("OSS回执CRC校验失败，本地图片保留".into());
    }
    if response.headers().contains_key("x-oss-version-id") {
        return Err("OSS版本控制与禁止覆盖策略不兼容，请管理员核验；本地图片保留".into());
    }
    let request_id = response
        .headers()
        .get("x-oss-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 256)
        .ok_or("OSS回执缺少请求编号，本地图片保留")?
        .to_string();
    Ok(Accepted {
        object_key: object_key(g, f)?,
        crc64: remote_crc.unwrap().to_string(),
        request_id,
    })
}
pub fn send(
    client: &reqwest::blocking::Client,
    g: &Grant,
    f: &Frame,
    bytes: &[u8],
) -> Result<Accepted, String> {
    send_to(client, &g.endpoint, g, f, bytes)
}
fn next(conn: &Connection, task: &str) -> Result<Option<Frame>, String> {
    conn.query_row("SELECT s.id,s.file_path,s.captured_at_ms,q.display_id,q.sha256,q.size_bytes,q.attempts,q.task_id FROM frame_upload_outbox q JOIN snapshots s ON s.id=q.snapshot_id LEFT JOIN oss_direct_receipts d ON d.snapshot_id=q.snapshot_id WHERE q.task_id=?1 AND q.status='pending' AND q.next_retry_ms<=?2 AND d.snapshot_id IS NULL ORDER BY s.captured_at_ms LIMIT 1",params![task,Utc::now().timestamp_millis()],|r|Ok(Frame{id:r.get(0)?,path:r.get(1)?,captured_at_ms:r.get(2)?,display_id:r.get(3)?,sha256:r.get(4)?,size_bytes:r.get::<_,i64>(5)? as u64,attempts:r.get(6)?,task_id:r.get(7)?})).optional().map_err(|e|e.to_string())
}
fn record(conn: &Connection, f: &Frame, r: &Accepted) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO oss_direct_receipts(snapshot_id,task_id,object_key,sha256,size_bytes,crc64,request_id,accepted_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![f.id,f.task_id,r.object_key,f.sha256,f.size_bytes as i64,r.crc64,r.request_id,Utc::now().timestamp_millis()]).map_err(|e|e.to_string())?;
    tx.execute(
        "UPDATE frame_upload_outbox SET last_error=NULL,next_retry_ms=0 WHERE snapshot_id=?",
        [&f.id],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}
fn worker(app: tauri::AppHandle, state: crate::AppState, cfg: InternalConfig, task: String) {
    let result = (|| -> Result<(), String> {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "无法创建OSS连接")?;
        let conn = frame_upload::db(&state.0.paths)?;
        let mut manifest_tick = std::time::Instant::now() - Duration::from_secs(61);
        let mut manifest_attempts: u32 = 0;
        while state.0.internal.frame_upload_active.load(Ordering::SeqCst) {
            super::archive::ensure_live(&state.0.paths.root, &task, Utc::now().timestamp_millis())?;
            let progress = state.0.internal.progress.lock().clone();
            super::archive::shorten(&state.0.paths.root, &progress)?;
            state.0.writer.check_health()?;
            if progress.integrity_error.is_some() {
                return Err("数据完整性待审核，已停止上传".into());
            }
            let grant = load(&state.0.paths.root, &cfg)?; // Expiry stops this run without mutating pending rows.
            if manifest_tick.elapsed() >= Duration::from_secs(60) {
                if let Err(error) = super::oss_manifest::publish(
                    &client,
                    &grant,
                    &conn,
                    &state.0.paths.root,
                    &progress,
                ) {
                    state.0.internal.edit_progress(|p| {
                        p.last_error = Some(error.clone());
                        Ok(())
                    })?;
                    if !super::oss_manifest::retryable(&error) {
                        return Err(error);
                    }
                    let delay = frame_upload::retry_delay_ms(manifest_attempts).min(60_000);
                    manifest_attempts = manifest_attempts.saturating_add(1);
                    let until = std::time::Instant::now() + Duration::from_millis(delay as u64);
                    while state.0.internal.frame_upload_active.load(Ordering::SeqCst)
                        && std::time::Instant::now() < until
                    {
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    continue;
                }
                manifest_attempts = 0;
                manifest_tick = std::time::Instant::now();
                state.0.internal.edit_progress(|p| {
                    if p.last_error
                        .as_ref()
                        .is_some_and(|e| e.starts_with("清单") && super::oss_manifest::retryable(e))
                    {
                        p.last_error = None;
                    }
                    Ok(())
                })?;
            }
            if let Some(frame) = next(&conn, &task)? {
                frame_upload::bind(&conn, &frame, &cfg)?;
                let result = frame_upload::frame_bytes(&state.0.paths, &frame).and_then(|bytes| {
                    if !state.0.internal.frame_upload_active.load(Ordering::SeqCst) {
                        return Err("后台上传已停止".into());
                    }
                    send(&client, &grant, &frame, &bytes)
                });
                match result {
                    Ok(receipt) => {
                        super::archive::ensure_live(
                            &state.0.paths.root,
                            &task,
                            Utc::now().timestamp_millis(),
                        )?;
                        record(&conn, &frame, &receipt)?;
                        state.0.internal.edit_progress(|p| {
                            p.last_error = None;
                            Ok(())
                        })?;
                    }
                    Err(error) => {
                        let blocked = error.contains("校验失败")
                            || error.contains("批准结束")
                            || error.contains("HTTP 409");
                        conn.execute("UPDATE frame_upload_outbox SET status=?2,attempts=attempts+1,next_retry_ms=?3,last_error=?4 WHERE snapshot_id=?1",params![frame.id,if blocked{"blocked"}else{"pending"},Utc::now().timestamp_millis()+frame_upload::retry_delay_ms(frame.attempts),error]).map_err(|e|e.to_string())?;
                        // A rejected/ambiguous response must never produce an accepted receipt.
                        let status = error
                            .split("HTTP ")
                            .nth(1)
                            .and_then(|v| v.get(..3))
                            .and_then(|v| v.parse::<u16>().ok());
                        let fatal = blocked
                            || error.contains("回执")
                            || status.is_some_and(|v| {
                                (300..500).contains(&v) && ![408, 429].contains(&v)
                            });
                        state.0.internal.edit_progress(|p| {
                            p.last_error = Some(error.clone());
                            Ok(())
                        })?;
                        if fatal {
                            return Err(error);
                        }
                    }
                }
            } else {
                let stats = frame_upload::stats(&conn, &task)?;
                if progress.ended_at_ms.is_some()
                    && !progress.needs_flush
                    && stats.pending == 0
                    && stats.blocked == 0
                {
                    if let Err(error) = super::oss_manifest::publish(
                        &client,
                        &grant,
                        &conn,
                        &state.0.paths.root,
                        &progress,
                    ) {
                        if super::oss_manifest::retryable(&error) {
                            manifest_tick = std::time::Instant::now() - Duration::from_secs(61);
                            continue;
                        }
                        return Err(error);
                    }
                    break;
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
            p.last_error = Some(error);
            Ok(())
        });
    }
    frame_upload::finish_run(&state.0.internal);
    let _ = app.emit(
        "internal_task_changed",
        state.0.internal.progress.lock().clone(),
    );
}
pub fn validate_confirmation(
    root: &Path,
    cfg: &InternalConfig,
    p: &super::internal::TaskProgress,
    expected: &super::desktop::UploadConfirmation,
) -> Result<(), String> {
    super::desktop::validate_confirmation(expected, cfg, p, true)?;
    let grant = load(root, cfg)?;
    if expected.grant_fingerprint != grant.fingerprint() {
        return Err("上传授权已变化，请重新核对并确认".into());
    }
    Ok(())
}
pub fn begin(
    app: &tauri::AppHandle,
    expected: &super::desktop::UploadConfirmation,
) -> Result<(), String> {
    let state = app.state::<crate::AppState>().inner().clone();
    let _gate = state.0.internal.gate.read();
    let cfg = state.0.internal.config.read().clone();
    let p = state.0.internal.progress.lock().clone();
    validate_confirmation(&state.0.paths.root, &cfg, &p, expected)?;
    if p.receipt.is_some()
        || p.stored_bundle.is_some()
        || p.bundle_path.is_some()
        || p.integrity_error.is_some()
        || (p.needs_flush && p.status != TaskStatus::Recording)
    {
        return Err("数据保存、完整性检查或旧回执需先核对".into());
    }
    super::archive::bind(&state.0.paths, &cfg, &p)?;
    frame_upload::start_run(&state.0.internal)?;
    let reset=frame_upload::db(&state.0.paths).and_then(|c|c.execute("UPDATE frame_upload_outbox SET status='pending',next_retry_ms=0,last_error=NULL WHERE task_id=? AND status!='verified' AND snapshot_id NOT IN (SELECT snapshot_id FROM oss_direct_receipts)",[&p.task_id]).map(|_|()).map_err(|e|e.to_string()));
    if let Err(e) = reset {
        frame_upload::finish_run(&state.0.internal);
        return Err(e);
    }
    drop(_gate);
    let handle = app.clone();
    std::thread::spawn(move || worker(handle, state, cfg, p.task_id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_id_survives_restart_and_corruption_never_regenerates() {
        let t = tempfile::tempdir().unwrap();
        let id = device_id(t.path()).unwrap();
        assert_eq!(id, device_id(t.path()).unwrap());
        let handles = (0..8)
            .map(|_| {
                let root = t.path().to_owned();
                std::thread::spawn(move || device_id(&root).unwrap())
            })
            .collect::<Vec<_>>();
        for h in handles {
            assert_eq!(h.join().unwrap(), id);
        }
        super::super::internal::write_json_atomic(
            &t.path().join("device-identity.json"),
            &"broken",
        )
        .unwrap();
        assert!(device_id(t.path()).is_err());
        assert!(
            String::from_utf8(fs::read(t.path().join("device-identity.json")).unwrap())
                .unwrap()
                .contains("broken")
        );
    }
    #[test]
    fn crc64_matches_standard_and_empty_vectors() {
        assert_eq!(crc64(b"123456789"), 0x995dc9bbdf1939fa);
        assert_eq!(crc64(b""), 0);
    }

    #[cfg(feature = "frame-contract-tests")]
    mod contract {
        use super::*;
        use std::{
            io::BufRead,
            process::{Child, Command, Stdio},
            sync::{atomic::AtomicBool, Arc},
        };
        fn server_root() -> std::path::PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../upload-server")
        }
        fn python() -> String {
            std::env::var("TIMELENS_TEST_PYTHON").unwrap_or_else(|_| "python3".into())
        }
        fn grant(device: &str) -> Grant {
            let out = Command::new(python())
                .arg("tests/oss_direct_fixture.py")
                .arg("--grant")
                .arg(device)
                .current_dir(server_root())
                .env("PYTHONPATH", server_root())
                .output()
                .unwrap();
            assert!(out.status.success(), "synthetic fixture failed");
            Grant::parse(
                std::str::from_utf8(&out.stdout).unwrap(),
                device,
                Utc::now().timestamp_millis(),
            )
            .unwrap()
        }
        fn fixture() -> (
            tempfile::TempDir,
            super::super::super::storage::DataPaths,
            Connection,
            Arc<super::super::super::internal::InternalControl>,
            Grant,
        ) {
            let t = tempfile::tempdir().unwrap();
            let root = t.path().to_path_buf();
            let p = super::super::super::storage::DataPaths {
                root: root.clone(),
                data_dir: root.join("data"),
                shots_dir: root.join("data/shots"),
                db_path: root.join("data/db.sqlite"),
                exports_dir: root.join("exports"),
            };
            p.ensure_dirs().unwrap();
            let c = super::super::super::storage::db::open_write(&p.db_path).unwrap();
            let control = super::super::super::internal::InternalControl::load(
                &p.root,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            let g = grant(&device_id(&p.root).unwrap());
            (t, p, c, control, g)
        }
        fn shared_raw() -> String {
            let out = Command::new(python()).arg("tests/oss_direct_fixture.py").arg("--shared-grant")
                .current_dir(server_root()).env("PYTHONPATH", server_root()).output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap()
        }
        #[test]
        fn shared_import_binds_two_profiles_roundtrips_and_never_starts_upload() {
            let raw = shared_raw();
            let (_a, first, _, control_a, _) = fixture();
            let (_b, second, _, control_b, _) = fixture();
            import(&first.root, &control_a, &raw).unwrap();
            import(&second.root, &control_b, &raw).unwrap();
            let ga = load(&first.root, &control_a.config.read()).unwrap();
            let gb = load(&second.root, &control_b.config.read()).unwrap();
            assert!(ga.is_shared() && gb.is_shared());
            assert_ne!(ga.device_id, gb.device_id);
            assert_ne!(ga.prefix, gb.prefix);
            assert_eq!(ga.fields["policy"], gb.fields["policy"]);
            assert_eq!(ga.fields["x-oss-signature"], gb.fields["x-oss-signature"]);
            assert_eq!(ga.fields["x-oss-meta-timelens-device"], ga.device_id);
            assert_eq!(gb.manifest_fields.as_ref().unwrap()["x-oss-meta-timelens-device"], gb.device_id);
            assert!(!control_a.frame_upload_active.load(Ordering::SeqCst));
            assert!(!control_a.frame_upload_running.load(Ordering::SeqCst));
            assert!(!control_b.frame_upload_active.load(Ordering::SeqCst));
            let saved = serde_json::to_string(&ga).unwrap();
            assert!(Grant::parse(&saved,&gb.device_id,Utc::now().timestamp_millis()).is_err());
            assert!(Grant::parse(&saved,&ga.device_id,millis(&ga.expires_at).unwrap()).is_err());
            for field in ["prefix", "deviceId", "format"] {
                let mut bad: Value = serde_json::from_str(&raw).unwrap();
                bad[field] = json!(if field == "deviceId" {gb.device_id.clone()} else {"invalid".into()});
                assert!(Grant::parse(&bad.to_string(),&ga.device_id,Utc::now().timestamp_millis()).is_err());
            }
            let mut bad: Value = serde_json::from_str(&raw).unwrap();
            bad["fields"]["x-oss-meta-timelens-device"] = json!(gb.device_id);
            assert!(Grant::parse(&bad.to_string(),&ga.device_id,Utc::now().timestamp_millis()).is_err());
        }
        #[test]
        fn import_scope_expiry_and_sensitive_fields_fail_closed() {
            let (_t, p, _c, control, g) = fixture();
            let raw = serde_json::to_string(&g).unwrap();
            import(&p.root, &control, &raw).unwrap();
            let cfg = control.config.read().clone();
            assert_eq!(cfg.upload_mode, "ossDirect");
            assert_eq!(load(&p.root, &cfg).unwrap().fingerprint(), g.fingerprint());
            let overview = summary(&p.root, &cfg).to_string();
            assert!(!overview.contains("policy"));
            assert!(!overview.contains("x-oss-signature"));
            let expire = millis(&g.expires_at).unwrap();
            assert!(g.validate(&g.device_id, expire - 1).is_ok());
            assert!(g.validate(&g.device_id, expire).is_err());
            assert!(g
                .validate(
                    &uuid::Uuid::new_v4().to_string(),
                    Utc::now().timestamp_millis()
                )
                .is_err());
            for kind in [
                "prefix", "bucket", "endpoint", "lifetime", "policy", "extra",
            ] {
                let mut value = serde_json::to_value(&g).unwrap();
                match kind {
                    "prefix" => value["prefix"] = json!("timelens/direct/v1/"),
                    "bucket" => value["bucket"] = json!("other-bucket"),
                    "endpoint" => value["endpoint"] = json!("https://attacker.invalid/"),
                    "lifetime" => {
                        value["expiresAt"] = json!(Utc
                            .timestamp_millis_opt(millis(&g.issued_at).unwrap() + 8 * 86_400_000)
                            .unwrap()
                            .to_rfc3339())
                    }
                    "policy" => value["fields"]["policy"] = json!(STANDARD.encode(b"{}")),
                    _ => value["accessKeySecret"] = json!("must-not-import"),
                }
                assert!(
                    import(&p.root, &control, &value.to_string()).is_err(),
                    "{kind}"
                );
                assert_eq!(load(&p.root, &cfg).unwrap().fingerprint(), g.fingerprint());
            }
        }
        #[test]
        fn renewal_is_same_destination_and_old_confirmation_cannot_authorize_new_grant() {
            let (_t, p, c, control, g) = fixture();
            import(&p.root, &control, &serde_json::to_string(&g).unwrap()).unwrap();
            let task = control.progress.lock().task_id.clone();
            super::super::super::archive::initialize_owner(&p, &c, &task).unwrap();
            let cfg = control.config.read().clone();
            let progress = control.progress.lock().clone();
            super::super::super::archive::bind(&p, &cfg, &progress).unwrap();
            let expected = super::super::super::desktop::UploadConfirmation {
                upload_mode: "ossDirect".into(),
                grant_fingerprint: g.fingerprint(),
                task_id: task,
                upload_url: cfg.upload_url.clone(),
                encryption_recipient: cfg.encryption_recipient.clone(),
                domestic_server_confirmed: true,
                cleanup_only: false,
                survey_id: cfg.upload_survey_id.clone(),
                device_id: cfg.upload_device_id.clone(),
                archive_end_at: cfg.archive_end_at.clone(),
            };
            assert!(validate_confirmation(&p.root, &cfg, &progress, &expected).is_ok());
            assert!(import(&p.root, &control, &shared_raw()).is_err());
            let mut next = g.clone();
            next.fields.insert("x-oss-signature".into(), "a".repeat(64));
            import(&p.root, &control, &serde_json::to_string(&next).unwrap()).unwrap();
            assert!(validate_confirmation(&p.root, &cfg, &progress, &expected).is_err());
            next = g.clone();
            next.user_id = "other-user".into();
            next.prefix = prefix(&next.user_id, &next.device_id);
            assert!(import(&p.root, &control, &serde_json::to_string(&next).unwrap()).is_err());
            let mut receiver = cfg.clone();
            receiver.upload_mode = "receiver".into();
            assert!(control.save_config(receiver).is_err());
            assert_eq!(device_id(&p.root).unwrap(), g.device_id);
            fs::remove_file(p.root.join(GRANT_FILE)).unwrap();
            assert!(import(&p.root, &control, &serde_json::to_string(&g).unwrap()).is_err());
            assert!(import(&p.root, &control, &shared_raw()).is_err());
        }
        struct ChildGuard(Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        #[test]
        fn tls_post_contract_crc_scope_retry_and_queue_accounting() { tls_contract(false); }
        #[test]
        fn shared_tls_post_image_manifest_crc_retry_and_queue_accounting() { tls_contract(true); }
        fn tls_contract(shared: bool) {
            let (_t, p, c, control, mut g) = fixture();
            if shared { g = Grant::parse(&shared_raw(), &g.device_id, Utc::now().timestamp_millis()).unwrap(); }
            let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
            let pem = p.root.join("fixture-cert.pem");
            let key = p.root.join("fixture-key.pem");
            fs::write(&pem, cert.cert.pem()).unwrap();
            fs::write(&key, cert.key_pair.serialize_pem()).unwrap();
            let mut child = ChildGuard(
                Command::new(python())
                    .arg("tests/oss_direct_fixture.py")
                    .arg(&pem)
                    .arg(&key)
                    .current_dir(server_root())
                    .env("PYTHONPATH", server_root())
                    .env_remove("OSS_ACCESS_KEY_ID")
                    .env_remove("OSS_ACCESS_KEY_SECRET")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap(),
            );
            let mut url = String::new();
            std::io::BufReader::new(child.0.stdout.take().unwrap())
                .read_line(&mut url)
                .unwrap();
            let url = url.trim();
            assert!(url.starts_with("https://127.0.0.1:"));
            let make_client = |fault: Option<&str>| {
                let mut headers = reqwest::header::HeaderMap::new();
                if let Some(value) = fault {
                    headers.insert("X-Fixture-Fault", value.parse().unwrap());
                }
                reqwest::blocking::Client::builder()
                    .https_only(true)
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(5))
                    .add_root_certificate(
                        reqwest::Certificate::from_pem(cert.cert.pem().as_bytes()).unwrap(),
                    )
                    .default_headers(headers)
                    .build()
                    .unwrap()
            };
            let client = make_client(None);
            let task = control.progress.lock().task_id.clone();
            let manifest_key = format!("{}manifests/{}/indexes/fixture.json", g.prefix, task);
            super::super::super::oss_manifest::post_to(
                &client,
                url,
                &g,
                &manifest_key,
                &task,
                b"{\"synthetic\":true}",
            )
            .unwrap();
            assert!(super::super::super::oss_manifest::post_to(
                &client,
                url,
                &g,
                &format!("{}tasks/evil.json", g.prefix),
                &task,
                b"{}"
            )
            .is_err());
            assert!(super::super::super::oss_manifest::post_to(
                &make_client(Some("crc")),
                url,
                &g,
                &format!("{}manifests/{}/indexes/bad-crc.json", g.prefix, task),
                &task,
                b"{}"
            )
            .is_err());
            let bytes = webp::Encoder::from_rgb(
                &[20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120, 130],
                2,
                2,
            )
            .encode(85.)
            .to_vec();
            let mut frame = Frame {
                task_id: control.progress.lock().task_id.clone(),
                id: uuid::Uuid::new_v4().to_string(),
                path: String::new(),
                captured_at_ms: Utc::now().timestamp_millis(),
                display_id: 1,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size_bytes: bytes.len() as u64,
                attempts: 0,
            };
            let receipt = send_to(&client, url, &g, &frame, &bytes).unwrap();
            assert!(receipt.object_key.starts_with(&g.prefix));
            assert!(send_to(&client, url, &g, &frame, &bytes)
                .unwrap_err()
                .contains("409"));
            c.execute("INSERT INTO window_sessions(id,app_name,window_title,start_ms,end_ms,duration_ms,is_active) VALUES('s','fixture','synthetic',1,2,1,0)",[]).unwrap();
            let file = p.shots_dir.join(format!("{}_fixture.webp", frame.id));
            fs::write(&file, &bytes).unwrap();
            frame.path = file.to_string_lossy().into_owned();
            c.execute("INSERT INTO snapshots(id,session_id,file_path,captured_at_ms,file_size_bytes,trigger_type) VALUES(?1,'s',?2,?3,?4,'poll')",params![frame.id,frame.path,frame.captured_at_ms,frame.size_bytes as i64]).unwrap();
            c.execute("INSERT INTO frame_upload_outbox(snapshot_id,task_id,display_id,sha256,size_bytes) VALUES(?1,?2,1,?3,?4)",params![frame.id,frame.task_id,frame.sha256,frame.size_bytes as i64]).unwrap();
            record(&c, &frame, &receipt).unwrap();
            let stats = frame_upload::stats(&c, &frame.task_id).unwrap();
            assert_eq!(
                (stats.pending, stats.verified, stats.oss_accepted),
                (0, 0, 1)
            );
            assert!(next(&c, &frame.task_id).unwrap().is_none());
            assert!(file.exists());
            for fault in ["crc", "disconnect", "redirect"] {
                frame.id = uuid::Uuid::new_v4().to_string();
                assert!(send_to(&make_client(Some(fault)), url, &g, &frame, &bytes).is_err());
                assert!(send_to(&client, url, &g, &frame, &bytes)
                    .unwrap_err()
                    .contains("409"));
                assert!(file.exists());
            }
            frame.id = uuid::Uuid::new_v4().to_string();
            let mut bad = g.clone();
            bad.fields.insert("x-oss-signature".into(), "0".repeat(64));
            assert!(send_to(&client, url, &bad, &frame, &bytes)
                .unwrap_err()
                .contains("403"));
            let mut too_late = frame.clone();
            too_late.captured_at_ms = millis(&g.archive_end_at).unwrap() + 1;
            assert!(send_to(&client, url, &g, &too_late, &bytes)
                .unwrap_err()
                .contains("批准结束"));
            let remote: Value = client.get(url).send().unwrap().json().unwrap();
            assert_eq!(remote["objects"].as_object().unwrap().len(), 6);
        }
    }
}
