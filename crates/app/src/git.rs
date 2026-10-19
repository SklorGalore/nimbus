//! Running git: reading a file as it was at a git revision, for "Compare
//! with HEAD", and everything the git window (M27, `nimbus --git`) shows.
//!
//! The editor itself does not understand git (`DESIGN.md` §2); this module
//! is the only place that runs it, and it keeps git at arm's length by running
//! the `git` program as a *subprocess* rather than linking a git library.
//! That costs nothing in binary size and always agrees with the user's own
//! git, config and filters included.
//!
//! A subprocess can misbehave in ways a library call cannot: it can hang (a
//! credential prompt, a slow network filesystem) or print far more than we
//! want to hold in memory. So every call goes through [`run`], which
//! enforces a [`TIMEOUT`] and an output cap ([`MAX_OUTPUT`]) and kills git
//! when either is exceeded. These calls block for up to [`TIMEOUT`], so the
//! app runs them on a worker thread, never on the event loop.
//!
//! The git window adds three submodules: [`parse`] reads git's
//! machine-readable output (*plumbing*: `status --porcelain=v2`, `log -z`,
//! `for-each-ref`, `diff-tree`, `cat-file --batch`), never the text git
//! prints for people, which changes between versions and languages;
//! [`Repo`] is one repository and the calls the window makes on it; and
//! [`Stream`] runs a long call such as `git log` whose output is read as
//! it arrives instead of waiting, capped, for the end.
//!
//! # Untrusted repositories (M26)
//!
//! The folder may be a repository the user cloned or unpacked, and a
//! repository's own `.git/config` can name programs for git to run: a
//! filesystem monitor (`core.fsmonitor`), hooks, and filters that rewrite
//! a file on checkout (`filter.NAME.smudge`). Running those would let
//! opening a file in Nimbus run code. So every call turns the monitor and
//! hooks off ([`git`]), never fetches missing objects from a remote, and
//! [`show`] applies checkout filters only when no filter program comes
//! from the repository's own config ([`repo_filters`]); filters the user
//! installed themselves, such as Git LFS in `~/.gitconfig`, still apply.

use std::fmt;
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub mod parse;
mod repo;
mod stream;

pub use parse::{Commit, FileChange, Kind, Ref, RefKind, Status, ago, date};
pub use repo::{Repo, Stash};
pub use stream::Stream;

#[cfg(test)]
pub(crate) use repo::tests::{Dir as TestDir, repo as test_repo};

/// How long one git call may take before it is killed. Reading one blob
/// takes milliseconds; anything near 10 s means git is stuck waiting on
/// something (a prompt, a lock, a dead network drive) that will not resolve.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The most git output kept, in bytes: 256 MB, the same limit a full diff
/// accepts, so a file too big to compare is refused before it is read whole.
pub const MAX_OUTPUT: usize = 256 << 20;

/// How much of git's error output is kept. Only the first lines are shown,
/// and a misbehaving child must not grow this without bound either.
const MAX_STDERR: usize = 64 << 10;

/// The most config text [`repo_filters`] reads. Real configs are a few
/// kilobytes; a larger one counts as unknown, so filters stay off.
const MAX_CONFIG: usize = 1 << 20;

/// How often [`run`] checks whether git has exited.
const POLL: Duration = Duration::from_millis(10);

/// Why a revision could not be read. Each variant's `Display` text is a
/// plain-English sentence shown in the editor's message line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// The `git` program could not be started: not installed, or not on
    /// `PATH`.
    NotFound,
    /// The file's folder is not inside a git repository.
    NotARepo,
    /// git ran longer than [`TIMEOUT`] and was killed.
    TimedOut,
    /// git's output passed [`MAX_OUTPUT`] and it was killed.
    TooLarge,
    /// git ran and reported an error; this is its own message, trimmed to
    /// the first lines (for example "path 'x' does not exist in 'HEAD'").
    Failed(String),
    /// Something on our side failed: the file's path could not be resolved,
    /// or a pipe to git broke. Holds the system's message.
    Io(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("git is not installed or not on PATH"),
            Self::NotARepo => f.write_str("not inside a git repository"),
            Self::TimedOut => write!(
                f,
                "git took longer than {} s and was stopped",
                TIMEOUT.as_secs()
            ),
            Self::TooLarge => write!(
                f,
                "the file at that revision is over {} MB",
                MAX_OUTPUT >> 20
            ),
            Self::Failed(message) => write!(f, "git: {message}"),
            Self::Io(message) => write!(f, "could not run git: {message}"),
        }
    }
}

