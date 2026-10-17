//! Syntax highlighting with tree-sitter (DESIGN.md §4.2).

pub mod edit;
pub mod highlight;
pub mod inject;
pub mod language;
pub mod lexical;
pub mod registry;
pub mod split;
pub mod state;
pub mod structure;
pub mod symbols;
pub mod text;

pub use language::LanguageId;
