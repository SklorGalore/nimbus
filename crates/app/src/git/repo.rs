//! A repository the git window shows (M27): where it is, and the
//! read-only calls the window makes. Every call runs git with the
//! safety flags of [`git`](super::git) and passes arguments as separate
//! words, never through a shell; object ids from git's own output are
//! checked to be hex before they go back on a command line.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::parse::{self, Blob, Commit, FileChange, Ref, Status};
use super::stream::Stream;
use super::{GitError, MAX_OUTPUT, MAX_STDERR, TIMEOUT, canonical, git, run, run_input};

/// The oldest git the window works with: 2.25 (January 2020). Older ones
/// lack parts of the plumbing it parses (`--show-scope` for the filter
/// check, `--absolute-git-dir`'s companions), so the window says so
/// instead of misreading them.
pub const MIN_VERSION: (u32, u32) = (2, 25);

/// The most refs, status lines or changed files read in one call: 64 MB
/// of output.
const MAX_LISTING: usize = 64 << 20;

/// A repository: its top folder (the working tree) and its git folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The working tree's top folder, canonical.
    pub top: PathBuf,
    /// The git folder (`.git`, or elsewhere for a worktree), absolute.
    pub git_dir: PathBuf,
}

/// One entry of the stash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// The stash commit.
    pub oid: String,
    /// Its name, `stash@{0}` for the newest.
    pub name: String,
    /// What `git stash` said about it ("WIP on main: ...").
    pub subject: String,
}

