//! Internal two-workday collection policy. No network or operating-system capture in this module.
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
    pub capture_interval_secs: u64,
    pub poll_width: u32,
    pub poll_quality: f32,
    pub workday_hours: u32,
    /// Native APIs capture the display containing the foreground window by default.
    pub all_displays: bool,
    pub screenshot_blocked_apps: Vec<String>,
    pub screenshot_blocked_domains: Vec<String>,
    pub upload_url: String,
    pub upload_survey_id: String,
    pub upload_device_id: String,
    pub encryption_recipient: String,
    pub domestic_server_confirmed: bool,
}

impl Default for InternalConfig {
    fn default() -> Self {
        Self {
            capture_interval_secs: 12,
            poll_width: 1600,
            poll_quality: 85.0,
            workday_hours: 8,
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
            upload_url: String::new(),
            upload_survey_id: String::new(),
            upload_device_id: String::new(),
            encryption_recipient: String::new(),
            domestic_server_confirmed: false,
        }
    }
}

impl InternalConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(10..=15).contains(&self.capture_interval_secs) {
            return Err("截图间隔必须为 10–15 秒".into());
        }
        if !(1600..=7680).contains(&self.poll_width)
            || !self.poll_quality.is_finite()
            || !(85.0..=100.0).contains(&self.poll_quality)
        {
            return Err("定时截图宽度须为 1600–7680，质量须为 85–100".into());
        }
        if !(1..=24).contains(&self.workday_hours) {
            return Err("每天录制时长须为 1–24 小时".into());
        }
        if self
            .screenshot_blocked_apps
            .iter()
            .chain(self.screenshot_blocked_domains.iter())
            .any(|s| s.trim().is_empty() || s.len() > 256)
        {
            return Err("隐私规则不能为空或超过 256 字符".into());
        }
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
    pub progress: Mutex<TaskProgress>,
    pub capture_notice: Mutex<super::desktop::CaptureNotice>,
    root: PathBuf,
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
    let tmp = path.with_extension("json.tmp");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&tmp).map_err(|e| e.to_string())?;
    use std::io::Write;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::rename(tmp, path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
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
    state.0.writer.check_health()?;
    if !super::acquisition::screen_capture_granted() || !super::acquisition::ax_trusted() {
        return Err("请先授予屏幕录制与辅助功能权限，然后再开始采集".into());
    }
    state.0.screen_ok.store(true, Ordering::SeqCst);
    state.0.internal.start()?;
    let _ = app.emit(
        "tracking_state_changed",
        serde_json::json!({"isRunning": true}),
    );
    Ok(())
}