impl std::error::Error for GitError {}

/// Finds the repository holding `file`: its top folder and the file's path
/// inside it, with `/` separators (the form git expects after `REV:`).
///
/// The repository is found by `git rev-parse --show-toplevel` run in the
/// file's folder, so worktrees, submodules and `GIT_DIR` tricks are git's
/// business, not ours. Both the file and the top folder are canonicalized
/// (symlinks resolved) before comparing them: on macOS `/var` is a symlink
/// to `/private/var`, and git reports the resolved form, so comparing the
/// raw paths would wrongly conclude the file is outside the repository.
///
/// # Errors
///
/// [`GitError::Io`] when the file does not exist or its path cannot be
/// resolved, [`GitError::NotARepo`] when its folder is not in a repository,
/// and the errors of [`run`] ([`GitError::NotFound`], [`GitError::TimedOut`],
/// [`GitError::Failed`]).
#[cfg(test)]
fn locate(file: &Path) -> Result<(PathBuf, String), GitError> {
    locate_with(file, TIMEOUT)
}

/// [`locate`] with the timeout as a parameter, so tests can shorten it.
fn locate_with(file: &Path, timeout: Duration) -> Result<(PathBuf, String), GitError> {
    let file = canonical(file)?;
    let folder = file
        .parent()
        .ok_or_else(|| GitError::Io(format!("{} has no folder", file.display())))?;
    let mut cmd = git();
    cmd.arg("-C")
        .arg(folder)
        .args(["rev-parse", "--show-toplevel"]);
    let out = match run(cmd, timeout, MAX_STDERR) {
        Ok(out) => out,
        Err(GitError::Failed(message)) if message.contains("not a git repository") => {
            return Err(GitError::NotARepo);
        }
        Err(e) => return Err(e),
    };
    let printed = String::from_utf8_lossy(&out);
    let printed = printed.trim_end_matches(['\r', '\n']);
    if printed.is_empty() {
        // A bare repository or the inside of `.git` has no work tree.
        return Err(GitError::NotARepo);
    }
    let top = canonical(Path::new(printed))?;
    let inside = file.strip_prefix(&top).map_err(|_| {
        GitError::Failed(format!(
            "{} is not inside the repository at {}",
            file.display(),
            top.display()
        ))
    })?;
    let mut rel = String::new();
    for part in inside.components() {
        let Component::Normal(name) = part else {
            continue;
        };
        let name = name
            .to_str()
            .ok_or_else(|| GitError::Failed("the file's path is not valid UTF-8".into()))?;
        if !rel.is_empty() {
            rel.push('/');
        }
        rel.push_str(name);
    }
    Ok((top, rel))
}

/// The contents of `file` at revision `rev`, as `git checkout` would write
/// them to disk. `rev` is anything git accepts as a revision: `HEAD`,
/// `HEAD~2`, a branch or tag name, or a commit hash.
///
/// This runs `git cat-file --filters REV:PATH` rather than the plan's
/// `git show REV:PATH`. Both read the stored blob, but `--filters` also
/// applies the conversions checkout applies (line-ending conversion from
/// `core.autocrlf` or `.gitattributes`, and smudge filters such as Git
/// LFS). Without them, a Windows working copy with CRLF line endings would
/// differ from the LF blob on every single line.
///
/// # Errors
///
/// [`GitError::Failed`] when `rev` is empty or starts with `-` (it would be
/// read as an option: argument injection), or when git cannot find the
/// revision or the path in it (with git's message); the errors of
/// [`locate`]; and [`GitError::TimedOut`] or [`GitError::TooLarge`] from
/// [`run`].
pub fn show(file: &Path, rev: &str) -> Result<Vec<u8>, GitError> {
    show_with(file, rev, TIMEOUT, MAX_OUTPUT)
}

