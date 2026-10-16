//! Simple highlighting for files too large to parse: a few rules per
//! language, applied line by line.
//!
//! A syntax tree costs about 28 bytes of memory per byte of source, so
//! files over [`crate::state::MAX_HIGHLIGHT_BYTES`] are not parsed. Instead
//! each line is split into comments, strings, keywords, numbers and a few
//! literals by small hand-written rules. The only thing carried from one
//! line to the next is whether it ended inside a block comment, a
//! multi-line string or a Markdown code fence ([`LexState`]), so memory
//! stays near zero at any file size.
//!
//! The state at the start of a line depends on every line before it. It is
//! saved every [`CHECKPOINT_LINES`] lines as lines are scanned, and an edit
//! discards the checkpoints after it. To reach a line far beyond the last
//! checkpoint, scanning starts at most [`LOOKBACK_BYTES`] before it,
//! assuming plain code there: colours can be wrong only where one comment or
//! string spans more than that above the screen.

use std::cell::RefCell;
use std::ops::Range;

use nimbus_core::{Edit, Text, long_line};

use crate::LanguageId;
use crate::highlight::Span;

/// Capture names of the spans this module produces, indexed by
/// [`Span::capture`]. Themes colour them like tree-sitter captures.
pub const CAPTURES: &[&str] = &[
    "comment",
    "string",
    "keyword",
    "number",
    "constant.builtin",
    "text.title",
    "text.literal",
];

const COMMENT: u32 = 0;
const STRING: u32 = 1;
const KEYWORD: u32 = 2;
const NUMBER: u32 = 3;
const CONSTANT: u32 = 4;
const TITLE: u32 = 5;
const LITERAL: u32 = 6;

/// Save the state at the start of every this many lines.
pub const CHECKPOINT_LINES: usize = 256;

/// The furthest back a scan starts from, when there is no checkpoint near:
/// about a millisecond of scanning.
pub const LOOKBACK_BYTES: usize = 256 * 1024;

/// A kind of string literal.
#[derive(Debug)]
struct StringRule {
    open: &'static str,
    close: &'static str,
    /// A backslash escapes the next character.
    escapes: bool,
    /// May continue onto following lines.
    multiline: bool,
}

const fn s(open: &'static str, close: &'static str, escapes: bool, multiline: bool) -> StringRule {
    StringRule {
        open,
        close,
        escapes,
        multiline,
    }
}

/// How one language looks, as far as these rules go. Built in for the
/// starter languages ([`builtin`]); user languages come from a
/// [`RuleSpec`] ([`Rules::from_spec`]).
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent traits of a language, written as a table"
)]
pub struct Rules {
    line_comments: &'static [&'static str],
    /// `#` starts a comment only at the start of a line or after
    /// whitespace (Bash's `$#`, YAML's `a#b`).
    hash_after_space: bool,
    block_comment: Option<(&'static str, &'static str)>,
    /// Block comments nest (Rust).
    nested_comments: bool,
    /// Checked in order, so longer openers (`"""`) come first.
    strings: &'static [StringRule],
    /// Rust's `'a'`, told apart from lifetimes like `'a`.
    char_literals: bool,
    keywords: &'static [&'static str],
    constants: &'static [&'static str],
    /// Headings and fenced code blocks instead of the rules above.
    markdown: bool,
}

const NONE: Rules = Rules {
    line_comments: &[],
    hash_after_space: false,
    block_comment: None,
    nested_comments: false,
    strings: &[],
    char_literals: false,
    keywords: &[],
    constants: &[],
    markdown: false,
};

const RUST: Rules = Rules {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    nested_comments: true,
    strings: &[s("\"", "\"", true, true)],
    char_literals: true,
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "unsafe", "use", "where", "while",
    ],
    constants: &["true", "false", "None"],
    ..NONE
};

