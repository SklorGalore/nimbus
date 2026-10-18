//! Window state remembered between runs: `state.toml` beside the config.
//!
//! The config file is the user's, edited by hand and full of their comments,
//! so Nimbus never writes to it. What the app remembers by itself (whether
//! the sidebar is shown, how wide it was dragged) goes in this separate file
//! instead. A missing or broken file just means the defaults.

use std::path::PathBuf;

use nimbus_core::{Normalize, Whitespace};
use serde::{Deserialize, Serialize};

use crate::sidebar::DEFAULT_WIDTH;

/// Remembered window state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The sidebar is shown.
    pub sidebar_visible: bool,
    /// Sidebar width in logical pixels.
    pub sidebar_width: f32,
    /// The compare view ignores whitespace: `"off"`, `"ends"` (leading and
    /// trailing) or `"all"` (M18).
    pub diff_ignore_whitespace: String,
    /// The compare view ignores ASCII case.
    pub diff_ignore_case: bool,
    /// The compare view drops changes made only of blank lines.
    pub diff_ignore_blank_lines: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sidebar_visible: true,
            sidebar_width: DEFAULT_WIDTH,
            diff_ignore_whitespace: "off".to_owned(),
            diff_ignore_case: false,
            diff_ignore_blank_lines: false,
        }
    }
}

impl State {
    /// The compare options as the diff takes them. An unknown whitespace
    /// word (a hand edit) means off.
    #[must_use]
    pub fn diff_options(&self) -> Normalize {
        Normalize {
            whitespace: match self.diff_ignore_whitespace.as_str() {
                "ends" => Whitespace::Ends,
                "all" => Whitespace::All,
                _ => Whitespace::Exact,
            },
            case: self.diff_ignore_case,
            blank_lines: self.diff_ignore_blank_lines,
        }
    }

    /// Remember `n` as the compare options.
    pub fn set_diff_options(&mut self, n: Normalize) {
        self.diff_ignore_whitespace = match n.whitespace {
            Whitespace::Exact => "off",
            Whitespace::Ends => "ends",
            Whitespace::All => "all",
        }
        .to_owned();
        self.diff_ignore_case = n.case;
        self.diff_ignore_blank_lines = n.blank_lines;
    }
}

fn path() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("state.toml")))
}

impl State {
    /// Read the remembered state, or the defaults.
    #[must_use]
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Remember this state. Failures are logged: losing it costs only a
    /// sidebar width.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        let text = match toml::to_string(self) {
            Ok(t) => t,
            Err(e) => {
                log::warn!("state: {e}");
                return;
            }
        };
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, text));
        if let Err(e) = written {
            log::warn!("cannot save {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_partial_files() {
        let mut s = State {
            sidebar_visible: false,
            sidebar_width: 300.0,
            ..State::default()
        };
        s.set_diff_options(Normalize {
            whitespace: Whitespace::Ends,
            case: true,
            blank_lines: false,
        });
        let text = toml::to_string(&s).unwrap();
        assert_eq!(toml::from_str::<State>(&text).unwrap(), s);
        let partial: State = toml::from_str("sidebar_width = 200.0\n").unwrap();
        assert!(partial.sidebar_visible);
        assert_eq!(partial.sidebar_width, 200.0);
        assert!(partial.diff_options().is_exact());
        let back: State = toml::from_str(&text).unwrap();
        assert_eq!(back.diff_options().whitespace, Whitespace::Ends);
        assert!(back.diff_options().case);
    }
}
