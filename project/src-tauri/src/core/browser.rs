//! Browser address acquisition. Only the address bar is read; form input values are never queried.
use super::acquisition::FrontWindowState;

pub fn is_browser(front: &FrontWindowState) -> bool {
    let name = format!(
        "{} {}",
        front.app_name,
        front.bundle_id.as_deref().unwrap_or("")
    )
    .to_lowercase();
    [
        "chrome",
        "msedge",
        "microsoft edge",
        "safari",
        "firefox",
        "brave",
        "arc.app",
        "chromium",
    ]
    .iter()
    .any(|s| name.contains(s))
}

pub fn sanitized_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 8192 {
        return None;
    }
    let explicit_scheme = raw.contains("://");
    let raw = if explicit_scheme {
        raw.to_owned()
    } else {
        format!("https://{}", raw.trim_start_matches("//"))
    };
    let mut url = reqwest::Url::parse(&raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    // Address bars may hide the scheme. Store that uncertainty rather than
    // claiming an HTTP page was HTTPS; domain privacy rules support both forms.
    Some(if explicit_scheme {
        url.to_string()
    } else {
        format!("//{}", url.as_str().strip_prefix("https://")?)
    })
}

pub fn url_host(raw: &str) -> Option<String> {
    let url = if raw.starts_with("//") { format!("https:{raw}") } else { raw.to_owned() };
    let url = reqwest::Url::parse(&url).ok()?;
    if !matches!(url.scheme(), "http" | "https") { return None; }
    url.host_str().map(str::to_owned)
}

pub fn url_for_front(front: &FrontWindowState) -> Option<String> {
    if is_browser(front) {
        current_url(front)
    } else {
        super::privacy::extract_url_and_path(&front.window_title)
            .0
            .and_then(|s| sanitized_url(&s))
    }
}

/// A single worker bounds provider requests and prevents accumulating stuck UI Automation threads.
pub fn current_url(front: &FrontWindowState) -> Option<String> {
    use std::sync::{mpsc, OnceLock};
    type Request = (FrontWindowState, mpsc::SyncSender<Option<String>>);
    static REQUESTS: OnceLock<mpsc::SyncSender<Request>> = OnceLock::new();
    if !is_browser(front) {
        return None;
    }
    let tx = REQUESTS.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Request>(1);
        std::thread::spawn(move || {
            while let Ok((front, reply)) = rx.recv() {
                let result = read_native(&front).and_then(|s| sanitized_url(&s));
                let _ = reply.try_send(result);
            }
        });
        tx
    });
    let (reply, rx) = mpsc::sync_channel(1);
    tx.try_send((front.clone(), reply)).ok()?;
    rx.recv_timeout(std::time::Duration::from_millis(900))
        .ok()
        .flatten()
}

