//! Font fallback (M8): finding characters the bundled font lacks (Chinese,
//! Japanese and Korean, many symbols) in the system's fonts.
//!
//! No one font covers everything: Hack has Latin, Greek, Cyrillic and a
//! few thousand symbols, but not the tens of thousands of CJK characters.
//! When the renderer meets a character Hack lacks, it asks this module,
//! which answers at once from what it already knows, and otherwise hands
//! the character to a worker thread and draws the missing-glyph box for
//! now. The worker, the first time, lists the system's fonts (`fontdb`;
//! it reads only their names, not their glyphs). For each character it
//! tries the fonts already loaded, then every system font from the
//! smallest file up, preferring regular upright faces, until one has the
//! character. That font's file is read into memory and sent back, and the
//! window is redrawn with the glyph in place of the box.
//!
//! Files over [`MAX_FONT_FILE`] are skipped, and the smallest file that
//! has the character is chosen, so loading costs a few megabytes, not the
//! 50 MB of a full CJK font, when a smaller one will do (owner's choice:
//! copying the file rather than mapping it, which would have needed
//! `unsafe`).
//!
//! **Colour emoji (M16).** Since the atlas gained a colour page, fonts made
//! of colour bitmaps or colour layers count too. For an emoji character
//! the colour fonts are tried first, so 🎉 comes from Apple Color Emoji,
//! Segoe UI Emoji or Noto Color Emoji rather than from a small font with a
//! black-and-white drawing of it. Apple Color Emoji is close to 200 MB, far
//! over [`MAX_FONT_FILE`], so a colour font is allowed up to
//! [`MAX_EMOJI_FILE`] and read into memory like any other fallback font.
//! That costs its whole size in memory on the first emoji (owner's choice
//! in M16: copying, again, rather than mapping with `unsafe`).
//! Nothing here runs before the first missing character, so startup is
//! unaffected.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use swash::FontRef;

use crate::font::Font;

/// Font files larger than this are never loaded: reading one into memory
/// would cost more than the budget allows for one script's characters.
pub const MAX_FONT_FILE: u64 = 64 * 1024 * 1024;

/// The size limit for a colour emoji font, which has no smaller
/// alternative: Apple Color Emoji is about 200 MB. Anything past this is
/// skipped, and emoji then draw from an outline font or as a box.
pub const MAX_EMOJI_FILE: u64 = 512 * 1024 * 1024;

/// The first face index fallback fonts get; 0 and 1 are the bundled
/// regular and bold faces.
pub(crate) const FIRST_FALLBACK: u8 = 2;

/// A wake-up for the window, so the frame is redrawn when a font arrives.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// The worker's answer for one character: which face draws it, with the
/// font itself if this is the first character it draws; `None` if no font
/// has it.
type Answer = (char, Option<(u8, Option<Font>)>);

/// The renderer's side of font fallback.
pub(crate) struct Fallback {
    /// Characters already answered: the face that draws each, or `None`.
    known: HashMap<char, Option<u8>>,
    /// Characters asked about and not answered yet.
    pending: HashSet<char>,
    worker: Option<(Sender<char>, Receiver<Answer>)>,
    notify: Option<Notify>,
}

impl Fallback {
    pub(crate) fn new() -> Self {
        Self {
            known: HashMap::new(),
            pending: HashSet::new(),
            worker: None,
            notify: None,
        }
    }

    /// Call `notify` whenever a font arrives (from the worker thread).
    pub(crate) fn set_notify(&mut self, notify: Notify) {
        self.notify = Some(notify);
    }

    /// The face that draws `c`, if known. Unknown characters are asked
    /// about once; until the answer arrives this returns `None`.
    pub(crate) fn face_for(&mut self, c: char) -> Option<u8> {
        if let Some(&face) = self.known.get(&c) {
            return face;
        }
        if self.pending.insert(c) {
            self.ask(c);
        }
        None
    }

