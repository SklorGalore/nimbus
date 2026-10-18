//! Detecting and writing a file's line endings.
//!
//! The buffer always splits lines at `\n`. What a file uses on disk is
//! detected once when it opens and reused for every line the user adds, so
//! a Windows file stays CRLF and a Unix file stays LF.

/// Line ending style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\n`, used on macOS and Linux.
    #[default]
    Lf,
    /// `\r\n`, used on Windows.
    CrLf,
}

impl LineEnding {
    /// The characters to insert when the user presses Enter.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
        }
    }

    /// Detect the ending used by most lines in `chunks`, which together form
    /// the text. A file with no line breaks gets the platform-neutral default,
    /// LF. Ties go to LF.
    pub fn detect<C: AsRef<[u8]>>(chunks: impl IntoIterator<Item = C>) -> Self {
        let (mut lf, mut crlf) = (0usize, 0usize);
        // A \r at the end of one chunk may pair with a \n at the start of
        // the next, so remember the last byte seen.
        let mut prev = 0u8;
        for chunk in chunks {
            for &b in chunk.as_ref() {
                if b == b'\n' {
                    if prev == b'\r' {
                        crlf += 1;
                    } else {
                        lf += 1;
                    }
                }
                prev = b;
            }
        }
        if crlf > lf {
            LineEnding::CrLf
        } else {
            LineEnding::Lf
        }
    }
}

/// Where the line endings in `text` that are not `to` are, as byte
/// offsets in order: for [`LineEnding::Lf`], the `\r` of each `\r\n`
/// (to delete); for [`LineEnding::CrLf`], each `\n` with no `\r` before
/// it (to put one in front of). A lone `\r` is ordinary text and stays.
///
/// The text is read a chunk at a time, never as whole lines, so this works
/// on any backend. `None` when there are more than `max`, so a huge file
/// is not turned into a huge list.
pub fn mismatched<T: crate::Text + ?Sized>(
    text: &T,
    to: LineEnding,
    max: usize,
) -> Option<Vec<usize>> {
    let mut found = Vec::new();
    let mut prev = 0u8;
    let mut pos = 0;
    let len = text.len_bytes();
    while pos < len {
        let (chunk, start) = text.chunk_at(pos);
        let from = pos - start;
        if chunk.len() <= from {
            break;
        }
        for (i, &b) in chunk[from..].iter().enumerate() {
            if b == b'\n' {
                let at = pos + i;
                match (to, prev == b'\r') {
                    (LineEnding::Lf, true) => found.push(at - 1),
                    (LineEnding::CrLf, false) => found.push(at),
                    _ => {}
                }
                if found.len() > max {
                    return None;
                }
            }
            prev = b;
        }
        pos += chunk.len() - from;
    }
    Some(found)
}

#[cfg(test)]
mod tests {
    use super::{LineEnding, mismatched};
    use crate::RopeBuffer;

    #[test]
    fn finds_the_endings_to_change() {
        let t = RopeBuffer::from_text("a\r\nb\nc\rd\r\n\n");
        assert_eq!(mismatched(&t, LineEnding::Lf, 10), Some(vec![1, 8]));
        assert_eq!(mismatched(&t, LineEnding::CrLf, 10), Some(vec![4, 10]));
        assert_eq!(mismatched(&t, LineEnding::Lf, 1), None);
        let empty = RopeBuffer::from_text("");
        assert_eq!(mismatched(&empty, LineEnding::CrLf, 0), Some(vec![]));
    }

    #[test]
    fn a_crlf_split_between_chunks_is_seen() {
        // Many chunks: a long text with endings at every position mod 7.
        let text = "abcdef\r\n".repeat(20_000);
        let t = RopeBuffer::from_text(&text);
        let found = mismatched(&t, LineEnding::Lf, usize::MAX).unwrap();
        assert_eq!(found.len(), 20_000);
        assert!(found.iter().all(|&p| &text[p..p + 2] == "\r\n"));
    }

    #[test]
    fn majority_wins_and_ties_go_to_lf() {
        assert_eq!(LineEnding::detect(["a\r\nb\r\nc\n"]), LineEnding::CrLf);
        assert_eq!(LineEnding::detect(["a\r\nb\n"]), LineEnding::Lf);
        assert_eq!(LineEnding::detect(["no breaks"]), LineEnding::Lf);
    }

    #[test]
    fn crlf_split_across_chunks_is_detected() {
        assert_eq!(LineEnding::detect(["a\r", "\nb\r", "\n"]), LineEnding::CrLf);
    }
}