const PYTHON: Rules = Rules {
    line_comments: &["#"],
    strings: &[
        s("\"\"\"", "\"\"\"", true, true),
        s("'''", "'''", true, true),
        s("\"", "\"", true, false),
        s("'", "'", true, false),
    ],
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield",
    ],
    constants: &["True", "False", "None"],
    ..NONE
};

const JS_KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "of",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "while",
    "yield",
];

const TS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "namespace",
    "new",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "type",
    "typeof",
    "var",
    "while",
    "yield",
];

const JS_STRINGS: &[StringRule] = &[
    s("\"", "\"", true, false),
    s("'", "'", true, false),
    s("`", "`", true, true),
];

const JAVASCRIPT: Rules = Rules {
    line_comments: &["//"],
    block_comment: Some(("/*", "*/")),
    strings: JS_STRINGS,
    keywords: JS_KEYWORDS,
    constants: &["true", "false", "null", "undefined", "NaN"],
    ..NONE
};

const TYPESCRIPT: Rules = Rules {
    keywords: TS_KEYWORDS,
    ..JAVASCRIPT
};

const JSON: Rules = Rules {
    strings: &[s("\"", "\"", true, false)],
    constants: &["true", "false", "null"],
    ..NONE
};

const TOML: Rules = Rules {
    line_comments: &["#"],
    strings: &[
        s("\"\"\"", "\"\"\"", true, true),
        s("'''", "'''", false, true),
        s("\"", "\"", true, false),
        s("'", "'", false, false),
    ],
    constants: &["true", "false"],
    ..NONE
};

const YAML: Rules = Rules {
    line_comments: &["#"],
    hash_after_space: true,
    strings: &[s("\"", "\"", true, false), s("'", "'", false, false)],
    constants: &["true", "false", "null"],
    ..NONE
};

const BASH: Rules = Rules {
    line_comments: &["#"],
    hash_after_space: true,
    strings: &[s("\"", "\"", true, true), s("'", "'", false, true)],
    keywords: &[
        "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
        "in", "local", "return", "then", "until", "while",
    ],
    ..NONE
};

const MARKDOWN: Rules = Rules {
    markdown: true,
    ..NONE
};

/// The built-in rules for `language`, used for its files over 16 MB.
#[must_use]
pub fn builtin(language: LanguageId) -> &'static Rules {
    match language {
        LanguageId::Rust => &RUST,
        LanguageId::Markdown => &MARKDOWN,
        LanguageId::Json => &JSON,
        LanguageId::Toml => &TOML,
        LanguageId::Yaml => &YAML,
        LanguageId::Python => &PYTHON,
        LanguageId::JavaScript => &JAVASCRIPT,
        LanguageId::TypeScript => &TYPESCRIPT,
        LanguageId::Bash => &BASH,
    }
}

/// A user-defined language's rules, as read from its file in the config
/// directory (the app parses the file; this checks and stores it).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RuleSpec {
    /// Markers that comment out the rest of the line, such as `--`.
    pub line_comments: Vec<String>,
    /// `#` starts a comment only at the start of a line or after
    /// whitespace.
    pub hash_needs_space: bool,
    /// Opening and closing marker of block comments.
    pub block_comment: Option<(String, String)>,
    /// Block comments nest.
    pub nested_comments: bool,
    /// String literals, checked in order: put longer openers first.
    pub strings: Vec<StringSpec>,
    /// Words coloured as keywords.
    pub keywords: Vec<String>,
    /// Words coloured as constants, such as `true` and `nil`.
    pub constants: Vec<String>,
}

/// One kind of string literal in a [`RuleSpec`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct StringSpec {
    /// Opening delimiter.
    pub open: String,
    /// Closing delimiter.
    pub close: String,
    /// A backslash escapes the next character.
    pub escapes: bool,
    /// May continue onto following lines.
    pub multiline: bool,
}

/// Why a [`RuleSpec`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// A comment marker or string delimiter is empty; it would match
    /// everywhere.
    EmptyDelimiter(&'static str),
    /// More string kinds than the line state can number.
    TooManyStrings(usize),
}

