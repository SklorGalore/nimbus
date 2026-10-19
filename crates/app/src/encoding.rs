//! Text encodings: how the bytes of a file stand for characters (M17).
//!
//! Inside Nimbus all text is UTF-8: the buffer, every byte offset, search
//! and highlighting assume it. A file in another encoding is converted at
//! the edge: decoded to UTF-8 when it is opened ([`crate::io`]) and encoded
//! back when it is saved, so nothing above the file boundary changes. An
//! encoding is a table from bytes to characters; UTF-8 and UTF-16 can
//! spell every character, Windows-1252 and ISO-8859-1 only 256 of them, so
//! saving an emoji into a Windows-1252 file has to be refused.
//!
//! A byte order mark (BOM) is a few bytes some programs put at the start of
//! a file to say which encoding (and, for UTF-16, which byte order) follows.
//! It is not text: it is kept out of the buffer and written back on save.
//!
//! When a file has no BOM and is not valid UTF-8, its encoding is a guess
//! ([`sniff`]): bounded to the first 64 KB, and shown in the status bar so a
//! wrong guess is visible and can be corrected with "Reopen with Encoding".
//!
//! The conversions come from `encoding_rs`, the decoder Firefox uses,
//! except UTF-16 output (which `encoding_rs` does not write, following the
//! web standard) and true ISO-8859-1 (which the web standard treats as
//! Windows-1252); both are a few lines here. East Asian encodings are
//! behind the `east-asian` cargo feature, off by default (DESIGN.md §9).

/// How much of a file is looked at to guess its encoding.
pub const SNIFF_BYTES: usize = 64 * 1024;

/// A text encoding a file can be read and written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Encoding {
    /// UTF-8, the buffer's own encoding: no conversion at all.
    #[default]
    Utf8,
    /// UTF-16, little-endian (Windows' usual order).
    Utf16Le,
    /// UTF-16, big-endian.
    Utf16Be,
    /// ISO-8859-1 (Latin-1): each byte is the character with that number.
    Latin1,
    /// Another encoding `encoding_rs` knows: Windows-1252 and the other
    /// single-byte tables, and with the `east-asian` feature the
    /// multi-byte ones.
    Other(&'static encoding_rs::Encoding),
}

/// Windows-1252, the usual encoding of Western text written on Windows
/// before UTF-8, and what a file that is not UTF-8 is guessed to be.
pub const WINDOWS_1252: Encoding = Encoding::Other(&WINDOWS_1252_INIT);

// `encoding_rs::WINDOWS_1252` is a `static` reference, which a `const`
// cannot name; the `_INIT` value behind it can be.
use encoding_rs::WINDOWS_1252_INIT;

/// A choice offered by "Reopen with Encoding" and "Save with Encoding":
/// an encoding and whether the file carries a byte order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    /// The encoding.
    pub encoding: Encoding,
    /// Write (or expect) a byte order mark.
    pub bom: bool,
}

impl Choice {
    /// The name shown in the pickers: the encoding's, with "BOM" for UTF-8
    /// with a byte order mark (UTF-16 has one as a rule).
    #[must_use]
    pub fn label(self) -> &'static str {
        match (self.encoding, self.bom) {
            (Encoding::Utf8, true) => "UTF-8 BOM",
            (e, _) => e.name(),
        }
    }
}

