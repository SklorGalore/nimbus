//! Workspace files (M19): a set of root folders and the session that goes
//! with them, saved under a name so it can be opened again.
//!
//! A workspace file lives in one of two places:
//!
//! - **Named**, in `workspaces/NAME.toml` in the config folder: "Save
//!   Workspace As…" writes one, "Open Workspace…" lists them, and
//!   `nimbus --workspace NAME` opens one. Paths in it are absolute.
//! - **Project-local**, as `.nimbus/workspace.toml` in the first root:
//!   "Save Workspace Here" writes it so the workspace travels with the
//!   repository. Paths under that root are written relative to it, so a
//!   clone somewhere else opens the same files. This is the one file
//!   Nimbus ever writes inside a project, and only on that action.
//!
//! The file holds the roots and the session fields from M12 (sidebar mode,
//! expanded folders, tabs), and nothing else: no settings, which stay in
//! the one `config.toml`.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::{Session, SessionTab, SidebarMode};

/// The folder inside a root that holds a project-local workspace file.
pub const LOCAL_DIR: &str = ".nimbus";

/// The project-local workspace file's name, inside [`LOCAL_DIR`].
pub const LOCAL_FILE: &str = "workspace.toml";

/// Why a workspace file could not be read or written, for the message
/// line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceError(pub String);

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WorkspaceError {}

/// The file's contents, as TOML spells them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    /// Root folders, the first naming the session.
    roots: Vec<PathBuf>,
    /// What the sidebar showed.
    mode: SidebarMode,
    /// Folders open in the tree.
    expanded: Vec<PathBuf>,
    /// The tabs, in order.
    tabs: Vec<SessionTab>,
}

/// Where named workspaces live: `workspaces/` beside the config file.
#[must_use]
pub fn dir() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("workspaces")))
}

/// The names of the saved workspaces, sorted.
#[must_use]
pub fn names() -> Vec<String> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    names_in(&dir)
}

/// The workspace names in `dir`: its `.toml` files without the extension.
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let p = e.path();
            if p.extension()? != "toml" {
                return None;
            }
            Some(p.file_stem()?.to_str()?.to_owned())
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// The file for workspace `name`. A name is a plain file name: no folders,
/// nothing hidden, nothing empty.
///
/// # Errors
///
/// Returns why the name cannot be used, or that there is no config folder.
pub fn named(name: &str) -> Result<PathBuf, WorkspaceError> {
    let name = name.trim();
    let mut parts = Path::new(name).components();
    let plain = matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    );
    if name.is_empty() || !plain || name.starts_with('.') {
        return Err(WorkspaceError(format!(
            "{name:?} cannot be a workspace name: use a plain name, no slashes"
        )));
    }
    let dir = dir().ok_or_else(|| WorkspaceError("no config folder to save in".into()))?;
    Ok(dir.join(format!("{name}.toml")))
}

/// The project-local workspace file of `root`.
#[must_use]
pub fn local(root: &Path) -> PathBuf {
    root.join(LOCAL_DIR).join(LOCAL_FILE)
}

/// `path` relative to `base` when it is inside it (`.` for `base`
/// itself), else unchanged. Relative paths are written with `/`, so a
/// file committed on Windows reads the same on a Mac.
fn relative(path: &Path, base: Option<&Path>) -> PathBuf {
    match base.and_then(|b| path.strip_prefix(b).ok()) {
        Some(rel) if rel.as_os_str().is_empty() => PathBuf::from("."),
        Some(rel) => PathBuf::from(
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        ),
        None => path.to_path_buf(),
    }
}

/// `path` as read from a file whose relative paths start at `base`.
/// Absolute paths stay as they are; `.` is the base.
fn absolute(path: &Path, base: Option<&Path>) -> PathBuf {
    match base {
        // `has_root`, not `is_relative`: `/w/x` has no drive on Windows,
        // so it counts as relative there, yet it is not one of ours.
        Some(b) if !path.has_root() => {
            let joined = b.join(path);
            // `b/.` reads oddly in messages; drop the `.`.
            joined
                .components()
                .filter(|c| *c != Component::CurDir)
                .collect()
        }
        _ => path.to_path_buf(),
    }
}

/// The text of a workspace file for `session`. With `base` (a
/// project-local file), paths inside it are written relative to it.
///
/// # Errors
///
/// Returns why the session could not be written as TOML.
pub fn to_text(session: &Session, base: Option<&Path>) -> Result<String, WorkspaceError> {
    let root = &session.root;
    let file = File {
        roots: std::iter::once(root)
            .chain(&session.roots)
            .map(|p| relative(p, base))
            .collect(),
        mode: session.mode,
        // Kept relative to the first root, as the session holds them;
        // ones in another root are absolute there and made relative here.
        expanded: session
            .expanded
            .iter()
            .map(|p| relative(&root.join(p), base))
            .collect(),
        tabs: session
            .tabs
            .iter()
            .map(|t| SessionTab {
                path: relative(&t.path, base),
                ..t.clone()
            })
            .collect(),
    };
    let head = "# A Nimbus workspace: its folders and the files it had open.\n";
    toml::to_string(&file)
        .map(|t| format!("{head}{t}"))
        .map_err(|e| WorkspaceError(e.to_string()))
}

