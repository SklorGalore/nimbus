//! The git window's workers (M27): every git call runs on a thread of its
//! own and reports back as a [`GitEvent`], so the event loop never waits
//! on git (the plan's budget: no git call on the event loop).
//!
//! Results carry the *token* of the request that started them. A newer
//! request (another commit clicked) bumps the token, and a result with an
//! old one is dropped on arrival: the worker is not stopped, only ignored,
//! since a short git call costs little to finish.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use nimbus_syntax::state::JobResult;

use super::history::History;
use crate::compare::DiffResult;
use crate::git::parse::{LogRecord, Records};
use crate::git::{Commit, FileChange, GitError, Ref, Repo, Stash, Status, Stream};

/// Something a worker finished, for the event loop.
#[derive(Debug)]
pub enum GitEvent {
    /// The repository was found, or why not.
    Opened(Result<Repo, GitError>),
    /// A refresh of the status, refs and stash finished.
    Refreshed(Box<Refresh>),
    /// The history load with this generation has new rows, or ended.
    History(u64),
    /// A commit's details arrived.
    Details(Box<Details>),
    /// The two sides of a file's diff were read.
    Texts(Box<Texts>),
    /// A compare's diff finished.
    Diff(DiffResult),
    /// A parse for a compare side finished.
    Syntax(JobResult),
    /// A fallback font arrived: draw again.
    FontLoaded,
    /// Something under the repository changed on disk.
    Disk,
    /// Something to say in the status bar, and whether it is an error.
    Say(String, bool),
    /// A change to the repository finished (M28).
    Done(Box<super::ops::Done>),
    /// A commit finished, was refused, or was stopped.
    Committed(Box<Result<crate::git::CommitOutput, GitError>>),
    /// Who commits are by, and the last commit's message (for Amend).
    Prefill(Box<super::ops::Prefill>),
    /// The palette's matcher has news: draw again.
    Redraw,
    /// The config's editor and interface fonts, looked up on a worker.
    Fonts(
        Box<(
            Option<nimbus_render::EditorFont>,
            Option<nimbus_render::EditorFont>,
        )>,
    ),
}

/// Where workers send their results: the event loop's proxy in a window,
/// a channel in a headless render. `false` once nobody listens.
pub type Send = Arc<dyn Fn(GitEvent) -> bool + std::marker::Send + Sync>;

/// The status, refs and stash, read together.
#[derive(Debug)]
pub struct Refresh {
    /// `git status`.
    pub status: Result<Status, GitError>,
    /// Branches, tags and remote-tracking branches.
    pub refs: Result<Vec<Ref>, GitError>,
    /// The stash's entries.
    pub stashes: Vec<Stash>,
}

/// A commit's object and its changed files.
#[derive(Debug)]
pub struct Details {
    /// The request this answers.
    pub token: u64,
    /// Its parsed object.
    pub commit: Result<Commit, GitError>,
    /// The files it changed against the chosen parent.
    pub files: Result<Vec<FileChange>, GitError>,
    /// Which parent the files are compared against (0 is the first).
    pub parent: usize,
}

/// One side of a file's diff.
#[derive(Debug)]
pub enum Side {
    /// The file's bytes.
    Text(Vec<u8>),
    /// The file does not exist on this side (added or deleted).
    Absent,
    /// It could not be read; why.
    Failed(String),
}

/// Both sides of a file's diff.
#[derive(Debug)]
pub struct Texts {
    /// The request this answers.
    pub token: u64,
    /// The file's path in the repository, for language detection and the
    /// labels.
    pub path: String,
    /// The old side.
    pub left: Side,
    /// The new side.
    pub right: Side,
}

/// Where a side of a diff comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A blob in the object store.
    Blob(String),
    /// The file in the working tree, at this path from the top folder.
    Disk(String),
    /// Nothing: the file is new or gone on this side.
    Absent,
}

/// Run `work` on a named thread; log when one cannot start.
fn spawn(name: &str, work: impl FnOnce() + std::marker::Send + 'static) {
    if let Err(e) = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
    {
        log::error!("could not start {name}: {e}");
    }
}

/// Find the repository holding `dir`.
pub fn open(dir: PathBuf, send: Send) {
    spawn("nimbus-git-open", move || {
        send(GitEvent::Opened(Repo::open(&dir)));
    });
}

/// Read the status, refs and stash.
pub fn refresh(repo: Repo, send: Send) {
    spawn("nimbus-git-status", move || {
        send(GitEvent::Refreshed(Box::new(read_refresh(&repo))));
    });
}

