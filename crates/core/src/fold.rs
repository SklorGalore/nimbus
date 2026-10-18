//! Folding: hiding runs of whole lines behind the line before them (M13).
//!
//! A fold is a byte range from the end of its first line, which stays on
//! screen, to the end of its last hidden line. With `ab\ncd\nef\ngh` and
//! the fold `2..8`, the line `ab` shows (with a chip after it), `cd` and
//! `ef` are hidden, and `gh` follows. A line is hidden exactly when its
//! start lies in `start + 1..=end`, which is also the rule for any other
//! position: the end of the first line is visible (a cursor sits there),
//! everything after it up to and including `end` is not.
//!
//! [`Folds`] keeps the folds as two sorted lists of ranges. The *active*
//! ones do not overlap, and they are what hides text. A fold made inside
//! an active one, or swallowed by a new fold around it, waits in the
//! *inner* list, and comes back when the fold around it opens. Nothing
//! is stored per line: going from a line to the next visible one is a
//! binary search over the folds, so a 4 GB file with a thousand folds
//! costs a thousand ranges, not an array of millions of lines
//! (`DESIGN.md` §4.1, the M7 rule).
//!
//! Folds follow edits the way selections do: an edit wholly before a fold
//! shifts it, one after it leaves it alone, and one touching its hidden
//! text opens it, since text you are changing should be on screen.
//!
//! The regions offered for folding come from [`indent_region`], by
//! indentation, and [`bracket_region`], by the bracket that opens at the
//! end of a line. Both look at a bounded number of lines or bytes, so
//! asking about a line in a huge file never scans to the end of it.

use std::ops::Range;

use crate::{Edit, LineSpan, Text, brackets};

/// The folds of one view of a text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folds {
    /// What is folded now: sorted by start, never overlapping.
    active: Vec<Range<usize>>,
    /// Folds inside an active one, sorted by start and then by end
    /// descending (so a fold comes before the ones it contains). Each is
    /// contained in some active fold.
    inner: Vec<Range<usize>>,
}

impl Folds {
    /// No folds.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether nothing is folded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// How many folds hide text now (inner ones not counted).
    #[must_use]
    pub fn len(&self) -> usize {
        self.active.len()
    }

    /// The folds hiding text now, in text order.
    #[must_use]
    pub fn active(&self) -> &[Range<usize>] {
        &self.active
    }

    /// Every fold, active ones first and then the ones kept inside them,
    /// for saving with the session.
    pub fn all(&self) -> impl Iterator<Item = &Range<usize>> {
        self.active.iter().chain(&self.inner)
    }

    /// Open every fold.
    pub fn clear(&mut self) {
        self.active.clear();
        self.inner.clear();
    }

    /// Fold `range`, which should come from [`lines_range`] (or a region
    /// function). A fold inside an active one is kept for when that one
    /// opens; active folds inside the new one are kept the same way. A
    /// fold that crosses an active one, or is empty, is refused. Returns
    /// whether the fold was added.
    pub fn fold(&mut self, range: Range<usize>) -> bool {
        if range.start >= range.end {
            return false;
        }
        if self.active.contains(&range) || self.inner.contains(&range) {
            return false;
        }
        // The active folds overlapping the new one form a run.
        let first = self.active.partition_point(|f| f.end <= range.start);
        let mut last = first;
        while last < self.active.len() && self.active[last].start < range.end {
            last += 1;
        }
        let overlapping = &self.active[first..last];
        if let [only] = overlapping
            && contains(only, &range)
        {
            if self.inner.iter().any(|g| crosses_range(g, &range)) {
                return false;
            }
            insert_inner(&mut self.inner, range);
            return true;
        }
        if overlapping.iter().any(|f| !contains(&range, f)) {
            return false;
        }

        for f in self.active.drain(first..last) {
            insert_inner(&mut self.inner, f);
        }
        self.active.insert(first, range);
        true
    }

    /// The active fold hiding position `pos` (see the module notes).
    #[must_use]
    pub fn hiding(&self, pos: usize) -> Option<Range<usize>> {
        let i = self.active.partition_point(|f| f.end < pos);
        self.active
            .get(i)
            .filter(|f| f.start < pos && pos <= f.end)
            .cloned()
    }

