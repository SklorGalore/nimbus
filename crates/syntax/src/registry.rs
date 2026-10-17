//! Grammars and their highlight queries, loaded the first time a language
//! is needed.
//!
//! Compiling a highlight query is the expensive part of setting up a
//! language (milliseconds to tens of milliseconds for the larger grammars),
//! so it never happens at startup: each language sits behind a `OnceLock`,
//! filled on first use, normally on a worker thread doing a file's first
//! parse.
//!
//! A user can replace a language's query with `queries/<key>.scm` in a
//! folder the app names with [`set_user_queries`], or add to it by starting
//! the file with `;; extends`. A user query that does not compile is
//! reported through [`take_warnings`] and the built-in one is used. The
//! other kinds of query a grammar can have (M14) live beside it as
//! `queries/<key>.tags.scm` and `queries/<key>.injections.scm`, with the
//! same rules.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use tree_sitter::{Language, Query};

use crate::LanguageId;

/// Which grammar a tree was parsed with. Every [`LanguageId`] has one;
/// Markdown has a second, for the inline structure of each paragraph
/// (emphasis, code spans, links), which only ever runs as an injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrammarKey {
    /// A language's own grammar.
    Language(LanguageId),
    /// `tree-sitter-markdown-inline`, injected into Markdown's `inline`
    /// nodes.
    MarkdownInline,
}

/// One more capture table than the grammars: the simple rules' captures
/// ([`crate::lexical::CAPTURES`]).
pub const LEXICAL_TABLE: usize = LanguageId::ALL.len();
/// The table of the Markdown inline grammar.
pub const INLINE_TABLE: usize = LanguageId::ALL.len() + 1;
/// How many capture tables there are: a [`crate::highlight::Span::table`]
/// is below this.
pub const TABLES: usize = LanguageId::ALL.len() + 2;

impl GrammarKey {
    /// The capture table spans from this grammar index into: the
    /// language's index, or [`INLINE_TABLE`].
    #[must_use]
    pub fn table(self) -> usize {
        match self {
            Self::Language(id) => id.index(),
            Self::MarkdownInline => INLINE_TABLE,
        }
    }

    /// The grammar an injection query names: a language key or alias
    /// (`"rust"`, `"py"`), or `"markdown_inline"`. `None` for a language
    /// Nimbus has no grammar for (`"latex"`, `"regex"`).
    #[must_use]
    pub fn from_injection_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("markdown_inline") {
            return Some(Self::MarkdownInline);
        }
        LanguageId::from_alias(name).map(Self::Language)
    }
}

/// A language ready to parse and highlight.
pub struct Grammar {
    /// The parser tables.
    pub language: Language,
    /// The highlight query: patterns that tag syntax nodes with capture
    /// names such as `keyword` or `string`.
    pub highlights: Query,
    /// Where other languages are nested inside this one, if the grammar
    /// says (M14).
    pub injections: Option<Injections>,
}

impl std::fmt::Debug for Grammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammar")
            .field("captures", &self.highlights.capture_names().len())
            .field("injections", &self.injections.is_some())
            .finish_non_exhaustive()
    }
}

/// A compiled injections query and what its patterns ask for.
///
/// The query captures each nested region as `@injection.content`, and
/// names its language either with `(#set! injection.language "css")` on
/// the pattern, or by capturing a node whose text is the name as
/// `@injection.language` (a code fence's info string).
pub struct Injections {
    /// The query.
    pub query: Query,
    /// Capture index of `@injection.content`.
    pub content: u32,
    /// Capture index of `@injection.language`, if any pattern uses it.
    pub language: Option<u32>,
    /// Per pattern: the grammar `#set! injection.language` names, if it
    /// is set and known. A pattern with neither this nor a language
    /// capture never injects.
    pub fixed: Vec<Option<GrammarKey>>,
}

