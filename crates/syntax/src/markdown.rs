//! The blocks of a Markdown document, read from its syntax tree (M20's
//! preview).
//!
//! Markdown is parsed in two layers: a block grammar for the shape of the
//! page (headings, paragraphs, lists, code, quotes, tables) and an inline
//! grammar for what is inside a paragraph. This module walks the block
//! tree only, into a flat list of [`Block`]s in reading order, each with
//! the byte ranges of its text. Lines of a paragraph inside a quote or a
//! list carry prefixes (`> `, indentation) that the tree marks as
//! continuations; they are left out of the ranges, so a block's ranges
//! joined are exactly the text to show.

use std::ops::Range;

use nimbus_core::Text;
use tree_sitter::{Node, Tree};

use crate::LanguageId;
use crate::state::SyntaxState;

/// What a block is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A heading of level 1 to 6.
    Heading(u8),
    /// A paragraph of inline text.
    Paragraph,
    /// A code block, with the language its fence names.
    Code(Option<String>),
    /// A horizontal rule.
    Rule,
    /// A table: rows of cells, the first the header.
    Table(Vec<Vec<Range<usize>>>),
    /// A block of HTML, shown as a placeholder.
    Html,
}

/// One block of the page, in reading order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// What it is.
    pub kind: Kind,
    /// The whole block in the source, for scrolling in step.
    pub range: Range<usize>,
    /// Its text, without quote and list prefixes: inline text for a
    /// heading or paragraph, lines for code. Empty for a rule or table.
    pub text: Vec<Range<usize>>,
    /// List levels it is inside.
    pub list_depth: u8,
    /// Quote levels it is inside.
    pub quote_depth: u8,
    /// The list item marker it starts, when it is the first block of an
    /// item: `•`, `1.`, `☐` or `☑`.
    pub bullet: Option<String>,
}

/// A Markdown document's syntax tree, copied to be read on another
/// thread while the document goes on being edited. Copying is cheap: the
/// copy shares the tree's nodes, which are never changed in place.
#[derive(Debug, Clone)]
pub struct Outline(Tree);

/// A copy of the tree of a Markdown document, when `state` has one that
/// describes the text as it is now.
#[must_use]
pub fn outline(state: &SyntaxState) -> Option<Outline> {
    if state.language() != Some(LanguageId::Markdown) {
        return None;
    }
    state.current_tree().map(|t| Outline(t.clone()))
}

/// The blocks of the Markdown document `outline` describes, read with
/// `text`, the text it was parsed from.
#[must_use]
pub fn blocks(outline: &Outline, text: &dyn Text) -> Vec<Block> {
    let mut out = Vec::new();
    let tree = &outline.0;
    let mut walk = Walk {
        text,
        out: &mut out,
        list_depth: 0,
        quote_depth: 0,
        bullet: None,
    };
    walk.children(tree.root_node());
    out
}

/// State carried down the tree.
struct Walk<'a> {
    text: &'a dyn Text,
    out: &'a mut Vec<Block>,
    list_depth: u8,
    quote_depth: u8,
    /// A list item's marker, waiting for the item's first block.
    bullet: Option<String>,
}