impl std::fmt::Display for RuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyDelimiter(what) => write!(f, "{what} must not be empty"),
            Self::TooManyStrings(n) => write!(f, "{n} string kinds; at most 255"),
        }
    }
}

impl std::error::Error for RuleError {}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

fn leak_all(v: &[String]) -> &'static [&'static str] {
    Box::leak(
        v.iter()
            .map(|s| leak(s))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    )
}

impl Rules {
    /// Check `spec` and turn it into rules.
    ///
    /// Rules live for the rest of the program, like the built-in ones, so
    /// the scanner needs no lifetimes. Each distinct spec is stored once:
    /// reloading an unchanged config reuses it, and a changed one costs a
    /// few kilobytes.
    ///
    /// # Errors
    ///
    /// A [`RuleError`] if a delimiter is empty or there are too many
    /// string kinds.
    pub fn from_spec(spec: &RuleSpec) -> Result<&'static Self, RuleError> {
        use std::collections::HashMap;
        use std::sync::{Mutex, OnceLock};
        static STORED: OnceLock<Mutex<HashMap<RuleSpec, &'static Rules>>> = OnceLock::new();

        if spec.line_comments.iter().any(String::is_empty) {
            return Err(RuleError::EmptyDelimiter("a line comment marker"));
        }
        if spec
            .block_comment
            .as_ref()
            .is_some_and(|(a, b)| a.is_empty() || b.is_empty())
        {
            return Err(RuleError::EmptyDelimiter("a block comment marker"));
        }
        if spec
            .strings
            .iter()
            .any(|s| s.open.is_empty() || s.close.is_empty())
        {
            return Err(RuleError::EmptyDelimiter("a string delimiter"));
        }
        if spec.strings.len() > usize::from(u8::MAX) {
            return Err(RuleError::TooManyStrings(spec.strings.len()));
        }
        let stored = STORED.get_or_init(Mutex::default);
        if let Some(rules) = stored.lock().ok().and_then(|m| m.get(spec).copied()) {
            return Ok(rules);
        }
        let strings: Vec<StringRule> = spec
            .strings
            .iter()
            .map(|s| StringRule {
                open: leak(&s.open),
                close: leak(&s.close),
                escapes: s.escapes,
                multiline: s.multiline,
            })
            .collect();
        let rules: &'static Self = Box::leak(Box::new(Self {
            line_comments: leak_all(&spec.line_comments),
            hash_after_space: spec.hash_needs_space,
            block_comment: spec.block_comment.as_ref().map(|(a, b)| (leak(a), leak(b))),
            nested_comments: spec.nested_comments,
            strings: Box::leak(strings.into_boxed_slice()),
            char_literals: false,
            keywords: leak_all(&spec.keywords),
            constants: leak_all(&spec.constants),
            markdown: false,
        }));
        if let Ok(mut m) = stored.lock() {
            m.insert(spec.clone(), rules);
        }
        Ok(rules)
    }
}

/// What a line starts inside of, carried over from the line before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LexState {
    #[default]
    Normal,
    /// A block comment, nested this deep.
    Comment(u8),
    /// A multi-line string, by its index in the language's string rules.
    Str(u8),
    /// A Markdown fenced code block.
    Fence,
}

/// Scan a block comment from `i`, `depth` levels deep, to its end or the
/// end of the line.
fn scan_comment(r: &Rules, b: &[u8], mut i: usize, mut depth: u8) -> (usize, LexState) {
    let Some((open, close)) = r.block_comment else {
        return (b.len(), LexState::Normal);
    };
    while i < b.len() {
        if b[i..].starts_with(close.as_bytes()) {
            i += close.len();
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return (i, LexState::Normal);
            }
        } else if r.nested_comments && b[i..].starts_with(open.as_bytes()) {
            i += open.len();
            depth = depth.saturating_add(1);
        } else {
            i += 1;
        }
    }
    (b.len(), LexState::Comment(depth))
}