#[cfg(target_os = "macos")]
fn read_native(front: &FrontWindowState) -> Option<String> {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let bundle = front.bundle_id.as_deref()?;
    let script = match bundle {
        "com.google.Chrome" => {
            "tell application id \"com.google.Chrome\" to get URL of active tab of front window"
        }
        "com.microsoft.edgemac" => {
            "tell application id \"com.microsoft.edgemac\" to get URL of active tab of front window"
        }
        "com.apple.Safari" => {
            "tell application id \"com.apple.Safari\" to get URL of front document"
        }
        _ => return None,
    };
    let mut child = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let began = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut text = String::new();
                child
                    .stdout
                    .take()?
                    .take(8193)
                    .read_to_string(&mut text)
                    .ok()?;
                return Some(text);
            }
            Ok(None) if began.elapsed() < Duration::from_millis(700) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn read_native(front: &FrontWindowState) -> Option<String> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationValuePattern, UIA_DocumentControlTypeId,
        UIA_EditControlTypeId, UIA_ToolBarControlTypeId, UIA_ValuePatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId, IsChild,
    };
    let executable = std::path::Path::new(front.bundle_id.as_deref()?)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    if !matches!(executable.as_str(), "chrome.exe" | "msedge.exe") {
        return None;
    }
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
        struct ComScope;
        impl Drop for ComScope {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _scope = ComScope;
        let automation: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let hwnd = GetForegroundWindow();
        let current = super::acquisition::sample_front_window().ok()?;
        if current.bundle_id != front.bundle_id || current.window_title != front.window_title {
            return None;
        }
        let mut class = [0u16; 128];
        let len = GetClassNameW(hwnd, &mut class);
        if len <= 0 || String::from_utf16_lossy(&class[..len as usize]) != "Chrome_WidgetWin_1" {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let root = automation.ElementFromHandle(hwnd).ok()?;
        if root.CurrentNativeWindowHandle().ok()? != hwnd
            || root.CurrentProcessId().ok()? != pid as i32
        {
            return None;
        }
        let walker = automation.ControlViewWalker().ok()?;
        let mut pending = vec![(root, 0u32, false)];
        let mut visited = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(600);
        while let Some((element, depth, in_toolbar)) = pending.pop() {
            visited += 1;
            if visited > 256
                || std::time::Instant::now() > deadline
                || GetForegroundWindow() != hwnd
            {
                return None;
            }
            let Ok(kind) = element.CurrentControlType() else {
                continue;
            };
            // Page content is never traversed, even if it imitates address-bar names/roles.
            if kind == UIA_DocumentControlTypeId
                || element.CurrentProcessId().ok() != Some(pid as i32)
            {
                continue;
            }
            let in_toolbar = in_toolbar || kind == UIA_ToolBarControlTypeId;
            if kind == UIA_EditControlTypeId && in_toolbar {
                let name = element.CurrentName().ok()?.to_string();
                let known = [
                    "Address and search bar",
                    "地址和搜索栏",
                    "地址与搜索栏",
                    "Search or enter web address",
                    "搜索或输入 Web 地址",
                ]
                .contains(&name.as_str());
                if known
                    && !element.CurrentIsPassword().ok()?.as_bool()
                    && !element.CurrentHasKeyboardFocus().ok()?.as_bool()
                {
                    let element_hwnd = element.CurrentNativeWindowHandle().ok()?;
                    if !element_hwnd.is_invalid()
                        && element_hwnd != hwnd
                        && !IsChild(hwnd, element_hwnd).as_bool()
                    {
                        continue;
                    }
                    let pattern: IUIAutomationValuePattern = element
                        .GetCurrentPattern(UIA_ValuePatternId)
                        .ok()?
                        .cast()
                        .ok()?;
                    let text = pattern.CurrentValue().ok()?.to_string();
                    if GetForegroundWindow() == hwnd
                        && !element.CurrentHasKeyboardFocus().ok()?.as_bool()
                        && !text.is_empty()
                    {
                        return Some(text);
                    }
                }
            }
            if depth >= 12 {
                continue;
            }
            if let Ok(mut child) = walker.GetFirstChildElement(&element) {
                loop {
                    if pending.len() + visited >= 256 {
                        return None;
                    }
                    pending.push((child.clone(), depth + 1, in_toolbar));
                    match walker.GetNextSiblingElement(&child) {
                        Ok(next) => child = next,
                        Err(_) => break,
                    }
                }
            }
        }
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_native(_front: &FrontWindowState) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_scheme_is_preserved_as_unknown_and_still_supports_privacy_host_matching() {
        let url = sanitized_url("127.0.0.1:43829/work?q=private#token").unwrap();
        assert_eq!(url, "//127.0.0.1:43829/work");
        assert_eq!(sanitized_url(&url).as_deref(), Some(url.as_str()));
        assert_eq!(url_host(&url).as_deref(), Some("127.0.0.1"));
        assert_eq!(sanitized_url("http://example.com/work").as_deref(), Some("http://example.com/work"));
        assert!(url_host("file:///private/report").is_none());
    }

    #[test]
    fn url_only_keeps_domain_and_path() {
        assert_eq!(
            sanitized_url("https://user:secret@example.com/work/a?q=private#token"),
            Some("https://example.com/work/a".into())
        );
        assert_eq!(
            sanitized_url("example.com/work"),
            Some("//example.com/work".into())
        );
        assert!(sanitized_url("file:///private/report").is_none());
        assert!(sanitized_url("chrome://settings").is_none());
    }
}
