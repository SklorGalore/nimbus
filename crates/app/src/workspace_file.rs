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
//! The file holds the roots and the session fields from M12 and M13
//! (sidebar mode, expanded folders, editor groups, tabs), and nothing else: no settings, which stay in
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
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    /// Root folders, the first naming the session.
    roots: Vec<PathBuf>,
    /// What the sidebar showed.
    mode: SidebarMode,
    /// Folders open in the tree.
    expanded: Vec<PathBuf>,
    /// Each editor group's share of the width (M13); empty for one group.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    groups: Vec<f32>,
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
    // Slashes are checked as text too: `Path` drops a trailing one, so
    // `a/` looks like the plain name `a` yet `a/.toml` is a file in a
    // folder `a` (found by the `workspace` fuzz target, M26).
    if name.is_empty() || !plain || name.starts_with('.') || name.contains(['/', '\\']) {
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
        groups: session.groups.clone(),
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
        groups: file.groups,
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

/// Read a project's own `.nimbus/workspace.toml` at `path`, whose
/// relative paths start at `root`, keeping only what is inside `root`
/// (M26). Returns the session and how many paths were dropped.
///
/// A project-local file arrives with whatever the user cloned or
/// unpacked, so it is untrusted: without this, a repository could list
/// `~/.ssh/id_ed25519` as a tab, or `/` as a root, and opening its
/// workspace would show that file or list that folder. Paths are
/// resolved first (symlinks and `..` included), so `../../.ssh` and a
/// symlink pointing out of the project are dropped too. A workspace with
/// roots outside its folder is a named workspace's job: those keep
/// absolute paths and live in the user's own config folder.
///
/// # Errors
///
/// The errors of [`load`], and an error when the first root itself is
/// outside `root`.
pub fn load_local(path: &Path, root: &Path) -> Result<(Session, usize), WorkspaceError> {
    let mut session = load(path, Some(root))?;
    let dropped = confine(&mut session, &crate::io::canonical(root));
    if !session.root.starts_with(crate::io::canonical(root)) {
        return Err(WorkspaceError(format!(
            "{}: its first folder is outside the project",
            path.display()
        )));
    }
    Ok((session, dropped))
}

/// Drop every root, expanded folder and tab of `session` that is not
/// inside `base` (canonical), once resolved. Returns how many went.
fn confine(session: &mut Session, base: &Path) -> usize {
    let inside = |p: &Path| crate::io::canonical(&lexical(p)).starts_with(base);
    let mut dropped = 0;
    let before = session.roots.len();
    session.roots.retain(|r| inside(r));
    dropped += before - session.roots.len();
    let root = session.root.clone();
    let before = session.expanded.len();
    session.expanded.retain(|e| inside(&root.join(e)));
    dropped += before - session.expanded.len();
    let before = session.tabs.len();
    session.tabs.retain(|t| inside(&t.path));
    dropped += before - session.tabs.len();
    dropped
}

/// `path` with `.` and `..` worked out on the text alone, for a path that
/// may not exist (so cannot be canonicalised): `a/b/../c` is `a/c`.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
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
            ..SessionTab::default()
        }
    }

    fn session() -> Session {
        Session {
            root: PathBuf::from("/w/app"),
            roots: vec![PathBuf::from("/w/lib")],
            mode: SidebarMode::Search,
            expanded: vec![PathBuf::from("src"), PathBuf::from("/w/lib/core")],
            tabs: vec![tab("/w/app/src/main.rs", false), tab("/w/lib/x.rs", true)],
            groups: vec![0.5, 0.5],
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
        for bad in ["", "  ", "a/b", "../x", ".hidden", "/abs", "+/", "a\\b"] {
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

    #[test]
    fn a_projects_own_file_opens_only_what_is_inside_it() {
        let dir = std::env::temp_dir().join(format!("nimbus-local-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        std::fs::write(project.join("src/main.rs"), "").unwrap();
        std::fs::write(dir.join("secret/key"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("secret/key"), project.join("link")).unwrap();
        // TOML literal strings ('...') need no escaping of Windows paths.
        let text = format!(
            "roots = ['.', '{secret}', 'src/..']\nexpanded = ['src', '../secret']\n\
             [[tabs]]\npath = 'src/main.rs'\n[[tabs]]\npath = '../secret/key'\n\
             [[tabs]]\npath = '{key}'\n[[tabs]]\npath = 'link'\n",
            secret = dir.join("secret").display(),
            key = dir.join("secret").join("key").display(),
        );
        let file = local(&project);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        let (session, dropped) = load_local(&file, &project).unwrap();
        let canonical = crate::io::canonical(&project);
        assert_eq!(session.root, canonical);
        assert_eq!(session.roots, vec![canonical]);
        assert_eq!(session.expanded, vec![PathBuf::from("src")]);
        let tabs: Vec<_> = session.tabs.iter().map(|t| t.path.clone()).collect();
        assert_eq!(tabs, vec![project.join("src").join("main.rs")]);
        // The outside root, folder and two tabs, and on Unix the symlink.
        let symlink = usize::from(cfg!(unix));
        assert_eq!(dropped, 1 + 1 + 2 + symlink, "{tabs:?}");

        // A file whose first folder is elsewhere is refused outright.
        let elsewhere = format!("roots = ['{}']\n", dir.join("secret").display());
        std::fs::write(&file, elsewhere).unwrap();
        assert!(load_local(&file, &project).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
