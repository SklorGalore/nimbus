//! Languages nested inside a language: the injection layer (M14).
//!
//! A Markdown file holds paragraphs whose inline structure (emphasis, code
//! spans, links) is a second grammar's business, and fenced code blocks in
//! any language at all; an HTML file holds scripts and styles. Tree-sitter
//! parses one language per tree, so each nested region gets a tree of its
//! own: a *child* parsed with `set_included_ranges`, which makes the inner
//! parser see only the bytes of that region, reading the text through the
//! same chunk callback as the host, so nothing is copied.
//!
//! Which regions are nested, and in what language, comes from the host
//! grammar's injections query (`registry::Injections`). Children are made
//! only for the part of the file on screen plus a margin, keyed by the
//! host node's `id()` (stable while the node survives incremental
//! reparses), reparsed incrementally after edits, and dropped when the
//! view scrolls far away or their host node vanishes. Children can have
//! children, to [`MAX_DEPTH`]: Markdown's inline grammar finds HTML tags.

use std::collections::HashMap;
use std::ops::Range;
use std::time::Instant;

use nimbus_core::Text;
use streaming_iterator::StreamingIterator;
use tree_sitter::{InputEdit, Node, Parser, QueryCursor, Tree};

use crate::edit::point_at;
use crate::registry::{self, Grammar, GrammarKey};
use crate::text::{NodeChunks, bytes_from};

/// A region larger than this is not injected: the host colours it. A
/// fenced block of a megabyte is not a paragraph, and parsing it would
/// stall a frame.
pub const MAX_INJECTION_BYTES: usize = 1024 * 1024;

/// How deep injections nest: a child of a child, no further.
pub const MAX_DEPTH: u8 = 2;

/// Children are kept while within this many screens of the view, in
/// each direction.
const MARGIN_SCREENS: usize = 2;

/// One nested region: its grammar, its tree, and where it is.
pub struct Child {
    /// The grammar the region is parsed with.
    pub key: GrammarKey,
    /// The grammar itself.
    pub grammar: &'static Grammar,
    /// The tree, once parsed; `None` until the budget allows.
    pub tree: Option<Tree>,
    /// Byte ranges of the region (more than one for a combined
    /// injection), kept in step with edits until the host is queried
    /// again.
    pub ranges: Vec<Range<usize>>,
    /// The tree is behind the text or the ranges.
    dirty: bool,
    /// Found by the latest query of the host; unseen children are
    /// dropped.
    seen: bool,
    /// Regions nested inside this one.
    pub children: Children,
}

impl std::fmt::Debug for Child {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Child")
            .field("key", &self.key)
            .field("ranges", &self.ranges)
            .field("parsed", &self.tree.is_some())
            .finish_non_exhaustive()
    }
}

impl Child {
    /// Whether any of the region overlaps `range`.
    #[must_use]
    pub fn intersects(&self, range: &Range<usize>) -> bool {
        self.ranges
            .iter()
            .any(|r| r.start < range.end && range.start < r.end)
    }

    fn start(&self) -> usize {
        self.ranges.first().map_or(0, |r| r.start)
    }
}

/// The children of one tree, by the host node's id.
#[derive(Debug, Default)]
pub struct Children {
    map: HashMap<usize, Child>,
}

impl Children {
    /// Children overlapping `range`, with their depth (1 for a child of
    /// the host), innermost last. Order among siblings is by position.
    pub fn visit(&self, range: &Range<usize>, f: &mut dyn FnMut(&Child, u8)) {
        self.visit_at(range, 1, f);
    }

    fn visit_at(&self, range: &Range<usize>, depth: u8, f: &mut dyn FnMut(&Child, u8)) {
        let mut in_range: Vec<&Child> = self
            .map
            .values()
            .filter(|c| c.tree.is_some() && c.intersects(range))
            .collect();
        in_range.sort_by_key(|c| c.start());
        for c in in_range {
            f(c, depth);
            c.children.visit_at(range, depth + 1, f);
        }
    }

    /// How many children there are, all depths.
    #[must_use]
    pub fn count(&self) -> usize {
        self.map.values().map(|c| 1 + c.children.count()).sum()
    }

