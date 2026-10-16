//! Nimbus editor core: text buffers, edits, undo, cursors and search.
//!
//! This crate has no I/O and no GPU dependencies so that everything in it can
//! be unit-tested without a window. All positions are byte offsets into UTF-8
//! (DESIGN.md §4.1).

pub mod any;
pub mod buffer;
pub mod column;
pub mod line_ending;
pub mod line_index;
pub mod long_line;
pub mod mapped;
pub mod movement;
pub mod rope;
pub mod search;
pub mod selection;
pub mod source;
pub mod undo;
pub mod wrap;

pub use any::AnyBuffer;
pub use buffer::{
    Buffer, Content, Edit, EditError, LineSpan, SUBSTITUTE, Snapshot, Text, report_sorted,
};
pub use line_ending::LineEnding;
pub use mapped::{Limits, MappedBuffer, MappedSnapshot, PieceRun};
pub use rope::RopeBuffer;
pub use selection::{Bias, Selection, Selections};
pub use source::{Source, VecSource};
pub use undo::History;

/// Crate version, surfaced in the window title and the debug overlay.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_semver_triple() {
        let parts: Vec<&str> = super::VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "version {:?}", super::VERSION);
        assert!(parts.iter().all(|p| p.parse::<u32>().is_ok()));
    }
}
