//! Cutting long lines into pieces for the diff (M18).
//!
//! A line diff treats each line as one token, so a file that is one long
//! line (minified JSON, a log without line breaks) compares as "everything
//! changed" however small the edit. The fix is to cut long lines into
//! pieces and diff the pieces instead.
//!
//! Where to cut matters. Cutting every 1 KB at fixed offsets breaks on the
//! first insertion: every cut after it moves by the inserted length, so
//! every piece after it differs. Instead a cut goes wherever the bytes just
//! before it hash to a value whose top bits are zero. The hash (a "gear"
//! hash) only looks at the last 64 bytes, so a cut depends only on the
//! content around it, not on its offset: an insertion changes the cuts
//! within 64 bytes of it and leaves every other cut where it was, relative
//! to the content. This is content-defined chunking, the idea behind rsync
//! and deduplicating backup tools.
//!
//! Only lines longer than [`SPLIT_AT`] are cut, and the cuts never reach
//! the buffer: the editor still sees one line. [`Splits`] records, per
//! text, which lines were cut and where, so the compare view can map a
//! piece (a *unit* of the diff) back to its line and byte range.

use std::ops::Range;

use crate::{LineSpan, Text};

/// Lines with more content bytes than this are cut into pieces.
pub const SPLIT_AT: usize = 4096;

/// A piece is cut here whatever the hash says, so text that never hits a
/// cut (a run of one repeated byte) still makes pieces of bounded size.
const MAX_PIECE: usize = 16 * 1024;

/// The hash bits that must be zero for a cut: ten bits, so a cut about
/// every 1 KB on varied text. The top bits, because a gear hash's bit `k`
/// depends on the last `k + 1` bytes: the top ten depend on all 64.
const MASK: u64 = 0x3FF << 54;

/// A random 64-bit number per byte value, fixed at compile time
/// (splitmix64 from a constant seed), so cuts are the same on every run
/// and every platform.
const GEAR: [u64; 256] = gear_table();

const fn gear_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut state: u64 = 0x6E69_6D62_7573_2131;
    let mut i = 0;
    while i < 256 {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        table[i] = z ^ (z >> 31);
        i += 1;
    }
    table
}

/// Whether a byte continues a UTF-8 character (`10xxxxxx`): no cut goes
/// before one, so every piece starts on a character.
fn continues_char(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// Append to `out` where the pieces of the line content `line` start,
/// after the first, as offsets from `line.start`. Nothing when the content
/// is not longer than [`SPLIT_AT`]. Reads the text once, in storage order.
pub fn cuts(t: &dyn Text, line: Range<usize>, out: &mut Vec<usize>) {
    if line.end.saturating_sub(line.start) <= SPLIT_AT {
        return;
    }
    let mut h: u64 = 0;
    let mut since = 0usize;
    let mut pos = line.start;
    while pos < line.end {
        let (chunk, base) = t.chunk_at(pos);
        let Some(bytes) = chunk.get(pos - base..) else {
            return;
        };
        let bytes = &bytes[..bytes.len().min(line.end - pos)];
        if bytes.is_empty() {
            return;
        }
        for (i, &b) in bytes.iter().enumerate() {
            // Decide on a cut before `b` from the 64 bytes before it.
            if since > 0 && !continues_char(b) && (h & MASK == 0 || since >= MAX_PIECE) {
                out.push(pos + i - line.start);
                since = 0;
            }
            h = (h << 1).wrapping_add(GEAR[usize::from(b)]);
            since += 1;
        }
        pos += bytes.len();
    }
}

/// Where a line's content ends, given its start and where the next line
/// starts (or the end of the text): before a final `\n` or `\r\n`.
#[must_use]
pub fn content_end(t: &dyn Text, start: usize, end: usize) -> usize {
    let mut e = end;
    if e > start && t.byte_at(e - 1) == Some(b'\n') {
        e -= 1;
        if e > start && t.byte_at(e - 1) == Some(b'\r') {
            e -= 1;
        }
    }
    e
}

/// Where the line starting at `start` ends: just after its `\n`, or at
/// `limit` when none comes first.
#[must_use]
pub fn line_end(t: &dyn Text, start: usize, limit: usize) -> usize {
    let mut pos = start;
    while pos < limit {
        let (chunk, base) = t.chunk_at(pos);
        let Some(bytes) = chunk.get(pos - base..) else {
            return limit;
        };
        let bytes = &bytes[..bytes.len().min(limit - pos)];
        if bytes.is_empty() {
            return limit;
        }
        if let Some(i) = memchr::memchr(b'\n', bytes) {
            return pos + i + 1;
        }
        pos += bytes.len();
    }
    limit
}

/// Whether any line of `t` is longer than [`SPLIT_AT`] bytes (ending
/// included): a quick check before choosing a diff that cuts lines. Reads
/// the text once, looking only for line breaks.
#[must_use]
pub fn has_long_lines(t: &dyn Text) -> bool {
    let len = t.len_bytes();
    let mut line_start = 0;
    let mut pos = 0;
    while pos < len {
        let (chunk, base) = t.chunk_at(pos);
        let Some(bytes) = chunk.get(pos - base..) else {
            break;
        };
        if bytes.is_empty() {
            break;
        }
        for i in memchr::memchr_iter(b'\n', bytes) {
            let end = pos + i + 1;
            if end - line_start > SPLIT_AT + 1 {
                return true;
            }
            line_start = end;
        }
        pos += bytes.len();
    }
    len - line_start > SPLIT_AT
}

/// One cut line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Split {
    /// Its line number, by the diff's convention.
    line: usize,
    /// Extra units (pieces after the first) of the cut lines before it.
    extra_before: usize,
    /// Where its pieces after the first start, as offsets from the line's
    /// start. Offsets rather than positions, so after an edit elsewhere a
    /// line still maps to its own bytes.
    cuts: Vec<usize>,
}

