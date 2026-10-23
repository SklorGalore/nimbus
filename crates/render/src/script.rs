//! Scripts (M24): which writing system a character belongs to, and so
//! which fallback fonts to try for it.
//!
//! Unicode gives every character a *script* property: `a` is Latin, `я`
//! Cyrillic, `漢` Han, `か` Hiragana, `한` Hangul, `क` Devanagari. Fonts
//! are made per script, so a font chosen because it has one character of a
//! script is likely to have the rest of that script too, in a matching
//! style. Nimbus reads the property from `swash`'s Unicode tables (no extra
//! dependency) and groups it into a [`Class`]: the key of a list of
//! preferred fonts, from the config's `fallback_fonts` table and from the
//! built-in lists in [`crate::fallback`].
//!
//! Some characters belong to no one script: punctuation, digits, symbols
//! (`Common`), and combining marks (`Inherited`), which take the script of
//! the letter they attach to. A Japanese sentence ends with `。`, which is
//! Common, and its long vowel mark `ー` is Common too. Drawing those from
//! whichever font happens to have them would mix styles in one sentence,
//! so [`resolve_runs`] gives each neutral character the script of the run
//! it sits in: the text before it, or after it at the start of a line.
//! That is what "per-script runs" means here.
//!
//! **Han unification.** Chinese, Japanese and Korean share one set of code
//! points for their ideographs, though each prefers slightly different
//! shapes for many of them (`骨`, `直`, `写` differ visibly). Which shapes
//! are right depends on the reader, not the character, so
//! [`CjkLanguage`] (the config's `cjk_language`, by default the system
//! language) picks the font list for Han and the language the shaper is
//! told, which lets a font with several regional forms pick the right one
//! through its `locl` feature.

use swash::text::{Codepoint as _, Language, Script};

/// The preference list a character's fallback font comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Punctuation, digits and symbols with no script of their own and no
    /// run to join: the config's `symbols` list, then the general rule.
    Symbols,
    /// Pictographs usually drawn in colour (`🎉`, `❤`): colour fonts first.
    Emoji,
    /// Hiragana and Katakana: Japanese fonts.
    Kana,
    /// Han ideographs: the fonts of [`CjkLanguage`].
    Han,
    /// Hangul: Korean fonts.
    Hangul,
    /// Any other script.
    Other(Script),
}

/// The script of `c`, as Unicode gives it.
#[must_use]
pub fn script_of(c: char) -> Script {
    c.script()
}

/// Whether `s` takes its script from the text around it: Common
/// (punctuation, digits, symbols), Inherited (combining marks) and
/// Unknown (unassigned code points).
#[must_use]
pub fn is_neutral(s: Script) -> bool {
    matches!(s, Script::Common | Script::Inherited | Script::Unknown)
}

/// The class of a script that is not neutral.
fn class_of_script(s: Script) -> Class {
    match s {
        Script::Hiragana | Script::Katakana => Class::Kana,
        Script::Han => Class::Han,
        Script::Hangul => Class::Hangul,
        other => Class::Other(other),
    }
}

/// The script of a grapheme: that of its first character with a script
/// of its own, or `None` if all of it is neutral.
fn grapheme_script(g: &str) -> Option<Script> {
    g.chars().map(script_of).find(|&s| !is_neutral(s))
}

/// Give each grapheme in `graphemes` its [`Class`], writing them to `out`
/// (cleared first), one per grapheme in order.
///
/// A grapheme with a script of its own gets that script's class. A
/// neutral one (punctuation, digits, symbols) joins the run it sits in:
/// the script of the nearest grapheme before it that has one, or, at the
/// start, the nearest after it. A pictograph is [`Class::Emoji`] unless
/// it sits in no run, and neutral graphemes with no run at all are
/// [`Class::Symbols`]. `out` is reused, so this allocates nothing once it
/// has grown to fit a line.
pub fn resolve_runs<'a>(graphemes: impl Iterator<Item = &'a str>, out: &mut Vec<Class>) {
    out.clear();
    // `None` marks a neutral grapheme still to be resolved.
    let mut pending: Option<usize> = None;
    let mut last: Option<Script> = None;
    for g in graphemes {
        if let Some(s) = grapheme_script(g) {
            let class = class_of_script(s);
            if let Some(from) = pending.take() {
                // Leading neutrals take the first script after them.
                for slot in &mut out[from..] {
                    if *slot == Class::Symbols {
                        *slot = class;
                    }
                }
            }
            last = Some(s);
            out.push(class);
            continue;
        }
        let first = g.chars().next().unwrap_or(' ');
        let class = if crate::fallback::is_emoji(first) {
            Class::Emoji
        } else if let Some(s) = last {
            class_of_script(s)
        } else {
            pending.get_or_insert(out.len());
            Class::Symbols
        };
        out.push(class);
    }
}

