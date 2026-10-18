//! The open documents, kept in one list and named by [`DocId`] (M13).
//!
//! A tab names its document instead of owning it, so that two tabs (in
//! two editor groups) can show the same file: one text, one undo history,
//! one dirty dot, one save. The list never shrinks while the app runs: a
//! closed document's slot is emptied and reused by the next one opened,
//! so a `DocId` held by a tab stays valid without renumbering.

use std::ops::{Index, IndexMut};

use crate::document::Document;

/// Which document a tab shows: its place in [`Docs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocId(usize);

/// The open documents.
#[derive(Debug, Default)]
pub struct Docs {
    /// Every slot, open or emptied.
    slots: Vec<Document>,
    /// Whether each slot holds an open document.
    open: Vec<bool>,
    /// Emptied slots, to reuse.
    free: Vec<usize>,
}

impl Docs {
    /// Add `doc`, returning its id. Its edits are kept for its tabs to
    /// follow ([`crate::document::Journal`]).
    pub fn insert(&mut self, mut doc: Document) -> DocId {
        doc.journal_mut().keep = true;
        if let Some(i) = self.free.pop() {
            self.slots[i] = doc;
            self.open[i] = true;
            return DocId(i);
        }
        self.slots.push(doc);
        self.open.push(true);
        DocId(self.slots.len() - 1)
    }

    /// Close the document `id`, handing it back so it drops (and lets go
    /// of its file) when the caller is done with it. Its slot is reused.
    pub fn remove(&mut self, id: DocId) -> Document {
        let Some(open) = self.open.get_mut(id.0) else {
            return Document::default();
        };
        if !*open {
            return Document::default();
        }
        *open = false;
        self.free.push(id.0);
        std::mem::take(&mut self.slots[id.0])
    }

    /// The open documents and their ids.
    pub fn iter(&self) -> impl Iterator<Item = (DocId, &Document)> {
        self.slots
            .iter()
            .zip(&self.open)
            .enumerate()
            .filter(|(_, (_, open))| **open)
            .map(|(i, (d, _))| (DocId(i), d))
    }

    /// The same, to change them.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (DocId, &mut Document)> {
        self.slots
            .iter_mut()
            .zip(&self.open)
            .enumerate()
            .filter(|(_, (_, open))| **open)
            .map(|(i, (d, _))| (DocId(i), d))
    }
}

impl Index<DocId> for Docs {
    type Output = Document;

    fn index(&self, id: DocId) -> &Document {
        &self.slots[id.0]
    }
}

impl IndexMut<DocId> for Docs {
    fn index_mut(&mut self, id: DocId) -> &mut Document {
        &mut self.slots[id.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_core::Text;

    #[test]
    fn closed_slots_are_reused_and_skipped() {
        let mut docs = Docs::default();
        let a = docs.insert(Document::from_text("a"));
        let b = docs.insert(Document::from_text("b"));
        assert_eq!(docs.remove(a).buffer.len_bytes(), 1);
        assert_eq!(docs.iter().map(|(id, _)| id).collect::<Vec<_>>(), [b]);
        let c = docs.insert(Document::from_text("cc"));
        assert_eq!(c, a, "the emptied slot is reused");
        assert_eq!(docs[c].buffer.len_bytes(), 2);
        assert_eq!(docs.iter().count(), 2);
    }
}
