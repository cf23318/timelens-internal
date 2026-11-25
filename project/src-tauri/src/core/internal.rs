//! Internal accumulated-hour collection policy. No network or operating-system capture in this module.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use parking_lot::{Condvar, Mutex, RwLock, RwLockReadGuard};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalConfig {
    pub auto_upload_enabled: bool,
    pub participant_name: String,
    pub capture_interval_secs: u64,
    pub poll_width: u32,
    pub poll_quality: f32,
    pub target_hours: u32,
    /// Read-only compatibility with old config files; new tasks use target_hours.
    #[serde(rename = "workdayHours", skip_serializing)]
    pub legacy_workday_hours: Option<u32>,
    /// Native APIs capture the display containing the foreground window by default.
    pub all_displays: bool,
    pub screenshot_blocked_apps: Vec<String>,
    pub screenshot_blocked_domains: Vec<String>,
    pub upload_mode: String,
    pub upload_url: String,
    pub upload_survey_id: String,
    pub upload_device_id: String,
    pub archive_end_at: String,
    pub encryption_recipient: String,
    pub domestic_server_confirmed: bool,
}

impl Default for InternalConfig {
    fn default() -> Self {
        Self {
            auto_upload_enabled: true,
            participant_name: String::new(),
            capture_interval_secs: 12,
            poll_width: 1600,
            poll_quality: 85.0,
            target_hours: 48,
            legacy_workday_hours: None,
            all_displays: false,
            screenshot_blocked_apps: [
                "1password",
                "keychain",
                "钥匙串",
                "keepass",
                "bitwarden",
                "lastpass",
                "dashlane",
                "keeper",
                "securityagent",
                "credentialui",
                "credwiz",
                "wechat",
                "微信",
                "weixin",
                "dingtalk",
                "钉钉",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            // This is an initial list, not an exhaustive identification of banking websites.
            screenshot_blocked_domains: [
                "icbc.com.cn",
                "ccb.com",
                "boc.cn",
                "abchina.com",
                "cmbchina.com",
                "bankcomm.com",
                "commbank.com.au",
                "nab.com.au",
                "westpac.com.au",
                "anz.com.au",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            upload_mode: "receiver".into(),
            upload_url: String::new(),
            upload_survey_id: String::new(),
            upload_device_id: String::new(),
            archive_end_at: String::new(),
            encryption_recipient: String::new(),
            domestic_server_confirmed: false,
        }
    }
}

impl InternalConfig {
    pub fn require_participant_name(&self) -> Result<(), String> {
        validate_participant_name(&self.participant_name)?;
        if self.upload_mode == "ossDirect" && self.participant_name.is_empty() {
            return Err("请先填写姓名".into());
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), String> {
        self.validate_capture()?;
        self.validate_upload()
    }

    fn validate_capture(&self) -> Result<(), String> {
        validate_participant_name(&self.participant_name)?;
        if !(10..=15).contains(&self.capture_interval_secs) {
            return Err("截图间隔必须为 10–15 秒".into());
        }
        if !(1600..=7680).contains(&self.poll_width)
            || !self.poll_quality.is_finite()
            || !(85.0..=100.0).contains(&self.poll_quality)
        {
            return Err("定时截图宽度须为 1600–7680，质量须为 85–100".into());
        }
        if !(1..=48).contains(&self.target_hours) {
            return Err("累计采集目标须为 1–48 小时".into());
        }
        if self.legacy_workday_hours.is_some_and(|v| !(1..=24).contains(&v)) {
            return Err("旧版每天录制时长无效".into());
        }
        if self
            .screenshot_blocked_apps
            .iter()
            .chain(self.screenshot_blocked_domains.iter())
            .any(|s| s.trim().is_empty() || s.len() > 256)
        {
            return Err("隐私规则不能为空或超过 256 字符".into());
        }
        Ok(())
    }

    pub fn validate_upload(&self) -> Result<(), String> {
        if !["receiver", "ossDirect"].contains(&self.upload_mode.as_str()) { return Err("上传模式无效".into()); }
        if !self.upload_url.is_empty() {
            let url = reqwest::Url::parse(&self.upload_url).map_err(|_| "上传地址无效")?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err("上传地址必须使用 HTTPS，且不能包含账号、查询参数或片段".into());
            }
        }
        if !self.archive_end_at.is_empty() { super::archive::end_ms(self)?; }
        if !self.encryption_recipient.is_empty()
            && self
                .encryption_recipient
                .trim()
                .parse::<age::x25519::Recipient>()
                .is_err()
        {
            return Err("加密公钥必须是有效的 age X25519 公钥".into());
        }
        Ok(())
    }

    pub fn blocks_screenshot(
        &self,
        app: &str,
        bundle: Option<&str>,
        title: &str,
        url: Option<&str>,
        browser: bool,
    ) -> bool {
        let identity = format!("{} {}", app, bundle.unwrap_or("")).to_lowercase();
        if self
            .screenshot_blocked_apps
            .iter()
            .any(|s| identity.contains(&s.trim().to_lowercase()))
        {
            return true;
        }
        if super::privacy::sensitive_title(title) {
            return true;
        }
        // An unreadable browser address must never silently bypass banking-domain rules.
        if browser && url.is_none() {
            return true;
        }
        let host = url
            .and_then(super::browser::url_host);
        if browser && host.is_none() {
            return true;
        }
        host.is_some_and(|host| {
            self.screenshot_blocked_domains.iter().any(|d| {
                let d = d.trim().trim_start_matches('.').to_lowercase();
                host == d || host.ends_with(&format!(".{d}"))
            })
        })
    }
}

/// A display name becomes one literal OSS path segment, never an identifier or a grant scope.
/// Empty is accepted only for old profiles/queue records; actions enforce the required field.
pub fn validate_participant_name(name: &str) -> Result<(), String> {
    if name != name.trim() || name.chars().count() > 64 || name.len() > 256
        || matches!(name, "." | "..")
        || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\')
            || matches!(c as u32, 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x206f | 0xfeff))
    {
        return Err("姓名须为1至64个字符，不能包含路径分隔符、控制字符或隐藏方向字符".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Ready,
    Recording,
    Paused,
    Completed,
    Uploading,
    Uploaded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskProgress {
    pub task_id: String,
    pub elapsed_ms: u64,
    pub target_ms: u64,
    #[serde(default)]
    pub ended_at_ms: Option<i64>,
    pub status: TaskStatus,
    pub last_error: Option<String>,
    pub bundle_path: Option<String>,
    #[serde(default)]
    pub receipt: Option<super::transfer::UploadReceipt>,
    #[serde(default)]
    pub stored_bundle: Option<super::transfer::StoredBundle>,
    #[serde(default)]
    pub integrity_error: Option<String>,
    #[serde(default)]
    pub needs_flush: bool,
}

pub(super) const LEGACY_UNCLEAN_EXIT_ERROR: &str =
    "上次采集未正常结束，排队数据可能未完整写入；需人工审核后处理";
const PROGRESS_SAVE_ERROR: &str = "无法保存任务进度";
const CAPTURE_GATE_WAIT: std::time::Duration = std::time::Duration::from_secs(8);
const COMPLETION_FILE: &str = "internal-task-completion.json";
const CONFIG_FILE: &str = "internal-config.json";
const TASK_FILE: &str = "internal-task.json";
const CONFIG_RECOVERY_FILE: &str = "internal-config-recovery.json";
const TASK_RECOVERY_FILE: &str = "internal-task-recovery.json";
const PROGRESS_CHECKPOINT_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum LocalTaskIssueKind {
    ConfigAccess,
    ConfigMissing,
    ConfigFormat,
    ConfigValues,
    ProgressAccess,
    ProgressMissing,
    ProgressFormat,
    ProgressValues,
    ArchiveInvalid,
    OwnerInvalid,
    IdentityMismatch,
    StorageUnavailable,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LocalTaskIssue {
    pub kind: LocalTaskIssueKind,
    pub detail: String,
}

impl LocalTaskIssue {
    pub fn new(kind: LocalTaskIssueKind, detail: impl Into<String>) -> Self {
        Self { kind, detail: detail.into() }
    }
}

#[derive(Serialize, Deserialize)]
struct CompletionRecord {
    task_id: String,
    elapsed_ms: u64,
    target_ms: u64,
    ended_at_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct ConfigCheckpoint {
    task_id: String,
    config: InternalConfig,
}

fn capture_has_failed(code: &str) -> bool {
    matches!(code, "save_error" | "capture_error" | "permission")
}

impl TaskProgress {
    fn fresh(target_hours: u32) -> Self {
        Self {
            task_id: uuid::Uuid::new_v4().to_string(),
            elapsed_ms: 0,
            ended_at_ms: None,
            target_ms: target_hours as u64 * 3_600_000,
            status: TaskStatus::Ready,
            last_error: None,
            bundle_path: None,
            receipt: None,
            stored_bundle: None,
            integrity_error: None,
            needs_flush: false,
        }
    }
}

// Even a zero-duration task may already have ended or enrolled in retention.
fn target_can_change(p: &TaskProgress, archive_bound: bool) -> bool {
    !archive_bound
        && p.status == TaskStatus::Ready
        && p.elapsed_ms == 0
        && p.ended_at_ms.is_none()
        && !p.needs_flush
        && p.integrity_error.is_none()
        && p.receipt.is_none()
        && p.stored_bundle.is_none()
        && p.bundle_path.is_none()
}

pub struct InternalControl {
    pub config: RwLock<InternalConfig>,
    pub gate: RwLock<()>,
    pub transition: Mutex<()>,
    native_jobs: (Mutex<usize>, Condvar),
    pub epoch: AtomicU64,
    pub tracking: Arc<AtomicBool>,
    /// Authorization is deliberately runtime-only: restart never sends old data.
    pub frame_upload_active: AtomicBool,
    pub frame_upload_running: AtomicBool,
    pub(super) device_repaired: AtomicBool,
    pub cloud_refresh: Mutex<()>,
    pub cloud_status: Mutex<super::cloud_auth::CloudStatus>,
    pub auto_failed_grant: Mutex<Option<String>>,
    pub progress: Mutex<TaskProgress>,
    pub capture_notice: Mutex<super::desktop::CaptureNotice>,
    pub(crate) startup_issue: Mutex<Option<LocalTaskIssue>>,
    pub(super) root: PathBuf,
}

pub struct NativeJob<'a>(&'a InternalControl);
impl Drop for NativeJob<'_> {
    fn drop(&mut self) {
        let mut count = self.0.native_jobs.0.lock();
        *count -= 1;
        self.0.native_jobs.1.notify_all();
    }
}

pub fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    write_bytes_atomic(path, &bytes)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("json.{}.tmp",uuid::Uuid::new_v4()));
    let result=(||->Result<(),String>{
        let mut opts=fs::OpenOptions::new();opts.write(true).create_new(true);
        #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt;opts.mode(0o600).custom_flags(libc::O_NOFOLLOW); }
        let mut file=opts.open(&tmp).map_err(|e|e.to_string())?;
        use std::io::Write;
        file.write_all(bytes).and_then(|_|file.sync_all()).map_err(|e|e.to_string())?;
        drop(file);
        fs::rename(&tmp,path).map_err(|e|e.to_string())?;
        #[cfg(unix)] if let Some(parent)=path.parent(){fs::File::open(parent).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;}
        Ok(())
    })();
    if result.is_err(){let _=fs::remove_file(&tmp);}
    result
}

pub(super) fn write_config(root: &Path, config: &InternalConfig, task_id: &str) -> Result<(), String> {
    let checkpoint = ConfigCheckpoint { task_id: task_id.to_owned(), config: config.clone() };
    let path = root.join(CONFIG_RECOVERY_FILE);
    if let Err(error) = write_json_atomic(&path, &checkpoint) {
        log::warn!("无法保存录制配置恢复副本：{error}");
        // Existing directories cannot be read as checkpoints. If an older
        // checkpoint file might still be read, invalidate it before changing
        // the effective privacy or upload settings.
        if !path.is_dir() {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("旧配置恢复副本暂被占用，请稍后重试保存设置".into()),
            }
        }
    }
    write_json_atomic(&root.join(CONFIG_FILE), config)
}

fn write_task(root: &Path, progress: &TaskProgress, checkpoint: bool) -> Result<(), String> {
    if checkpoint {
        if let Err(error) = write_json_atomic(&root.join(TASK_RECOVERY_FILE), progress) {
            log::warn!("无法保存任务恢复副本：{error}");
            // Keep an independent end record before replacing a terminal
            // primary so a stale active checkpoint cannot reopen that task.
            if let Some(end) = completion_record(progress) {
                write_json_atomic(&root.join(COMPLETION_FILE), &end)?;
            }
        }
    }
    write_json_atomic(&root.join(TASK_FILE), progress)
}

fn completion_record(progress: &TaskProgress) -> Option<CompletionRecord> {
    if !matches!(progress.status, TaskStatus::Completed | TaskStatus::Uploading | TaskStatus::Uploaded) {
        return None;
    }
    Some(CompletionRecord {
        task_id: progress.task_id.clone(),
        elapsed_ms: progress.elapsed_ms,
        target_ms: progress.target_ms,
        ended_at_ms: progress.ended_at_ms?,
    })
}

fn read_checkpoint<T: serde::de::DeserializeOwned>(root: &Path, file: &str) -> Result<T, String> {
    let path = root.join(file);
    let metadata = fs::symlink_metadata(&path).map_err(|_| "本机没有可用的恢复副本")?;
    if !metadata.is_file() {
        return Err("本机恢复副本不是常规文件".into());
    }
    serde_json::from_slice(&fs::read(&path).map_err(|_| "本机恢复副本暂时无法读取")?)
        .map_err(|_| "本机恢复副本未通过格式校验".into())
}

pub(super) fn preserve_damaged_file(root: &Path, file: &str) -> Result<(), String> {
    let path = root.join(file);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(metadata) if metadata.is_file() => {}
        _ => return Err("原状态文件暂时无法安全保存，请解除文件占用后重试".into()),
    }
    let bytes = fs::read(&path).map_err(|_| "原状态文件暂时无法读取，请解除文件占用后重试")?;
    use sha2::{Digest, Sha256};
    let name = format!("{file}-{:x}.bak", Sha256::digest(&bytes));
    let directory = super::storage::private_subdir(root, "local-repair").map_err(|e| e.to_string())?;
    let destination = directory.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_file() && fs::read(&destination).is_ok_and(|saved| saved == bytes) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => write_bytes_atomic(&destination, &bytes)?,
        _ => return Err("原文件的保留副本无法校验，已保留原文件".into()),
    }
    Ok(())
}

