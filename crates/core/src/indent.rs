//! Indentation: how wide a level is, whether it is a tab or spaces, and
//! finding that out from a file (M11).
//!
//! Detection is a majority vote on a sample: lines beginning with a tab
//! against lines beginning with spaces, and among the space-indented
//! lines the most common count. The sample is the first
//! [`SAMPLE_LINES`] lines, never the whole file, so opening a 4 GB log
//! costs the same as opening a small one; and since every indented line
//! in a file written at one width has a multiple of that width, the most
//! common small count is the width itself.

use std::borrow::Cow;

use crate::Text;

/// How many lines detection looks at.
pub const SAMPLE_LINES: usize = 4000;

/// One level of indentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Indent {
    /// Columns per level: the number of spaces, or how wide a tab shows.
    pub width: usize,
    /// A tab per level rather than spaces.
    pub tabs: bool,
}

impl Default for Indent {
    fn default() -> Self {
        Self {
            width: 4,
            tabs: false,
        }
    }
}

impl Indent {
    /// The text of one level: a tab, or `width` spaces.
    #[must_use]
    pub fn unit(self) -> Cow<'static, str> {
        if self.tabs {
            Cow::Borrowed("\t")
        } else {
            Cow::Owned(" ".repeat(self.width))
        }
    }

    /// The text that brings column `col` to the next level: a tab, or the
    /// spaces up to the next multiple of `width`.
    #[must_use]
    pub fn to_next_stop(self, col: usize) -> Cow<'static, str> {
        if self.tabs {
            Cow::Borrowed("\t")
        } else {
            Cow::Owned(" ".repeat(self.width - col % self.width))
        }
    }

    /// How the status bar names it: "Spaces: 4" or "Tabs".
    #[must_use]
    pub fn label(self) -> String {
        if self.tabs {
            "Tabs".to_owned()
        } else {
            format!("Spaces: {}", self.width)
        }
    }

    /// `leading`, a line's leading whitespace, with one level removed:
    /// how many of its bytes to delete.
    #[must_use]
    pub fn dedent_bytes(self, leading: &str) -> usize {
        if leading.starts_with('\t') {
            1
        } else {
            leading
                .bytes()
                .take(self.width)
                .take_while(|b| *b == b' ')
                .count()
        }
    }

    /// Detect the indentation of `t` from its first lines: `None` when
    /// too few lines are indented to say.
    #[must_use]
    pub fn detect(t: &dyn Text) -> Option<Self> {
        let mut tabs = 0usize;
        let mut spaces = 0usize;
        let mut counts = [0usize; 9];
        let mut pos = 0;
        for _ in 0..SAMPLE_LINES {
            let span = t.line_span(pos);
            // Only the start of each line is read: a long line costs no
            // more than a short one.
            let head = t.slice(span.start..span.end.min(span.start + 64));
            match head.as_bytes().first() {
                Some(b'\t') => tabs += 1,
                Some(b' ') => {
                    let n = head.bytes().take_while(|b| *b == b' ').count();
                    // A line of only spaces says nothing.
                    if n < head.len() {
                        spaces += 1;
                        if (2..=8).contains(&n) {
                            counts[n] += 1;
                        }
                    }
                }
                _ => {}
            }
            match span.next {
                Some(n) => pos = n,
                None => break,
            }
        }
        if tabs == 0 && spaces == 0 {
            return None;
        }
        if tabs > spaces {
            return Some(Self {
                width: Self::default().width,
                tabs: true,
            });
        }
        let width = (2..=8usize).rev().max_by_key(|&n| counts[n])?;
        (counts[width] > 0).then_some(Self { width, tabs: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RopeBuffer;

    fn detect(text: &str) -> Option<Indent> {
        Indent::detect(&RopeBuffer::from_text(text))
    }

    #[test]
    fn detects_tabs_two_and_four_spaces_and_nothing() {
        assert_eq!(
            detect("a\n\tb\n\t\tc\n"),
            Some(Indent {
                width: 4,
                tabs: true
            })
        );
        assert_eq!(
            detect("a\n  b\n    c\n  d\n"),
            Some(Indent {
                width: 2,
                tabs: false
            })
        );
        assert_eq!(
            detect("a\n    b\n        c\n    d\n"),
            Some(Indent {
                width: 4,
                tabs: false
            })
        );
        assert_eq!(
            detect("a\nb\n   \n"),
            None,
            "flat files, and lines of spaces, say nothing"
        );
        // Mixed: the majority wins.
        assert_eq!(detect("\ta\n  b\n\tc\n").map(|i| i.tabs), Some(true));
        // A one-space indent (a comment continuation) is not a width.
        assert_eq!(detect(" a\n b\n"), None);
    }

    #[test]
    fn units_stops_labels_and_dedent() {
        let spaces = Indent {
            width: 4,
            tabs: false,
        };
        let tabs = Indent {
            width: 4,
            tabs: true,
        };
        assert_eq!(spaces.unit(), "    ");
        assert_eq!(tabs.unit(), "\t");
        assert_eq!(spaces.to_next_stop(6), "  ");
        assert_eq!(spaces.to_next_stop(8), "    ");
        assert_eq!(tabs.to_next_stop(6), "\t");
        assert_eq!(spaces.label(), "Spaces: 4");
        assert_eq!(tabs.label(), "Tabs");
        assert_eq!(spaces.dedent_bytes("      x"), 4);
        assert_eq!(spaces.dedent_bytes("  x"), 2);
        assert_eq!(spaces.dedent_bytes("\t  x"), 1);
        assert_eq!(tabs.dedent_bytes("    x"), 4);
    }
}
