//! Where to start a query in a wide tree.
//!
//! A query cursor restricted to a byte range still steps through every
//! child of each node it starts from, to find the ones in range. That is
//! nothing for code, whose trees are deep and narrow, but Markdown's tree
//! is wide and flat: a `document` or `section` with a child per
//! paragraph, thousands of them in a large file, and stepping past them
//! cost about 2 ms per query on a 1 MB file, the whole frame budget.
//!
//! So a query over a range descends first: from the root, down through
//! each node whose one child covers the range, and across a wide node to
//! only the children that touch the range, found by a cursor walk rather
//! than a query. Each node passed through is queried *self-only*: patterns
//! may start at it (a `(section)` fold region still matches) but the
//! cursor does not look for pattern starts among its children. The
//! nodes reached at the bottom are queried whole.

use std::ops::Range;

use tree_sitter::{Node, QueryCursor};

/// A node with more children than this is crossed, not queried whole.
pub const WIDE: u32 = 64;

/// How far the descent goes before the node is queried whole.
const MAX_DEPTH: u8 = 12;

/// A node to run a query from, and whether patterns may start only at
/// that node (`true` for a node passed through on the way down).
pub type Root<'t> = (Node<'t>, bool);

/// Call `f` with each node to run a query from to cover `range` under
/// `node`, in file order.
pub fn for_each_root<'t>(node: Node<'t>, range: &Range<usize>, f: &mut dyn FnMut(Root<'t>)) {
    roots_at(node, range, f, 0);
}

fn roots_at<'t>(node: Node<'t>, range: &Range<usize>, f: &mut dyn FnMut(Root<'t>), depth: u8) {
    if depth >= MAX_DEPTH || node.child_count() == 0 {
        f((node, false));
        return;
    }
    let mut cursor = node.walk();
    if cursor.goto_first_child_for_byte(range.start).is_none() {
        // The range is past every child: only the node itself can match.
        f((node, true));
        return;
    }
    let first = cursor.node();
    if first.end_byte() >= range.end {
        // One child covers the range: pass through to it.
        f((node, true));
        roots_at(first, range, f, depth + 1);
        return;
    }
    if node.child_count() <= WIDE {
        f((node, false));
        return;
    }
    // Wide: cross to the children in range.
    f((node, true));
    loop {
        let child = cursor.node();
        if child.start_byte() >= range.end {
            break;
        }
        if child.end_byte() > range.start {
            roots_at(child, range, f, depth + 1);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Set `cursor` up for one of the roots: the byte range, and whether
/// patterns may start below the root.
pub fn prepare(cursor: &mut QueryCursor, root: &Root<'_>, range: &Range<usize>) {
    cursor.set_byte_range(range.clone());
    cursor.set_max_start_depth(root.1.then_some(0));
}

#[cfg(all(test, feature = "markdown"))]
mod tests {
    use super::*;
    use crate::LanguageId;
    use crate::state::SyntaxState;
    use nimbus_core::RopeBuffer;

    #[test]
    fn a_wide_markdown_document_splits_into_its_paragraphs() {
        let src = "para\n\n".repeat(200);
        let buf = RopeBuffer::from_text(&src);
        let mut state = SyntaxState::new(Some(LanguageId::Markdown), src.len());
        state.parse_now(&buf);
        let (tree, _) = state.tree().expect("tree");
        let root = tree.root_node();
        let section = root.child(0).expect("section");
        assert!(section.child_count() > WIDE, "{}", section.to_sexp());
        let mut out = Vec::new();
        for_each_root(root, &(600..660), &mut |r| out.push(r));
        assert_eq!(out[0], (root, true), "passed through");
        assert_eq!(out[1], (section, true), "crossed");
        let paragraphs: Vec<_> = out[2..].iter().map(|(n, _)| n.start_byte()).collect();
        assert_eq!(
            paragraphs,
            [600, 606, 612, 618, 624, 630, 636, 642, 648, 654]
        );
        assert!(out[2..].iter().all(|(_, only)| !only));
        // A range inside one paragraph descends to it.
        let mut one = Vec::new();
        for_each_root(root, &(601..603), &mut |r| one.push(r));
        let kinds: Vec<_> = one.iter().map(|(n, only)| (n.kind(), *only)).collect();
        assert_eq!(
            kinds,
            [
                ("document", true),
                ("section", true),
                ("paragraph", true),
                ("inline", false)
            ]
        );
    }

    #[cfg(feature = "rust")]
    #[test]
    fn a_code_tree_descends_to_the_function_around_the_range() {
        let src = "fn a() {}\nfn b() {\n    let x = 1;\n}\n";
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(LanguageId::Rust), src.len());
        state.parse_now(&buf);
        let (tree, _) = state.tree().expect("tree");
        let mut out = Vec::new();
        for_each_root(tree.root_node(), &(20..30), &mut |r| out.push(r));
        let kinds: Vec<_> = out.iter().map(|(n, only)| (n.kind(), *only)).collect();
        assert_eq!(
            kinds,
            [
                ("source_file", true),
                ("function_item", true),
                ("block", true),
                ("let_declaration", false)
            ]
        );
        // The whole file: the root, whole.
        let mut all = Vec::new();
        for_each_root(tree.root_node(), &(0..src.len()), &mut |r| all.push(r));
        assert_eq!(all.len(), 1);
    }
}
