//! Fail-closed visible-window inspection before and after a multi-display grab.
//! The foreground window alone cannot describe pixels on a secondary monitor.

use super::screens::Region;
use crate::core::{acquisition::FrontWindowState, internal::InternalConfig};

struct VisibleWindow {
    front: FrontWindowState,
    content_window: bool,
    bounds: Region,
}

fn browser_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "chrome", "safari", "firefox", "edge", "brave", "opera", "vivaldi", "arc",
    ]
    .iter()
    .any(|browser| name.contains(browser))
}

fn check_visible_windows(
    windows: &[VisibleWindow],
    cfg: &InternalConfig,
    regions: &[Region],
) -> Result<(), String> {
    if windows.is_empty() {
        return Err("visible window list is empty".into());
    }
    for window in windows {
        if !regions.iter().any(|r| r.intersects(&window.bounds)) {
            continue;
        }
        if window.front.app_name.trim().is_empty() {
            return Err("visible window owner is unknown".into());
        }
        // Background URLs cannot be read reliably. Inspect titles and app identity;
        // foreground URL inspection remains in capture.rs, and pixels are not masked.
        let url = crate::core::privacy::extract_url_and_path(&window.front.window_title).0;
        if cfg.blocks_screenshot(
            &window.front.app_name,
            window.front.bundle_id.as_deref(),
            &window.front.window_title,
            url.as_deref(),
            false,
        ) {
            return Err("a sensitive window is visible on a captured display".into());
        }
        // A browser's absent title can hide a banking/login tab when screen
        // recording access is incomplete. Do not infer that it is safe.
        if window.content_window
            && browser_name(&window.front.app_name)
            && window.front.window_title.trim().is_empty()
        {
            return Err("a visible browser title cannot be inspected".into());
        }
    }
    Ok(())
}

pub(super) fn permit_capture(cfg: &InternalConfig, regions: &[Region]) -> Result<(), String> {
    if regions.is_empty() {
        return Err("未选择可采集屏幕".into());
    }
    check_visible_windows(&visible_windows()?, cfg, regions)
}

#[cfg(target_os = "macos")]
fn visible_windows() -> Result<Vec<VisibleWindow>, String> {
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};
    use core_graphics::window::{
        copy_window_info, kCGNullWindowID, kCGWindowBounds, kCGWindowLayer,
        kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowName,
        kCGWindowOwnerName, kCGWindowOwnerPID,
    };
    use std::ffi::c_void;

    fn value(dict: &CFDictionary, key: CFStringRef) -> Option<CFType> {
        let raw = *dict.find(key as *const c_void)?;
        if raw.is_null() {
            None
        } else {
            Some(unsafe { CFType::wrap_under_get_rule(raw as CFTypeRef) })
        }
    }
    fn text(dict: &CFDictionary, key: CFStringRef) -> Result<Option<String>, String> {
        value(dict, key)
            .map(|value| {
                value
                    .downcast::<CFString>()
                    .map(|s| s.to_string())
                    .ok_or_else(|| "window text has an unexpected type".to_owned())
            })
            .transpose()
    }

    let array = copy_window_info(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    )
    .ok_or("CGWindowListCopyWindowInfo failed")?;
    let mut windows = Vec::with_capacity(array.len() as usize);
    for raw in array.get_all_values() {
        if raw.is_null() {
            return Err("CGWindowList returned a null window".into());
        }
        let cf = unsafe { CFType::wrap_under_get_rule(raw as CFTypeRef) };
        let dict = cf
            .downcast::<CFDictionary>()
            .ok_or("CGWindowList returned a non-dictionary window")?;
        let owner = text(&dict, unsafe { kCGWindowOwnerName })?
            .filter(|s| !s.trim().is_empty())
            .ok_or("visible window owner is unavailable")?;
        let title = text(&dict, unsafe { kCGWindowName })?.unwrap_or_default();
        let layer = value(&dict, unsafe { kCGWindowLayer })
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i32())
            .ok_or("visible window layer is unavailable")?;
        let rect = value(&dict, unsafe { kCGWindowBounds })
            .and_then(|v| v.downcast::<CFDictionary>())
            .and_then(|d| core_graphics::geometry::CGRect::from_dict_representation(&d))
            .ok_or("可见窗口坐标不可用")?;
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            continue;
        }
        let pid = value(&dict, unsafe { kCGWindowOwnerPID })
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|v| v.to_i32())
            .ok_or("可见窗口进程不可用")?;
        let bundle_id =
            objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                .and_then(|a| a.bundleIdentifier())
                .map(|b| b.to_string());
        windows.push(VisibleWindow {
            front: FrontWindowState {
                app_name: owner,
                bundle_id,
                window_title: title,
                is_fullscreen: false,
            },
            content_window: layer == 0,
            bounds: Region::new(
                "window",
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            )?,
        });
    }
    Ok(windows)
}