/// [`show`] with the timeout and the output cap as parameters, so tests can
/// exercise both limits quickly.
fn show_with(file: &Path, rev: &str, timeout: Duration, cap: usize) -> Result<Vec<u8>, GitError> {
    let rev = rev.trim();
    if rev.is_empty() {
        return Err(GitError::Failed("no revision given".into()));
    }
    if rev.starts_with('-') {
        return Err(GitError::Failed(format!(
            "a revision cannot start with '-': {rev}"
        )));
    }
    let (top, rel) = locate_with(file, timeout)?;
    // `--filters` runs the filter programs `.gitattributes` asks for; when
    // the repository defines one itself, read the stored blob as it is
    // instead (no line-ending conversion either, which is the cost).
    let filters = if repo_filters(&top, timeout) {
        log::info!(
            "{}: the repository's own config names a filter program; reading {rev}:{rel} unfiltered",
            top.display()
        );
        "blob"
    } else {
        "--filters"
    };
    let mut cmd = git();
    cmd.arg("-C")
        .arg(&top)
        .args(["cat-file", filters])
        .arg(format!("{rev}:{rel}"));
    run(cmd, timeout, cap)
}

/// Whether the repository at `top` names a filter program in its own
/// config (`.git/config`, a worktree's config, or a file either includes)
/// rather than the user's or the system's: `filter.NAME.smudge` or
/// `filter.NAME.process`. Those come with the repository, so running them
/// could run code someone else wrote. Also `true` when git cannot say,
/// such as a git older than 2.26 without `--show-scope`.
fn repo_filters(top: &Path, timeout: Duration) -> bool {
    let mut cmd = git();
    cmd.arg("-C")
        .arg(top)
        .args(["config", "--show-scope", "--list"]);
    let Ok(out) = run(cmd, timeout, MAX_CONFIG) else {
        return true;
    };
    names_repo_filter(&out)
}

/// Whether `config`, the output of `git config --show-scope --list`, has a
/// filter program from the repository's own config (see
/// [`repo_filters`]). Lines are `SCOPE<tab>KEY=VALUE`; a value may hold
/// anything, `=` and tabs included, so only the part before the first
/// `=` is the key.
pub(crate) fn names_repo_filter(config: &[u8]) -> bool {
    String::from_utf8_lossy(config).lines().any(|line| {
        let Some((scope, entry)) = line.split_once('\t') else {
            return false;
        };
        let key = entry.split_once('=').map_or(entry, |(k, _)| k);
        matches!(scope, "local" | "worktree")
            && key.starts_with("filter.")
            && (key.ends_with(".smudge") || key.ends_with(".process"))
    })
}