    fn edit(&mut self, ie: &InputEdit) {
        let delta = |b: usize| b - ie.old_end_byte + ie.new_end_byte;
        for c in self.map.values_mut() {
            if let Some(t) = &mut c.tree {
                t.edit(ie);
            }
            for r in &mut c.ranges {
                if ie.start_byte <= r.end && ie.old_end_byte >= r.start {
                    // The edit touches the region: its extent is the
                    // host's to say, next time it is queried.
                    c.dirty = true;
                } else if r.start >= ie.old_end_byte {
                    *r = delta(r.start)..delta(r.end);
                }
            }
            c.children.edit(ie);
        }
    }
}

/// Everything shared by one refresh: the parsers (one per grammar, reused
/// for every child of that grammar), the query cursor, and the deadline.
struct Pass<'a> {
    parsers: &'a mut HashMap<GrammarKey, Parser>,
    cursor: &'a mut QueryCursor,
    text: &'a dyn Text,
    visible: &'a [Range<usize>],
    keep: Range<usize>,
    deadline: Option<Instant>,
    /// A parse ran in this pass: the deadline is checked only after the
    /// first, so every frame makes progress.
    parsed_one: bool,
    /// A child was skipped for the deadline.
    pending: bool,
}

/// A document's injected trees.
#[derive(Default)]
pub struct Injected {
    /// Children of the host tree.
    pub children: Children,
    parsers: HashMap<GrammarKey, Parser>,
    cursor: Option<QueryCursor>,
    rev: u64,
    pending: bool,
}

impl std::fmt::Debug for Injected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Injected")
            .field("children", &self.children.count())
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}

impl Injected {
    /// Changes whenever a child tree is parsed, replaced or dropped, so
    /// highlight caches can key on it.
    #[must_use]
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// A child was left unparsed by the last refresh's deadline, so the
    /// view is not fully coloured yet: draw again.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Drop every child: the host tree is gone or replaced.
    pub fn clear(&mut self) {
        if !self.children.map.is_empty() {
            self.children.map.clear();
            self.rev += 1;
        }
        self.pending = false;
    }

    /// Keep the children in step with an edit the host tree has seen.
    pub fn edit(&mut self, ie: &InputEdit) {
        self.children.edit(ie);
    }

    /// Bring the children up to date for a view of `visible` (sorted,
    /// disjoint byte ranges) of `text`, whose host tree is `host`: query
    /// the host for nested regions near the view, parse the new or edited
    /// ones (those on screen first) until `deadline`, drop the rest. At
    /// least one child is parsed per call even past the deadline, so a
    /// view always finishes colouring over a few frames.
    pub fn refresh(
        &mut self,
        host: &Tree,
        grammar: &Grammar,
        text: &dyn Text,
        visible: &[Range<usize>],
        deadline: Option<Instant>,
    ) {
        self.pending = false;
        let (Some(first), Some(last)) = (visible.first(), visible.last()) else {
            return;
        };
        let len = (last.end - first.start).max(4096);
        let keep = first.start.saturating_sub(MARGIN_SCREENS * len)
            ..last.end.saturating_add(MARGIN_SCREENS * len);
        let cursor = self.cursor.get_or_insert_with(QueryCursor::new);
        let mut pass = Pass {
            parsers: &mut self.parsers,
            cursor,
            text,
            visible,
            keep,
            deadline,
            parsed_one: false,
            pending: false,
        };
        if refresh_layer(&mut self.children, &mut pass, host.root_node(), grammar, 1) {
            self.rev += 1;
        }
        self.pending = pass.pending;
    }
}