    /// Whether characters are still being looked for.
    pub(crate) fn is_waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Take the worker's answers that have arrived, adding each new font
    /// to `faces` (whose length is its face index). Returns whether any
    /// arrived, so the caller knows to draw again.
    pub(crate) fn receive(&mut self, faces: &mut Vec<Font>) -> bool {
        let Some((_, from)) = &self.worker else {
            return false;
        };
        let mut any = false;
        loop {
            match from.try_recv() {
                Ok((c, answer)) => {
                    self.pending.remove(&c);
                    let face = answer.map(|(face, font)| {
                        if let Some(font) = font {
                            faces.push(font);
                        }
                        face
                    });
                    self.known.insert(c, face);
                    any = true;
                }
                Err(TryRecvError::Empty) => return any,
                Err(TryRecvError::Disconnected) => {
                    // The worker failed to start or stopped: what is still
                    // pending stays as boxes.
                    for c in self.pending.drain() {
                        self.known.insert(c, None);
                    }
                    return any;
                }
            }
        }
    }

    fn ask(&mut self, c: char) {
        if self.worker.is_none() {
            let (to_worker, requests) = channel();
            let (answers, from_worker) = channel();
            let notify = self.notify.clone();
            let spawned = std::thread::Builder::new()
                .name("nimbus-fonts".to_owned())
                .spawn(move || run_worker(&requests, &answers, notify.as_deref()));
            if let Err(e) = spawned {
                log::warn!("font fallback unavailable: {e}");
            }
            self.worker = Some((to_worker, from_worker));
        }
        if let Some((to, _)) = &self.worker {
            let _ = to.send(c);
        }
    }
}

/// A system font the worker may load: where it is and how to rank it.
struct Candidate {
    id: fontdb::ID,
    path: PathBuf,
    index: u32,
    /// Smaller is tried first: (not upright, distance from regular weight,
    /// file size).
    rank: (bool, u16, u64),
    /// The file is over [`MAX_FONT_FILE`]: kept only in case it is a colour
    /// font (up to [`MAX_EMOJI_FILE`]).
    large: bool,
}

/// The worker thread: answer each character asked about, in order.
fn run_worker(
    requests: &Receiver<char>,
    answers: &Sender<Answer>,
    notify: Option<&(dyn Fn() + Send + Sync)>,
) {
    let started = std::time::Instant::now();
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let candidates = candidates(&db);
    log::debug!(
        "font fallback: {} system faces, {} usable, listed in {:?}",
        db.len(),
        candidates.len(),
        started.elapsed()
    );
    // Fonts loaded so far, with the face index each was given.
    let mut loaded: Vec<(Font, PathBuf, u32)> = Vec::new();
    // Candidates known not to work (no outlines, unreadable).
    let mut useless: HashSet<fontdb::ID> = HashSet::new();
    while let Ok(c) = requests.recv() {
        let answer = find(c, &db, &candidates, &mut loaded, &mut useless);
        if answers.send((c, answer)).is_err() {
            return;
        }
        if let Some(notify) = notify {
            notify();
        }
    }
}

/// Every usable system face, best first.
fn candidates(db: &fontdb::Database) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = db
        .faces()
        .filter_map(|face| {
            let path = match &face.source {
                fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => p.clone(),
                fontdb::Source::Binary(_) => return None,
            };
            // macOS's last-resort font maps every character to a
            // placeholder picture, which is no better than our box.
            if face
                .families
                .iter()
                .any(|(name, _)| name.contains("LastResort"))
            {
                return None;
            }
            let size = std::fs::metadata(&path).ok()?.len();
            let large = size > MAX_FONT_FILE;
            // Only a font with "emoji" in its name is worth reading when
            // large; anything else that size is a CJK font and skipped.
            let emoji = face
                .families
                .iter()
                .any(|(name, _)| name.to_ascii_lowercase().contains("emoji"));
            (!large || (emoji && size <= MAX_EMOJI_FILE)).then(|| Candidate {
                id: face.id,
                path,
                index: face.index,
                rank: (
                    face.style != fontdb::Style::Normal,
                    face.weight.0.abs_diff(400),
                    size,
                ),
                large,
            })
        })
        .collect();
    out.sort_by_key(|c| c.rank);
    out
}

/// Common characters of a script: a font chosen for one character of it
/// must have all of these too, so a run of Chinese, Japanese or Korean
/// text comes from one complete font rather than whichever small font
/// happens to have each character.
fn script_sample(c: char) -> &'static str {
    match u32::from(c) {
        // Kana.
        0x3040..=0x30FF => "あいうえおかきくけこアイウエオカキクケコ",
        // Hangul.
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => "가나다라마바사아자차카타파하한국어",
        // CJK ideographs: the most frequent in Chinese and Japanese text.
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => {
            "的一是不了人我在有他这中大来上国个到说们日本語文字"
        }
        _ => "",
    }
}

