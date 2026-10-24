//! What the git window changes in a repository (M28): staging, unstaging
//! and discarding, whole files or picked lines, and putting a discard
//! back. Each runs on a worker like the reads in `load.rs`, and reports
//! with [`GitEvent::Done`]; the window then reads the status again, so
//! the lists show what git now says rather than what we expect.
//!
//! **Discard keeps a way back.** Before anything in the working tree is
//! thrown away, its bytes go into the repository's object store
//! ([`Repo::keep`]): writing an object adds a file under `.git/objects`
//! and moves no branch, so nothing the user sees changes, and it is gone
//! at git's next garbage collection once old enough. The window remembers
//! the object's id, which is how Undo Discard puts the file back, for as
//! long as the window stays open (the owner's choice, `DESIGN.md` §9).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::load::{GitEvent, Send};
use crate::git::patch::{self, Apply, Pick};
use crate::git::{GitError, Repo};

/// How to put a discard back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discarded {
    /// Write these bytes (an object's id) back to the file.
    Bytes {
        /// The file, from the top folder.
        path: String,
        /// The object holding what it was.
        oid: String,
    },
    /// The discard brought back a deleted file; delete it again.
    Delete(String),
    /// An untracked folder went to the trash; ask the trash for it.
    Trashed(String),
}

impl Discarded {
    /// The file or folder it is about.
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Bytes { path, .. } | Self::Delete(path) | Self::Trashed(path) => path,
        }
    }
}

/// Part of a file's changes, as a patch to make on a worker.
#[derive(Debug, Clone)]
pub struct PatchOp {
    /// The file, from the top folder.
    pub path: String,
    /// The two sides the compare showed, as read.
    pub old: Arc<Vec<u8>>,
    /// The new side.
    pub new: Arc<Vec<u8>>,
    /// The new side is the working tree's file: convert it as `git add`
    /// would before comparing (line endings, clean filters).
    pub new_is_disk: bool,
    /// The lines to stage, unstage or discard.
    pub pick: Pick,
    /// Forward (staging) or undoing (unstaging, discarding).
    pub apply: Apply,
    /// Apply to the index (stage, unstage) or the working tree (discard).
    pub cached: bool,
    /// The file is not in the index yet: the patch creates it, with this
    /// mode.
    pub create: Option<&'static str>,
}

/// A whole file's unstaged changes to discard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscardFile {
    /// A tracked file: back to the index's version.
    Tracked(String),
    /// A tracked file deleted from the working tree: bring it back.
    Deleted(String),
    /// A file git does not track: to the trash.
    Untracked(String),
    /// A folder of untracked files: to the trash.
    UntrackedFolder(String),
}

/// One change to make.
#[derive(Debug, Clone)]
pub enum Op {
    /// `git add` these files.
    Stage(Vec<String>),
    /// Take these files out of the index; `true` when HEAD exists.
    Unstage(Vec<String>, bool),
    /// Stage everything.
    StageAll,
    /// Unstage everything; `true` when HEAD exists.
    UnstageAll(bool),
    /// Stage, unstage or discard part of a file.
    Patch(Box<PatchOp>),
    /// Discard a whole file's changes.
    Discard(DiscardFile),
    /// Put a discard back.
    Undo(Discarded),
}

/// What an [`Op`] did.
#[derive(Debug)]
pub struct Done {
    /// What to say when it worked: "Staged a.txt".
    pub said: String,
    /// How to undo a discard it made, or why it failed.
    pub result: Result<Option<Discarded>, String>,
    /// It was an Undo Discard.
    pub undid: bool,
}

/// Run `op` on a worker.
pub fn run(repo: Repo, op: Op, said: String, send: Send) {
    let spawned = std::thread::Builder::new()
        .name("nimbus-git-write".to_owned())
        .spawn(move || {
            let undid = matches!(op, Op::Undo(_));
            let result = perform(&repo, op);
            send(GitEvent::Done(Box::new(Done {
                said,
                result,
                undid,
            })));
        });
    if let Err(e) = spawned {
        log::error!("could not start a git worker: {e}");
    }
}

/// [`run`] on this thread.
///
/// # Errors
///
/// Why it failed, in plain words for the status bar.
pub fn perform(repo: &Repo, op: Op) -> Result<Option<Discarded>, String> {
    fn refs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }
    let words = |e: GitError| e.to_string();
    match op {
        Op::Stage(paths) => repo.stage(&refs(&paths)).map(|()| None).map_err(words),
        Op::Unstage(paths, head) => repo
            .unstage(&refs(&paths), head)
            .map(|()| None)
            .map_err(words),
        Op::StageAll => repo.stage_all().map(|()| None).map_err(words),
        Op::UnstageAll(head) => repo.unstage_all(head).map(|()| None).map_err(words),
        Op::Patch(p) => patch_op(repo, &p),
        Op::Discard(d) => discard(repo, d),
        Op::Undo(d) => undo(repo, &d).map(|()| None),
    }
}