    /// The active fold whose first line is `span`: it starts within the
    /// line's ending. That is where its chip shows.
    #[must_use]
    pub fn on_line(&self, span: LineSpan) -> Option<Range<usize>> {
        let i = self.active.partition_point(|f| f.start < span.start);
        let next = span.next.unwrap_or(usize::MAX);
        self.active
            .get(i)
            .filter(|f| f.start >= span.end && f.start < next)
            .cloned()
    }

    /// Open the active fold whose first line is `span`, bringing back any
    /// folds kept inside it. Returns the opened fold.
    pub fn unfold_line(&mut self, span: LineSpan) -> Option<Range<usize>> {
        let f = self.on_line(span)?;
        self.remove_active(&f);
        Some(f)
    }

    /// Open whatever folds hide `pos`, inner ones included, so that it is
    /// on screen: for a find match or a go-to-line. Returns whether any
    /// opened.
    pub fn reveal(&mut self, pos: usize) -> bool {
        let mut any = false;
        while let Some(f) = self.hiding(pos) {
            self.remove_active(&f);
            any = true;
        }
        any
    }

    /// Open every active fold that hides any of `range`, as [`Folds::reveal`]
    /// does for one position: for a selection made by find or a jump.
    pub fn reveal_range(&mut self, range: Range<usize>) -> bool {
        let mut any = false;
        loop {
            let i = self.active.partition_point(|f| f.end <= range.start);
            let Some(f) = self.active.get(i).cloned() else {
                break;
            };
            // Hidden bytes are `f.start + 1..=f.end`; the range covers
            // `range.start..range.end`, or just the point for an empty one.
            let hits = if range.is_empty() {
                f.start < range.start && range.start <= f.end
            } else {
                f.start + 1 < range.end && range.start <= f.end
            };
            if !hits {
                break;
            }
            self.remove_active(&f);
            any = true;
        }
        any
    }

    /// Where the visible line after `span` starts: `span.next`, unless
    /// that line is hidden, in which case the line after the fold.
    #[must_use]
    pub fn next_line(&self, t: &dyn Text, span: LineSpan) -> Option<usize> {
        let mut next = span.next?;
        // Folds can touch (one ends where the next starts), so the line
        // after one fold can start inside the next.
        while let Some(f) = self.hiding(next) {
            next = t.line_span(f.end).next?;
        }
        Some(next)
    }

    /// Where the visible line before the one starting at `start` starts,
    /// skipping hidden lines; `None` at the top.
    #[must_use]
    pub fn prev_line(&self, t: &dyn Text, start: usize) -> Option<usize> {
        let mut prev = t.line_span(start.checked_sub(1)?).start;
        while let Some(f) = self.hiding(prev) {
            prev = t.line_span(f.start).start;
        }
        Some(prev)
    }

    /// The visible line containing `pos`: its own, or the first line of
    /// the fold hiding it.
    #[must_use]
    pub fn visible_line(&self, t: &dyn Text, pos: usize) -> LineSpan {
        let mut at = pos;
        while let Some(f) = self.hiding(at) {
            at = f.start;
        }
        t.line_span(at)
    }

    /// Move the folds through `edits`: the edits of one change, in
    /// descending order with offsets in the text before any of them, as
    /// [`crate::Buffer::replace_sorted`] returns them and
    /// [`crate::Selections::map_through`] takes them. A fold an edit
    /// touches opens (see [`touches`]); the rest shift by how much the
    /// edits before them grew the text. One pass over each list, and no
    /// allocation unless a fold opens.
    pub fn apply_edits(&mut self, edits: &[Edit]) {
        if edits.is_empty() || (self.active.is_empty() && self.inner.is_empty()) {
            return;
        }
        let opened_any = map_list(&mut self.active, edits);
        let opened_inner = map_list(&mut self.inner, edits);
        if opened_any || opened_inner {
            // A fold that opened leaves the folds kept inside it without
            // an active fold around them.
            self.promote();
        }
    }

    /// Remove the active fold `f` and bring back the folds kept in it.
    fn remove_active(&mut self, f: &Range<usize>) {
        if let Some(i) = self.active.iter().position(|a| a == f) {
            self.active.remove(i);
            self.promote();
        }
    }

