//! Which language a file is written in.
//!
//! Detection looks at the file name first (extension, or a few well-known
//! names such as `Cargo.lock`), then at a `#!` line for scripts without an
//! extension. Unknown files are plain text: no highlighting.

use std::path::Path;

/// A language Nimbus can highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguageId {
    /// Rust.
    Rust,
    /// Markdown (block structure: headings, lists, code fences).
    Markdown,
    /// JSON.
    Json,
    /// TOML.
    Toml,
    /// YAML.
    Yaml,
    /// Python.
    Python,
    /// JavaScript, including JSX.
    JavaScript,
    /// TypeScript. (`.tsx` is not highlighted: the TSX grammar alone adds
    /// about 1.4 MB, which the binary budget cannot spare; DESIGN.md §10.)
    TypeScript,
    /// Bash and other POSIX shell scripts.
    Bash,
}

impl LanguageId {
    /// Every language, for tests and listings.
    pub const ALL: [Self; 9] = [
        Self::Rust,
        Self::Markdown,
        Self::Json,
        Self::Toml,
        Self::Yaml,
        Self::Python,
        Self::JavaScript,
        Self::TypeScript,
        Self::Bash,
    ];

    /// Position in [`LanguageId::ALL`], for per-language tables.
    #[must_use]
    pub fn index(self) -> usize {
        self as usize
    }

    /// A short name for the status bar and logs.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Markdown => "Markdown",
            Self::Json => "JSON",
            Self::Toml => "TOML",
            Self::Yaml => "YAML",
            Self::Python => "Python",
            Self::JavaScript => "JavaScript",
            Self::TypeScript => "TypeScript",
            Self::Bash => "Shell",
        }
    }

    /// Detect from a file path and, failing that, the file's first line.
    #[must_use]
    pub fn detect(path: Option<&Path>, first_line: &str) -> Option<Self> {
        path.and_then(Self::from_path)
            .or_else(|| Self::from_shebang(first_line))
    }

    /// Detect from the file name alone.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        let by_name = match name {
            "Cargo.lock" | "Pipfile" | "poetry.lock" | "uv.lock" => Some(Self::Toml),
            ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" | ".zprofile" | ".envrc" => {
                Some(Self::Bash)
            }
            _ => None,
        };
        if by_name.is_some() {
            return by_name;
        }
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "rs" => Self::Rust,
            "md" | "markdown" | "mdown" | "mkd" => Self::Markdown,
            "json" | "jsonc" | "json5" | "geojson" => Self::Json,
            "toml" => Self::Toml,
            "yaml" | "yml" => Self::Yaml,
            "py" | "pyi" | "pyw" => Self::Python,
            "js" | "mjs" | "cjs" | "jsx" => Self::JavaScript,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "sh" | "bash" | "zsh" | "ksh" => Self::Bash,
            _ => return None,
        })
    }

    /// Detect from a `#!` line such as `#!/usr/bin/env python3`.
    #[must_use]
    pub fn from_shebang(first_line: &str) -> Option<Self> {
        let rest = first_line.strip_prefix("#!")?.trim();
        let mut words = rest.split_whitespace();
        let mut program = words.next()?.rsplit('/').next()?;
        if program == "env" {
            // Skip env's options, such as `-S`.
            program = words.find(|w| !w.starts_with('-'))?;
        }
        let program = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        Some(match program {
            "python" | "pypy" => Self::Python,
            "node" | "nodejs" | "bun" => Self::JavaScript,
            "deno" | "ts-node" | "tsx" => Self::TypeScript,
            "sh" | "bash" | "zsh" | "dash" | "ksh" => Self::Bash,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> Option<LanguageId> {
        LanguageId::from_path(Path::new(p))
    }

    #[test]
    fn extensions_and_names() {
        assert_eq!(path("src/main.rs"), Some(LanguageId::Rust));
        assert_eq!(path("README.MD"), Some(LanguageId::Markdown));
        assert_eq!(path("Cargo.lock"), Some(LanguageId::Toml));
        assert_eq!(path("app.tsx"), None, "TSX grammar left out for size");
        assert_eq!(path("x.mjs"), Some(LanguageId::JavaScript));
        assert_eq!(path("/home/u/.zshrc"), Some(LanguageId::Bash));
        assert_eq!(path("notes.txt"), None);
        assert_eq!(path("Makefile"), None);
    }

    #[test]
    fn shebangs() {
        assert_eq!(
            LanguageId::from_shebang("#!/usr/bin/env python3"),
            Some(LanguageId::Python)
        );
        assert_eq!(
            LanguageId::from_shebang("#!/usr/bin/python3.12 -u"),
            Some(LanguageId::Python)
        );
        assert_eq!(
            LanguageId::from_shebang("#!/bin/bash"),
            Some(LanguageId::Bash)
        );
        assert_eq!(
            LanguageId::from_shebang("#!/usr/bin/env -S deno run"),
            Some(LanguageId::TypeScript)
        );
        assert_eq!(LanguageId::from_shebang("#!/usr/bin/env perl"), None);
        assert_eq!(LanguageId::from_shebang("// not a shebang"), None);
    }

    #[test]
    fn path_wins_over_shebang() {
        let d = LanguageId::detect(Some(Path::new("tool.py")), "#!/bin/bash");
        assert_eq!(d, Some(LanguageId::Python));
        let d = LanguageId::detect(Some(Path::new("tool")), "#!/bin/bash");
        assert_eq!(d, Some(LanguageId::Bash));
    }
}