fn patch_op(repo: &Repo, p: &PatchOp) -> Result<Option<Discarded>, String> {
    let words = |e: GitError| e.to_string();
    let new = if p.new_is_disk {
        repo.clean(&p.path, p.new.to_vec()).map_err(words)?
    } else {
        p.new.to_vec()
    };
    // The picked lines are numbered as the compare showed them; a filter
    // that changes more than line endings (Git LFS) breaks that.
    if patch::lines(&new).len() != patch::lines(&p.new).len() {
        return Err(
            "git converts this file with a filter, so its lines cannot be picked; use the whole file"
                .to_owned(),
        );
    }
    let patch = patch::build(&p.path, (&p.old, &new), &p.pick, p.apply, p.create)
        .map_err(|e| e.to_string())?;
    // A discard keeps the file's bytes first, as they are on disk now.
    let kept = if p.cached {
        None
    } else {
        let bytes = read(repo, &p.path)?;
        Some(Discarded::Bytes {
            path: p.path.clone(),
            oid: repo.keep(bytes).map_err(words)?,
        })
    };
    repo.apply(patch, p.cached).map_err(|e| match e {
        GitError::Failed(m) => {
            format!("git did not apply it, so nothing changed (the file changed meanwhile?): {m}")
        }
        e => e.to_string(),
    })?;
    Ok(kept)
}

fn read(repo: &Repo, path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(repo.top.join(path)).map_err(|e| format!("cannot read {path}: {e}"))
}

fn discard(repo: &Repo, d: DiscardFile) -> Result<Option<Discarded>, String> {
    let words = |e: GitError| e.to_string();
    match d {
        DiscardFile::Tracked(path) => {
            let oid = repo.keep(read(repo, &path)?).map_err(words)?;
            repo.restore(&[&path]).map_err(words)?;
            Ok(Some(Discarded::Bytes { path, oid }))
        }
        DiscardFile::Deleted(path) => {
            repo.restore(&[&path]).map_err(words)?;
            Ok(Some(Discarded::Delete(path)))
        }
        DiscardFile::Untracked(path) => {
            let oid = repo.keep(read(repo, &path)?).map_err(words)?;
            trash(&repo.top.join(&path))?;
            Ok(Some(Discarded::Bytes { path, oid }))
        }
        DiscardFile::UntrackedFolder(path) => {
            trash(&repo.top.join(path.trim_end_matches('/')))?;
            Ok(Some(Discarded::Trashed(path)))
        }
    }
}

fn trash(path: &Path) -> Result<(), String> {
    crate::tree_ops::run(&crate::tree_ops::Op::Trash(path.to_path_buf()))
}

fn undo(repo: &Repo, d: &Discarded) -> Result<(), String> {
    let full = |p: &str| -> PathBuf { repo.top.join(p.trim_end_matches('/')) };
    match d {
        Discarded::Bytes { path, oid } => {
            let bytes = repo.blob(oid).map_err(|e| e.to_string())?;
            let target = full(path);
            crate::state::write_atomic(&target, &bytes)
                .map_err(|e| format!("cannot write {path}: {e}"))
        }
        Discarded::Delete(path) => {
            std::fs::remove_file(full(path)).map_err(|e| format!("cannot delete {path}: {e}"))
        }
        Discarded::Trashed(path) => crate::tree_ops::run(&crate::tree_ops::Op::Restore(full(path))),
    }
}

/// What the message box shows around it: who a commit is by, and the
/// last commit's message for Amend.
#[derive(Debug, Clone)]
pub struct Prefill {
    /// `Name <email>` from git's config, or why there is none.
    pub author: Result<String, String>,
    /// The last commit's message; `None` before the first commit.
    pub last: Option<String>,
}

impl Default for Prefill {
    /// Before the worker answers: no author line yet (an empty error is
    /// drawn as nothing) and no last message.
    fn default() -> Self {
        Self {
            author: Err(String::new()),
            last: None,
        }
    }
}

/// Read [`Prefill`] on a worker.
pub fn prefill(repo: Repo, send: Send) {
    let spawned = std::thread::Builder::new()
        .name("nimbus-git-prefill".to_owned())
        .spawn(move || {
            let p = Prefill {
                author: repo.author().map_err(|e| e.to_string()),
                last: repo.last_message().ok(),
            };
            send(GitEvent::Prefill(Box::new(p)));
        });
    if let Err(e) = spawned {
        log::error!("could not start a git worker: {e}");
    }
}