/// The class of one grapheme on its own, without a run: for interface
/// text drawn a grapheme at a time.
#[must_use]
pub fn class_of(g: &str) -> Class {
    match grapheme_script(g) {
        Some(s) => class_of_script(s),
        None if g.chars().next().is_some_and(crate::fallback::is_emoji) => Class::Emoji,
        None => Class::Symbols,
    }
}

/// The script to shape a grapheme of `class` with: complex scripts such
/// as Devanagari need theirs to reorder and join letters.
pub(crate) fn shaping_script(class: Class) -> Script {
    match class {
        Class::Symbols | Class::Emoji => Script::Common,
        Class::Kana => Script::Hiragana,
        Class::Han => Script::Han,
        Class::Hangul => Script::Hangul,
        Class::Other(s) => s,
    }
}

impl Class {
    /// The key of this class in the config's `fallback_fonts` table:
    /// `han`, `kana`, `hangul`, `emoji`, `symbols`, or the script's
    /// Unicode name in lower case with `_` for spaces (`cyrillic`,
    /// `devanagari`, `old_italic`).
    #[must_use]
    pub fn key(self) -> String {
        match self {
            Class::Symbols => "symbols".to_owned(),
            Class::Emoji => "emoji".to_owned(),
            Class::Kana => "kana".to_owned(),
            Class::Han => "han".to_owned(),
            Class::Hangul => "hangul".to_owned(),
            Class::Other(s) => s.name().to_ascii_lowercase().replace(' ', "_"),
        }
    }
}

/// Keys the config's `fallback_fonts` table is checked against: the
/// classes above and the scripts a text editor is likely to meet. Others
/// still work (any Unicode script name does) but get a warning, since a
/// typo would otherwise be silent.
pub const KNOWN_KEYS: &[&str] = &[
    "symbols",
    "emoji",
    "kana",
    "han",
    "hangul",
    "bopomofo",
    "yi",
    "latin",
    "greek",
    "cyrillic",
    "armenian",
    "georgian",
    "hebrew",
    "arabic",
    "syriac",
    "thaana",
    "devanagari",
    "bengali",
    "gurmukhi",
    "gujarati",
    "oriya",
    "tamil",
    "telugu",
    "kannada",
    "malayalam",
    "sinhala",
    "thai",
    "lao",
    "tibetan",
    "myanmar",
    "khmer",
    "mongolian",
    "ethiopic",
    "cherokee",
    "canadian_aboriginal",
    "braille",
];

/// The language Han ideographs are drawn for: which regional shapes they
/// take (the config's `cjk_language`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CjkLanguage {
    /// Japanese (`ja`).
    Japanese,
    /// Simplified Chinese, as in mainland China and Singapore (`zh-Hans`).
    /// The default when the system language is not one of these four.
    #[default]
    SimplifiedChinese,
    /// Traditional Chinese, as in Taiwan and Hong Kong (`zh-Hant`).
    TraditionalChinese,
    /// Korean (`ko`).
    Korean,
}

impl CjkLanguage {
    /// Read the config's spelling: `ja`, `zh-Hans`, `zh-Hant` or `ko`, in
    /// any case, also accepting region forms (`zh-CN`, `zh-TW`, `ja-JP`).
    /// `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let lower = s.trim().to_ascii_lowercase().replace('_', "-");
        let mut parts = lower.split('-');
        match parts.next()? {
            "ja" => Some(Self::Japanese),
            "ko" => Some(Self::Korean),
            "zh" => Some(match parts.next() {
                Some("hant" | "tw" | "hk" | "mo") => Self::TraditionalChinese,
                // `zh`, `zh-Hans`, `zh-CN` and any other subtag.
                _ => Self::SimplifiedChinese,
            }),
            _ => None,
        }
    }

    /// The language from a system locale such as `ja-JP`, `zh-Hant-TW` or
    /// `en-US`: a CJK locale gives its language, any other the default,
    /// [`CjkLanguage::SimplifiedChinese`], the most widely read form.
    #[must_use]
    pub fn from_locale(locale: &str) -> Self {
        Self::parse(locale).unwrap_or_default()
    }

    /// The config's spelling.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Japanese => "ja",
            Self::SimplifiedChinese => "zh-Hans",
            Self::TraditionalChinese => "zh-Hant",
            Self::Korean => "ko",
        }
    }
}