impl Encoding {
    /// The name shown in the status bar and the pickers.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16 LE",
            Self::Utf16Be => "UTF-16 BE",
            Self::Latin1 => "ISO-8859-1",
            Self::Other(e) => match e.name() {
                "windows-874" => "Windows-874",
                "windows-1250" => "Windows-1250",
                "windows-1251" => "Windows-1251",
                "windows-1252" => "Windows-1252",
                "windows-1253" => "Windows-1253",
                "windows-1254" => "Windows-1254",
                "windows-1255" => "Windows-1255",
                "windows-1256" => "Windows-1256",
                "windows-1257" => "Windows-1257",
                "windows-1258" => "Windows-1258",
                "macintosh" => "Mac Roman",
                "x-mac-cyrillic" => "Mac Cyrillic",
                other => other,
            },
        }
    }

    /// Whether this is UTF-8, which needs no conversion.
    #[must_use]
    pub fn is_utf8(self) -> bool {
        self == Self::Utf8
    }

    /// The byte order mark of this encoding, if it has one.
    #[must_use]
    pub fn bom(self) -> Option<&'static [u8]> {
        match self {
            Self::Utf8 => Some(b"\xEF\xBB\xBF"),
            Self::Utf16Le => Some(b"\xFF\xFE"),
            Self::Utf16Be => Some(b"\xFE\xFF"),
            Self::Latin1 | Self::Other(_) => None,
        }
    }

    /// The encoding a name stands for, as `--encoding` and the config's
    /// `[encodings]` table write it: `utf-8`, `utf-16le`, `latin1`,
    /// `windows-1252`, `cp1252`, … Case, and `_` or a space for `-`, do not matter.
    /// `None` for a name not known, or for an East Asian encoding in a
    /// build without the `east-asian` feature.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        let label = label.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        let ours = match label.as_str() {
            "utf-8" | "utf8" => Some(Self::Utf8),
            "utf-16" | "utf-16le" | "utf16" | "utf16le" | "utf-16-le" => Some(Self::Utf16Le),
            "utf-16be" | "utf16be" | "utf-16-be" => Some(Self::Utf16Be),
            "latin1" | "latin-1" | "iso-8859-1" | "iso8859-1" | "l1" => Some(Self::Latin1),
            "windows-1252" | "cp1252" | "win1252" | "ansi" => Some(WINDOWS_1252),
            "mac-roman" | "macroman" => Some(Self::Other(encoding_rs::MACINTOSH)),
            "mac-cyrillic" => Some(Self::Other(encoding_rs::X_MAC_CYRILLIC)),
            _ => None,
        };
        ours.or_else(|| Self::from_encoding_rs(encoding_rs::Encoding::for_label(label.as_bytes())?))
    }

    /// Ours for an `encoding_rs` encoding, leaving out the ones not offered:
    /// its stand-ins for undecodable input, and East Asian encodings when
    /// their feature is off.
    fn from_encoding_rs(e: &'static encoding_rs::Encoding) -> Option<Self> {
        if e == encoding_rs::UTF_8 {
            Some(Self::Utf8)
        } else if e == encoding_rs::UTF_16LE {
            Some(Self::Utf16Le)
        } else if e == encoding_rs::UTF_16BE {
            Some(Self::Utf16Be)
        } else if e == encoding_rs::REPLACEMENT || e == encoding_rs::X_USER_DEFINED {
            None
        } else if e.is_single_byte() || cfg!(feature = "east-asian") {
            Some(Self::Other(e))
        } else {
            None
        }
    }

    /// Every encoding offered by the pickers, most common first.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut all = vec![
            Self::Utf8,
            Self::Utf16Le,
            Self::Utf16Be,
            WINDOWS_1252,
            Self::Latin1,
        ];
        let others = [
            encoding_rs::ISO_8859_15,
            encoding_rs::WINDOWS_1250,
            encoding_rs::WINDOWS_1251,
            encoding_rs::WINDOWS_1253,
            encoding_rs::WINDOWS_1254,
            encoding_rs::WINDOWS_1255,
            encoding_rs::WINDOWS_1256,
            encoding_rs::WINDOWS_1257,
            encoding_rs::WINDOWS_1258,
            encoding_rs::WINDOWS_874,
            encoding_rs::ISO_8859_2,
            encoding_rs::ISO_8859_3,
            encoding_rs::ISO_8859_4,
            encoding_rs::ISO_8859_5,
            encoding_rs::ISO_8859_6,
            encoding_rs::ISO_8859_7,
            encoding_rs::ISO_8859_8,
            encoding_rs::ISO_8859_10,
            encoding_rs::ISO_8859_13,
            encoding_rs::ISO_8859_14,
            encoding_rs::ISO_8859_16,
            encoding_rs::KOI8_R,
            encoding_rs::KOI8_U,
            encoding_rs::IBM866,
            encoding_rs::MACINTOSH,
            encoding_rs::X_MAC_CYRILLIC,
        ];
        all.extend(others.into_iter().map(Self::Other));
        #[cfg(feature = "east-asian")]
        all.extend(
            [
                encoding_rs::SHIFT_JIS,
                encoding_rs::EUC_JP,
                encoding_rs::ISO_2022_JP,
                encoding_rs::GBK,
                encoding_rs::GB18030,
                encoding_rs::BIG5,
                encoding_rs::EUC_KR,
            ]
            .into_iter()
            .map(Self::Other),
        );
        all
    }

    /// A decoder for this encoding. Byte order marks are not looked for:
    /// the caller has already skipped one.
    #[must_use]
    pub fn decoder(self) -> Decoder {
        let inner = match self {
            Self::Latin1 => DecoderKind::Latin1,
            Self::Utf8 => DecoderKind::Rs(encoding_rs::UTF_8.new_decoder_without_bom_handling()),
            Self::Utf16Le => {
                DecoderKind::Rs(encoding_rs::UTF_16LE.new_decoder_without_bom_handling())
            }
            Self::Utf16Be => {
                DecoderKind::Rs(encoding_rs::UTF_16BE.new_decoder_without_bom_handling())
            }
            Self::Other(e) => DecoderKind::Rs(e.new_decoder_without_bom_handling()),
        };
        Decoder {
            inner,
            lossy: false,
        }
    }

    /// An encoder for this encoding.
    #[must_use]
    pub fn encoder(self) -> Encoder {
        Encoder(match self {
            Self::Utf8 => EncoderKind::Utf8,
            Self::Utf16Le => EncoderKind::Utf16 { big_endian: false },
            Self::Utf16Be => EncoderKind::Utf16 { big_endian: true },
            Self::Latin1 => EncoderKind::Latin1,
            Self::Other(e) => EncoderKind::Rs(e.new_encoder()),
        })
    }
}

