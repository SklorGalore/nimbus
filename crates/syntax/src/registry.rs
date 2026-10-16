//! Grammars and their highlight queries, loaded the first time a language
//! is needed.
//!
//! Compiling a highlight query is the expensive part of setting up a
//! language (milliseconds to tens of milliseconds for the larger grammars),
//! so it never happens at startup: each language sits behind a `OnceLock`,
//! filled on first use, normally on a worker thread doing a file's first
//! parse.

use std::sync::OnceLock;

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
                feature = "bash"
            ),
            expect(
                unreachable_patterns,
                reason = "reachable only when a grammar feature is off"
            )
        )]
        _ => None,
    }
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
            let (language, query) = source(id)?;
            match Query::new(&language, &query) {
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