/// [`refresh`] on this thread, for headless renders.
pub fn read_refresh(repo: &Repo) -> Refresh {
    let refs = repo.refs();
    let has_stash = refs
        .as_ref()
        .is_ok_and(|r| r.iter().any(|r| r.kind == crate::git::RefKind::Stash));
    let stashes = if has_stash {
        repo.stashes().unwrap_or_else(|e| {
            log::warn!("git window: cannot list the stash: {e}");
            Vec::new()
        })
    } else {
        Vec::new()
    };
    Refresh {
        status: repo.status(),
        refs,
        stashes,
    }
}

/// A history being loaded: the rows so far, shared with the worker that
/// appends them, and the running `git log`. Dropping it stops the load.
#[derive(Debug)]
pub struct Loading {
    /// The rows so far.
    pub history: Arc<Mutex<History>>,
    /// Set by the worker when it has news, cleared by the window when it
    /// takes it, so a fast stream sends one event per frame, not one per
    /// chunk.
    pub news: Arc<AtomicBool>,
    /// Which load this is; events of older ones are ignored.
    pub generation: u64,
    /// Only HEAD's newest commits, shown until the whole history catches
    /// up (see [`Repo::log`]).
    pub preview: bool,
    _stream: Stream,
}

/// Start streaming the history into a new [`History`]. `head`: include
/// HEAD (false before the first commit, when it names nothing);
/// `preview`: only HEAD's newest this many commits.
///
/// # Errors
///
/// When git cannot be started.
pub fn history(
    repo: &Repo,
    (head, preview): (bool, Option<u32>),
    generation: u64,
    send: &Send,
) -> Result<Loading, GitError> {
    let history = Arc::new(Mutex::new(History::default()));
    let news = Arc::new(AtomicBool::new(false));
    let (rows, flag) = (Arc::clone(&history), Arc::clone(&news));
    let (sink_send, done_send) = (Arc::clone(send), Arc::clone(send));
    let done_rows = Arc::clone(&history);
    let mut records = Records::default();
    let tell = move || {
        if !flag.swap(true, Ordering::AcqRel) {
            sink_send(GitEvent::History(generation));
        }
    };
    let stream = repo.log(
        head,
        preview,
        move |chunk| {
            {
                let mut h = rows.lock().unwrap_or_else(PoisonError::into_inner);
                records.feed(chunk, |rec| push(&mut h, rec));
            }
            tell();
            true
        },
        move |outcome| {
            let mut h = done_rows.lock().unwrap_or_else(PoisonError::into_inner);
            h.finish(outcome.map_err(|e| e.to_string()));
            drop(h);
            done_send(GitEvent::History(generation));
        },
    )?;
    Ok(Loading {
        history,
        news,
        generation,
        preview: preview.is_some(),
        _stream: stream,
    })
}

fn push(h: &mut History, rec: &[u8]) {
    let text = String::from_utf8_lossy(rec);
    if let Some(r) = LogRecord::parse(&text) {
        h.push(&r);
    }
}

/// Read a commit's object and its files against parent number `parent`.
pub fn details(repo: Repo, token: u64, oid: String, parent: usize, send: Send) {
    spawn("nimbus-git-details", move || {
        send(GitEvent::Details(Box::new(read_details(
            &repo, token, &oid, parent,
        ))));
    });
}

/// [`details`] on this thread.
pub fn read_details(repo: &Repo, token: u64, oid: &str, parent: usize) -> Details {
    let commit = repo.commit(oid);
    let files = match &commit {
        Ok(c) => {
            let p = c.parents.get(parent).or_else(|| c.parents.first());
            repo.changes(oid, p.map(String::as_str))
        }
        Err(e) => Err(e.clone()),
    };
    Details {
        token,
        commit,
        files,
        parent,
    }
}

/// Read both sides of a file's diff.
pub fn texts(repo: Repo, token: u64, path: String, sides: (Source, Source), send: Send) {
    spawn("nimbus-git-blobs", move || {
        send(GitEvent::Texts(Box::new(read_texts(
            &repo, token, path, sides,
        ))));
    });
}

