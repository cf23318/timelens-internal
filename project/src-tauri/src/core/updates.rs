//! Best-effort notification from the public internal release feed.
use semver::Version;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::{fs, future::Future, io, path::Path, time::Duration};
use tokio::sync::Mutex;

const FEED_URL: &str = "https://github.com/cf23318/timelens-internal/releases.atom";
const RELEASE_PREFIX: &str = "https://github.com/cf23318/timelens-internal/releases/tag/v";
const CACHE_FILE: &str = "update-check.json";
const ATTEMPT_FILE: &str = "update-last-attempt.json";
const CHECK_INTERVAL_MS: i64 = 3 * 60 * 60 * 1000;
const MAX_FEED_BYTES: usize = 512 * 1024;

#[derive(Default)]
pub struct UpdateChecker(Mutex<()>);

#[derive(Default, Deserialize, Serialize)]
struct Cache {
    last_attempt_ms: Option<i64>,
    latest: Option<String>,
}

impl UpdateChecker {
    pub async fn check(&self, root: &Path, current: &Version) -> Option<String> {
        self.check_with(root, current, chrono::Utc::now().timestamp_millis(), fetch_feed)
            .await
    }

    async fn check_with<F, Pending>(
        &self,
        root: &Path,
        current: &Version,
        now: i64,
        fetch: F,
    ) -> Option<String>
    where
        F: FnOnce() -> Pending,
        Pending: Future<Output = Result<Option<Version>, ()>>,
    {
        // Only update invocations share this mutex. A second WebView request
        // waits asynchronously for the same result and never starts another fetch.
        let _guard = self.0.lock().await;
        let cache_path = root.join(CACHE_FILE);
        let attempt_path = root.join(ATTEMPT_FILE);
        let old_cache = read_json::<Cache>(&cache_path);
        let last_attempt = match read_json::<i64>(&attempt_path) {
            Ok(Some(last)) if last >= 0 => Some(last),
            Ok(None) => {
                // Older builds stored the attempt in the version cache. If it
                // is unreadable, reserve a full interval before trying again.
                let last = match &old_cache {
                    Ok(None) => None,
                    Ok(Some(cache)) => Some(cache.last_attempt_ms.filter(|last| *last >= 0).unwrap_or(now)),
                    Err(_) => Some(now),
                };
                if let Some(last) = last {
                    let _ = super::internal::write_json_atomic(&attempt_path, &last);
                }
                last
            }
            _ => {
                let _ = super::internal::write_json_atomic(&attempt_path, &now);
                Some(now)
            }
        };
        let mut cache = old_cache.ok().flatten().unwrap_or_default();
        let due = last_attempt.is_none_or(|last| now.saturating_sub(last) >= CHECK_INTERVAL_MS);
        // Reserve the interval before the request, including on failure or exit.
        if due && super::internal::write_json_atomic(&attempt_path, &now).is_ok() {
            if let Ok(latest) = fetch().await {
                cache.last_attempt_ms = Some(now);
                cache.latest = latest.map(|version| version.to_string());
                let _ = super::internal::write_json_atomic(&cache_path, &cache);
            }
        }
        cache.latest.filter(|s| stable_version(s).is_some_and(|latest| latest > *current))
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(Into::into),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn stable_version(text: &str) -> Option<Version> {
    let version = Version::parse(text).ok()?;
    (version.pre.is_empty() && version.build.is_empty()).then_some(version)
}

pub fn release_url(version: &str) -> Option<String> {
    let version = stable_version(version)?;
    Some(format!("{RELEASE_PREFIX}{version}"))
}

#[derive(Deserialize)]
struct Feed {
    #[serde(rename = "entry", default)]
    entries: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "link", default)]
    links: Vec<ReleaseLink>,
}

#[derive(Deserialize)]
struct ReleaseLink {
    #[serde(rename = "@href")]
    href: String,
}

fn latest_release(bytes: &[u8]) -> Result<Option<Version>, quick_xml::DeError> {
    let feed: Feed = quick_xml::de::from_reader(bytes)?;
    Ok(feed
        .entries
        .into_iter()
        .flat_map(|entry| entry.links)
        .filter_map(|link| stable_version(link.href.strip_prefix(RELEASE_PREFIX)?))
        .max())
}

