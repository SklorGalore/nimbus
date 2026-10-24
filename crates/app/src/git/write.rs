//! The calls that change a repository (M28): staging, unstaging,
//! discarding and committing.
//!
//! Everything else in this module only reads, with the M26 settings that
//! keep a repository's own config from running programs. These calls run
//! only when the user presses a button, in a repository they opened, and
//! they behave as the same commands typed in a terminal (`docs/security.md`,
//! rules for M27–M30): `git add` runs the clean filters `.gitattributes`
//! asks for, so a staged file is what `git add` would stage, and `git
//! commit` runs the repository's hooks and signs as configured. Two things
//! stay as for reading: no terminal prompt (nobody is at one) and no
//! filesystem monitor (a speed-up only, and a program the repository
//! names). Paths are passed as literal paths (`GIT_LITERAL_PATHSPECS`), so
//! a file named `*` or `:(glob)x` means only itself.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::Repo;
use super::repo::{is_oid, not_an_oid};
use super::{GitError, MAX_OUTPUT, MAX_STDERR, POLL, TIMEOUT, drain, run, run_input, stop};

/// The most output of a commit kept to show: hooks can print a lot.
const MAX_HOOK_OUTPUT: usize = 256 << 10;

/// How long the pipes are read after the commit exits: a hook may leave a
/// program running that holds them open.
const PIPE_GRACE: Duration = Duration::from_secs(2);

/// What `git commit` printed, and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutput {
    /// It made the commit.
    pub ok: bool,
    /// Its output and its hooks', standard output then errors.
    pub text: String,
}

