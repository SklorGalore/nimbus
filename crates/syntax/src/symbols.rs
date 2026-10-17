//! The definitions in a file, for Go to Symbol (M14).
//!
//! A *tags* query is the convention tree-sitter grammars use to mark
//! definitions: each pattern captures the whole definition as
//! `@definition.function`, `@definition.class` and so on, and its name as
//! `@name`. Most grammar crates ship one for code navigation tools; this
//! module runs it over the whole tree and keeps the definitions in file
//! order. A tree knows structure, not meaning: this lists what the file
//! declares, with no idea whether a name is used, which is all a per-file
//! symbol picker needs (DESIGN.md §2 rules out a language server).

use nimbus_core::Text;
use streaming_iterator::StreamingIterator;
use tree_sitter::QueryCursor;

use crate::registry;
use crate::state::SyntaxState;
use crate::text::NodeChunks;

/// More definitions than a picker can usefully list; a generated file is
/// cut off here rather than held in memory.
pub const MAX_SYMBOLS: usize = 20_000;

/// What kind of thing a symbol is, from the capture that marked it
/// (`@definition.function` gives [`SymbolKind::Function`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    /// A free function.
    Function,
    /// A method: a function inside a type's body.
    Method,
    /// A struct, enum, class or union.
    Class,
    /// A type alias or other named type.
    Type,
    /// A trait or interface.
    Interface,
    /// A module or namespace.
    Module,
    /// A macro definition.
    Macro,
    /// A constant or static.
    Constant,
    /// A Markdown heading.
    Heading,
    /// A definition the query names some other way.
    Other,
}

impl SymbolKind {
    /// From the part of the capture name after `definition.`.
    #[must_use]
    pub fn from_capture(suffix: &str) -> Self {
        match suffix {
            "function" => Self::Function,
            "method" => Self::Method,
            "class" => Self::Class,
            "type" => Self::Type,
            "interface" => Self::Interface,
            "module" => Self::Module,
            "macro" => Self::Macro,
            "constant" => Self::Constant,
            "heading" => Self::Heading,
            _ => Self::Other,
        }
    }

    /// A word for the picker's detail column.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "type",
            Self::Type => "type alias",
            Self::Interface => "interface",
            Self::Module => "module",
            Self::Macro => "macro",
            Self::Constant => "constant",
            Self::Heading => "heading",
            Self::Other => "definition",
        }
    }
}

/// One definition in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The name as written (a heading's text for Markdown).
    pub name: String,
    /// What it is.
    pub kind: SymbolKind,
    /// Byte offset of the name, where the cursor goes.
    pub start: usize,
}

