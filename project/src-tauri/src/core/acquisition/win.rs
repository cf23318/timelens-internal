//! Windows 内部版：前台窗口和 GetLastInputInfo 空闲检测；不安装键鼠钩子。

use std::path::Path;

use screenshots::Screen;
use windows::core::PWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::WiFi::{
    wlan_interface_state_connected, wlan_intf_opcode_current_connection, WlanCloseHandle,
    WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface, DOT11_SSID,
    WLAN_API_VERSION, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO, WLAN_INTERFACE_INFO_LIST,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::WinRT::RoInitialize;
use windows::Win32::System::WinRT::RO_INIT_MULTITHREADED;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, GetWindowTextW, GetWindowThreadProcessId, SM_CMONITORS,
};
use windows::UI::Notifications::Management::{
    UserNotificationListener, UserNotificationListenerAccessStatus,
};

#[derive(Debug, Clone)]
pub struct FrontWindowState {
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub window_title: String,
    pub is_fullscreen: bool,
}

pub fn ax_trusted() -> bool {
    true
}

pub fn idle_seconds() -> f64 {
    unsafe {
        let mut lii = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if !GetLastInputInfo(&mut lii).as_bool() {
            return 0.0;
        }
        let tick = windows::Win32::System::SystemInformation::GetTickCount();
        let idle_ms = tick.wrapping_sub(lii.dwTime);
        idle_ms as f64 / 1000.0
    }
}

pub fn screen_capture_granted() -> bool {
    screen_capture_probe()
}

pub fn screen_capture_refresh_access() -> bool {
    screen_capture_probe()
}

pub fn screen_capture_poll_check() -> bool {
    screen_capture_probe()
}

pub fn request_screen_capture_access() -> bool {
    screen_capture_probe()
}

pub fn active_display_count() -> i32 {
    unsafe { GetSystemMetrics(SM_CMONITORS).max(1) }
}

/// Internal Windows builds report no fabricated key/click counts.
pub fn hardware_input_delta() -> (u32, u32) {
    (0, 0)
}

pub fn notifications_listener_access_granted() -> bool {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
    match UserNotificationListener::Current() {
        Ok(l) => l
            .GetAccessStatus()
            .map(|s| s == UserNotificationListenerAccessStatus::Allowed)
            .unwrap_or(false),
        Err(_) => false,
    }
}

pub fn sample_ambient_extras() -> super::AmbientExtras {
    super::AmbientExtras {
        wifi_ssid: wifi_ssid_connected(),
        battery_percent: battery_percent_win(),
        is_charging: ac_power_online_win(),
    }
}

fn battery_percent_win() -> Option<f64> {
    let mut st = SYSTEM_POWER_STATUS::default();
    unsafe {
        GetSystemPowerStatus(&mut st).ok()?;
    }
    if st.BatteryLifePercent >= 1 && st.BatteryLifePercent <= 100 {
        Some(f64::from(st.BatteryLifePercent))
    } else {
        None
    }
}

fn ac_power_online_win() -> Option<i64> {
    let mut st = SYSTEM_POWER_STATUS::default();
    unsafe {
        GetSystemPowerStatus(&mut st).ok()?;
    }
    match st.ACLineStatus {
        0 => Some(0),
        1 => Some(1),
        _ => None,
    }
}

fn dot11_ssid_to_string(s: &DOT11_SSID) -> Option<String> {
    let n = s.uSSIDLength as usize;
    if n == 0 || n > s.ucSSID.len() {
        return None;
    }
    std::str::from_utf8(&s.ucSSID[..n])
        .ok()
        .map(|x| x.to_string())
}

fn wifi_ssid_connected() -> Option<String> {
    unsafe {
        let mut negotiated = 0u32;
        let mut client = windows::Win32::Foundation::HANDLE::default();
        if WlanOpenHandle(WLAN_API_VERSION, None, &mut negotiated, &mut client) != ERROR_SUCCESS.0 {
            return None;
        }
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        let r = WlanEnumInterfaces(client, None, &mut list);
        if r != ERROR_SUCCESS.0 || list.is_null() {
            let _ = WlanCloseHandle(client, None);
            return None;
        }
        let mut out = None;
        let n = (*list).dwNumberOfItems as usize;
        let base: *const WLAN_INTERFACE_INFO = std::ptr::addr_of!((*list).InterfaceInfo[0]);
        for i in 0..n {
            let info = &*base.add(i);
            if info.isState != wlan_interface_state_connected {
                continue;
            }
            let mut sz = 0u32;
            let mut pdata: *mut core::ffi::c_void = std::ptr::null_mut();
            let qr = WlanQueryInterface(
                client,
                &info.InterfaceGuid,
                wlan_intf_opcode_current_connection,
                None,
                &mut sz,
                &mut pdata,
                None,
            );
            if qr != ERROR_SUCCESS.0
                || pdata.is_null()
                || sz < std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() as u32
            {
                continue;
            }
            let attrs = &*(pdata as *const WLAN_CONNECTION_ATTRIBUTES);
            out = dot11_ssid_to_string(&attrs.wlanAssociationAttributes.dot11Ssid);
            WlanFreeMemory(pdata);
            break;
        }
        WlanFreeMemory(list as *mut _);
        let _ = WlanCloseHandle(client, None);
        out
    }
}

fn screen_capture_probe() -> bool {
    Screen::all().map(|s| !s.is_empty()).unwrap_or(false)
}

unsafe fn exe_path_for_pid(pid: u32) -> Option<String> {
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buffer = [0u16; 4096];
    let mut size = buffer.len() as u32;
    let r = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        PWSTR(buffer.as_mut_ptr()),
        &mut size,
    );
    let _ = CloseHandle(handle);
    r.ok()?;
    Some(String::from_utf16_lossy(&buffer[..size as usize]))
}

pub fn sample_front_window() -> Result<FrontWindowState, String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return Err("no foreground window".into());
        }
        let mut title_buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut title_buf);
        let window_title = if len > 0 {
            String::from_utf16_lossy(&title_buf[..len as usize])
        } else {
            String::new()
        };
        let mut pid = 0u32;
        let _tid = GetWindowThreadProcessId(hwnd, Some(std::ptr::addr_of_mut!(pid)));
        let (app_name, bundle_id) = if pid == 0 {
            ("Unknown".to_string(), None)
        } else if let Some(path) = exe_path_for_pid(pid) {
            let stem = Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string();
            (stem, Some(path))
        } else {
            (format!("PID {pid}"), None)
        };
        Ok(FrontWindowState {
            app_name,
            bundle_id,
            window_title,
            is_fullscreen: false,
        })
    }
}

pub fn foreground_window_center() -> Option<(i32, i32)> {
    use windows::Win32::{Foundation::RECT, UI::WindowsAndMessaging::GetWindowRect};
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        Some((
            rect.left + (rect.right - rect.left) / 2,
            rect.top + (rect.bottom - rect.top) / 2,
        ))
    }
}