/// [`texts`] on this thread. Blobs on both sides are read through one
/// `cat-file --batch`.
pub fn read_texts(repo: &Repo, token: u64, path: String, (left, right): (Source, Source)) -> Texts {
    let blobs: Vec<&str> = [&left, &right]
        .into_iter()
        .filter_map(|s| match s {
            Source::Blob(oid) => Some(oid.as_str()),
            _ => None,
        })
        .collect();
    let mut read = if blobs.is_empty() {
        Ok(Vec::new())
    } else {
        repo.blobs(&blobs)
    }
    .map(Vec::into_iter);
    let mut side = |s: &Source| match s {
        Source::Absent => Side::Absent,
        Source::Disk(rel) => match std::fs::read(repo.top.join(rel)) {
            Ok(bytes) => Side::Text(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Side::Absent,
            Err(e) => Side::Failed(e.to_string()),
        },
        Source::Blob(_) => match &mut read {
            Ok(it) => match it.next() {
                Some(Some(bytes)) => Side::Text(bytes),
                _ => Side::Failed("git does not have this object".to_owned()),
            },
            Err(e) => Side::Failed(e.to_string()),
        },
    };
    let left = side(&left);
    let right = side(&right);
    Texts {
        token,
        path,
        left,
        right,
    }
}

/// Hand `path` to the Nimbus editor that is running, through M25's
/// single-instance socket, or start one with it when none is.
pub fn open_in_editor(path: PathBuf, send: Send) {
    spawn("nimbus-git-open-editor", move || {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let request = crate::instance::Request {
            paths: vec![path.clone()],
            wait: false,
        };
        if let Some(dir) = crate::instance::dir()
            && let crate::instance::Knock::Answered(_) =
                crate::instance::knock(&dir, Some(&request))
        {
            send(GitEvent::Say(format!("Opened {name} in Nimbus"), false));
            return;
        }
        match start_nimbus(&[path.as_os_str()]) {
            Ok(()) => send(GitEvent::Say(
                format!("Opening {name} in a new Nimbus"),
                false,
            )),
            Err(e) => send(GitEvent::Say(format!("Cannot start Nimbus: {e}"), true)),
        };
    });
}

/// Start this program again with `args`, in the background: its output
/// goes nowhere and it outlives this window.
///
/// # Errors
///
/// When this program cannot be found or started.
pub fn start_nimbus(args: &[&std::ffi::OsStr]) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::detach(&mut command);
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// M27's history budget on a big repository (`bench/gen.ps1 git` makes
    /// one with a million commits): the first screen of rows, the whole
    /// load, and bytes a commit. Run with
    /// `NIMBUS_GIT_REPO=$PWD/bench/out/git-1m cargo test --release -p nimbus
    /// --bin nimbus -- --ignored --nocapture git_history_bench`.
    #[test]
    #[ignore = "needs NIMBUS_GIT_REPO, a large repository"]
    fn git_history_bench() {
        let Some(dir) = std::env::var_os("NIMBUS_GIT_REPO") else {
            eprintln!("set NIMBUS_GIT_REPO");
            return;
        };
        let started = Instant::now();
        let repo = Repo::open(std::path::Path::new(&dir)).unwrap();
        let opened = started.elapsed();
        let (tx, rx) = std::sync::mpsc::channel::<GitEvent>();
        let tx = Mutex::new(tx);
        let send: Send = Arc::new(move |e| tx.lock().is_ok_and(|t| t.send(e).is_ok()));
        // What the window does: a preview of HEAD for the first screen,
        // and the whole history behind it.
        let begun = Instant::now();
        let preview = history(&repo, (true, Some(2_000)), 0, &send).unwrap();
        loop {
            let _ = rx.recv_timeout(Duration::from_millis(1));
            if preview.history.lock().unwrap().len() >= 60 {
                break;
            }
        }
        let preview_screen = begun.elapsed();
        drop(preview);
        let begun = Instant::now();
        let loading = history(&repo, (true, None), 1, &send).unwrap();
        let mut first_screen = None;
        loop {
            let _ = rx.recv_timeout(Duration::from_millis(5));
            loading.news.store(false, Ordering::Release);
            let h = loading.history.lock().unwrap();
            if first_screen.is_none() && h.len() >= 60 {
                first_screen = Some(begun.elapsed());
            }
            if h.finished.is_some() {
                break;
            }
        }
        let total = begun.elapsed();
        let h = loading.history.lock().unwrap();
        assert_eq!(h.finished, Some(Ok(())));
        eprintln!(
            "open {opened:?}, first screen (preview) {preview_screen:?}, whole history's first 60 rows {:?}, all {} commits {total:?}, {} bytes a commit ({} with subjects), {} lanes",
            first_screen.unwrap_or(total),
            h.len(),
            (h.bytes() - h.subject_bytes()) / h.len().max(1),
            h.bytes() / h.len().max(1),
            h.lanes_used
        );
    }
}
