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

/// A language ready to parse and highlight.
pub struct Grammar {
    /// The parser tables.
    pub language: Language,
    /// The highlight query: patterns that tag syntax nodes with capture
    /// names such as `keyword` or `string`.
    pub highlights: Query,
}

impl std::fmt::Debug for Grammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammar")
            .field("captures", &self.highlights.capture_names().len())
            .finish_non_exhaustive()
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

/// The compiled tags query for `id`, loaded on first use like the
/// grammar: `None` when the language has none, or it fails to compile.
/// Users replace or extend it with `queries/<key>.tags.scm`.
#[must_use]
pub fn symbols_query(id: LanguageId) -> Option<&'static Query> {
    static SLOTS: [OnceLock<Option<Query>>; LanguageId::ALL.len()] =
        [const { OnceLock::new() }; LanguageId::ALL.len()];
    SLOTS
        .get(id.index())?
        .get_or_init(|| {
            let grammar = grammar(id)?;
            let builtin = tags_source(id)?;
            let file = format!("{}.tags.scm", id.key());
            let user = USER_QUERIES
                .get()
                .and_then(|dir| user_query(dir, &file, &builtin));
            match compile(&grammar.language, &builtin, user) {
                Ok(q) => Some(q),
                Err(e) => {
                    log::error!("{} tags query does not compile: {e}", id.name());
                    None
                }
            }
        })
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

fn slot(id: LanguageId) -> &'static OnceLock<Option<Grammar>> {
    static SLOTS: [OnceLock<Option<Grammar>>; LanguageId::ALL.len()] =
        [const { OnceLock::new() }; LanguageId::ALL.len()];
    &SLOTS[id.index()]
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
    slot(id)
        .get_or_init(|| {
            let started = std::time::Instant::now();
            let (language, builtin) = source(id)?;
            let file = format!("{}.scm", id.key());
            let user = USER_QUERIES
                .get()
                .and_then(|dir| user_query(dir, &file, &builtin));
            let compiled = compile(&language, &builtin, user);
            match compiled {
                Ok(highlights) => {
                    log::debug!("{} grammar ready in {:?}", id.name(), started.elapsed());
                    Some(Grammar {
                        language,
                        highlights,
                    })
                }
                Err(e) => {
                    log::error!("{} highlight query does not compile: {e}", id.name());
                    None
                }
            }
        })
        .as_ref()
}

/// Whether `id`'s grammar is loaded already.
#[cfg(test)]
fn is_loaded(id: LanguageId) -> bool {
    slot(id).get().is_some()
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
