//! Toggling comments (M11).
//!
//! A line comment goes on every selected line that is not blank, at the
//! column of the shallowest indentation among them, so a commented block
//! reads as one. If every such line already carries the token, all of
//! them are uncommented instead: the common convention, and the one that
//! makes pressing the key twice a no-op.
//!
//! The tokens come from the language ([`Tokens`]); this module only places
//! them. Like the rest of `core`, it reads only the lines the selections
//! touch, and of each line only its start.

use std::borrow::Cow;
use std::ops::Range;

use crate::lines::{Batch, Block, blocks};
use crate::long_line::NEAR;
use crate::{Selection, Selections, Text};

/// A language's comment syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tokens {
    /// What starts a comment that runs to the end of the line (`//`).
    pub line: Option<&'static str>,
    /// What opens and closes a comment that can span lines (`/*`, `*/`).
    pub block: Option<(&'static str, &'static str)>,
}

/// Add or remove `token` on the lines of every selection.
#[must_use]
pub fn toggle_line<'t>(t: &'t dyn Text, sels: &Selections, token: &str) -> Batch<'t> {
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    for (b, _) in blocks(t, sels, false) {
        toggle_block_lines(t, b, token, &mut items);
    }
    Batch { items, after: None }
}

/// One block of lines: comment them all, or uncomment them all.
fn toggle_block_lines<'t>(
    t: &'t dyn Text,
    b: Block,
    token: &str,
    items: &mut Vec<(Range<usize>, Cow<'t, str>)>,
) {
    // Pass one: the lines, each with its indentation and whether it is
    // already commented. Only the start of each line is read.
    let mut lines: Vec<(usize, usize, Option<usize>)> = Vec::new(); // (start, indent, token end)
    let mut pos = b.start;
    loop {
        let span = t.line_span(pos);
        let head = t.slice(span.start..span.end.min(span.start + NEAR));
        let indent = head.len() - head.trim_start_matches([' ', '\t']).len();
        let blank = indent == head.len() && span.end - span.start == head.len();
        if !blank {
            let rest = &head[indent..];
            let commented = rest.starts_with(token).then(|| {
                let after = indent + token.len();
                // A space after the token, if the comment was made by us.
                if rest[token.len()..].starts_with(' ') {
                    after + 1
                } else {
                    after
                }
            });
            lines.push((span.start, indent, commented));
        }
        match span.next {
            Some(n) if n <= b.end => pos = n,
            _ => break,
        }
    }
    if lines.is_empty() {
        // Only blank lines: comment them where they are.
        let mut pos = b.start;
        loop {
            let span = t.line_span(pos);
            items.push((span.start..span.start, Cow::Owned(format!("{token} "))));
            match span.next {
                Some(n) if n <= b.end => pos = n,
                _ => break,
            }
        }
        return;
    }
    if lines.iter().all(|(_, _, c)| c.is_some()) {
        for (start, indent, commented) in lines {
            if let Some(end) = commented {
                items.push((start + indent..start + end, Cow::Borrowed("")));
            }
        }
    } else {
        let column = lines.iter().map(|(_, i, _)| *i).min().unwrap_or(0);
        for (start, _, _) in lines {
            let at = start + column;
            items.push((at..at, Cow::Owned(format!("{token} "))));
        }
    }
}

