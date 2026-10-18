//! The find and replace bar.
//!
//! The query and replacement fields are small [`Document`]s, so they get
//! cursor movement, selection, undo and the clipboard from the same code as
//! the editor. Search runs synchronously for small documents and on a
//! background thread for large ones, streaming results back as each chunk
//! finishes; a newer search cancels an older one through a shared
//! [`Generation`] counter.

use std::ops::Range;
use std::sync::Arc;

use nimbus_core::search::{self, Generation, Query};
use nimbus_core::{Buffer, Snapshot, Text};
use regex::bytes::Regex;

use crate::document::Document;

/// Documents at least this large are searched on a background thread.
pub const BACKGROUND_SEARCH_BYTES: usize = 1024 * 1024;

/// Which field of the bar has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The search pattern.
    Find,
    /// The replacement text.
    Replace,
}

/// Results from a background search, sent to the event loop.
#[derive(Debug)]
pub struct SearchBatch {
    /// The search this batch belongs to; stale ones are dropped.
    pub generation: u64,
    /// The document version that was searched.
    pub version: u64,
    /// Matches found in one chunk, in order.
    pub matches: Vec<Range<usize>>,
    /// This is the last batch: the search reached the end.
    pub done: bool,
}

/// State of the find bar.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent UI flags (shown, replace row, focus, options, done); an enum would not be clearer"
)]
pub struct FindBar {
    /// Whether the bar is shown.
    pub open: bool,
    /// Whether the replace field is shown.
    pub replace_mode: bool,
    /// Focused field, while the bar has focus.
    pub focus: Field,
    /// Whether the bar has keyboard focus (rather than the editor).
    pub focused: bool,
    /// The query field.
    pub find: Document,
    /// The replacement field.
    pub replace: Document,
    /// Regex matching on.
    pub regex: bool,
    /// Case-sensitive matching on.
    pub case_sensitive: bool,
    /// Matches found so far, sorted.
    pub matches: Vec<Range<usize>>,
    /// Index in `matches` of the current match.
    pub current: Option<usize>,
    /// The search has reached the end of the document.
    pub complete: bool,
    /// More matches were found than are kept ([`MAX_KEPT`]).
    pub capped: bool,
    /// The pattern is not a valid regex.
    pub error: Option<String>,
    /// Document version the matches belong to.
    pub version: u64,
    generation: Arc<Generation>,
    compiled: Option<Regex>,
}

impl Default for FindBar {
    fn default() -> Self {
        Self {
            open: false,
            replace_mode: false,
            focus: Field::Find,
            focused: false,
            find: Document::untitled(),
            replace: Document::untitled(),
            regex: false,
            case_sensitive: false,
            matches: Vec::new(),
            current: None,
            complete: true,
            error: None,
            capped: false,
            version: 0,
            generation: Arc::new(Generation::default()),
            compiled: None,
        }
    }
}

/// Matches kept for one search, about 4 MB of ranges; past this they are
/// counted as "+" but not stored, and Replace All is refused.
pub const MAX_KEPT: usize = 250_000;

/// Text of a single-line field document.
fn field_text(doc: &Document) -> String {
    doc.buffer.slice(0..doc.buffer.len_bytes()).into_owned()
}

impl FindBar {
    /// Open the bar with focus on the query, pre-filled with `selection` if
    /// it is a short single line.
    pub fn open(&mut self, replace_mode: bool, selection: Option<String>) {
        self.open = true;
        self.focused = true;
        self.focus = Field::Find;
        self.replace_mode = replace_mode;
        if let Some(text) = selection.filter(|t| !t.contains('\n') && t.len() <= 200) {
            self.find = Document::untitled();
            let _ = self.find.insert(&text);
        }
        self.find.select_all();
    }

    /// Hide the bar and clear highlights.
    pub fn close(&mut self) {
        self.open = false;
        self.focused = false;
        self.generation.bump();
        self.matches.clear();
        self.current = None;
    }

    /// The focused field.
    pub fn field_mut(&mut self) -> &mut Document {
        match self.focus {
            Field::Find => &mut self.find,
            Field::Replace => &mut self.replace,
        }
    }

    /// The query as currently typed.
    #[must_use]
    pub fn query(&self) -> Query {
        Query {
            pattern: field_text(&self.find),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: false,
        }
    }

