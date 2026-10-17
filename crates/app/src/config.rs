//! The config file: `config.toml` in the platform's config directory.
//!
//! Every setting has a default, so a missing file, a partial file, or one
//! with mistakes still gives a working editor. Problems become warnings shown
//! in the message line, never errors that stop startup.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Settings read from the config file.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Editor font size in points.
    pub font_size: f64,
    /// Columns per tab stop.
    pub tab_width: usize,
    /// Start new tabs with soft wrap on.
    pub soft_wrap: bool,
    /// Colour theme: "dark", "light", or the name of a user theme in the
    /// config directory's `themes` folder. Checked when the theme loads.
    pub theme: String,
    /// Key binding overrides: action id to chords; an empty list unbinds.
    pub keys: HashMap<String, Vec<String>>,
    /// File name or `*.ext` pattern to language name.
    pub languages: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 14.0,
            tab_width: 4,
            soft_wrap: false,
            theme: "dark".to_owned(),
            keys: HashMap::new(),
            languages: HashMap::new(),
        }
    }
}

/// Read one setting from `table` into `into`, keeping the default and
/// adding a warning if it has the wrong type.
fn take<T: serde::de::DeserializeOwned>(
    table: &toml::Table,
    key: &str,
    into: &mut T,
    warnings: &mut Vec<String>,
) {
    if let Some(v) = table.get(key) {
        match v.clone().try_into::<T>() {
            Ok(x) => *into = x,
            Err(e) => warnings.push(format!("config: {key}: {}; using the default", e.message())),
        }
    }
}

const KNOWN: &[&str] = &[
    "font_size",
    "tab_width",
    "soft_wrap",
    "theme",
    "keys",
    "languages",
];

/// A commented starting point written by "Open Config File" when there is
/// no config yet.
pub const TEMPLATE: &str = r#"# Nimbus configuration. Every setting is optional.

# font_size = 14.0
# tab_width = 4
# soft_wrap = false
# theme = "dark"            # or "light", or NAME for themes/NAME.toml here

# Key bindings: action id = list of chords. "Primary" is Cmd on macOS and
# Ctrl elsewhere. An empty list unbinds the action. The command palette
# (Primary+Shift+P) shows every action's name.
# [keys]
# "view.toggleWrap" = ["Alt+W"]
# "file.quickOpen" = ["Primary+P", "Primary+E"]

# Languages: a file name or "*.ext" pattern = language. Built-in languages
# are rust, markdown, json, toml, yaml, python, javascript, typescript and
# bash. Add your own as languages/NAME.toml here (simple colouring rules),
# and change a built-in's colours with queries/NAME.scm (restart to apply).
# [languages]
# "Justfile" = "bash"
# "*.jsonc" = "json"
"#;

/// Where the config file lives: `~/Library/Application Support/nimbus` on
/// macOS, `~/.config/nimbus` on Linux, `%APPDATA%\nimbus` on Windows, or
/// the folder in `NIMBUS_CONFIG_DIR` if set (for trying a config without
/// touching the real one). Themes, languages and queries live beside it.
#[must_use]
pub fn path() -> Option<PathBuf> {
    let dir = std::env::var_os("NIMBUS_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|d| d.join("nimbus")))?;
    Some(dir.join("config.toml"))
}

impl Config {
    /// Parse config text. Returns the settings (defaults for anything
    /// missing or wrong) and warnings to show.
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let table = match text.parse::<toml::Table>() {
            Ok(t) => t,
            Err(e) => {
                let msg = e.to_string();
                let first = msg.lines().next().unwrap_or("syntax error");
                warnings.push(format!("config: {first}; using defaults"));
                return (Self::default(), warnings);
            }
        };
        for key in table.keys() {
            if !KNOWN.contains(&key.as_str()) {
                warnings.push(format!("config: unknown setting {key:?}"));
            }
        }
        // Read each setting on its own, so one wrong value costs only that
        // setting, not the whole file.
        let mut config = Self::default();
        take(&table, "font_size", &mut config.font_size, &mut warnings);
        take(&table, "tab_width", &mut config.tab_width, &mut warnings);
        take(&table, "soft_wrap", &mut config.soft_wrap, &mut warnings);
        take(&table, "theme", &mut config.theme, &mut warnings);
        take(&table, "keys", &mut config.keys, &mut warnings);
        take(&table, "languages", &mut config.languages, &mut warnings);
        if !(6.0..=72.0).contains(&config.font_size) {
            warnings.push(format!(
                "config: font_size {} out of range 6–72",
                config.font_size
            ));
            config.font_size = Self::default().font_size;
        }
        if !(1..=16).contains(&config.tab_width) {
            warnings.push(format!(
                "config: tab_width {} out of range 1–16",
                config.tab_width
            ));
            config.tab_width = Self::default().tab_width;
        }
        (config, warnings)
    }

    /// Read the config file. A missing file gives the defaults silently.
    #[must_use]
    pub fn load() -> (Self, Vec<String>) {
        let Some(path) = path() else {
            return (Self::default(), Vec::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), Vec::new()),
            Err(e) => (
                Self::default(),
                vec![format!("config: cannot read {}: {e}", path.display())],
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_partial_files_use_defaults() {
        assert_eq!(Config::parse(""), (Config::default(), vec![]));
        let (c, w) = Config::parse("tab_width = 2\n");
        assert_eq!(c.tab_width, 2);
        assert_eq!(c.font_size, 14.0);
        assert_eq!(w, Vec::<String>::new());
    }

    #[test]
    fn keys_table_parses() {
        let (c, w) =
            Config::parse("[keys]\n\"view.toggleWrap\" = [\"Alt+W\"]\n\"file.save\" = []\n");
        assert_eq!(w, Vec::<String>::new());
        assert_eq!(c.keys["view.toggleWrap"], vec!["Alt+W".to_owned()]);
        assert_eq!(c.keys["file.save"], Vec::<String>::new());
    }

    #[test]
    fn mistakes_become_warnings_not_failures() {
        let (c, w) = Config::parse("tab_width = \n");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
        let (_, w) = Config::parse("tab_widht = 2\n");
        assert!(w[0].contains("tab_widht"), "{w:?}");
        let (c, w) = Config::parse("tab_width = 99\nfont_size = 2.0\ntheme = \"solar\"\n");
        // The theme name is checked when the theme loads, not here.
        assert_eq!(
            (c.tab_width, c.font_size, c.theme.as_str()),
            (4, 14.0, "solar")
        );
        assert_eq!(w.len(), 2, "{w:?}");
        let (c, w) = Config::parse("tab_width = \"wide\"\n");
        assert_eq!(c, Config::default());
        assert_eq!(w.len(), 1);
        // One wrong value does not cost the other settings.
        let (c, w) = Config::parse(
            "font_size = 16.0\nsoft_wrap = \"yes\"\n[keys]\n\"file.save\" = [\"F2\"]\n",
        );
        assert_eq!(c.font_size, 16.0);
        assert_eq!(c.keys["file.save"], vec!["F2".to_owned()]);
        assert_eq!(w.len(), 1, "{w:?}");
    }

    #[test]
    fn template_parses_cleanly() {
        assert_eq!(Config::parse(TEMPLATE), (Config::default(), vec![]));
    }
}