/// A git command for a call that writes, run in `top`.
fn writer(top: &std::path::Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(top)
        .args(["-c", "core.fsmonitor=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LITERAL_PATHSPECS", "1");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW, as in `git()`.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Paths from git's own output, refused if one could be read as an option.
fn paths_ok(paths: &[&str]) -> Result<(), GitError> {
    match paths.iter().find(|p| p.is_empty() || p.contains('\0')) {
        Some(p) => Err(GitError::Failed(format!("not a path: {p:?}"))),
        None => Ok(()),
    }
}

impl Repo {
    fn write_cmd(&self) -> Command {
        writer(&self.top)
    }

    /// Stage these files whole, as `git add` does: changes, new files and
    /// deletions (`-A`). A folder stages everything in it.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn stage(&self, paths: &[&str]) -> Result<(), GitError> {
        paths_ok(paths)?;
        let mut cmd = self.write_cmd();
        cmd.args(["add", "-A", "--"]).args(paths);
        run(cmd, TIMEOUT, MAX_STDERR).map(drop)
    }

    /// Stage every change in the working tree (`git add -A`).
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn stage_all(&self) -> Result<(), GitError> {
        let mut cmd = self.write_cmd();
        cmd.args(["add", "-A"]);
        run(cmd, TIMEOUT, MAX_STDERR).map(drop)
    }

    /// Take these files out of the index back to HEAD's version
    /// (`git restore --staged`), or out of the index altogether before
    /// the first commit (`has_head` false), leaving the working tree.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn unstage(&self, paths: &[&str], has_head: bool) -> Result<(), GitError> {
        paths_ok(paths)?;
        let mut cmd = self.write_cmd();
        if has_head {
            cmd.args(["restore", "--staged", "--"]);
        } else {
            cmd.args(["rm", "--cached", "-r", "-q", "--ignore-unmatch", "--"]);
        }
        cmd.args(paths);
        run(cmd, TIMEOUT, MAX_STDERR).map(drop)
    }

    /// Unstage everything: the index goes back to HEAD (`git reset`), or
    /// is emptied before the first commit.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn unstage_all(&self, has_head: bool) -> Result<(), GitError> {
        let mut cmd = self.write_cmd();
        if has_head {
            cmd.args(["reset", "-q"]);
        } else {
            cmd.args(["rm", "--cached", "-r", "-q", "--ignore-unmatch", "--", "."]);
        }
        run(cmd, TIMEOUT, MAX_STDERR).map(drop)
    }

    /// Apply `patch` (from [`super::patch::build`]) through standard
    /// input, never a temporary file: to the index alone (`cached`), or to
    /// the working tree. A patch that undoes changes is already written
    /// the other way round ([`super::patch::Apply::Reverse`]), so git
    /// never needs its own `--reverse`. git checks every line the
    /// patch names first and changes nothing when one does not match, so
    /// a stale patch fails whole instead of landing in the wrong place.
    ///
    /// # Errors
    ///
    /// The errors of `run`, with git's message when the patch does not
    /// apply.
    pub fn apply(&self, patch: Vec<u8>, cached: bool) -> Result<(), GitError> {
        let mut cmd = self.write_cmd();
        cmd.args(["apply", "--unidiff-zero", "--whitespace=nowarn"]);
        if cached {
            cmd.arg("--cached");
        }
        cmd.arg("-");
        run_input(cmd, Some(patch), TIMEOUT, MAX_STDERR).map(drop)
    }

    /// The working tree's `bytes` for `path` as git would store them: the
    /// clean filters and line-ending conversion `.gitattributes` and
    /// `core.autocrlf` ask for (`hash-object --path`). The result is
    /// written as an object, which only adds a file to the object store
    /// and moves no branch.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn clean(&self, path: &str, bytes: Vec<u8>) -> Result<Vec<u8>, GitError> {
        paths_ok(&[path])?;
        let mut cmd = self.write_cmd();
        cmd.args(["hash-object", "-w", "--stdin"])
            .arg(format!("--path={path}"));
        let oid = run_input(cmd, Some(bytes), TIMEOUT, MAX_STDERR)?;
        self.blob(String::from_utf8_lossy(&oid).trim())
    }

    /// Keep `bytes` in the object store exactly as they are (no filters),
    /// for Undo Discard; returns the object's id. Writing an object moves
    /// no branch and changes nothing the user sees; git's garbage
    /// collection removes it in a few weeks once nothing refers to it.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn keep(&self, bytes: Vec<u8>) -> Result<String, GitError> {
        let mut cmd = self.write_cmd();
        cmd.args(["hash-object", "-w", "--no-filters", "--stdin"]);
        let out = run_input(cmd, Some(bytes), TIMEOUT, MAX_STDERR)?;
        let oid = String::from_utf8_lossy(&out).trim().to_owned();
        if is_oid(&oid) {
            Ok(oid)
        } else {
            Err(not_an_oid(&oid))
        }
    }

    /// The bytes of one object, as stored.
    ///
    /// # Errors
    ///
    /// [`GitError::Failed`] for an id that is not hex, and the errors of
    /// `run`.
    pub fn blob(&self, oid: &str) -> Result<Vec<u8>, GitError> {
        if !is_oid(oid) {
            return Err(not_an_oid(oid));
        }
        let mut cmd = self.write_cmd();
        cmd.args(["cat-file", "blob", oid]);
        run(cmd, TIMEOUT, MAX_OUTPUT)
    }

    /// Put these files in the working tree back to the index's version
    /// (`git restore --worktree`), which also brings back a deleted file.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn restore(&self, paths: &[&str]) -> Result<(), GitError> {
        paths_ok(paths)?;
        let mut cmd = self.write_cmd();
        cmd.args(["restore", "--worktree", "--"]).args(paths);
        run(cmd, TIMEOUT, MAX_STDERR).map(drop)
    }

    /// The last commit's whole message, for Amend.
    ///
    /// # Errors
    ///
    /// The errors of `run` (there is no commit yet, for one).
    pub fn last_message(&self) -> Result<String, GitError> {
        let mut cmd = self.cmd();
        cmd.args(["log", "-1", "--format=%B", "HEAD", "--"]);
        let out = run(cmd, TIMEOUT, MAX_STDERR.max(1 << 20))?;
        Ok(String::from_utf8_lossy(&out).trim_end().to_owned())
    }

    /// Who a commit made now would be by, from git's config:
    /// `Name <email>`.
    ///
    /// # Errors
    ///
    /// The errors of `run`; git fails when no name or email is set.
    pub fn author(&self) -> Result<String, GitError> {
        let mut cmd = self.cmd();
        cmd.args(["var", "GIT_AUTHOR_IDENT"]);
        let out = run(cmd, TIMEOUT, MAX_STDERR)?;
        let text = String::from_utf8_lossy(&out);
        // "Name <email> 1700000000 +0100": drop the time.
        let ident = match text.rfind('>') {
            Some(end) => &text[..=end],
            None => text.trim(),
        };
        Ok(ident.to_owned())
    }

    /// Commit the index with `message` on standard input (`git commit -F
    /// -`), amending the last commit when `amend`. Hooks, signing and the
    /// config apply as in a terminal, and hooks may take long, so there is
    /// no timeout: setting `cancel` stops it.
    ///
    /// # Errors
    ///
    /// [`GitError::NotFound`] without git, [`GitError::Cancelled`] when
    /// cancelled, and [`GitError::Io`] when a pipe fails. git's own
    /// failure (a hook said no, nothing staged) is not an error but a
    /// [`CommitOutput`] with `ok` false and what it printed.
    pub fn make_commit(
        &self,
        message: &str,
        amend: bool,
        cancel: &Arc<AtomicBool>,
    ) -> Result<CommitOutput, GitError> {
        let mut cmd = self.write_cmd();
        cmd.args(["commit", "-F", "-"]);
        if amend {
            cmd.arg("--amend");
        }
        run_cancellable(cmd, message.as_bytes().to_vec(), cancel)
    }
}

