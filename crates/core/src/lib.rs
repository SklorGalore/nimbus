//! Nimbus editor core: text buffers, edits, undo, cursors and search.
//!
//! This crate has no I/O and no GPU dependencies so that everything in it can
//! be unit-tested without a window. Milestone 2 adds the `Buffer` trait and
//! the rope-backed implementation described in DESIGN.md §4.1.

/// Crate version, surfaced in the window title and the debug overlay.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert!(!super::VERSION.is_empty());
    }
}
