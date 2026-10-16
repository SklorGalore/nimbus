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
    pub fn detect<'a>(chunks: impl IntoIterator<Item = &'a str>) -> Self {
        let (mut lf, mut crlf) = (0usize, 0usize);
        // A \r at the end of one chunk may pair with a \n at the start of
        // the next, so remember the last byte seen.
        let mut prev = 0u8;
        for chunk in chunks {
            for &b in chunk.as_bytes() {
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

#[cfg(test)]
mod tests {
    use super::LineEnding;

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
