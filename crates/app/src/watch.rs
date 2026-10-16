//! Noticing changes on disk.
//!
//! The OS reports changes to watched folders as events (`notify` wraps
//! `FSEvents` on macOS, `inotify` on Linux and `ReadDirectoryChangesW` on
//! Windows). One save can produce several events (write, rename, metadata),
//! and a build can produce thousands, so events are *debounced*: collected
//! for [`DEBOUNCE`] after the first one, deduplicated, then delivered as one
//! batch of paths.
//!
//! Only the folders the app asks for are watched, each on its own (not
//! recursively): the folders open in the sidebar and the folders of open
//! files. A recursive watch of a large repository would register every
//! folder up front on Linux, costing startup time and running into the
//! system's watch limit.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

/// How long to collect events before delivering them.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// A path that changed on disk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Change {
    /// What changed.
    pub path: PathBuf,
    /// Created, removed or renamed (the folder's listing changed), rather
    /// than only written to.
    pub structural: bool,
}

/// Watches a set of folders and reports the paths that changed.
///
/// The OS watcher lives on its own thread, and this handle only sends it
/// the set of folders to watch. Changing what is watched can block: on
/// macOS, `notify` waits for its event loop to go idle before restarting
/// the stream, which never happens while a build writes thousands of files
/// under a watched folder (macOS watches are recursive underneath). On
/// that thread a wait only delays the update; on the main thread it froze
/// the window. Dropping the handle ends the thread without waiting for it.
pub struct Watch {
    tx: Option<mpsc::Sender<HashSet<PathBuf>>>,
    /// The set last sent, so an unchanged set is not sent again.
    last: HashSet<PathBuf>,
    /// What the thread is watching, for tests.
    #[cfg(test)]
    watched: std::sync::Arc<std::sync::Mutex<HashSet<PathBuf>>>,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("folders", &self.last.len())
            .finish_non_exhaustive()
    }
}

impl Watch {
    /// Start a watcher thread watching nothing yet. `on_change` gets each
    /// batch of changes, and `on_warning` a message the first time the
    /// system refuses a watch; both are called on background threads.
    pub fn new(
        on_change: impl Fn(Vec<Change>) + Send + 'static,
        on_warning: impl Fn(String) + Send + 'static,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::channel::<Change>();
        let spawned = std::thread::Builder::new()
            .name("nimbus-watch-events".to_owned())
            .spawn(move || debounce(&events_rx, &on_change));
        if let Err(e) = spawned {
            log::warn!("could not start the watch thread: {e}");
        }
        let (tx, rx) = mpsc::channel::<HashSet<PathBuf>>();
        #[cfg(test)]
        let watched = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
        #[cfg(test)]
        let shared = std::sync::Arc::clone(&watched);
        let spawned = std::thread::Builder::new()
            .name("nimbus-watch".to_owned())
            .spawn(move || {
                let mut owner = Owner::new(events_tx);
                // Each message is the whole wanted set; only the newest
                // queued one matters.
                while let Ok(mut wanted) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        wanted = newer;
                    }
                    if let Some(w) = owner.apply(&wanted) {
                        on_warning(w);
                    }
                    #[cfg(test)]
                    if let Ok(mut w) = shared.lock() {
                        w.clone_from(&owner.watched);
                    }
                }
            });
        let tx = match spawned {
            Ok(_) => Some(tx),
            Err(e) => {
                log::warn!("could not start the watcher: {e}");
                None
            }
        };
        Self {
            tx,
            last: HashSet::new(),
            #[cfg(test)]
            watched,
        }
    }

    /// Watch exactly `wanted` from now on. Returns at once; the watcher
    /// thread applies it.
    pub fn sync(&mut self, wanted: &HashSet<PathBuf>) {
        if *wanted == self.last {
            return;
        }
        self.last.clone_from(wanted);
        if let Some(tx) = &self.tx {
            let _ = tx.send(wanted.clone());
        }
    }

    /// Folders the thread is watching, for tests.
    #[cfg(test)]
    #[must_use]
    pub fn watched(&self) -> HashSet<PathBuf> {
        self.watched.lock().map(|w| w.clone()).unwrap_or_default()
    }
}

/// The OS watcher and what it watches; lives on the watcher thread.
struct Owner {
    watcher: Option<RecommendedWatcher>,
    watched: HashSet<PathBuf>,
    limit_hit: bool,
}