/// Run `cmd` with `input` on its standard input until it exits or `cancel`
/// is set, keeping both outputs (each up to [`MAX_HOOK_OUTPUT`]).
fn run_cancellable(
    mut cmd: Command,
    input: Vec<u8>,
    cancel: &Arc<AtomicBool>,
) -> Result<CommitOutput, GitError> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => GitError::NotFound,
        _ => GitError::Io(e.to_string()),
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        thread::spawn(move || {
            // git closing its end early shows in its exit status.
            let _ = stdin.write_all(&input);
        });
    }
    let stdout = drain(child.stdout.take(), MAX_HOOK_OUTPUT, None);
    let stderr = drain(child.stderr.take(), MAX_HOOK_OUTPUT, None);
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            stop(&mut child);
            return Err(GitError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL),
            Err(e) => {
                stop(&mut child);
                return Err(GitError::Io(e.to_string()));
            }
        }
    };
    let deadline = Instant::now() + PIPE_GRACE;
    let mut text = String::new();
    for rx in [&stdout, &stderr] {
        let left = deadline.saturating_duration_since(Instant::now());
        if let Ok(Ok(d)) = rx.recv_timeout(left) {
            let part = String::from_utf8_lossy(&d.bytes);
            let part = part.trim_end();
            if !part.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(part);
            }
        }
    }
    Ok(CommitOutput {
        ok: status.success(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::super::patch::{self, Apply, Pick};
    use super::super::repo::tests::{Dir, repo};
    use super::*;

    fn g(dir: &std::path::Path, args: &[&str]) -> String {
        let mut cmd = super::super::git();
        cmd.arg("-C").arg(dir).args(args);
        String::from_utf8(run(cmd, TIMEOUT, MAX_OUTPUT).unwrap()).unwrap()
    }

    fn open(dir: &Dir) -> Repo {
        Repo::open(&dir.0).unwrap()
    }

    /// A repository with one file committed: ten numbered lines.
    fn ten(name: &str) -> Option<(Dir, Repo)> {
        let dir = repo(name)?;
        let p = dir.0.as_path();
        g(p, &["reset", "-q", "--hard"]);
        let _ = std::fs::remove_file(p.join("new.txt"));
        let text: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        dir.write("ten.txt", &text);
        g(p, &["add", "ten.txt"]);
        g(p, &["commit", "-q", "-m", "ten"]);
        let r = open(&dir);
        Some((dir, r))
    }

    fn index(dir: &Dir, path: &str) -> String {
        g(&dir.0, &["show", &format!(":{path}")])
    }

    #[test]
    fn staging_picked_lines_puts_only_those_in_the_index() {
        let Some((dir, r)) = ten("git-write-lines") else {
            return;
        };
        let old = index(&dir, "ten.txt");
        let new = old
            .replace("line 2\n", "two\n")
            .replace("line 9\n", "nine\n");
        dir.write("ten.txt", &new);
        // Only the change at line 2 (index 1).
        let pick = Pick {
            old: vec![1..2],
            new: vec![1..2],
        };
        let clean = r.clean("ten.txt", new.clone().into_bytes()).unwrap();
        let p = patch::build(
            "ten.txt",
            (old.as_bytes(), &clean),
            &pick,
            Apply::Forward,
            None,
        )
        .unwrap();
        r.apply(p, true).unwrap();
        assert_eq!(index(&dir, "ten.txt"), old.replace("line 2\n", "two\n"));
        // The working tree is untouched.
        assert_eq!(std::fs::read_to_string(dir.0.join("ten.txt")).unwrap(), new);
        // Unstage it again, from the index's point of view.
        let staged = index(&dir, "ten.txt");
        let p = patch::build(
            "ten.txt",
            (old.as_bytes(), staged.as_bytes()),
            &Pick::all(),
            Apply::Reverse,
            None,
        )
        .unwrap();
        r.apply(p, true).unwrap();
        assert_eq!(index(&dir, "ten.txt"), old);
    }

    #[test]
    fn discarding_a_hunk_changes_only_the_working_tree() {
        let Some((dir, r)) = ten("git-write-discard") else {
            return;
        };
        let old = index(&dir, "ten.txt");
        let new = old.replace("line 5\n", "five\n").replace("line 6\n", "");
        dir.write("ten.txt", &new);
        let pick = Pick {
            old: vec![4..6],
            new: vec![4..5],
        };
        let p = patch::build(
            "ten.txt",
            (old.as_bytes(), new.as_bytes()),
            &pick,
            Apply::Reverse,
            None,
        )
        .unwrap();
        r.apply(p, false).unwrap();
        assert_eq!(std::fs::read_to_string(dir.0.join("ten.txt")).unwrap(), old);
    }

    #[test]
    fn a_stale_patch_changes_nothing() {
        let Some((dir, r)) = ten("git-write-stale") else {
            return;
        };
        let old = index(&dir, "ten.txt");
        let shown = old.replace("line 3\n", "three\n");
        let p = patch::build(
            "ten.txt",
            (old.as_bytes(), shown.as_bytes()),
            &Pick::all(),
            Apply::Forward,
            None,
        )
        .unwrap();
        // Meanwhile, from a terminal: line 3 changed in the index.
        dir.write("ten.txt", &old.replace("line 3\n", "THREE\n"));
        g(&dir.0, &["add", "ten.txt"]);
        let before = index(&dir, "ten.txt");
        assert!(r.apply(p, true).is_err());
        assert_eq!(index(&dir, "ten.txt"), before);
    }

    #[test]
    fn crlf_and_no_final_newline_survive_staging() {
        let Some((dir, r)) = ten("git-write-crlf") else {
            return;
        };
        let p = dir.0.as_path();
        dir.write(".gitattributes", "*.txt -text\n");
        dir.write("crlf.txt", "a\r\nb\r\nc");
        g(p, &["add", "."]);
        g(p, &["commit", "-q", "-m", "crlf"]);
        let old = index(&dir, "crlf.txt");
        let new = "a\r\nB\r\nc\r\nd";
        dir.write("crlf.txt", new);
        let clean = r.clean("crlf.txt", new.as_bytes().to_vec()).unwrap();
        // Everything but the change to b.
        let pick = Pick {
            old: vec![2..3],
            new: vec![2..4],
        };
        let patch = patch::build(
            "crlf.txt",
            (old.as_bytes(), &clean),
            &pick,
            Apply::Forward,
            None,
        )
        .unwrap();
        r.apply(patch, true).unwrap();
        assert_eq!(index(&dir, "crlf.txt"), "a\r\nb\r\nc\r\nd");
    }

    #[test]
    fn autocrlf_files_stage_as_git_add_would() {
        let Some((dir, r)) = ten("git-write-autocrlf") else {
            return;
        };
        g(&dir.0, &["config", "core.autocrlf", "true"]);
        let old = index(&dir, "ten.txt");
        // The working copy as a Windows checkout has it.
        let disk = old.replace("line 4\n", "four\n").replace('\n', "\r\n");
        dir.write("ten.txt", &disk);
        let clean = r.clean("ten.txt", disk.into_bytes()).unwrap();
        assert_eq!(clean, old.replace("line 4\n", "four\n").into_bytes());
        let p = patch::build(
            "ten.txt",
            (old.as_bytes(), &clean),
            &Pick::all(),
            Apply::Forward,
            None,
        )
        .unwrap();
        r.apply(p, true).unwrap();
        assert_eq!(index(&dir, "ten.txt"), old.replace("line 4\n", "four\n"));
    }

    #[test]
    fn lines_of_a_staged_rename_unstage_under_its_new_name() {
        let Some((dir, r)) = ten("git-write-rename") else {
            return;
        };
        let p = dir.0.as_path();
        g(p, &["mv", "ten.txt", "moved.txt"]);
        let head = g(p, &["show", "HEAD:ten.txt"]);
        let changed = head.replace("line 7\n", "seven\n");
        dir.write("moved.txt", &changed);
        g(p, &["add", "moved.txt"]);
        let pick = Pick {
            old: vec![6..7],
            new: vec![6..7],
        };
        let staged = index(&dir, "moved.txt");
        let patch = patch::build(
            "moved.txt",
            (head.as_bytes(), staged.as_bytes()),
            &pick,
            Apply::Reverse,
            None,
        )
        .unwrap();
        r.apply(patch, true).unwrap();
        assert_eq!(index(&dir, "moved.txt"), head);
    }

    #[test]
    fn part_of_a_new_file_can_be_staged() {
        let Some((dir, r)) = ten("git-write-new") else {
            return;
        };
        dir.write("fresh.txt", "keep\nleave\n");
        let pick = Pick {
            old: Vec::new(),
            new: vec![0..1],
        };
        let p = patch::build(
            "fresh.txt",
            (b"", b"keep\nleave\n"),
            &pick,
            Apply::Forward,
            Some("100644"),
        )
        .unwrap();
        r.apply(p, true).unwrap();
        assert_eq!(index(&dir, "fresh.txt"), "keep\n");
    }

    #[test]
    fn whole_files_stage_unstage_and_restore() {
        let Some((dir, r)) = ten("git-write-files") else {
            return;
        };
        let p = dir.0.as_path();
        dir.write("ten.txt", "changed\n");
        dir.write("odd *name.txt", "x\n");
        r.stage(&["ten.txt", "odd *name.txt"]).unwrap();
        let s = r.status().unwrap();
        assert_eq!(s.entries.iter().filter(|e| e.is_staged()).count(), 2);
        r.unstage(&["ten.txt"], true).unwrap();
        assert_eq!(index(&dir, "ten.txt"), g(p, &["show", "HEAD:ten.txt"]));
        let kept = r.keep(b"changed\n".to_vec()).unwrap();
        r.restore(&["ten.txt"]).unwrap();
        assert_eq!(
            std::fs::read_to_string(p.join("ten.txt")).unwrap(),
            g(p, &["show", "HEAD:ten.txt"])
        );
        assert_eq!(r.blob(&kept).unwrap(), b"changed\n");
        r.stage_all().unwrap();
        r.unstage_all(true).unwrap();
        assert!(r.status().unwrap().entries.iter().all(|e| !e.is_staged()));
    }

    #[test]
    fn commit_and_amend_run_hooks_and_show_their_output() {
        let Some((dir, r)) = ten("git-write-commit") else {
            return;
        };
        let p = dir.0.as_path();
        let cancel = Arc::new(AtomicBool::new(false));
        dir.write("ten.txt", "committed\n");
        r.stage(&["ten.txt"]).unwrap();
        let out = r
            .make_commit("Subject\n\nBody line\n", false, &cancel)
            .unwrap();
        assert!(out.ok, "{}", out.text);
        assert_eq!(r.last_message().unwrap(), "Subject\n\nBody line");
        let out = r.make_commit("Subject, amended\n", true, &cancel).unwrap();
        assert!(out.ok, "{}", out.text);
        assert_eq!(r.last_message().unwrap(), "Subject, amended");
        assert!(r.author().unwrap().starts_with("Test <test@example.com>"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let hook = p.join(".git/hooks/pre-commit");
            std::fs::write(&hook, "#!/bin/sh\necho 'lint: 3 problems' >&2\nexit 1\n").unwrap();
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
            dir.write("ten.txt", "again\n");
            r.stage(&["ten.txt"]).unwrap();
            let out = r.make_commit("Refused", false, &cancel).unwrap();
            assert!(!out.ok);
            assert!(out.text.contains("lint: 3 problems"), "{}", out.text);
        }
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(r.make_commit("x", false, &cancel), Err(GitError::Cancelled));
    }
}