impl Injections {
    /// Read a compiled query's patterns. `None` when it has no
    /// `@injection.content` capture, so it could never inject anything.
    fn new(query: Query) -> Option<Self> {
        let content = query.capture_index_for_name("injection.content")?;
        let language = query.capture_index_for_name("injection.language");
        let fixed = (0..query.pattern_count())
            .map(|i| {
                query.property_settings(i).iter().find_map(|p| {
                    (&*p.key == "injection.language")
                        .then(|| p.value.as_deref().and_then(GrammarKey::from_injection_name))
                        .flatten()
                })
            })
            .collect();
        Some(Self {
            query,
            content,
            language,
            fixed,
        })
    }
}

/// The grammar and highlight query source for a language, or `None` if
/// its cargo feature is off. Queries are concatenated where a grammar
/// builds on another: TypeScript's own query covers only TypeScript
/// additions, so JavaScript's comes first and TypeScript's after it (later
/// patterns win).
#[expect(
    clippy::too_many_lines,
    reason = "one arm per grammar; splitting it would hide the list"
)]
fn source(id: LanguageId) -> Option<(Language, String)> {
    match id {
        #[cfg(feature = "rust")]
        LanguageId::Rust => Some((
            tree_sitter_rust::LANGUAGE.into(),
            include_str!("../queries/rust.scm").to_owned(),
        )),
        #[cfg(feature = "markdown")]
        LanguageId::Markdown => Some((
            tree_sitter_md::LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_owned(),
        )),
        #[cfg(feature = "json")]
        LanguageId::Json => Some((
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY.to_owned(),
        )),
        #[cfg(feature = "toml")]
        LanguageId::Toml => Some((
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_owned(),
        )),
        #[cfg(feature = "yaml")]
        LanguageId::Yaml => Some((
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY.to_owned(),
        )),
        #[cfg(feature = "python")]
        LanguageId::Python => Some((
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY.to_owned(),
        )),
        #[cfg(feature = "javascript")]
        LanguageId::JavaScript => Some((
            tree_sitter_javascript::LANGUAGE.into(),
            format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            ),
        )),
        #[cfg(feature = "typescript")]
        LanguageId::TypeScript => Some((
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
        )),
        #[cfg(feature = "bash")]
        LanguageId::Bash => Some((
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY.to_owned(),
        )),
        #[cfg(feature = "c")]
        LanguageId::C => Some((
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY.to_owned(),
        )),
        #[cfg(feature = "go")]
        LanguageId::Go => Some((
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY.to_owned(),
        )),
        #[cfg(feature = "html")]
        LanguageId::Html => Some((
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY.to_owned(),
        )),
        #[cfg(feature = "css")]
        LanguageId::Css => Some((
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY.to_owned(),
        )),
        // TSX is TypeScript plus JSX, so its query is JavaScript's, then
        // JSX's, then TypeScript's additions.
        #[cfg(feature = "tsx")]
        LanguageId::Tsx => Some((
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            format!(
                "{}\n{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
        )),
        #[cfg_attr(
            all(
                feature = "rust",
                feature = "markdown",
                feature = "json",
                feature = "toml",
                feature = "yaml",
                feature = "python",
                feature = "javascript",
                feature = "typescript",
                feature = "bash",
                feature = "c",
                feature = "go",
                feature = "html",
                feature = "css",
                feature = "tsx"
            ),
            expect(
                unreachable_patterns,
                reason = "reachable only when a grammar feature is off"
            )
        )]
        _ => None,
    }
}

/// The injections query for a grammar, where it ships one that names
/// languages Nimbus has. Rust's (macro bodies reparsed as Rust) is left
/// out: the highlight query already colours the tokens in a macro call,
/// and a child tree per `println!` would cost more than it shows.
/// JavaScript's (tagged template strings such as `` css`...` ``) also
/// serves TypeScript and TSX, whose grammars share those node names.
fn injections_source(key: GrammarKey) -> Option<&'static str> {
    match key {
        #[cfg(feature = "markdown")]
        GrammarKey::Language(LanguageId::Markdown) => Some(tree_sitter_md::INJECTION_QUERY_BLOCK),
        #[cfg(feature = "markdown")]
        GrammarKey::MarkdownInline => Some(tree_sitter_md::INJECTION_QUERY_INLINE),
        #[cfg(feature = "html")]
        GrammarKey::Language(LanguageId::Html) => Some(tree_sitter_html::INJECTIONS_QUERY),
        #[cfg(feature = "javascript")]
        GrammarKey::Language(LanguageId::JavaScript | LanguageId::TypeScript | LanguageId::Tsx) => {
            Some(tree_sitter_javascript::INJECTIONS_QUERY)
        }
        _ => None,
    }
}