fn restore_local_task_files(root: &Path) -> Result<(), String> {
    use LocalTaskIssueKind::*;
    let mut config_repaired = false;
    let mut task_repaired = false;
    loop {
        let issue = match InternalControl::read_local_task(root) {
            Ok(_) => return Ok(()),
            Err(issue) => issue,
        };
        let repair_config = match issue.kind {
            ConfigMissing | ConfigFormat | ConfigValues if !config_repaired => true,
            ProgressMissing | ProgressFormat | ProgressValues if !task_repaired => false,
            _ => return Err(issue.detail),
        };
        let owner = super::archive::read_owner(root)?;
        let archive = super::archive::load(root)?;
        let primary = fs::read(root.join(TASK_FILE)).ok()
            .and_then(|bytes| serde_json::from_slice::<TaskProgress>(&bytes).ok())
            .filter(|progress| uuid::Uuid::parse_str(&progress.task_id).is_ok());
        // A task checkpoint cannot establish its own identity. A readable
        // primary task may corroborate a configuration checkpoint.
        let evidence = [
            owner.as_deref(),
            archive.as_ref().map(|binding| binding.task_id.as_str()),
            if repair_config { primary.as_ref().map(|progress| progress.task_id.as_str()) } else { None },
        ];
        let mut known = evidence.into_iter().flatten();
        let task = known.next().ok_or("本机找不到可以核对的原任务编号，已保留现有记录")?;
        if uuid::Uuid::parse_str(task).is_err() || known.any(|other| other != task)
            || primary.as_ref().is_some_and(|progress| progress.task_id != task)
        {
            return Err("本机任务编号、归属或归档记录互相冲突，已保留现有记录".into());
        }

        if repair_config {
            let checkpoint: ConfigCheckpoint = read_checkpoint(root, CONFIG_RECOVERY_FILE)?;
            if checkpoint.task_id != task {
                return Err("录制配置副本属于其他任务，已保留现有记录".into());
            }
            let config = checkpoint.config;
            config.validate_capture()?;
            if archive.as_ref().is_some_and(|binding| {
                config.upload_mode != binding.upload_mode || config.upload_url != binding.upload_url
                    || config.upload_device_id != binding.device_id || config.upload_survey_id != binding.survey_id
                    || super::archive::end_ms(&config).ok() != Some(binding.planned_end_ms)
            }) {
                return Err("录制配置副本与本机已绑定归档不一致，已保留现有记录".into());
            }
            preserve_damaged_file(root, CONFIG_FILE)?;
            write_json_atomic(&root.join(CONFIG_FILE), &config)?;
            config_repaired = true;
        } else {
            let progress: TaskProgress = read_checkpoint(root, TASK_RECOVERY_FILE)?;
            if progress.task_id != task || progress.target_ms == 0
                || progress.target_ms > 48 * 3_600_000 || progress.elapsed_ms > progress.target_ms
                || archive.as_ref().and_then(|binding| binding.target_ms).is_some_and(|target| progress.elapsed_ms > target)
            {
                return Err("进度副本的任务编号或累计时长未通过校验，已保留现有记录".into());
            }
            if archive.as_ref().is_some_and(|binding| {
                (binding.deleted || binding.effective_end_ms < binding.planned_end_ms)
                    && matches!(progress.status, TaskStatus::Ready | TaskStatus::Recording | TaskStatus::Paused)
            }) {
                return Err("已结束或清理的任务不能使用旧进度恢复采集，已保留现有记录".into());
            }
            preserve_damaged_file(root, TASK_FILE)?;
            write_json_atomic(&root.join(TASK_FILE), &progress)?;
            task_repaired = true;
        }
    }
}

