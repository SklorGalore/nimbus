//! Following a file that grows on disk, such as a log being written (M12).
//!
//! When another program appends to an open file, reading the whole file
//! again (M6's reload) throws away the undo history and costs time in
//! proportion to the file. Appending only the new bytes costs time in
//! proportion to what arrived, and leaves everything else alone. The new
//! text arrives through [`Document::append_external`], which the syntax
//! state and the compare view see like any edit, but which neither marks
//! the document dirty nor records an undo step: the text still matches the
//! file, so there is nothing to save and nothing to undo.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nimbus_core::{Buffer, ExtendError, Source, Text};

use super::{Document, Stamp};
use crate::indexer;
use crate::io;
use crate::mapping::MappedFile;

/// The least time between two extensions of a following mapped document
/// ([`Document::extend_mapping`]): each maps the file again.
pub(super) const EXTEND_GAP: Duration = Duration::from_millis(200);

/// Whether a document's file grew on disk while it was open, and whether
/// the view follows its end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tail {
    /// The file has not grown since it was opened.
    #[default]
    Fixed,
    /// It grew; the status bar offers Follow Tail.
    Grew,
    /// The view stays at the end as new lines arrive, until the user
    /// scrolls up or turns it off.
    Following,
}

/// How many bytes before the old end are compared with the file, and how
/// many at the start, to tell an append from a rewrite. A log only ever
/// grows at the end; a program that rewrote the file almost always changed
/// one of these too, and then the file is reloaded instead.
const CHECK_BYTES: usize = 64;

impl Document {
    /// The file grew from `old` to `now`: append the new bytes to this rope
    /// document, which has no unsaved edits. Returns `false`, changing
    /// nothing, when this is not a plain append (the bytes before the old
    /// end differ, the new bytes are not UTF-8, or the file is now too big
    /// for a rope); the caller reloads then.
    ///
    /// A writer may stop in the middle of a character. The complete
    /// characters are appended and the rest waits for the next change: the
    /// recorded length says where the document's text ends in the file.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file cannot be read.
    pub(super) fn append_external(
        &mut self,
        path: &Path,
        old: Stamp,
        now: Stamp,
    ) -> std::io::Result<bool> {
        let end = self.buffer.len_bytes();
        let bom = if self.bom { 3 } else { 0 };
        if old.len != end as u64 + bom || now.len >= io::MAX_ROPE_BYTES {
            return Ok(false);
        }
        let mut file = std::fs::File::open(path)?;
        // The start of the file, as the document has it.
        let head = start_within(&self.buffer, CHECK_BYTES.min(end));
        let mut bytes = vec![0; head];
        file.seek(SeekFrom::Start(bom))?;
        file.read_exact(&mut bytes)?;
        if !same(&self.buffer, 0, &bytes) {
            return Ok(false);
        }
        // The end of the document's text, then everything after it.
        let mut from = end.saturating_sub(CHECK_BYTES);
        while from > 0 && !self.buffer.is_char_boundary(from) {
            from -= 1;
        }
        bytes.clear();
        file.seek(SeekFrom::Start(from as u64 + bom))?;
        file.take(now.len - from as u64 - bom)
            .read_to_end(&mut bytes)?;
        let check = end - from;
        if bytes.len() < check || !same(&self.buffer, from, &bytes[..check]) {
            return Ok(false);
        }
        let new = &bytes[check..];
        let text = match std::str::from_utf8(new) {
            Ok(t) => t,
            // Cut short inside a character: take the whole ones.
            Err(e) if e.error_len().is_none() => {
                std::str::from_utf8(&new[..e.valid_up_to()]).unwrap_or_default()
            }
            Err(_) => return Ok(false),
        };
        if !text.is_empty() {
            let edit = self
                .buffer
                .replace(end..end, text)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            self.edits.note(&self.buffer, &edit);
            self.syntax.edit(&self.buffer, &edit);
            self.version += 1;
            self.remember_disk_text();
        }
        self.disk = Some(Stamp {
            len: old.len + text.len() as u64,
            modified: now.modified,
        });
        Ok(true)
    }