/// The capture names of a table (see [`TABLES`]): a grammar's, once it
/// is loaded, or the simple rules'. `None` for a grammar not loaded yet,
/// whose spans cannot exist either.
#[must_use]
pub fn capture_table(table: usize) -> Option<&'static [&'static str]> {
    if table == LEXICAL_TABLE {
        return Some(crate::lexical::CAPTURES);
    }
    let key = if table == INLINE_TABLE {
        GrammarKey::MarkdownInline
    } else {
        GrammarKey::Language(*LanguageId::ALL.get(table)?)
    };
    let slot = slot(key);
    Some(slot.get()?.as_ref()?.highlights.capture_names())
}

/// The tags query for a language: patterns that capture definitions
/// (`@definition.function` with its `@name`), the convention code
/// navigation tools share. From the grammar crate where it ships one,
/// with Nimbus's additions for constants; vendored for Markdown. `None`
/// for data formats and for languages whose feature is off.
fn tags_source(id: LanguageId) -> Option<String> {
    match id {
        #[cfg(feature = "rust")]
        LanguageId::Rust => Some(include_str!("../queries/rust-tags.scm").to_owned()),
        #[cfg(feature = "markdown")]
        LanguageId::Markdown => Some(include_str!("../queries/markdown-tags.scm").to_owned()),
        #[cfg(feature = "python")]
        LanguageId::Python => Some(tree_sitter_python::TAGS_QUERY.to_owned()),
        #[cfg(feature = "javascript")]
        LanguageId::JavaScript => Some(tree_sitter_javascript::TAGS_QUERY.to_owned()),
        // TypeScript's tags cover only its additions, like its highlights.
        #[cfg(feature = "typescript")]
        LanguageId::TypeScript | LanguageId::Tsx => Some(format!(
            "{}\n{}",
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY
        )),
        #[cfg(feature = "c")]
        LanguageId::C => Some(tree_sitter_c::TAGS_QUERY.to_owned()),
        #[cfg(feature = "go")]
        LanguageId::Go => Some(format!(
            "{}\n(const_declaration (const_spec name: (identifier) @name) @definition.constant)\n",
            tree_sitter_go::TAGS_QUERY
        )),
        _ => None,
    }
}

/// The fold query for a language: nodes captured as `@fold` are regions
/// M13's folding can collapse. All vendored in `queries/folds/`;
/// TypeScript's and TSX's are JavaScript's plus TypeScript's additions.
fn folds_source(id: LanguageId) -> String {
    let own = match id {
        LanguageId::Rust => include_str!("../queries/folds/rust.scm"),
        LanguageId::Markdown => include_str!("../queries/folds/markdown.scm"),
        LanguageId::Json => include_str!("../queries/folds/json.scm"),
        LanguageId::Toml => include_str!("../queries/folds/toml.scm"),
        LanguageId::Yaml => include_str!("../queries/folds/yaml.scm"),
        LanguageId::Python => include_str!("../queries/folds/python.scm"),
        LanguageId::JavaScript | LanguageId::TypeScript | LanguageId::Tsx => {
            let js = include_str!("../queries/folds/javascript.scm");
            let ts = include_str!("../queries/folds/typescript.scm");
            let jsx = include_str!("../queries/folds/jsx.scm");
            return match id {
                LanguageId::JavaScript => format!("{js}\n{jsx}"),
                LanguageId::TypeScript => format!("{js}\n{ts}"),
                _ => format!("{js}\n{ts}\n{jsx}"),
            };
        }
        LanguageId::Bash => include_str!("../queries/folds/bash.scm"),
        LanguageId::C => include_str!("../queries/folds/c.scm"),
        LanguageId::Go => include_str!("../queries/folds/go.scm"),
        LanguageId::Html => include_str!("../queries/folds/html.scm"),
        LanguageId::Css => include_str!("../queries/folds/css.scm"),
    };
    own.to_owned()
}