pub fn external_ai_client() -> Result<reqwest::blocking::Client, String> {
    Err("内部采集版已停用外部 AI 网络连接".into())
}
pub fn deny_external_action() -> Result<(), String> {
    Err("内部采集版已停用外部 AI、自动粘贴和外部链接".into())
}

pub fn deny_personal_action() -> Result<(), String> {
    Err("内部采集版未开放个人分析、报表、明文导出或意图编辑，请使用受控的加密回收".into())
}

pub fn start_collection(app: &tauri::AppHandle, state: &crate::AppState) -> Result<(), String> {
    use tauri::Emitter;
    state.0.internal.require_operable()?;
    state.0.writer.check_health()?;
    super::recovery::check_capture_storage(&state.0.paths)?;
    let screen=super::acquisition::screen_capture_granted();
    state.0.screen_ok.store(screen, Ordering::SeqCst);
    start_local_policy(&state.0.internal, screen && super::acquisition::ax_trusted())?;
    let _ = app.emit(
        "tracking_state_changed",
        serde_json::json!({"isRunning": true}),
    );
    Ok(())
}
/// The local policy is separate from native OS checks so its complete state
/// transition can be verified with synthetic permissions and no screen capture.
pub(super) fn start_local_policy(control: &InternalControl, permissions_ready: bool) -> Result<(), String> {
    if !permissions_ready {return Err("请先授予屏幕录制与辅助功能权限，然后再开始采集".into());}
    control.start()
}

pub fn allow_webview_navigation(url: &reqwest::Url) -> bool {
    (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && url.scheme() == "http"
            && url.host_str() == Some("localhost")
            && url.port() == Some(1420))
}

pub fn pause_collection(app:&tauri::AppHandle,state:&crate::AppState,complete:bool)->Result<(),String>{ pause_collection_inner(app,state,complete,false) }
pub fn pause_for_expiry(app:&tauri::AppHandle,state:&crate::AppState)->Result<(),String>{ pause_collection_inner(app,state,false,true) }
fn pause_collection_inner(
    app: &tauri::AppHandle,
    state: &crate::AppState,
    complete: bool,
    expiry: bool,
) -> Result<(), String> {
    use tauri::Emitter;
    let _transition = state.0.internal.transition.try_lock_for(CAPTURE_GATE_WAIT)
        .ok_or("采集状态仍在切换，请稍后重试")?;
    let paused = state.0.internal.pause(complete);
    let _ = app.emit(
        "tracking_state_changed",
        serde_json::json!({"isRunning": false}),
    );
    paused?;
    state
        .0
        .aggregation_tx
        .send(super::models::AggregationCmd::EnterRecordingBlackout {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
        .map_err(|_| "会话线程不可用")?;
    let (done, rx) = std::sync::mpsc::channel();
    state
        .0
        .aggregation_tx
        .send(super::models::AggregationCmd::Barrier(done))
        .map_err(|_| "会话线程不可用")?;
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "等待会话关闭超时")?;
    if !expiry { if let Err(e) = state.0.writer.flush() {
        let persistent = state.0.writer.check_health().is_err();
        let _ = state.0.internal.edit_progress(|p| {
            if persistent {
                p.integrity_error = Some(e.clone());
            }
            p.last_error = Some(e.clone());
            Ok(())
        });
        return Err(e);
    } }
    let replay = if !expiry {
        let task = state.0.internal.progress.lock().task_id.clone();
        Some(super::recovery::replay_after_pause(&state.0.paths, &task))
    } else { None };
    state.0.internal.edit_progress(|p| {
        p.needs_flush = false;
        if p.last_error.as_deref().is_some_and(|e| e.starts_with(PROGRESS_SAVE_ERROR)) {
            p.last_error = None;
        }
        match replay {
            Some(Ok(())) => {
                for slot in [&mut p.integrity_error, &mut p.last_error] {
                    if slot.as_deref().is_some_and(|e| e.starts_with(super::recovery::RECOVERY_ERROR)) {
                        *slot = None;
                    }
                }
            }
            Some(Err(error)) => {
                let error = format!("{}{error}", super::recovery::RECOVERY_ERROR);
                p.integrity_error.get_or_insert_with(|| error.clone());
                p.last_error = Some(error);
            }
            None => {}
        }
        Ok(())
    })?;
    let _ = app.emit(
        "internal_task_changed",
        state.0.internal.progress.lock().clone(),
    );
    Ok(())
}

pub fn spawn_task_clock(app: tauri::AppHandle) {
    use tauri::{Emitter, Manager};
    std::thread::spawn(move || {
        let mut last = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(last).as_millis() as u64;
            last = now;
            let Some(state) = app.try_state::<crate::AppState>() else {
                break;
            };
            if state.0.exiting.load(Ordering::SeqCst) {
                break;
            }
            if state.0.tracking.load(Ordering::SeqCst) {
                if let Err(e) = state.0.writer.check_health() {
                    let _ = pause_collection(&app, &state, false);
                    let mut p = state.0.internal.progress.lock();
                    p.last_error = Some(e.clone());
                    p.integrity_error = Some(e);
                    drop(p);
                    let _ = state.0.internal.persist_progress();
                }
            }
            if let Err(e) = state.0.internal.advance(elapsed) {
                log::error!("录制进度保存失败：{e}");
            }
            if state.0.internal.completion_pending() {
                if let Err(e) = pause_collection(&app, &state, true)
                    .and_then(|_| super::frame_upload::on_task_finished(&app))
                {
                    log::error!("任务结束保存失败，将重试：{e}");
                }
            }
            let p = state.0.internal.progress.lock().clone();
            let remaining = p.target_ms.saturating_sub(p.elapsed_ms) / 60_000;
            let label = match p.status {
                TaskStatus::Recording => "录制中",
                TaskStatus::Ready => "待开始",
                TaskStatus::Paused => "已暂停",
                TaskStatus::Completed => "已停止",
                TaskStatus::Uploading => "上传中",
                TaskStatus::Uploaded => "已回收",
            };
            if let Some(tray) = app.tray_by_id("timelens_tray") {
                let _ = tray.set_tooltip(Some(format!(
                    "TimeLens 内部版 · {label} · 剩余 {} 小时 {} 分钟",
                    remaining / 60,
                    remaining % 60
                )));
            }
            let _ = app.emit("internal_task_changed", p);
        }
    });
}

impl InternalControl {
    pub fn open_for_app(root: &Path, tracking: Arc<AtomicBool>) -> Arc<Self> {
        match Self::load_checked(root, tracking.clone()) {
            Ok(control) => control,
            Err(error) => {
                tracking.store(false, Ordering::SeqCst);
                let config = InternalConfig::default();
                let control = Self::new(root, tracking, config.clone(), TaskProgress::fresh(config.target_hours));
                control.block_startup(error);
                control
            }
        }
    }

    pub(crate) fn block_startup(&self, issue: LocalTaskIssue) {
        *self.startup_issue.lock() = Some(issue);
    }

    pub(crate) fn require_operable(&self) -> Result<(), String> {
        self.startup_issue.lock().as_ref().map_or(Ok(()), |issue| Err(issue.detail.clone()))
    }