    /// The file of this mapped document, which has no unsaved edits, grew
    /// to `now`: map it again at its new length and append the new bytes
    /// (M12, `DESIGN.md` §9). Returns `false`, changing nothing, when the
    /// file was rewritten rather than appended to, or for a document not
    /// backed by a mapping.
    ///
    /// A mapping has a fixed length, so growth means a second mapping of
    /// the same file. Its first bytes are the very bytes the first one
    /// maps, so every piece of the document reads the same through it; the
    /// piece table checks a few of them, takes the new mapping as its
    /// source and appends one piece for the new bytes
    /// ([`nimbus_core::MappedBuffer::extend_source`]). The old mapping is
    /// unmapped when the last snapshot or undo step holding it goes. Memory
    /// does not grow with the file: mapped pages are the system's file
    /// cache, as in M7, and the line index grows by a checkpoint per 1024
    /// lines.
    ///
    /// # Errors
    ///
    /// Returns why the file could not be mapped or read.
    pub(super) fn extend_mapping(&mut self, path: &Path, now: Stamp) -> Result<bool, String> {
        let Some(old) = self.mapping.as_ref() else {
            return Ok(false);
        };
        // A byte order mark stays left out, as when the file was opened.
        let skip = usize::try_from(old.file_len() - old.len() as u64).unwrap_or(0);
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mapped = Arc::new(
            MappedFile::new(file)
                .map_err(|e| e.to_string())?
                .skip_prefix(skip),
        );
        if !mapped.continues(old).map_err(|e| e.to_string())? {
            return Ok(false);
        }
        let counting = self.counting_lines();
        let Some(buffer) = self.buffer.as_mapped_mut() else {
            return Ok(false);
        };
        let source: Arc<dyn Source> = mapped.clone();
        let edit = match buffer.extend_source(source) {
            Ok(edit) => edit,
            Err(ExtendError::Shorter { .. } | ExtendError::Differs) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
        // Count the new lines: here, for the little a log adds at a time,
        // or on a worker for more, or when one is still counting (it
        // stops, and the new one carries on from what it merged).
        let gap = mapped.len() as u64 - buffer.line_index().indexed();
        if counting || gap > indexer::FIRST_BLOCK as u64 {
            let job = indexer::resume(&mapped, buffer.line_index());
            self.adopt_indexing(Some(job));
        } else {
            let chunks = indexer::catch_up(&mapped, buffer.line_index())?;
            for chunk in chunks {
                if let Err(e) = buffer.merge_index(chunk) {
                    log::warn!("line index chunk refused: {e}");
                    break;
                }
            }
        }
        self.edits.note(&self.buffer, &edit);
        self.syntax.edit(&self.buffer, &edit);
        self.syntax.touch();
        self.version += 1;
        self.disk = Some(Stamp {
            len: mapped.file_len(),
            modified: now.modified,
        });
        self.mapping = Some(mapped);
        self.conflict = false;
        Ok(true)
    }

    /// An upward scroll: stop following, so reading an older part of the
    /// file is not interrupted by the next lines arriving.
    pub fn stop_following(&mut self) {
        if self.tail == Tail::Following {
            self.tail = Tail::Grew;
        }
    }

    /// After a command: following stops once the cursor leaves the end or
    /// the text is edited, since either means the user is doing something
    /// other than watching the end.
    pub fn check_following(&mut self) {
        if self.tail == Tail::Following
            && (self.is_dirty()
                || self.selections.count() > 1
                || self.selection() != nimbus_core::Selection::cursor(self.buffer.len_bytes()))
        {
            self.tail = Tail::Grew;
        }
    }

    /// Put the cursor at the end of the text, for a view following it.
    pub fn follow_end(&mut self) {
        let end = self.buffer.len_bytes();
        if self.selections.count() > 1 || self.selection() != nimbus_core::Selection::cursor(end) {
            self.click(end, false);
        }
    }
}

/// The largest character boundary at or below `at`.
fn start_within(t: &dyn Text, mut at: usize) -> usize {
    while at > 0 && !t.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Whether `t` holds `bytes` at `at`, read in the text's own chunks so
/// nothing is copied.
fn same(t: &dyn Text, mut at: usize, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let (chunk, start) = t.chunk_at(at);
        let chunk = chunk.get(at - start..).unwrap_or_default();
        if chunk.is_empty() {
            return false;
        }
        let n = chunk.len().min(bytes.len());
        if chunk[..n] != bytes[..n] {
            return false;
        }
        at += n;
        bytes = &bytes[n..];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DiskChange;
    use crate::languages::Languages;
    use std::io::Write;

    fn append(path: &Path, text: &[u8]) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(text).unwrap();
    }

    #[test]
    fn appended_text_arrives_clean_and_without_undo() {
        let dir = std::env::temp_dir().join(format!("nimbus-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.txt");
        std::fs::write(&path, "one\n").unwrap();
        let languages = Languages::default();
        let mut doc = Document::open(&path).unwrap();
        doc.click(2, false);
        append(&path, "two\nthr\u{e9}".as_bytes());
        // Only the first byte of the last é: it waits.
        let cut = std::fs::read(&path).unwrap();
        std::fs::write(&path, &cut[..cut.len() - 1]).unwrap();
        assert_eq!(doc.disk_changed(&languages), DiskChange::Appended);
        assert_eq!(doc.buffer.slice(0..doc.buffer.len_bytes()), "one\ntwo\nthr");
        assert!(!doc.is_dirty());
        assert_eq!(doc.tail, Tail::Grew);
        assert_eq!(doc.cursor(), 2, "the cursor stays where it was");
        std::fs::write(&path, &cut).unwrap();
        assert_eq!(doc.disk_changed(&languages), DiskChange::Appended);
        assert_eq!(
            doc.buffer.slice(0..doc.buffer.len_bytes()),
            "one\ntwo\nthr\u{e9}"
        );
        assert!(!doc.history.can_undo(), "nothing to undo");
        doc.follow_end();
        assert_eq!(doc.cursor(), doc.buffer.len_bytes());

        // A rewrite that is longer is not an append: it reloads.
        std::fs::write(&path, "ONE\ntwo\nthr\u{e9} and more").unwrap();
        assert_eq!(doc.disk_changed(&languages), DiskChange::Reloaded);
        assert_eq!(doc.buffer.slice(0..3), "ONE");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_following_mapped_document_maps_the_grown_file() {
        let dir = std::env::temp_dir().join(format!("nimbus-tail-map-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");
        std::fs::write(&path, "\u{feff}one\ntwo\n").unwrap();
        let languages = Languages::default();
        let mut doc = Document::open_mapped(&path).unwrap();
        assert!(doc.buffer.is_mapped() && doc.bom);

        // Not following: the file stays as it was loaded.
        append(&path, b"three\n");
        assert_eq!(doc.disk_changed(&languages), DiskChange::NotReloaded);
        assert_eq!(doc.tail, Tail::Grew);
        assert_eq!(doc.buffer.len_bytes(), 8);

        doc.tail = Tail::Following;
        assert_eq!(doc.disk_changed(&languages), DiskChange::Appended);
        let all = |d: &Document| d.buffer.slice(0..d.buffer.len_bytes()).into_owned();
        assert_eq!(all(&doc), "one\ntwo\nthree\n");
        assert!(!doc.is_dirty() && !doc.conflict);
        assert!(
            doc.buffer.is_fully_indexed(),
            "the few new lines are counted at once"
        );
        assert_eq!(doc.buffer.line_number(doc.buffer.len_bytes()), Some(3));
        assert_eq!(doc.backing_file().unwrap().file_len(), 17);

        // Soon after, it waits; later it maps again.
        append(&path, b"four\n");
        assert!(matches!(doc.disk_changed(&languages), DiskChange::Later(_)));
        doc.extended_at = None;
        assert_eq!(doc.disk_changed(&languages), DiskChange::Appended);
        assert_eq!(all(&doc), "one\ntwo\nthree\nfour\n");

        // Rewritten, not appended to: not taken as an append. (Windows
        // refuses to truncate a mapped file, so this cannot happen there.)
        #[cfg(unix)]
        {
            doc.extended_at = None;
            std::fs::write(&path, "\u{feff}ONE\ntwo\nthree\nfour\nfive\n").unwrap();
            assert_eq!(doc.disk_changed(&languages), DiskChange::NotReloaded);
            // (Rewritten in place, so the mapped bytes changed too: M7's
            // documented hazard. The document is just not extended.)
            assert_eq!(doc.buffer.len_bytes(), 19);
        }
        drop(doc);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