/// Read a workspace file's text into a session. With `base` (a
/// project-local file), relative paths start there.
///
/// # Errors
///
/// Returns why the text is not a workspace file.
pub fn from_text(text: &str, base: Option<&Path>) -> Result<Session, WorkspaceError> {
    let file: File = toml::from_str(text).map_err(|e| WorkspaceError(e.message().to_owned()))?;
    let mut roots = file.roots.iter().map(|p| absolute(p, base));
    let root = roots
        .next()
        .ok_or_else(|| WorkspaceError("the workspace lists no folders".into()))?;
    let roots: Vec<PathBuf> = roots.collect();
    let expanded = file
        .expanded
        .iter()
        .map(|p| {
            let abs = absolute(p, base.or(Some(&root)));
            abs.strip_prefix(&root)
                .map_or_else(|_| abs.clone(), Path::to_path_buf)
        })
        .collect();
    let tabs = file
        .tabs
        .into_iter()
        .map(|t| SessionTab {
            path: absolute(&t.path, base.or(Some(&root))),
            ..t
        })
        .collect();
    Ok(Session {
        root,
        roots,
        mode: file.mode,
        expanded,
        tabs,
        ..Session::default()
    })
}

/// Write `session` to `path`; `base` as for [`to_text`]. Written beside
/// itself and renamed into place, like the state file.
///
/// # Errors
///
/// Returns why it could not be written.
pub fn save(path: &Path, session: &Session, base: Option<&Path>) -> Result<(), WorkspaceError> {
    let text = to_text(session, base)?;
    crate::state::write_atomic(path, text.as_bytes())
        .map_err(|e| WorkspaceError(format!("{}: {e}", path.display())))
}

/// Read the workspace file at `path`; `base` as for [`from_text`]. Roots
/// are made canonical, so they match the paths the tree and sessions use.
///
/// # Errors
///
/// Returns why it could not be read.
pub fn load(path: &Path, base: Option<&Path>) -> Result<Session, WorkspaceError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| WorkspaceError(format!("{}: {e}", path.display())))?;
    let mut session =
        from_text(&text, base).map_err(|e| WorkspaceError(format!("{}: {e}", path.display())))?;
    session.root = crate::io::canonical(&session.root);
    for r in &mut session.roots {
        *r = crate::io::canonical(r);
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(path: &str, active: bool) -> SessionTab {
        SessionTab {
            path: PathBuf::from(path),
            cursor: 4,
            scroll: 2,
            active,
        }
    }

    fn session() -> Session {
        Session {
            root: PathBuf::from("/w/app"),
            roots: vec![PathBuf::from("/w/lib")],
            mode: SidebarMode::Search,
            expanded: vec![PathBuf::from("src"), PathBuf::from("/w/lib/core")],
            tabs: vec![tab("/w/app/src/main.rs", false), tab("/w/lib/x.rs", true)],
            ..Session::default()
        }
    }

    #[test]
    fn named_files_keep_absolute_paths() {
        let s = session();
        let text = to_text(&s, None).unwrap();
        assert!(text.contains("\"/w/lib/x.rs\""));
        assert_eq!(from_text(&text, None).unwrap(), s);
    }

    #[test]
    fn local_files_are_relative_to_their_root() {
        let s = session();
        let base = Path::new("/w/app");
        let text = to_text(&s, Some(base)).unwrap();
        assert!(text.contains("roots = [\".\", \"/w/lib\"]"), "{text}");
        assert!(text.contains("\"src/main.rs\""));
        // Moved: the same workspace opens at the new place.
        let moved = from_text(&text, Some(Path::new("/elsewhere/app"))).unwrap();
        assert_eq!(moved.root, PathBuf::from("/elsewhere/app"));
        assert_eq!(moved.roots, vec![PathBuf::from("/w/lib")]);
        assert_eq!(
            moved.tabs[0].path,
            PathBuf::from("/elsewhere/app/src/main.rs")
        );
        assert_eq!(
            moved.expanded,
            vec![PathBuf::from("src"), PathBuf::from("/w/lib/core")]
        );
        assert_eq!(from_text(&text, Some(base)).unwrap(), s);
    }

    #[test]
    fn bad_files_and_names_are_errors() {
        assert!(from_text("roots = []\n", None).is_err());
        assert!(from_text("roots = 3\n", None).is_err());
        let partial = from_text("roots = [\"/a\"]\n", None).unwrap();
        assert_eq!(partial.root, PathBuf::from("/a"));
        assert_eq!(partial.tabs, Vec::new());
        for bad in ["", "  ", "a/b", "../x", ".hidden", "/abs"] {
            assert!(named(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn lists_and_round_trips_on_disk() {
        let dir = std::env::temp_dir().join(format!("nimbus-workspaces-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = session();
        save(&dir.join("Zed.toml"), &s, None).unwrap();
        save(&dir.join("alpha.toml"), &s, None).unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        assert_eq!(names_in(&dir), ["alpha", "Zed"]);
        let back = load(&dir.join("alpha.toml"), None).unwrap();
        assert_eq!(back.tabs, s.tabs);
        assert!(load(&dir.join("none.toml"), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