    pub fn configuration_write(&self) -> Result<RwLockReadGuard<'_, ()>, String> {
        self.require_operable()?;
        let guard = self.gate.try_read().ok_or("回收处理中，暂不能修改设置")?;
        let p = self.progress.lock();
        if matches!(p.status, TaskStatus::Uploading | TaskStatus::Uploaded)
            || p.stored_bundle.is_some()
            || p.receipt.is_some()
        {
            return Err("数据回收已开始，暂不能修改采集设置".into());
        }
        Ok(guard)
    }
    #[cfg(test)]
    pub fn load(root: &Path, tracking: Arc<AtomicBool>) -> Result<Arc<Self>, String> {
        Self::load_checked(root, tracking).map_err(|issue| issue.detail)
    }

    fn load_checked(root: &Path, tracking: Arc<AtomicBool>) -> Result<Arc<Self>, LocalTaskIssue> {
        let (config, mut progress, recovered_completion) = Self::read_local_task(root)?;
        tracking.store(false, Ordering::SeqCst);
        if let Err(error) = write_task(root, &progress, true) {
            progress.last_error = Some(format!("{PROGRESS_SAVE_ERROR}，请重试：{error}"));
        } else if recovered_completion
            && read_checkpoint::<TaskProgress>(root, TASK_RECOVERY_FILE).ok()
                .and_then(|checkpoint| completion_record(&checkpoint))
                .is_some_and(|end| end.task_id == progress.task_id)
        {
            let _ = fs::remove_file(root.join(COMPLETION_FILE));
        }
        if root.join(CONFIG_FILE).is_file() {
            let checkpoint = ConfigCheckpoint { task_id: progress.task_id.clone(), config: config.clone() };
            if let Err(error) = write_json_atomic(&root.join(CONFIG_RECOVERY_FILE), &checkpoint) {
                log::warn!("无法保存录制配置恢复副本：{error}");
            }
        }
        Ok(Self::new(root, tracking, config, progress))
    }

    #[cfg(test)]
    fn recheck_local_task(&self) -> Option<LocalTaskIssue> {
        Self::read_local_task(&self.root).err()
    }

    /// A blocked profile has no capture, upload or retention worker active.
    /// Prepare and verify the candidate before publishing it to the live app.
    pub(crate) fn repair_local_task(
        &self,
        paths: &super::storage::DataPaths,
        writer: &super::writer::WriterHandle,
    ) -> Result<bool, String> {
        if self.require_operable().is_ok() {
            return Ok(false);
        }
        let _transition = self.transition.try_lock_for(CAPTURE_GATE_WAIT)
            .ok_or("本机任务仍在处理，请稍后再试")?;
        let _gate = self.gate.try_write_for(CAPTURE_GATE_WAIT)
            .ok_or("本机任务仍在处理，请稍后再试")?;
        if self.require_operable().is_ok() {
            return Ok(false);
        }
        writer.flush()?;
        restore_local_task_files(&self.root)?;
        let candidate = Self::load_checked(&self.root, Arc::new(AtomicBool::new(false)))
            .map_err(|issue| issue.detail)?;
        let task = candidate.progress.lock().task_id.clone();
        let mut conn = super::frame_upload::db(paths)?;
        super::archive::initialize_owner(paths, &conn, &task)?;
        super::recovery::startup(paths, &mut conn, &candidate)?;
        let progress = candidate.progress.lock().clone();
        if progress.last_error.as_deref().is_some_and(|error| error.starts_with(PROGRESS_SAVE_ERROR)) {
            return Err("任务文件暂时无法保存，请释放磁盘空间或解除占用后重试".into());
        }
        *self.config.write() = candidate.config.read().clone();
        *self.progress.lock() = progress;
        *self.startup_issue.lock() = None;
        Ok(true)
    }

    pub(crate) fn repair_device_identity(&self) -> Result<bool, String> {
        self.require_operable()?;
        if self.config.read().upload_mode != "ossDirect"
            || !super::oss_direct::device_identity_needs_repair(&self.root)
        {
            return Ok(false);
        }
        let _gate = self.gate.try_write_for(CAPTURE_GATE_WAIT)
            .ok_or("截图正在保存，请稍后再试")?;
        let cfg = self.config.read().clone();
        if cfg.upload_mode != "ossDirect" {
            return Ok(false);
        }
        let task = self.progress.lock().task_id.clone();
        if super::oss_direct::repair_device_id(&self.root, &cfg, &task)? {
            *self.auto_failed_grant.lock() = None;
            self.cloud_status.lock().last_error = None;
            self.device_repaired.store(true, Ordering::SeqCst);
            return Ok(true);
        }
        Ok(false)
    }

    fn read_local_task(root: &Path) -> Result<(InternalConfig, TaskProgress, bool), LocalTaskIssue> {
        use LocalTaskIssueKind::*;
        let config_access = |error| LocalTaskIssue::new(ConfigAccess, format!("无法读取录制配置：{error}"));
        let progress_access = |error| LocalTaskIssue::new(ProgressAccess, format!("无法读取任务状态：{error}"));
        let cfg_path = root.join(CONFIG_FILE);
        let mut config: InternalConfig = if cfg_path.try_exists().map_err(config_access)? {
            serde_json::from_slice(&fs::read(&cfg_path).map_err(config_access)?)
                .map_err(|e| LocalTaskIssue::new(ConfigFormat, format!("内部版配置读取失败：第 {} 行，位置 {}", e.line(), e.column())))?
        } else if root.join(CONFIG_RECOVERY_FILE).try_exists().map_err(config_access)? {
            return Err(LocalTaskIssue::new(ConfigMissing, "原录制配置文件缺失"));
        } else {
            // Fresh profiles use OSS. Keep deserialization defaults for legacy receiver tasks.
            InternalConfig { upload_mode: "ossDirect".into(), ..InternalConfig::default() }
        };
        config.validate_capture().map_err(|error| LocalTaskIssue::new(ConfigValues, error))?;
        let owner = super::archive::read_owner(root).map_err(|error| LocalTaskIssue::new(OwnerInvalid, error))?;
        let progress_path = root.join(TASK_FILE);
        let mut progress: TaskProgress = if progress_path.try_exists().map_err(progress_access)? {
            serde_json::from_slice(&fs::read(&progress_path).map_err(progress_access)?)
                .map_err(|e| LocalTaskIssue::new(ProgressFormat, format!("任务状态读取失败：第 {} 行，位置 {}", e.line(), e.column())))?
        } else if owner.is_some()
            || root.join(COMPLETION_FILE).try_exists().map_err(progress_access)?
            || root.join(TASK_RECOVERY_FILE).try_exists().map_err(progress_access)?
        {
            return Err(LocalTaskIssue::new(ProgressMissing, "本机任务状态文件缺失，检测到已有任务"));
        } else {
            TaskProgress::fresh(config.target_hours)
        };
        uuid::Uuid::parse_str(&progress.task_id).map_err(|_| LocalTaskIssue::new(ProgressValues, "任务编号无效"))?;
        if progress.target_ms == 0
            || progress.target_ms > 48 * 3_600_000
            || progress.elapsed_ms > progress.target_ms
        {
            return Err(LocalTaskIssue::new(ProgressValues, "任务累计时长或目标时长无效"));
        }
        // Before any recorded time, configuration is the single durable source of the target.
        // Saving configuration must not require a transaction across two independent JSON files.
        let archive = super::archive::load(root).map_err(|error| LocalTaskIssue::new(ArchiveInvalid, error))?;
        if archive.as_ref().is_some_and(|b| b.task_id != progress.task_id)
            || owner.as_ref().is_some_and(|task| task != &progress.task_id)
        {
            return Err(LocalTaskIssue::new(IdentityMismatch, "任务编号与本机归属或归档记录不匹配"));
        }
        if let Some(target) = archive.as_ref().and_then(|b| b.target_ms) {
            if progress.elapsed_ms > target {
                return Err(LocalTaskIssue::new(ProgressValues, "任务累计时长超过已绑定目标"));
            }
            // Binding seals the current in-memory target, including a config
            // saved before any progress-file write. Older bindings use progress.
            progress.target_ms = target;
        }
        // A file-specific lock can prevent replacing the progress JSON while the
        // profile directory is still writable. Recover the terminal checkpoint.
        let completion_path = root.join(COMPLETION_FILE);
        let completion = fs::read(&completion_path).ok()
            .and_then(|bytes| serde_json::from_slice::<CompletionRecord>(&bytes).ok())
            .filter(|end| end.task_id == progress.task_id && end.target_ms == progress.target_ms
                && end.elapsed_ms >= progress.elapsed_ms && end.elapsed_ms <= end.target_ms
                && end.ended_at_ms > 0);
        if let Some(end) = &completion {
            if matches!(progress.status, TaskStatus::Ready | TaskStatus::Recording | TaskStatus::Paused) {
                progress.status = TaskStatus::Completed;
                progress.elapsed_ms = end.elapsed_ms;
                progress.ended_at_ms = Some(end.ended_at_ms);
                progress.needs_flush = true;
            }
        }
        if target_can_change(&progress, archive.is_some()) {
            progress.target_ms = config.target_hours as u64 * 3_600_000;
        } else if progress.target_ms % 3_600_000 == 0 {
            // The durable target of an existing task wins over the new default.
            config.target_hours = (progress.target_ms / 3_600_000) as u32;
        }
        // Relaunch never silently resumes collection or assumes an interrupted upload succeeded.
        if progress.status == TaskStatus::Recording {
            progress.status = TaskStatus::Paused;
            progress.needs_flush = true;
        }
        if progress.status == TaskStatus::Uploading {
            progress.status = TaskStatus::Completed;
        }
        if progress.elapsed_ms >= progress.target_ms
            && matches!(progress.status, TaskStatus::Ready | TaskStatus::Paused)
        {
            progress.status = TaskStatus::Completed;
        }
        Ok((config, progress, completion.is_some()))
    }

    fn new(root: &Path, tracking: Arc<AtomicBool>, config: InternalConfig, progress: TaskProgress) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(config),
            gate: RwLock::new(()),
            transition: Mutex::new(()),
            native_jobs: (Mutex::new(0), Condvar::new()),
            epoch: AtomicU64::new(0),
            tracking,
            frame_upload_active: AtomicBool::new(false),
            frame_upload_running: AtomicBool::new(false),
            device_repaired: AtomicBool::new(false),
            cloud_refresh: Mutex::new(()),
            cloud_status: Mutex::new(super::cloud_auth::CloudStatus::default()),
            auto_failed_grant: Mutex::new(None),
            progress: Mutex::new(progress),
            capture_notice: Mutex::new(super::desktop::CaptureNotice::default()),
            startup_issue: Mutex::new(None),
            root: root.to_owned(),
        })
    }

    pub fn save_config(&self, mut config: InternalConfig) -> Result<(), String> {
        self.require_operable()?;
        config.participant_name = config.participant_name.trim().to_string();
        config.require_participant_name()?;
        config.validate()?;
        let _guard = self.gate.try_write_for(CAPTURE_GATE_WAIT)
            .ok_or("采集仍在保存，请稍后再修改设置")?;
        if let Some(b)=super::archive::load(&self.root)? { if b.upload_mode!=config.upload_mode || b.upload_url!=config.upload_url || b.survey_id!=config.upload_survey_id || b.device_id!=config.upload_device_id || b.planned_end_ms!=super::archive::end_ms(&config)? { return Err("归档已绑定，不能修改目的地或结束时间".into()); } }
        if self.frame_upload_running.load(Ordering::SeqCst) {
            return Err("请先停止后台上传，再修改设置".into());
        }
        let mut p = self.progress.lock();
        if matches!(p.status, TaskStatus::Recording | TaskStatus::Uploading) {
            return Err("请先暂停，上传期间不能修改配置".into());
        }
        let may_change_target = target_can_change(&p, super::archive::load(&self.root)?.is_some());
        if !may_change_target && config.target_hours != self.config.read().target_hours {
            return Err("任务开始、结束或绑定归档后不能修改目标时长".into());
        }
        if p.stored_bundle
            .as_ref()
            .is_some_and(|b| b.recipient != config.encryption_recipient.trim())
        {
            return Err("已有待回收数据包，修改加密公钥前需要人工审核回收状态".into());
        }
        write_config(&self.root, &config, &p.task_id)?;
        if may_change_target {
            p.target_ms = config.target_hours as u64 * 3_600_000;
        }
        *self.config.write() = config;
        Ok(())
    }

    /// Name-only onboarding never sends a stale full config back from the UI.
    pub fn save_participant_name(&self,name:&str)->Result<(),String> {
        self.require_operable()?;
        let name=name.trim();validate_participant_name(name)?;
        if name.is_empty() {return Err("请填写姓名".into());}
        let _guard=self.gate.try_write_for(CAPTURE_GATE_WAIT).ok_or("采集仍在保存，请稍后再试")?;
        let mut cfg=self.config.read().clone();
        if cfg.participant_name==name {return Ok(());}
        let p=self.progress.lock();
        if self.frame_upload_running.load(Ordering::SeqCst) || matches!(p.status,TaskStatus::Recording|TaskStatus::Uploading|TaskStatus::Uploaded) || p.receipt.is_some() || p.stored_bundle.is_some() || p.bundle_path.is_some() {return Err("任务进行中不能修改姓名，请先联系管理员".into());}
        cfg.participant_name=name.into();cfg.validate_capture()?;
        write_config(&self.root, &cfg, &p.task_id)?;
        *self.config.write()=cfg;Ok(())
    }

    pub fn start(&self) -> Result<(), String> {
        self.require_operable()?;
        let _transition = self.transition.try_lock_for(CAPTURE_GATE_WAIT)
            .ok_or("采集状态仍在切换，请稍后重试")?;
        let _guard = self.gate.try_write_for(CAPTURE_GATE_WAIT)
            .ok_or("上一次截图仍在退出，请稍后再继续采集")?;
        self.config.read().require_participant_name()?;
        let mut p = self.progress.lock();
        if p.needs_flush {
            return Err("停止落盘尚未完成，请先重试暂停保存".into());
        }
        if !matches!(p.status, TaskStatus::Ready | TaskStatus::Paused)
            || p.elapsed_ms >= p.target_ms
            || p.receipt.is_some()
            || p.stored_bundle.is_some()
            || p.bundle_path.is_some()
        {
            return Err("任务已结束，不能继续采集".into());
        }
        let now = chrono::Utc::now().timestamp_millis();
        super::archive::ensure_live(&self.root, &p.task_id, now)?;
        let config = self.config.read();
        if config.upload_mode == "ossDirect"
            && super::archive::end_ms(&config).is_ok_and(|end| now >= end + super::archive::RETENTION_MS)
        {
            return Err("归档任务已到删除截止，不能继续采集".into());
        }
        let mut next = p.clone();
        next.status = TaskStatus::Recording;
        next.last_error = None;
        next.needs_flush = true;
        write_task(&self.root, &next, true)?;
        *p = next;
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.capture_notice.lock() = super::desktop::CaptureNotice::default();
        self.tracking.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn pause(&self, complete: bool) -> Result<(), String> {
        self.require_operable()?;
        self.tracking.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.capture_notice.lock() = super::desktop::CaptureNotice::default();
        let Some(_guard) = self.gate.try_write_for(CAPTURE_GATE_WAIT) else {
            if let Some(mut progress) = self.progress.try_lock() {
                if progress.status == TaskStatus::Recording {
                    progress.status = TaskStatus::Paused;
                }
            }
            return Err("采集已停止，正在等待上一次截图释放文件；请重试暂停保存".into());
        };
        let mut p = self.progress.lock();
        if matches!(
            p.status,
            TaskStatus::Ready | TaskStatus::Recording | TaskStatus::Paused
        ) {
            if complete && p.ended_at_ms.is_none() { p.ended_at_ms=Some(chrono::Utc::now().timestamp_millis()); }
            p.status = if complete {
                TaskStatus::Completed
            } else {
                TaskStatus::Paused
            };
        }
        self.write_progress(&p, true)?;
        drop(p);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut count = self.native_jobs.0.lock();
        while *count > 0 {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("系统采集任务尚未退出，未开始上传或清理；请重试暂停保存".into());
            }
            self.native_jobs.1.wait_for(&mut count, remaining);
        }
        Ok(())
    }

    pub fn active(&self, expected_epoch: u64) -> Option<RwLockReadGuard<'_, ()>> {
        if !self.is_current(expected_epoch) {
            return None;
        }
        let guard = self.gate.read();
        if !self.tracking.load(Ordering::SeqCst)
            || self.epoch.load(Ordering::SeqCst) != expected_epoch
        {
            None
        } else {
            Some(guard)
        }
    }

    pub fn is_current(&self, epoch: u64) -> bool {
        self.tracking.load(Ordering::SeqCst) && self.epoch.load(Ordering::SeqCst) == epoch
    }

    pub fn note_capture(&self, epoch: u64, code: &str) {
        let mut notice = self.capture_notice.lock();
        if self.is_current(epoch) {
            if capture_has_failed(&notice.code) && !capture_has_failed(code) && code != "saved" {
                return;
            }
            *notice = super::desktop::CaptureNotice {
                code: code.into(),
                at_ms: chrono::Utc::now().timestamp_millis(),
            };
        }
    }

    pub fn begin_native(&self, epoch: u64) -> Option<NativeJob<'_>> {
        let _active = self.active(epoch)?;
        *self.native_jobs.0.lock() += 1;
        Some(NativeJob(self))
    }

    pub fn ocr_temp_dir(&self) -> PathBuf {
        self.root.join("data").join("tmp")
    }

    fn completion_pending(&self) -> bool {
        let p = self.progress.lock();
        p.status == TaskStatus::Completed && p.needs_flush
    }

    fn write_progress(&self, progress: &TaskProgress, checkpoint: bool) -> Result<(), String> {
        let result = write_task(&self.root, progress, checkpoint);
        if result.is_err() {
            if let Some(end) = completion_record(progress) {
                if let Err(error) = write_json_atomic(&self.root.join(COMPLETION_FILE), &end) {
                    log::warn!("任务结束恢复记录写入失败：{error}");
                }
            }
        }
        result
    }

    /// Persist before publishing state to other threads; failed writes leave the prior state retryable.
    pub fn edit_progress(
        &self,
        edit: impl FnOnce(&mut TaskProgress) -> Result<(), String>,
    ) -> Result<(), String> {
        self.require_operable()?;
        let mut p = self.progress.lock();
        let mut next = p.clone();
        edit(&mut next)?;
        self.write_progress(&next, true)?;
        *p = next;
        Ok(())
    }

    /// Called with monotonic elapsed time. Long sleep/suspend gaps do not count as work.
    pub fn advance(&self, delta_ms: u64) -> Result<bool, String> {
        // Nonterminal ticks must not wait on a slow capture. At completion,
        // stamp the end only after the current capture has committed.
        let terminal={let p=self.progress.lock();p.elapsed_ms.saturating_add(delta_ms)>=p.target_ms};
        let _gate=if terminal { match self.gate.try_write(){Some(g)=>Some(g),None=>return Ok(false)} } else {None};
        let mut p = self.progress.lock();
        if !self.tracking.load(Ordering::SeqCst) || delta_ms > 5_000
            || capture_has_failed(&self.capture_notice.lock().code)
        {
            return Ok(false);
        }
        let previous_elapsed = p.elapsed_ms;
        p.elapsed_ms = p.elapsed_ms.saturating_add(delta_ms).min(p.target_ms);
        let ended = p.elapsed_ms >= p.target_ms;
        if ended {
            if p.ended_at_ms.is_none() { p.ended_at_ms=Some(chrono::Utc::now().timestamp_millis()); }
            p.status = TaskStatus::Completed;
            self.tracking.store(false, Ordering::SeqCst);
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
        if p.last_error.as_deref().is_some_and(|e| e.starts_with(PROGRESS_SAVE_ERROR)) {
            p.last_error = None;
        }
        let checkpoint = ended || p.elapsed_ms / PROGRESS_CHECKPOINT_MS != previous_elapsed / PROGRESS_CHECKPOINT_MS;
        if let Err(e) = self.write_progress(&p, checkpoint) {
            p.last_error = Some(format!("{PROGRESS_SAVE_ERROR}，可写入后将重试：{e}"));
            return Err(e);
        }
        Ok(ended)
    }

    pub fn persist_progress(&self) -> Result<(), String> {
        self.require_operable()?;
        self.write_progress(&self.progress.lock(), true)
    }
}