/// Scan a string's body from `i` to just past its closing delimiter, or to
/// the end of the line. Returns the end and whether it closed.
fn scan_string(rule: &StringRule, b: &[u8], mut i: usize) -> (usize, bool) {
    while i < b.len() {
        if rule.escapes && b[i] == b'\\' {
            i += 2;
        } else if b[i..].starts_with(rule.close.as_bytes()) {
            return (i + rule.close.len(), true);
        } else {
            i += 1;
        }
    }
    (b.len(), false)
}

/// The length of a Rust character literal starting at `i` (a `'`), or
/// `None` for a lifetime.
fn char_literal(line: &str, i: usize) -> Option<usize> {
    let rest = line.get(i + 1..)?;
    let first = rest.chars().next()?;
    if first == '\\' {
        // An escape: `'\n'`, `'\''`, `'\u{1F600}'`.
        let close = rest.get(2..)?.find('\'')? + 2;
        (close <= 10).then_some(close + 2)
    } else {
        rest.get(first.len_utf8()..)?
            .starts_with('\'')
            .then_some(first.len_utf8() + 2)
    }
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn markdown_line(line: &str, state: LexState, emit: &mut dyn FnMut(Range<usize>, u32)) -> LexState {
    let trimmed = line.trim_start();
    let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
    if state == LexState::Fence {
        emit(0..line.len(), LITERAL);
        return if fence {
            LexState::Normal
        } else {
            LexState::Fence
        };
    }
    if fence {
        emit(0..line.len(), LITERAL);
        return LexState::Fence;
    }
    if trimmed.starts_with('#') {
        emit(0..line.len(), TITLE);
    }
    LexState::Normal
}

/// Finish on this line what the previous line left open. Returns where
/// ordinary scanning continues, or `None` if it stays open past this line.
fn resume(
    rules: &Rules,
    bytes: &[u8],
    state: LexState,
    emit: &mut dyn FnMut(Range<usize>, u32),
) -> Option<usize> {
    match state {
        LexState::Comment(depth) => {
            let (end, next) = scan_comment(rules, bytes, 0, depth);
            emit(0..end, COMMENT);
            (next == LexState::Normal).then_some(end)
        }
        LexState::Str(k) => {
            let rule = rules.strings.get(usize::from(k))?;
            let (end, closed) = scan_string(rule, bytes, 0);
            emit(0..end, STRING);
            closed.then_some(end)
        }
        LexState::Normal | LexState::Fence => Some(0),
    }
}

/// Highlight one line, given the state it starts in; returns the state the
/// next line starts in. `emit` gets byte ranges within `line`.
fn lex_line(
    rules: &Rules,
    line: &str,
    mut state: LexState,
    emit: &mut dyn FnMut(Range<usize>, u32),
) -> LexState {
    if rules.markdown {
        return markdown_line(line, state, emit);
    }
    let bytes = line.as_bytes();
    let Some(mut i) = resume(rules, bytes, state, emit) else {
        return state;
    };
    state = LexState::Normal;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let ch = bytes[i];
        // Block comments first: an opener may begin with the line
        // comment marker (Lua's `--[[` and `--`).
        if let Some((open, _)) = rules.block_comment
            && rest.starts_with(open.as_bytes())
        {
            let (end, next) = scan_comment(rules, bytes, i + open.len(), 1);
            emit(i..end, COMMENT);
            if next != LexState::Normal {
                return next;
            }
            i = end;
            continue;
        }
        let line_comment = rules.line_comments.iter().any(|m| {
            rest.starts_with(m.as_bytes())
                && !(*m == "#"
                    && rules.hash_after_space
                    && i > 0
                    && !bytes[i - 1].is_ascii_whitespace())
        });
        if line_comment {
            emit(i..bytes.len(), COMMENT);
            return state;
        }
        if let Some(k) = rules
            .strings
            .iter()
            .position(|s| rest.starts_with(s.open.as_bytes()))
        {
            let rule = &rules.strings[k];
            let (end, closed) = scan_string(rule, bytes, i + rule.open.len());
            emit(i..end, STRING);
            if !closed {
                return match u8::try_from(k) {
                    Ok(k) if rule.multiline => LexState::Str(k),
                    _ => LexState::Normal,
                };
            }
            i = end;
            continue;
        }
        if rules.char_literals && ch == b'\'' {
            if let Some(n) = char_literal(line, i) {
                emit(i..i + n, STRING);
                i += n;
            } else {
                i += 1;
            }
            continue;
        }
        let after_word = i > 0 && is_word(bytes[i - 1]);
        if (ch.is_ascii_alphabetic() || ch == b'_') && !after_word {
            let start = i;
            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }
            let word = &line[start..i];
            if rules.keywords.contains(&word) {
                emit(start..i, KEYWORD);
            } else if rules.constants.contains(&word) {
                emit(start..i, CONSTANT);
            }
            continue;
        }
        if ch.is_ascii_digit() && !after_word {
            let start = i;
            while i < bytes.len() && (is_word(bytes[i]) || bytes[i] == b'.') {
                i += 1;
            }
            emit(start..i, NUMBER);
            continue;
        }
        i += 1;
    }
    state
}

