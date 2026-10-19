//! Fuzz the parts of `git.rs` that read git's output (M26): the error
//! text shown in the message line, and the `git config --show-scope
//! --list` listing that decides whether a repository's own config names a
//! filter program. Git runs inside folders the user did not create, and
//! its output can carry anything a repository's config holds.
//!
//! Neither may panic; the error text is at most two lines; and the filter
//! check agrees with a plain re-reading of the rule: a line whose scope is
//! `local` or `worktree` and whose key is `filter.NAME.smudge` or
//! `filter.NAME.process`.
//!
//! The app's own `git.rs` is compiled in with `#[path]`:
//! `cargo fuzz run git_output --features app-git`.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code, reason = "the app's module, of which this uses a part")]
#[path = "../../crates/app/src/git.rs"]
mod git;

fuzz_target!(|data: &[u8]| {
    let status = exit_status();
    let message = git::first_lines(data, status);
    assert!(message.lines().count() <= 2, "{message:?}");

    let expected = String::from_utf8_lossy(data).lines().any(|line| {
        let mut parts = line.splitn(2, '\t');
        let (Some(scope), Some(entry)) = (parts.next(), parts.next()) else {
            return false;
        };
        let key = entry.split('=').next().unwrap_or_default();
        (scope == "local" || scope == "worktree")
            && key.starts_with("filter.")
            && (key.ends_with(".smudge") || key.ends_with(".process"))
    });
    assert_eq!(git::names_repo_filter(data), expected);
});

/// A failed exit status, as git's would be.
fn exit_status() -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1 << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }
}