/// Turns a file's bytes into UTF-8 text, a block at a time.
pub struct Decoder {
    inner: DecoderKind,
    /// Some bytes were not valid in the encoding and became U+FFFD.
    lossy: bool,
}

enum DecoderKind {
    Rs(encoding_rs::Decoder),
    Latin1,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("lossy", &self.lossy)
            .finish_non_exhaustive()
    }
}

impl Decoder {
    /// Decode `src`, appending the text to `dst`. `last` says no more bytes
    /// follow, so a character cut short at the end is an error rather than
    /// something to finish with the next block. Bytes that are not valid
    /// become U+FFFD and mark the decoder [`lossy`](Decoder::lossy).
    pub fn decode(&mut self, src: &[u8], last: bool, dst: &mut String) {
        match &mut self.inner {
            DecoderKind::Latin1 => dst.extend(src.iter().map(|&b| char::from(b))),
            DecoderKind::Rs(d) => {
                let mut src = src;
                loop {
                    // The worst case for the rest, so one round is usually
                    // enough; more if it would overflow.
                    let need = d
                        .max_utf8_buffer_length(src.len())
                        .unwrap_or(src.len().saturating_mul(3));
                    dst.reserve(need.max(4));
                    let (result, read, replaced) = d.decode_to_string(src, dst, last);
                    self.lossy |= replaced;
                    src = &src[read..];
                    match result {
                        encoding_rs::CoderResult::InputEmpty => break,
                        encoding_rs::CoderResult::OutputFull => {}
                    }
                }
            }
        }
    }

    /// Whether any bytes so far were not valid in the encoding.
    #[must_use]
    pub fn lossy(&self) -> bool {
        self.lossy
    }
}

/// Turns UTF-8 text into a file's bytes, a block at a time.
pub struct Encoder(EncoderKind);

enum EncoderKind {
    Utf8,
    Utf16 { big_endian: bool },
    Latin1,
    Rs(encoding_rs::Encoder),
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder").finish_non_exhaustive()
    }
}

/// A character the target encoding has no bytes for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unmappable {
    /// The character.
    pub ch: char,
    /// Its byte offset in the text given to [`Encoder::encode`].
    pub at: usize,
}