pub fn allow_webview_navigation(url: &reqwest::Url) -> bool {
    (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"))
        || (cfg!(debug_assertions)
            && url.scheme() == "http"
            && url.host_str() == Some("localhost")
            && url.port() == Some(1420))
}

pub fn pause_collection(
    app: &tauri::AppHandle,
    state: &crate::AppState,
    complete: bool,
) -> Result<(), String> {
    use tauri::Emitter;
    let _transition = state.0.internal.transition.lock();
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
    if let Err(e) = state.0.writer.flush() {
        let _ = state.0.internal.edit_progress(|p| {
            p.integrity_error = Some(e.clone());
            p.last_error = Some(e.clone());
            Ok(())
        });
        return Err(e);
    }
    state.0.internal.edit_progress(|p| {
        p.needs_flush = false;
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
            match state.0.internal.advance(elapsed) {
                Ok(true) => {
                    if let Err(e) = pause_collection(&app, &state, true)
                        .and_then(|_| super::frame_upload::on_task_finished(&app))
                    {
                        log::error!("自动回收失败：{e}");
                    }
                }
                Err(e) => {
                    log::error!("录制进度保存失败：{e}");
                    let _ = pause_collection(&app, &state, false);
                }
                _ => {}
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
    pub fn configuration_write(&self) -> Result<RwLockReadGuard<'_, ()>, String> {
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
    pub fn load(root: &Path, tracking: Arc<AtomicBool>) -> Result<Arc<Self>, String> {
        let cfg_path = root.join("internal-config.json");
        let config: InternalConfig = if cfg_path.exists() {
            serde_json::from_slice(&fs::read(&cfg_path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("内部版配置读取失败：{e}"))?
        } else {
            InternalConfig::default()
        };
        config.validate()?;
        let progress_path = root.join("internal-task.json");
        let mut progress: TaskProgress = if progress_path.exists() {
            serde_json::from_slice(&fs::read(&progress_path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("任务状态读取失败：{e}"))?
        } else {
            TaskProgress {
                task_id: uuid::Uuid::new_v4().to_string(),
                elapsed_ms: 0,
                target_ms: 2 * config.workday_hours as u64 * 3_600_000,
                status: TaskStatus::Ready,
                last_error: None,
                bundle_path: None,
                receipt: None,
                stored_bundle: None,
                integrity_error: None,
                needs_flush: false,
            }
        };
        uuid::Uuid::parse_str(&progress.task_id).map_err(|_| "任务编号无效")?;
        if progress.target_ms == 0
            || progress.target_ms > 48 * 3_600_000
            || progress.elapsed_ms > progress.target_ms
        {
            return Err("任务时长状态无效".into());
        }
        // Before any recorded time, configuration is the single durable source of the target.
        // Saving configuration must not require a transaction across two independent JSON files.
        if progress.elapsed_ms == 0 {
            progress.target_ms = 2 * config.workday_hours as u64 * 3_600_000;
        }
        // Relaunch never silently resumes collection or assumes an interrupted upload succeeded.
        if progress.status == TaskStatus::Recording || progress.needs_flush {
            if progress.status == TaskStatus::Recording {
                progress.status = TaskStatus::Paused;
            }
            progress.integrity_error =
                Some("上次采集未正常结束，排队数据可能未完整写入；需人工审核后处理".into());
            progress.last_error = progress.integrity_error.clone();
        }
        if progress.status == TaskStatus::Uploading {
            progress.status = TaskStatus::Completed;
        }
        if progress.elapsed_ms >= progress.target_ms
            && matches!(progress.status, TaskStatus::Ready | TaskStatus::Paused)
        {
            progress.status = TaskStatus::Completed;
        }
        tracking.store(false, Ordering::SeqCst);
        write_json_atomic(&progress_path, &progress)?;
        Ok(Arc::new(Self {
            config: RwLock::new(config),
            gate: RwLock::new(()),
            transition: Mutex::new(()),
            native_jobs: (Mutex::new(0), Condvar::new()),
            epoch: AtomicU64::new(0),
            tracking,
            frame_upload_active: AtomicBool::new(false),
            frame_upload_running: AtomicBool::new(false),
            progress: Mutex::new(progress),
            capture_notice: Mutex::new(super::desktop::CaptureNotice::default()),
            root: root.to_owned(),
        }))
    }

    pub fn save_config(&self, config: InternalConfig) -> Result<(), String> {
        config.validate()?;
        let _guard = self.gate.write();
        if self.frame_upload_running.load(Ordering::SeqCst) {
            return Err("请先停止后台上传，再修改设置".into());
        }
        let mut p = self.progress.lock();
        if matches!(p.status, TaskStatus::Recording | TaskStatus::Uploading) {
            return Err("请先暂停，上传期间不能修改配置".into());
        }
        if p.elapsed_ms > 0 && config.workday_hours != self.config.read().workday_hours {
            return Err("任务开始后不能修改目标时长".into());
        }
        if p.stored_bundle
            .as_ref()
            .is_some_and(|b| b.recipient != config.encryption_recipient.trim())
        {
            return Err("已有待回收数据包，修改加密公钥前需要人工审核回收状态".into());
        }
        write_json_atomic(&self.root.join("internal-config.json"), &config)?;
        if p.elapsed_ms == 0 {
            p.target_ms = 2 * config.workday_hours as u64 * 3_600_000;
        }
        *self.config.write() = config;
        Ok(())
    }

    pub fn start(&self) -> Result<(), String> {
        let _transition = self.transition.lock();
        let _guard = self.gate.write();
        let mut p = self.progress.lock();
        if let Some(ref e) = p.integrity_error {
            return Err(e.clone());
        }
        if p.needs_flush {
            return Err("停止落盘尚未完成，请先重试暂停保存".into());
        }
        if !matches!(p.status, TaskStatus::Ready | TaskStatus::Paused)
            || p.elapsed_ms >= p.target_ms
        {
            return Err("任务已结束，不能继续采集".into());
        }
        let mut next = p.clone();
        next.status = TaskStatus::Recording;
        next.last_error = None;
        next.needs_flush = true;
        write_json_atomic(&self.root.join("internal-task.json"), &next)?;
        *p = next;
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.capture_notice.lock() = super::desktop::CaptureNotice::default();
        self.tracking.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub fn pause(&self, complete: bool) -> Result<(), String> {
        self.tracking.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        *self.capture_notice.lock() = super::desktop::CaptureNotice::default();
        let _guard = self.gate.write();
        let mut p = self.progress.lock();
        if matches!(
            p.status,
            TaskStatus::Ready | TaskStatus::Recording | TaskStatus::Paused
        ) {
            p.status = if complete {
                TaskStatus::Completed
            } else {
                TaskStatus::Paused
            };
        }
        write_json_atomic(&self.root.join("internal-task.json"), &*p)?;
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

    /// Persist before publishing state to other threads; failed writes leave the prior state retryable.
    pub fn edit_progress(
        &self,
        edit: impl FnOnce(&mut TaskProgress) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut p = self.progress.lock();
        let mut next = p.clone();
        edit(&mut next)?;
        write_json_atomic(&self.root.join("internal-task.json"), &next)?;
        *p = next;
        Ok(())
    }

    /// Called with monotonic elapsed time. Long sleep/suspend gaps do not count as work.
    pub fn advance(&self, delta_ms: u64) -> Result<bool, String> {
        let mut p = self.progress.lock();
        if !self.tracking.load(Ordering::SeqCst) || delta_ms > 5_000 {
            return Ok(false);
        }
        p.elapsed_ms = p.elapsed_ms.saturating_add(delta_ms).min(p.target_ms);
        let ended = p.elapsed_ms >= p.target_ms;
        if ended {
            p.status = TaskStatus::Completed;
            self.tracking.store(false, Ordering::SeqCst);
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
        if let Err(e) = write_json_atomic(&self.root.join("internal-task.json"), &*p) {
            self.tracking.store(false, Ordering::SeqCst);
            self.epoch.fetch_add(1, Ordering::SeqCst);
            p.status = if ended {
                TaskStatus::Completed
            } else {
                TaskStatus::Paused
            };
            p.last_error = Some(format!("无法保存任务进度，已暂停：{e}"));
            return Err(e);
        }
        Ok(ended)
    }

    pub fn persist_progress(&self) -> Result<(), String> {
        write_json_atomic(
            &self.root.join("internal-task.json"),
            &*self.progress.lock(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn config_target_has_one_durable_source_before_recording() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        let mut config = InternalConfig::default();
        config.workday_hours = 6;
        // Configuration saving remains atomic even when the unrelated progress file is unwritable.
        fs::create_dir(d.path().join("internal-task.json.tmp")).unwrap();
        c.save_config(config.clone()).unwrap();
        assert_eq!(c.progress.lock().target_ms, 12 * 3_600_000);
        fs::remove_dir(d.path().join("internal-task.json.tmp")).unwrap();
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(restored.progress.lock().target_ms, 12 * 3_600_000);
        fs::create_dir(d.path().join("internal-config.json.tmp")).unwrap();
        config.workday_hours = 7;
        assert!(restored.save_config(config).is_err());
        assert_eq!(restored.config.read().workday_hours, 6);
        assert_eq!(restored.progress.lock().target_ms, 12 * 3_600_000);
    }
    #[test]
    fn clean_pause_can_resume_after_restart_but_unclean_task_stays_blocked() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        c.start().unwrap();
        c.pause(false).unwrap();
        // The application clears this only after aggregation and writer barriers acknowledge.
        c.edit_progress(|p| {
            p.needs_flush = false;
            Ok(())
        })
        .unwrap();
        let restored = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        restored.start().unwrap();
        let unclean = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        assert!(unclean.progress.lock().integrity_error.is_some());
        assert!(unclean.start().is_err());
    }
    #[test]
    fn start_and_recovery_state_save_failures_remain_retryable() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        fs::create_dir(d.path().join("internal-task.json.tmp")).unwrap();
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
        fs::remove_dir(d.path().join("internal-task.json.tmp")).unwrap();
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
        c.start().unwrap();
        c.progress.lock().target_ms = 1000;
        fs::create_dir(d.path().join("internal-task.json.tmp")).unwrap();
        assert!(c.advance(1000).is_err());
        assert_eq!(c.progress.lock().status, TaskStatus::Completed);
        assert!(!c.tracking.load(Ordering::SeqCst));
        fs::remove_dir(d.path().join("internal-task.json.tmp")).unwrap();
        assert!(c.start().is_err());
    }
    #[test]
    fn active_capture_does_not_block_nonterminal_clock_progress() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
        c.start().unwrap();
        let _active_capture = c.active(c.epoch.load(Ordering::SeqCst)).unwrap();
        assert!(!c.advance(1000).unwrap());
        assert_eq!(c.progress.lock().elapsed_ms, 1000);
    }
    #[test]
    fn duration_survives_restart_without_resuming() {
        let d = tempfile::tempdir().unwrap();
        let c = InternalControl::load(d.path(), Arc::new(AtomicBool::new(false))).unwrap();
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
