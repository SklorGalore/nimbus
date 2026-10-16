//! Counting a huge file's lines in the background (M7).
//!
//! A mapped document opens with only its first few megabytes indexed, so
//! the first screen shows at once with line numbers. A worker thread then
//! reads the rest of the file in blocks and sends the line index on in
//! chunks; the app merges each into the document. Until it is done, line
//! numbers past the indexed part show as `~`, and a go-to-line waits.
//!
//! The worker reads with ordinary file reads (`MappedFile::read_at`), not
//! through the mapping, so sweeping a 4 GB file does not leave its pages
//! counted against Nimbus, and a file cut short ends the scan with an error
//! rather than a crash.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nimbus_core::Source;
use nimbus_core::line_index::{IndexChunk, LineIndex, Scanner};

use crate::mapping::MappedFile;

/// Indexed at open, before the first frame: a few milliseconds.
pub const FIRST_BLOCK: usize = 4 * 1024 * 1024;

/// Read at a time by the worker, into one reused buffer.
const BLOCK: usize = 4 * 1024 * 1024;

/// The worker sends a chunk of the index after this many bytes.
const PUBLISH: usize = 64 * 1024 * 1024;

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

/// The rest of a file's indexing, to run on a worker.
pub struct Indexing {
    /// Matches the document this index is for; results for any other
    /// (a closed tab, a reloaded file) are dropped.
    pub token: u64,
    scanner: Scanner,
    source: Arc<MappedFile>,
    /// Set to stop the worker early: the document closed or reloaded.
    pub cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for Indexing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Indexing")
            .field("token", &self.token)
            .field("at", &self.scanner.position())
            .finish_non_exhaustive()
    }
}

/// Index the start of `source` now. Returns the index so far and, if the
/// file is longer, the rest of the work.
///
/// # Errors
///
/// Returns the error from reading the file.
pub fn start(source: &Arc<MappedFile>) -> std::io::Result<(LineIndex, Option<Indexing>)> {
    let mut scanner = Scanner::default();
    let first = source.len().min(FIRST_BLOCK);
    let mut buf = vec![0; first];
    source.read_at(0, &mut buf)?;
    scanner.feed(&buf);
    let done = first == source.len();
    if done {
        scanner.finish();
    }
    let mut index = LineIndex::new();
    index
        .merge(scanner.take_chunk())
        .map_err(std::io::Error::other)?;
    let rest = (!done).then(|| Indexing {
        token: NEXT_TOKEN.fetch_add(1, Ordering::Relaxed),
        scanner,
        source: Arc::clone(source),
        cancel: Arc::new(AtomicBool::new(false)),
    });
    Ok((index, rest))
}

impl Indexing {
    /// Index the rest of the file, calling `send` with each chunk of the
    /// index (every [`PUBLISH`] bytes, and at the end). Stops early when
    /// cancelled, when `send` returns `false`, or when a read fails (the
    /// file was cut short: the index then stays partial).
    pub fn run(mut self, mut send: impl FnMut(u64, IndexChunk) -> bool) {
        let len = self.source.len();
        let mut pos = usize::try_from(self.scanner.position()).unwrap_or(len);
        let mut buf = vec![0; BLOCK.min(len.saturating_sub(pos))];
        let mut unsent = 0;
        while pos < len {
            if self.cancel.load(Ordering::Relaxed) {
                return;
            }
            let n = BLOCK.min(len - pos);
            if let Err(e) = self.source.read_at(pos, &mut buf[..n]) {
                log::warn!("indexing stopped at byte {pos}: {e}");
                let _ = send(self.token, self.scanner.take_chunk());
                return;
            }
            self.scanner.feed(&buf[..n]);
            pos += n;
            unsent += n;
            if pos == len {
                self.scanner.finish();
            }
            if (unsent >= PUBLISH || pos == len) && !send(self.token, self.scanner.take_chunk()) {
                return;
            }
            if unsent >= PUBLISH {
                unsent = 0;
            }
        }
    }

    /// Run [`Indexing::run`] on its own thread.
    pub fn spawn(self, send: impl FnMut(u64, IndexChunk) -> bool + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("nimbus-index".to_owned())
            .spawn(move || self.run(send));
        if let Err(e) = spawned {
            log::error!("could not start the indexing thread: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn small_files_are_indexed_at_once_and_large_ones_in_chunks() {
        let dir = std::env::temp_dir().join(format!("nimbus-index-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            // 9 MB of 100-byte lines: more than the first block.
            let line = format!("{}\n", "x".repeat(99));
            for _ in 0..90_000 {
                f.write_all(line.as_bytes()).unwrap();
            }
        }
        let source = Arc::new(MappedFile::new(std::fs::File::open(&path).unwrap()).unwrap());
        let (mut index, rest) = start(&source).unwrap();
        assert_eq!(index.indexed(), FIRST_BLOCK as u64);
        let rest = rest.unwrap();
        let mut chunks = 0;
        rest.run(|_, chunk| {
            chunks += 1;
            index.merge(chunk).unwrap();
            true
        });
        assert!(index.is_done());
        assert_eq!(index.lines(), 90_000);
        assert_eq!(
            chunks, 1,
            "under one publish interval: one chunk at the end"
        );

        std::fs::write(dir.join("small.txt"), "a\nb\n").unwrap();
        let small =
            Arc::new(MappedFile::new(std::fs::File::open(dir.join("small.txt")).unwrap()).unwrap());
        let (index, rest) = start(&small).unwrap();
        assert!(index.is_done() && rest.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