/// The state after line `l`, given the state before it. A long line is not
/// read: the state is assumed to pass through it unchanged (an unclosed
/// comment inside a megabyte-long line is lost, by design).
fn carry(r: &Rules, text: &dyn Text, l: usize, state: LexState) -> LexState {
    let start = text.line_to_byte(l);
    if long_line::is_long(text.line_span(start)) {
        return state;
    }
    lex_line(r, &text.line(l), state, &mut |_, _| {})
}

/// Line-by-line highlighting state for one large document.
#[derive(Debug)]
pub struct Lexical {
    rules: &'static Rules,
    /// State at the start of line `k * CHECKPOINT_LINES`, contiguous from
    /// the first line. Filled as lines are scanned; behind a `RefCell`
    /// because highlighting reads the document state immutably.
    checkpoints: RefCell<Vec<LexState>>,
    /// The last state worked out by scanning only the lookback distance:
    /// (line, state). Reused while the view stays near it, so typing far
    /// from any checkpoint does not rescan the lookback on every keystroke.
    near: RefCell<Option<(usize, LexState)>>,
}

impl Lexical {
    /// Highlighting by `rules`: [`builtin`] ones, or a user language's.
    #[must_use]
    pub fn new(rules: &'static Rules) -> Self {
        Self {
            rules,
            checkpoints: RefCell::new(vec![LexState::Normal]),
            near: RefCell::new(None),
        }
    }

    /// Record an edit just applied to `text` (the text after it): states
    /// after its first line may have changed.
    pub fn edit(&self, text: &dyn Text, edit: &Edit) {
        let line = text.byte_to_line(edit.start);
        // The checkpoint at a line depends only on the lines before it.
        self.checkpoints
            .borrow_mut()
            .truncate(line / CHECKPOINT_LINES + 1);
        let mut near = self.near.borrow_mut();
        if near.is_some_and(|(l, _)| l > line) {
            *near = None;
        }
    }

