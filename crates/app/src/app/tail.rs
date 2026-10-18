//! Follow Tail as the app runs it (M12): the toggle, and keeping each
//! following tab's cursor at the end as the file grows. The appending
//! itself is the document's ([`crate::document::Tail`]).

use super::App;
use crate::document::{DiskChange, Tail};

impl App {
    /// Turn Follow Tail on or off for the active tab. Turning it on reads
    /// what has arrived since the last change and goes to the end.
    pub(super) fn toggle_follow(&mut self) {
        let doc = &mut self.tabs[self.active].doc;
        if doc.tail == Tail::Following {
            doc.tail = Tail::Grew;
            return;
        }
        if doc.path.is_none() || doc.is_placeholder() {
            self.say("Follow Tail is for files on disk", false);
            return;
        }
        if doc.is_dirty() {
            self.say(
                "Follow Tail needs the file without unsaved edits: save or undo them first",
                false,
            );
            return;
        }
        // Catch up with what arrived; a mapped document extends only while
        // following, so that goes first.
        doc.tail = Tail::Following;
        match doc.disk_changed(&self.languages) {
            DiskChange::Failed(e) => {
                doc.tail = Tail::Grew;
                self.say(format!("Cannot read the file: {e}"), true);
                return;
            }
            DiskChange::Later(due) => {
                if let Some(path) = doc.canonical.clone() {
                    self.recheck_later(due, path);
                }
            }
            _ => {}
        }
        let doc = &mut self.tabs[self.active].doc;
        doc.follow_end();
        self.start_indexing();
        self.changed(true);
    }

    /// After changes on disk: move the cursor of each following tab to
    /// the new end. Returns whether the active tab follows, so its view
    /// scrolls there.
    pub(super) fn follow_tails(&mut self) -> bool {
        // A grown mapped file may need its new lines counted on a worker.
        self.start_indexing();
        for tab in &mut self.tabs {
            if tab.doc.tail == Tail::Following {
                tab.doc.follow_end();
            }
        }
        self.tabs[self.active].doc.tail == Tail::Following
    }

    /// Look at `path` on disk again at `due`, as if the watcher had said
    /// it changed.
    pub(super) fn recheck_later(&mut self, due: std::time::Instant, path: std::path::PathBuf) {
        self.recheck_at = Some(self.recheck_at.map_or(due, |t| t.min(due)));
        if !self.recheck.contains(&path) {
            self.recheck.push(path);
        }
    }
}