/// A `git` command with the environment every call needs.
///
/// `GIT_TERMINAL_PROMPT=0` makes git fail instead of asking for a password
/// on a terminal nobody is looking at, and `GIT_OPTIONAL_LOCKS=0` stops a
/// read from taking the index lock, so we never block the user's own git.
///
/// The rest keeps a repository's own config from running programs (M26,
/// see the module notes): `core.fsmonitor=false` turns off the filesystem
/// monitor a config can name; `core.hooksPath` points at a folder that does
/// not exist, so no hook can run (none of our commands runs hooks today;
/// this keeps it so as commands are added); and `GIT_NO_LAZY_FETCH=1`
/// stops a partial clone fetching a missing object from whatever remote
/// its config names (git 2.45 and later; older ones ignore it).
fn git() -> Command {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
    ])
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env("GIT_NO_LAZY_FETCH", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a GUI app starting a console program would
        // otherwise flash a console window on screen.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Runs `cmd` and returns what it printed on stdout, enforcing `timeout`
/// and a cap of `cap` bytes on the output.
///
/// The child's stdout and stderr are pipes, and a pipe holds only a few
/// kilobytes: if nobody reads it, the child blocks writing and never exits.
/// So each pipe is drained on a helper thread while this thread polls
/// [`Child::try_wait`] against the deadline. When the deadline passes, or
/// stdout passes the cap, the child is killed. The helpers then finish on
/// their own once the pipe closes; they are not waited for, because a
/// grandchild (a shell started by an alias or filter) can keep the pipe
/// open after git itself is gone.
///
/// # Errors
///
/// [`GitError::NotFound`] when the program cannot be started,
/// [`GitError::TimedOut`] and [`GitError::TooLarge`] for the two limits,
/// [`GitError::Failed`] with the first lines of stderr when the command
/// exits unsuccessfully, and [`GitError::Io`] for other failures.
fn run(cmd: Command, timeout: Duration, cap: usize) -> Result<Vec<u8>, GitError> {
    run_input(cmd, None, timeout, cap)
}

/// [`run`] with `input` written to git's standard input (then closed), for
/// commands that read their requests there, such as `cat-file --batch`.
/// The input is written on a helper thread: git may answer before it has
/// read everything, and a full output pipe would otherwise deadlock both.
fn run_input(
    mut cmd: Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    cap: usize,
) -> Result<Vec<u8>, GitError> {
    let deadline = Instant::now() + timeout;
    cmd.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        ErrorKind::NotFound => GitError::NotFound,
        _ => GitError::Io(e.to_string()),
    })?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        thread::spawn(move || {
            use std::io::Write as _;
            // A failed write means git stopped reading: it exited, and its
            // status says why.
            let _ = stdin.write_all(&input);
        });
    }
    let over = Arc::new(AtomicBool::new(false));
    let stdout = drain(child.stdout.take(), cap, Some(Arc::clone(&over)));
    let stderr = drain(child.stderr.take(), MAX_STDERR, None);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                stop(&mut child);
                return Err(GitError::Io(e.to_string()));
            }
        }
        if over.load(Ordering::Relaxed) {
            stop(&mut child);
            return Err(GitError::TooLarge);
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            return Err(GitError::TimedOut);
        }
        thread::sleep(POLL);
    };
    let out = collect(&stdout, deadline)?;
    if out.over {
        return Err(GitError::TooLarge);
    }
    if !status.success() {
        let err = collect(&stderr, deadline)?;
        return Err(GitError::Failed(first_lines(&err.bytes, status)));
    }
    Ok(out.bytes)
}

/// What a helper thread read from one pipe.
struct Drained {
    /// The bytes read, at most the cap.
    bytes: Vec<u8>,
    /// Whether the pipe had more than the cap.
    over: bool,
}

/// Reads `pipe` to its end on a new thread, keeping at most `cap` bytes.
/// When the pipe has more, the thread stops reading, sets `over_flag` so
/// [`run`] can kill the child early, and reports `over`.
fn drain<R: Read + Send + 'static>(
    pipe: Option<R>,
    cap: usize,
    over_flag: Option<Arc<AtomicBool>>,
) -> mpsc::Receiver<std::io::Result<Drained>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = (|| {
            let mut bytes = Vec::new();
            let Some(pipe) = pipe else {
                return Ok(Drained { bytes, over: false });
            };
            // One byte past the cap tells "exactly the cap" from "more".
            let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
            pipe.take(limit).read_to_end(&mut bytes)?;
            let over = bytes.len() > cap;
            if over {
                bytes.truncate(cap);
                if let Some(flag) = over_flag {
                    flag.store(true, Ordering::Relaxed);
                }
            }
            Ok(Drained { bytes, over })
        })();
        // The receiver is gone when `run` already gave up; nothing to do.
        let _ = tx.send(result);
    });
    rx
}

/// Waits for a helper's result until `deadline`.
fn collect(
    rx: &mpsc::Receiver<std::io::Result<Drained>>,
    deadline: Instant,
) -> Result<Drained, GitError> {
    let left = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(left) {
        Ok(Ok(drained)) => Ok(drained),
        Ok(Err(e)) => Err(GitError::Io(e.to_string())),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(GitError::TimedOut),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(GitError::Io("the output reader stopped".into()))
        }
    }
}

/// Kills the child and reaps it so it does not linger as a zombie. Errors
/// are ignored: the child may already have exited on its own.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The first lines of git's error output, without git's `fatal: ` or
/// `error: ` prefix, for the message line. Falls back to the exit status
/// when git printed nothing.
pub(crate) fn first_lines(stderr: &[u8], status: std::process::ExitStatus) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(2)
        .map(|l| {
            l.strip_prefix("fatal: ")
                .or_else(|| l.strip_prefix("error: "))
                .unwrap_or(l)
        })
        .collect();
    if lines.is_empty() {
        format!("exited with {status}")
    } else {
        lines.join("; ")
    }
}

