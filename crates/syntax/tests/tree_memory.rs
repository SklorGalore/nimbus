//! How much memory a syntax tree keeps, per byte of source. Tree-sitter
//! allocates with C `malloc`, so this reads the process's resident size from
//! `ps` (Unix only). Opt-in:
//! `cargo test --release -p nimbus-syntax --test tree_memory -- --ignored --nocapture`.

#![cfg(all(feature = "rust", unix))]

use nimbus_core::{RopeBuffer, Text};
use nimbus_syntax::LanguageId;
use nimbus_syntax::state::SyntaxState;

/// Resident set size of this process in bytes.
#[expect(clippy::unwrap_used, reason = "test helper")]
fn rss() -> usize {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<usize>()
        .unwrap()
        * 1024
}

#[test]
#[ignore = "measurement"]
fn tree_bytes_per_source_byte() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/out/src-10mb.rs");
    let src = std::fs::read_to_string(path).expect("run bench/gen.sh first");
    let buf = RopeBuffer::from_text(&src);
    drop(src);
    // Load the grammar and query outside the measurement.
    SyntaxState::new(Some(LanguageId::Rust), 0).parse_now(&RopeBuffer::from_text("fn a() {}"));
    let before = rss();
    let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
    state.parse_now(&buf);
    let after = rss();
    let nodes = state.tree().unwrap().0.root_node().descendant_count();
    #[expect(clippy::cast_precision_loss, reason = "ratios for display")]
    let (ratio, per_node, density) = (
        (after - before) as f64 / buf.len_bytes() as f64,
        (after - before) as f64 / nodes as f64,
        buf.len_bytes() as f64 / nodes as f64,
    );
    println!(
        "{} MB source: +{} MB resident after parsing ({ratio:.1}x); {nodes} nodes, {per_node:.0} bytes each, one per {density:.1} source bytes",
        buf.len_bytes() >> 20,
        (after - before) >> 20,
    );
}