    /// The state line `line` starts in.
    fn state_at(&self, text: &dyn Text, line: usize) -> LexState {
        let r = self.rules;
        let mut cps = self.checkpoints.borrow_mut();
        let k = (line / CHECKPOINT_LINES).min(cps.len().saturating_sub(1));
        let base = k * CHECKPOINT_LINES;
        let target = text.line_to_byte(line);
        if target - text.line_to_byte(base) <= LOOKBACK_BYTES {
            let mut state = cps.get(k).copied().unwrap_or_default();
            for l in base..line {
                if l.is_multiple_of(CHECKPOINT_LINES) && l / CHECKPOINT_LINES == cps.len() {
                    cps.push(state);
                }
                state = carry(r, text, l, state);
            }
            if line.is_multiple_of(CHECKPOINT_LINES) && line / CHECKPOINT_LINES == cps.len() {
                cps.push(state);
            }
            state
        } else {
            // Too far to scan from the last checkpoint. Continue from the
            // last guess if it is above and near; otherwise start the
            // lookback distance up, assuming plain code there.
            let mut near = self.near.borrow_mut();
            let (from, mut state) = match *near {
                Some((l, state))
                    if l <= line && target - text.line_to_byte(l) <= LOOKBACK_BYTES =>
                {
                    (l, state)
                }
                _ => (
                    text.byte_to_line(target - LOOKBACK_BYTES) + 1,
                    LexState::Normal,
                ),
            };
            for l in from..line {
                state = carry(r, text, l, state);
            }
            *near = Some((line, state));
            state
        }
    }

