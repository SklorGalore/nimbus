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
//!
//! **Preferred fonts per script (M24).** Smallest-first finds *a* font,
//! not necessarily a good one: a Japanese sentence could come from a
//! Chinese font, with Chinese shapes for the shared ideographs. So each
//! request carries the character's [`Class`] (its script, from
//! [`crate::script`]), and the worker first tries the fonts preferred for
//! that class: the config's `fallback_fonts` list for it, then a built-in
//! list for the platform ([`builtin`]), with Han's list chosen by
//! [`CjkLanguage`]. A preferred font missing from this machine is skipped,
//! never an error. Only when none of them has the character does the
//! general rule above run. Answers are remembered per (class, character),
//! so a page of Japanese text costs one lookup per new character.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use swash::FontRef;

use crate::font::Font;
use crate::script::{CjkLanguage, Class};

/// Font files larger than this are never loaded: reading one into memory
/// would cost more than the budget allows for one script's characters.
pub const MAX_FONT_FILE: u64 = 64 * 1024 * 1024;

/// The size limit for a colour emoji font, which has no smaller
/// alternative: Apple Color Emoji is about 200 MB. Anything past this is
/// skipped, and emoji then draw from an outline font or as a box.
pub const MAX_EMOJI_FILE: u64 = 512 * 1024 * 1024;

/// The size limit for a font named in a preference list (the config's or
/// the built-in one): a complete CJK font such as `PingFang` or
/// `Noto Sans CJK` is tens of megabytes, and is worth its memory when it
/// is the one asked for.
pub const MAX_PREFERRED_FILE: u64 = 192 * 1024 * 1024;

/// The first face index fallback fonts get; 0 and 1 are the bundled
/// regular and bold faces.
pub(crate) const FIRST_FALLBACK: u8 = 2;

/// A wake-up for the window, so the frame is redrawn when a font arrives.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Which fonts to try first for each script (M24): the config's
/// `fallback_fonts` table and `cjk_language`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FallbackPrefs {
    /// Family names to try, in order, by [`Class::key`] (`han`, `kana`,
    /// `cyrillic`, ...). Tried before the built-in list.
    pub fonts: HashMap<String, Vec<String>>,
    /// The regional forms Han ideographs take.
    pub cjk: CjkLanguage,
}

/// A character to find a font for, and the class whose preferred fonts
/// come first.
type Wanted = (Class, char);

/// What the renderer sends the worker.
enum Request {
    /// Find a face for this character.
    Find(Wanted),
    /// Use these preferences from now on.
    Prefs(Box<FallbackPrefs>),
}

/// The worker's answer for one character: the preferences generation it
/// was found under, which face draws it, with the font itself if this is
/// the first character it draws; `None` if no font has it.
type Answer = (u32, Wanted, Option<(u8, Option<Font>)>);

/// The renderer's side of font fallback.
pub(crate) struct Fallback {
    /// Characters already answered: the face that draws each, or `None`.
    known: HashMap<Wanted, Option<u8>>,
    /// Characters asked about and not answered yet.
    pending: HashSet<Wanted>,
    worker: Option<(Sender<Request>, Receiver<Answer>)>,
    notify: Option<Notify>,
    prefs: FallbackPrefs,
    /// Counts preference changes; answers found under older preferences
    /// are not remembered.
    generation: u32,
}

impl Fallback {
    pub(crate) fn new() -> Self {
        Self {
            known: HashMap::new(),
            pending: HashSet::new(),
            worker: None,
            notify: None,
            prefs: FallbackPrefs::default(),
            generation: 0,
        }
    }

