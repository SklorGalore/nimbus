//! Rows that align two or three texts side by side (M9 task 5).
//!
//! A compare or merge view draws each text in its own column, with equal
//! lines on the same row and a change's lines next to each other, the
//! shorter side padded with filler rows. Long unchanged runs fold to one
//! "N unchanged lines" row with a few lines of context either side.
//!
//! Nothing here is per line. The texts are described by *segments*: an
//! equal run (one row per line, or a fixed few when folded) or a change
//! (as many rows as its longest side). A prefix sum of segment row counts
//! and a binary search take a row to its segment in O(log segments), and
//! a line to its row the same way; memory is O(segments), which for two
//! 100 MB files is a few thousand entries. Toggling a fold recomputes the
//! prefix sum, O(segments).
//!
//! The model is generic over the tag a change carries: the compare view
//! uses [`Change`], the merge view [`crate::merge::Kind`].

use std::ops::Range;

use crate::diff::Hunk;
use crate::merge::{Chunk, Kind};

/// How equal runs fold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Folding {
    /// Equal lines kept visible on each side of a fold.
    pub context: usize,
    /// An equal run folds only when at least this many lines would hide.
    pub min_hidden: usize,
}

impl Default for Folding {
    fn default() -> Self {
        Self {
            context: 3,
            min_hidden: 3,
        }
    }
}

impl Folding {
    /// The shortest equal run that folds.
    fn threshold(self) -> usize {
        self.context
            .saturating_mul(2)
            .saturating_add(self.min_hidden)
    }
}

/// What a two-way change is, from the shape of its hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Lines only in the new text.
    Added,
    /// Lines only in the old text.
    Removed,
    /// Lines replaced.
    Changed,
}

impl Change {
    fn of(h: &Hunk) -> Self {
        match (h.old.is_empty(), h.new.is_empty()) {
            (true, _) => Self::Added,
            (_, true) => Self::Removed,
            _ => Self::Changed,
        }
    }
}

/// At most this many columns.
pub const MAX_COLUMNS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SegmentKind<T> {
    /// The same `len` lines in every column, starting at `lines[c]`.
    Equal { len: usize, folded: bool },
    /// A change: each column shows its own range.
    Change { tag: T, number: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment<T> {
    kind: SegmentKind<T>,
    /// Line range per column (only the first `columns` used).
    lines: [Range<usize>; MAX_COLUMNS],
}

/// What a column shows on a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    /// This line of the column's text.
    Line(usize),
    /// Nothing: the other column has more lines in this change.
    Filler,
    /// A fold hiding lines of the column.
    Fold {
        /// The first hidden line.
        first: usize,
        /// How many lines are hidden.
        hidden: usize,
    },
}

/// One row across the columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row<T> {
    /// Per column (only the first `columns` are meaningful).
    pub cells: [Cell; MAX_COLUMNS],
    /// The change this row is part of, with its number from 0, or `None`
    /// on an equal or folded row.
    pub change: Option<(T, usize)>,
}

/// The aligned rows of two or three texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rows<T> {
    columns: usize,
    folding: Folding,
    segments: Vec<Segment<T>>,
    /// First row of each segment, plus the total at the end.
    starts: Vec<usize>,
    changes: usize,
}

impl Rows<Change> {
    /// Rows for a two-way diff of `old_lines` against `new_lines` lines.
    #[must_use]
    pub fn two_way(hunks: &[Hunk], old_lines: usize, new_lines: usize, folding: Folding) -> Self {
        let mut segments = Vec::with_capacity(2 * hunks.len() + 1);
        let (mut o, mut n) = (0, 0);
        let mut number = 0;
        for h in hunks {
            push_equal(
                &mut segments,
                [o..h.old.start, n..h.new.start, 0..0],
                h.old.start - o,
                folding,
            );
            segments.push(Segment {
                kind: SegmentKind::Change {
                    tag: Change::of(h),
                    number,
                },
                lines: [h.old.clone(), h.new.clone(), 0..0],
            });
            number += 1;
            o = h.old.end;
            n = h.new.end;
        }
        push_equal(
            &mut segments,
            [o..old_lines, n..new_lines, 0..0],
            old_lines.saturating_sub(o),
            folding,
        );
        Self::build(2, folding, segments, number)
    }
}

impl Rows<Kind> {
    /// Rows for a three-way merge: base, ours, theirs.
    #[must_use]
    pub fn three_way(chunks: &[Chunk], folding: Folding) -> Self {
        let mut segments = Vec::with_capacity(chunks.len());
        let mut number = 0;
        for c in chunks {
            let lines = [c.base.clone(), c.ours.clone(), c.theirs.clone()];
            if c.kind == Kind::Same {
                push_equal(&mut segments, lines, c.base.len(), folding);
            } else {
                segments.push(Segment {
                    kind: SegmentKind::Change {
                        tag: c.kind,
                        number,
                    },
                    lines,
                });
                number += 1;
            }
        }
        Self::build(3, folding, segments, number)
    }
}

