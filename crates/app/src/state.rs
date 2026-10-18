//! Window state remembered between runs: `state.toml` beside the config.
//!
//! The config file is the user's, edited by hand and full of their comments,
//! so Nimbus never writes to it. What the app remembers by itself (whether
//! the sidebar is shown, how wide it was dragged, the zoom) goes in this
//! separate file instead. A missing or broken file just means the defaults.

use std::path::PathBuf;

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
    /// The editor font size the zoom actions left, in points (M16); `None`
    /// when it is the config's `font_size`, so a later change to the config
    /// still takes effect. Read at startup unless `remember_zoom = false`.
    pub font_size: Option<f64>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            sidebar_visible: true,
            sidebar_width: DEFAULT_WIDTH,
            font_size: None,
        }
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
        let s = State {
            sidebar_visible: false,
            sidebar_width: 300.0,
            font_size: Some(18.0),
        };
        let text = toml::to_string(&s).unwrap();
        assert_eq!(toml::from_str::<State>(&text).unwrap(), s);
        let partial: State = toml::from_str("sidebar_width = 200.0\n").unwrap();
        assert!(partial.sidebar_visible);
        assert_eq!(partial.sidebar_width, 200.0);
        assert_eq!(partial.font_size, None);
    }
}
