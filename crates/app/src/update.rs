//! Updates: ask GitHub for the latest release, and install it when the
//! person asks (M31).
//!
//! **Checking.** "Check for Updates" in the palette, and once after the
//! first frame at each start unless the config says
//! `check_updates = false`, asks GitHub's API for the latest release and
//! compares it with the running version. That is one HTTPS request with
//! a `nimbus/X.Y.Z` user agent and nothing else; the startup check speaks
//! only when a newer release exists.
//!
//! **Installing** happens only on request: Install Update downloads the
//! release's package for this system on a worker ([`download`]), proves
//! it is the owner's signed release for that version ([`verify`]), and
//! unpacks it beside the running copy ([`install`]). Restart to Update
//! (or simply quitting) swaps it in.
//!
//! The requests go over HTTPS with `ureq` and the system's certificate
//! store (`rustls-platform-verifier`: Keychain on macOS, the Windows
//! store, `/etc/ssl` on Linux), so a corporate or test certificate the
//! system trusts works without a bundled root list. The one JSON field
//! wanted, `tag_name`, is read with a small scan rather than a JSON
//! crate: the shape is fixed and a wrong guess only means "cannot check".
//! Package names are built from the tag, so no other field is needed.

pub mod download;
pub mod install;
pub mod verify;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use download::Source;
use install::{Plan, Staged};
use verify::VerifyError;

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

/// Why a check or an update could not go on.
#[derive(Debug)]
pub enum UpdateError {
    /// The request failed: no network, a certificate the system does not
    /// trust, or GitHub answering with an error.
    Request(String),
    /// The answer had no `tag_name`, or one that is not a version.
    Malformed,
    /// The running version is not `MAJOR.MINOR.PATCH`.
    Version(String),
    /// The release has no `SHA256SUMS.minisig`: it was never signed, so
    /// it is not installed.
    Unsigned,
    /// The release failed verification.
    Verify(VerifyError),
    /// The package is larger than any release could be.
    TooLarge,
    /// Cancel Update was run.
    Cancelled,
    /// A file could not be written or unpacked (a full disk, a folder
    /// that cannot be written).
    Io(String),
    /// This install is not updated by Nimbus; the text says what to do.
    NotHere(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(e) => write!(f, "Cannot reach GitHub: {e}"),
            Self::Malformed => f.write_str("GitHub's answer had no release version in it"),
            Self::Version(v) => write!(f, "Running version {v:?} is not a version number"),
            Self::Unsigned => f.write_str(
                "This release is not signed, so Nimbus will not install it; use Open Latest Release Page",
            ),
            Self::Verify(e) => write!(f, "Update refused: {e}"),
            Self::TooLarge => f.write_str("Update refused: the package is far larger than a release"),
            Self::Cancelled => f.write_str("Update cancelled"),
            Self::Io(e) => write!(f, "Cannot prepare the update: {e}"),
            Self::NotHere(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<VerifyError> for UpdateError {
    fn from(e: VerifyError) -> Self {
        Self::Verify(e)
    }
}

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

/// Where downloads wait until they are unpacked: `nimbus/updates` in the
/// user's cache folder (`~/Library/Caches` on macOS, `~/.cache` on Linux,
/// `%LOCALAPPDATA%` on Windows), never next to the person's files.
#[must_use]
pub fn download_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("nimbus").join("updates"))
}

/// Delete what earlier downloads left in `dir`.
pub fn clear_downloads(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let _ = std::fs::remove_file(entry.path());
    }
}