/// An equal run of `len` lines, folded from the start when long enough.
fn push_equal<T>(
    segments: &mut Vec<Segment<T>>,
    lines: [Range<usize>; MAX_COLUMNS],
    len: usize,
    folding: Folding,
) {
    if len == 0 {
        return;
    }
    let folded = len >= folding.threshold();
    segments.push(Segment {
        kind: SegmentKind::Equal { len, folded },
        lines,
    });
}

impl<T: Copy> Rows<T> {
    fn build(columns: usize, folding: Folding, segments: Vec<Segment<T>>, changes: usize) -> Self {
        let mut rows = Self {
            columns,
            folding,
            segments,
            starts: Vec::new(),
            changes,
        };
        rows.reindex();
        rows
    }

    fn reindex(&mut self) {
        self.starts.clear();
        self.starts.reserve(self.segments.len() + 1);
        let mut row = 0;
        for s in &self.segments {
            self.starts.push(row);
            row += self.segment_rows(s);
        }
        self.starts.push(row);
    }

    fn segment_rows(&self, s: &Segment<T>) -> usize {
        match &s.kind {
            SegmentKind::Equal { len, folded: false } => *len,
            SegmentKind::Equal { folded: true, .. } => 2 * self.folding.context + 1,
            SegmentKind::Change { .. } => s.lines[..self.columns]
                .iter()
                .map(ExactSizeIterator::len)
                .max()
                .unwrap_or(0),
        }
    }

