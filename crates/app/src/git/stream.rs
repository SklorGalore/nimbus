//! Streaming a git command's output as it arrives (M27).
//!
//! [`run`](super::run) waits for git to finish and returns everything it
//! printed. That suits short calls, but not `git log` on a large
//! repository: its output is hundreds of megabytes and takes seconds, and
//! the history should show its first screen long before the end. So a
//! [`Stream`] starts the child and reads its standard output on a worker
//! thread in chunks, handing each chunk to a callback the moment it
//! arrives: the caller parses records out of it as they come.
//!
//! **Cancelling by dropping.** The handle owns the child. Dropping it (the
//! window closed, or a newer load replaced this one) kills git; the reader
//! then sees the pipe close and stops without reporting. No timeout
//! applies: a long history legitimately takes a while, and the handle
//! decides when it is no longer wanted.

use std::io::{ErrorKind, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use super::{GitError, MAX_STDERR, collect, drain, first_lines};

/// How much is read from the pipe at a time.
const CHUNK: usize = 64 << 10;

/// A running git command whose output is being read on a worker. Dropping
/// it kills the command.
#[derive(Debug)]
pub struct Stream {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
}

impl Stream {
    /// Start `cmd` and read its output on a new thread. `sink` gets each
    /// chunk in order and returns `false` to stop early (git is then
    /// killed); `done` is called once at the end, unless the stream was
    /// dropped first, with `Ok(())` when git succeeded.
    ///
    /// # Errors
    ///
    /// [`GitError::NotFound`] when git cannot be started, and
    /// [`GitError::Io`] when the worker thread cannot.
    pub fn start(
        mut cmd: Command,
        mut sink: impl FnMut(&[u8]) -> bool + Send + 'static,
        done: impl FnOnce(Result<(), GitError>) + Send + 'static,
    ) -> Result<Self, GitError> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            ErrorKind::NotFound => GitError::NotFound,
            _ => GitError::Io(e.to_string()),
        })?;
        let stdout = child.stdout.take();
        let stderr = drain(child.stderr.take(), MAX_STDERR, None);
        let child = Arc::new(Mutex::new(Some(child)));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (owned, flag) = (Arc::clone(&child), Arc::clone(&cancelled));
        let spawned = thread::Builder::new()
            .name("nimbus-git-stream".to_owned())
            .spawn(move || {
                let read = read_all(stdout, &mut sink, &flag);
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                let outcome = finish(&owned, read, &stderr);
                if !flag.load(Ordering::Relaxed) {
                    done(outcome);
                }
            });
        let stream = Self { child, cancelled };
        match spawned {
            Ok(_) => Ok(stream),
            // Dropping `stream` kills the child that nobody would read.
            Err(e) => Err(GitError::Io(e.to_string())),
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        kill(&self.child);
    }
}

fn kill(child: &Mutex<Option<Child>>) {
    let mut guard = child.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(mut c) = guard.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Feed `pipe` to `sink` until it ends, `sink` says stop, or the stream
/// is cancelled. `Ok(true)` when the whole output was read.
fn read_all<R: Read>(
    pipe: Option<R>,
    sink: &mut impl FnMut(&[u8]) -> bool,
    cancelled: &AtomicBool,
) -> std::io::Result<bool> {
    let Some(mut pipe) = pipe else {
        return Ok(true);
    };
    let mut buf = vec![0; CHUNK];
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let n = match pipe.read(&mut buf) {
            Ok(0) => return Ok(true),
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if !sink(buf.get(..n).unwrap_or_default()) {
            return Ok(false);
        }
    }
}

/// Reap the child and say how the command ended.
fn finish(
    child: &Mutex<Option<Child>>,
    read: std::io::Result<bool>,
    stderr: &std::sync::mpsc::Receiver<std::io::Result<super::Drained>>,
) -> Result<(), GitError> {
    let whole = read.map_err(|e| GitError::Io(e.to_string()))?;
    let taken = child.lock().unwrap_or_else(PoisonError::into_inner).take();
    let Some(mut c) = taken else {
        return Ok(());
    };
    if !whole {
        // The sink asked to stop: not an error.
        let _ = c.kill();
        let _ = c.wait();
        return Ok(());
    }
    let status = c.wait().map_err(|e| GitError::Io(e.to_string()))?;
    if status.success() {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + super::TIMEOUT;
    let err = collect(stderr, deadline)?;
    Err(GitError::Failed(first_lines(&err.bytes, status)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn available() -> bool {
        super::super::available()
    }

    #[test]
    fn chunks_arrive_in_order_and_done_reports_success() {
        if !available() {
            return;
        }
        let mut cmd = super::super::git();
        cmd.arg("--version");
        let (tx, rx) = mpsc::channel();
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink_got = Arc::clone(&got);
        let _s = Stream::start(
            cmd,
            move |c| {
                sink_got.lock().unwrap().extend_from_slice(c);
                true
            },
            move |r| tx.send(r).unwrap(),
        )
        .unwrap();
        let r = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(r, Ok(()));
        assert!(got.lock().unwrap().starts_with(b"git version"));
    }

    #[test]
    fn a_failing_command_reports_gits_message() {
        if !available() {
            return;
        }
        let mut cmd = super::super::git();
        cmd.args(["no-such-command-at-all"]);
        let (tx, rx) = mpsc::channel();
        let _s = Stream::start(cmd, |_| true, move |r| tx.send(r).unwrap()).unwrap();
        let r = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(matches!(r, Err(GitError::Failed(_))), "{r:?}");
    }

    #[test]
    fn a_sink_that_stops_early_ends_the_stream_cleanly() {
        if !available() {
            return;
        }
        let mut cmd = super::super::git();
        cmd.arg("--version");
        let (tx, rx) = mpsc::channel();
        let _s = Stream::start(cmd, |_| false, move |r| tx.send(r).unwrap()).unwrap();
        let r = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(r, Ok(()));
    }

    #[test]
    fn dropping_the_stream_never_hangs() {
        if !available() {
            return;
        }
        let mut cmd = super::super::git();
        cmd.arg("--version");
        let s = Stream::start(cmd, |_| true, |_| {}).unwrap();
        drop(s);
    }
}
