//! Colours for git's own files: ignore patterns, attributes, config, commit
//! messages and the interactive rebase to-do list.
//!
//! These formats are made of lines that each stand on their own, so they
//! need no grammar: a few rules per line colour them fully, with the same
//! line-by-line machinery (and the same checkpoints) as the simple rules
//! for huge files. Only two formats carry anything from one line to the
//! next, in [`LexState::Git`]:
//!
//! - a commit message, which starts with its summary line, may end with
//!   the diff that `git commit -v` adds below a "scissors" line, and
//!   tells that diff's file headers (`--- a/file`) from its removed lines
//!   (`--- text`) by whether a hunk has started;
//! - a config value continued onto the next line by a final backslash.
//!
//! Every format comments with `#` at the start of a line; elsewhere `#`
//! is only a comment in config files (`;` too), as git reads them.

use std::ops::Range;

use super::{
    COMMENT, CONSTANT, DIFF_MINUS, DIFF_PLUS, ESCAPE, KEYWORD, LABEL, LexState, NUMBER, OPERATOR,
    PROPERTY, PUNCTUATION, SPECIAL, STRING, TITLE, TYPE, URI,
};

/// One of git's formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFile {
    /// `.gitignore` and `.git/info/exclude`: one path pattern per line.
    Ignore,
    /// `.gitattributes`: a pattern, then attributes to set or unset.
    Attributes,
    /// `.gitconfig`, `.gitmodules`, `.git/config`: `[section]` headers and
    /// `key = value` lines.
    Config,
    /// A commit message being written (`COMMIT_EDITMSG`).
    Commit,
    /// An interactive rebase's to-do list: a command per line.
    Rebase,
}

// Where a commit message is. The first line starts in `LexState::Normal`,
// which means "the summary line is still to come": git skips blank and
// comment lines before it.
/// Past the summary line.
const BODY: u8 = 1;
/// Past the scissors line, before the diff's first file.
const SCISSORED: u8 = 2;
/// In a file's header lines (`index`, `--- a/f`, `+++ b/f`).
const DIFF_HEAD: u8 = 3;
/// In a hunk, after its `@@` line.
const HUNK: u8 = 4;

// Where a config value continued by a final backslash is.
/// Outside quotes.
const CONTINUED: u8 = 5;
/// Inside quotes.
const CONTINUED_QUOTED: u8 = 6;

type Emit<'a> = &'a mut dyn FnMut(Range<usize>, u32);

/// Emit `range` unless it is empty.
fn put(emit: Emit<'_>, range: Range<usize>, capture: u32) {
    if range.start < range.end {
        emit(range, capture);
    }
}

/// Highlight one line of `file`, given the state it starts in; returns
/// the state the next line starts in.
pub(super) fn line(file: GitFile, line: &str, state: LexState, emit: Emit<'_>) -> LexState {
    match file {
        GitFile::Ignore => {
            ignore_line(line, emit);
            LexState::Normal
        }
        GitFile::Attributes => {
            attributes_line(line, emit);
            LexState::Normal
        }
        GitFile::Config => config_line(line, state, emit),
        GitFile::Commit => commit_line(line, state, emit),
        GitFile::Rebase => {
            rebase_line(line, emit);
            LexState::Normal
        }
    }
}