    /// How many columns: 2 or 3.
    #[must_use]
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Total rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.starts.last().copied().unwrap_or(0)
    }

    /// No rows at all (both texts empty).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many changes there are.
    #[must_use]
    pub fn changes(&self) -> usize {
        self.changes
    }

    /// The segment holding `row`, and the row's offset in it.
    fn locate(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.len() {
            return None;
        }
        // `starts` is sorted; the last entry equal to or below `row`.
        let i = self.starts.partition_point(|&s| s <= row) - 1;
        Some((i, row - self.starts[i]))
    }

    /// What each column shows on `row`. `None` past the end.
    #[must_use]
    pub fn at(&self, row: usize) -> Option<Row<T>> {
        let (i, off) = self.locate(row)?;
        let s = &self.segments[i];
        let mut cells = [Cell::Filler; MAX_COLUMNS];
        let change = match &s.kind {
            SegmentKind::Equal { len, folded } => {
                let ctx = self.folding.context;
                for (c, cell) in cells.iter_mut().enumerate().take(self.columns) {
                    let start = s.lines[c].start;
                    *cell = if !*folded || off < ctx {
                        Cell::Line(start + off)
                    } else if off == ctx {
                        Cell::Fold {
                            first: start + ctx,
                            hidden: len - 2 * ctx,
                        }
                    } else {
                        Cell::Line(start + len - (2 * ctx + 1 - off))
                    };
                }
                None
            }
            SegmentKind::Change { tag, number } => {
                for (c, cell) in cells.iter_mut().enumerate().take(self.columns) {
                    let r = &s.lines[c];
                    *cell = if off < r.len() {
                        Cell::Line(r.start + off)
                    } else {
                        Cell::Filler
                    };
                }
                Some((*tag, *number))
            }
        };
        Some(Row { cells, change })
    }

    /// The row showing line `line` of column `column`: its own row, or
    /// the fold row hiding it. Lines past the end give the last row.
    #[must_use]
    pub fn row_of(&self, column: usize, line: usize) -> usize {
        if self.segments.is_empty() {
            return 0;
        }
        // Segments' line starts in a column are non-decreasing; find the
        // last segment starting at or before `line` that has lines in
        // this column (empty ranges share a start with their neighbour).
        let mut i = self
            .segments
            .partition_point(|s| s.lines[column].start <= line);
        i = i.saturating_sub(1);
        while i > 0
            && self.segments[i].lines[column].is_empty()
            && self.segments[i].lines[column].start > line
        {
            i -= 1;
        }
        // Prefer a segment that actually holds the line.
        while i > 0
            && !self.segments[i].lines[column].contains(&line)
            && self.segments[i - 1].lines[column].contains(&line)
        {
            i -= 1;
        }
        let s = &self.segments[i];
        let off = line.saturating_sub(s.lines[column].start);
        let rows = self.segment_rows(s);
        let row_off = match &s.kind {
            SegmentKind::Equal { len, folded: true } => {
                let ctx = self.folding.context;
                if off < ctx {
                    off
                } else if off >= len - ctx {
                    2 * ctx + 1 - (len - off)
                } else {
                    ctx
                }
            }
            _ => off.min(rows.saturating_sub(1)),
        };
        self.starts[i] + row_off
    }

    /// Unfold or refold the equal run on `row`, if it is one that can
    /// fold. Returns whether anything changed.
    pub fn toggle_fold(&mut self, row: usize) -> bool {
        let Some((i, _)) = self.locate(row) else {
            return false;
        };
        let threshold = self.folding.threshold();
        let SegmentKind::Equal { len, folded } = &mut self.segments[i].kind else {
            return false;
        };
        if *len < threshold {
            return false;
        }
        *folded = !*folded;
        self.reindex();
        true
    }

    /// Fold every equal run that is long enough, or unfold all.
    pub fn fold_all(&mut self, fold: bool) {
        let min = self.folding.threshold();
        for s in &mut self.segments {
            if let SegmentKind::Equal { len, folded } = &mut s.kind
                && *len >= min
            {
                *folded = fold;
            }
        }
        self.reindex();
    }

    /// First row of change number `n` (from 0), if it exists.
    #[must_use]
    pub fn change_row(&self, n: usize) -> Option<usize> {
        if n >= self.changes {
            return None;
        }
        // Change segments are in order; the n-th is found by a binary
        // search over the segments' change numbers.
        let i = self.segments.partition_point(|s| match &s.kind {
            SegmentKind::Change { number, .. } => *number < n,
            SegmentKind::Equal { .. } => true,
        });
        // `partition_point` lands on the first segment that is a change
        // with number >= n, but equal runs count as "before", so step
        // over any equal run.
        let mut i = i;
        while i < self.segments.len() && matches!(self.segments[i].kind, SegmentKind::Equal { .. })
        {
            i += 1;
        }
        (i < self.segments.len()).then(|| self.starts[i])
    }

    /// The change number on or after `row`, and its first row.
    #[must_use]
    pub fn next_change(&self, row: usize) -> Option<(usize, usize)> {
        let (i, _) = self
            .locate(row)
            .or_else(|| self.segments.len().checked_sub(1).map(|i| (i, 0)))?;
        self.segments[i..]
            .iter()
            .zip(&self.starts[i..])
            .find_map(|(s, &start)| match &s.kind {
                SegmentKind::Change { number, .. }
                    if start >= row || start + self.segment_rows(s) > row =>
                {
                    Some((*number, start))
                }
                _ => None,
            })
    }

    /// The change number strictly before `row`, and its first row.
    #[must_use]
    pub fn prev_change(&self, row: usize) -> Option<(usize, usize)> {
        let (i, _) = self.locate(row).unwrap_or((self.segments.len(), 0));
        self.segments[..i]
            .iter()
            .zip(&self.starts[..i])
            .rev()
            .find_map(|(s, &start)| match &s.kind {
                SegmentKind::Change { number, .. } => Some((*number, start)),
                SegmentKind::Equal { .. } => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_FOLD: Folding = Folding {
        context: 1,
        min_hidden: usize::MAX,
    };

    fn two(hunks: &[Hunk], old: usize, new: usize) -> Rows<Change> {
        Rows::two_way(hunks, old, new, NO_FOLD)
    }

    fn cells(rows: &Rows<Change>) -> Vec<(Cell, Cell)> {
        (0..rows.len())
            .map(|r| {
                let row = rows.at(r).unwrap();
                (row.cells[0], row.cells[1])
            })
            .collect()
    }

    #[test]
    fn equal_lines_share_rows_and_changes_pad_with_filler() {
        // old: a b c d ; new: a X Y c  (b -> X Y, d removed)
        let rows = two(
            &[
                Hunk {
                    old: 1..2,
                    new: 1..3,
                },
                Hunk {
                    old: 3..4,
                    new: 4..4,
                },
            ],
            4,
            4,
        );
        assert_eq!(rows.len(), 5);
        assert_eq!(
            cells(&rows),
            vec![
                (Cell::Line(0), Cell::Line(0)),
                (Cell::Line(1), Cell::Line(1)),
                (Cell::Filler, Cell::Line(2)),
                (Cell::Line(2), Cell::Line(3)),
                (Cell::Line(3), Cell::Filler),
            ]
        );
        assert_eq!(rows.at(1).unwrap().change, Some((Change::Changed, 0)));
        assert_eq!(rows.at(4).unwrap().change, Some((Change::Removed, 1)));
        assert_eq!(rows.at(0).unwrap().change, None);
        assert_eq!(rows.changes(), 2);
    }

    #[test]
    fn rows_map_back_to_lines() {
        let rows = two(
            &[
                Hunk {
                    old: 1..2,
                    new: 1..3,
                },
                Hunk {
                    old: 3..4,
                    new: 4..4,
                },
            ],
            4,
            4,
        );
        for line in 0..4 {
            let r = rows.row_of(0, line);
            assert_eq!(
                rows.at(r).unwrap().cells[0],
                Cell::Line(line),
                "old line {line}"
            );
            let r = rows.row_of(1, line);
            assert_eq!(
                rows.at(r).unwrap().cells[1],
                Cell::Line(line),
                "new line {line}"
            );
        }
        // Past the end: the last row.
        assert_eq!(rows.row_of(1, 99), rows.len() - 1);
    }

    #[test]
    fn long_equal_runs_fold_with_context() {
        let folding = Folding {
            context: 2,
            min_hidden: 2,
        };
        // 10 equal lines, a change, 3 equal lines (too short to fold).
        let rows = Rows::two_way(
            &[Hunk {
                old: 10..11,
                new: 10..11,
            }],
            14,
            14,
            folding,
        );
        // 2 + fold + 2, change, 3
        assert_eq!(rows.len(), 5 + 1 + 3);
        assert_eq!(
            rows.at(2).unwrap().cells[0],
            Cell::Fold {
                first: 2,
                hidden: 6
            }
        );
        assert_eq!(rows.at(3).unwrap().cells[0], Cell::Line(8));
        assert_eq!(rows.at(4).unwrap().cells[1], Cell::Line(9));
        assert_eq!(rows.at(5).unwrap().cells[0], Cell::Line(10));
        // A hidden line maps to the fold row; a context line to itself.
        assert_eq!(rows.row_of(0, 5), 2);
        assert_eq!(rows.row_of(0, 9), 4);
        assert_eq!(rows.row_of(1, 1), 1);
        // Unfold, then every line has its own row.
        assert!(rows.clone().toggle_fold(2));
        let mut open = rows.clone();
        open.toggle_fold(2);
        assert_eq!(open.len(), 14);
        assert_eq!(open.row_of(0, 5), 5);
        assert!(!open.toggle_fold(13), "a short run does not fold");
        open.fold_all(true);
        assert_eq!(open, rows);
    }

    #[test]
    fn change_navigation() {
        let rows = two(
            &[
                Hunk {
                    old: 1..2,
                    new: 1..2,
                },
                Hunk {
                    old: 5..5,
                    new: 5..7,
                },
                Hunk {
                    old: 8..9,
                    new: 10..10,
                },
            ],
            9,
            10,
        );
        assert_eq!(rows.change_row(0), Some(1));
        assert_eq!(rows.change_row(1), Some(5));
        assert_eq!(rows.change_row(2), Some(10));
        assert_eq!(rows.change_row(3), None);
        assert_eq!(rows.next_change(0), Some((0, 1)));
        assert_eq!(rows.next_change(2), Some((1, 5)));
        assert_eq!(
            rows.next_change(6),
            Some((1, 5)),
            "inside a change: that change"
        );
        assert_eq!(rows.prev_change(6), Some((0, 1)));
        assert_eq!(rows.prev_change(1), None);
        assert_eq!(rows.next_change(11), None);
    }

    #[test]
    fn three_columns_from_merge_chunks() {
        let chunks = [
            Chunk {
                kind: Kind::Same,
                base: 0..1,
                ours: 0..1,
                theirs: 0..1,
            },
            Chunk {
                kind: Kind::Conflict,
                base: 1..2,
                ours: 1..3,
                theirs: 1..2,
            },
            Chunk {
                kind: Kind::Same,
                base: 2..3,
                ours: 3..4,
                theirs: 2..3,
            },
        ];
        let rows = Rows::three_way(&chunks, NO_FOLD);
        assert_eq!(rows.columns(), 3);
        assert_eq!(rows.len(), 4);
        let r = rows.at(2).unwrap();
        assert_eq!(r.cells, [Cell::Filler, Cell::Line(2), Cell::Filler]);
        assert_eq!(r.change, Some((Kind::Conflict, 0)));
        assert_eq!(rows.row_of(1, 3), 3);
        assert_eq!(rows.row_of(2, 2), 3);
    }

    #[test]
    fn empty_texts() {
        let rows = two(&[], 0, 0);
        assert!(rows.is_empty());
        assert_eq!(rows.at(0), None);
        assert_eq!(rows.row_of(0, 0), 0);
        assert_eq!(rows.next_change(0), None);
    }
}