/// Everything Install Update does before the restart, in order: fetch
/// release `version`'s `SHA256SUMS` and signature, check the signature
/// with `keys` (and that `version` is newer than `running`), download the
/// package `plan` needs into `dir` while hashing it, check the hash
/// against the signed list, then unpack it beside the running copy.
/// Nothing is written in place of the running copy here. Blocks: call it
/// on a worker.
///
/// # Errors
///
/// At the first step that fails; a package that failed its checks has
/// been deleted.
#[expect(
    clippy::too_many_arguments,
    reason = "each input is a separate seam for the tests"
)]
pub fn prepare(
    source: &Source,
    keys: &[&str],
    plan: &Plan,
    version: &str,
    running: &str,
    dir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Staged, UpdateError> {
    if let Some(why) = plan.refusal(version) {
        return Err(UpdateError::NotHere(why));
    }
    let asset = plan
        .asset(version)
        .ok_or_else(|| UpdateError::NotHere("No package for this system".to_owned()))?;
    let signature = source
        .fetch_small(version, "SHA256SUMS.minisig")?
        .ok_or(UpdateError::Unsigned)?;
    let signature =
        String::from_utf8(signature).map_err(|_| VerifyError::Malformed("signature"))?;
    let sums = source
        .fetch_small(version, "SHA256SUMS")?
        .ok_or(UpdateError::Unsigned)?;
    // Before the large download: a release that is not the owner's is
    // refused without fetching its package.
    let checked = verify::verify_sums(&sums, &signature, keys, version, running)?;
    std::fs::create_dir_all(dir).map_err(|e| UpdateError::Io(e.to_string()))?;
    let package = dir.join(&asset);
    let hash = source.fetch_to(version, &asset, &package, cancel, progress)?;
    if let Err(e) = checked.check(&asset, &hash) {
        let _ = std::fs::remove_file(&package);
        return Err(e.into());
    }
    install::stage(plan, &package, version).map_err(|e| {
        let _ = std::fs::remove_file(&package);
        UpdateError::Io(e.to_string())
    })
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

    /// What the test server answers for one path.
    #[derive(Clone)]
    enum Reply {
        Ok(Vec<u8>),
        /// Says `Content-Length: claimed` but sends only these bytes.
        Truncated(Vec<u8>, usize),
    }

    /// A release server on 127.0.0.1 speaking just enough HTTP/1.1 for
    /// ureq: one request per connection, 404 for anything not in
    /// `files`. Runs until the test process ends.
    fn serve(files: Vec<(String, Reply)>) -> Source {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = first.split(' ').nth(1).unwrap_or("").to_owned();
                let reply = files
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, r)| r.clone());
                let (status, body, len) = match reply {
                    Some(Reply::Ok(b)) => ("200 OK", b.clone(), b.len()),
                    Some(Reply::Truncated(b, claimed)) => ("200 OK", b, claimed),
                    None => ("404 Not Found", Vec::new(), 0),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Source {
            base: format!("http://127.0.0.1:{port}/download"),
            https_only: false,
        }
    }

    /// The fake 2.0.0 release in `update/testdata`, signed with the test
    /// key; `change` adjusts it for each failure.
    fn release(change: impl FnOnce(&mut Vec<(String, Reply)>)) -> Source {
        let mut files = vec![
            (
                "/download/v2.0.0/SHA256SUMS".to_owned(),
                Reply::Ok(verify::tests::SUMS.to_vec()),
            ),
            (
                "/download/v2.0.0/SHA256SUMS.minisig".to_owned(),
                Reply::Ok(verify::tests::SIG.as_bytes().to_vec()),
            ),
        ];
        for name in [
            "Nimbus-2.0.0-macos.zip",
            "Nimbus-2.0.0-x86_64.AppImage",
            "nimbus-2.0.0-linux-x86_64.tar.gz",
            "nimbus-2.0.0-windows-x86_64-setup.exe",
        ] {
            files.push((
                format!("/download/v2.0.0/{name}"),
                Reply::Ok(verify::tests::fake_package(name)),
            ));
        }
        change(&mut files);
        serve(files)
    }

    fn set(files: &mut [(String, Reply)], file: &str, reply: &Reply) {
        let path = format!("/download/v2.0.0/{file}");
        for (p, r) in files.iter_mut() {
            if *p == path {
                *r = reply.clone();
            }
        }
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("nimbus-update-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Run [`prepare`] for an `AppImage` install in `d` against `source`.
    fn prepare_in(d: &Dir, source: &Source, cancel: bool) -> Result<Staged, UpdateError> {
        let image = d.0.join("Nimbus.AppImage");
        std::fs::write(&image, "old").unwrap();
        let plan = Plan::AppImage(image);
        let cancel = AtomicBool::new(cancel);
        let mut seen = Vec::new();
        let result = prepare(
            source,
            &[verify::tests::TEST_KEY],
            &plan,
            "2.0.0",
            "1.1.0",
            &d.0.join("downloads"),
            &cancel,
            &mut |got, total| seen.push((got, total)),
        );
        if result.is_ok() {
            assert_eq!(seen.last().map(|s| s.0), seen.last().and_then(|s| s.1));
        }
        result
    }

    /// Nothing is left in the downloads folder after a failure.
    fn assert_no_download(d: &Dir) {
        let left = std::fs::read_dir(d.0.join("downloads")).map_or(0, Iterator::count);
        assert_eq!(left, 0, "a refused package is deleted");
        assert!(!d.0.join("Nimbus.AppImage.new").exists());
    }

    #[test]
    fn a_good_release_is_staged_beside_the_install() {
        let d = Dir::new("good");
        let staged = prepare_in(&d, &release(|_| {}), false).unwrap();
        let new = d.0.join("Nimbus.AppImage.new");
        assert_eq!(
            staged,
            Staged::Replace {
                version: "2.0.0".to_owned(),
                target: d.0.join("Nimbus.AppImage"),
                new: new.clone(),
                cleanup: None,
            }
        );
        assert_eq!(
            std::fs::read(&new).unwrap(),
            verify::tests::fake_package("Nimbus-2.0.0-x86_64.AppImage")
        );
        assert_eq!(
            std::fs::read_to_string(d.0.join("Nimbus.AppImage")).unwrap(),
            "old"
        );
    }

    #[test]
    fn a_changed_package_is_refused() {
        let d = Dir::new("wrong-hash");
        let source = release(|f| {
            set(
                f,
                "Nimbus-2.0.0-x86_64.AppImage",
                &Reply::Ok(b"evil".to_vec()),
            );
        });
        let err = prepare_in(&d, &source, false).unwrap_err();
        assert!(
            matches!(err, UpdateError::Verify(VerifyError::HashMismatch(_))),
            "{err}"
        );
        assert_no_download(&d);
    }

    #[test]
    fn a_signature_by_another_key_is_refused() {
        let d = Dir::new("bad-sig");
        let other = include_str!("update/testdata/SHA256SUMS-other-key.minisig");
        let source = release(|f| {
            set(
                f,
                "SHA256SUMS.minisig",
                &Reply::Ok(other.as_bytes().to_vec()),
            );
        });
        let err = prepare_in(&d, &source, false).unwrap_err();
        assert!(
            matches!(err, UpdateError::Verify(VerifyError::BadSignature)),
            "{err}"
        );
        assert_no_download(&d);
    }

    #[test]
    fn a_signature_for_another_version_is_refused() {
        let d = Dir::new("old-sig");
        let old = include_str!("update/testdata/SHA256SUMS-old-version.minisig");
        let source = release(|f| {
            set(f, "SHA256SUMS.minisig", &Reply::Ok(old.as_bytes().to_vec()));
        });
        let err = prepare_in(&d, &source, false).unwrap_err();
        assert!(
            matches!(err, UpdateError::Verify(VerifyError::WrongRelease { .. })),
            "{err}"
        );
        assert_no_download(&d);
    }

    #[test]
    fn an_unsigned_release_is_not_installed() {
        let d = Dir::new("unsigned");
        let source = release(|f| f.retain(|(p, _)| !p.ends_with(".minisig")));
        let err = prepare_in(&d, &source, false).unwrap_err();
        assert!(matches!(err, UpdateError::Unsigned), "{err}");
        assert_no_download(&d);
    }

    #[test]
    fn a_truncated_download_is_refused() {
        let d = Dir::new("truncated");
        let name = "Nimbus-2.0.0-x86_64.AppImage";
        let whole = verify::tests::fake_package(name);
        let source = release(|f| {
            set(
                f,
                name,
                &Reply::Truncated(whole[..10].to_vec(), whole.len()),
            );
        });
        let err = prepare_in(&d, &source, false).unwrap_err();
        assert!(matches!(err, UpdateError::Request(_)), "{err}");
        assert_no_download(&d);
    }

    #[test]
    fn a_cancelled_download_stops() {
        let d = Dir::new("cancelled");
        let err = prepare_in(&d, &release(|_| {}), true).unwrap_err();
        assert!(matches!(err, UpdateError::Cancelled), "{err}");
        assert_no_download(&d);
    }

    #[test]
    fn a_folder_that_cannot_be_written_stops_the_download() {
        let d = Dir::new("cannot-write");
        // A file where the downloads folder should be.
        std::fs::write(d.0.join("downloads"), "").unwrap();
        let err = prepare_in(&d, &release(|_| {}), false).unwrap_err();
        assert!(matches!(err, UpdateError::Io(_)), "{err}");
    }

    /// No space left: the package is written through a link to
    /// `/dev/full`, where every write fails with ENOSPC.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_full_disk_stops_the_download() {
        let d = Dir::new("full");
        std::fs::create_dir_all(d.0.join("downloads")).unwrap();
        std::os::unix::fs::symlink(
            "/dev/full",
            d.0.join("downloads/Nimbus-2.0.0-x86_64.AppImage"),
        )
        .unwrap();
        let err = prepare_in(&d, &release(|_| {}), false).unwrap_err();
        assert!(matches!(err, UpdateError::Io(_)), "{err}");
        assert!(err.to_string().to_lowercase().contains("space"), "{err}");
    }

    #[test]
    fn installs_owned_by_others_are_handed_back() {
        let d = Dir::new("managed");
        let err = prepare(
            &release(|_| {}),
            &[verify::tests::TEST_KEY],
            &Plan::Managed(install::Manager::Homebrew),
            "2.0.0",
            "1.1.0",
            &d.0,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("brew upgrade --cask nimbus"),
            "{err}"
        );
    }

    #[test]
    fn only_web_links_are_opened() {
        assert!(open_url("file:///etc/passwd").is_err());
        assert!(open_url("nimbus.exe").is_err());
    }
}