/// `path` with symlinks resolved, in the plain form git understands.
///
/// On Windows, [`std::fs::canonicalize`] returns "verbatim" paths such as
/// `\\?\C:\repo`; git may not accept those after `-C`, so the `\\?\` prefix
/// is removed from ordinary drive paths (network `\\?\UNC\` paths are left
/// alone). Both sides of a comparison go through here, so they stay
/// comparable.
fn canonical(path: &Path) -> Result<PathBuf, GitError> {
    let path = std::fs::canonicalize(path)
        .map_err(|e| GitError::Io(format!("{}: {e}", path.display())))?;
    Ok(plain(path))
}

/// Removes the `\\?\` verbatim prefix from a Windows drive path.
#[cfg(windows)]
fn plain(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// Unix canonical paths are already plain.
#[cfg(not(windows))]
fn plain(path: PathBuf) -> PathBuf {
    path
}

/// Whether `git` can be started at all; tests skip themselves without it.
#[cfg(test)]
fn available() -> bool {
    let mut cmd = git();
    cmd.arg("--version");
    run(cmd, TIMEOUT, MAX_STDERR).is_ok()
}

/// Lets tests run `git` with an argument list in a folder.
#[cfg(test)]
fn in_dir<I, S>(dir: &Path, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = git();
    cmd.arg("-C").arg(dir).args(args);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile_dir::Dir;

    /// Runs git in `dir` and fails the test if it fails.
    fn g(dir: &Path, args: &[&str]) {
        run(in_dir(dir, args), TIMEOUT, MAX_OUTPUT).unwrap();
    }

    /// A repository with `a.txt` committed twice ("one", then "two") and a
    /// branch `side` at the first commit, with the working copy edited.
    fn repo(name: &str) -> Option<Dir> {
        if !available() {
            log::warn!("git is not available; skipping");
            return None;
        }
        let dir = Dir::new(name);
        let p = dir.path();
        g(p, &["init", "-q"]);
        g(p, &["config", "user.name", "Test"]);
        g(p, &["config", "user.email", "test@example.com"]);
        g(p, &["config", "commit.gpgsign", "false"]);
        g(p, &["config", "core.autocrlf", "false"]);
        dir.write("sub/a.txt", "one\n");
        g(p, &["add", "."]);
        g(p, &["commit", "-q", "-m", "first"]);
        g(p, &["branch", "side"]);
        dir.write("sub/a.txt", "two\n");
        g(p, &["commit", "-q", "-am", "second"]);
        dir.write("sub/a.txt", "three, unsaved to git\n");
        Some(dir)
    }

    #[test]
    fn locate_finds_the_top_and_a_slash_path() {
        let Some(dir) = repo("git-locate") else {
            return;
        };
        let (top, rel) = locate(&dir.path().join("sub/a.txt")).unwrap();
        assert_eq!(top, canonical(dir.path()).unwrap());
        assert_eq!(rel, "sub/a.txt");
    }

    #[test]
    fn show_reads_revisions_not_the_working_copy() {
        let Some(dir) = repo("git-show") else { return };
        let file = dir.path().join("sub/a.txt");
        assert_eq!(show(&file, "HEAD").unwrap(), b"two\n");
        assert_eq!(show(&file, "HEAD~1").unwrap(), b"one\n");
        assert_eq!(show(&file, "side").unwrap(), b"one\n");
        assert_eq!(show(&file, " HEAD ").unwrap(), b"two\n");
    }

    #[test]
    fn show_applies_checkout_line_endings() {
        let Some(dir) = repo("git-crlf") else { return };
        g(dir.path(), &["config", "core.autocrlf", "true"]);
        let file = dir.path().join("sub/a.txt");
        assert_eq!(show(&file, "HEAD").unwrap(), b"two\r\n");
    }

    #[test]
    fn a_repositorys_own_filter_program_never_runs() {
        let Some(dir) = repo("git-filter") else {
            return;
        };
        let p = dir.path();
        dir.write(".gitattributes", "*.txt filter=evil\n");
        g(p, &["add", ".gitattributes"]);
        g(p, &["commit", "-q", "-m", "attributes"]);
        // What a hostile clone's .git/config could say.
        g(
            p,
            &["config", "filter.evil.smudge", "touch ran-smudge; cat"],
        );
        g(p, &["config", "filter.evil.process", "touch ran-process"]);
        g(
            p,
            &["config", "core.fsmonitor", "touch ran-fsmonitor; false"],
        );
        g(p, &["config", "core.autocrlf", "true"]);
        let file = p.join("sub/a.txt");
        assert!(repo_filters(&canonical(p).unwrap(), TIMEOUT));
        // The stored blob, unconverted: no filter, so no CRLF either.
        assert_eq!(show(&file, "HEAD").unwrap(), b"two\n");
        for marker in ["ran-smudge", "ran-process", "ran-fsmonitor"] {
            assert!(!p.join(marker).exists(), "{marker} ran");
            assert!(!p.join("sub").join(marker).exists(), "{marker} ran");
        }
    }

    #[test]
    fn errors_say_what_went_wrong() {
        let Some(dir) = repo("git-errors") else {
            return;
        };
        let file = dir.path().join("sub/a.txt");
        // Not in HEAD: git's own message.
        dir.write("new.txt", "x");
        match show(&dir.path().join("new.txt"), "HEAD") {
            Err(GitError::Failed(m)) => assert!(m.contains("new.txt"), "{m}"),
            other => panic!("{other:?}"),
        }
        // A revision that does not exist.
        assert!(matches!(show(&file, "nope"), Err(GitError::Failed(_))));
        // Options are refused before git runs.
        match show(&file, "--output=/tmp/x") {
            Err(GitError::Failed(m)) => assert!(m.contains("'-'"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(show(&file, ""), Err(GitError::Failed(_))));
        // A missing file.
        assert!(matches!(
            show(&dir.path().join("gone.txt"), "HEAD"),
            Err(GitError::Io(_))
        ));
    }

    #[test]
    fn a_folder_outside_any_repository() {
        if !available() {
            return;
        }
        // The system temp folder is not inside a repository.
        let dir = Dir::new("git-norepo");
        dir.write("a.txt", "x");
        assert_eq!(locate(&dir.path().join("a.txt")), Err(GitError::NotARepo));
    }

    #[test]
    fn a_hung_command_is_killed() {
        let Some(dir) = repo("git-timeout") else {
            return;
        };
        let started = Instant::now();
        let cmd = in_dir(dir.path(), ["-c", "alias.nap=!sleep 5", "nap"]);
        assert_eq!(
            run(cmd, Duration::from_millis(100), MAX_OUTPUT),
            Err(GitError::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn output_over_the_cap_is_refused() {
        let Some(dir) = repo("git-cap") else { return };
        let file = dir.path().join("sub/a.txt");
        assert_eq!(
            show_with(&file, "HEAD", TIMEOUT, 2),
            Err(GitError::TooLarge)
        );
        // Exactly the cap is fine.
        assert_eq!(show_with(&file, "HEAD", TIMEOUT, 4).unwrap(), b"two\n");
    }

    #[test]
    fn a_missing_program_is_not_found() {
        let cmd = Command::new("nimbus-no-such-program-here");
        assert_eq!(run(cmd, TIMEOUT, MAX_OUTPUT), Err(GitError::NotFound));
    }

    #[test]
    fn messages_are_plain_english() {
        assert_eq!(
            GitError::TimedOut.to_string(),
            "git took longer than 10 s and was stopped"
        );
        assert_eq!(
            GitError::TooLarge.to_string(),
            "the file at that revision is over 256 MB"
        );
    }

    mod tempfile_dir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        impl Dir {
            pub fn new(name: &str) -> Self {
                let unique = format!(
                    "nimbus-{name}-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                );
                let path = std::env::temp_dir().join(unique);
                let _ = std::fs::remove_dir_all(&path);
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }

            pub fn write(&self, rel: &str, text: &str) {
                let p = self.0.join(rel);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, text).unwrap();
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