impl Encoder {
    /// Encode `src`, appending the bytes to `out`. `last` says no more text
    /// follows (a stateful encoding such as ISO-2022-JP then returns to its
    /// initial state).
    ///
    /// # Errors
    ///
    /// Returns the first character the encoding cannot hold, and where it
    /// is in `src`; what came before it is in `out`.
    pub fn encode(&mut self, src: &str, last: bool, out: &mut Vec<u8>) -> Result<(), Unmappable> {
        match &mut self.0 {
            EncoderKind::Utf8 => out.extend_from_slice(src.as_bytes()),
            EncoderKind::Utf16 { big_endian } => {
                out.reserve(src.len() * 2);
                for unit in src.encode_utf16() {
                    out.extend_from_slice(&if *big_endian {
                        unit.to_be_bytes()
                    } else {
                        unit.to_le_bytes()
                    });
                }
            }
            EncoderKind::Latin1 => {
                out.reserve(src.len());
                for (at, ch) in src.char_indices() {
                    match u8::try_from(u32::from(ch)) {
                        Ok(b) => out.push(b),
                        Err(_) => return Err(Unmappable { ch, at }),
                    }
                }
            }
            EncoderKind::Rs(e) => {
                let mut done = 0;
                loop {
                    let rest = &src[done..];
                    let need = e
                        .max_buffer_length_from_utf8_without_replacement(rest.len())
                        .unwrap_or(rest.len().saturating_mul(2));
                    out.reserve(need.max(8));
                    let (result, read) =
                        e.encode_from_utf8_to_vec_without_replacement(rest, out, last);
                    done += read;
                    match result {
                        encoding_rs::EncoderResult::InputEmpty => break,
                        encoding_rs::EncoderResult::OutputFull => {}
                        encoding_rs::EncoderResult::Unmappable(_) => {
                            // The character counts as read, so it is the
                            // last one read. It is taken from the text
                            // rather than from the result: ISO-2022-JP
                            // reports its escape characters (ESC, SO, SI)
                            // as U+FFFD, whose length is not theirs (found
                            // by the `encoding` fuzz target, M26).
                            let ch = rest[..read].chars().next_back().unwrap_or('\u{FFFD}');
                            return Err(Unmappable {
                                ch,
                                at: done.saturating_sub(ch.len_utf8()),
                            });
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// The encoding a byte order mark at the start of `bytes` names, and the
/// mark's length. UTF-16 marks are told apart by their byte order.
#[must_use]
pub fn bom_of(bytes: &[u8]) -> Option<(Encoding, usize)> {
    [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be]
        .into_iter()
        .find_map(|e| {
            let mark = e.bom()?;
            bytes.starts_with(mark).then_some((e, mark.len()))
        })
}

/// What the first bytes of a file suggest it is, with no byte order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    /// Valid UTF-8 as far as the sample goes.
    Utf8,
    /// Not UTF-8, but reads as Windows-1252 text.
    Windows1252,
    /// Neither: treat it as bytes (M7's path), as for a binary file.
    Bytes,
}

/// Guess the encoding of a huge file, which is not read whole, from
/// `sample`: its first [`SNIFF_BYTES`] (fewer if the file is shorter;
/// `complete` says the sample is the whole file). UTF-8 first, since any
/// ASCII file is valid UTF-8 and most text files today are UTF-8; then
/// Windows-1252 if the sample [looks like Western text](looks_like_windows_1252).
#[must_use]
pub fn sniff(sample: &[u8], complete: bool) -> Sniffed {
    match std::str::from_utf8(sample) {
        Ok(_) => return Sniffed::Utf8,
        // A character cut off by the end of the sample is not an error.
        Err(e) if e.error_len().is_none() && !complete => return Sniffed::Utf8,
        Err(_) => {}
    }
    if looks_like_windows_1252(sample) {
        Sniffed::Windows1252
    } else {
        Sniffed::Bytes
    }
}

/// Guess the encoding of a file found not to be valid UTF-8, from
/// `sample`, its first [`SNIFF_BYTES`]. A sample that is UTF-8 with
/// non-ASCII characters in it means UTF-8 text with stray bytes further
/// on: reading it as Windows-1252 would garble every one of those
/// characters, so it stays bytes. Otherwise Windows-1252 if the sample
/// [looks like Western text](looks_like_windows_1252).
#[must_use]
pub fn sniff_not_utf8(sample: &[u8]) -> Sniffed {
    let utf8_so_far = match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    };
    if utf8_so_far && !sample.is_ascii() {
        Sniffed::Bytes
    } else if looks_like_windows_1252(sample) {
        Sniffed::Windows1252
    } else {
        Sniffed::Bytes
    }
}

/// Whether `sample` reads as Windows-1252 text.
///
/// Every byte has a meaning in Windows-1252 (as `encoding_rs` decodes it),
/// so "it decodes" proves nothing: a binary file decodes too. The check is
/// instead that the sample looks like text: no NUL or other control bytes
/// beyond tab, line breaks, form feed and escape, none of the five bytes
/// Windows-1252 leaves undefined, and at least 95 % printable characters.
#[must_use]
pub fn looks_like_windows_1252(sample: &[u8]) -> bool {
    let mut printable = 0usize;
    for &b in sample {
        match b {
            b'\t' | b'\n' | b'\r' | 0x20..=0x7E | 0xA0..=0xFF => printable += 1,
            0x0C | 0x1B => {}
            0x00..=0x1F | 0x7F | 0x81 | 0x8D | 0x8F | 0x90 | 0x9D => return false,
            // Punctuation and letters at 0x80-0x9F: € ‚ ƒ „ … † ‡ ˆ ‰ Š ‹ Œ …
            _ => printable += 1,
        }
    }
    printable * 100 >= sample.len() * 95
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(e: Encoding, bytes: &[u8], block: usize) -> (String, bool) {
        let mut d = e.decoder();
        let mut out = String::new();
        let mut blocks = bytes.chunks(block.max(1)).peekable();
        if blocks.peek().is_none() {
            d.decode(&[], true, &mut out);
        }
        while let Some(b) = blocks.next() {
            d.decode(b, blocks.peek().is_none(), &mut out);
        }
        (out, d.lossy())
    }

    fn encode(e: Encoding, text: &str) -> Result<Vec<u8>, Unmappable> {
        let mut out = Vec::new();
        e.encoder().encode(text, true, &mut out)?;
        Ok(out)
    }

    #[test]
    fn every_encoding_round_trips_what_it_can_hold() {
        let western = "café, naïve, 5 €, «quoted» – x\r\n";
        for (e, text) in [
            (Encoding::Utf8, "naïve 日本 👩‍💻\n"),
            (Encoding::Utf16Le, "naïve 日本 👩‍💻\n"),
            (Encoding::Utf16Be, "naïve 日本 👩‍💻\n"),
            (WINDOWS_1252, western),
            (Encoding::Latin1, "café, naïve, «quoted» ÿ\u{80}\n"),
            (Encoding::Other(encoding_rs::WINDOWS_1251), "Привет, мир\n"),
        ] {
            let bytes = encode(e, text).unwrap();
            for block in [1, 2, 3, 7, 1024] {
                assert_eq!(decode(e, &bytes, block), (text.to_owned(), false), "{e:?}");
            }
        }
    }

    #[test]
    fn single_byte_files_round_trip_byte_for_byte() {
        // Every byte value: decoding then encoding gives the bytes back.
        let all: Vec<u8> = (0..=255).collect();
        for e in [WINDOWS_1252, Encoding::Latin1] {
            let (text, lossy) = decode(e, &all, 13);
            assert!(!lossy, "{e:?}");
            assert_eq!(encode(e, &text).unwrap(), all, "{e:?}");
        }
    }

    #[test]
    fn unencodable_characters_are_found_with_their_offset() {
        let text = "ok €\nthen 😀 here";
        let at = text.find('😀').unwrap();
        for e in [WINDOWS_1252, Encoding::Latin1] {
            let err = encode(e, text).unwrap_err();
            let expected = if e == Encoding::Latin1 {
                Unmappable {
                    ch: '€',
                    at: text.find('€').unwrap(),
                }
            } else {
                Unmappable { ch: '😀', at }
            };
            assert_eq!(err, expected, "{e:?}");
        }
        assert!(encode(Encoding::Utf16Le, text).is_ok());
        // ISO-2022-JP cannot write its own escape character, and reports it
        // as U+FFFD; the error still names the character and its offset.
        let jis = Encoding::Other(encoding_rs::ISO_2022_JP);
        assert_eq!(
            encode(jis, "m\u{1b}\u{1b}").unwrap_err(),
            Unmappable { ch: '\u{1b}', at: 1 }
        );
    }

    #[test]
    fn invalid_bytes_decode_as_replacement_and_mark_lossy() {
        // An unpaired surrogate in UTF-16 LE.
        let (text, lossy) = decode(Encoding::Utf16Le, b"a\x00\x00\xD8b\x00", 2);
        assert_eq!(text, "a\u{fffd}b");
        assert!(lossy);
    }

    #[test]
    fn byte_order_marks_name_their_encoding() {
        assert_eq!(bom_of(b"\xEF\xBB\xBFx"), Some((Encoding::Utf8, 3)));
        assert_eq!(bom_of(b"\xFF\xFEx\x00"), Some((Encoding::Utf16Le, 2)));
        assert_eq!(bom_of(b"\xFE\xFF\x00x"), Some((Encoding::Utf16Be, 2)));
        assert_eq!(bom_of(b"plain"), None);
    }

    #[test]
    fn sniffing_tells_utf8_western_text_and_binary_apart() {
        assert_eq!(sniff(b"plain ascii\n", true), Sniffed::Utf8);
        assert_eq!(sniff("café".as_bytes(), true), Sniffed::Utf8);
        // A character cut by the end of the sample is still UTF-8 ...
        assert_eq!(sniff(&"café".as_bytes()[..4], false), Sniffed::Utf8);
        // ... unless the sample is the whole file.
        assert_eq!(sniff(&"café".as_bytes()[..4], true), Sniffed::Windows1252);
        assert_eq!(sniff(b"caf\xe9 au lait\r\n", true), Sniffed::Windows1252);
        assert_eq!(sniff(b"\x7fELF\x02\x01\x01\x00\xff", true), Sniffed::Bytes);
        assert_eq!(sniff(b"abc\x81def", true), Sniffed::Bytes);
        // After the whole file failed as UTF-8: ASCII so far means the
        // stray bytes are later, likely Western text ...
        assert_eq!(sniff_not_utf8(b"id,name\n1,ok\n"), Sniffed::Windows1252);
        // ... but UTF-8 characters so far mean UTF-8 with stray bytes.
        assert_eq!(sniff_not_utf8("id,café\n".as_bytes()), Sniffed::Bytes);
        assert_eq!(sniff_not_utf8(b"caf\xe9\n"), Sniffed::Windows1252);
        // Mostly form feeds and escapes: control bytes, not text.
        assert_eq!(sniff(b"\x1b\x1b\x1b\x0c\x0c\xe9", true), Sniffed::Bytes);
    }

    #[test]
    fn labels_name_encodings() {
        assert_eq!(Encoding::from_label("UTF-8"), Some(Encoding::Utf8));
        assert_eq!(Encoding::from_label("latin1"), Some(Encoding::Latin1));
        assert_eq!(Encoding::from_label("cp1252"), Some(WINDOWS_1252));
        assert_eq!(Encoding::from_label("Windows_1252"), Some(WINDOWS_1252));
        assert_eq!(Encoding::from_label("utf-16"), Some(Encoding::Utf16Le));
        assert_eq!(
            Encoding::from_label("koi8-r").map(Encoding::name),
            Some("KOI8-R")
        );
        assert_eq!(Encoding::from_label("klingon"), None);
        assert_eq!(
            Encoding::from_label("shift_jis").is_some(),
            cfg!(feature = "east-asian")
        );
        for e in Encoding::all() {
            assert_eq!(Encoding::from_label(e.name()), Some(e), "{}", e.name());
        }
    }
}