/// Wrap each selection in `open` and `close`, or unwrap it if it is
/// already wrapped. A bare cursor gets an empty comment to type into.
#[must_use]
pub fn toggle_block<'t>(
    t: &'t dyn Text,
    sels: &Selections,
    (open, close): (&str, &str),
) -> Batch<'t> {
    let mut items: Vec<(Range<usize>, Cow<'t, str>)> = Vec::new();
    let mut after = Vec::new();
    let mut delta = 0isize;
    for s in sels.iter() {
        let r = s.range();
        let text = t.slice(r.clone());
        let inner = text.trim();
        let wrapped = inner.len() >= open.len() + close.len()
            && inner.starts_with(open)
            && inner.ends_with(close);
        if wrapped {
            // Remove the tokens and one space inside each, if present.
            let lead = text.len() - text.trim_start().len();
            let a = r.start + lead;
            let mut a_end = a + open.len();
            if text[lead + open.len()..].starts_with(' ') {
                a_end += 1;
            }
            let trail = text.len() - text.trim_end().len();
            let b_end = r.end - trail;
            let mut b = b_end - close.len();
            if b > a_end && text[..b - r.start].ends_with(' ') {
                b -= 1;
            }
            let start = a.saturating_add_signed(delta);
            delta -= (a_end - a).cast_signed();
            let end = b.saturating_add_signed(delta);
            delta -= (b_end - b).cast_signed();
            items.push((a..a_end, Cow::Borrowed("")));
            items.push((b..b_end, Cow::Borrowed("")));
            after.push(directed(start..end, *s));
        } else {
            let before = format!("{open} ");
            let behind = format!(" {close}");
            let start = r.start.saturating_add_signed(delta) + before.len();
            delta += before.len().cast_signed();
            let end = r.end.saturating_add_signed(delta);
            delta += behind.len().cast_signed();
            items.push((r.start..r.start, Cow::Owned(before)));
            items.push((r.end..r.end, Cow::Owned(behind)));
            after.push(directed(start..end, *s));
        }
    }
    Batch {
        items,
        after: Some(Selections::from_vec(after, sels.primary_index())),
    }
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
    fn line_comments_toggle_at_the_shallowest_indent_and_skip_blank_lines() {
        let text = "fn f() {\n    a();\n\n        b();\n}";
        let t = RopeBuffer::from_text(text);
        let sel = Selections::single(Selection {
            anchor: 9,
            head: 28,
        });
        let (out, s) = apply(text, &sel, toggle_line(&t, &sel, "//"));
        assert_eq!(out, "fn f() {\n    // a();\n\n    //     b();\n}");
        assert_eq!(
            s.primary(),
            Selection {
                anchor: 9,
                head: 34
            }
        );
        // Again: every line is commented, so all are uncommented.
        let t2 = RopeBuffer::from_text(&out);
        let (back, _) = apply(&out, &s, toggle_line(&t2, &s, "//"));
        assert_eq!(back, text);
        // A cursor comments its own line, with the cursor moved past the
        // token; three cursors comment three lines.
        let (out, s) = apply(
            text,
            &cursors(&[13]),
            toggle_line(&t, &cursors(&[13]), "//"),
        );
        assert_eq!(out, "fn f() {\n    // a();\n\n        b();\n}");
        assert_eq!(s.primary().head, 16);
        let three = cursors(&[0, 13, 26]);
        let (out, s) = apply(text, &three, toggle_line(&t, &three, "#"));
        assert_eq!(out, "# fn f() {\n    # a();\n\n        # b();\n}");
        assert_eq!(s.count(), 3);
        // Mixed: one commented line among plain ones gets commented too.
        let text = "a\n// b\nc";
        let t = RopeBuffer::from_text(text);
        let all = Selections::single(Selection { anchor: 0, head: 8 });
        let (out, _) = apply(text, &all, toggle_line(&t, &all, "//"));
        assert_eq!(out, "// a\n// // b\n// c");
        // Only blank lines: they get the token.
        let text = "\n\n";
        let t = RopeBuffer::from_text(text);
        let (out, _) = apply(text, &cursors(&[0]), toggle_line(&t, &cursors(&[0]), "//"));
        assert_eq!(out, "// \n\n");
    }

    #[test]
    fn block_comments_wrap_and_unwrap_with_one_and_three_cursors() {
        let text = "a b c";
        let t = RopeBuffer::from_text(text);
        let sel = Selections::single(Selection { anchor: 2, head: 3 });
        let (out, s) = apply(text, &sel, toggle_block(&t, &sel, ("/*", "*/")));
        assert_eq!(out, "a /* b */ c");
        assert_eq!(s.primary(), Selection { anchor: 5, head: 6 });
        let t2 = RopeBuffer::from_text(&out);
        let wrapped = Selections::single(Selection { anchor: 2, head: 9 });
        let (back, s) = apply(&out, &wrapped, toggle_block(&t2, &wrapped, ("/*", "*/")));
        assert_eq!(back, text);
        assert_eq!(s.primary(), Selection { anchor: 2, head: 3 });
        // A cursor gets an empty comment to type into.
        let (out, s) = apply(
            text,
            &cursors(&[1]),
            toggle_block(&t, &cursors(&[1]), ("<!--", "-->")),
        );
        assert_eq!(out, "a<!--  --> b c");
        assert_eq!(s.primary().head, 6);
        let three = cursors(&[0, 2, 4]);
        let (out, s) = apply(text, &three, toggle_block(&t, &three, ("/*", "*/")));
        assert_eq!(out, "/*  */a /*  */b /*  */c");
        assert_eq!(s.iter().map(|s| s.head).collect::<Vec<_>>(), [3, 11, 19]);
    }
}
