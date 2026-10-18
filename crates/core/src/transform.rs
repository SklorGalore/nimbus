//! Changing the selected text (M11): letter case, sorting lines, removing
//! duplicate lines, trimming trailing whitespace.
//!
//! Case changes work on each selection's text; a bare cursor changes the
//! word under it. The line operations work on the lines each selection
//! touches, and with no selection on the whole text; the app bounds that
//! with [`crate::lines::Batch::check_size`], so a 4 GB file is never read
//! here.

use std::borrow::Cow;
use std::ops::Range;

use crate::lines::{Batch, Block, blocks};
use crate::{Selection, Selections, Text, movement};

/// Which case to change the text to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// `HELLO WORLD`.
    Upper,
    /// `hello world`.
    Lower,
    /// `Hello World`: the first letter of each word up, the rest down.
    Title,
}

impl Case {
    fn apply(self, text: &str) -> String {
        match self {
            Self::Upper => text.to_uppercase(),
            Self::Lower => text.to_lowercase(),
            Self::Title => {
                let mut out = String::with_capacity(text.len());
                let mut start = true;
                for c in text.chars() {
                    if c.is_alphanumeric() {
                        if start {
                            out.extend(c.to_uppercase());
                        } else {
                            out.extend(c.to_lowercase());
                        }
                        start = false;
                    } else {
                        out.push(c);
                        start = c != '\'';
                    }
                }
                out
            }
        }
    }
}

/// Change the case of each selection's text, or of the word under a bare
/// cursor. The selections keep covering the same text, which may have
/// changed length (`ß` to `SS`).
#[must_use]
pub fn change_case<'t>(t: &'t dyn Text, sels: &Selections, case: Case) -> Batch<'t> {
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    let mut after = Vec::new();
    let mut delta = 0isize;
    for s in sels.iter() {
        let r = if s.is_empty() {
            movement::word_at(t, s.head)
        } else {
            s.range()
        };
        let text = t.slice(r.clone());
        let changed = case.apply(&text);
        let start = r.start.saturating_add_signed(delta);
        if changed != *text {
            delta += changed.len().cast_signed() - r.len().cast_signed();
            items.push((r.clone(), Cow::Owned(changed)));
        }
        let end = r.end.saturating_add_signed(delta);
        after.push(if s.is_empty() {
            Selection::cursor(s.head.saturating_add_signed(delta).min(end).max(start))
        } else {
            directed(start..end, *s)
        });
    }
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
}

/// What a line operation does with the lines it is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    /// Sort them, byte-wise, keeping equal lines in order.
    Sort,
    /// Sort them the other way round.
    SortReversed,
    /// Keep the first of each run of equal lines, anywhere in the block.
    Dedupe,
}

/// Rewrite the lines each selection touches (every line when nothing is
/// selected) with `op`. The selections end up covering their lines whole.
#[must_use]
pub fn rewrite_lines<'t>(t: &'t dyn Text, sels: &Selections, op: Lines, ending: &str) -> Batch<'t> {
    let whole = sels.count() == 1 && sels.primary().is_empty();
    let groups: Vec<(Block, Vec<usize>)> = if whole {
        vec![(Block::of(t, 0..t.len_bytes()), vec![0])]
    } else {
        blocks(t, sels, false)
    };
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    let mut after = Vec::new();
    let mut delta = 0isize;
    for (b, members) in groups {
        let text = t.slice(b.start..b.end);
        let mut lines: Vec<&str> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        match op {
            Lines::Sort => lines.sort_unstable(),
            Lines::SortReversed => {
                lines.sort_unstable();
                lines.reverse();
            }
            Lines::Dedupe => {
                let mut seen = std::collections::HashSet::new();
                lines.retain(|l| seen.insert(*l));
            }
        }
        let new = lines.join(ending);
        let start = b.start.saturating_add_signed(delta);
        if new != *text {
            delta += new.len().cast_signed() - text.len().cast_signed();
            items.push((b.start..b.end, Cow::Owned(new)));
        }
        let end = b.end.saturating_add_signed(delta);
        let like = sels.as_slice()[members[0]];
        after.push(directed(start..end, like));
    }
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
}

/// Remove spaces and tabs from the end of every line each selection
/// touches, or of every line when nothing is selected. The selections
/// stay on their text.
#[must_use]
pub fn trim_trailing<'t>(t: &'t dyn Text, sels: &Selections) -> Batch<'t> {
    let whole = sels.count() == 1 && sels.primary().is_empty();
    let groups = if whole {
        vec![(Block::of(t, 0..t.len_bytes()), Vec::new())]
    } else {
        blocks(t, sels, false)
    };
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    for (b, _) in groups {
        let mut pos = b.start;
        loop {
            let span = t.line_span(pos);
            if let Some(r) = trailing_whitespace(t, span.start, span.end) {
                items.push((r, Cow::Borrowed("")));
            }
            match span.next {
                Some(n) if n <= b.end => pos = n,
                _ => break,
            }
        }
    }
    Batch { items, after: None }
}

