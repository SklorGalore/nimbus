//! Nimbus editor core: text buffers, edits, undo, cursors, search and diff.
//!
//! This crate has no I/O and no GPU dependencies so that everything in it can
//! be unit-tested without a window. All positions are byte offsets into UTF-8
//! (DESIGN.md §4.1).

pub mod align;
pub mod any;
pub mod brackets;
pub mod buffer;
pub mod column;
pub mod comment;
pub mod compare;
pub mod diff;
pub mod expand;
pub mod indent;
pub mod line_ending;
pub mod line_index;
pub mod lines;
pub mod live_diff;
pub mod long_line;
pub mod mapped;
pub mod markers;
pub mod merge;
pub mod movement;
pub mod multi;
pub mod regions;
pub mod rope;
pub mod search;
pub mod selection;
pub mod source;
pub mod transform;
pub mod undo;
pub mod wrap;

pub use align::{Cell, Change, Folding, Row, Rows};
pub use any::AnyBuffer;
pub use buffer::{
    Buffer, Content, Edit, EditError, LineSpan, SUBSTITUTE, Snapshot, Text, report_sorted,
};
pub use compare::{Comparison, compare};
pub use diff::{DIFF_LIMIT, DiffError, Hunk, Interner, Lines};
pub use line_ending::LineEnding;
pub use live_diff::{EditSpan, LiveDiff, Region, Side};
pub use mapped::{Limits, MappedBuffer, MappedSnapshot, PieceRun};
pub use merge::{Chunk, Kind, Merge, Pick};
pub use regions::{RegionTag, Regions, Resolution, diff_lines};
pub use rope::RopeBuffer;
pub use selection::{Bias, Selection, Selections};
pub use source::{Source, VecSource};
pub use undo::History;

/// Version of the `nimbus-core` crate. This is not the app's version: crates
/// are versioned separately, and `nimbus --version` prints the app's own.
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