/// The first byte at or after `i` (and before `end`) that is not a space
/// or tab.
fn skip_space(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

/// The end of the word starting at `i`: the next space or tab.
fn word_end(b: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && b[i] != b' ' && b[i] != b'\t' {
        i += 1;
    }
    i
}

/// The length of the backslash escape at `i`: the backslash and the
/// character after it, if any.
fn escape_len(line: &str, i: usize) -> usize {
    1 + line
        .get(i + 1..)
        .and_then(|rest| rest.chars().next())
        .map_or(0, char::len_utf8)
}

/// Just past the closing quote of the string opening at `i`, or `end`.
fn quoted_end(line: &str, i: usize, end: usize) -> usize {
    let b = line.as_bytes();
    let mut j = i + 1;
    while j < end {
        match b[j] {
            b'\\' => j += escape_len(line, j),
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    end
}

/// Colour the path pattern in `line[range]`: wildcards (`*`, `**`, `?`)
/// and bracket expressions (`[a-z]`) stand out, and so do backslash
/// escapes such as `\#` and `\!`, which make those characters literal.
fn pattern(line: &str, range: Range<usize>, emit: Emit<'_>) {
    let b = line.as_bytes();
    let end = range.end;
    let mut i = range.start;
    while i < end {
        match b[i] {
            b'\\' => {
                let e = (i + escape_len(line, i)).min(end);
                put(emit, i..e, ESCAPE);
                i = e;
            }
            b'*' | b'?' => {
                let mut e = i + 1;
                while e < end && b[e] == b'*' {
                    e += 1;
                }
                put(emit, i..e, SPECIAL);
                i = e;
            }
            b'[' => {
                // To the next `]`; a `]` first in the set (after any `!`)
                // is one of its characters, not its end.
                let mut e = i + 1;
                if e < end && (b[e] == b'!' || b[e] == b'^') {
                    e += 1;
                }
                if e < end && b[e] == b']' {
                    e += 1;
                }
                while e < end && b[e] != b']' {
                    e += 1;
                }
                if e < end {
                    put(emit, i..e + 1, SPECIAL);
                    i = e + 1;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
}

/// `build/`, `*.log`, `!keep.log`: a `!` turns a pattern into an exception.
fn ignore_line(line: &str, emit: Emit<'_>) {
    let end = line.len();
    if line.starts_with('#') {
        put(emit, 0..end, COMMENT);
        return;
    }
    let mut start = 0;
    if line.starts_with('!') {
        put(emit, 0..1, KEYWORD);
        start = 1;
    }
    pattern(line, start..end, emit);
}

/// `*.png binary -diff text=auto`, and macro definitions such as
/// `[attr]binary -diff -merge -text`.
fn attributes_line(line: &str, emit: Emit<'_>) {
    let b = line.as_bytes();
    let end = line.len();
    let mut i = skip_space(b, 0, end);
    if i >= end {
        return;
    }
    if b[i] == b'#' {
        put(emit, i..end, COMMENT);
        return;
    }
    if line[i..].starts_with("[attr]") {
        put(emit, i..i + 6, KEYWORD);
        let e = word_end(b, i + 6, end);
        put(emit, i + 6..e, LABEL);
        i = e;
    } else if b[i] == b'"' {
        let e = quoted_end(line, i, end);
        put(emit, i..e, STRING);
        i = e;
    } else {
        let e = word_end(b, i, end);
        pattern(line, i..e, emit);
        i = e;
    }
    loop {
        i = skip_space(b, i, end);
        if i >= end {
            return;
        }
        let e = word_end(b, i, end);
        attribute(line, i..e, emit);
        i = e;
    }
}

/// One attribute: `text` (set), `-text` (unset), `!text` (unspecified),
/// `eol=lf` (a value).
fn attribute(line: &str, range: Range<usize>, emit: Emit<'_>) {
    let mut s = range.start;
    if matches!(line.as_bytes()[s], b'-' | b'!') {
        put(emit, s..s + 1, KEYWORD);
        s += 1;
    }
    if let Some(k) = line[s..range.end].find('=') {
        let eq = s + k;
        put(emit, s..eq, PROPERTY);
        put(emit, eq..eq + 1, OPERATOR);
        put(emit, eq + 1..range.end, STRING);
    } else {
        put(emit, s..range.end, PROPERTY);
    }
}

/// `[remote "origin"]` headers and `url = git@host:repo.git` settings.
fn config_line(line: &str, state: LexState, emit: Emit<'_>) -> LexState {
    let b = line.as_bytes();
    let end = line.len();
    if let LexState::Git(s @ (CONTINUED | CONTINUED_QUOTED)) = state {
        return config_value(line, 0, s == CONTINUED_QUOTED, emit);
    }
    let mut i = skip_space(b, 0, end);
    if i < end && b[i] == b'[' {
        // `[section]`, `[section "subsection"]`, or the old `[section.sub]`.
        put(emit, i..i + 1, PUNCTUATION);
        let mut j = i + 1;
        while j < end && !matches!(b[j], b' ' | b'\t' | b'"' | b']') {
            j += 1;
        }
        put(emit, i + 1..j, TYPE);
        j = skip_space(b, j, end);
        if j < end && b[j] == b'"' {
            let e = quoted_end(line, j, end);
            put(emit, j..e, STRING);
            j = skip_space(b, e, end);
        }
        if j < end && b[j] == b']' {
            put(emit, j..j + 1, PUNCTUATION);
            j += 1;
        }
        // A setting may follow on the same line.
        i = skip_space(b, j, end);
    }
    if i < end && matches!(b[i], b'#' | b';') {
        put(emit, i..end, COMMENT);
        return LexState::Normal;
    }
    let key = i;
    while i < end && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
        i += 1;
    }
    put(emit, key..i, PROPERTY);
    i = skip_space(b, i, end);
    if i < end && b[i] == b'=' {
        put(emit, i..i + 1, OPERATOR);
        return config_value(line, i + 1, false, emit);
    }
    if i < end && matches!(b[i], b'#' | b';') {
        put(emit, i..end, COMMENT);
    }
    LexState::Normal
}

/// A config value from `from`: quoted parts as strings, escapes, a
/// trailing comment, and a plain value that is a boolean (`true`, `yes`,
/// `on`...) or a number (`10`, `512m`) coloured as one. A backslash at the
/// end of the line continues the value on the next.
fn config_value(line: &str, from: usize, mut quoted: bool, emit: Emit<'_>) -> LexState {
    let b = line.as_bytes();
    let end = line.len();
    let mut i = from;
    let mut string_from = quoted.then_some(from);
    let mut plain = !quoted;
    let mut comment = None;
    while i < end {
        match b[i] {
            b'\\' => {
                plain = false;
                if let Some(s) = string_from.take() {
                    put(emit, s..i, STRING);
                }
                let n = escape_len(line, i);
                put(emit, i..i + n, ESCAPE);
                if n == 1 {
                    return LexState::Git(if quoted { CONTINUED_QUOTED } else { CONTINUED });
                }
                i += n;
                if quoted {
                    string_from = Some(i);
                }
            }
            b'"' => {
                plain = false;
                if let Some(s) = string_from.take() {
                    put(emit, s..i + 1, STRING);
                } else {
                    string_from = Some(i);
                }
                quoted = !quoted;
                i += 1;
            }
            b'#' | b';' if !quoted => {
                comment = Some(i);
                break;
            }
            _ => i += 1,
        }
    }
    if let Some(s) = string_from {
        // Unclosed: git refuses the file, but colour what is there.
        put(emit, s..i, STRING);
    }
    if plain {
        let raw = &line[from..i];
        let value = raw.trim();
        let start = from + (raw.len() - raw.trim_start().len());
        let lower = value.to_ascii_lowercase();
        let digits = value.strip_prefix('-').unwrap_or(value);
        let digits = digits
            .strip_suffix(['k', 'K', 'm', 'M', 'g', 'G'])
            .unwrap_or(digits);
        if matches!(
            lower.as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off"
        ) {
            put(emit, start..start + value.len(), CONSTANT);
        } else if !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit()) {
            put(emit, start..start + value.len(), NUMBER);
        }
    }
    if let Some(c) = comment {
        put(emit, c..end, COMMENT);
    }
    LexState::Normal
}

/// A commit message: the summary line as a title, `#` lines as comments,
/// trailers such as `Signed-off-by:`, and the diff below the scissors.
fn commit_line(line: &str, state: LexState, emit: Emit<'_>) -> LexState {
    let end = line.len();
    let at = match state {
        LexState::Git(s) => s,
        _ => 0,
    };
    match at {
        DIFF_HEAD | HUNK => return diff_line(line, at, emit),
        SCISSORED => {
            if line.starts_with("diff ") {
                put(emit, 0..end, TITLE);
                return LexState::Git(DIFF_HEAD);
            }
            if line.starts_with('#') {
                put(emit, 0..end, COMMENT);
            }
            return state;
        }
        _ => {}
    }
    if let Some(rest) = line.strip_prefix('#') {
        put(emit, 0..end, COMMENT);
        // `# ------------------------ >8 ------------------------`: git
        // ignores everything below it.
        if rest.trim_start().starts_with("---") && rest.contains(" >8 ") {
            return LexState::Git(SCISSORED);
        }
        return state;
    }
    if at == 0 {
        if line.trim().is_empty() {
            return state;
        }
        put(emit, 0..end, TITLE);
        return LexState::Git(BODY);
    }
    trailer(line, emit);
    LexState::Git(BODY)
}

/// `Signed-off-by: Name <name@example.com>`: a key with a hyphen in it
/// (so prose such as `Note: ...` is left alone), then an address.
fn trailer(line: &str, emit: Emit<'_>) {
    let Some(colon) = line.find(": ") else {
        return;
    };
    let key = &line[..colon];
    let is_key = !key.is_empty()
        && key.contains('-')
        && key.as_bytes()[0].is_ascii_alphabetic()
        && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
    if !is_key {
        return;
    }
    put(emit, 0..colon, PROPERTY);
    let rest = colon + 2;
    if let Some(open) = line[rest..].find('<')
        && let Some(close) = line[rest + open..].find('>')
    {
        let a = rest + open;
        put(emit, a..a + close + 1, URI);
    }
}

/// A line of the diff `git commit -v` shows: file headers, `@@` hunk
/// lines, and added and removed lines.
fn diff_line(line: &str, at: u8, emit: Emit<'_>) -> LexState {
    let end = line.len();
    if line.starts_with("diff ") {
        put(emit, 0..end, TITLE);
        return LexState::Git(DIFF_HEAD);
    }
    if let Some(rest) = line.strip_prefix("@@") {
        // `@@ -1,2 +1,3 @@ fn context()`: the ranges, not the context.
        let close = rest.find("@@").map_or(end, |k| k + 4);
        put(emit, 0..close, LABEL);
        return LexState::Git(HUNK);
    }
    if at == DIFF_HEAD {
        put(emit, 0..end, TITLE);
        return LexState::Git(DIFF_HEAD);
    }
    match line.as_bytes().first() {
        Some(b'+') => put(emit, 0..end, DIFF_PLUS),
        Some(b'-') => put(emit, 0..end, DIFF_MINUS),
        Some(b'\\') => put(emit, 0..end, COMMENT),
        _ => {}
    }
    LexState::Git(HUNK)
}

/// `pick 1a2b3c4 Subject`, `fixup -C 1a2b3c4 Subject`, `exec make test`,
/// `label onto`, `merge -C 1a2b3c4 topic # Merge branch 'topic'`.
fn rebase_line(line: &str, emit: Emit<'_>) {
    /// What follows a command.
    enum Then {
        /// A commit, then its subject.
        Commit,
        /// A label (or a ref, for `update-ref`).
        Label,
        /// Perhaps `-C commit`, then a label, then a `#` comment.
        Merge,
        /// A shell command, or nothing.
        Other,
    }
    let b = line.as_bytes();
    let end = line.len();
    let mut i = skip_space(b, 0, end);
    if i >= end {
        return;
    }
    if b[i] == b'#' {
        put(emit, i..end, COMMENT);
        return;
    }
    let e = word_end(b, i, end);
    let then = match &line[i..e] {
        "pick" | "p" | "reword" | "r" | "edit" | "e" | "squash" | "s" | "fixup" | "f" | "drop"
        | "d" => Then::Commit,
        "label" | "l" | "reset" | "t" | "update-ref" | "u" => Then::Label,
        "merge" | "m" => Then::Merge,
        "exec" | "x" | "break" | "b" | "noop" => Then::Other,
        _ => return,
    };
    put(emit, i..e, KEYWORD);
    i = skip_space(b, e, end);
    let mut word = |i: &mut usize, capture: Option<u32>| {
        let e = word_end(b, *i, end);
        if let Some(c) = capture {
            put(emit, *i..e, c);
        }
        *i = skip_space(b, e, end);
    };
    let is_option = |i: usize| matches!(line.get(i..word_end(b, i, end)), Some("-C" | "-c"));
    let is_hash = |i: usize| {
        let w = &line[i..word_end(b, i, end)];
        w.len() >= 4 && w.bytes().all(|c| c.is_ascii_hexdigit())
    };
    match then {
        Then::Commit => {
            if is_option(i) {
                word(&mut i, Some(OPERATOR));
            }
            if i < end && is_hash(i) {
                word(&mut i, Some(CONSTANT));
            }
        }
        Then::Label => word(&mut i, Some(LABEL)),
        Then::Merge => {
            if is_option(i) {
                word(&mut i, Some(OPERATOR));
                let hash = i < end && is_hash(i);
                word(&mut i, hash.then_some(CONSTANT));
            }
            word(&mut i, Some(LABEL));
            if i < end && b[i] == b'#' {
                put(emit, i..end, COMMENT);
            }
        }
        Then::Other => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::LanguageId;
    use crate::lexical::{CAPTURES, Lexical, builtin};
    use nimbus_core::RopeBuffer;

    /// `(text, capture name)` for each span of `src`.
    fn spans(language: LanguageId, src: &str) -> Vec<(String, &'static str)> {
        let buf = RopeBuffer::from_text(src);
        let mut out = Vec::new();
        Lexical::new(builtin(language)).spans(&buf, 0..src.len(), &mut out);
        for w in out.windows(2) {
            assert!(w[0].end <= w[1].start, "overlapping spans {w:?} in {src:?}");
        }
        out.iter()
            .map(|s| (src[s.start..s.end].to_owned(), CAPTURES[s.capture as usize]))
            .collect()
    }

    fn pairs(v: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
        v.iter().map(|(t, n)| ((*t).to_owned(), *n)).collect()
    }

    #[test]
    fn ignore_patterns() {
        let src = "# build output\n/target/\n*.log\n!keep.log\nsrc/**/gen-[a-z]?.rs\n\\#not-a-comment\nfoo # bar\n";
        assert_eq!(
            spans(LanguageId::GitIgnore, src),
            pairs(&[
                ("# build output", "comment"),
                ("*", "string.special"),
                ("!", "keyword"),
                ("**", "string.special"),
                ("[a-z]", "string.special"),
                ("?", "string.special"),
                ("\\#", "string.escape"),
            ])
        );
    }

    #[test]
    fn attributes() {
        let src = "# comment\n* text=auto eol=lf\n*.png binary -diff !merge\n[attr]bin -text\n\"a b.txt\" text\n";
        assert_eq!(
            spans(LanguageId::GitAttributes, src),
            pairs(&[
                ("# comment", "comment"),
                ("*", "string.special"),
                ("text", "property"),
                ("=", "operator"),
                ("auto", "string"),
                ("eol", "property"),
                ("=", "operator"),
                ("lf", "string"),
                ("*", "string.special"),
                ("binary", "property"),
                ("-", "keyword"),
                ("diff", "property"),
                ("!", "keyword"),
                ("merge", "property"),
                ("[attr]", "keyword"),
                ("bin", "label"),
                ("-", "keyword"),
                ("text", "property"),
                ("\"a b.txt\"", "string"),
                ("text", "property"),
            ])
        );
    }

    #[test]
    fn config() {
        let src = "[core]\n\teditor = nimbus --wait ; mine\n\tautocrlf = false\n[remote \"origin\"]\n\turl = \"git@x:y.git\"\n# note\n\tbig = 512m\n\talias = !git log \\\n  --oneline\n";
        assert_eq!(
            spans(LanguageId::GitConfig, src),
            pairs(&[
                ("[", "punctuation"),
                ("core", "type"),
                ("]", "punctuation"),
                ("editor", "property"),
                ("=", "operator"),
                ("; mine", "comment"),
                ("autocrlf", "property"),
                ("=", "operator"),
                ("false", "constant.builtin"),
                ("[", "punctuation"),
                ("remote", "type"),
                ("\"origin\"", "string"),
                ("]", "punctuation"),
                ("url", "property"),
                ("=", "operator"),
                ("\"git@x:y.git\"", "string"),
                ("# note", "comment"),
                ("big", "property"),
                ("=", "operator"),
                ("512m", "number"),
                ("alias", "property"),
                ("=", "operator"),
                ("\\", "string.escape"),
            ])
        );
        // The continued line is a value, not a key.
        let src = "[a]\nk = \"x \\\ny\" # c\n";
        assert_eq!(
            spans(LanguageId::GitConfig, src),
            pairs(&[
                ("[", "punctuation"),
                ("a", "type"),
                ("]", "punctuation"),
                ("k", "property"),
                ("=", "operator"),
                ("\"x ", "string"),
                ("\\", "string.escape"),
                ("y\"", "string"),
                ("# c", "comment"),
            ])
        );
    }

    #[test]
    fn commit_message() {
        let src = "\n# Please enter the commit message\nFix the thing\n\nLonger text: here.\n\nSigned-off-by: A B <a@b.c>\n# On branch main\n";
        assert_eq!(
            spans(LanguageId::GitCommit, src),
            pairs(&[
                ("# Please enter the commit message", "comment"),
                ("Fix the thing", "text.title"),
                ("Signed-off-by", "property"),
                ("<a@b.c>", "text.uri"),
                ("# On branch main", "comment"),
            ])
        );
    }

    #[test]
    fn commit_message_with_diff() {
        let src = "Summary\n# ------------------------ >8 ------------------------\n# Do not modify the line above.\ndiff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@ fn x()\n same\n--- removed\n+++ added\n\\ No newline at end of file\n";
        assert_eq!(
            spans(LanguageId::GitCommit, src),
            pairs(&[
                ("Summary", "text.title"),
                (
                    "# ------------------------ >8 ------------------------",
                    "comment"
                ),
                ("# Do not modify the line above.", "comment"),
                ("diff --git a/f b/f", "text.title"),
                ("index 1..2 100644", "text.title"),
                ("--- a/f", "text.title"),
                ("+++ b/f", "text.title"),
                ("@@ -1,2 +1,2 @@", "label"),
                ("--- removed", "diff.minus"),
                ("+++ added", "diff.plus"),
                ("\\ No newline at end of file", "comment"),
            ])
        );
    }

    #[test]
    fn rebase_todo() {
        let src = "pick 1a2b3c4 First\nfixup -C deadbeef Second\nexec cargo test\nlabel onto\nmerge -C 0123abcd topic # Merge topic\nbogus line\n# Commands:\n";
        assert_eq!(
            spans(LanguageId::GitRebase, src),
            pairs(&[
                ("pick", "keyword"),
                ("1a2b3c4", "constant.builtin"),
                ("fixup", "keyword"),
                ("-C", "operator"),
                ("deadbeef", "constant.builtin"),
                ("exec", "keyword"),
                ("label", "keyword"),
                ("onto", "label"),
                ("merge", "keyword"),
                ("-C", "operator"),
                ("0123abcd", "constant.builtin"),
                ("topic", "label"),
                ("# Merge topic", "comment"),
                ("# Commands:", "comment"),
            ])
        );
    }

    #[test]
    fn odd_text_never_panics_or_overlaps() {
        let odd = [
            "", "\\", "[", "[\"", "[a \"b", "\"", "k=\"\\", "!", "[!]", "[]]", "a = \\é", "@@",
            "@@ x", "é\\é", "merge -C", "fixup -c", "#", "x: <", "A-b: <a",
        ];
        for language in [
            LanguageId::GitIgnore,
            LanguageId::GitAttributes,
            LanguageId::GitConfig,
            LanguageId::GitCommit,
            LanguageId::GitRebase,
        ] {
            for a in odd {
                for b in odd {
                    let _ = spans(language, &format!("{a}\n{b}\n{a}{b}"));
                }
            }
        }
    }
}