impl Owner {
    fn new(events: mpsc::Sender<Change>) -> Self {
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                // Reads are not changes.
                Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
                Ok(event) => {
                    // Writes leave a folder's listing alone; anything else
                    // (including kinds a platform cannot tell apart) may not.
                    let structural = !matches!(
                        event.kind,
                        EventKind::Modify(
                            notify::event::ModifyKind::Data(_)
                                | notify::event::ModifyKind::Metadata(_)
                        )
                    );
                    for path in event.paths {
                        let _ = events.send(Change { path, structural });
                    }
                }
                Err(e) => log::warn!("file watcher: {e}"),
            }
        });
        let watcher = match watcher {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("file watching unavailable: {e}");
                None
            }
        };
        Self {
            watcher,
            watched: HashSet::new(),
            limit_hit: false,
        }
    }

    /// Start and stop watches to match `wanted`, as one batch (one stream
    /// restart on macOS). Returns a warning the first time the system
    /// refuses a watch for lack of watches.
    fn apply(&mut self, wanted: &HashSet<PathBuf>) -> Option<String> {
        let watcher = self.watcher.as_mut()?;
        let gone: Vec<PathBuf> = self.watched.difference(wanted).cloned().collect();
        let new: Vec<PathBuf> = wanted.difference(&self.watched).cloned().collect();
        if gone.is_empty() && new.is_empty() {
            return None;
        }
        let mut warning = None;
        let mut paths = watcher.paths_mut();
        for path in gone {
            let _ = paths.remove(&path);
            self.watched.remove(&path);
        }
        for path in new {
            match paths.add(&path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.watched.insert(path);
                }
                // A folder that vanished: the next listing drops it.
                Err(e) if matches!(e.kind, notify::ErrorKind::PathNotFound) => {}
                Err(e) => {
                    log::warn!("cannot watch {}: {e}", path.display());
                    if !self.limit_hit && matches!(e.kind, notify::ErrorKind::MaxFilesWatch) {
                        self.limit_hit = true;
                        warning = Some(
                            "The system's file watch limit was reached; some changes on disk \
                             will not show until reopened"
                                .to_owned(),
                        );
                    }
                }
            }
        }
        if let Err(e) = paths.commit() {
            log::warn!("file watcher: {e}");
        }
        warning
    }
}

/// Collect paths from `rx` and hand them on in batches: from the first
/// path, wait [`DEBOUNCE`], then deliver everything that arrived. Ends when
/// the watcher is dropped.
fn debounce(rx: &mpsc::Receiver<Change>, on_change: &impl Fn(Vec<Change>)) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(path) => batch.push(path),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    deliver(batch, on_change);
                    return;
                }
            }
        }
        deliver(batch, on_change);
    }
}

/// Hand on one change per path: structural if any of its events were.
fn deliver(mut batch: Vec<Change>, on_change: &impl Fn(Vec<Change>)) {
    // Structural sorts after not, so the last of each path is kept.
    batch.sort();
    let mut out: Vec<Change> = Vec::with_capacity(batch.len());
    for change in batch {
        match out.last_mut() {
            Some(last) if last.path == change.path => *last = change,
            _ => out.push(change),
        }
    }
    on_change(out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn bursts_arrive_as_one_batch() {
        let (tx, rx) = mpsc::channel();
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&got);
        let worker = std::thread::spawn(move || {
            debounce(&rx, &|batch| sink.lock().unwrap().push(batch));
        });
        for (name, structural) in [("b", false), ("a", true), ("b", false), ("a", false)] {
            tx.send(Change {
                path: PathBuf::from(name),
                structural,
            })
            .unwrap();
        }
        drop(tx);
        worker.join().unwrap();
        let got = got.lock().unwrap();
        let change = |p: &str, structural| Change {
            path: PathBuf::from(p),
            structural,
        };
        assert_eq!(*got, vec![vec![change("a", true), change("b", false)]]);
    }

    #[test]
    fn changes_in_a_watched_folder_are_reported() {
        let dir = std::env::temp_dir().join(format!("nimbus-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Some platforms report the real path (/private/var on macOS).
        let dir = std::fs::canonicalize(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut watch = Watch::new(
            move |batch| {
                let _ = tx.send(batch);
            },
            |_| {},
        );
        let wanted: HashSet<PathBuf> = [dir.clone()].into_iter().collect();
        watch.sync(&wanted);
        let deadline = Instant::now() + Duration::from_secs(5);
        while watch.watched() != wanted && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(watch.watched(), wanted);
        // Give the OS watch a moment to start (FSEvents is asynchronous).
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        let batch = rx.recv_timeout(Duration::from_secs(5)).expect("an event");
        assert!(
            batch.iter().any(|c| c.path.ends_with("new.txt")),
            "{batch:?}"
        );
        watch.sync(&HashSet::new());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !watch.watched().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(watch.watched(), HashSet::new());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;

    /// Changing the watched set and dropping the watcher return at once,
    /// even while files under a watched folder change non-stop (as during
    /// a build), which on macOS keeps the watcher's event loop busy.
    #[test]
    fn syncing_and_dropping_never_wait_for_a_busy_watcher() {
        let dir = std::env::temp_dir().join(format!("nimbus-busy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("deep/er")).unwrap();
        let dir = std::fs::canonicalize(&dir).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let (dir, stop) = (dir.clone(), std::sync::Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut i = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = std::fs::write(dir.join("deep/er").join(format!("f{}", i % 50)), "x");
                    i += 1;
                }
            })
        };
        let mut watch = Watch::new(|_| {}, |_| {});
        let one: HashSet<PathBuf> = [dir.clone()].into_iter().collect();
        let two: HashSet<PathBuf> = [dir.clone(), dir.join("deep")].into_iter().collect();
        let started = Instant::now();
        for _ in 0..20 {
            watch.sync(&one);
            watch.sync(&two);
        }
        drop(watch);
        let took = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        writer.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            took < Duration::from_millis(50),
            "the caller waited {took:?}"
        );
    }
}