/// Whether `s` looks like an object id: hex digits only, so it can never
/// be read as an option or a revision expression.
pub(super) fn is_oid(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(super) fn not_an_oid(s: &str) -> GitError {
    GitError::Failed(format!("not an object id: {s:?}"))
}

impl Repo {
    /// Find the repository holding `dir`, after checking that git is
    /// installed and new enough.
    ///
    /// # Errors
    ///
    /// [`GitError::NotFound`] without git, [`GitError::Failed`] for a git
    /// older than [`MIN_VERSION`], [`GitError::NotARepo`] outside a
    /// repository or in a bare one, and the errors of `run`.
    pub fn open(dir: &Path) -> Result<Self, GitError> {
        check_version()?;
        let dir = canonical(dir)?;
        let dir = if dir.is_dir() {
            dir
        } else {
            dir.parent().map(Path::to_path_buf).unwrap_or(dir)
        };
        let mut cmd = git();
        cmd.arg("-C")
            .arg(&dir)
            .args(["rev-parse", "--show-toplevel", "--absolute-git-dir"]);
        let out = match run(cmd, TIMEOUT, MAX_STDERR) {
            Ok(out) => out,
            Err(GitError::Failed(m)) if m.contains("not a git repository") => {
                return Err(GitError::NotARepo);
            }
            Err(GitError::Failed(m)) if m.contains("must be run in a work tree") => {
                return Err(GitError::NotARepo);
            }
            Err(e) => return Err(e),
        };
        let text = String::from_utf8_lossy(&out);
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        let (Some(top), Some(git_dir)) = (lines.next(), lines.next()) else {
            return Err(GitError::NotARepo);
        };
        Ok(Self {
            top: canonical(Path::new(top))?,
            git_dir: PathBuf::from(git_dir),
        })
    }

    /// The repository's name: its top folder's.
    #[must_use]
    pub fn name(&self) -> String {
        self.top.file_name().map_or_else(
            || self.top.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// A git command run in the top folder.
    pub(super) fn cmd(&self) -> Command {
        let mut cmd = git();
        cmd.arg("-C").arg(&self.top);
        cmd
    }

    /// The working tree's changes and the branch. Untracked folders are
    /// listed as one entry each, as `git status` does by default, so a
    /// folder of thousands of new files is one line.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn status(&self) -> Result<Status, GitError> {
        let mut cmd = self.cmd();
        cmd.args([
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ]);
        run(cmd, TIMEOUT, MAX_LISTING).map(|out| parse::status(&out))
    }

    /// Every branch, remote-tracking branch, tag and the stash.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn refs(&self) -> Result<Vec<Ref>, GitError> {
        let mut cmd = self.cmd();
        cmd.args(["for-each-ref", parse::REF_FORMAT]);
        run(cmd, TIMEOUT, MAX_LISTING).map(|out| parse::refs(&out))
    }

    /// The stash's entries, newest first; empty when there is no stash.
    ///
    /// # Errors
    ///
    /// The errors of `run`.
    pub fn stashes(&self) -> Result<Vec<Stash>, GitError> {
        let mut cmd = self.cmd();
        // The stash is the reflog of refs/stash; walk it.
        cmd.args([
            "-c",
            "log.showSignature=false",
            "log",
            "--walk-reflogs",
            "-z",
            "--format=%H%x1f%gd%x1f%gs",
            "refs/stash",
            "--",
        ]);
        let out = run(cmd, TIMEOUT, MAX_LISTING)?;
        Ok(out
            .split(|&b| b == 0)
            .filter_map(|rec| {
                let text = String::from_utf8_lossy(rec);
                let mut parts = text.trim_start_matches('\n').splitn(3, '\x1f');
                let oid = parts.next()?.to_owned();
                let name = parts.next()?.to_owned();
                let subject = parts.next().unwrap_or("").to_owned();
                is_oid(&oid).then_some(Stash { oid, name, subject })
            })
            .collect())
    }

    /// A commit's parents, people, dates and whole message, read from the
    /// commit object itself (`cat-file commit`).
    ///
    /// # Errors
    ///
    /// [`GitError::Failed`] for an `oid` that is not hex, and the errors of
    /// `run`.
    pub fn commit(&self, oid: &str) -> Result<Commit, GitError> {
        if !is_oid(oid) {
            return Err(not_an_oid(oid));
        }
        let mut cmd = self.cmd();
        cmd.args(["cat-file", "commit", oid]);
        run(cmd, TIMEOUT, MAX_LISTING).map(|out| parse::commit(&out))
    }

    /// The files `oid` changed against `parent`, or against nothing for a
    /// root commit (`parent` `None`), with renames found (`-M`).
    ///
    /// # Errors
    ///
    /// [`GitError::Failed`] for an id that is not hex, and the errors of
    /// `run`.
    pub fn changes(&self, oid: &str, parent: Option<&str>) -> Result<Vec<FileChange>, GitError> {
        if !is_oid(oid) {
            return Err(not_an_oid(oid));
        }
        let mut cmd = self.cmd();
        cmd.args([
            "diff-tree",
            "-r",
            "-z",
            "--raw",
            "--no-abbrev",
            "-M",
            "--no-commit-id",
        ]);
        match parent {
            Some(p) if is_oid(p) => {
                cmd.arg(p);
            }
            Some(p) => return Err(not_an_oid(p)),
            None => {
                cmd.arg("--root");
            }
        }
        cmd.arg(oid);
        run(cmd, TIMEOUT, MAX_LISTING).map(|out| parse::diff_tree(&out))
    }

    /// The contents of several blobs through one `cat-file --batch`
    /// process: one process however many objects, which matters on
    /// Windows, where starting a process is slow. Ids that are not hex
    /// read as missing.
    ///
    /// # Errors
    ///
    /// The errors of `run`, including [`GitError::TooLarge`] past 256 MB.
    pub fn blobs(&self, oids: &[&str]) -> Result<Vec<Blob>, GitError> {
        let mut input = Vec::new();
        for oid in oids {
            // A request that is not an id gets "missing" back, keeping the
            // answers in step with the questions.
            input.extend_from_slice(if is_oid(oid) { oid.as_bytes() } else { b"0" });
            input.push(b'\n');
        }
        let mut cmd = self.cmd();
        cmd.args(["cat-file", "--batch"]);
        let out = run_input(cmd, Some(input), TIMEOUT, MAX_OUTPUT)?;
        Ok(parse::batch(&out, oids.len()))
    }

    /// Stream the whole history: every commit reachable from a branch, a
    /// remote-tracking branch, a tag or HEAD, newest first, in *date
    /// order* (never a parent before all its children, which the graph
    /// needs), one [`parse::LOG_FORMAT`] record per commit, NUL-separated.
    /// The stash is left out: its commits are not history.
    ///
    /// `log.showSignature` is forced off: with it on, a repository's
    /// config would make `git log` run a signature program for every
    /// commit (M26: no program from the repository runs).
    ///
    /// # Errors
    ///
    /// `preview`: only the newest this many commits of HEAD, for a first
    /// screen while the whole history loads. git can stream a date-order
    /// walk from one tip at once, but with several tips it first walks
    /// down to the oldest one (an old tag or stale branch), which on a
    /// million commits takes seconds before the first line.
    ///
    /// # Errors
    ///
    /// The errors of [`Stream::start`].
    pub fn log(
        &self,
        head: bool,
        preview: Option<u32>,
        sink: impl FnMut(&[u8]) -> bool + Send + 'static,
        done: impl FnOnce(Result<(), GitError>) + Send + 'static,
    ) -> Result<Stream, GitError> {
        let mut cmd = self.cmd();
        cmd.args([
            "-c",
            "log.showSignature=false",
            "log",
            "--date-order",
            "-z",
            "--no-color",
            parse::LOG_FORMAT,
        ]);
        if let Some(n) = preview {
            cmd.arg(format!("--max-count={n}"));
        } else {
            cmd.args(["--branches", "--remotes", "--tags"]);
        }
        // HEAD on its own covers a detached HEAD; before the first commit
        // it names nothing and git would fail.
        if head {
            cmd.arg("HEAD");
        }
        cmd.arg("--");
        Stream::start(cmd, sink, done)
    }
}

/// Check once that git runs and is at least [`MIN_VERSION`].
fn check_version() -> Result<(), GitError> {
    let mut cmd = git();
    cmd.arg("--version");
    let out = run(cmd, TIMEOUT, MAX_STDERR)?;
    let text = String::from_utf8_lossy(&out);
    match parse::version(&text) {
        Some(v) if v >= MIN_VERSION => Ok(()),
        Some((major, minor)) => Err(GitError::Failed(format!(
            "the git window needs git {}.{} or later; this is git {major}.{minor}",
            MIN_VERSION.0, MIN_VERSION.1
        ))),
        // A build that prints something else: let it try.
        None => Ok(()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use super::super::parse::{Kind, LogRecord, Records, RefKind};

    /// A temporary folder, removed when dropped.
    pub(crate) struct Dir(pub PathBuf);

    impl Dir {
        pub(crate) fn new(name: &str) -> Self {
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

        pub(crate) fn write(&self, rel: &str, text: &str) {
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

    fn g(dir: &Path, args: &[&str]) -> String {
        let mut cmd = git();
        cmd.arg("-C").arg(dir).args(args);
        String::from_utf8(run(cmd, TIMEOUT, MAX_OUTPUT).unwrap()).unwrap()
    }

    /// A repository with history: `main` has three commits, `side` forks
    /// from the first and is merged back, a tag on the second, and the
    /// working tree has a staged, an unstaged and an untracked change.
    pub(crate) fn repo(name: &str) -> Option<Dir> {
        if !super::super::available() {
            return None;
        }
        let dir = Dir::new(name);
        let p = dir.0.as_path();
        g(p, &["init", "-q", "-b", "main"]);
        for (k, v) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
        ] {
            g(p, &["config", k, v]);
        }
        dir.write("a.txt", "one\n");
        dir.write("b.txt", "bee\n");
        g(p, &["add", "."]);
        g(p, &["commit", "-q", "-m", "first"]);
        g(p, &["branch", "side"]);
        dir.write("a.txt", "two\n");
        g(p, &["commit", "-q", "-am", "second"]);
        g(p, &["tag", "-a", "v1", "-m", "tag one"]);
        g(p, &["checkout", "-q", "side"]);
        g(p, &["mv", "b.txt", "c.txt"]);
        g(p, &["commit", "-q", "-m", "rename on side"]);
        g(p, &["checkout", "-q", "main"]);
        g(p, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        dir.write("a.txt", "staged\n");
        g(p, &["add", "a.txt"]);
        dir.write("a.txt", "unstaged\n");
        dir.write("new.txt", "new\n");
        Some(dir)
    }

    #[test]
    fn open_finds_the_top_from_a_subfolder_and_refuses_elsewhere() {
        let Some(dir) = repo("git-open") else { return };
        std::fs::create_dir_all(dir.0.join("deep/er")).unwrap();
        let r = Repo::open(&dir.0.join("deep/er")).unwrap();
        assert_eq!(r.top, canonical(&dir.0).unwrap());
        assert!(r.git_dir.ends_with(".git"));
        assert_eq!(r.name(), dir.0.file_name().unwrap().to_string_lossy());
        let outside = Dir::new("git-open-outside");
        assert_eq!(Repo::open(&outside.0), Err(GitError::NotARepo));
    }

    #[test]
    fn status_sees_staged_unstaged_and_untracked() {
        let Some(dir) = repo("git-status") else {
            return;
        };
        let s = Repo::open(&dir.0).unwrap().status().unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
        let a = s.entries.iter().find(|e| e.path == "a.txt").unwrap();
        assert_eq!((a.staged, a.unstaged), (Kind::Modified, Kind::Modified));
        let n = s.entries.iter().find(|e| e.path == "new.txt").unwrap();
        assert_eq!(n.unstaged, Kind::Untracked);
    }

    #[test]
    fn refs_list_branches_and_peel_tags() {
        let Some(dir) = repo("git-refs") else { return };
        let r = Repo::open(&dir.0).unwrap();
        let refs = r.refs().unwrap();
        let names: Vec<_> = refs.iter().map(|r| (r.kind, r.name.as_str())).collect();
        assert_eq!(
            names,
            [
                (RefKind::Branch, "main"),
                (RefKind::Branch, "side"),
                (RefKind::Tag, "v1")
            ]
        );
        // The tag names its commit, not the tag object.
        let tagged = g(&dir.0, &["rev-parse", "v1^{commit}"]);
        assert_eq!(refs[2].target, tagged.trim());
        assert_eq!(r.commit(&refs[2].target).unwrap().message, "second\n");
    }

    #[test]
    fn a_merge_commit_has_two_parents_and_changes_against_either() {
        let Some(dir) = repo("git-merge") else { return };
        let r = Repo::open(&dir.0).unwrap();
        let head = g(&dir.0, &["rev-parse", "HEAD"]);
        let head = head.trim();
        let c = r.commit(head).unwrap();
        assert_eq!(c.parents.len(), 2);
        assert_eq!(c.message, "merge side\n");
        assert_eq!(c.author.name, "Test");
        // Against the first parent the merge brought the rename in.
        let first = r.changes(head, Some(&c.parents[0])).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, Kind::Renamed);
        assert_eq!(
            (first[0].old_path.as_deref(), first[0].path.as_str()),
            (Some("b.txt"), "c.txt")
        );
        // Against the second, it brought main's edit of a.txt.
        let second = r.changes(head, Some(&c.parents[1])).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].path, "a.txt");
        let blobs = r
            .blobs(&[
                second[0].old_oid.as_deref().unwrap(),
                second[0].new_oid.as_deref().unwrap(),
                "deadbeef",
                "--not-an-id",
            ])
            .unwrap();
        assert_eq!(blobs[0].as_deref(), Some(&b"one\n"[..]));
        assert_eq!(blobs[1].as_deref(), Some(&b"two\n"[..]));
        assert_eq!((blobs[2].clone(), blobs[3].clone()), (None, None));
    }

    #[test]
    fn the_root_commit_adds_its_files() {
        let Some(dir) = repo("git-root") else { return };
        let r = Repo::open(&dir.0).unwrap();
        let root = g(&dir.0, &["rev-list", "--max-parents=0", "HEAD"]);
        let files = r.changes(root.trim(), None).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.kind == Kind::Added));
    }

    #[test]
    fn ids_that_are_not_hex_never_reach_git() {
        let Some(dir) = repo("git-ids") else { return };
        let r = Repo::open(&dir.0).unwrap();
        assert!(matches!(r.commit("--output=x"), Err(GitError::Failed(_))));
        assert!(matches!(r.changes("HEAD", None), Err(GitError::Failed(_))));
        assert!(matches!(
            r.changes("abcd", Some("-p")),
            Err(GitError::Failed(_))
        ));
    }

    #[test]
    fn the_log_streams_every_commit_children_first() {
        let Some(dir) = repo("git-log") else { return };
        let r = Repo::open(&dir.0).unwrap();
        g(&dir.0, &["stash", "-q"]);
        let records = Arc::new(Mutex::new(Records::default()));
        let got = Arc::new(Mutex::new(Vec::<String>::new()));
        let (sink_records, sink_got) = (Arc::clone(&records), Arc::clone(&got));
        let (tx, rx) = mpsc::channel();
        let _stream = r
            .log(
                true,
                None,
                move |chunk| {
                    sink_records.lock().unwrap().feed(chunk, |rec| {
                        let text = String::from_utf8_lossy(rec);
                        let rec = LogRecord::parse(&text).unwrap();
                        sink_got.lock().unwrap().push(rec.subject.to_owned());
                    });
                    true
                },
                move |r| tx.send(r).unwrap(),
            )
            .unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), Ok(()));
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 4, "the stash's commits are not history: {got:?}");
        assert_eq!(got[0], "merge side");
        assert_eq!(got.last().map(String::as_str), Some("first"));
        // The stash shows in its own list.
        let stashes = r.stashes().unwrap();
        assert_eq!(stashes.len(), 1);
        assert_eq!(stashes[0].name, "stash@{0}");
        assert!(stashes[0].subject.contains("main"), "{:?}", stashes[0]);
    }

    #[test]
    fn a_repository_with_no_commits_yet() {
        if !super::super::available() {
            return;
        }
        let dir = Dir::new("git-empty");
        g(&dir.0, &["init", "-q"]);
        let r = Repo::open(&dir.0).unwrap();
        let s = r.status().unwrap();
        assert_eq!(s.head, None);
        assert_eq!(r.refs().unwrap().len(), 0);
        assert!(r.stashes().is_err() || r.stashes().unwrap().is_empty());
    }
}
