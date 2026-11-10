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
    /// TypeScript.
    TypeScript,
    /// Bash and other POSIX shell scripts.
    Bash,
    /// C.
    C,
    /// Go.
    Go,
    /// HTML.
    Html,
    /// CSS.
    Css,
    /// TypeScript with JSX (`.tsx`): a second parser in the TypeScript
    /// grammar, added in M14 once the binary budget had room for it.
    Tsx,
    /// PowerShell (`.ps1` scripts, `.psm1` modules, `.psd1` data files).
    PowerShell,
    /// Git's ignore files: `.gitignore`, `.git/info/exclude`. This and the
    /// other git formats below have no tree-sitter grammar; they are
    /// coloured line by line ([`LanguageId::simple_only`]).
    GitIgnore,
    /// Git's `.gitattributes`.
    GitAttributes,
    /// Git's config files: `.gitconfig`, `.gitmodules`, `.git/config`.
    GitConfig,
    /// A commit message being written: `COMMIT_EDITMSG` and the like.
    GitCommit,
    /// The to-do list of an interactive rebase (`git-rebase-todo`).
    GitRebase,
}

impl LanguageId {
    /// Every language, for tests and listings.
    pub const ALL: [Self; 20] = [
        Self::Rust,
        Self::Markdown,
        Self::Json,
        Self::Toml,
        Self::Yaml,
        Self::Python,
        Self::JavaScript,
        Self::TypeScript,
        Self::Bash,
        Self::C,
        Self::Go,
        Self::Html,
        Self::Css,
        Self::Tsx,
        Self::PowerShell,
        Self::GitIgnore,
        Self::GitAttributes,
        Self::GitConfig,
        Self::GitCommit,
        Self::GitRebase,
    ];

