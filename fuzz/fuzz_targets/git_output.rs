//! Fuzz the parts of `git.rs` that read git's output (M26): the error
//! text shown in the message line, and the `git config --show-scope
//! --list` listing that decides whether a repository's own config names a
//! filter program. Git runs inside folders the user did not create, and
//! its output can carry anything a repository's config holds.
//!
//! The git window's parsers (M27) read the same untrusted output:
//! `status`, `log`, `for-each-ref`, `diff-tree`, `cat-file` and a commit
//! object. None of them may panic on any bytes, and feeding log records
//! in two pieces gives the same records as feeding them whole.
//!
//! Neither of the first two may panic; the error text is at most two
//! lines; and the filter check agrees with a plain re-reading of the rule: a line whose scope is
//! `local` or `worktree` and whose key is `filter.NAME.smudge` or
//! `filter.NAME.process`.
//!
//! The app's own `git.rs` is compiled in with `#[path]`:
//! `cargo fuzz run git_output --features app-git`.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[allow(
    dead_code,
    unused_imports,
    reason = "the app's module, of which this uses a part"
)]
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

    let _ = git::parse::status(data);
    let _ = git::parse::refs(data);
    let _ = git::parse::diff_tree(data);
    let _ = git::parse::batch(data, 3);
    let commit = git::parse::commit(data);
    let _ = git::parse::date(commit.author.time, commit.author.offset);
    let mut ago = String::new();
    git::parse::ago(&mut ago, commit.author.time, commit.committer.time);
    let text = String::from_utf8_lossy(data);
    let _ = git::parse::version(&text);
    let _ = git::parse::LogRecord::parse(&text);
    let whole = records(&[data]);
    let cut = usize::from(data.first().copied().unwrap_or(0)).min(data.len());
    let (a, b) = data.split_at(cut);
    assert_eq!(records(&[a, b]), whole);
});

/// The NUL-separated records in `chunks`, fed one chunk at a time.
fn records(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut r = git::parse::Records::default();
    for chunk in chunks {
        r.feed(chunk, |rec| out.push(rec.to_vec()));
    }
    out
}

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