async fn fetch_feed() -> Result<Option<Version>, ()> {
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .user_agent("TimeLens-Internal")
        .build()
        .map_err(|_| ())?;
    let mut response = client.get(FEED_URL).send().await.map_err(|_| ())?;
    if !response.status().is_success()
        || response.content_length().is_some_and(|len| len > MAX_FEED_BYTES as u64)
    {
        return Err(());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len() + chunk.len() > MAX_FEED_BYTES {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    latest_release(&bytes).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn identifies_highest_stable_version_from_the_internal_feed() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
            <feed xmlns="http://www.w3.org/2005/Atom">
              <link href="https://github.com/cf23318/timelens-internal/releases/tag/v20.0.0"/>
              <entry><link rel="alternate" href="https://github.com/cf23318/timelens-internal/releases/tag/v0.17.9"/></entry>
              <entry><link rel="alternate" href="https://github.com/cf23318/timelens-internal/releases/tag/v0.17.12"/></entry>
              <entry><link rel="alternate" href="https://github.com/cf23318/timelens-internal/releases/tag/v0.18.0-beta.1"/></entry>
              <entry><link rel="alternate" href="https://github.com/other/timelens-internal/releases/tag/v1.0.0"/></entry>
              <entry><link rel="alternate" href="https://github.com.example/cf23318/timelens-internal/releases/tag/v2.0.0"/></entry>
            </feed>"#;
        assert_eq!(latest_release(xml).unwrap(), Some(Version::new(0, 17, 12)));
        assert!(release_url("0.17.12?x=y").is_none());
        assert!(release_url("0.17.12/../../example").is_none());
        assert!(latest_release(b"<feed><entry>").is_err());
    }

    #[tokio::test]
    async fn throttles_across_launches_and_caches_a_verified_version() {
        let dir = tempfile::tempdir().unwrap();
        let current = Version::new(0, 17, 12);
        let latest = Version::new(0, 17, 13);
        let fetches = AtomicUsize::new(0);
        let fetch = || async {
            fetches.fetch_add(1, Ordering::Relaxed);
            Ok(Some(latest.clone()))
        };
        let first = UpdateChecker::default().check_with(dir.path(), &current, 1000, fetch).await;
        assert_eq!(first.as_deref(), Some("0.17.13"));
        let reopened = UpdateChecker::default();
        let cached = reopened.check_with(dir.path(), &current, 1000 + CHECK_INTERVAL_MS - 1, fetch).await;
        assert_eq!(cached, first);
        assert_eq!(fetches.load(Ordering::Relaxed), 1);
        assert_eq!(reopened.check_with(dir.path(), &latest, 1001, fetch).await, None);
        assert_eq!(reopened.check_with(dir.path(), &current, 1000 + CHECK_INTERVAL_MS, fetch).await, first);
        assert_eq!(fetches.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn a_failed_or_concurrent_check_cannot_bypass_the_interval() {
        let dir = tempfile::tempdir().unwrap();
        let checker = UpdateChecker::default();
        let current = Version::new(0, 17, 12);
        let fetches = AtomicUsize::new(0);
        let fetch = || async {
            fetches.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
            Err(())
        };
        let (first, duplicate) = tokio::join!(
            checker.check_with(dir.path(), &current, 1000, fetch),
            checker.check_with(dir.path(), &current, 1000, fetch),
        );
        assert_eq!((first, duplicate), (None, None));
        assert_eq!(UpdateChecker::default().check_with(dir.path(), &current, 999, fetch).await, None);
        assert_eq!(fetches.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn damaged_or_deleted_version_cache_keeps_attempt_cooldown() {
        for damage in ["corrupt", "deleted"] {
            let dir = tempfile::tempdir().unwrap();
            let current = Version::new(0, 17, 12);
            let calls = AtomicUsize::new(0);
            let fetch = || async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(Some(Version::new(0, 17, 13)))
            };
            let path = dir.path().join(CACHE_FILE);
            UpdateChecker::default().check_with(dir.path(), &current, 1000, fetch).await;
            match damage {
                "corrupt" => fs::write(&path, "{").unwrap(),
                _ => fs::remove_file(&path).unwrap(),
            }
            let reopened = UpdateChecker::default();
            assert_eq!(reopened.check_with(dir.path(), &current, 1000 + CHECK_INTERVAL_MS - 1, fetch).await, None);
            assert_eq!(calls.load(Ordering::Relaxed), 1, "{damage}");
            assert_eq!(reopened.check_with(dir.path(), &current, 1000 + CHECK_INTERVAL_MS, fetch).await.as_deref(), Some("0.17.13"));
            assert_eq!(calls.load(Ordering::Relaxed), 2, "{damage}");
        }
    }

    #[tokio::test]
    async fn migrates_older_attempt_even_if_its_cache_is_lost_afterward() {
        let dir = tempfile::tempdir().unwrap();
        let current = Version::new(0, 17, 12);
        let cache_path = dir.path().join(CACHE_FILE);
        fs::write(&cache_path, br#"{"last_attempt_ms":1000,"latest":"0.17.13"}"#).unwrap();
        let checker = UpdateChecker::default();
        let no_fetch = || async { panic!("checked before the migrated interval expired") };
        assert_eq!(checker.check_with(dir.path(), &current, 1010, no_fetch).await.as_deref(), Some("0.17.13"));
        assert_eq!(read_json::<i64>(&dir.path().join(ATTEMPT_FILE)).unwrap(), Some(1000));
        fs::remove_file(cache_path).unwrap();
        assert_eq!(checker.check_with(dir.path(), &current, 1020, no_fetch).await, None);
    }

    #[tokio::test]
    async fn unreadable_attempt_metadata_starts_a_new_cooldown_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let current = Version::new(0, 17, 12);
        let path = dir.path().join(ATTEMPT_FILE);
        fs::write(&path, "{").unwrap();
        let checker = UpdateChecker::default();
        let no_fetch = || async { panic!("invalid metadata must not trigger a request") };
        assert_eq!(checker.check_with(dir.path(), &current, 1000, no_fetch).await, None);
        assert_eq!(read_json::<i64>(&path).unwrap(), Some(1000));
        assert_eq!(checker.check_with(dir.path(), &current, 1000 + CHECK_INTERVAL_MS - 1, no_fetch).await, None);
    }
}