/// The trailing spaces and tabs of the line `start..end`, if any. Reads
/// from the end of the line, so a long line costs its tail, not its length.
#[must_use]
pub fn trailing_whitespace(t: &dyn Text, start: usize, end: usize) -> Option<Range<usize>> {
    let mut from = end;
    let mut probe = end;
    while probe > start {
        let chunk_start = probe.saturating_sub(crate::long_line::NEAR).max(start);
        let chunk = t.slice(chunk_start..probe);
        let kept = chunk.trim_end_matches([' ', '\t']).len();
        from = chunk_start + kept;
        if kept > 0 {
            break;
        }
        probe = chunk_start;
    }
    (from < end).then_some(from..end)
}

fn directed(range: Range<usize>, like: Selection) -> Selection {
    if like.head < like.anchor {
        Selection {
            anchor: range.end,
            head: range.start,
        }
    } else {
        Selection {
            anchor: range.start,
            head: range.end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bias, RopeBuffer, multi};

    fn cursors(at: &[usize]) -> Selections {
        Selections::from_vec(
            at.iter().map(|&p| Selection::cursor(p)).collect(),
            at.len() - 1,
        )
    }

    fn sel(a: usize, h: usize) -> Selections {
        Selections::single(Selection { anchor: a, head: h })
    }

    fn apply(text: &str, before: &Selections, batch: Batch<'_>) -> (String, Selections) {
        let mut b = RopeBuffer::from_text(text);
        let mut items = batch.items;
        let edits = multi::apply_each(&mut b, &mut items).unwrap();
        let after = batch.after.unwrap_or_else(|| {
            let mut s = before.clone();
            s.map_through(&edits, Bias::Before, Bias::After);
            s
        });
        (b.slice(0..b.len_bytes()).into_owned(), after)
    }

    #[test]
    fn case_changes_with_one_and_three_cursors() {
        let text = "hello wORLD straße";
        let t = RopeBuffer::from_text(text);
        let (out, s) = apply(text, &sel(0, 11), change_case(&t, &sel(0, 11), Case::Upper));
        assert_eq!(out, "HELLO WORLD straße");
        assert_eq!(
            s.primary(),
            Selection {
                anchor: 0,
                head: 11
            }
        );
        let (out, _) = apply(text, &sel(0, 11), change_case(&t, &sel(0, 11), Case::Title));
        assert_eq!(out, "Hello World straße");
        // Bare cursors change the word under them; ß grows to SS and the
        // later cursor shifts with it.
        let three = cursors(&[1, 7, 14]);
        let (out, s) = apply(text, &three, change_case(&t, &three, Case::Upper));
        assert_eq!(out, "HELLO WORLD STRASSE");
        assert_eq!(s.iter().map(|s| s.head).collect::<Vec<_>>(), [1, 7, 14]);
        let (out, _) = apply(text, &sel(6, 11), change_case(&t, &sel(6, 11), Case::Lower));
        assert_eq!(out, "hello world straße");
        assert_eq!(
            Case::Title.apply("it's o'neil-smith 3rd"),
            "It's O'neil-Smith 3rd"
        );
    }

    #[test]
    fn sort_dedupe_and_trim_with_one_and_three_cursors() {
        let text = "b\na\nb\nc  \n";
        let t = RopeBuffer::from_text(text);
        // No selection: every line; the final line ending stays.
        let (out, _) = apply(
            text,
            &cursors(&[0]),
            rewrite_lines(&t, &cursors(&[0]), Lines::Sort, "\n"),
        );
        assert_eq!(out, "a\nb\nb\nc  \n");
        let (out, s) = apply(
            text,
            &sel(0, 5),
            rewrite_lines(&t, &sel(0, 5), Lines::Sort, "\n"),
        );
        assert_eq!(out, "a\nb\nb\nc  \n");
        assert_eq!(s.primary(), Selection { anchor: 0, head: 5 });
        let (out, _) = apply(
            text,
            &sel(0, 5),
            rewrite_lines(&t, &sel(0, 5), Lines::SortReversed, "\n"),
        );
        assert_eq!(out, "b\nb\na\nc  \n");
        let (out, s) = apply(
            text,
            &sel(0, 5),
            rewrite_lines(&t, &sel(0, 5), Lines::Dedupe, "\n"),
        );
        assert_eq!(out, "b\na\nc  \n");
        assert_eq!(s.primary(), Selection { anchor: 0, head: 3 });
        // Three cursors on three separate lines: each line is its own
        // block, so sorting changes nothing but the selections cover
        // their lines.
        let three = cursors(&[0, 2, 6]);
        let (out, s) = apply(text, &three, rewrite_lines(&t, &three, Lines::Sort, "\n"));
        assert_eq!(out, text);
        assert_eq!(s.count(), 3);
        // Trimming: the selection's lines, or all of them.
        let (out, _) = apply(text, &sel(0, 3), trim_trailing(&t, &sel(0, 3)));
        assert_eq!(out, text, "no trailing whitespace on those lines");
        let (out, _) = apply(text, &cursors(&[0]), trim_trailing(&t, &cursors(&[0])));
        assert_eq!(out, "b\na\nb\nc\n");
        let three = cursors(&[0, 2, 7]);
        let (out, s) = apply(text, &three, trim_trailing(&t, &three));
        assert_eq!(out, "b\na\nb\nc\n");
        assert_eq!(s.iter().map(|s| s.head).collect::<Vec<_>>(), [0, 2, 7]);
        assert_eq!(trailing_whitespace(&t, 6, 9), Some(7..9));
        assert_eq!(trailing_whitespace(&t, 0, 1), None);
    }
}