/// A query loaded on first use beside the grammar: the tags or folds
/// query, from `builtin` unless the user's `<key>.<kind>.scm` replaces
/// or extends it.
fn side_query(id: LanguageId, kind: &str, builtin: Option<String>) -> Option<Query> {
    let grammar = grammar(id)?;
    let builtin = builtin?;
    let file = format!("{}.{kind}.scm", id.key());
    let user = USER_QUERIES
        .get()
        .and_then(|dir| user_query(dir, &file, &builtin));
    match compile(&grammar.language, &builtin, user) {
        Ok(q) => Some(q),
        Err(e) => {
            log::error!("{} {kind} query does not compile: {e}", id.name());
            None
        }
    }
}

/// The compiled fold query for `id`, loaded on first use like the
/// grammar: `None` when it fails to compile. Users replace or extend it
/// with `queries/<key>.folds.scm`.
#[must_use]
pub fn folds_query(id: LanguageId) -> Option<&'static Query> {
    static SLOTS: [OnceLock<Option<Query>>; LanguageId::ALL.len()] =
        [const { OnceLock::new() }; LanguageId::ALL.len()];
    SLOTS
        .get(id.index())?
        .get_or_init(|| side_query(id, "folds", Some(folds_source(id))))
        .as_ref()
}

/// The compiled tags query for `id`, loaded on first use like the
/// grammar: `None` when the language has none, or it fails to compile.
/// Users replace or extend it with `queries/<key>.tags.scm`.
#[must_use]
pub fn symbols_query(id: LanguageId) -> Option<&'static Query> {
    static SLOTS: [OnceLock<Option<Query>>; LanguageId::ALL.len()] =
        [const { OnceLock::new() }; LanguageId::ALL.len()];
    SLOTS
        .get(id.index())?
        .get_or_init(|| side_query(id, "tags", tags_source(id)))
        .as_ref()
}

static USER_QUERIES: OnceLock<PathBuf> = OnceLock::new();
static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Look for user queries in `dir` (`dir/rust.scm`, ...). Call once, before
/// any grammar loads; later calls are ignored, since loaded queries are
/// never recompiled (a change takes effect on restart).
pub fn set_user_queries(dir: PathBuf) {
    let _ = USER_QUERIES.set(dir);
}

/// Problems found while loading user queries since the last call.
#[must_use]
pub fn take_warnings() -> Vec<String> {
    WARNINGS
        .lock()
        .map(|mut w| std::mem::take(&mut *w))
        .unwrap_or_default()
}

fn warn(message: String) {
    log::warn!("{message}");
    if let Ok(mut w) = WARNINGS.lock() {
        w.push(message);
    }
}

/// The user's query in `dir/file`, if there is one: the whole query, or
/// the built-in followed by the user's additions for an `;; extends` file.
fn user_query(dir: &std::path::Path, file: &str, builtin: &str) -> Option<(PathBuf, String)> {
    let path = dir.join(file);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn(format!("cannot read {}: {e}", path.display()));
            return None;
        }
    };
    let extends = text
        .lines()
        .next()
        .is_some_and(|l| l.trim() == ";; extends");
    let query = if extends {
        format!("{builtin}\n{text}")
    } else {
        text
    };
    Some((path, query))
}

/// The user's query if it compiles, else the built-in one.
fn compile(
    language: &Language,
    builtin: &str,
    user: Option<(PathBuf, String)>,
) -> Result<Query, tree_sitter::QueryError> {
    if let Some((path, query)) = user {
        match Query::new(language, &query) {
            Ok(q) => return Ok(q),
            Err(e) => warn(format!("{}: {e}; using the built-in query", path.display())),
        }
    }
    Query::new(language, builtin)
}

