//! What highlighting adds to a keystroke and to opening a file, on
//! Rust source made from this repository's own code. Opt-in:
//! `cargo test --release -p nimbus-syntax --test typing_cost -- --ignored --nocapture`.

#![cfg(feature = "rust")]

use std::time::{Duration, Instant};

use nimbus_core::{Buffer, RopeBuffer, Text};
use nimbus_syntax::LanguageId;
use nimbus_syntax::highlight::Highlighter;
use nimbus_syntax::state::SyntaxState;

#[expect(clippy::unwrap_used, reason = "test helper")]
fn rust_source(size: usize) -> String {
    let mut seed = String::new();
    let mut stack = vec![std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.."
    ))];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                seed.push_str(&std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    let mut out = String::with_capacity(size + seed.len());
    while out.len() < size {
        out.push_str(&seed);
    }
    let cut = out[..size].rfind('\n').unwrap() + 1;
    out.truncate(cut);
    out
}

fn percentile(sorted: &[Duration], q: usize) -> Duration {
    sorted[(sorted.len() * q / 100).min(sorted.len() - 1)]
}

#[test]
#[ignore = "benchmark"]
fn keystroke_cost_in_a_1mb_file() {
    let src = rust_source(1 << 20);
    let mut buf = RopeBuffer::from_text(&src);
    let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
    let t = Instant::now();
    state.parse_now(&buf);
    println!("1 MB first parse (with query load): {:?}", t.elapsed());
    let mut h = Highlighter::new();
    // Type in the middle of the file, with 60 lines on screen.
    let line = buf.len_lines() / 2;
    let mut at = buf.line_to_byte(line) + 4;
    let mut samples = Vec::new();
    let mut highlight = Vec::new();
    let mut over = 0;
    for i in 0..300 {
        let ch = if i % 10 == 9 { "\n" } else { "x" };
        let start = Instant::now();
        let edit = buf.replace(at..at, ch).unwrap();
        state.edit(&buf, &edit);
        if state.refresh(&buf, Duration::from_millis(2)).is_some() {
            over += 1;
            let l = buf.byte_to_line(at);
            println!("over budget at keystroke {i}: {:?}", buf.line(l).trim_end());
            // A worker would finish this; here, finish it in place.
            state.parse_now(&buf);
        }
        let top = buf.line_to_byte(line.saturating_sub(30));
        let bottom = buf.line_to_byte((line + 30).min(buf.len_lines() - 1));
        let parsed = Instant::now();
        let n = h
            .spans(&state, &buf, std::slice::from_ref(&(top..bottom)))
            .len();
        highlight.push(parsed.elapsed());
        samples.push(start.elapsed());
        assert_ne!(n, 0);
        at += ch.len();
    }
    samples.sort();
    highlight.sort();
    println!(
        "  of which highlighting: p50 {:?} p99 {:?}; editing at {:?}",
        percentile(&highlight, 50),
        percentile(&highlight, 99),
        buf.line(line).trim_end()
    );
    println!(
        "per keystroke (edit + reparse + highlight 60 lines): p50 {:?} p99 {:?} max {:?}; {over} over budget",
        percentile(&samples, 50),
        percentile(&samples, 99),
        samples[samples.len() - 1]
    );
    // An occasional reparse may move to a worker (the old colours stay
    // until it returns); typing must not do so routinely.
    assert!(
        over * 100 <= samples.len(),
        "{over} of {} over budget",
        samples.len()
    );
}

#[test]
#[ignore = "benchmark"]
fn first_parse_times() {
    for mb in [1, 5, 10] {
        let src = rust_source(mb << 20);
        let buf = RopeBuffer::from_text(&src);
        // Load the grammar outside the timing.
        SyntaxState::new(Some(LanguageId::Rust), 0).parse_now(&RopeBuffer::from_text("fn a() {}"));
        let mut state = SyntaxState::new(Some(LanguageId::Rust), buf.len_bytes());
        let t = Instant::now();
        state.parse_now(&buf);
        println!("{mb} MB first parse: {:?}", t.elapsed());
    }
}

#[test]
#[ignore = "benchmark"]
fn simple_highlighting_in_a_100mb_file() {
    use nimbus_syntax::lexical::Lexical;
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/out/src-100mb.rs");
    let src = std::fs::read_to_string(path).expect("run bench/gen.ps1 first");
    let mut buf = RopeBuffer::from_text(&src);
    drop(src);
    let lex = Lexical::new(nimbus_syntax::lexical::builtin(LanguageId::Rust));
    let screen = |buf: &RopeBuffer, line: usize| {
        buf.line_to_byte(line)..buf.line_to_byte((line + 60).min(buf.len_lines() - 1))
    };
    let mut out = Vec::new();
    let t = Instant::now();
    lex.spans(&buf, screen(&buf, 0), &mut out);
    println!("100 MB, first screen: {:?}", t.elapsed());
    let middle = buf.len_lines() / 2;
    let t = Instant::now();
    out.clear();
    lex.spans(&buf, screen(&buf, middle), &mut out);
    println!(
        "100 MB, jump to the middle: {:?} ({} spans)",
        t.elapsed(),
        out.len()
    );
    // Typing there: each edit drops the checkpoints after it, then redraws.
    let mut samples = Vec::new();
    let first = buf.line_to_byte(middle + 30) + 4;
    for at in first..first + 100 {
        let start = Instant::now();
        let edit = buf.replace(at..at, "x").unwrap();
        lex.edit(&buf, &edit);
        out.clear();
        lex.spans(&buf, screen(&buf, middle), &mut out);
        samples.push(start.elapsed());
    }
    samples.sort();
    println!(
        "100 MB, per keystroke (edit + spans for 60 lines): p50 {:?} p99 {:?}",
        percentile(&samples, 50),
        percentile(&samples, 99)
    );
}
