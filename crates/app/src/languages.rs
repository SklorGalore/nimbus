//! Which language a file is, including the user's own additions.
//!
//! Three things can name a file's language, tried in this order:
//!
//! 1. the `[languages]` table in the config, mapping a file name or a
//!    `*.ext` pattern to a language (`"Justfile" = "bash"`);
//! 2. the user's simple-rule languages: `languages/NAME.toml` files in the
//!    config directory, each listing its own extensions and file names;
//! 3. the built-in detection by extension, file name and `#!` line.
//!
//! User-defined languages use the simple line-by-line rules
//! (`nimbus_syntax::lexical`), not a tree-sitter grammar, at any file size.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use nimbus_syntax::LanguageId;
use nimbus_syntax::lexical::{RuleSpec, Rules, StringSpec};

/// What highlights a file.
#[derive(Debug, Clone, Copy)]
pub enum Detected {
    /// A built-in language, with its tree-sitter grammar.
    Builtin(LanguageId),
    /// A user-defined language: its name and simple rules.
    Simple(&'static str, &'static Rules),
}

impl Detected {
    /// A name that changes exactly when the language does.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Builtin(id) => id.key(),
            Self::Simple(name, _) => name,
        }
    }
}

/// A user language's file, as written. Every field is optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LanguageFile {
    extensions: Vec<String>,
    filenames: Vec<String>,
    line_comments: Vec<String>,
    hash_needs_space: bool,
    block_comment: Option<[String; 2]>,
    nested_comments: bool,
    strings: Vec<StringEntry>,
    keywords: Vec<String>,
    constants: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StringEntry {
    open: String,
    /// The opener if left out.
    close: Option<String>,
    #[serde(default = "yes")]
    escapes: bool,
    #[serde(default)]
    multiline: bool,
}

fn yes() -> bool {
    true
}

/// A starting point for a new language file, written by nothing yet but
/// shown in the docs.
#[cfg(test)]
const EXAMPLE: &str = r#"
extensions = ["lua"]
filenames = []
line_comments = ["--"]
block_comment = ["--[[", "]]"]
strings = [
    { open = "[[", close = "]]", escapes = false, multiline = true },
    { open = "\"" },
    { open = "'" },
]
keywords = ["and", "do", "else", "end", "for", "function", "if", "local", "return", "then", "while"]
constants = ["true", "false", "nil"]
"#;

/// The languages a file can be, built from the config and the config
/// directory.
#[derive(Debug, Default)]
pub struct Languages {
    /// Exact file names from `[languages]`.
    names: HashMap<String, Detected>,
    /// `*.suffix` patterns from `[languages]`, longest first.
    suffixes: Vec<(String, Detected)>,
    /// Extensions (lower case) and file names claimed by user languages.
    user_extensions: HashMap<String, Detected>,
    user_names: HashMap<String, Detected>,
}

/// Where user language files live: `languages/` next to the config file.
#[must_use]
pub fn dir() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("languages")))
}

/// Where user highlight queries live: `queries/` next to the config file.
#[must_use]
pub fn queries_dir() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("queries")))
}

/// Read one user language file. Errors are warnings, so a broken file
/// costs only that language.
fn read_language(name: &str, text: &str) -> Result<(LanguageFile, &'static Rules), String> {
    let file: LanguageFile = toml::from_str(text).map_err(|e| {
        let at = e
            .span()
            .map(|r| {
                format!(
                    "line {}: ",
                    text[..r.start.min(text.len())].matches('\n').count() + 1
                )
            })
            .unwrap_or_default();
        format!("{at}{}", e.message())
    })?;
    let spec = RuleSpec {
        line_comments: file.line_comments.clone(),
        hash_needs_space: file.hash_needs_space,
        block_comment: file.block_comment.clone().map(|[a, b]| (a, b)),
        nested_comments: file.nested_comments,
        strings: file
            .strings
            .iter()
            .map(|s| StringSpec {
                open: s.open.clone(),
                close: s.close.clone().unwrap_or_else(|| s.open.clone()),
                escapes: s.escapes,
                multiline: s.multiline,
            })
            .collect(),
        keywords: file.keywords.clone(),
        constants: file.constants.clone(),
    };
    let rules = Rules::from_spec(&spec).map_err(|e| format!("{name}: {e}"))?;
    Ok((file, rules))
}

