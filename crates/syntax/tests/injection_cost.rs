//! What nested languages (M14 injections) add to a frame and a keystroke
//! in a large Markdown file. Opt-in:
//! `cargo test --release -p nimbus-syntax --test injection_cost -- --ignored --nocapture`.

#![cfg(all(feature = "markdown", feature = "rust"))]

use std::time::{Duration, Instant};

use nimbus_core::{Buffer, RopeBuffer, Text};
use nimbus_syntax::LanguageId;
use nimbus_syntax::highlight::Highlighter;
use nimbus_syntax::state::SyntaxState;

/// Markdown of about `size` bytes: headings, paragraphs with inline
/// markup, and fenced Rust blocks, in the proportions of a README.
fn markdown(size: usize) -> String {
    let piece = "## Section\n\nSome *emphasis*, **strong text**, a `code span` and a [link](https://example.com) in a paragraph that runs on for a while so that it wraps on screen like real prose does.\n\n- a list item with `code`\n- another with *emphasis*\n\n```rust\nfn example(x: u32) -> u32 {\n    let s = \"text\";\n    x + s.len() as u32\n}\n```\n\n";
    let mut out = String::with_capacity(size + piece.len());
    while out.len() < size {
        out.push_str(piece);
    }
    out
}

fn percentile(sorted: &[Duration], q: usize) -> Duration {
    sorted[(sorted.len() * q / 100).min(sorted.len() - 1)]
}

/// Highlight a 60-line screen until no injection is pending; returns
/// the number of frames it took.
fn frames_to_settle(
    h: &mut Highlighter,
    state: &SyntaxState,
    buf: &RopeBuffer,
    range: std::ops::Range<usize>,
) -> usize {
    let mut frames = 0;
    loop {
        frames += 1;
        let _ = h.spans(state, buf, std::slice::from_ref(&range));
        if !state.injections_pending() || frames > 100 {
            return frames;
        }
    }
}

#[test]
#[ignore = "benchmark"]
fn keystroke_and_scroll_cost_in_markdown() {
    for mb in [1usize, 15] {
        let src = markdown(mb << 20);
        let mut buf = RopeBuffer::from_text(&src);
        let mut state = SyntaxState::new(Some(LanguageId::Markdown), buf.len_bytes());
        let t = Instant::now();
        state.parse_now(&buf);
        println!("{mb} MB Markdown first parse: {:?}", t.elapsed());
        let mut h = Highlighter::new();
        let line = buf.len_lines() / 2;
        let screen =
            |buf: &RopeBuffer, line: usize| buf.line_to_byte(line)..buf.line_to_byte(line + 60);
        // A jump into the middle: children for the screen and margin.
        let t = Instant::now();
        let frames = frames_to_settle(&mut h, &state, &buf, screen(&buf, line));
        println!(
            "  jump to the middle: {:?} over {frames} frame(s), {} child trees",
            t.elapsed(),
            state.injected().borrow().children.count()
        );
        // Scroll a screen at a time.
        let mut scroll = Vec::new();
        for i in 1..=40 {
            let t = Instant::now();
            frames_to_settle(&mut h, &state, &buf, screen(&buf, line + 60 * i));
            scroll.push(t.elapsed());
        }
        scroll.sort();
        println!(
            "  scroll a screen: p50 {:?} p99 {:?}, {} child trees kept",
            percentile(&scroll, 50),
            percentile(&scroll, 99),
            state.injected().borrow().children.count()
        );
        // Type inside a paragraph on screen.
        let top = line + 60 * 40;
        let range = screen(&buf, top);
        let para = buf
            .slice(range.clone())
            .find("Some *emphasis*")
            .map_or(range.start, |i| range.start + i + 5);
        let mut typing = Vec::new();
        let mut over = 0;
        for at in para..para + 200 {
            let t = Instant::now();
            let edit = buf.replace(at..at, "x").expect("edit");
            state.edit(&buf, &edit);
            if state.refresh(&buf, Duration::from_millis(2)).is_some() {
                over += 1;
                state.parse_now(&buf);
            }
            let _ = h.spans(&state, &buf, std::slice::from_ref(&range));
            typing.push(t.elapsed());
        }
        typing.sort();
        println!(
            "  keystroke (edit + reparse + highlight): p50 {:?} p99 {:?}, {over} host reparses over 2 ms",
            percentile(&typing, 50),
            percentile(&typing, 99)
        );
    }
}

#[test]
#[ignore = "benchmark"]
fn where_the_frame_time_goes() {
    let src = markdown(1 << 20);
    let buf = RopeBuffer::from_text(&src);
    let mut state = SyntaxState::new(Some(LanguageId::Markdown), buf.len_bytes());
    state.parse_now(&buf);
    let mut h = Highlighter::new();
    h.injection_budget = Duration::from_secs(10);
    let line = buf.len_lines() / 2;
    let range = buf.line_to_byte(line)..buf.line_to_byte(line + 60);
    let t = Instant::now();
    let _ = h.spans(&state, &buf, std::slice::from_ref(&range));
    println!(
        "settle in one frame: {:?}, {} children",
        t.elapsed(),
        state.injected().borrow().children.count()
    );
    let t = Instant::now();
    let _ = h.spans(&state, &buf, std::slice::from_ref(&range));
    println!("same frame again (refresh only): {:?}", t.elapsed());
    let t = Instant::now();
    {
        let (tree, grammar) = state.tree().expect("tree");
        state.injected().borrow_mut().refresh(
            tree,
            grammar,
            &buf,
            std::slice::from_ref(&range),
            None,
        );
    }
    println!("refresh alone: {:?}", t.elapsed());
    state.touch();
    let t = Instant::now();
    let _ = h.spans(&state, &buf, std::slice::from_ref(&range));
    println!("spans recomputed (no parse): {:?}", t.elapsed());
}