    /// Append spans within `range` of `text` to `out`, sorted and not
    /// overlapping.
    pub fn spans(&self, text: &dyn Text, range: Range<usize>, out: &mut Vec<Span>) {
        if range.is_empty() || text.len_lines() == 0 {
            return;
        }
        let r = self.rules;
        let first = text.byte_to_line(range.start);
        let last = text.byte_to_line(range.end - 1).min(text.len_lines() - 1);
        let mut state = self.state_at(text, first);
        for l in first..=last {
            let start = text.line_to_byte(l);
            let span = text.line_span(start);
            if long_line::is_long(span) {
                // Only the part in `range` is read, lexed from its own
                // start; the state is assumed to pass through the line.
                let (a, b) = (range.start.max(span.start), range.end.min(span.end));
                if a < b {
                    let from = if a == span.start {
                        state
                    } else {
                        LexState::Normal
                    };
                    let part = text.slice(a..b);
                    lex_line(r, &part, from, &mut |sp, capture| {
                        out.push(Span {
                            start: a + sp.start,
                            end: a + sp.end,
                            capture,
                        });
                    });
                }
                continue;
            }
            let line = text.line(l);
            state = lex_line(r, &line, state, &mut |span, capture| {
                let (s, e) = (
                    (start + span.start).max(range.start),
                    (start + span.end).min(range.end),
                );
                if s < e {
                    out.push(Span {
                        start: s,
                        end: e,
                        capture,
                    });
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_core::{Buffer, RopeBuffer};

    /// `(text, capture name)` for each span of `src`.
    fn names(language: LanguageId, src: &str) -> Vec<(String, &'static str)> {
        let buf = RopeBuffer::from_text(src);
        let mut out = Vec::new();
        Lexical::new(builtin(language)).spans(&buf, 0..src.len(), &mut out);
        out.iter()
            .map(|s| (src[s.start..s.end].to_owned(), CAPTURES[s.capture as usize]))
            .collect()
    }

    fn has(spans: &[(String, &str)], text: &str, name: &str) -> bool {
        spans.iter().any(|(t, n)| t == text && *n == name)
    }

    #[test]
    fn rust_basics() {
        let src = "fn main() { let s = \"a \\\" b\"; // done\n    let c = '\"'; let l: &'a str = r; 42 }\n";
        let spans = names(LanguageId::Rust, src);
        assert!(has(&spans, "fn", "keyword"), "{spans:?}");
        assert!(has(&spans, "\"a \\\" b\"", "string"), "{spans:?}");
        assert!(has(&spans, "// done", "comment"), "{spans:?}");
        assert!(has(&spans, "'\"'", "string"), "a char literal: {spans:?}");
        assert!(has(&spans, "42", "number"), "{spans:?}");
        // The lifetime is not a string, so `str` after it stays plain.
        assert!(!spans.iter().any(|(t, _)| t.contains("'a")), "{spans:?}");
        // Words containing keywords are not keywords.
        assert!(!has(
            &names(LanguageId::Rust, "letter fn_x x1"),
            "fn",
            "keyword"
        ));
    }

    #[test]
    fn state_carries_across_lines() {
        let src = "a /* one\n/* nested */ still\nend */ fn\n";
        let spans = names(LanguageId::Rust, src);
        assert!(has(&spans, "/* one", "comment"), "{spans:?}");
        assert!(has(&spans, "/* nested */ still", "comment"), "{spans:?}");
        assert!(has(&spans, "end */", "comment"), "{spans:?}");
        assert!(has(&spans, "fn", "keyword"), "{spans:?}");

        let py = names(
            LanguageId::Python,
            "x = \"\"\"doc\nmore\n\"\"\" if\n'not\nopen'\n",
        );
        assert!(has(&py, "more", "string"), "{py:?}");
        assert!(has(&py, "if", "keyword"), "{py:?}");
        assert!(has(&py, "'not", "string"), "{py:?}");
        assert!(
            !has(&py, "open'", "string"),
            "single quotes end at the line: {py:?}"
        );

        let md = names(
            LanguageId::Markdown,
            "# Title\ntext\n```\ncode # not a title\n```\nafter\n",
        );
        assert!(has(&md, "# Title", "text.title"), "{md:?}");
        assert!(has(&md, "code # not a title", "text.literal"), "{md:?}");
        assert!(!md.iter().any(|(t, _)| t == "after"), "{md:?}");
    }

    #[test]
    fn hash_comments_need_space_in_bash_and_yaml() {
        let sh = names(LanguageId::Bash, "echo $# a#b # c\n");
        assert!(has(&sh, "# c", "comment"), "{sh:?}");
        assert_eq!(
            sh.iter().filter(|(_, n)| *n == "comment").count(),
            1,
            "{sh:?}"
        );
        let py = names(LanguageId::Python, "x#c\n");
        assert!(has(&py, "#c", "comment"), "{py:?}");
    }

    #[test]
    fn spans_are_clipped_to_the_range() {
        let src = "let x = \"long string\";\n";
        let buf = RopeBuffer::from_text(src);
        let mut out = Vec::new();
        Lexical::new(builtin(LanguageId::Rust)).spans(&buf, 10..15, &mut out);
        assert_eq!(
            out,
            vec![Span {
                start: 10,
                end: 15,
                capture: STRING
            }]
        );
    }

    #[test]
    fn checkpoints_follow_edits() {
        // Many lines, so checkpoints are used; a comment opened near the
        // top must reach the bottom, and closing it must too.
        let body = "let x = 1;\n".repeat(3 * CHECKPOINT_LINES);
        let mut buf = RopeBuffer::from_text(&body);
        let lex = Lexical::new(builtin(LanguageId::Rust));
        let last = buf.line_to_byte(buf.len_lines() - 2);
        let bottom = |lex: &Lexical, buf: &RopeBuffer| {
            let mut out = Vec::new();
            lex.spans(buf, last..last + 3, &mut out);
            out.first().map(|s| CAPTURES[s.capture as usize])
        };
        assert_eq!(bottom(&lex, &buf), Some("keyword"));
        assert!(lex.checkpoints.borrow().len() >= 3);
        let edit = buf.replace(5..5, "/*").unwrap();
        lex.edit(&buf, &edit);
        assert_eq!(
            lex.checkpoints.borrow().len(),
            1,
            "everything after line 0 dropped"
        );
        assert_eq!(bottom(&lex, &buf), Some("comment"));
        let edit = buf.replace(5..7, "").unwrap();
        lex.edit(&buf, &edit);
        assert_eq!(bottom(&lex, &buf), Some("keyword"));
    }

    #[test]
    fn far_jumps_scan_only_a_bounded_distance() {
        // A comment opened at the top, then more than the lookback of code:
        // far below, the state is assumed plain rather than scanned for.
        let mut src = String::from("/* never closed\n");
        src.push_str(&"let x = 1;\n".repeat(LOOKBACK_BYTES / 11 + 2 * CHECKPOINT_LINES));
        let buf = RopeBuffer::from_text(&src);
        let lex = Lexical::new(builtin(LanguageId::Rust));
        let line = buf.len_lines() - 2;
        assert_eq!(lex.state_at(&buf, line), LexState::Normal, "approximate");
        assert_eq!(
            lex.checkpoints.borrow().len(),
            1,
            "nothing saved from a guess"
        );
        // Scanning down from the top (as scrolling does) is exact.
        for l in (0..=line).step_by(CHECKPOINT_LINES / 2) {
            lex.state_at(&buf, l);
        }
        assert_eq!(lex.state_at(&buf, line), LexState::Comment(1));
    }

    #[test]
    fn a_far_guess_is_kept_while_typing_below_it() {
        let src = "let x = 1;\n".repeat(LOOKBACK_BYTES / 11 + 4 * CHECKPOINT_LINES);
        let mut buf = RopeBuffer::from_text(&src);
        let lex = Lexical::new(builtin(LanguageId::Rust));
        let top = buf.len_lines() - 100;
        lex.state_at(&buf, top);
        assert_eq!(*lex.near.borrow(), Some((top, LexState::Normal)));
        // Typing on screen keeps it; an edit above it (or above the
        // checkpoints) drops it.
        let at = buf.line_to_byte(top + 10);
        let edit = buf.replace(at..at, "/*").unwrap();
        lex.edit(&buf, &edit);
        assert_eq!(*lex.near.borrow(), Some((top, LexState::Normal)));
        assert_eq!(lex.state_at(&buf, top + 11), LexState::Comment(1));
        let at = buf.line_to_byte(top - 5);
        let edit = buf.replace(at..at, "x").unwrap();
        lex.edit(&buf, &edit);
        assert_eq!(*lex.near.borrow(), None);
    }

    #[test]
    fn user_rules_work_and_are_checked() {
        let spec = RuleSpec {
            line_comments: vec!["--".to_owned()],
            block_comment: Some(("--[[".to_owned(), "]]".to_owned())),
            strings: vec![StringSpec {
                open: "\"".to_owned(),
                close: "\"".to_owned(),
                escapes: true,
                multiline: false,
            }],
            keywords: vec!["local".to_owned(), "end".to_owned()],
            constants: vec!["nil".to_owned()],
            ..RuleSpec::default()
        };
        let rules = Rules::from_spec(&spec).unwrap();
        assert!(
            std::ptr::eq(rules, Rules::from_spec(&spec).unwrap()),
            "stored once"
        );
        let src = "local x = nil -- note\n--[[ a\nb ]] end \"s\"\n";
        let buf = RopeBuffer::from_text(src);
        let mut out = Vec::new();
        Lexical::new(rules).spans(&buf, 0..src.len(), &mut out);
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|s| (&src[s.start..s.end], CAPTURES[s.capture as usize]))
            .collect();
        assert_eq!(
            got,
            [
                ("local", "keyword"),
                ("nil", "constant.builtin"),
                ("-- note", "comment"),
                ("--[[ a", "comment"),
                ("b ]]", "comment"),
                ("end", "keyword"),
                ("\"s\"", "string"),
            ]
        );
        let empty = RuleSpec {
            strings: vec![StringSpec::default()],
            ..RuleSpec::default()
        };
        assert_eq!(
            Rules::from_spec(&empty).unwrap_err(),
            RuleError::EmptyDelimiter("a string delimiter")
        );
    }

    #[test]
    fn every_language_has_rules() {
        for id in LanguageId::ALL {
            let r = builtin(id);
            let some = r.markdown || !r.strings.is_empty() || !r.line_comments.is_empty();
            assert!(some, "{}", id.name());
        }
    }
}