    /// Make every inner fold no longer inside an active one active, the
    /// outermost first, keeping the ones inside those as inner.
    fn promote(&mut self) {
        let mut i = 0;
        while i < self.inner.len() {
            let f = self.inner[i].clone();
            let at = self.active.partition_point(|a| a.end <= f.start);
            let inside = self.active.get(at).is_some_and(|a| contains(a, &f));
            if inside {
                i += 1;
            } else {
                self.inner.remove(i);
                // The inner list is sorted outermost first, so nothing
                // active overlaps this one yet.
                self.active.insert(at, f);
            }
        }
    }
}

/// Whether two folds overlap without one containing the other.
fn crosses_range(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end && !contains(a, b) && !contains(b, a)
}

/// Whether `outer` covers all of `inner`.
fn contains(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Insert into a list sorted by start, then end descending.
fn insert_inner(list: &mut Vec<Range<usize>>, f: Range<usize>) {
    let key = |r: &Range<usize>| (r.start, std::cmp::Reverse(r.end));
    let at = list.partition_point(|r| key(r) < key(&f));
    list.insert(at, f);
}

/// Whether the edit replacing `old` (a range in the text before it)
/// touches the fold `f`: it is not wholly before the fold's hidden text
/// (`old.end <= f.start`) and not wholly after it (`old.start > f.end`).
/// So typing at the end of the first line, which is visible, shifts the
/// fold; typing in a hidden line, or deleting the line break after the
/// last one, opens it.
#[must_use]
pub fn touches(f: &Range<usize>, old: &Range<usize>) -> bool {
    old.end > f.start && old.start <= f.end
}

/// Map one sorted list of folds through descending `edits`, dropping the
/// ones touched. Returns whether any were dropped.
///
/// Folds in the inner list nest, so their starts rise but their ends do
/// not; the pointer over edits wholly before a fold depends only on the
/// fold's start, which is why one pass works for both lists. The scan for
/// a touching edit starts at that pointer and stops at the first edit
/// past the fold, so a fold costs one step plus the edits inside it.
fn map_list(list: &mut Vec<Range<usize>>, edits: &[Edit]) -> bool {
    // Edits in text order are `edits` reversed.
    let n = edits.len();
    let at = |i: usize| &edits[n - 1 - i];
    let mut before = 0; // edits wholly before the current fold
    let mut shift: isize = 0;
    let mut dropped = false;
    list.retain_mut(|f| {
        while before < n && at(before).old_range().end <= f.start {
            shift += at(before).growth();
            before += 1;
        }
        let mut j = before;
        while j < n && at(j).start <= f.end {
            if touches(f, &at(j).old_range()) {
                dropped = true;
                return false;
            }
            j += 1;
        }
        f.start = f.start.saturating_add_signed(shift);
        f.end = f.end.saturating_add_signed(shift);
        true
    });
    dropped
}

/// The fold covering the lines from the one containing `first` to the
/// one containing `last`, the first staying visible; `None` unless `last`
/// is on a later line.
#[must_use]
pub fn lines_range(t: &dyn Text, first: usize, last: usize) -> Option<Range<usize>> {
    let a = t.line_span(first);
    let b = t.line_span(last);
    (b.start > a.start).then_some(a.end..b.end)
}

/// Whether a line is only spaces and tabs.
fn is_blank(line: &str) -> bool {
    line.bytes().all(|b| b == b' ' || b == b'\t')
}

/// How wide a line's leading whitespace is, with tabs to the next stop.
fn indent_width(line: &str, tab_width: usize) -> usize {
    let tab = tab_width.max(1);
    let mut col = 0;
    for b in line.bytes() {
        match b {
            b' ' => col += 1,
            b'\t' => col += tab - col % tab,
            _ => break,
        }
    }
    col
}

/// The indentation region starting at the line containing `pos`: the
/// lines after it indented deeper, with blank lines among and after them
/// belonging to it (the convention for Python and YAML), up to the next
/// line indented as little as it or less. `None` for a blank line, when
/// the next non-blank line is not deeper, or when the region runs past
/// `limit` lines, so that asking about a line in a huge file whose lines
/// all share one indentation costs `limit` lines, not the file.
#[must_use]
pub fn indent_region(
    t: &dyn Text,
    pos: usize,
    tab_width: usize,
    limit: usize,
) -> Option<Range<usize>> {
    let head = t.line_span(pos);
    let text = t.slice(head.start..head.end);
    if is_blank(&text) {
        return None;
    }
    let level = indent_width(&text, tab_width);
    let mut last: Option<LineSpan> = None;
    let mut deeper = false;
    let mut next = head.next;
    let mut seen = 0;
    while let Some(start) = next {
        seen += 1;
        if seen > limit {
            return None;
        }
        let span = t.line_span(start);
        let line = t.slice(span.start..span.end);
        if !is_blank(&line) {
            if indent_width(&line, tab_width) <= level {
                break;
            }
            deeper = true;
        }
        last = Some(span);
        next = span.next;
    }
    let last = last.filter(|_| deeper)?;
    Some(head.end..last.end)
}

/// The bracket region starting at the line containing `pos`: from the
/// last bracket on the line that opens and closes on a later line, to the
/// line before the one holding its partner, so the closing line stays in
/// view (`fn f() {` folds down to its `}`). `None` if no bracket on the
/// line closes at least two lines further down within `limit` bytes.
/// `window` is scratch space the caller keeps.
#[must_use]
pub fn bracket_region(
    t: &dyn Text,
    pos: usize,
    limit: usize,
    window: &mut Vec<u8>,
) -> Option<Range<usize>> {
    let head = t.line_span(pos);
    let next = head.next?;
    // The openers on the line, last first; a few are enough.
    let mut tries = 0;
    let mut at = head.end;
    while at > head.start && tries < 16 {
        at -= 1;
        let Some(b) = t.byte_at(at) else { break };
        if !matches!(brackets::partner(b), Some((_, true))) {
            continue;
        }
        tries += 1;
        let Some((_, close)) = brackets::matching_with(t, at, limit, window) else {
            continue;
        };
        if close < next {
            // Closes on this line.
            continue;
        }
        let closing = t.line_span(close);
        return lines_range(t, head.start, closing.start.checked_sub(1)?);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, Content, RopeBuffer};
    use proptest::prelude::*;

    fn text(s: &str) -> RopeBuffer {
        RopeBuffer::from_text(s)
    }

    /// The active folds as pairs, which compare more simply in asserts.
    fn spans(f: &Folds) -> Vec<(usize, usize)> {
        f.active().iter().map(|r| (r.start, r.end)).collect()
    }

    /// Line starts reachable by walking down from the top through folds.
    fn walk_down(t: &dyn Text, f: &Folds) -> Vec<usize> {
        let mut out = vec![0];
        let mut span = t.line_span(0);
        while let Some(n) = f.next_line(t, span) {
            out.push(n);
            span = t.line_span(n);
        }
        out
    }

    /// The same, walking up from the last visible line.
    fn walk_up(t: &dyn Text, f: &Folds) -> Vec<usize> {
        let last = f.visible_line(t, t.len_bytes()).start;
        let mut out = vec![last];
        let mut at = last;
        while let Some(p) = f.prev_line(t, at) {
            out.push(p);
            at = p;
        }
        out.reverse();
        out
    }

    /// The oracle: every line start, minus the ones a fold hides, worked
    /// out from the string with no folding code involved.
    fn oracle(s: &str, folds: &[Range<usize>]) -> Vec<usize> {
        std::iter::once(0)
            .chain(s.match_indices('\n').map(|(i, _)| i + 1))
            .filter(|&ls| !folds.iter().any(|f| f.start < ls && ls <= f.end))
            .collect()
    }

    #[test]
    fn a_fold_hides_the_lines_after_its_first() {
        let t = text("ab\ncd\nef\ngh");
        let mut f = Folds::new();
        let r = lines_range(&t, 0, 6).unwrap();
        assert_eq!(r, 2..8);
        assert!(f.fold(r));
        assert_eq!(walk_down(&t, &f), vec![0, 9]);
        assert_eq!(walk_up(&t, &f), vec![0, 9]);
        assert_eq!(f.hiding(2), None, "end of the first line is visible");
        assert_eq!(f.hiding(3), Some(2..8));
        assert_eq!(f.hiding(8), Some(2..8));
        assert_eq!(f.hiding(9), None);
        assert_eq!(f.on_line(t.line_span(1)), Some(2..8));
        assert_eq!(f.visible_line(&t, 4).start, 0);
    }

    #[test]
    fn crlf_lines_fold_the_same_way() {
        let t = text("ab\r\ncd\r\nef");
        let r = lines_range(&t, 0, 4).unwrap();
        assert_eq!(r, 2..6);
        let mut f = Folds::new();
        f.fold(r);
        assert_eq!(walk_down(&t, &f), vec![0, 8]);
    }

    #[test]
    fn nested_folds_wait_inside_the_outer_one() {
        // 0 a / 2 b / 4 c / 6 d / 8 e
        let t = text("a\nb\nc\nd\ne");
        let mut f = Folds::new();
        assert!(f.fold(lines_range(&t, 2, 4).unwrap())); // hides c
        assert!(f.fold(lines_range(&t, 0, 6).unwrap())); // hides b..d
        assert_eq!(spans(&f), [(1, 7)]);
        assert_eq!(walk_down(&t, &f), vec![0, 8]);
        assert_eq!(f.unfold_line(t.line_span(0)), Some(1..7));
        assert_eq!(spans(&f), [(3, 5)], "the inner fold comes back");
        assert_eq!(walk_down(&t, &f), vec![0, 2, 6, 8]);
        // Folding inside an active one keeps it for later.
        let mut g = Folds::new();
        g.fold(1..7);
        assert!(g.fold(3..5));
        assert_eq!(spans(&g), [(1, 7)]);
        assert!(g.reveal(4), "revealing c opens both");
        assert!(g.is_empty());
    }

    #[test]
    fn crossing_and_repeated_folds_are_refused() {
        let mut f = Folds::new();
        assert!(f.fold(3..9));
        assert!(!f.fold(5..12));
        assert!(!f.fold(3..9));
        assert!(!f.fold(4..4));
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn edits_shift_or_open_folds() {
        let fold = || {
            let mut f = Folds::new();
            f.fold(2..8);
            f
        };
        let edit = |start: usize, removed: &str, inserted: &str| Edit {
            start,
            removed: Content::Bytes(removed.into()),
            inserted: Content::Bytes(inserted.into()),
        };
        // Typing at the end of the first line shifts the fold.
        let mut f = fold();
        f.apply_edits(&[edit(2, "", "xy")]);
        assert_eq!(spans(&f), [(4, 10)]);
        // Deleting before it shifts it back.
        let mut f = fold();
        f.apply_edits(&[edit(0, "ab", "")]);
        assert_eq!(spans(&f), [(0, 6)]);
        // Typing in a hidden line opens it.
        let mut f = fold();
        f.apply_edits(&[edit(4, "", "z")]);
        assert!(f.is_empty());
        // So does deleting the line break after the last hidden line.
        let mut f = fold();
        f.apply_edits(&[edit(8, "\n", "")]);
        assert!(f.is_empty());
        // An edit after it changes nothing.
        let mut f = fold();
        f.apply_edits(&[edit(9, "g", "G")]);
        assert_eq!(spans(&f), [(2, 8)]);
    }

    #[test]
    fn indentation_regions() {
        let s = "fn a\n    b\n\n    c\nd\n    e\n";
        let t = text(s);
        // a: lines b, blank, c.
        assert_eq!(indent_region(&t, 0, 4, 100), Some(4..17));
        assert_eq!(indent_region(&t, 5, 4, 100), None, "b has nothing deeper");
        // d: e and the empty last line after it.
        let d = s.find("d\n").unwrap();
        assert_eq!(indent_region(&t, d, 4, 100), Some(d + 1..s.len()));
        assert_eq!(indent_region(&t, 0, 4, 2), None, "past the limit");
        assert_eq!(indent_region(&t, 11, 4, 100), None, "blank line");
        // Tabs count to the next stop.
        let t = text("a\n\tb\nc");
        assert_eq!(indent_region(&t, 0, 4, 10), Some(1..4));
    }

    #[test]
    fn bracket_regions_keep_the_closer_in_view() {
        let s = "fn f() {\n    x();\n    y();\n}\n";
        let t = text(s);
        let mut w = Vec::new();
        assert_eq!(bracket_region(&t, 0, 1 << 16, &mut w), Some(8..26));
        // A bracket closing on the next line hides nothing.
        let t = text("f(\n)\n");
        assert_eq!(bracket_region(&t, 0, 1 << 16, &mut w), None);
        // One closing on its own line is skipped for an earlier one.
        let t = text("a { b() \nc\nd\n}");
        assert_eq!(bracket_region(&t, 0, 1 << 16, &mut w), Some(8..12));
    }

    /// Random text of short lines, some of them blank.
    fn lines() -> impl Strategy<Value = String> {
        prop::collection::vec("[ab ]{0,3}", 1..24).prop_map(|v| v.join("\n"))
    }

    fn line_starts(s: &str) -> Vec<usize> {
        std::iter::once(0)
            .chain(s.match_indices('\n').map(|(i, _)| i + 1))
            .collect()
    }

    proptest! {
        /// Walking through folds, either way, visits exactly the lines no
        /// active fold hides, whatever was folded and unfolded.
        #[test]
        fn visible_lines_match_the_oracle(
            s in lines(),
            ops in prop::collection::vec((any::<bool>(), 0usize..64, 0usize..64), 0..12),
        ) {
            let t = text(&s);
            let starts = line_starts(&s);
            let mut f = Folds::new();
            for (fold, a, b) in ops {
                let (a, b) = (starts[a % starts.len()], starts[b % starts.len()]);
                if fold {
                    if let Some(r) = lines_range(&t, a.min(b), a.max(b)) {
                        f.fold(r);
                    }
                } else {
                    f.unfold_line(t.line_span(a));
                    f.reveal(b);
                }
                let act = f.active().to_vec();
                prop_assert!(act.windows(2).all(|w| w[0].end <= w[1].start), "{act:?}");
                prop_assert!(f.inner.iter().all(|i| act.iter().any(|a| contains(a, i))));
                let want = oracle(&s, &act);
                prop_assert_eq!(walk_down(&t, &f), want.clone());
                prop_assert_eq!(walk_up(&t, &f), want);
            }
        }

        /// After edits, every surviving fold is where per-fold arithmetic,
        /// done one edit at a time, puts it; and a fold an edit touched is
        /// gone.
        #[test]
        fn edits_match_one_at_a_time_arithmetic(
            s in lines(),
            folds in prop::collection::vec((0usize..64, 0usize..64), 0..6),
            edits in prop::collection::vec((0usize..80, 0usize..3, "[ab\n]{0,3}"), 1..5),
        ) {
            let mut t = text(&s);
            let starts = line_starts(&s);
            let mut f = Folds::new();
            for (a, b) in folds {
                let (a, b) = (starts[a % starts.len()], starts[b % starts.len()]);
                if let Some(r) = lines_range(&t, a.min(b), a.max(b)) {
                    f.fold(r);
                }
            }
            // Non-overlapping edits in descending order.
            let mut ranges: Vec<(usize, usize, String)> = Vec::new();
            let mut floor = s.len() + 1;
            let mut sorted = edits;
            sorted.sort_by_key(|e| std::cmp::Reverse(e.0));
            for (at, len, ins) in sorted {
                let start = at.min(s.len());
                let end = (start + len).min(s.len());
                if end >= floor || (end == start && start == floor) {
                    continue;
                }
                floor = start;
                ranges.push((start, end, ins));
            }
            let mut edits = Vec::new();
            for (start, end, ins) in &ranges {
                let e = t.replace(*start..*end, ins).unwrap();
                edits.push(e);
            }
            // The oracle: each active fold on its own, edit by edit.
            let mut want: Vec<Range<usize>> = Vec::new();
            'folds: for fr in f.active() {
                let mut r = fr.clone();
                for e in &edits {
                    let old = e.old_range();
                    if touches(&r, &old) {
                        continue 'folds;
                    }
                    if old.end <= r.start {
                        r.start = r.start.saturating_add_signed(e.growth());
                        r.end = r.end.saturating_add_signed(e.growth());
                    }
                }
                want.push(r);
            }
            let had_inner = !f.inner.is_empty();
            f.apply_edits(&edits);
            if had_inner {
                // Opened folds bring back inner ones; only check the
                // survivors are all there.
                for w in &want {
                    prop_assert!(f.active().contains(w), "{w:?} in {:?}", f.active());
                }
            } else {
                prop_assert_eq!(f.active(), &want[..]);
            }
            let act = f.active().to_vec();
            prop_assert!(act.windows(2).all(|w| w[0].end <= w[1].start));
        }
    }
}