impl Languages {
    /// Build from the config's `[languages]` table and the language files
    /// in `dir` (if any). Returns warnings for anything that was skipped.
    #[must_use]
    pub fn new(table: &HashMap<String, String>, dir: Option<&Path>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut this = Self::default();
        let mut user: HashMap<String, Detected> = HashMap::new();

        let mut files: Vec<PathBuf> = dir
            .and_then(|d| std::fs::read_dir(d).ok())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let name = name.to_ascii_lowercase();
            if LanguageId::from_name(&name).is_some() {
                warnings.push(format!(
                    "languages/{name}.toml: {name} is built in; to change its colours, \
                     use queries/{name}.scm"
                ));
                continue;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    warnings.push(format!("cannot read {}: {e}", path.display()));
                    continue;
                }
            };
            match read_language(&name, &text) {
                Ok((file, rules)) => {
                    let detected =
                        Detected::Simple(Box::leak(name.clone().into_boxed_str()), rules);
                    for ext in file.extensions {
                        let ext = ext.trim_start_matches('.').to_ascii_lowercase();
                        this.user_extensions.insert(ext, detected);
                    }
                    for file_name in file.filenames {
                        this.user_names.insert(file_name, detected);
                    }
                    user.insert(name, detected);
                }
                Err(e) => warnings.push(format!("languages/{name}.toml: {e}")),
            }
        }

        for (pattern, language) in table {
            let detected = LanguageId::from_name(language)
                .map(Detected::Builtin)
                .or_else(|| user.get(&language.to_ascii_lowercase()).copied());
            let Some(detected) = detected else {
                warnings.push(format!(
                    "config: languages: {pattern:?}: no language called {language:?}"
                ));
                continue;
            };
            if let Some(suffix) = pattern.strip_prefix('*') {
                if suffix.is_empty() || suffix.contains(['*', '/']) {
                    warnings.push(format!(
                        "config: languages: {pattern:?}: use a file name or *.ext"
                    ));
                    continue;
                }
                this.suffixes.push((suffix.to_ascii_lowercase(), detected));
            } else {
                this.names.insert(pattern.clone(), detected);
            }
        }
        // Longest pattern first, so `*.d.ts` beats `*.ts`.
        this.suffixes
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        (this, warnings)
    }

    /// Load from the config and the config directory.
    #[must_use]
    pub fn load(table: &HashMap<String, String>) -> (Self, Vec<String>) {
        Self::new(table, dir().as_deref())
    }

    /// The language of a file at `path` (if it has one) whose first line is
    /// `first_line`.
    #[must_use]
    pub fn detect(&self, path: Option<&Path>, first_line: &str) -> Option<Detected> {
        let file_name = path.and_then(|p| p.file_name()).and_then(|n| n.to_str());
        if let Some(name) = file_name {
            if let Some(d) = self.names.get(name) {
                return Some(*d);
            }
            let lower = name.to_ascii_lowercase();
            if let Some((_, d)) = self
                .suffixes
                .iter()
                .find(|(s, _)| lower.ends_with(s.as_str()))
            {
                return Some(*d);
            }
            if let Some(d) = self.user_names.get(name) {
                return Some(*d);
            }
            let ext = path
                .and_then(|p| p.extension())
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            if let Some(d) = ext.and_then(|e| self.user_extensions.get(&e)) {
                return Some(*d);
            }
        }
        LanguageId::detect(path, first_line).map(Detected::Builtin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nimbus-langs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn key(langs: &Languages, path: &str) -> Option<&'static str> {
        langs.detect(Some(Path::new(path)), "").map(Detected::key)
    }

    #[test]
    fn config_patterns_win_then_user_languages_then_built_ins() {
        let dir = temp_dir("order");
        std::fs::write(dir.join("lua.toml"), EXAMPLE).unwrap();
        let table: HashMap<String, String> = [
            ("Justfile", "bash"),
            ("*.rs.in", "Rust"),
            ("*.in", "lua"),
            ("*.md", "lua"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect();
        let (langs, warnings) = Languages::new(&table, Some(&dir));
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(key(&langs, "Justfile"), Some("bash"));
        assert_eq!(key(&langs, "x.rs.in"), Some("rust"), "longest pattern");
        assert_eq!(key(&langs, "x.in"), Some("lua"));
        assert_eq!(
            key(&langs, "README.MD"),
            Some("lua"),
            "config beats built-in"
        );
        assert_eq!(key(&langs, "init.LUA"), Some("lua"), "user extension");
        assert_eq!(key(&langs, "main.rs"), Some("rust"), "built-in");
        assert_eq!(key(&langs, "notes"), None);
        assert_eq!(
            langs
                .detect(Some(Path::new("run")), "#!/bin/bash")
                .map(Detected::key),
            Some("bash"),
            "shebang"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mistakes_are_warnings() {
        let dir = temp_dir("mistakes");
        std::fs::write(dir.join("broken.toml"), "keywords = [1]\n").unwrap();
        std::fs::write(dir.join("typo.toml"), "keyword = [\"a\"]\n").unwrap();
        std::fs::write(dir.join("empty.toml"), "line_comments = [\"\"]\n").unwrap();
        std::fs::write(dir.join("rust.toml"), "").unwrap();
        std::fs::write(dir.join("fine.toml"), "extensions = [\"fine\"]\n").unwrap();
        let table: HashMap<String, String> = [("*.x", "nope"), ("*", "bash"), ("a/*.y", "bash")]
            .into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect();
        let (langs, warnings) = Languages::new(&table, Some(&dir));
        // Four bad files, and an unknown language and a bare "*" in the table;
        // "a/*.y" is an exact name that never matches, which is harmless.
        assert_eq!(warnings.len(), 6, "{warnings:#?}");
        assert_eq!(key(&langs, "a.fine"), Some("fine"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