fn slot(key: GrammarKey) -> &'static OnceLock<Option<Grammar>> {
    static SLOTS: [OnceLock<Option<Grammar>>; LanguageId::ALL.len() + 1] =
        [const { OnceLock::new() }; LanguageId::ALL.len() + 1];
    &SLOTS[match key {
        GrammarKey::Language(id) => id.index(),
        GrammarKey::MarkdownInline => LanguageId::ALL.len(),
    }]
}

/// The grammar for `id`, compiling its query on first use. `None` if its
/// feature is off, or the query does not compile against the grammar (a
/// version mismatch between a grammar crate and its query file), which is
/// logged once; the file is then shown without highlighting.
///
/// May block while another thread compiles the same query, so it is called
/// from parse jobs on worker threads. The main thread reaches a loaded
/// grammar through the tree it parsed ([`crate::state::SyntaxState::tree`]).
#[must_use]
pub fn grammar(id: LanguageId) -> Option<&'static Grammar> {
    grammar_for(GrammarKey::Language(id))
}

/// The grammar behind `key`; see [`grammar`]. The Markdown inline grammar
/// loads here too, the first time a Markdown paragraph is on screen.
#[must_use]
pub fn grammar_for(key: GrammarKey) -> Option<&'static Grammar> {
    slot(key)
        .get_or_init(|| {
            let started = std::time::Instant::now();
            let (name, file_key, language, builtin) = match key {
                GrammarKey::Language(id) => {
                    let (language, builtin) = source(id)?;
                    (id.name(), id.key(), language, builtin)
                }
                GrammarKey::MarkdownInline => {
                    let (language, builtin) = inline_source()?;
                    ("Markdown inline", "markdown_inline", language, builtin)
                }
            };
            let file = format!("{file_key}.scm");
            let user = USER_QUERIES
                .get()
                .and_then(|dir| user_query(dir, &file, &builtin));
            let highlights = match compile(&language, &builtin, user) {
                Ok(q) => q,
                Err(e) => {
                    log::error!("{name} highlight query does not compile: {e}");
                    return None;
                }
            };
            let injections = injections_source(key).and_then(|builtin| {
                let file = format!("{file_key}.injections.scm");
                let user = USER_QUERIES
                    .get()
                    .and_then(|dir| user_query(dir, &file, builtin));
                match compile(&language, builtin, user) {
                    Ok(q) => Injections::new(q),
                    Err(e) => {
                        log::error!("{name} injections query does not compile: {e}");
                        None
                    }
                }
            });
            log::debug!("{name} grammar ready in {:?}", started.elapsed());
            Some(Grammar {
                language,
                highlights,
                injections,
            })
        })
        .as_ref()
}

/// The Markdown inline grammar and its highlight query.
#[cfg_attr(
    feature = "markdown",
    expect(
        clippy::unnecessary_wraps,
        reason = "None when the markdown feature is off"
    )
)]
fn inline_source() -> Option<(Language, String)> {
    #[cfg(feature = "markdown")]
    {
        Some((
            tree_sitter_md::INLINE_LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_INLINE.to_owned(),
        ))
    }
    #[cfg(not(feature = "markdown"))]
    {
        None
    }
}

