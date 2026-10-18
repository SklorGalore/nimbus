//! Check for Updates: ask GitHub for the latest release and compare it
//! with the running version.
//!
//! The check is on demand (a palette action) and, only when the config
//! says `check_updates = true`, once at startup after the first frame. A
//! personal tool should not phone home unless asked, so the default is
//! off. It never downloads or installs anything: when a newer version
//! exists, "Open Latest Release Page" opens the release in the browser
//! and the person installs it the way they did the first time.
//!
//! The request goes over HTTPS with `ureq` and the system's certificate
//! store (`rustls-platform-verifier`: Keychain on macOS, the Windows
//! store, `/etc/ssl` on Linux), so a corporate or test certificate the
//! system trusts works without a bundled root list. The one JSON field
//! wanted, `tag_name`, is read with a small scan rather than a JSON
//! crate: the shape is fixed and a wrong guess only means "cannot check".

use std::fmt;
use std::time::Duration;

/// The project's releases page, opened when no check has run yet.
pub const RELEASES_URL: &str = "https://github.com/SklorGalore/nimbus/releases";

/// GitHub's "latest release" endpoint: the newest release that is neither
/// a draft nor a pre-release.
const LATEST_URL: &str = "https://api.github.com/repos/SklorGalore/nimbus/releases/latest";

/// How long the whole request may take; past this the check fails and
/// says so, rather than hanging a worker thread.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The most a response body is read: a release description is a few KB.
const BODY_LIMIT: u64 = 1024 * 1024;

/// The latest release GitHub reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version, without the tag's `v`: `"0.3.0"`.
    pub version: String,
    /// Its page on GitHub, where the packages are.
    pub url: String,
}

/// Why a check could not say anything.
#[derive(Debug)]
pub enum UpdateError {
    /// The request failed: no network, a certificate the system does not
    /// trust, or GitHub answering with an error.
    Request(String),
    /// The answer had no `tag_name`, or one that is not a version.
    Malformed,
    /// The running version is not `MAJOR.MINOR.PATCH`.
    Version(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(e) => write!(f, "Cannot reach GitHub: {e}"),
            Self::Malformed => f.write_str("GitHub's answer had no release version in it"),
            Self::Version(v) => write!(f, "Running version {v:?} is not a version number"),
        }
    }
}

impl std::error::Error for UpdateError {}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The latest release is newer than the running version.
    Newer(Release),
    /// The running version is the latest release, or ahead of it (a
    /// build from source after the last tag).
    UpToDate(Release),
}

/// Ask GitHub for the latest release and compare it with `current`, the
/// running version. Blocks for up to [`TIMEOUT`]: call it on a worker.
///
/// # Errors
///
/// When the request fails, the answer cannot be read, or `current` is not
/// a version number.
pub fn check(current: &str) -> Result<Outcome, UpdateError> {
    let running = parse_version(current).ok_or_else(|| UpdateError::Version(current.to_owned()))?;
    let body = fetch(LATEST_URL)?;
    let release = parse_latest(&body).ok_or(UpdateError::Malformed)?;
    let latest = parse_version(&release.version).ok_or(UpdateError::Malformed)?;
    Ok(if latest > running {
        Outcome::Newer(release)
    } else {
        Outcome::UpToDate(release)
    })
}

/// GET `url` and return its body as text.
fn fetch(url: &str) -> Result<String, UpdateError> {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("nimbus/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| UpdateError::Request(e.to_string()))?;
    response
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_string()
        .map_err(|e| UpdateError::Request(e.to_string()))
}

/// Pull the release out of GitHub's JSON answer: the `tag_name` field,
/// with its leading `v` dropped. The release page is built from the tag
/// rather than read from the answer, which has several `html_url`s.
#[must_use]
pub fn parse_latest(json: &str) -> Option<Release> {
    let tag = json_string(json, "tag_name")?;
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    Some(Release {
        version: version.to_owned(),
        url: format!("{RELEASES_URL}/tag/{tag}"),
    })
}

/// The value of the first `"key": "value"` pair in `json`, when the value
/// is a string with no escapes (a tag never has any).
fn json_string<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let after_key = &json[json.find(&needle)? + needle.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?.trim_start();
    let value = after_colon.strip_prefix('"')?;
    let end = value.find('"')?;
    let value = &value[..end];
    (!value.contains('\\')).then_some(value)
}

/// A version as three numbers, comparable in order: 0.10.0 is newer than
/// 0.9.1, which string comparison gets wrong. A suffix after `-` (a
/// pre-release such as `0.3.0-rc1`) is dropped, so a pre-release counts
/// as its release; GitHub's "latest" never names one anyway.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
    let core = text.split('-').next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// Open `url` in the system's browser: `open` on macOS, `xdg-open` on
/// Linux, the URL handler on Windows. The browser is started and not
/// waited for.
///
/// # Errors
///
/// When the opener program cannot be started.
pub fn open_url(url: &str) -> Result<(), String> {
    // Only http(s) links: handing an arbitrary string to the opener could
    // run a program on some systems.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("not a web link: {url}"));
    }
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn().map(drop).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("0.9.1") < parse_version("0.10.0"));
        assert_eq!(parse_version("v1.0.0"), parse_version("1.0.0"));
        assert!(parse_version("1.0.1") > parse_version("1.0.0"));
        assert_eq!(parse_version("0.3.0-rc1"), Some((0, 3, 0)));
        assert_eq!(parse_version("0.3"), None);
        assert_eq!(parse_version("0.3.0.1"), None);
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn latest_release_is_read_from_the_tag() {
        let json = r#"{"url":"https://api.github.com/x","html_url":"https://github.com/SklorGalore/nimbus/releases/tag/v0.3.0","tag_name": "v0.3.0","name":"Nimbus 0.3.0","author":{"html_url":"https://github.com/SklorGalore"}}"#;
        assert_eq!(
            parse_latest(json),
            Some(Release {
                version: "0.3.0".to_owned(),
                url: "https://github.com/SklorGalore/nimbus/releases/tag/v0.3.0".to_owned(),
            })
        );
    }

    #[test]
    fn malformed_answers_are_refused() {
        assert_eq!(parse_latest("{}"), None);
        assert_eq!(parse_latest(r#"{"tag_name": 3}"#), None);
        assert_eq!(parse_latest(r#"{"tag_name": "v0.3.0"#), None);
        assert_eq!(parse_latest(r#"{"tag_name": ""}"#), None);
        assert_eq!(parse_latest(r#"{"tag_name": "v0.3.0\n"}"#), None);
        assert_eq!(parse_latest(r#"{"tag_name": "../x"}"#), None);
        assert_eq!(parse_latest(r#"{"message": "Not Found"}"#), None);
    }

    /// Opt-in: talks to GitHub. `cargo test -p nimbus live_check -- --ignored`.
    #[test]
    #[ignore = "needs the network"]
    fn live_check_finds_a_release() {
        let outcome = check("0.0.1").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(outcome, Outcome::Newer(_)), "{outcome:?}");
        let outcome = check("999.0.0").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(outcome, Outcome::UpToDate(_)), "{outcome:?}");
    }

    #[test]
    fn only_web_links_are_opened() {
        assert!(open_url("file:///etc/passwd").is_err());
        assert!(open_url("nimbus.exe").is_err());
    }
}