/// Whether a font has a glyph for `c` it can draw, and for the common
/// characters of its script.
fn draws_script(font: FontRef<'_>, c: char) -> bool {
    draws(font, c) && script_sample(c).chars().all(|s| font.charmap().map(s) != 0)
}

/// Whether `font` has `table`.
fn has(font: FontRef<'_>, table: [u8; 4]) -> bool {
    font.table(swash::tag_from_bytes(&table)).is_some()
}

/// Whether a font has colour glyphs: colour bitmaps (`CBDT`, `sbix`) or
/// colour layers (`COLR`).
fn is_color(font: FontRef<'_>) -> bool {
    [*b"CBDT", *b"sbix", *b"COLR"]
        .into_iter()
        .any(|t| has(font, t))
}

/// Whether a font has a glyph for `c` it can draw: an outline, or a colour
/// glyph now that the atlas has a colour page. (Black-and-white bitmap
/// fonts are still skipped.)
fn draws(font: FontRef<'_>, c: char) -> bool {
    let outlines = [*b"glyf", *b"CFF ", *b"CFF2"]
        .into_iter()
        .any(|t| has(font, t));
    (outlines || is_color(font)) && font.charmap().map(c) != 0
}

/// Whether `c` is usually drawn as an emoji: the pictographic blocks, plus
/// the older symbols (☀, ❤, ✂) that emoji fonts draw in colour. For these
/// a colour font is tried before any other.
#[must_use]
pub fn is_emoji(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF
    )
}

/// Find a face for `c`: one already loaded, else the best system font that
/// has it, read into memory.
fn find(
    c: char,
    db: &fontdb::Database,
    candidates: &[Candidate],
    loaded: &mut Vec<(Font, PathBuf, u32)>,
    useless: &mut HashSet<fontdb::ID>,
) -> Option<(u8, Option<Font>)> {
    let emoji = is_emoji(c);
    // For an emoji, a loaded colour font first, then any loaded font.
    let loaded_hit = loaded
        .iter()
        .position(|(f, _, _)| emoji && f.has_color() && draws(f.face(), c))
        .or_else(|| {
            loaded
                .iter()
                .position(|(f, _, _)| draws_script(f.face(), c))
        });
    if let Some(i) = loaded_hit {
        return Some((face_number(i)?, None));
    }
    // Two passes for an emoji: colour fonts, then the rest.
    let passes: &[Option<bool>] = if emoji {
        &[Some(true), Some(false)]
    } else {
        &[None]
    };
    for &want_color in passes {
        if let Some(found) = find_in(c, want_color, db, candidates, loaded, useless) {
            return Some(found);
        }
    }
    None
}

/// One pass of [`find`] over the system fonts: those with colour glyphs
/// when `want_color` is `Some(true)`, without when `Some(false)`, or all.
fn find_in(
    c: char,
    want_color: Option<bool>,
    db: &fontdb::Database,
    candidates: &[Candidate],
    loaded: &mut Vec<(Font, PathBuf, u32)>,
    useless: &mut HashSet<fontdb::ID>,
) -> Option<(u8, Option<Font>)> {
    for cand in candidates {
        if useless.contains(&cand.id)
            || loaded
                .iter()
                .any(|(_, p, i)| *p == cand.path && *i == cand.index)
        {
            continue;
        }
        let has = db.with_face_data(cand.id, |data, index| {
            FontRef::from_index(data, index as usize).map(|f| {
                let color = is_color(f);
                // A large file is usable only as a colour font.
                let wanted = want_color.is_none_or(|w| w == color) && (!cand.large || color);
                wanted && draws_script(f, c)
            })
        });
        match has {
            Some(Some(true)) => {}
            Some(Some(false)) => continue,
            _ => {
                useless.insert(cand.id);
                continue;
            }
        }
        let Some(font) = load(&cand.path, cand.index) else {
            useless.insert(cand.id);
            continue;
        };
        log::info!(
            "font fallback: {} for U+{:04X}",
            cand.path.display(),
            u32::from(c)
        );
        let face = face_number(loaded.len())?;
        loaded.push((font, cand.path.clone(), cand.index));
        return Some((face, Some(font)));
    }
    None
}