/// The definitions in the document, in file order, from its current
/// tree. Empty when there is no tree yet, or the language has no tags
/// query (plain data formats such as JSON have no definitions). Costs a
/// walk of the whole tree, so the caller caches the result by
/// [`SyntaxState::rev`].
#[must_use]
pub fn symbols(state: &SyntaxState, text: &dyn Text) -> Vec<Symbol> {
    let mut out = Vec::new();
    let Some((tree, _)) = state.tree() else {
        return out;
    };
    let Some(query) = state.language().and_then(registry::symbols_query) else {
        return out;
    };
    let Some(name_capture) = query.capture_index_for_name("name") else {
        return out;
    };
    // The kind of each pattern's definition capture, worked out once.
    let kinds: Vec<Option<(u32, SymbolKind)>> = (0..query.pattern_count()).map(|_| None).collect();
    let mut kinds = kinds;
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), |node: tree_sitter::Node<'_>| {
        NodeChunks::new(text, node)
    });
    while let Some(m) = matches.next() {
        let Some(slot) = kinds.get_mut(m.pattern_index) else {
            continue;
        };
        if slot.is_none() {
            // The first capture of this pattern named `definition.*`.
            *slot = m.captures().iter().find_map(|c| {
                let name = names.get(c.index as usize)?;
                let suffix = name.strip_prefix("definition.")?;
                Some((c.index, SymbolKind::from_capture(suffix)))
            });
        }
        let Some((def_capture, kind)) = *slot else {
            // A reference pattern, or one with a bare `@name`.
            continue;
        };
        let (mut name_node, mut found_def) = (None, false);
        for c in m.captures() {
            if c.index == name_capture && name_node.is_none() {
                name_node = Some(c.node);
            }
            found_def |= c.index == def_capture;
        }
        let Some(node) = name_node.filter(|_| found_def) else {
            continue;
        };
        let start = node.start_byte();
        // Rust's tags mark a method twice (as a method inside a body and
        // as a function); the first pattern wins.
        if out.last().is_some_and(|s: &Symbol| s.start == start) {
            continue;
        }
        let name: String = NodeChunks::new(text, node)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        out.push(Symbol {
            name: name.to_owned(),
            kind,
            start,
        });
        if out.len() >= MAX_SYMBOLS {
            break;
        }
    }
    // Matches come in tree order, which is mostly file order; nested
    // patterns can report a parent after its children.
    out.sort_by_key(|s| s.start);
    out.dedup_by_key(|s| s.start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LanguageId;
    use nimbus_core::RopeBuffer;

    fn list(src: &str, language: LanguageId) -> Vec<(String, SymbolKind)> {
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(language), src.len());
        state.parse_now(&buf);
        symbols(&state, &buf)
            .into_iter()
            .map(|s| (s.name, s.kind))
            .collect()
    }

    #[cfg(feature = "rust")]
    #[test]
    fn rust_definitions_in_file_order() {
        let src = "const N: u8 = 1;\nstruct S;\nimpl S {\n    fn m(&self) {}\n}\nfn f() { g(); }\nmod m {\n    fn inner() {}\n}\n";
        let got = list(src, LanguageId::Rust);
        let expect = [
            ("N", SymbolKind::Constant),
            ("S", SymbolKind::Class),
            ("m", SymbolKind::Method),
            ("f", SymbolKind::Function),
            ("m", SymbolKind::Module),
            ("inner", SymbolKind::Function),
        ];
        let expect: Vec<_> = expect.into_iter().map(|(n, k)| (n.to_owned(), k)).collect();
        assert_eq!(
            got, expect,
            "no duplicate for the method, no call references"
        );
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(LanguageId::Rust), src.len());
        assert!(symbols(&state, &buf).is_empty(), "no tree yet");
        state.parse_now(&buf);
        assert_eq!(symbols(&state, &buf)[1].start, src.find("S;").unwrap_or(0));
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn markdown_headings_are_symbols() {
        let got = list(
            "# Title\n\ntext\n\nSub\n---\n\n## Two\n",
            LanguageId::Markdown,
        );
        assert_eq!(
            got,
            [
                ("Title".to_owned(), SymbolKind::Heading),
                ("Sub".to_owned(), SymbolKind::Heading),
                ("Two".to_owned(), SymbolKind::Heading),
            ]
        );
    }

    #[cfg(feature = "json")]
    #[test]
    fn languages_without_a_tags_query_have_no_symbols() {
        assert!(list("{\"a\": 1}", LanguageId::Json).is_empty());
    }

    #[cfg(all(feature = "go", feature = "c", feature = "python", feature = "tsx"))]
    #[test]
    fn the_other_grammars_find_functions_and_types() {
        let go = list(
            "package p\nconst K = 1\ntype T struct{}\nfunc (t T) M() {}\nfunc F() {}\n",
            LanguageId::Go,
        );
        assert!(
            go.contains(&("F".to_owned(), SymbolKind::Function)),
            "{go:?}"
        );
        assert!(go.contains(&("M".to_owned(), SymbolKind::Method)), "{go:?}");
        assert!(go.contains(&("T".to_owned(), SymbolKind::Type)), "{go:?}");
        assert!(
            go.contains(&("K".to_owned(), SymbolKind::Constant)),
            "{go:?}"
        );
        let c = list(
            "struct P { int x; };\nint main(void) { return 0; }\n",
            LanguageId::C,
        );
        assert!(
            c.contains(&("main".to_owned(), SymbolKind::Function)),
            "{c:?}"
        );
        assert!(c.contains(&("P".to_owned(), SymbolKind::Class)), "{c:?}");
        let py = list(
            "class A:\n    def m(self): pass\ndef f(): pass\n",
            LanguageId::Python,
        );
        assert_eq!(py.len(), 3, "{py:?}");
        let tsx = list(
            "interface I {}\nfunction f(): I { return <div/>; }\n",
            LanguageId::Tsx,
        );
        assert!(
            tsx.contains(&("I".to_owned(), SymbolKind::Interface)),
            "{tsx:?}"
        );
        assert!(
            tsx.contains(&("f".to_owned(), SymbolKind::Function)),
            "{tsx:?}"
        );
    }
}