#[cfg(test)]
pub(crate) fn fill_test_participant_name(control: &InternalControl) {
    let mut cfg = control.config.read().clone();
    if cfg.participant_name.is_empty() {
        cfg.participant_name = "测试用户".into();
        control.save_config(cfg).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_click_recovery_reuses_task_progress_privacy_rules_and_pending_frames() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let paths = crate::core::storage::DataPaths {
            root: root.to_owned(), data_dir: root.join("data"), shots_dir: root.join("data/shots"),
            db_path: root.join("data/db.sqlite"), exports_dir: root.join("exports"),
        };
        paths.ensure_dirs().unwrap();
        let conn = crate::core::storage::db::open_write(&paths.db_path).unwrap();
        let clock = Arc::new(AtomicBool::new(false));
        let original = InternalControl::load(root, clock.clone()).unwrap();
        let task = original.progress.lock().task_id.clone();
        crate::core::archive::initialize_owner(&paths, &conn, &task).unwrap();
        let mut config = original.config.read().clone();
        config.participant_name = "测试员工".into();
        config.auto_upload_enabled = false;
        config.screenshot_blocked_apps.push("test-private-application".into());
        original.save_config(config).unwrap();
        original.start().unwrap();
        original.advance(2000).unwrap();
        original.pause(false).unwrap();

        let id = uuid::Uuid::new_v4().to_string();
        let day = crate::core::storage::private_subdir(&paths.shots_dir, "1970-01-01").unwrap();
        let path = day.join(format!("{id}_display-1_poll.webp"));
        let bytes = webp::Encoder::from_rgb(&[0; 12], 2, 2).encode(85.).to_vec();
        let row = crate::core::models::SnapshotRow {
            id, session_id: "synthetic-session".into(), file_path: path.to_string_lossy().into(),
            captured_at_ms: 1000, file_size_bytes: bytes.len() as i64, trigger_type: "poll".into(),
            resolution: Some("2x2".into()), format: "webp".into(), perceptual_hash: None,
        };
        let draft = crate::core::frame_upload::UploadDraft::new(&task, "display-1", &bytes, "测试员工").unwrap();
        crate::core::recovery::stage(&paths, &row, &draft, &bytes).unwrap();
        drop(original);
        fs::write(root.join(CONFIG_FILE), b"bad config").unwrap();
        fs::write(root.join(TASK_FILE), b"bad task").unwrap();
        let opened = InternalControl::open_for_app(root, clock.clone());
        assert!(opened.require_operable().is_err());
        let writer = crate::core::writer::spawn_writer_thread(conn, Arc::new(Default::default()));
        assert!(opened.repair_local_task(&paths, &writer).unwrap());
        assert!(!opened.repair_local_task(&paths, &writer).unwrap());
        assert!(!clock.load(Ordering::SeqCst));
        let p = opened.progress.lock().clone();
        assert_eq!(p.task_id, task);
        assert_eq!(p.elapsed_ms, 2000);
        assert_eq!(p.status, TaskStatus::Paused);
        assert!(!p.needs_flush);
        assert!(opened.config.read().screenshot_blocked_apps.iter().any(|app| app == "test-private-application"));
        assert!(!opened.config.read().auto_upload_enabled);
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert_eq!(crate::core::frame_upload::stats(&crate::core::frame_upload::db(&paths).unwrap(), &task).unwrap().pending, 1);
        assert_eq!(fs::read_dir(root.join("local-repair")).unwrap().count(), 2);
        opened.start().unwrap();
    }

    #[test]
    fn repair_rejects_a_foreign_checkpoint_and_preserves_the_original_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let control = InternalControl::load(root, Arc::new(AtomicBool::new(false))).unwrap();
        write_json_atomic(&root.join("archive-profile-owner.json"), &control.progress.lock().task_id).unwrap();
        let foreign = TaskProgress::fresh(48);
        write_json_atomic(&root.join(TASK_RECOVERY_FILE), &foreign).unwrap();
        fs::write(root.join(TASK_FILE), b"broken original").unwrap();
        assert!(restore_local_task_files(root).unwrap_err().contains("未通过校验"));
        assert_eq!(fs::read(root.join(TASK_FILE)).unwrap(), b"broken original");
        assert!(!root.join("local-repair").exists());
    }

    #[test]
    fn completed_task_remains_ended_when_its_primary_file_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let control = InternalControl::load(root, Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&control);
        let task = control.progress.lock().task_id.clone();
        write_json_atomic(&root.join("archive-profile-owner.json"), &task).unwrap();
        control.start().unwrap();
        control.advance(1000).unwrap();
        control.pause(true).unwrap();
        let ended_at = control.progress.lock().ended_at_ms;
        fs::remove_file(root.join(TASK_FILE)).unwrap();
        restore_local_task_files(root).unwrap();
        let restored = InternalControl::load(root, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(restored.progress.lock().ended_at_ms, ended_at);
        assert_eq!(restored.progress.lock().status, TaskStatus::Completed);
        assert!(restored.start().is_err());
    }

    #[test]
    fn temporary_checkpoint_write_failure_does_not_stop_local_recording() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let control = InternalControl::load(root, Arc::new(AtomicBool::new(false))).unwrap();
        let unlock_config = block_write_target(&root.join(CONFIG_RECOVERY_FILE));
        fill_test_participant_name(&control);
        assert!(root.join(CONFIG_FILE).is_file());
        unlock_config();
        let unlock = block_write_target(&root.join(TASK_RECOVERY_FILE));
        control.start().unwrap();
        control.advance(1000).unwrap();
        assert!(control.tracking.load(Ordering::SeqCst));
        control.pause(true).unwrap();
        assert!(root.join(COMPLETION_FILE).exists());
        unlock();
    }

    #[test]
    fn invalid_upload_config_does_not_block_local_capture() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = InternalConfig {
            participant_name: "测试用户".into(),
            upload_mode: "ossDirect".into(),
            upload_url: "http://invalid.example/".into(),
            ..InternalConfig::default()
        };
        write_json_atomic(&dir.path().join("internal-config.json"), &cfg).unwrap();
        let control = InternalControl::open_for_app(dir.path(), Arc::new(AtomicBool::new(false)));
        assert!(control.config.read().validate_upload().is_err());
        control.start().unwrap();
        assert!(control.tracking.load(Ordering::SeqCst));
    }

    #[test]
    fn corrupt_task_opens_for_review_without_overwriting_local_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("internal-task.json");
        fs::write(&path, b"{truncated").unwrap();
        let tracking = Arc::new(AtomicBool::new(true));
        let control = InternalControl::open_for_app(dir.path(), tracking.clone());
        assert!(!tracking.load(Ordering::SeqCst));
        assert!(control.start().unwrap_err().contains("任务状态读取失败"));
        assert!(control.save_participant_name("测试用户").is_err());
        assert!(control.persist_progress().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{truncated");
    }

    #[test]
    fn recheck_reads_repaired_files_without_writing_or_starting_capture() {
        let dir = tempfile::tempdir().unwrap();
        let tracking = Arc::new(AtomicBool::new(false));
        let original = InternalControl::load(dir.path(), tracking.clone()).unwrap();
        fill_test_participant_name(&original);
        original.start().unwrap();
        original.advance(1000).unwrap();
        let task = original.progress.lock().task_id.clone();
        let path = dir.path().join("internal-task.json");
        let good_progress = fs::read(&path).unwrap();
        write_json_atomic(&dir.path().join("archive-profile-owner.json"), &task).unwrap();
        fs::write(&path, b"broken").unwrap();
        let control = InternalControl::open_for_app(dir.path(), tracking.clone());
        assert_eq!(control.recheck_local_task().unwrap().kind, LocalTaskIssueKind::ProgressFormat);
        fs::write(&path, &good_progress).unwrap();
        assert!(control.recheck_local_task().is_none());
        assert!(!tracking.load(Ordering::SeqCst));
        assert_eq!(fs::read(&path).unwrap(), good_progress);
        let reopened = InternalControl::load(dir.path(), tracking.clone()).unwrap();
        let restored = reopened.progress.lock();
        assert_eq!(restored.task_id, task);
        assert_eq!(restored.elapsed_ms, 1000);
        assert_eq!(restored.status, TaskStatus::Paused);
        assert!(!tracking.load(Ordering::SeqCst));
    }

    #[test]
    fn recheck_distinguishes_invalid_values_missing_progress_and_mixed_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let control = InternalControl::load(dir.path(), Arc::new(AtomicBool::new(false))).unwrap();
        let cfg_path = dir.path().join("internal-config.json");
        fs::write(&cfg_path, b"{incomplete").unwrap();
        assert_eq!(control.recheck_local_task().unwrap().kind, LocalTaskIssueKind::ConfigFormat);
        let mut cfg = control.config.read().clone();
        cfg.capture_interval_secs = 1;
        write_json_atomic(&cfg_path, &cfg).unwrap();
        assert_eq!(control.recheck_local_task().unwrap().kind, LocalTaskIssueKind::ConfigValues);
        fs::remove_file(&cfg_path).unwrap();

        let progress_path = dir.path().join("internal-task.json");
        let good_progress = fs::read(&progress_path).unwrap();
        let owner_path = dir.path().join("archive-profile-owner.json");
        write_json_atomic(&owner_path, &uuid::Uuid::new_v4().to_string()).unwrap();
        assert_eq!(control.recheck_local_task().unwrap().kind, LocalTaskIssueKind::IdentityMismatch);
        fs::remove_file(&progress_path).unwrap();
        assert_eq!(control.recheck_local_task().unwrap().kind, LocalTaskIssueKind::ProgressMissing);
        assert!(!progress_path.exists());
        fs::write(&progress_path, &good_progress).unwrap();
        fs::write(&owner_path, b"broken").unwrap();
        assert_eq!(control.recheck_local_task().unwrap().kind, LocalTaskIssueKind::OwnerInvalid);
        fs::remove_file(&owner_path).unwrap();
        fs::write(dir.path().join("archive-binding.json"), b"broken").unwrap();
        assert_eq!(control.recheck_local_task().unwrap().kind, LocalTaskIssueKind::ArchiveInvalid);
        assert_eq!(fs::read(&progress_path).unwrap(), good_progress);
    }

    #[test]
    fn capture_fault_stops_counting_until_a_frame_is_durably_saved() {
        let dir = tempfile::tempdir().unwrap();
        let control = InternalControl::load(dir.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&control);
        control.start().unwrap();
        let epoch = control.epoch.load(Ordering::SeqCst);
        control.note_capture(epoch, "save_error");
        control.note_capture(epoch, "privacy");
        control.advance(1000).unwrap();
        assert_eq!(control.progress.lock().elapsed_ms, 0);
        control.note_capture(epoch, "saved");
        control.advance(1000).unwrap();
        assert_eq!(control.progress.lock().elapsed_ms, 1000);
    }

    // Block the final rename rather than depending on a temporary filename.
    // Preserve the previous durable state so recovery assertions remain meaningful.
    fn block_write_target(path: &Path) -> impl FnOnce() {
        let path = path.to_owned();
        let previous = fs::read(&path).ok();
        if previous.is_some() { fs::remove_file(&path).unwrap(); }
        fs::create_dir(&path).unwrap();
        move || {
            fs::remove_dir(&path).unwrap();
            if let Some(bytes) = previous { fs::write(&path, bytes).unwrap(); }
        }
    }

    #[test]
    fn name_is_empty_on_upgrade_but_required_before_direct_actions() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(c.config.read().participant_name, "");
        let blank = c.config.read().clone();
        assert!(c.save_config(blank).unwrap_err().contains("姓名"));
        assert!(c.start().unwrap_err().contains("姓名"));
        assert!(!c.tracking.load(Ordering::SeqCst));
        let mut cfg = c.config.read().clone();
        cfg.participant_name = "  张三  ".into();
        c.save_config(cfg).unwrap();
        assert_eq!(c.config.read().participant_name, "张三");
        fill_test_participant_name(&c);
        c.start().unwrap();
        c.pause(false).unwrap();
        assert_eq!(InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap().config.read().participant_name, "张三");
        let old = serde_json::json!({"uploadMode":"ossDirect"});
        let cfg: InternalConfig = serde_json::from_value(old).unwrap();
        assert!(cfg.validate().is_ok());
        assert!(cfg.require_participant_name().is_err());
    }

    #[test]
    fn participant_names_reject_unsafe_or_oversized_segments() {
        for name in ["", "张三", "John Smith", "O’Neil", "张三 · A"] {
            validate_participant_name(name).unwrap();
        }
        validate_participant_name(&"A".repeat(64)).unwrap();
        for name in [".", "..", "../x", "a/b", "a\\b", " 张三", "张三 ", "a\nb", "\u{202e}张三", "\u{200b}张三"] {
            assert!(validate_participant_name(name).is_err(), "{name:?}");
        }
        assert!(validate_participant_name(&"A".repeat(65)).is_err());
    }

    #[test]
    fn configuration_writes_cannot_race_bundle_or_cleanup() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert!(c.configuration_write().is_ok());
        let cleanup = c.gate.write();
        assert!(c.configuration_write().is_err());
        drop(cleanup);
        for status in [TaskStatus::Uploading, TaskStatus::Uploaded] {
            c.progress.lock().status = status;
            assert!(c.configuration_write().is_err());
        }
        c.progress.lock().status = TaskStatus::Completed;
        c.progress.lock().receipt = Some(crate::core::transfer::UploadReceipt {
            task_id: "fixture".into(),
            sha256: "fixture".into(),
            size_bytes: 1,
            stored: true,
        });
        assert!(c.configuration_write().is_err());
    }
    #[test]
    fn pause_cancels_new_native_jobs_and_waits_for_existing_job_cleanup() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        let epoch = c.epoch.load(Ordering::SeqCst);
        let job = c.begin_native(epoch).unwrap();
        let other = c.clone();
        let (done, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            done.send(other.pause(false)).unwrap();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while c.is_current(epoch) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!c.is_current(epoch));
        assert!(c.begin_native(epoch).is_none());
        assert!(rx.try_recv().is_err());
        drop(job);
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn fresh_profiles_use_oss_while_legacy_receiver_configs_keep_their_mode() {
        let fresh = tempfile::tempdir().unwrap();
        let control = InternalControl::load(fresh.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(control.config.read().upload_mode, "ossDirect");
        assert_eq!(control.progress.lock().target_ms, 48 * 3_600_000);
        assert_eq!(control.progress.lock().status, TaskStatus::Ready);
        assert!(!control.tracking.load(Ordering::SeqCst));
        assert!(!control.frame_upload_active.load(Ordering::SeqCst));
        assert!(!control.frame_upload_running.load(Ordering::SeqCst));
        drop(control);
        for saved in [r#"{"workdayHours":8}"#, r#"{"uploadMode":"receiver"}"#] {
            let legacy = tempfile::tempdir().unwrap();
            fs::write(legacy.path().join("internal-config.json"), saved).unwrap();
            let control = InternalControl::load(legacy.path(), Arc::new(AtomicBool::new(false))).unwrap();
            assert_eq!(control.config.read().upload_mode, "receiver");
            assert!(!control.frame_upload_active.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn target_config_accepts_legacy_keys_but_new_tasks_default_to_48_hours() {
        let legacy: InternalConfig = serde_json::from_str(r#"{"workdayHours":8}"#).unwrap();
        assert_eq!(legacy.target_hours, 48);
        assert!(legacy.validate().is_ok());
        let encoded = serde_json::to_value(&legacy).unwrap();
        assert_eq!(encoded["targetHours"], 48);
        assert!(encoded.get("workdayHours").is_none());
        assert!(serde_json::from_str::<InternalConfig>(r#"{"unknownTarget":48}"#).is_err());
        for hours in [0, 1, 47, 48, 49] {
            let mut c = InternalConfig::default();
            c.target_hours = hours;
            assert_eq!(c.validate().is_ok(), (1..=48).contains(&hours));
        }
    }

    #[test]
    fn unstarted_legacy_task_uses_48_hours_without_changing_its_identity() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("internal-config.json"), r#"{"workdayHours":8}"#).unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(c.progress.lock().target_ms, 48 * 3_600_000);
        let task = c.progress.lock().task_id.clone();
        c.edit_progress(|p| {p.target_ms = 16 * 3_600_000; Ok(())}).unwrap();
        drop(c);
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(restored.progress.lock().task_id, task);
        assert_eq!(restored.progress.lock().target_ms, 48 * 3_600_000);
        assert!(!restored.tracking.load(Ordering::SeqCst));
    }

    #[test]
    fn started_ended_and_pending_legacy_tasks_keep_their_durable_target() {
        for (status, elapsed, ended, needs_flush) in [
            (TaskStatus::Paused, 1000, None, false),
            (TaskStatus::Paused, 0, None, false),
            (TaskStatus::Completed, 0, Some(1), false),
            (TaskStatus::Completed, 0, None, false),
            (TaskStatus::Paused, 0, None, true),
        ] {
            let d = tempfile::tempdir().unwrap();
            let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
            c.edit_progress(|p| {
                p.target_ms = 16 * 3_600_000;
                p.status = status; p.elapsed_ms = elapsed;
                p.ended_at_ms = ended; p.needs_flush = needs_flush; Ok(())
            }).unwrap();
            drop(c);
            let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
            assert_eq!(restored.progress.lock().target_ms, 16 * 3_600_000);
            assert_eq!(restored.config.read().target_hours, 16);
            assert_eq!(restored.progress.lock().ended_at_ms, ended);
            let mut cfg = restored.config.read().clone();
            cfg.target_hours = 48;
            assert!(restored.save_config(cfg).is_err());
            assert_eq!(restored.progress.lock().target_ms, 16 * 3_600_000);
        }
    }

    #[test]
    fn clean_pause_before_first_tick_still_freezes_the_started_target() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        c.pause(false).unwrap();
        c.edit_progress(|p| {p.needs_flush = false; Ok(())}).unwrap();
        assert_eq!(c.progress.lock().elapsed_ms, 0);
        let mut cfg = c.config.read().clone();
        cfg.target_hours = 47;
        assert!(c.save_config(cfg).is_err());
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(restored.progress.lock().target_ms, 48 * 3_600_000);
    }

    #[test]
    fn forty_eight_hour_boundary_stops_once_and_does_not_count_sleep_or_pause() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        c.edit_progress(|p| {p.elapsed_ms = p.target_ms - 1000; Ok(())}).unwrap();
        assert!(!c.advance(60_000).unwrap());
        assert_eq!(c.progress.lock().elapsed_ms, 48 * 3_600_000 - 1000);
        assert!(!c.advance(999).unwrap());
        assert!(c.progress.lock().ended_at_ms.is_none());
        assert!(c.advance(1).unwrap());
        let ended = c.progress.lock().ended_at_ms;
        assert!(ended.is_some());
        assert_eq!(c.progress.lock().status, TaskStatus::Completed);
        assert!(!c.tracking.load(Ordering::SeqCst));
        assert!(!c.advance(1000).unwrap());
        assert_eq!(c.progress.lock().ended_at_ms, ended);
        c.edit_progress(|p| {p.needs_flush = false; Ok(())}).unwrap();
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(restored.progress.lock().target_ms, 48 * 3_600_000);
        assert_eq!(restored.progress.lock().ended_at_ms, ended);
        assert!(restored.start().is_err());
        assert!(!restored.advance(1000).unwrap());
    }

    #[test]
    fn config_target_has_one_durable_source_before_recording() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        let mut config = InternalConfig::default();
        config.target_hours = 12;
        // Configuration saving remains atomic even when the unrelated progress file is unwritable.
        let restore_target = block_write_target(&d.path().join("internal-task.json"));
        c.save_config(config.clone()).unwrap();
        assert_eq!(c.progress.lock().target_ms, 12 * 3_600_000);
        restore_target();
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(restored.progress.lock().target_ms, 12 * 3_600_000);
        let restore_target = block_write_target(&d.path().join("internal-config.json"));
        config.target_hours = 14;
        assert!(restored.save_config(config).is_err());
        assert_eq!(restored.config.read().target_hours, 12);
        assert_eq!(restored.progress.lock().target_ms, 12 * 3_600_000);
        restore_target();
    }
    #[test]
    fn clean_pause_can_resume_after_restart_but_unclean_task_waits_for_recovery() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        c.pause(false).unwrap();
        // The application clears this only after aggregation and writer barriers acknowledge.
        c.edit_progress(|p| {
            p.needs_flush = false;
            Ok(())
        })
        .unwrap();
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&restored);
        restored.start().unwrap();
        let unclean = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert!(unclean.progress.lock().needs_flush);
        assert!(unclean.progress.lock().integrity_error.is_none());
        assert!(unclean.start().is_err());
    }
    #[test]
    fn temporary_progress_save_failure_keeps_recording_and_retries_on_next_tick() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        let epoch = c.epoch.load(Ordering::SeqCst);
        let restore_target = block_write_target(&d.path().join("internal-task.json"));
        assert!(c.advance(1000).is_err());
        assert!(c.is_current(epoch));
        assert_eq!(c.progress.lock().status, TaskStatus::Recording);
        assert!(c.progress.lock().last_error.is_some());
        restore_target();
        assert!(!c.advance(1000).unwrap());
        assert!(c.progress.lock().last_error.is_none());
        let disk: TaskProgress = serde_json::from_slice(
            &fs::read(d.path().join("internal-task.json")).unwrap(),
        ).unwrap();
        assert_eq!(disk.elapsed_ms, 2000);
    }
    #[test]
    fn start_and_recovery_state_save_failures_remain_retryable() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        let restore_target = block_write_target(&d.path().join("internal-task.json"));
        assert!(c.start().is_err());
        assert_eq!(c.progress.lock().status, TaskStatus::Ready);
        assert!(!c.tracking.load(Ordering::SeqCst));
        c.progress.lock().status = TaskStatus::Completed;
        assert!(c
            .edit_progress(|p| {
                p.status = TaskStatus::Uploading;
                Ok(())
            })
            .is_err());
        assert_eq!(c.progress.lock().status, TaskStatus::Completed);
        restore_target();
        c.edit_progress(|p| {
            p.status = TaskStatus::Uploading;
            Ok(())
        })
        .unwrap();
        assert_eq!(c.progress.lock().status, TaskStatus::Uploading);
    }
    #[test]
    fn deadline_stays_terminal_when_progress_save_fails() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.edit_progress(|p| {
            p.target_ms = 1000;
            p.status = TaskStatus::Paused;
            Ok(())
        }).unwrap();
        c.start().unwrap();
        let restore_target = block_write_target(&d.path().join("internal-task.json"));
        assert!(c.advance(1000).is_err());
        assert_eq!(c.progress.lock().status, TaskStatus::Completed);
        assert!(!c.tracking.load(Ordering::SeqCst));
        assert!(c.completion_pending());
        assert!(c.pause(true).is_err());
        assert!(!c.advance(1000).unwrap());
        assert!(c.completion_pending());
        assert!(c.pause(true).is_err());
        restore_target();
        assert!(!c.advance(1000).unwrap());
        assert!(c.completion_pending());
        c.pause(true).unwrap();
        c.edit_progress(|p| {
            p.needs_flush = false;
            Ok(())
        }).unwrap();
        let reopened = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(reopened.progress.lock().elapsed_ms, 1000);
        assert_eq!(reopened.progress.lock().status, TaskStatus::Completed);
        assert!(reopened.progress.lock().ended_at_ms.is_some());
        assert!(!reopened.completion_pending());
        assert!(reopened.start().is_err());
    }
    #[test]
    fn active_capture_does_not_block_nonterminal_clock_progress() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        let _active_capture = c.active(c.epoch.load(Ordering::SeqCst)).unwrap();
        assert!(!c.advance(1000).unwrap());
        assert_eq!(c.progress.lock().elapsed_ms, 1000);
    }

    #[test]
    fn completion_survives_restart_while_primary_progress_was_locked() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.edit_progress(|p| {
            p.target_ms = 1000;
            p.status = TaskStatus::Paused;
            Ok(())
        }).unwrap();
        c.start().unwrap();
        let task = c.progress.lock().task_id.clone();
        let unlock = block_write_target(&d.path().join("internal-task.json"));
        assert!(c.advance(1000).is_err());
        assert!(d.path().join(COMPLETION_FILE).exists());
        let ended_at_ms = c.progress.lock().ended_at_ms;
        drop(c);
        unlock();
        let reopened = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        let progress = reopened.progress.lock().clone();
        assert_eq!(progress.task_id, task);
        assert_eq!(progress.status, TaskStatus::Completed);
        assert_eq!(progress.elapsed_ms, 1000);
        assert_eq!(progress.ended_at_ms, ended_at_ms);
        assert!(!d.path().join(COMPLETION_FILE).exists());
        assert!(reopened.start().is_err());
    }
    #[test]
    fn duration_survives_restart_without_resuming() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        c.advance(2_000).unwrap();
        c.advance(60_000).unwrap();
        assert_eq!(c.progress.lock().elapsed_ms, 2_000);
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(true))).unwrap();
        assert_eq!(restored.progress.lock().status, TaskStatus::Paused);
        assert!(!restored.tracking.load(Ordering::SeqCst));
        assert_eq!(restored.progress.lock().elapsed_ms, 2_000);
    }
    #[test]
    fn deadline_blocks_resume_and_invalidates_pending_capture() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fill_test_participant_name(&c);
        c.start().unwrap();
        let epoch = c.epoch.load(Ordering::SeqCst);
        c.progress.lock().target_ms = 1_000;
        assert!(c.advance(1_000).unwrap());
        assert!(c.active(epoch).is_none());
        assert!(c.start().is_err());
    }
    #[test]
    fn privacy_rules_fail_closed_and_match_domain_boundaries() {
        let c = InternalConfig::default();
        assert!(c.blocks_screenshot("WeChat", None, "", None, false));
        assert!(c.blocks_screenshot("Chrome", None, "", None, true));
        assert!(c.blocks_screenshot("Chrome", None, "", Some("not a URL"), true));
        assert!(c.blocks_screenshot("Chrome", None, "", Some("https://my.icbc.com.cn/a"), true));
        assert!(c.blocks_screenshot("Chrome", None, "", Some("//my.icbc.com.cn/a"), true));
        assert!(!c.blocks_screenshot("Chrome", None, "", Some("//icbc.com.cn.example.org/a"), true));
        for title in ["password-test.txt", "PASSWORD", "登录", "VERIFICATION CODE"] {
            assert!(c.blocks_screenshot("notepad", None, title, None, false));
            assert_eq!(super::super::privacy::redact_title(title), "[redacted]");
        }
        assert!(!c.blocks_screenshot(
            "Chrome",
            None,
            "",
            Some("https://icbc.com.cn.example.org/a"),
            true
        ));
    }
    #[test]
    fn config_rejects_cleartext_credentials_and_invalid_capture_parameters() {
        let mut c = InternalConfig::default();
        c.capture_interval_secs = 9;
        assert!(c.validate().is_err());
        c.capture_interval_secs = 12;
        c.upload_url = "https://user:password@example.com/upload".into();
        assert!(c.validate().is_err());
        c.upload_url = "http://example.com/upload".into();
        assert!(c.validate().is_err());
    }
}