/// The face index of the `i`-th loaded fallback font, if it fits.
fn face_number(i: usize) -> Option<u8> {
    u8::try_from(i).ok()?.checked_add(FIRST_FALLBACK)
}

/// Read a font file into memory for good: a fallback font, once needed,
/// is needed for the rest of the session, so its bytes are leaked rather
/// than tracked, which lets it be a [`Font`] like the bundled ones.
fn load(path: &PathBuf, index: u32) -> Option<Font> {
    let bytes = std::fs::read(path).ok()?;
    let data: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    Font::from_data(data, index)
}

/// A font the config names for the editor (`font = "JetBrains Mono"`), with
/// its bold weight when the family has one.
#[derive(Debug, Clone, Copy)]
pub struct EditorFont {
    /// The regular weight, which shapes every line.
    pub regular: Font,
    /// The bold weight, for themes that make captures bold; `None` draws
    /// bold text from the regular weight.
    pub bold: Option<Font>,
}

/// Find the font the config names: a font file's path, or the family name
/// of an installed font. Listing the system's fonts takes tens to hundreds
/// of milliseconds, so call this on a worker thread, never before the first
/// frame (DESIGN.md §4.3). `None` when there is no such font.
#[must_use]
pub fn find_editor_font(name: &str) -> Option<EditorFont> {
    let lower = name.to_ascii_lowercase();
    if [".ttf", ".otf", ".ttc"].iter().any(|e| lower.ends_with(e)) {
        let regular = load(&PathBuf::from(name), 0)?;
        return Some(EditorFont {
            regular,
            bold: None,
        });
    }
    let started = std::time::Instant::now();
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let face = |weight: fontdb::Weight| {
        let id = db.query(&fontdb::Query {
            families: &[fontdb::Family::Name(name)],
            weight,
            stretch: fontdb::Stretch::Normal,
            style: fontdb::Style::Normal,
        })?;
        let info = db.face(id)?;
        let path = match &info.source {
            fontdb::Source::File(p) | fontdb::Source::SharedFile(p, _) => p.clone(),
            fontdb::Source::Binary(_) => return None,
        };
        Some((info.weight, path, info.index))
    };
    let (_, path, index) = face(fontdb::Weight::NORMAL)?;
    let regular = load(&path, index)?;
    // A family with no bold answers the bold query with its regular face.
    let bold = face(fontdb::Weight::BOLD)
        .filter(|(w, _, _)| w.0 >= 600)
        .and_then(|(_, p, i)| load(&p, i));
    log::info!(
        "editor font {name:?}: {} (bold: {}) in {:?}",
        path.display(),
        bold.is_some(),
        started.elapsed()
    );
    Some(EditorFont { regular, bold })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_numbers_start_after_the_bundled_faces() {
        assert_eq!(face_number(0), Some(2));
        assert_eq!(face_number(253), Some(255));
        assert_eq!(face_number(254), None);
    }

    #[test]
    fn emoji_are_told_apart_from_text() {
        assert!(is_emoji('🎉'));
        assert!(is_emoji('❤'));
        assert!(!is_emoji('a'));
        assert!(!is_emoji('漢'));
    }

    #[test]
    fn the_bundled_font_draws_ascii_but_not_cjk() {
        let font = Font::bundled().unwrap();
        assert!(draws(font.face(), 'a'));
        assert!(!draws(font.face(), '漢'));
    }

    /// Looks through this machine's fonts: run with `--ignored` to see
    /// which file each script falls back to.
    #[test]
    #[ignore = "depends on the fonts installed on this machine"]
    fn system_fonts_cover_cjk() {
        let mut fb = Fallback::new();
        for c in ['漢', 'あ', '한', '⌘', '∀'] {
            let _ = fb.face_for(c);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut faces = Vec::new();
        while fb.is_waiting() && std::time::Instant::now() < deadline {
            fb.receive(&mut faces);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(fb.face_for('漢').map(|f| f >= FIRST_FALLBACK), Some(true));
    }
}