    /// Lower-case key used in file names and the config:
    /// `queries/rust.scm`, `"*.tpl" = "bash"`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Markdown => "markdown",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Yaml => "yaml",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Bash => "bash",
            Self::C => "c",
            Self::Go => "go",
            Self::Html => "html",
            Self::Css => "css",
            Self::Tsx => "tsx",
            Self::PowerShell => "powershell",
            Self::GitIgnore => "gitignore",
            Self::GitAttributes => "gitattributes",
            Self::GitConfig => "gitconfig",
            Self::GitCommit => "gitcommit",
            Self::GitRebase => "gitrebase",
        }
    }

    /// The language a fenced code block or an injection query names, by
    /// key, display name or a common alias (`py`, `sh`, `js`, `md`, ...).
    /// `None` for a language Nimbus has no grammar for.
    #[must_use]
    pub fn from_alias(name: &str) -> Option<Self> {
        let name = name.trim();
        if let Some(l) = Self::from_name(name) {
            return Some(l);
        }
        Some(match name.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "md" => Self::Markdown,
            "jsonc" | "json5" => Self::Json,
            "yml" => Self::Yaml,
            "py" | "python3" => Self::Python,
            "js" | "jsx" | "mjs" | "cjs" | "node" => Self::JavaScript,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "sh" | "zsh" | "shell" | "console" => Self::Bash,
            "h" => Self::C,
            "golang" => Self::Go,
            "htm" | "xhtml" => Self::Html,
            "ps1" | "pwsh" | "posh" => Self::PowerShell,
            "git-config" => Self::GitConfig,
            "git-commit" => Self::GitCommit,
            "git-rebase-todo" | "git-rebase" => Self::GitRebase,
            _ => return None,
        })
    }

    /// The language with this key or display name, ignoring case
    /// (`"rust"`, `"Shell"`, `"bash"`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|l| l.key().eq_ignore_ascii_case(name) || l.name().eq_ignore_ascii_case(name))
    }

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
            Self::C => "C",
            Self::Go => "Go",
            Self::Html => "HTML",
            Self::Css => "CSS",
            Self::Tsx => "TSX",
            Self::PowerShell => "PowerShell",
            Self::GitIgnore => "Git Ignore",
            Self::GitAttributes => "Git Attributes",
            Self::GitConfig => "Git Config",
            Self::GitCommit => "Git Commit",
            Self::GitRebase => "Git Rebase",
        }
    }

    /// Whether the language is coloured only by the line-by-line rules in
    /// [`crate::lexical`], at any size. Git's own files are: each line
    /// stands on its own (a pattern, a setting, a rebase step), so simple
    /// rules colour them fully and no grammar is worth its binary size.
    #[must_use]
    pub fn simple_only(self) -> bool {
        matches!(
            self,
            Self::GitIgnore
                | Self::GitAttributes
                | Self::GitConfig
                | Self::GitCommit
                | Self::GitRebase
        )
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
            ".gitignore" => Some(Self::GitIgnore),
            ".gitattributes" => Some(Self::GitAttributes),
            ".gitconfig" | ".gitmodules" | ".lfsconfig" => Some(Self::GitConfig),
            "COMMIT_EDITMSG" | "MERGE_MSG" | "SQUASH_MSG" | "TAG_EDITMSG" | "NOTES_EDITMSG" => {
                Some(Self::GitCommit)
            }
            "git-rebase-todo" => Some(Self::GitRebase),
            _ => Self::from_git_dir(path, name),
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
            "c" | "h" => Self::C,
            "go" => Self::Go,
            "html" | "htm" | "xhtml" => Self::Html,
            "css" => Self::Css,
            "tsx" => Self::Tsx,
            "ps1" | "psm1" | "psd1" => Self::PowerShell,
            // Collections of ignore patterns or included config files,
            // such as `Rust.gitignore` or `work.gitconfig`.
            "gitignore" => Self::GitIgnore,
            "gitconfig" => Self::GitConfig,
            _ => return None,
        })
    }

    /// Git files known by their folder rather than their name alone:
    /// `.git/config`, `.git/info/exclude` and `.git/info/attributes` in a
    /// repository, and `config`, `ignore` and `attributes` in the user's
    /// `~/.config/git` folder. A bare `config` elsewhere (`~/.ssh/config`)
    /// is not git's.
    fn from_git_dir(path: &Path, name: &str) -> Option<Self> {
        fn folder(p: Option<&Path>) -> Option<&str> {
            p.and_then(Path::file_name).and_then(|n| n.to_str())
        }
        let parent = path.parent();
        let grandparent = parent.and_then(Path::parent);
        let in_user_git = folder(parent) == Some("git") && folder(grandparent) == Some(".config");
        let in_info = folder(parent) == Some("info") && folder(grandparent) == Some(".git");
        match name {
            "config" if folder(parent) == Some(".git") || in_user_git => Some(Self::GitConfig),
            "exclude" if in_info => Some(Self::GitIgnore),
            "ignore" if in_user_git => Some(Self::GitIgnore),
            "attributes" if in_info || in_user_git => Some(Self::GitAttributes),
            _ => None,
        }
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
            "pwsh" | "powershell" => Self::PowerShell,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_keys_round_trip() {
        for l in LanguageId::ALL {
            assert_eq!(LanguageId::from_name(l.key()), Some(l));
            assert_eq!(LanguageId::from_name(&l.name().to_uppercase()), Some(l));
        }
        assert_eq!(LanguageId::from_name("Shell"), Some(LanguageId::Bash));
        assert_eq!(LanguageId::from_name("lua"), None);
    }

    #[test]
    fn aliases_name_fenced_block_languages() {
        assert_eq!(LanguageId::from_alias("py"), Some(LanguageId::Python));
        assert_eq!(LanguageId::from_alias(" sh "), Some(LanguageId::Bash));
        assert_eq!(LanguageId::from_alias("Rust"), Some(LanguageId::Rust));
        assert_eq!(LanguageId::from_alias("tsx"), Some(LanguageId::Tsx));
        assert_eq!(LanguageId::from_alias("pwsh"), Some(LanguageId::PowerShell));
        assert_eq!(
            LanguageId::from_alias("PowerShell"),
            Some(LanguageId::PowerShell)
        );
        assert_eq!(LanguageId::from_alias("latex"), None);
        assert_eq!(LanguageId::from_alias(""), None);
    }

    #[test]
    fn git_files() {
        use LanguageId::{GitAttributes, GitCommit, GitConfig, GitIgnore, GitRebase};
        assert_eq!(path("repo/.gitignore"), Some(GitIgnore));
        assert_eq!(path("templates/Rust.gitignore"), Some(GitIgnore));
        assert_eq!(path("repo/.git/info/exclude"), Some(GitIgnore));
        assert_eq!(path("/home/me/.config/git/ignore"), Some(GitIgnore));
        assert_eq!(path("repo/.gitattributes"), Some(GitAttributes));
        assert_eq!(path("repo/.git/info/attributes"), Some(GitAttributes));
        assert_eq!(path("/home/me/.gitconfig"), Some(GitConfig));
        assert_eq!(path("repo/.gitmodules"), Some(GitConfig));
        assert_eq!(path("repo/.git/config"), Some(GitConfig));
        assert_eq!(path("/home/me/.config/git/config"), Some(GitConfig));
        assert_eq!(path("work.gitconfig"), Some(GitConfig));
        assert_eq!(path("repo/.git/COMMIT_EDITMSG"), Some(GitCommit));
        assert_eq!(path("repo/.git/MERGE_MSG"), Some(GitCommit));
        assert_eq!(
            path("repo/.git/rebase-merge/git-rebase-todo"),
            Some(GitRebase)
        );
        // Names git uses only inside its own folders.
        assert_eq!(path("/home/me/.ssh/config"), None);
        assert_eq!(path("docs/info/exclude"), None);
        assert_eq!(path("ignore"), None);
        for l in [GitIgnore, GitAttributes, GitConfig, GitCommit, GitRebase] {
            assert!(l.simple_only());
        }
        assert!(!LanguageId::Rust.simple_only());
        assert_eq!(LanguageId::from_name("Git Config"), Some(GitConfig));
        assert_eq!(LanguageId::from_alias("git-commit"), Some(GitCommit));
    }

    fn path(p: &str) -> Option<LanguageId> {
        LanguageId::from_path(Path::new(p))
    }

    #[test]
    fn extensions_and_names() {
        assert_eq!(path("src/main.rs"), Some(LanguageId::Rust));
        assert_eq!(path("README.MD"), Some(LanguageId::Markdown));
        assert_eq!(path("Cargo.lock"), Some(LanguageId::Toml));
        assert_eq!(path("app.tsx"), Some(LanguageId::Tsx));
        assert_eq!(path("lib.h"), Some(LanguageId::C));
        assert_eq!(path("main.go"), Some(LanguageId::Go));
        assert_eq!(path("index.HTM"), Some(LanguageId::Html));
        assert_eq!(path("site.css"), Some(LanguageId::Css));
        assert_eq!(path("x.mjs"), Some(LanguageId::JavaScript));
        assert_eq!(path("/home/u/.zshrc"), Some(LanguageId::Bash));
        assert_eq!(
            path("scripts/bundle-macos.ps1"),
            Some(LanguageId::PowerShell)
        );
        assert_eq!(path("Module.PSM1"), Some(LanguageId::PowerShell));
        assert_eq!(path("Module.psd1"), Some(LanguageId::PowerShell));
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
        assert_eq!(
            LanguageId::from_shebang("#!/usr/bin/env pwsh"),
            Some(LanguageId::PowerShell)
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