    /// Start a new search of `doc`. Small documents are searched now;
    /// large ones on a thread that reports through `send`. Returns `true`
    /// if results are complete already.
    pub fn search(
        &mut self,
        doc: &Document,
        send: impl Fn(SearchBatch) -> bool + Send + 'static,
    ) -> bool {
        let generation = self.generation.bump();
        self.matches.clear();
        self.current = None;
        self.version = doc.version;
        self.complete = false;
        self.compiled = None;
        let re = match self.query().compile() {
            Ok(Some(re)) => re,
            Ok(None) => {
                self.error = None;
                self.complete = true;
                return true;
            }
            Err(e) => {
                self.error = Some(e.0);
                self.complete = true;
                return true;
            }
        };
        self.error = None;
        self.compiled = Some(re.clone());
        if doc.buffer.len_bytes() < BACKGROUND_SEARCH_BYTES {
            let mut all = Vec::new();
            search::find_all(
                &doc.buffer,
                &re,
                || true,
                |batch| {
                    all.extend_from_slice(batch);
                    true
                },
            );
            self.matches = all;
            self.complete = true;
            return true;
        }
        let snapshot: Box<dyn Snapshot> = doc.buffer.snapshot();
        let version = doc.version;
        let gen_counter = Arc::clone(&self.generation);
        let spawned = std::thread::Builder::new()
            .name("search".into())
            .spawn(move || {
                let text: &dyn Text = &*snapshot;
                let finished = search::find_all(
                    text,
                    &re,
                    || gen_counter.current() == generation,
                    |batch| {
                        send(SearchBatch {
                            generation,
                            version,
                            matches: batch.to_vec(),
                            done: false,
                        })
                    },
                );
                if finished {
                    send(SearchBatch {
                        generation,
                        version,
                        matches: Vec::new(),
                        done: true,
                    });
                }
            });
        if let Err(e) = spawned {
            log::error!("could not start search thread: {e}");
            self.complete = true;
        }
        false
    }

    /// Stop a background search, keeping the matches found so far (the
    /// count then shows as incomplete). Returns whether one was running.
    /// Saving a huge file on Windows does this, because the search's
    /// snapshot keeps the old file mapped; the app searches again after.
    pub fn stop(&mut self) -> bool {
        if self.complete {
            return false;
        }
        self.generation.bump();
        true
    }

    /// Search `doc` to the end on this thread, replacing any results so far.
    /// Used before replace all, which must see every match.
    pub fn search_now(&mut self, doc: &Document) {
        self.generation.bump();
        self.matches.clear();
        self.current = None;
        self.version = doc.version;
        // Compile here too: this may run before any asynchronous search
        // (headless rendering) or after the query changed.
        match self.query().compile() {
            Ok(re) => {
                self.compiled = re;
                self.error = None;
            }
            Err(e) => {
                self.compiled = None;
                self.error = Some(e.0);
            }
        }
        self.capped = false;
        if let Some(re) = self.compiled.clone() {
            let mut all = Vec::new();
            search::find_all(
                &doc.buffer,
                &re,
                || true,
                |batch| {
                    all.extend_from_slice(batch);
                    // Past the cap there is nothing more to keep.
                    all.len() <= MAX_KEPT
                },
            );
            if all.len() > MAX_KEPT {
                all.truncate(MAX_KEPT);
                self.capped = true;
            }
            self.matches = all;
        }
        self.complete = true;
    }

    /// Take a batch from a background search. Returns `false` if it was
    /// stale and ignored.
    pub fn receive(&mut self, batch: SearchBatch) -> bool {
        if batch.generation != self.generation.current() || batch.version != self.version {
            return false;
        }
        self.keep(batch.matches);
        self.complete |= batch.done;
        true
    }

    /// Add matches, up to [`MAX_KEPT`] in all.
    fn keep(&mut self, found: Vec<Range<usize>>) {
        let room = MAX_KEPT.saturating_sub(self.matches.len());
        if found.len() > room {
            self.capped = true;
        }
        self.matches.extend(found.into_iter().take(room));
    }

    /// Pick the match at or after `pos` (or before, with `backwards`) as
    /// current, wrapping around. Returns its range.
    pub fn select_from(&mut self, pos: usize, backwards: bool) -> Option<Range<usize>> {
        self.current = if backwards {
            search::prev_match(&self.matches, pos)
        } else {
            search::next_match(&self.matches, pos)
        };
        self.current.and_then(|i| self.matches.get(i).cloned())
    }

    /// The replacement for the match at `range` in `doc`: capture groups
    /// like `$1` expand in regex mode; the replacement is literal otherwise.
    /// `None` when the text there no longer matches: leave it alone.
    ///
    /// The regex runs again on the match *with the text around it*, so
    /// assertions such as `\b`, `\B`, `^` and `$` see what they saw during
    /// the search. Run on the bare match they could fail and leave the
    /// match unreplaced.
    ///
    /// Captured text that is not valid UTF-8 (from a file with invalid
    /// bytes) is inserted with each bad sequence replaced by `�`.
    #[must_use]
    pub fn replacement_for(&self, doc: &Document, range: Range<usize>) -> Option<String> {
        const CONTEXT: usize = 256;
        let rep = field_text(&self.replace);
        let (Some(re), true) = (&self.compiled, self.regex) else {
            return Some(rep);
        };
        let ctx_start = range.start.saturating_sub(CONTEXT);
        let ctx_end = (range.end + CONTEXT).min(doc.buffer.len_bytes());
        let mut window = Vec::new();
        doc.buffer.read_into(ctx_start..ctx_end, &mut window).ok()?;
        let at = range.start - ctx_start;
        let caps = re.captures_at(&window, at)?;
        if caps.get(0).map(|m| m.range()) != Some(at..at + range.len()) {
            return None; // the text changed under the match
        }
        let mut out = Vec::new();
        caps.expand(rep.as_bytes(), &mut out);
        Some(String::from_utf8_lossy(&out).into_owned())
    }