impl Walk<'_> {
    /// Visit `node`'s named children in order. A tree cursor steps to a
    /// sibling in constant time, where asking a node for its next sibling
    /// searches from the parent: quadratic over a long document's
    /// sections.
    fn children(&mut self, node: Node<'_>) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            if child.is_named() {
                self.node(child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn node(&mut self, node: Node<'_>) {
        match node.kind() {
            "atx_heading" => {
                let level = heading_level(node);
                let content = node.child_by_field_name("heading_content");
                let text = content.map(|c| segments(c)).unwrap_or_default();
                self.push(Kind::Heading(level), node, text);
            }
            "setext_heading" => {
                let level = if has_child(node, "setext_h1_underline") {
                    1
                } else {
                    2
                };
                let content = node.child_by_field_name("heading_content");
                let text = content
                    .and_then(|p| child_of_kind(p, "inline").or(Some(p)))
                    .map(|c| segments(c))
                    .unwrap_or_default();
                self.push(Kind::Heading(level), node, text);
            }
            "paragraph" => {
                let text = child_of_kind(node, "inline")
                    .map(|c| segments(c))
                    .unwrap_or_default();
                self.push(Kind::Paragraph, node, text);
            }
            "fenced_code_block" => {
                let lang = child_of_kind(node, "info_string")
                    .and_then(|i| child_of_kind(i, "language"))
                    .map(|l| self.text.slice(l.start_byte()..l.end_byte()).into_owned());
                let text = child_of_kind(node, "code_fence_content")
                    .map(|c| segments(c))
                    .unwrap_or_default();
                self.push(Kind::Code(lang), node, text);
            }
            "indented_code_block" => {
                let text = segments(node);
                self.push(Kind::Code(None), node, text);
            }
            "thematic_break" => self.push(Kind::Rule, node, Vec::new()),
            "html_block" => self.push(Kind::Html, node, Vec::new()),
            "pipe_table" => {
                let rows = table_rows(node);
                self.push(Kind::Table(rows), node, Vec::new());
            }
            "block_quote" => {
                self.quote_depth = self.quote_depth.saturating_add(1);
                self.children(node);
                self.quote_depth -= 1;
            }
            "list" => {
                self.list_depth = self.list_depth.saturating_add(1);
                self.children(node);
                self.list_depth -= 1;
            }
            "list_item" => {
                self.bullet = Some(self.marker(node));
                self.children(node);
                // An item with no block of its own (`- ` alone) still
                // shows its marker.
                if let Some(b) = self.bullet.take() {
                    self.out.push(Block {
                        kind: Kind::Paragraph,
                        range: node.start_byte()..node.end_byte(),
                        text: Vec::new(),
                        list_depth: self.list_depth,
                        quote_depth: self.quote_depth,
                        bullet: Some(b),
                    });
                }
            }
            "section" | "document" => self.children(node),
            _ => {}
        }
    }

    /// A list item's marker as shown: a bullet, the item's number, or a
    /// task box.
    fn marker(&self, item: Node<'_>) -> String {
        if has_child(item, "task_list_marker_checked") {
            return "☑".to_owned();
        }
        if has_child(item, "task_list_marker_unchecked") {
            return "☐".to_owned();
        }
        let mut cursor = item.walk();
        for c in item.named_children(&mut cursor) {
            match c.kind() {
                "list_marker_dot" | "list_marker_parenthesis" => {
                    return self
                        .text
                        .slice(c.start_byte()..c.end_byte())
                        .trim()
                        .to_owned();
                }
                "list_marker_minus" | "list_marker_plus" | "list_marker_star" => {
                    return "•".to_owned();
                }
                _ => {}
            }
        }
        "•".to_owned()
    }

    fn push(&mut self, kind: Kind, node: Node<'_>, text: Vec<Range<usize>>) {
        self.out.push(Block {
            kind,
            range: node.start_byte()..node.end_byte(),
            text,
            list_depth: self.list_depth,
            quote_depth: self.quote_depth,
            bullet: self.bullet.take(),
        });
    }
}

/// A node's range with its continuation children (quote markers and
/// indentation on later lines) cut out.
fn segments(node: Node<'_>) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = node.start_byte();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "block_continuation" {
            if c.start_byte() > at {
                out.push(at..c.start_byte());
            }
            at = at.max(c.end_byte());
        }
    }
    if node.end_byte() > at {
        out.push(at..node.end_byte());
    }
    out
}

/// The cells of a table, row by row, the header first; the delimiter
/// row is left out.
fn table_rows(table: Node<'_>) -> Vec<Vec<Range<usize>>> {
    let mut rows = Vec::new();
    let mut cursor = table.walk();
    for row in table.named_children(&mut cursor) {
        if !matches!(row.kind(), "pipe_table_header" | "pipe_table_row") {
            continue;
        }
        let mut c = row.walk();
        let cells = row
            .named_children(&mut c)
            .filter(|n| n.kind() == "pipe_table_cell")
            .map(|n| n.start_byte()..n.end_byte())
            .collect();
        rows.push(cells);
    }
    rows
}