#[cfg(target_os = "windows")]
fn visible_windows() -> Result<Vec<VisibleWindow>, String> {
    use std::path::Path;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible,
    };

    struct Scan {
        windows: Vec<VisibleWindow>,
        error: Option<String>,
    }
    unsafe fn inspect(hwnd: HWND) -> Result<Option<VisibleWindow>, String> {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return Ok(None);
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).map_err(|_| "visible window bounds unavailable")?;
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return Ok(None);
        }
        let mut pid = 0u32;
        if GetWindowThreadProcessId(hwnd, Some(&mut pid)) == 0 || pid == 0 {
            return Err("visible window process unavailable".into());
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|_| "visible window process cannot be inspected")?;
        let mut path = [0u16; 4096];
        let mut len = path.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut len,
        );
        let _ = windows::Win32::Foundation::CloseHandle(process);
        result.map_err(|_| "visible window process path unavailable")?;
        let executable = String::from_utf16_lossy(&path[..len as usize]);
        let app_name = Path::new(&executable)
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("visible window process name unavailable")?
            .to_owned();
        let mut title = [0u16; 1024];
        let title_len = GetWindowTextW(hwnd, &mut title);
        let window_title = String::from_utf16_lossy(&title[..title_len.max(0) as usize]);
        Ok(Some(VisibleWindow {
            front: FrontWindowState {
                app_name,
                bundle_id: Some(executable),
                window_title,
                is_fullscreen: false,
            },
            content_window: true,
            bounds: Region::new(
                "window",
                rect.left as f64,
                rect.top as f64,
                (rect.right - rect.left) as f64,
                (rect.bottom - rect.top) as f64,
            )?,
        }))
    }
    unsafe extern "system" fn enumerate(hwnd: HWND, param: LPARAM) -> BOOL {
        let scan = &mut *(param.0 as *mut Scan);
        match inspect(hwnd) {
            Ok(Some(window)) => scan.windows.push(window),
            Ok(None) => {}
            Err(error) => {
                scan.error = Some(error);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut scan = Scan {
        windows: Vec::new(),
        error: None,
    };
    let result = unsafe { EnumWindows(Some(enumerate), LPARAM((&mut scan as *mut Scan) as isize)) };
    if let Some(error) = scan.error {
        return Err(error);
    }
    result.map_err(|_| "EnumWindows failed".to_owned())?;
    Ok(scan.windows)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn visible_windows() -> Result<Vec<VisibleWindow>, String> {
    Err("visible window enumeration unsupported on this platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn window(app: &str, title: &str, x: f64) -> VisibleWindow {
        VisibleWindow {
            front: FrontWindowState {
                app_name: app.into(),
                bundle_id: None,
                window_title: title.into(),
                is_fullscreen: false,
            },
            content_window: true,
            bounds: Region::new("window", x, 0., 100., 100.).unwrap(),
        }
    }
    fn screen(x: f64) -> Region {
        Region::new("screen", x, 0., 1920., 1080.).unwrap()
    }
    #[test]
    fn background_sensitive_only_blocks_selected_screen() {
        let cfg = InternalConfig::default();
        let ws = [
            window("Editor", "fixture", 0.),
            window("WeChat", "private", 1920.),
        ];
        assert!(check_visible_windows(&ws, &cfg, &[screen(0.)]).is_ok());
        assert!(check_visible_windows(&ws, &cfg, &[screen(0.), screen(1920.)]).is_err());
        assert!(
            check_visible_windows(&[window("Keeper", "vault", 0.)], &cfg, &[screen(0.)]).is_err()
        );
    }
    #[test]
    fn unknown_browser_title_and_intersection_edges() {
        let cfg = InternalConfig::default();
        assert!(check_visible_windows(&[], &cfg, &[screen(0.)]).is_err());
        assert!(check_visible_windows(&[window("Safari", "", 0.)], &cfg, &[screen(0.)]).is_err());
        assert!(!screen(-1920.).intersects(&screen(0.)));
        assert!(screen(-1919.).intersects(&screen(0.)));
        assert!(Region::new("bad", f64::NAN, 0., 10., 10.).is_err());
        assert!(Region::new("bad", 0., 0., 0., 10.).is_err());
    }
}