/// Refresh the children of one tree (the host's, or a child's). Returns
/// whether any child changed.
fn refresh_layer(
    children: &mut Children,
    pass: &mut Pass<'_>,
    root: Node<'_>,
    grammar: &Grammar,
    depth: u8,
) -> bool {
    let Some(inj) = &grammar.injections else {
        let had = !children.map.is_empty();
        children.map.clear();
        return had;
    };
    for c in children.map.values_mut() {
        c.seen = false;
    }
    let mut found = find_regions(children, pass, root, inj);
    let before = children.map.len();
    children.map.retain(|_, c| c.seen);
    let mut changed = children.map.len() != before;

    // On-screen regions first, so a scroll colours what is seen before
    // the margin.
    found.sort_by_key(|id| {
        children
            .map
            .get(id)
            .is_none_or(|c| !pass.visible.iter().any(|v| c.intersects(v)))
    });
    for id in found {
        let Some(child) = children.map.get_mut(&id) else {
            continue;
        };
        if child.dirty || child.tree.is_none() {
            let over = pass.parsed_one && pass.deadline.is_some_and(|d| Instant::now() >= d);
            if over {
                pass.pending = true;
                continue;
            }
            if let Some(tree) = parse_child(child, pass) {
                child.tree = Some(tree);
                child.dirty = false;
                changed = true;
            }
            pass.parsed_one = true;
        }
        if depth < MAX_DEPTH
            && let Some(tree) = &child.tree
        {
            changed |= refresh_layer(
                &mut child.children,
                pass,
                tree.root_node(),
                child.grammar,
                depth + 1,
            );
        }
    }
    changed
}

/// Query `root` for nested regions near the view, adding new ones to
/// `children` and marking the ones found as seen. Returns their ids in
/// file order.
fn find_regions(
    children: &mut Children,
    pass: &mut Pass<'_>,
    root: Node<'_>,
    inj: &registry::Injections,
) -> Vec<usize> {
    let mut found: Vec<usize> = Vec::new();
    pass.cursor.set_byte_range(pass.keep.clone());
    let text = pass.text;
    let mut matches = pass.cursor.matches(&inj.query, root, |node: Node<'_>| {
        NodeChunks::new(text, node)
    });
    while let Some(m) = matches.next() {
        let key = inj
            .fixed
            .get(m.pattern_index)
            .copied()
            .flatten()
            .or_else(|| {
                let capture = inj.language?;
                let node = m.captures().iter().find(|c| c.index == capture)?.node;
                let name: String = NodeChunks::new(text, node)
                    .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
                    .collect();
                GrammarKey::from_injection_name(&name)
            });
        let Some(key) = key else {
            continue;
        };
        let mut ranges = Vec::new();
        let mut id = None;
        for c in m.captures() {
            if c.index == inj.content && c.node.start_byte() < c.node.end_byte() {
                id.get_or_insert(c.node.id());
                ranges.push(c.node.start_byte()..c.node.end_byte());
            }
        }
        let Some(id) = id else {
            continue;
        };
        if ranges.iter().map(Range::len).sum::<usize>() > MAX_INJECTION_BYTES {
            continue;
        }
        match children.map.get_mut(&id) {
            Some(c) if c.key == key => {
                if c.ranges != ranges {
                    c.ranges = ranges;
                    c.dirty = true;
                }
                c.seen = true;
            }
            _ => {
                let Some(grammar) = registry::grammar_for(key) else {
                    continue;
                };
                children.map.insert(
                    id,
                    Child {
                        key,
                        grammar,
                        tree: None,
                        ranges,
                        dirty: true,
                        seen: true,
                        children: Children::default(),
                    },
                );
            }
        }
        found.push(id);
    }
    found
}

/// Parse one region, reusing its old tree when it has one.
fn parse_child(child: &Child, pass: &mut Pass<'_>) -> Option<Tree> {
    let parser = pass.parsers.entry(child.key).or_default();
    parser.set_language(&child.grammar.language).ok()?;
    let text = pass.text;
    let ranges: Vec<tree_sitter::Range> = child
        .ranges
        .iter()
        .map(|r| tree_sitter::Range {
            start_byte: r.start,
            end_byte: r.end,
            start_point: point_at(text, r.start),
            end_point: point_at(text, r.end),
        })
        .collect();
    parser.set_included_ranges(&ranges).ok()?;
    let tree =
        parser.parse_with_options(&mut |i, _| bytes_from(text, i), child.tree.as_ref(), None);
    // Leave the parser general again for whoever uses it next.
    let _ = parser.set_included_ranges(&[]);
    tree
}