/// Which lines of a text were cut, and where. Empty for a text with no
/// long lines, which is the usual case: then units and lines are the same.
///
/// A *unit* is what the diff compares: a whole line, or one piece of a
/// cut line. Units are numbered in text order, so a cut line with three
/// pieces takes three consecutive unit numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Splits {
    lines: Vec<Split>,
}

impl Splits {
    /// Find and cut every long line of `t`. Reads the whole text once.
    #[must_use]
    pub fn scan(t: &dyn Text) -> Self {
        let mut lines = Vec::new();
        let mut extra = 0;
        let mut line = 0;
        let mut line_start = 0;
        let len = t.len_bytes();
        let mut pos = 0;
        let mut found = |line: usize, start: usize, end: usize, extra: &mut usize| {
            let content = content_end(t, start, end);
            if content - start > SPLIT_AT {
                let mut c = Vec::new();
                cuts(t, start..content, &mut c);
                if !c.is_empty() {
                    let n = c.len();
                    lines.push(Split {
                        line,
                        extra_before: *extra,
                        cuts: c,
                    });
                    *extra += n;
                }
            }
        };
        while pos < len {
            let (chunk, base) = t.chunk_at(pos);
            let Some(bytes) = chunk.get(pos - base..) else {
                break;
            };
            if bytes.is_empty() {
                break;
            }
            for i in memchr::memchr_iter(b'\n', bytes) {
                let end = pos + i + 1;
                if end - line_start > SPLIT_AT {
                    found(line, line_start, end, &mut extra);
                }
                line += 1;
                line_start = end;
            }
            pos += bytes.len();
        }
        if line_start < len && len - line_start > SPLIT_AT {
            found(line, line_start, len, &mut extra);
        }
        Self { lines }
    }

    /// No line was cut.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// How many lines were cut.
    #[must_use]
    pub fn cut_lines(&self) -> usize {
        self.lines.len()
    }

    /// Units in a text of `lines` lines.
    #[must_use]
    pub fn units(&self, lines: usize) -> usize {
        lines
            + self
                .lines
                .last()
                .map_or(0, |s| s.extra_before + s.cuts.len())
    }

    /// The first unit of line `line`.
    #[must_use]
    pub fn unit_of_line(&self, line: usize) -> usize {
        let i = self.lines.partition_point(|s| s.line < line);
        let extra = i
            .checked_sub(1)
            .and_then(|i| self.lines.get(i))
            .map_or(0, |s| s.extra_before + s.cuts.len());
        line + extra
    }

    /// The line holding unit `unit`, and which of its pieces it is (0 for
    /// an uncut line).
    #[must_use]
    pub fn locate(&self, unit: usize) -> (usize, usize) {
        let i = self
            .lines
            .partition_point(|s| s.line + s.extra_before <= unit);
        let Some(s) = i.checked_sub(1).and_then(|i| self.lines.get(i)) else {
            return (unit, 0);
        };
        let first = s.line + s.extra_before;
        if unit - first <= s.cuts.len() {
            (s.line, unit - first)
        } else {
            (unit - s.extra_before - s.cuts.len(), 0)
        }
    }

