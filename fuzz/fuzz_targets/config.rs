//! Fuzz the config file parser: `Config::parse` on arbitrary text must
//! never panic, and whatever it returns must be usable: every setting the
//! parser range-checks is in range (a wrong value falls back to the
//! default with a warning), and the per-language lookups work.
//!
//! The app crate is a binary with no library to link against, so this
//! target compiles the app's own `config.rs` in, unchanged, with `#[path]`.
//! That works because the file uses only std, serde, toml, dirs and
//! nimbus-core, which this crate depends on behind the `app-config`
//! feature. Run it with `cargo fuzz run config --features app-config`.

#![no_main]

use libfuzzer_sys::fuzz_target;

#[allow(dead_code, reason = "the app's module, of which this uses a part")]
#[path = "../../crates/app/src/config.rs"]
mod config;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let (config, _warnings) = config::Config::parse(&text);
    check(&config);
});

fn check(config: &config::Config) {
    assert!(
        (6.0..=72.0).contains(&config.font_size),
        "font_size {}",
        config.font_size
    );
    assert!(
        (1..=16).contains(&config.tab_width),
        "tab_width {}",
        config.tab_width
    );
    assert!(
        (40.0..=400.0).contains(&config.minimap_width),
        "minimap_width {}",
        config.minimap_width
    );
    if let Some(w) = config.indent.width {
        assert!((1..=16).contains(&w), "indent.width {w}");
    }
    let mut languages: Vec<Option<&str>> = vec![None, Some("rust"), Some("markdown")];
    languages.extend(config.editor.languages.keys().map(|k| Some(k.as_str())));
    languages.extend(config.indent.languages.keys().map(|k| Some(k.as_str())));
    for language in languages {
        let _ = config.editor.for_language(language);
        let indenting = config.indent.for_language(language);
        assert!(
            (1..=16).contains(&indenting.indent.width),
            "indent width {} for {language:?}",
            indenting.indent.width
        );
    }
}
