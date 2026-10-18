//! The crash record: one line in `crash.log` when Nimbus panics.
//!
//! Release builds use `panic = "abort"`, so a panic ends the process at
//! once and nothing else gets a chance to run. Rust lets a program install
//! a *panic hook*: a function the runtime calls between the panic and the
//! abort. Ours writes the version, where the panic happened and its
//! message to `crash.log` beside `config.toml`, then hands over to the
//! default hook (which prints the same to stderr) and the abort proceeds.
//!
//! The hook must not make things worse, so it writes with plain `std::fs`,
//! ignores its own errors (a full disk or a missing folder just means no
//! record) and never touches documents. No text from the user's buffers
//! is written: some standard panic messages quote the string they were
//! slicing, so anything between backticks is cut out, newlines are
//! flattened and the line is capped (`DESIGN.md` §9, M21).
//!
//! On the next start, [`take_report`] turns the record into a message for
//! the message line and renames the file to `crash.log.old`, so the
//! message shows once while the record stays around for a bug report.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// The longest message kept, in bytes, so one record stays one short line.
const MAX_MESSAGE: usize = 300;

/// Where the record goes: beside the config file, like `state.toml`.
fn path() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("crash.log")))
}

/// Install the hook. Called first thing in `main`, so a panic anywhere
/// later, on any thread, leaves a record.
pub fn install() {
    let Some(path) = path() else { return };
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map_or_else(String::new, |l| format!("{}:{}", l.file(), l.line()));
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("(no message)");
        let line = record(env!("CARGO_PKG_VERSION"), unix_now(), &location, message);
        // Errors are ignored on purpose: the abort proceeds either way.
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::File::create(&path) {
            let _ = f.write_all(line.as_bytes());
        }
        default(info);
    }));
}

/// Seconds since 1970, or 0 if the clock is before it.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The one line written: `time=… version=… at=… message`, ending in a
/// newline.
fn record(version: &str, time: u64, location: &str, message: &str) -> String {
    format!(
        "time={time} version={version} at={location} {}\n",
        scrub(message)
    )
}

/// The panic message with quoted text removed, on one line, capped.
/// Standard library messages put the string they were working on between
/// backticks (``byte index 5 is not a char boundary … of `text` ``); that
/// text may be the user's, so it becomes `…`.
fn scrub(message: &str) -> String {
    let mut out = String::with_capacity(message.len().min(MAX_MESSAGE));
    let mut quoted = false;
    for c in message.chars() {
        if c == '`' {
            if !quoted {
                out.push('…');
            }
            quoted = !quoted;
            continue;
        }
        if quoted {
            continue;
        }
        if out.len() + c.len_utf8() > MAX_MESSAGE {
            out.push('…');
            break;
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    out
}

/// If the last run left a record, a message saying so, and the record is
/// moved to `crash.log.old` so the message shows only once.
#[must_use]
pub fn take_report() -> Option<String> {
    let path = path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let old = path.with_extension("log.old");
    if let Err(e) = std::fs::rename(&path, &old) {
        log::warn!("cannot move {}: {e}", path.display());
    }
    Some(report(text.lines().next().unwrap_or(""), &old))
}

/// The message-line text for a record `line` kept at `old`.
fn report(line: &str, old: &std::path::Path) -> String {
    let version = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("version="))
        .unwrap_or("?");
    format!(
        "Nimbus {version} quit unexpectedly last time; the record is in {} (no file contents are in it)",
        old.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_text_is_removed() {
        let m =
            "byte index 5 is not a char boundary; it is inside 'é' (bytes 4..6) of `secret text`";
        let s = scrub(m);
        assert!(!s.contains("secret"));
        assert!(s.ends_with("of …"), "{s}");
    }

    #[test]
    fn unterminated_quote_drops_the_rest() {
        assert_eq!(scrub("before `after"), "before …");
    }

    #[test]
    fn one_line_and_capped() {
        let s = scrub(&"a\nb".repeat(500));
        assert!(!s.contains('\n'));
        assert!(s.len() <= MAX_MESSAGE + '…'.len_utf8());
        let r = record("1.0.0", 7, "src/x.rs:3", "boom\nagain");
        assert_eq!(r, "time=7 version=1.0.0 at=src/x.rs:3 boom again\n");
        assert_eq!(r.matches('\n').count(), 1);
    }

    #[test]
    fn report_names_the_version() {
        let r = report(
            "time=7 version=1.0.0 at=a.rs:1 boom",
            std::path::Path::new("/c/crash.log.old"),
        );
        assert!(r.starts_with("Nimbus 1.0.0 quit unexpectedly"), "{r}");
        assert!(r.contains("/c/crash.log.old"));
    }
}