    /// The bytes of piece `piece` of the line at `span`, when that line
    /// was cut: the range (clamped to the line's content, which may have
    /// changed since the scan) and whether the piece continues the one
    /// before it. `None` for an uncut line, which draws whole.
    #[must_use]
    pub fn piece(&self, line: usize, piece: usize, span: LineSpan) -> Option<(Range<usize>, bool)> {
        let i = self.lines.binary_search_by_key(&line, |s| s.line).ok()?;
        let cuts = &self.lines[i].cuts;
        let at = |k: usize| (span.start + k).min(span.end);
        let start = if piece == 0 {
            span.start
        } else {
            at(*cuts.get(piece - 1)?)
        };
        let end = cuts.get(piece).map_or(span.end, |&k| at(k));
        Some((start..end, piece > 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffer, RopeBuffer};

    /// Deterministic varied ASCII, like minified JSON.
    fn varied(len: usize, seed: u64) -> String {
        let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789{}[]:,\" ";
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                char::from(alphabet[(x % alphabet.len() as u64) as usize])
            })
            .collect()
    }

    fn cuts_of(text: &str) -> Vec<usize> {
        let b = RopeBuffer::from_text(text);
        let mut out = Vec::new();
        cuts(&b, 0..text.len(), &mut out);
        out
    }

    #[test]
    fn short_lines_are_not_cut_and_long_ones_average_a_kilobyte() {
        assert!(cuts_of(&varied(SPLIT_AT, 1)).is_empty());
        let c = cuts_of(&varied(1 << 20, 2));
        let average = (1 << 20) / (c.len() + 1);
        assert!((512..2048).contains(&average), "average piece {average}");
        assert!(
            c.windows(2)
                .all(|w| w[0] < w[1] && w[1] - w[0] <= MAX_PIECE)
        );
    }

    #[test]
    fn a_run_of_one_byte_still_makes_bounded_pieces() {
        let c = cuts_of(&"x".repeat(100_000));
        assert!(!c.is_empty());
        assert!(c.windows(2).all(|w| w[1] - w[0] <= MAX_PIECE));
    }

    #[test]
    fn pieces_start_on_characters() {
        let text: String = varied(50_000, 3).replace('a', "é").replace('b', "日");
        for k in cuts_of(&text) {
            assert!(text.is_char_boundary(k), "{k}");
        }
    }

    /// The property content-defined chunking exists for: an insertion
    /// moves at most two cut points; every other cut stays with its
    /// content (shifted by the inserted length when after it).
    #[test]
    fn an_insertion_moves_at_most_two_cuts() {
        for seed in 1..40u64 {
            let base = varied(64 * 1024, seed);
            let at = (seed as usize * 1543) % base.len();
            let insert = varied(1 + (seed as usize % 40), seed + 1000);
            let mut edited = base.clone();
            edited.insert_str(at, &insert);
            let before: Vec<usize> = cuts_of(&base)
                .into_iter()
                .map(|k| if k > at { k + insert.len() } else { k })
                .collect();
            let after = cuts_of(&edited);
            let only_before = before.iter().filter(|k| !after.contains(k)).count();
            let only_after = after.iter().filter(|k| !before.contains(k)).count();
            assert!(
                only_before + only_after <= 2,
                "seed {seed}: {only_before} cuts lost, {only_after} gained"
            );
        }
    }

    #[test]
    fn scan_numbers_units_across_cut_lines() {
        let long = varied(20_000, 5);
        let text = format!("short\n{long}\nmiddle\n{long}");
        let b = RopeBuffer::from_text(&text);
        let s = Splits::scan(&b);
        assert_eq!(s.cut_lines(), 2);
        let pieces = cuts_of(&long).len() + 1;
        assert_eq!(s.units(4), 4 + 2 * (pieces - 1));
        assert_eq!(s.unit_of_line(0), 0);
        assert_eq!(s.unit_of_line(1), 1);
        assert_eq!(s.unit_of_line(2), 1 + pieces);
        assert_eq!(s.unit_of_line(3), 2 + pieces);
        assert_eq!(s.locate(0), (0, 0));
        assert_eq!(s.locate(1), (1, 0));
        assert_eq!(s.locate(pieces), (1, pieces - 1));
        assert_eq!(s.locate(pieces + 1), (2, 0));
        assert_eq!(s.locate(pieces + 2), (3, 0));
        assert_eq!(s.locate(2 * pieces + 1), (3, pieces - 1));
        // Pieces cover the line's content exactly, in order.
        let span = b.line_span(b.line_to_byte(1));
        let mut at = span.start;
        for p in 0..pieces {
            let (r, continued) = s.piece(1, p, span).unwrap();
            assert_eq!(r.start, at);
            assert_eq!(continued, p > 0);
            at = r.end;
        }
        assert_eq!(at, span.end);
        assert!(s.piece(0, 0, b.line_span(0)).is_none());
        assert!(has_long_lines(&b));
        assert!(!has_long_lines(&RopeBuffer::from_text("a\nb\n")));
    }
}
