//! Growing a selection outwards (M11): from the cursor to the word, then
//! the quoted string or bracket pair around it, then the line, then the
//! whole text; and shrinking back the same way.
//!
//! Without a syntax tree the structure comes from counting: the nearest
//! unmatched opening bracket before the selection and its partner after,
//! or a pair of quotes on the line around it. Both are approximations (a
//! bracket in a string counts) that the tree will refine in M14. Each
//! step picks the smallest candidate that strictly contains the current
//! selection, so nesting works out by itself: `(a (b))` from `b` grows to
//! `(b)`, then `a (b)`, then the outer pair.
//!
//! Shrinking retraces the steps: the document keeps the selections each
//! expansion started from on a stack, and any other movement clears it.

use std::ops::Range;

use crate::long_line::{NEAR, around};
use crate::{Selection, Text, brackets, movement};

/// The quote characters that can enclose a selection.
const QUOTES: [u8; 3] = *b"\"'`";

/// The selection one step larger than `sel`, or `None` when it already
/// covers everything. `limit` bounds how far brackets are looked for.
#[must_use]
pub fn expand(t: &dyn Text, sel: Selection, limit: usize) -> Option<Selection> {
    let r = sel.range();
    let mut best: Option<Range<usize>> = None;
    let mut offer = |c: Range<usize>| {
        if c.start <= r.start && c.end >= r.end && c != r {
            let better = best.as_ref().is_none_or(|b| c.len() < b.len());
            if better {
                best = Some(c);
            }
        }
    };
    // The word under the cursor, or under the start of the selection.
    let word = movement::word_at(t, r.start);
    if !word.is_empty() {
        offer(word);
    }
    for inner in quoted(t, r.clone()).into_iter().flatten() {
        offer(inner.start + 1..inner.end - 1);
        offer(inner);
    }
    if let Some(pair) = enclosing_brackets(t, r.clone(), limit) {
        offer(pair.start + 1..pair.end - 1);
        offer(pair);
    }
    let span = t.line_span(r.start);
    let line_end = if r.end > span.end {
        t.line_span(r.end).end
    } else {
        span.end
    };
    offer(span.start..line_end);
    offer(0..t.len_bytes());
    best.map(|range| Selection {
        anchor: range.start,
        head: range.end,
    })
}

/// For each quote character: the quoted run on the line around `r`, from
/// the quote before `r` to the one after, when there is one each side and
/// the one before opens (an even number of that quote precede it on the
/// line).
fn quoted(t: &dyn Text, r: Range<usize>) -> [Option<Range<usize>>; 3] {
    let span = t.line_span(r.start);
    if r.end > span.end {
        return [None, None, None];
    }
    let (line, base) = around(t, span, r.start);
    let (a, b) = (r.start - base, (r.end - base).min(line.len()));
    let bytes = line.as_bytes();
    QUOTES.map(|q| {
        let before = bytes[..a].iter().rposition(|&c| c == q)?;
        let opens = bytes[..before]
            .iter()
            .fold(0, |n, &c| n + usize::from(c == q))
            % 2
            == 0;
        if !opens {
            return None;
        }
        let after = b + bytes[b..].iter().position(|&c| c == q)?;
        // A pair that contains nothing but the selection itself still
        // counts (so `""` grows to include the quotes).
        Some(base + before..base + after + 1)
    })
}

/// The nearest bracket pair enclosing `r`: scan back from its start for an
/// unmatched opener within `limit` bytes, then forward from there for the
/// partner, which must reach past `r`.
fn enclosing_brackets(t: &dyn Text, r: Range<usize>, limit: usize) -> Option<Range<usize>> {
    let start = r.start.saturating_sub(limit);
    let mut window = Vec::new();
    t.read_into(start..r.start, &mut window).ok()?;
    let mut depth = [0usize; 3];
    let mut opener = None;
    for (i, &b) in window.iter().enumerate().rev() {
        let Some((_, opens)) = brackets::partner(b) else {
            continue;
        };
        let k = kind(b);
        if opens {
            if depth[k] == 0 {
                opener = Some(start + i);
                break;
            }
            depth[k] -= 1;
        } else {
            depth[k] += 1;
        }
    }
    let open_at = opener?;
    let (open_at, close_at) = brackets::matching_with(t, open_at, limit + NEAR, &mut window)?;
    (close_at + 1 >= r.end).then_some(open_at..close_at + 1)
}

fn kind(b: u8) -> usize {
    match b {
        b'(' | b')' => 0,
        b'[' | b']' => 1,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RopeBuffer;

    fn grow(t: &RopeBuffer, sel: Selection) -> Vec<Range<usize>> {
        let mut out = Vec::new();
        let mut s = sel;
        while let Some(next) = expand(t, s, 1024) {
            out.push(next.range());
            s = next;
        }
        out
    }

    #[test]
    fn grows_word_then_quotes_then_brackets_then_line_then_all() {
        let text = "fn f() {\n    g(\"a b\", [1, 2]);\n}";
        let t = RopeBuffer::from_text(text);
        // Inside "a b", on the a (byte 16).
        let steps = grow(&t, Selection::cursor(16));
        assert_eq!(
            steps,
            [
                16..17,
                16..19,
                15..20,
                15..28,
                14..29,
                9..30,
                8..31,
                7..32,
                0..32
            ]
        );
        // From the 1 in [1, 2]: word, inside the brackets, the brackets,
        // then inside the argument list.
        let steps = grow(&t, Selection::cursor(23));
        assert_eq!(steps[..4], [23..24, 23..27, 22..28, 15..28]);
        // A quote that closes does not start a pair: from after "a b" the
        // next quote on the line is looked for before the one before.
        let text = "x = \"ab\" + y";
        let t = RopeBuffer::from_text(text);
        let steps = grow(&t, Selection::cursor(11));
        assert_eq!(steps, [11..12, 0..text.len()]);
    }

    #[test]
    fn a_selection_over_lines_grows_to_those_lines_then_all() {
        let text = "aa\nbb\ncc";
        let t = RopeBuffer::from_text(text);
        let steps = grow(&t, Selection { anchor: 1, head: 4 });
        assert_eq!(steps, [0..5, 0..8]);
        assert_eq!(expand(&t, Selection { anchor: 0, head: 8 }, 64), None);
    }
}