/// The language to tell the shaper for a class, so a font with regional
/// forms (its `locl` feature) picks the right ones: Han follows
/// `cjk`, Kana is always Japanese and Hangul always Korean.
pub(crate) fn shaping_language(class: Class, cjk: CjkLanguage) -> Option<Language> {
    let tag = match class {
        Class::Han => cjk.name(),
        Class::Kana => "ja",
        Class::Hangul => "ko",
        _ => return None,
    };
    Language::parse(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(line: &str) -> Vec<Class> {
        use unicode_segmentation::UnicodeSegmentation as _;
        let mut out = Vec::new();
        resolve_runs(line.graphemes(true), &mut out);
        out
    }

    #[test]
    fn letters_take_their_own_script() {
        assert_eq!(script_of('a'), Script::Latin);
        assert_eq!(script_of('я'), Script::Cyrillic);
        assert_eq!(script_of('漢'), Script::Han);
        assert_eq!(script_of('か'), Script::Hiragana);
        assert_eq!(script_of('カ'), Script::Katakana);
        assert_eq!(script_of('한'), Script::Hangul);
        assert_eq!(script_of('क'), Script::Devanagari);
        assert!(is_neutral(script_of('。')));
        assert!(is_neutral(script_of('1')));
    }

    #[test]
    fn punctuation_joins_the_run_it_ends() {
        let c = classes("日本語。");
        assert_eq!(c, [Class::Han; 4]);
        // The long vowel mark is Common; it joins the katakana around it.
        let c = classes("コーヒー");
        assert_eq!(c, [Class::Kana; 4]);
    }

    #[test]
    fn leading_punctuation_takes_the_script_after_it() {
        let c = classes("「한국」");
        assert_eq!(c, [Class::Hangul; 4]);
    }

    #[test]
    fn a_mixed_line_is_cut_into_runs() {
        let c = classes("ab 漢字 и 한");
        let expected = [
            Class::Other(Script::Latin),
            Class::Other(Script::Latin),
            Class::Other(Script::Latin),
            Class::Han,
            Class::Han,
            Class::Han,
            Class::Other(Script::Cyrillic),
            Class::Other(Script::Cyrillic),
            Class::Hangul,
        ];
        assert_eq!(c, expected);
    }

    #[test]
    fn devanagari_graphemes_keep_their_marks() {
        // न, म, स्ते: vowel signs and the virama are Inherited or part of
        // the script, and stay in their letter's grapheme.
        let c = classes("नमस्ते");
        assert!(c.iter().all(|&k| k == Class::Other(Script::Devanagari)));
    }

    #[test]
    fn emoji_and_lone_symbols_have_their_own_classes() {
        assert_eq!(classes("🎉"), [Class::Emoji]);
        assert_eq!(classes("∀"), [Class::Symbols]);
        assert_eq!(class_of("∀"), Class::Symbols);
        assert_eq!(class_of("漢"), Class::Han);
    }

    #[test]
    fn keys_are_the_lower_case_script_names() {
        assert_eq!(Class::Han.key(), "han");
        assert_eq!(Class::Other(Script::Cyrillic).key(), "cyrillic");
        assert_eq!(Class::Other(Script::OldItalic).key(), "old_italic");
        for k in KNOWN_KEYS {
            assert_eq!(k.to_ascii_lowercase(), *k);
        }
    }

    #[test]
    fn cjk_languages_parse_from_config_and_locale() {
        assert_eq!(CjkLanguage::parse("ja"), Some(CjkLanguage::Japanese));
        assert_eq!(
            CjkLanguage::parse("zh-Hant"),
            Some(CjkLanguage::TraditionalChinese)
        );
        assert_eq!(
            CjkLanguage::parse("ZH_tw"),
            Some(CjkLanguage::TraditionalChinese)
        );
        assert_eq!(
            CjkLanguage::parse("zh-Hans"),
            Some(CjkLanguage::SimplifiedChinese)
        );
        assert_eq!(CjkLanguage::parse("ko-KR"), Some(CjkLanguage::Korean));
        assert_eq!(CjkLanguage::parse("fr"), None);
        assert_eq!(
            CjkLanguage::from_locale("en-US"),
            CjkLanguage::SimplifiedChinese
        );
        assert_eq!(CjkLanguage::from_locale("ja-JP"), CjkLanguage::Japanese);
        assert_eq!(
            CjkLanguage::from_locale("zh-Hant-HK"),
            CjkLanguage::TraditionalChinese
        );
        for l in [
            CjkLanguage::Japanese,
            CjkLanguage::SimplifiedChinese,
            CjkLanguage::TraditionalChinese,
            CjkLanguage::Korean,
        ] {
            assert_eq!(CjkLanguage::parse(l.name()), Some(l));
        }
    }

    #[test]
    fn the_shaper_is_told_the_regional_language() {
        let tag = |c, l| shaping_language(c, l).and_then(Language::to_opentype);
        let t = swash::tag_from_bytes;
        assert_eq!(tag(Class::Han, CjkLanguage::Japanese), Some(t(b"JAN ")));
        assert_eq!(
            tag(Class::Han, CjkLanguage::TraditionalChinese),
            Some(t(b"ZHT "))
        );
        assert_eq!(
            tag(Class::Han, CjkLanguage::SimplifiedChinese),
            Some(t(b"ZHS "))
        );
        assert_eq!(tag(Class::Hangul, CjkLanguage::Japanese), Some(t(b"KOR ")));
        assert_eq!(tag(Class::Other(Script::Latin), CjkLanguage::Korean), None);
    }
}
