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
pub struct Watch {
    watcher: Option<RecommendedWatcher>,
    watched: HashSet<PathBuf>,
    /// The watch limit was hit; said once.
    pub limit_hit: bool,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("watched", &self.watched.len())
            .finish_non_exhaustive()
    }
}

impl Watch {
    /// Start watching nothing yet. `on_change` gets each batch of changed
    /// paths, on a background thread.
    pub fn new(on_change: impl Fn(Vec<Change>) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<Change>();
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
                        let _ = tx.send(Change { path, structural });
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
        let spawned = std::thread::Builder::new()
            .name("nimbus-watch".to_owned())
            .spawn(move || debounce(&rx, &on_change));
        if let Err(e) = spawned {
            log::warn!("could not start the watch thread: {e}");
        }
        Self {
            watcher,
            watched: HashSet::new(),
            limit_hit: false,
        }
    }

    /// Watch exactly `wanted`: start on new folders, stop on ones no longer
    /// wanted. Returns a warning the first time the system refuses a watch.
    pub fn sync(&mut self, wanted: &HashSet<PathBuf>) -> Option<String> {
        let watcher = self.watcher.as_mut()?;
        let gone: Vec<PathBuf> = self.watched.difference(wanted).cloned().collect();
        for path in gone {
            let _ = watcher.unwatch(&path);
            self.watched.remove(&path);
        }
        let mut warning = None;
        for path in wanted.difference(&self.watched.clone()) {
            match watcher.watch(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.watched.insert(path.clone());
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
        warning
    }

    /// Folders being watched.
    #[cfg(test)]
    #[must_use]
    pub fn watched(&self) -> &HashSet<PathBuf> {
        &self.watched
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
        let mut watch = Watch::new(move |batch| {
            let _ = tx.send(batch);
        });
        let wanted: HashSet<PathBuf> = [dir.clone()].into_iter().collect();
        assert_eq!(watch.sync(&wanted), None);
        assert_eq!(watch.watched(), &wanted);
        // Give the OS watch a moment to start (FSEvents is asynchronous).
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        let batch = rx.recv_timeout(Duration::from_secs(5)).expect("an event");
        assert!(
            batch.iter().any(|c| c.path.ends_with("new.txt")),
            "{batch:?}"
        );
        watch.sync(&HashSet::new());
        assert!(watch.watched().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