/// Commit with `message` on a worker; setting `cancel` stops it.
pub fn commit(
    repo: Repo,
    message: String,
    amend: bool,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    send: Send,
) {
    let spawned = std::thread::Builder::new()
        .name("nimbus-git-commit".to_owned())
        .spawn(move || {
            let out = repo.make_commit(&message, amend, &cancel);
            send(GitEvent::Committed(Box::new(out)));
        });
    if let Err(e) = spawned {
        log::error!("could not start a git worker: {e}");
    }
}

/// The mode a new file gets in the index: executable or not, as on disk.
#[must_use]
pub fn mode_of(path: &Path) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0) {
            return "100755";
        }
    }
    let _ = path;
    "100644"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_repo;

    #[test]
    fn a_discard_comes_back_with_undo() {
        let Some(dir) = test_repo("git-ops-undo") else {
            return;
        };
        let repo = Repo::open(&dir.0).unwrap();
        let a = dir.0.join("a.txt");
        let before = std::fs::read(&a).unwrap();
        let undo = perform(&repo, Op::Discard(DiscardFile::Tracked("a.txt".into())))
            .unwrap()
            .unwrap();
        // Back to the index's version ("staged"), the unstaged edit gone.
        assert_eq!(std::fs::read(&a).unwrap(), b"staged\n");
        perform(&repo, Op::Undo(undo)).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), before);
        // An untracked file goes to the trash and comes back.
        let new = dir.0.join("new.txt");
        let undo = perform(&repo, Op::Discard(DiscardFile::Untracked("new.txt".into())));
        if let Ok(Some(undo)) = undo {
            assert!(!new.exists());
            perform(&repo, Op::Undo(undo)).unwrap();
            assert_eq!(std::fs::read(&new).unwrap(), b"new\n");
        }
    }

    #[test]
    fn picked_lines_discard_from_the_working_tree_only() {
        let Some(dir) = test_repo("git-ops-lines") else {
            return;
        };
        let repo = Repo::open(&dir.0).unwrap();
        dir.write("a.txt", "staged\nmore\n");
        let op = PatchOp {
            path: "a.txt".into(),
            old: Arc::new(b"staged\n".to_vec()),
            new: Arc::new(b"staged\nmore\n".to_vec()),
            new_is_disk: true,
            pick: Pick::all(),
            apply: Apply::Reverse,
            cached: false,
            create: None,
        };
        let undo = perform(&repo, Op::Patch(Box::new(op))).unwrap();
        assert_eq!(std::fs::read(dir.0.join("a.txt")).unwrap(), b"staged\n");
        perform(&repo, Op::Undo(undo.unwrap())).unwrap();
        assert_eq!(
            std::fs::read(dir.0.join("a.txt")).unwrap(),
            b"staged\nmore\n"
        );
    }

    /// Staging one hunk of a 10 MB file, start to finish on the worker:
    /// cleaning the working-tree text, the diff, the patch and `git
    /// apply`. The M28 budget is the lists updating within 100 ms after
    /// git returns; this times the whole operation, git included.
    #[test]
    #[ignore = "benchmark: writes a 10 MB file"]
    fn staging_bench() {
        use std::fmt::Write as _;
        let Some(dir) = test_repo("git-ops-bench") else {
            return;
        };
        let mut text = String::new();
        let mut n = 0;
        while text.len() < 10 << 20 {
            let _ = writeln!(text, "fn line_{n}() {{ let x = {n}; }}");
            n += 1;
        }
        dir.write("big.rs", &text);
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir.0)
                .args(args)
                .output()
                .unwrap()
        };
        git(&["add", "big.rs"]);
        git(&["commit", "-q", "-m", "big"]);
        let edited = text.replacen("let x = 100;", "let x = 101;", 1).replacen(
            "let x = 90000;",
            "let x = 90001;",
            1,
        );
        dir.write("big.rs", &edited);
        let repo = Repo::open(&dir.0).unwrap();
        let old = Arc::new(text.into_bytes());
        let new = Arc::new(edited.into_bytes());
        let first = patch::hunks(&old, &new);
        assert_eq!(first.len(), 2);
        let h = &first[0];
        let pick = Pick {
            old: vec![h.old.clone()],
            new: vec![h.new.clone()],
        };
        let op = PatchOp {
            path: "big.rs".into(),
            old,
            new,
            new_is_disk: true,
            pick,
            apply: Apply::Forward,
            cached: true,
            create: None,
        };
        let started = std::time::Instant::now();
        perform(&repo, Op::Patch(Box::new(op))).unwrap();
        let took = started.elapsed();
        eprintln!("staged one hunk of a 10 MB file in {took:?}");
        let diff = git(&["diff", "--cached", "--numstat"]);
        assert_eq!(String::from_utf8_lossy(&diff.stdout).trim(), "1\t1\tbig.rs");
        assert!(took.as_millis() < 2000, "{took:?}");
    }
}
