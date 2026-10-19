//! Fuzz encoding detection and conversion (M26): every file Nimbus opens
//! goes through `sniff` and a decoder, and every save of a file that is
//! not UTF-8 through an encoder, so a panic here loses work.
//!
//! For each encoding Nimbus offers (East Asian ones included):
//! - decoding arbitrary bytes in pieces, cut where the fuzzer says, gives
//!   the same text and the same "lossy" answer as decoding them at once
//!   (files are read in chunks, so a character split across two must
//!   come out whole);
//! - encoding arbitrary text in pieces gives the same bytes, or the same
//!   refusal, as encoding it at once;
//! - text that encodes decodes back to itself, for the encodings that
//!   promise it (UTF-8, UTF-16, Latin-1 and the single-byte tables; the
//!   East Asian ones map a few characters many-to-one, so they are only
//!   checked for not crashing).
//!
//! The app's own `encoding.rs` is compiled in with `#[path]`, as in the
//! `config` target: `cargo fuzz run encoding --features app-encoding`.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

#[allow(dead_code, reason = "the app's module, of which this uses a part")]
#[path = "../../crates/app/src/encoding.rs"]
mod encoding;

use encoding::Encoding;

#[derive(Arbitrary, Debug)]
struct Input {
    which: u8,
    bytes: Vec<u8>,
    text: String,
    cuts: Vec<u16>,
}

fuzz_target!(|input: Input| {
    let all = Encoding::all();
    let enc = all[usize::from(input.which) % all.len()];
    let _ = encoding::bom_of(&input.bytes);
    let _ = encoding::sniff_not_utf8(&input.bytes);
    for complete in [false, true] {
        let s = encoding::sniff(&input.bytes, complete);
        if std::str::from_utf8(&input.bytes).is_ok() {
            assert_eq!(s, encoding::Sniffed::Utf8);
        }
    }

    // Decoding in pieces equals decoding at once.
    let (whole, whole_lossy) = decode(enc, &input.bytes, &[]);
    let (pieces, pieces_lossy) = decode(enc, &input.bytes, &input.cuts);
    assert_eq!(whole, pieces, "{enc:?}: decoding in pieces differs");
    assert_eq!(whole_lossy, pieces_lossy, "{enc:?}: lossy differs");

    // Encoding in pieces equals encoding at once.
    let at_once = encode(enc, &input.text, &[]);
    let in_pieces = encode(enc, &input.text, &input.cuts);
    assert_eq!(at_once, in_pieces, "{enc:?}: encoding in pieces differs");

    // What encodes decodes back, where the encoding promises it.
    if let Ok(bytes) = at_once
        && round_trips(enc)
    {
        let (back, lossy) = decode(enc, &bytes, &[]);
        assert!(!lossy, "{enc:?}: its own output decodes lossily");
        assert_eq!(back, input.text, "{enc:?}: no round trip");
    }
});

fn round_trips(enc: Encoding) -> bool {
    match enc {
        Encoding::Utf8 | Encoding::Utf16Le | Encoding::Utf16Be | Encoding::Latin1 => true,
        Encoding::Other(e) => e.is_single_byte(),
    }
}

/// Byte offsets to cut `len` bytes at, from the fuzzer's numbers.
fn cut_points(len: usize, cuts: &[u16]) -> Vec<usize> {
    let mut at: Vec<usize> = cuts
        .iter()
        .take(16)
        .map(|&c| usize::from(c) % (len + 1))
        .collect();
    at.sort_unstable();
    at.dedup();
    at
}

fn decode(enc: Encoding, bytes: &[u8], cuts: &[u16]) -> (String, bool) {
    let mut d = enc.decoder();
    let mut out = String::new();
    let mut start = 0;
    for at in cut_points(bytes.len(), cuts) {
        d.decode(&bytes[start..at], false, &mut out);
        start = at;
    }
    d.decode(&bytes[start..], true, &mut out);
    (out, d.lossy())
}

fn encode(enc: Encoding, text: &str, cuts: &[u16]) -> Result<Vec<u8>, char> {
    let mut e = enc.encoder();
    let mut out = Vec::new();
    let mut start = 0;
    for at in cut_points(text.len(), cuts) {
        if !text.is_char_boundary(at) {
            continue;
        }
        e.encode(&text[start..at], false, &mut out)
            .map_err(|u| u.ch)?;
        start = at;
    }
    e.encode(&text[start..], true, &mut out).map_err(|u| u.ch)?;
    Ok(out)
}