fn heading_level(node: Node<'_>) -> u8 {
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if let Some(n) = c
            .kind()
            .strip_prefix("atx_h")
            .and_then(|k| k.strip_suffix("_marker"))
            .and_then(|d| d.parse::<u8>().ok())
        {
            return n;
        }
    }
    1
}

fn has_child(node: Node<'_>, kind: &str) -> bool {
    child_of_kind(node, kind).is_some()
}

fn child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).find(|c| c.kind() == kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_core::RopeBuffer;

    fn parse(src: &str) -> (RopeBuffer, Vec<Block>) {
        let buf = RopeBuffer::from_text(src);
        let mut state = SyntaxState::new(Some(LanguageId::Markdown), src.len());
        state.parse_now(&buf);
        let Some(o) = outline(&state) else {
            panic!("no tree");
        };
        let b = blocks(&o, &buf);
        (buf, b)
    }

    fn joined(buf: &RopeBuffer, b: &Block) -> String {
        b.text.iter().map(|r| buf.slice(r.clone())).collect()
    }

    #[test]
    fn headings_paragraphs_code_and_rules() {
        let src = "# Title\n\nSome *text*\nover two lines.\n\n---\n\n```rust\nfn main() {}\n```\n\nUnder\n=====\n";
        let (buf, b) = parse(src);
        let kinds: Vec<&Kind> = b.iter().map(|b| &b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                &Kind::Heading(1),
                &Kind::Paragraph,
                &Kind::Rule,
                &Kind::Code(Some("rust".to_owned())),
                &Kind::Heading(1),
            ]
        );
        assert_eq!(joined(&buf, &b[0]).trim(), "Title");
        assert_eq!(joined(&buf, &b[1]), "Some *text*\nover two lines.");
        assert_eq!(joined(&buf, &b[3]), "fn main() {}\n");
        assert_eq!(joined(&buf, &b[4]).trim(), "Under");
    }

    /// Each `>` opens a block the Markdown scanner keeps in its saved
    /// state, and tree-sitter gives that state 1024 bytes. tree-sitter-md
    /// 0.5.3 wrote past them from 255 nested quotes on, corrupting the
    /// parser's memory (found by the `highlight` fuzz target). 0.5.4 stops
    /// opening blocks at the limit and reads further markers as text.
    #[test]
    fn deeply_nested_quotes_stay_within_the_scanner_state() {
        for depth in [254, 255, 400, 2000] {
            let src = format!("{} deep\n", ">".repeat(depth));
            let (_, b) = parse(&src);
            assert!(!b.is_empty(), "depth {depth}");
        }
    }

    #[test]
    fn lists_and_quotes_lose_their_prefixes() {
        let src = "- one\n- two\n  1. inner\n- [x] done\n\n> quoted\n> lines\n";
        let (buf, b) = parse(src);
        let shown: Vec<(Option<&str>, u8, u8, String)> = b
            .iter()
            .map(|x| {
                (
                    x.bullet.as_deref(),
                    x.list_depth,
                    x.quote_depth,
                    joined(&buf, x),
                )
            })
            .collect();
        assert_eq!(shown[0], (Some("•"), 1, 0, "one".to_owned()));
        assert_eq!(shown[1], (Some("•"), 1, 0, "two".to_owned()));
        assert_eq!(shown[2], (Some("1."), 2, 0, "inner".to_owned()));
        assert_eq!(shown[3].0, Some("☑"));
        assert!(shown[3].3.ends_with("done"), "{shown:?}");
        assert_eq!(shown[4], (None, 0, 1, "quoted\nlines".to_owned()));
    }

    #[test]
    fn tables_give_their_cells() {
        let src = "| a | b |\n|---|---|\n| 1 | 22 |\n";
        let (buf, b) = parse(src);
        let Kind::Table(rows) = &b[0].kind else {
            panic!("{b:?}");
        };
        let cells: Vec<Vec<String>> = rows
            .iter()
            .map(|r| {
                r.iter()
                    .map(|c| buf.slice(c.clone()).trim().to_owned())
                    .collect()
            })
            .collect();
        assert_eq!(cells, vec![vec!["a", "b"], vec!["1", "22"]]);
    }
}
