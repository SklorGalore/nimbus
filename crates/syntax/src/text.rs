//! Feeding text to tree-sitter straight from the buffer's storage.
//!
//! Tree-sitter asks for text by byte offset; we answer with the rest of the
//! storage chunk containing that offset, so a parse never copies the file.

use nimbus_core::Text;
use tree_sitter::Node;

/// The bytes from `byte` to the end of the chunk that contains it; empty at
/// the end of the text, which tells tree-sitter the input is over.
#[must_use]
pub fn bytes_from(text: &dyn Text, byte: usize) -> &[u8] {
    if byte >= text.len_bytes() {
        return &[];
    }
    let (chunk, start) = text.chunk_at(byte);
    chunk.as_bytes().get(byte - start..).unwrap_or(&[])
}

/// The text of a node, chunk by chunk, for query predicates such as
/// `#match?` that need to read it.
pub struct NodeChunks<'a> {
    text: &'a dyn Text,
    pos: usize,
    end: usize,
}

impl<'a> NodeChunks<'a> {
    /// The chunks covering `node` in `text`.
    #[must_use]
    pub fn new(text: &'a dyn Text, node: Node<'_>) -> Self {
        Self {
            text,
            pos: node.start_byte(),
            end: node.end_byte(),
        }
    }
}

impl<'a> Iterator for NodeChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.end {
            return None;
        }
        let chunk = bytes_from(self.text, self.pos);
        if chunk.is_empty() {
            return None;
        }
        let n = chunk.len().min(self.end - self.pos);
        self.pos += n;
        chunk.get(..n)
    }
}

#[cfg(all(test, feature = "rust"))]
mod tests {
    use super::*;
    use nimbus_core::RopeBuffer;

    #[test]
    fn parsing_from_chunks_matches_parsing_a_string() {
        let src = "fn f(x: u32) -> u32 { x + 1 } // filler\n".repeat(300);
        let rope = RopeBuffer::from_text(&src);
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let from_string = parser.parse(&src, None).unwrap();
        let text: &dyn Text = &rope;
        let from_chunks = parser
            .parse_with_options(&mut |i, _| bytes_from(text, i), None, None)
            .unwrap();
        assert_eq!(
            from_chunks.root_node().to_sexp(),
            from_string.root_node().to_sexp()
        );
        assert!(src.len() > 10_000, "spans many rope chunks");
    }
}