    /// Short status for the bar, such as "3 of 120" or "no matches".
    #[must_use]
    pub fn status(&self) -> String {
        if let Some(e) = &self.error {
            return e.lines().last().unwrap_or("invalid pattern").to_owned();
        }
        if self.find.buffer.len_bytes() == 0 {
            return String::new();
        }
        let more = if self.complete && !self.capped {
            ""
        } else {
            "+"
        };
        match (self.matches.len(), self.current) {
            (0, _) if self.complete => "no matches".to_owned(),
            (n, Some(i)) => format!("{} of {n}{more}", i + 1),
            (n, None) => format!("{n}{more} matches"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_with(pattern: &str) -> FindBar {
        let mut bar = FindBar::default();
        bar.open(false, None);
        let _ = bar.find.insert(pattern);
        bar
    }

    #[test]
    fn small_documents_are_searched_at_once() {
        let doc = Document::from_text("foo bar foo\nFOO");
        let mut bar = bar_with("foo");
        assert!(bar.search(&doc, |_| true));
        assert_eq!(bar.matches, vec![0..3, 8..11, 12..15]);
        bar.case_sensitive = true;
        bar.search(&doc, |_| true);
        assert_eq!(bar.matches.len(), 2);
        assert_eq!(bar.select_from(4, false), Some(8..11));
        assert_eq!(bar.status(), "2 of 2");
    }

    #[test]
    fn invalid_regex_reports_an_error() {
        let doc = Document::from_text("x");
        let mut bar = bar_with("(");
        bar.regex = true;
        bar.search(&doc, |_| true);
        assert!(bar.error.is_some());
        assert_eq!(bar.matches, Vec::<Range<usize>>::new());
    }

    #[test]
    fn regex_replacement_expands_captures() {
        let doc = Document::from_text("key=value");
        let mut bar = bar_with(r"(\w+)=(\w+)");
        bar.regex = true;
        let _ = bar.replace.insert("$2=$1");
        bar.search(&doc, |_| true);
        assert_eq!(
            bar.replacement_for(&doc, 0..9).as_deref(),
            Some("value=key")
        );
        bar.regex = false;
        assert_eq!(
            bar.replacement_for(&doc, 0..9).as_deref(),
            Some("$2=$1"),
            "literal mode"
        );
    }

    #[test]
    fn regex_replacement_sees_the_surrounding_text() {
        let doc = Document::from_text("foobaz");
        let mut bar = bar_with(r"foo\B");
        bar.regex = true;
        let _ = bar.replace.insert("bar");
        bar.search(&doc, |_| true);
        assert_eq!(bar.matches, vec![0..3]);
        assert_eq!(bar.replacement_for(&doc, 0..3).as_deref(), Some("bar"));
    }

    #[test]
    fn search_now_works_without_a_previous_search() {
        let doc = Document::from_text("a wrap b wrap");
        let mut bar = bar_with("wrap");
        bar.search_now(&doc);
        assert_eq!(bar.matches, vec![2..6, 9..13]);
        assert!(bar.complete);
    }

    #[test]
    fn large_documents_stream_results_from_a_thread() {
        let text = "needle in a haystack\n".repeat(80_000);
        let doc = Document::from_text(&text);
        let mut bar = bar_with("needle");
        let (tx, rx) = std::sync::mpsc::channel();
        let done_now = bar.search(&doc, move |b| tx.send(b).is_ok());
        assert!(!done_now);
        while !bar.complete {
            let batch = rx.recv().unwrap();
            assert!(bar.receive(batch));
        }
        assert_eq!(bar.matches.len(), 80_000);
    }

    #[test]
    fn a_new_search_makes_old_batches_stale() {
        let text = "needle\n".repeat(200_000);
        let doc = Document::from_text(&text);
        let mut bar = bar_with("needle");
        let (tx, rx) = std::sync::mpsc::channel();
        let tx2 = tx.clone();
        bar.search(&doc, move |b| tx.send(b).is_ok());
        bar.search(&doc, move |b| tx2.send(b).is_ok());
        let mut fresh = 0;
        while !bar.complete {
            if bar.receive(rx.recv().unwrap()) {
                fresh += 1;
            }
        }
        assert!(fresh > 0);
        assert_eq!(
            bar.matches.len(),
            200_000,
            "no duplicates from the old search"
        );
    }

    #[test]
    fn matches_past_the_cap_are_counted_not_kept() {
        let doc = Document::from_text(&"x".repeat(MAX_KEPT + 10));
        let mut bar = bar_with("x");
        bar.search_now(&doc);
        assert_eq!(bar.matches.len(), MAX_KEPT);
        assert!(bar.capped);
        assert!(bar.status().contains('+'), "{}", bar.status());
    }
}