/// Whether `id`'s grammar is loaded already.
#[cfg(test)]
fn is_loaded(id: LanguageId) -> bool {
    slot(GrammarKey::Language(id)).get().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_query_compiles_against_its_grammar() {
        for id in LanguageId::ALL {
            if source(id).is_none() {
                assert!(grammar(id).is_none(), "{} feature is off", id.name());
                continue;
            }
            let started = std::time::Instant::now();
            let g = grammar(id).unwrap_or_else(|| panic!("{} query failed", id.name()));
            println!(
                "{:<10} {:>4} captures, compiled in {:?}",
                id.name(),
                g.highlights.capture_names().len(),
                started.elapsed()
            );
            assert_ne!(g.highlights.capture_names().len(), 0);
            assert!(is_loaded(id));
        }
    }

    #[cfg(feature = "json")]
    #[test]
    fn user_queries_replace_extend_or_fall_back() {
        let dir = std::env::temp_dir().join(format!("nimbus-queries-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (language, builtin) = source(LanguageId::Json).unwrap();
        let names = |user| {
            let q = compile(&language, &builtin, user).unwrap();
            q.capture_names()
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(user_query(&dir, "json.scm", &builtin), None, "no file");

        std::fs::write(dir.join("json.scm"), "(string) @mine\n").unwrap();
        let user = user_query(&dir, "json.scm", &builtin);
        assert_eq!(names(user), ["mine"], "replaces");

        std::fs::write(dir.join("json.scm"), ";; extends\n(string) @mine\n").unwrap();
        let user = user_query(&dir, "json.scm", &builtin);
        let got = names(user);
        assert!(
            got.contains(&"mine".to_owned()) && got.contains(&"string".to_owned()),
            "{got:?}"
        );

        std::fs::write(dir.join("json.scm"), "(not_a_node) @x\n").unwrap();
        let _ = take_warnings();
        let user = user_query(&dir, "json.scm", &builtin);
        assert!(names(user).contains(&"string".to_owned()), "falls back");
        assert_eq!(take_warnings().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn injection_queries_compile_and_name_known_grammars() {
        let mut keys: Vec<GrammarKey> = LanguageId::ALL
            .into_iter()
            .map(GrammarKey::Language)
            .collect();
        keys.push(GrammarKey::MarkdownInline);
        for key in keys {
            let Some(g) = grammar_for(key) else {
                continue;
            };
            assert_eq!(
                g.injections.is_some(),
                injections_source(key).is_some(),
                "{key:?} injections"
            );
            if let Some(inj) = &g.injections {
                assert!(
                    inj.fixed.iter().any(Option::is_some) || inj.language.is_some(),
                    "{key:?} injects nothing"
                );
            }
        }
        #[cfg(feature = "markdown")]
        {
            let md = grammar(LanguageId::Markdown).and_then(|g| g.injections.as_ref());
            let md = md.expect("markdown injections");
            assert!(md.fixed.contains(&Some(GrammarKey::MarkdownInline)));
            assert!(
                md.language.is_some(),
                "fenced code blocks name their language"
            );
            assert!(grammar_for(GrammarKey::MarkdownInline).is_some());
            assert!(capture_table(INLINE_TABLE).is_some_and(|n| n.contains(&"text.emphasis")));
        }
        assert_eq!(capture_table(LEXICAL_TABLE), Some(crate::lexical::CAPTURES));
        assert_eq!(capture_table(TABLES), None);
        assert_eq!(
            GrammarKey::from_injection_name("markdown_inline"),
            Some(GrammarKey::MarkdownInline)
        );
        assert_eq!(GrammarKey::from_injection_name("regex"), None);
    }

    #[test]
    fn every_fold_query_compiles() {
        for id in LanguageId::ALL {
            if grammar(id).is_none() {
                continue;
            }
            let q = folds_query(id).unwrap_or_else(|| panic!("{} folds query", id.name()));
            assert_eq!(q.capture_names(), ["fold"], "{}", id.name());
        }
    }

    #[test]
    fn every_tags_query_compiles() {
        for id in LanguageId::ALL {
            let has_source = tags_source(id).is_some();
            let compiled = symbols_query(id).is_some();
            assert_eq!(has_source, compiled, "{} tags query", id.name());
            if let Some(q) = symbols_query(id) {
                assert!(q.capture_index_for_name("name").is_some(), "{}", id.name());
            }
        }
    }

    /// Prints every capture name, for writing themes.
    #[test]
    #[ignore = "diagnostic"]
    fn list_capture_names() {
        let mut all = std::collections::BTreeSet::new();
        for id in LanguageId::ALL {
            if let Some(g) = grammar(id) {
                all.extend(g.highlights.capture_names().iter().copied());
            }
        }
        println!("{}", all.into_iter().collect::<Vec<_>>().join(" "));
    }
}