    /// Prefer these fonts from now on. Answers already found are
    /// forgotten, so characters are looked up again under the new
    /// preferences; fonts already loaded stay loaded.
    pub(crate) fn set_prefs(&mut self, prefs: FallbackPrefs) {
        if prefs == self.prefs {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        self.known.clear();
        self.pending.clear();
        if let Some((to, _)) = &self.worker {
            let _ = to.send(Request::Prefs(Box::new(prefs.clone())));
        }
        self.prefs = prefs;
    }

    /// The preferences in use.
    pub(crate) fn prefs(&self) -> &FallbackPrefs {
        &self.prefs
    }

    /// Call `notify` whenever a font arrives (from the worker thread).
    pub(crate) fn set_notify(&mut self, notify: Notify) {
        self.notify = Some(notify);
    }

    /// The face that draws `c`, a character of `class`, if known. Unknown
    /// characters are asked about once; until the answer arrives this
    /// returns `None`.
    pub(crate) fn face_for(&mut self, c: char, class: Class) -> Option<u8> {
        let wanted = (class, c);
        if let Some(&face) = self.known.get(&wanted) {
            return face;
        }
        if self.pending.insert(wanted) {
            self.ask(wanted);
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
                Ok((generation, wanted, answer)) => {
                    // A new font is pushed whatever the generation: face
                    // numbers count every font the worker loaded.
                    let face = answer.map(|(face, font)| {
                        if let Some(font) = font {
                            faces.push(font);
                        }
                        face
                    });
                    if generation == self.generation {
                        self.pending.remove(&wanted);
                        self.known.insert(wanted, face);
                    }
                    any = true;
                }
                Err(TryRecvError::Empty) => return any,
                Err(TryRecvError::Disconnected) => {
                    // The worker failed to start or stopped: what is still
                    // pending stays as boxes.
                    for wanted in self.pending.drain() {
                        self.known.insert(wanted, None);
                    }
                    return any;
                }
            }
        }
    }

