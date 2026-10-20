use regex::Regex;
use std::sync::LazyLock;

static BLACKLIST_BUNDLE: &[&str] = &[
    "com.apple.keychainaccess",
    "com.1password",
];

static BLACKLIST_TITLE_SUBSTR: &[&str] = &[
    "密码", "password", "登录", "login", "网银", "网上银行", "online banking",
    "验证码", "支付验证", "verification code", "one-time password",
];

static URL_IN_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(https?://[^\s]+)").expect("regex")
});

// Chrome can put a URL without its scheme in the title. Process every match,
// including query/fragment-only addresses, while leaving ordinary filenames alone.
static TITLE_URLS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:https?://[^\s<>"']+|(?:[^\s/@<>"']+@)?(?:localhost|(?:[\p{L}\p{N}-]+\.)+[\p{L}\p{N}-]+)(?::[0-9]+)?(?:[/?#][^\s<>"']*)?)"#)
        .expect("title URL regex")
});

/// Shared by screenshot blocking and metadata redaction so their rules cannot drift.
pub fn sensitive_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    BLACKLIST_TITLE_SUBSTR.iter().any(|s| lower.contains(s))
}

fn redact_title_url(raw: &str) -> String {
    let explicit_scheme = raw.contains("://");
    let candidate = if explicit_scheme { raw.to_owned() } else { format!("https://{raw}") };
    let Ok(mut url) = reqwest::Url::parse(&candidate) else {
        return "[redacted-url]".into();
    };
    let has_suffix = url.query().is_some() || url.fragment().is_some();
    if !has_suffix && url.username().is_empty() && url.password().is_none() {
        return raw.to_owned();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    let clean = url.as_str();
    let clean = if explicit_scheme { clean } else { clean.strip_prefix("https://").unwrap_or(clean) };
    format!("{clean}{}", if has_suffix { "…" } else { "" })
}

pub fn should_redact_bundle(bundle_id: &str) -> bool {
    if BLACKLIST_BUNDLE
        .iter()
        .any(|b| bundle_id.eq_ignore_ascii_case(b))
    {
        return true;
    }
    let lower = bundle_id.to_lowercase();
    lower.contains("1password")
        || lower.contains("keychain")
        || lower.contains("credentialui")
        || lower.contains("credwiz")
}

pub fn redact_title(title: &str) -> String {
    if sensitive_title(title) {
        return "[redacted]".to_string();
    }
    TITLE_URLS.replace_all(title, |caps: &regex::Captures<'_>| redact_title_url(&caps[0])).into_owned()
}

fn trim_url_params(url: &str) -> String {
    if let Some(i) = url.find('?') {
        format!("{}…", &url[..i])
    } else {
        url.to_string()
    }
}

/// Best-effort URL / path hints from window title (no extra OS calls).
pub fn extract_url_and_path(title: &str) -> (Option<String>, Option<String>) {
    let url = URL_IN_TITLE
        .captures(title)
        .and_then(|c| c.get(1).map(|m| trim_url_params(m.as_str())));
    let path_hint = if (title.contains('/') || title.contains('\\'))
        && (title.contains(".rs") || title.contains(".ts"))
    {
        static FILEISH: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"([\w./\\~-]+\.(?:rs|ts|tsx|js|json|md|py))\b").expect("regex")
        });
        FILEISH
            .captures(title)
            .and_then(|c| c.get(1).map(|m| redact_path(m.as_str())))
    } else {
        None
    };
    (url, path_hint)
}

pub fn redact_path(path: &str) -> String {
    if path.is_empty() {
        return path.to_string();
    }
    if let Some(home) = dirs::home_dir() {
        let hs = home.to_string_lossy();
        if path.starts_with(hs.as_ref()) {
            return path.replacen(hs.as_ref(), "~", 1);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_title_password_substring() {
        assert_eq!(redact_title("my login page"), "[redacted]");
        assert_eq!(redact_title("normal title"), "normal title");
    }

    #[test]
    fn screenshot_sensitive_titles_are_always_redacted_case_insensitively() {
        for title in ["password-test.txt - 记事本", "PASSWORD vault", "LoGiN page", "登录", "网银", "Verification Code", "支付验证"] {
            assert!(sensitive_title(title));
            assert_eq!(redact_title(title), "[redacted]");
        }
    }

    #[test]
    fn all_title_urls_remove_credentials_query_and_fragment_with_or_without_scheme() {
        let title = "https://alice:secret@example.com/a?q=private#token and example.org/b?key=hidden - Google Chrome";
        let redacted = redact_title(title);
        assert!(!redacted.contains("secret") && !redacted.contains("private") && !redacted.contains("hidden"));
        assert!(redacted.contains("https://example.com/a…"));
        assert!(redacted.contains("example.org/b…"));
        for title in ["example.com?key=secret", "example.com/#secret", "localhost:43829/work?q=secret", "127.0.0.1:43829/work?q=secret", "alice:secret@example.com/work?q=hidden"] {
            assert!(!redact_title(title).contains("secret"), "{title}");
        }
        for title in ["访问https://example.com/a?key=secret", "例子.中国/a?key=secret"] {
            assert!(!redact_title(title).contains("secret"), "{title}");
        }
        for title in ["work.txt - 记事本", "Normal report ? draft #2", "example.com/work - Google Chrome"] {
            assert_eq!(redact_title(title), title);
        }
    }

    #[test]
    fn redact_title_trims_url_query_in_place() {
        let t = redact_title("tab — https://example.com/path?q=secret&x=1");
        assert!(
            !t.contains("q=secret"),
            "query should be stripped or shortened: {t}"
        );
    }

    #[test]
    fn should_redact_keychain_bundle() {
        assert!(should_redact_bundle("com.apple.keychainaccess"));
        assert!(!should_redact_bundle("com.timelens.desktop"));
    }

    #[test]
    fn should_redact_windows_exe_path_hints() {
        assert!(should_redact_bundle(
            r"C:\Program Files\1Password\1Password.exe"
        ));
    }
}
