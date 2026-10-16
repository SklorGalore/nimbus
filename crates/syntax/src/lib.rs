//! Syntax highlighting with tree-sitter (DESIGN.md §4.2).

pub mod edit;
pub mod highlight;
pub mod language;
pub mod registry;
pub mod state;
pub mod text;

pub use language::LanguageId;