    fn ask(&mut self, wanted: Wanted) {
        if self.worker.is_none() {
            let (to_worker, requests) = channel();
            let (answers, from_worker) = channel();
            let notify = self.notify.clone();
            let start = (self.prefs.clone(), self.generation);
            let spawned = std::thread::Builder::new()
                .name("nimbus-fonts".to_owned())
                .spawn(move || run_worker(start, &requests, &answers, notify.as_deref()));
            if let Err(e) = spawned {
                log::warn!("font fallback unavailable: {e}");
            }
            self.worker = Some((to_worker, from_worker));
        }
        if let Some((to, _)) = &self.worker {
            let _ = to.send(Request::Find(wanted));
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
    /// The file's size in bytes.
    size: u64,
    /// The family has "emoji" in its name: allowed up to
    /// [`MAX_EMOJI_FILE`].
    emoji: bool,
    /// Family names, in lower case, for matching preference lists.
    families: Vec<String>,
}

impl Candidate {
    /// Whether the general smallest-first rule may load it: within
    /// [`MAX_FONT_FILE`], or a colour font within [`MAX_EMOJI_FILE`].
    fn general(&self) -> bool {
        self.size <= MAX_FONT_FILE || (self.emoji && self.size <= MAX_EMOJI_FILE)
    }

    /// Over [`MAX_FONT_FILE`]: usable by the general rule only as a colour
    /// font.
    fn large(&self) -> bool {
        self.size > MAX_FONT_FILE
    }
}

/// A fallback font the worker has loaded, with the face index it was
/// given (its position in the list plus [`FIRST_FALLBACK`]).
struct Loaded {
    font: Font,
    path: PathBuf,
    index: u32,
    /// Family names in lower case.
    families: Vec<String>,
}

/// The worker's state: the system's fonts and what it has loaded.
struct Worker {
    db: fontdb::Database,
    candidates: Vec<Candidate>,
    loaded: Vec<Loaded>,
    /// Candidates known not to work (no outlines, unreadable).
    useless: HashSet<fontdb::ID>,
    prefs: FallbackPrefs,
}

/// The worker thread: answer each character asked about, in order.
fn run_worker(
    (prefs, mut generation): (FallbackPrefs, u32),
    requests: &Receiver<Request>,
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
    let mut worker = Worker {
        db,
        candidates,
        loaded: Vec::new(),
        useless: HashSet::new(),
        prefs,
    };
    while let Ok(request) = requests.recv() {
        let wanted = match request {
            Request::Prefs(prefs) => {
                worker.prefs = *prefs;
                generation = generation.wrapping_add(1);
                continue;
            }
            Request::Find(wanted) => wanted,
        };
        let answer = worker.find(wanted);
        if answers.send((generation, wanted, answer)).is_err() {
            return;
        }
        if let Some(notify) = notify {
            notify();
        }
    }
}

/// Every system face that might be used, best first. Files over
/// [`MAX_PREFERRED_FILE`] are left out unless they are colour fonts.
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
            let families: Vec<String> = face
                .families
                .iter()
                .map(|(name, _)| name.to_lowercase())
                .collect();
            // Only a font with "emoji" in its name is worth reading when
            // very large; anything else that size is skipped.
            let emoji = families.iter().any(|name| name.contains("emoji"));
            let limit = if emoji {
                MAX_EMOJI_FILE
            } else {
                MAX_PREFERRED_FILE
            };
            (size <= limit).then(|| Candidate {
                id: face.id,
                path,
                index: face.index,
                rank: (
                    face.style != fontdb::Style::Normal,
                    face.weight.0.abs_diff(400),
                    size,
                ),
                size,
                emoji,
                families,
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

impl Worker {
    /// Find a face for `c`: a preferred font for its class, else one
    /// already loaded, else the best system font that has it, read into
    /// memory.
    fn find(&mut self, (class, c): Wanted) -> Option<(u8, Option<Font>)> {
        if let Some(found) = self.find_preferred(class, c) {
            return Some(found);
        }
        let emoji = class == Class::Emoji || is_emoji(c);
        // For an emoji, a loaded colour font first, then any loaded font.
        let loaded_hit = self
            .loaded
            .iter()
            .position(|l| emoji && l.font.has_color() && draws(l.font.face(), c))
            .or_else(|| {
                self.loaded
                    .iter()
                    .position(|l| draws_script(l.font.face(), c))
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
            if let Some(found) = self.find_in(c, want_color) {
                return Some(found);
            }
        }
        None
    }

    /// The first font in `class`'s preference list (the config's, then the
    /// built-in one) that has `c`: loaded already, or loaded now.
    fn find_preferred(&mut self, class: Class, c: char) -> Option<(u8, Option<Font>)> {
        let configured = self.prefs.fonts.get(&class.key()).cloned();
        let names = configured
            .iter()
            .flatten()
            .map(String::as_str)
            .chain(builtin(class, self.prefs.cjk).iter().copied())
            .map(str::to_lowercase);
        for name in names {
            if let Some(i) = self
                .loaded
                .iter()
                .position(|l| l.families.contains(&name) && draws(l.font.face(), c))
            {
                return Some((face_number(i)?, None));
            }
            for k in 0..self.candidates.len() {
                if !self.candidates[k].families.contains(&name) {
                    continue;
                }
                if let Some(found) = self.try_load(k, c, |_, _| true) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// One pass of the general rule over the system fonts: those with
    /// colour glyphs when `want_color` is `Some(true)`, without when
    /// `Some(false)`, or all.
    fn find_in(&mut self, c: char, want_color: Option<bool>) -> Option<(u8, Option<Font>)> {
        for k in 0..self.candidates.len() {
            let cand = &self.candidates[k];
            if !cand.general() {
                continue;
            }
            let large = cand.large();
            let found = self.try_load(k, c, |f, color| {
                // A large file is usable only as a colour font.
                want_color.is_none_or(|w| w == color) && (!large || color) && draws_script(f, c)
            });
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Load candidate `k` if `accept` says it will do for `c` (given the
    /// font and whether it has colour glyphs) and it draws `c`. Skips
    /// fonts already loaded and remembers ones that cannot be read.
    fn try_load(
        &mut self,
        k: usize,
        c: char,
        accept: impl Fn(FontRef<'_>, bool) -> bool,
    ) -> Option<(u8, Option<Font>)> {
        let cand = &self.candidates[k];
        if self.useless.contains(&cand.id)
            || self
                .loaded
                .iter()
                .any(|l| l.path == cand.path && l.index == cand.index)
        {
            return None;
        }
        let has = self.db.with_face_data(cand.id, |data, index| {
            FontRef::from_index(data, index as usize).map(|f| accept(f, is_color(f)) && draws(f, c))
        });
        match has {
            Some(Some(true)) => {}
            Some(Some(false)) => return None,
            _ => {
                self.useless.insert(cand.id);
                return None;
            }
        }
        let Some(font) = load(&cand.path, cand.index) else {
            self.useless.insert(cand.id);
            return None;
        };
        log::info!(
            "font fallback: {} for U+{:04X}",
            cand.path.display(),
            u32::from(c)
        );
        let face = face_number(self.loaded.len())?;
        self.loaded.push(Loaded {
            font,
            path: cand.path.clone(),
            index: cand.index,
            families: cand.families.clone(),
        });
        Some((face, Some(font)))
    }
}

/// The built-in preference list for `class` on this platform: fonts that
/// ship with the system (or, on Linux, with common distributions), best
/// first. Han's list follows `cjk`, so `骨` gets the shapes its reader
/// expects. A font that is not installed is skipped.
#[must_use]
pub fn builtin(class: Class, cjk: CjkLanguage) -> &'static [&'static str] {
    use swash::text::Script;
    let cjk = match class {
        Class::Han => Some(cjk),
        Class::Kana => Some(CjkLanguage::Japanese),
        Class::Hangul => Some(CjkLanguage::Korean),
        Class::Other(Script::Bopomofo) => Some(CjkLanguage::TraditionalChinese),
        _ => None,
    };
    if let Some(lang) = cjk {
        return cjk_fonts(lang);
    }
    match class {
        Class::Emoji => &["Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"],
        Class::Other(s) => script_fonts(s),
        _ => &[],
    }
}

/// Fonts for one regional form of CJK text.
#[cfg(target_os = "macos")]
fn cjk_fonts(lang: CjkLanguage) -> &'static [&'static str] {
    match lang {
        CjkLanguage::Japanese => &["Hiragino Sans", "Hiragino Kaku Gothic ProN", "Osaka"],
        CjkLanguage::SimplifiedChinese => &["PingFang SC", "Hiragino Sans GB", "Heiti SC"],
        CjkLanguage::TraditionalChinese => &["PingFang TC", "PingFang HK", "Heiti TC"],
        CjkLanguage::Korean => &["Apple SD Gothic Neo", "AppleGothic"],
    }
}

/// Fonts for one regional form of CJK text.
#[cfg(target_os = "windows")]
fn cjk_fonts(lang: CjkLanguage) -> &'static [&'static str] {
    match lang {
        CjkLanguage::Japanese => &["Yu Gothic", "Meiryo", "MS Gothic"],
        CjkLanguage::SimplifiedChinese => &["Microsoft YaHei", "SimSun", "DengXian"],
        CjkLanguage::TraditionalChinese => &["Microsoft JhengHei", "MingLiU"],
        CjkLanguage::Korean => &["Malgun Gothic", "Gulim"],
    }
}

/// Fonts for one regional form of CJK text: Noto (Google) and Source Han
/// (Adobe) are the same design under two names; Linux distributions ship
/// one or the other, often only the monospaced cut.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn cjk_fonts(lang: CjkLanguage) -> &'static [&'static str] {
    match lang {
        CjkLanguage::Japanese => &[
            "Noto Sans Mono CJK JP",
            "Noto Sans CJK JP",
            "Source Han Sans JP",
            "IPAGothic",
            "TakaoGothic",
        ],
        CjkLanguage::SimplifiedChinese => &[
            "Noto Sans Mono CJK SC",
            "Noto Sans CJK SC",
            "Source Han Sans SC",
            "WenQuanYi Zen Hei Mono",
            "WenQuanYi Micro Hei",
        ],
        CjkLanguage::TraditionalChinese => &[
            "Noto Sans Mono CJK TC",
            "Noto Sans CJK TC",
            "Source Han Sans TC",
            "AR PL UMing TW",
        ],
        CjkLanguage::Korean => &[
            "Noto Sans Mono CJK KR",
            "Noto Sans CJK KR",
            "Source Han Sans KR",
            "NanumGothicCoding",
            "NanumGothic",
        ],
    }
}

/// Fonts for scripts other than CJK.
#[cfg(target_os = "macos")]
fn script_fonts(s: swash::text::Script) -> &'static [&'static str] {
    use swash::text::Script as S;
    match s {
        S::Devanagari => &["Kohinoor Devanagari", "Devanagari Sangam MN"],
        S::Bengali => &["Kohinoor Bangla", "Bangla Sangam MN"],
        S::Gurmukhi => &["Gurmukhi Sangam MN", "Gurmukhi MN"],
        S::Gujarati => &["Kohinoor Gujarati", "Gujarati Sangam MN"],
        S::Oriya => &["Oriya Sangam MN"],
        S::Tamil => &["Tamil Sangam MN", "Tamil MN"],
        S::Telugu => &["Kohinoor Telugu", "Telugu Sangam MN"],
        S::Kannada => &["Kannada Sangam MN", "Kannada MN"],
        S::Malayalam => &["Malayalam Sangam MN", "Malayalam MN"],
        S::Sinhala => &["Sinhala Sangam MN"],
        S::Thai => &["Thonburi", "Ayuthaya"],
        S::Lao => &["Lao Sangam MN"],
        S::Khmer => &["Khmer Sangam MN"],
        S::Myanmar => &["Myanmar Sangam MN"],
        S::Tibetan => &["Kailasa"],
        S::Ethiopic => &["Kefa"],
        S::Armenian => &["Mshtakan"],
        S::Georgian => &["Helvetica Neue"],
        S::Cherokee => &["Plantagenet Cherokee"],
        _ => &[],
    }
}

/// Fonts for scripts other than CJK: Nirmala UI covers the Indian scripts.
#[cfg(target_os = "windows")]
fn script_fonts(s: swash::text::Script) -> &'static [&'static str] {
    use swash::text::Script as S;
    match s {
        S::Devanagari
        | S::Bengali
        | S::Gurmukhi
        | S::Gujarati
        | S::Oriya
        | S::Tamil
        | S::Telugu
        | S::Kannada
        | S::Malayalam
        | S::Sinhala => &["Nirmala UI"],
        S::Thai | S::Lao => &["Leelawadee UI"],
        S::Khmer => &["Leelawadee UI", "Khmer UI"],
        S::Myanmar => &["Myanmar Text"],
        S::Tibetan => &["Microsoft Himalaya"],
        S::Ethiopic => &["Ebrima"],
        S::Armenian | S::Georgian => &["Sylfaen", "Segoe UI"],
        S::Cherokee => &["Gadugi"],
        S::Mongolian => &["Mongolian Baiti"],
        S::Yi => &["Microsoft Yi Baiti"],
        _ => &[],
    }
}

/// Fonts for scripts other than CJK: the Noto family, which most
/// distributions package per script.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn script_fonts(s: swash::text::Script) -> &'static [&'static str] {
    use swash::text::Script as S;
    match s {
        S::Devanagari => &["Noto Sans Devanagari", "Lohit Devanagari"],
        S::Bengali => &["Noto Sans Bengali", "Lohit Bengali"],
        S::Gurmukhi => &["Noto Sans Gurmukhi", "Lohit Gurmukhi"],
        S::Gujarati => &["Noto Sans Gujarati", "Lohit Gujarati"],
        S::Oriya => &["Noto Sans Oriya", "Lohit Odia"],
        S::Tamil => &["Noto Sans Tamil", "Lohit Tamil"],
        S::Telugu => &["Noto Sans Telugu", "Lohit Telugu"],
        S::Kannada => &["Noto Sans Kannada", "Lohit Kannada"],
        S::Malayalam => &["Noto Sans Malayalam", "Lohit Malayalam"],
        S::Sinhala => &["Noto Sans Sinhala"],
        S::Thai => &["Noto Sans Thai", "Loma"],
        S::Lao => &["Noto Sans Lao"],
        S::Khmer => &["Noto Sans Khmer"],
        S::Myanmar => &["Noto Sans Myanmar"],
        S::Tibetan => &["Noto Serif Tibetan"],
        S::Ethiopic => &["Noto Sans Ethiopic"],
        S::Armenian => &["Noto Sans Armenian"],
        S::Georgian => &["Noto Sans Georgian"],
        S::Cherokee => &["Noto Sans Cherokee"],
        S::Mongolian => &["Noto Sans Mongolian"],
        _ => &[],
    }
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

/// The weight bold text is drawn at when the regular text is drawn at
/// `regular`: three steps heavier, at least 700, at most 900.
fn bold_weight(regular: Option<u16>) -> u16 {
    regular.map_or(700, |w| w.saturating_add(300).clamp(700, 900))
}

/// A variable font as the editor font: regular text at `weight` (or the
/// font's default instance), bold text three steps heavier, both from
/// the same file (M24).
fn variable_pair(font: Font, weight: Option<u16>) -> EditorFont {
    EditorFont {
        regular: weight.map_or(font, |w| font.with_weight(f32::from(w))),
        bold: Some(font.with_weight(f32::from(bold_weight(weight)))),
    }
}

/// The bundled font as the editor font at `weight` (`font_weight` in the
/// config, M24). Hack is a static family of two weights, regular and
/// bold, so a weight of 600 or more draws everything bold, and any other
/// is regular.
///
/// # Errors
///
/// As for [`Font::bundled`].
pub fn bundled_editor_font(weight: Option<u16>) -> Result<EditorFont, crate::FontError> {
    let regular = Font::bundled()?;
    let bold = Font::bundled_bold().ok();
    Ok(match (weight, bold) {
        (Some(w), Some(b)) if w >= 600 => EditorFont {
            regular: b,
            bold: Some(b),
        },
        _ => EditorFont { regular, bold },
    })
}

/// Find the font the config names: a font file's path, or the family name
/// of an installed font, at `weight` (100 to 900; `None` for its regular
/// weight). Listing the system's fonts takes tens to hundreds of
/// milliseconds, so call this on a worker thread, never before the first
/// frame (DESIGN.md §4.3). `None` when there is no such font.
///
/// A variable font (one file with a weight axis) draws at exactly the
/// weight asked for, and its bold three steps heavier. A family of static
/// files gives the face nearest the weight, by the CSS matching rules
/// `fontdb` follows, and its bold if it has a heavier face.
#[must_use]
pub fn find_editor_font(name: &str, weight: Option<u16>) -> Option<EditorFont> {
    let lower = name.to_ascii_lowercase();
    if [".ttf", ".otf", ".ttc"].iter().any(|e| lower.ends_with(e)) {
        let regular = load(&PathBuf::from(name), 0)?;
        if regular.is_variable() {
            return Some(variable_pair(regular, weight));
        }
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
    let wanted = weight.map_or(fontdb::Weight::NORMAL, fontdb::Weight);
    let (found, path, index) = face(wanted)?;
    let regular = load(&path, index)?;
    if regular.is_variable() {
        log::info!(
            "editor font {name:?}: {} (variable, weight {}) in {:?}",
            path.display(),
            weight.map_or_else(|| "default".to_owned(), |w| w.to_string()),
            started.elapsed()
        );
        return Some(variable_pair(regular, weight));
    }
    // A family with no heavier face answers the bold query with the
    // regular one (or one barely heavier), which would draw bold as plain.
    let heavier = found.0.saturating_add(200).max(600);
    let bold = face(fontdb::Weight(bold_weight(weight)))
        .filter(|(w, _, _)| w.0 >= heavier)
        .and_then(|(_, p, i)| load(&p, i));
    log::info!(
        "editor font {name:?}: {} (weight {}, bold: {}) in {:?}",
        path.display(),
        found.0,
        bold.is_some(),
        started.elapsed()
    );
    Some(EditorFont { regular, bold })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bold_is_three_steps_heavier() {
        assert_eq!(bold_weight(None), 700);
        assert_eq!(bold_weight(Some(300)), 700);
        assert_eq!(bold_weight(Some(500)), 800);
        assert_eq!(bold_weight(Some(800)), 900);
    }

    #[test]
    fn the_bundled_font_has_two_weights() {
        let regular = bundled_editor_font(None).unwrap();
        let heavy = bundled_editor_font(Some(700)).unwrap();
        let light = bundled_editor_font(Some(300)).unwrap();
        let bold_id = Font::bundled_bold().unwrap().glyph_id('a');
        assert_eq!(
            heavy.regular.face().data.len(),
            Font::bundled_bold().unwrap().face().data.len()
        );
        assert_eq!(heavy.regular.glyph_id('a'), bold_id);
        assert_eq!(
            light.regular.face().data.len(),
            regular.regular.face().data.len()
        );
    }

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
    fn builtin_lists_cover_cjk_by_language() {
        for lang in [
            CjkLanguage::Japanese,
            CjkLanguage::SimplifiedChinese,
            CjkLanguage::TraditionalChinese,
            CjkLanguage::Korean,
        ] {
            assert_ne!(builtin(Class::Han, lang), [] as [&str; 0]);
        }
        // Kana is Japanese and Hangul Korean whatever the setting.
        assert_eq!(
            builtin(Class::Kana, CjkLanguage::Korean),
            builtin(Class::Han, CjkLanguage::Japanese)
        );
        assert_eq!(
            builtin(Class::Hangul, CjkLanguage::Japanese),
            builtin(Class::Han, CjkLanguage::Korean)
        );
        assert_ne!(
            builtin(Class::Han, CjkLanguage::Japanese),
            builtin(Class::Han, CjkLanguage::SimplifiedChinese)
        );
        assert_ne!(
            builtin(Class::Emoji, CjkLanguage::default()),
            [] as [&str; 0]
        );
        assert_eq!(
            builtin(Class::Symbols, CjkLanguage::default()),
            [] as [&str; 0]
        );
    }

    #[test]
    fn new_preferences_forget_old_answers() {
        let mut fb = Fallback::new();
        fb.known.insert((Class::Han, '漢'), Some(2));
        fb.set_prefs(FallbackPrefs::default());
        assert_eq!(fb.known.len(), 1, "the same preferences change nothing");
        fb.set_prefs(FallbackPrefs {
            cjk: CjkLanguage::Japanese,
            ..FallbackPrefs::default()
        });
        assert!(fb.known.is_empty());
        assert_eq!(fb.prefs().cjk, CjkLanguage::Japanese);
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
            let _ = fb.face_for(c, crate::script::class_of(c.encode_utf8(&mut [0; 4])));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut faces = Vec::new();
        while fb.is_waiting() && std::time::Instant::now() < deadline {
            fb.receive(&mut faces);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            fb.face_for('漢', Class::Han).map(|f| f >= FIRST_FALLBACK),
            Some(true)
        );
    }
}
