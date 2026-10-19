//! Only the foreground application/window participates in screenshot filtering.
//! Background pixels may appear in a whole-display image; they are not masked.

use crate::core::{acquisition, browser, internal::InternalConfig};
use acquisition::FrontWindowState;

fn check_front_window(
    cfg: &InternalConfig,
    expected: &FrontWindowState,
    expected_url: Option<&str>,
    actual: &FrontWindowState,
    actual_url: Option<&str>,
) -> Result<(), String> {
    if actual.app_name.trim().is_empty()
        || actual.app_name != expected.app_name
        || actual.bundle_id != expected.bundle_id
        || actual.window_title != expected.window_title
        || actual_url != expected_url
    {
        return Err("前台窗口或网址已变化，丢弃本次截图".into());
    }
    if cfg.blocks_screenshot(
        &actual.app_name,
        actual.bundle_id.as_deref(),
        &actual.window_title,
        actual_url,
        browser::is_browser(actual),
    ) {
        return Err("前台命中隐私规则，跳过截图".into());
    }
    Ok(())
}

/// Re-read focus at every existing capture gate, without enumerating background windows.
pub(super) fn permit_capture(
    cfg: &InternalConfig,
    expected: &FrontWindowState,
    expected_url: Option<&str>,
) -> Result<(), String> {
    let actual = acquisition::sample_front_window()?;
    let actual_url = browser::url_for_front(&actual);
    check_front_window(cfg, expected, expected_url, &actual, actual_url.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn front(app: &str, title: &str) -> FrontWindowState {
        FrontWindowState {
            app_name: app.into(),
            bundle_id: Some(format!("test.{app}")),
            window_title: title.into(),
            is_fullscreen: false,
        }
    }

    #[test]
    fn normal_foreground_is_allowed_and_private_foreground_still_blocks() {
        let cfg = InternalConfig::default();
        let editor = front("Editor", "work document");
        assert!(check_front_window(&cfg, &editor, None, &editor, None).is_ok());
        for app in [
            "WeChat",
            "微信",
            "DingTalk",
            "钉钉",
            "1Password",
            "Bitwarden",
        ] {
            let private = front(app, "document");
            assert!(check_front_window(&cfg, &private, None, &private, None).is_err());
        }
        let password = front("Editor", "登录密码");
        assert!(check_front_window(&cfg, &password, None, &password, None).is_err());
    }

    #[test]
    fn foreground_browser_keeps_domain_and_unreadable_address_protection() {
        let cfg = InternalConfig::default();
        let chrome = front("Google Chrome", "document");
        for url in [
            Some("https://boc.cn/"),
            Some("https://online.boc.cn/"),
            None,
        ] {
            assert!(check_front_window(&cfg, &chrome, url, &chrome, url).is_err());
        }
        let url = Some("https://example.com/work");
        assert!(check_front_window(&cfg, &chrome, url, &chrome, url).is_ok());
    }

    #[test]
    fn changing_focus_or_url_during_capture_discards_the_image() {
        let cfg = InternalConfig::default();
        let expected = front("Editor", "work document");
        let mut changes = vec![front("WeChat", "document")];
        let mut changed = expected.clone();
        changed.bundle_id = Some("another.editor".into());
        changes.push(changed);
        let mut changed = expected.clone();
        changed.window_title = "another document".into();
        changes.push(changed);
        for actual in changes {
            assert!(check_front_window(&cfg, &expected, None, &actual, None).is_err());
        }
        let chrome = front("Google Chrome", "document");
        assert!(check_front_window(
            &cfg,
            &chrome,
            Some("https://example.com/a"),
            &chrome,
            Some("https://example.com/b")
        )
        .is_err());
        let unknown = front("", "");
        assert!(check_front_window(&cfg, &unknown, None, &unknown, None).is_err());
    }

    #[test]
    fn custom_foreground_rules_are_still_honored() {
        let mut cfg = InternalConfig::default();
        cfg.screenshot_blocked_apps.push("Editor".into());
        let editor = front("Editor", "work document");
        assert!(check_front_window(&cfg, &editor, None, &editor, None).is_err());
    }
}
