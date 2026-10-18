//! Sessions: which files a project folder had open, so they come back.
//!
//! The rules for the command line (`DESIGN.md` §9, M12):
//!
//! - `nimbus FOLDER` reopens that folder's session.
//! - `nimbus` with no arguments reopens the last session.
//! - `nimbus FILE…` opens just those files, and leaves sessions alone: a
//!   quick edit (or git asking for a commit message) is not a workspace.
//! - `--no-session` neither restores nor saves one, for a clean start that
//!   does not overwrite what was there.
//!
//! Restoring opens only the tabs in front, one per editor group, before
//! the first frame; the others start as [`Document::placeholder`]s and are
//! read on a worker after the first frame, or when shown, whichever comes
//! first.
//!
//! Since M13 a session also keeps the editor groups (each tab's group and
//! each group's width and front tab) and each tab's folds. A file open in
//! two groups is listed once per group, and comes back as one document
//! shown in both.

use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::document::Document;
use crate::groups::MAX_GROUPS;
use crate::io;
use crate::state::{Session, SessionTab, State};

/// What `main` hands the app to restore.
#[derive(Debug, Default)]
pub struct Restore {
    /// The tabs, in order.
    pub tabs: Vec<RestoredTab>,
    /// Which tab is in front.
    pub active: usize,
    /// The sidebar's mode.
    pub mode: Option<crate::sidebar::Mode>,
    /// Folders to open in the tree (absolute).
    pub expanded: Vec<PathBuf>,
    /// Files of the session that are gone, for a message-line note.
    pub dropped: Vec<PathBuf>,
    /// Further root folders of the workspace (M19) that are still there.
    pub roots: Vec<PathBuf>,
    /// The Search panel's history and saved searches.
    pub search: crate::state::SearchHistory,
    /// Each editor group's share of the width, left to right; the groups
    /// are numbered with no gaps, and there is at least one.
    pub shares: Vec<f32>,
}

/// One restored tab.
#[derive(Debug)]
pub struct RestoredTab {
    /// The document: read already for the front tab, a placeholder for
    /// the rest.
    pub doc: Document,
    /// Its file's canonical path.
    pub canonical: PathBuf,
    /// Where the cursor and the view's top were, as byte offsets.
    pub cursor: usize,
    /// The byte the view's top row starts in.
    pub scroll: usize,
    /// The editor group it goes in.
    pub group: usize,
    /// It is its group's active tab.
    pub front: bool,
    /// Its folds, to make once the file is read if it has not changed
    /// since ([`RestoredTab::disk`]).
    pub folds: Vec<Range<usize>>,
    /// The file's size and time when the folds were saved.
    pub disk: Option<[u64; 2]>,
}

/// How this run treats sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Files were named: open them, leave sessions alone.
    Files,
    /// A project folder (canonical), whose session is restored and saved.
    Folder(PathBuf),
}

/// Decide how this run treats sessions. `folder` is the first folder
/// named on the command line; `files` is whether any file was named (or
/// `--diff`, `--merge`).
#[must_use]
pub fn launch(state: &State, folder: Option<&Path>, files: bool, no_session: bool) -> Launch {
    if files || no_session {
        return Launch::Files;
    }
    if let Some(f) = folder {
        return Launch::Folder(io::canonical(f));
    }
    match state.last_session() {
        Some(s) if s.root.is_dir() => Launch::Folder(s.root.clone()),
        // No session yet, or its folder is gone: start in the current
        // folder, as before sessions, and remember that one.
        _ => std::env::current_dir().map_or(Launch::Files, |d| Launch::Folder(io::canonical(&d))),
    }
}

/// The folders of `paths` that are still there.
fn existing_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().filter(|p| p.is_dir()).cloned().collect()
}

/// Plan the restore of `session`: drop files that are gone, read the
/// front tab of each editor group, and make placeholders of the rest.
/// `open` reads a file (the real [`Document::open`], or a stand-in in
/// tests); a front file that cannot be read is dropped and the next tab of
/// its group is tried. A group left with no tabs goes, and the groups
/// right of it move left.
pub fn restore(
    session: &Session,
    mut open: impl FnMut(&Path) -> Result<Document, String>,
) -> (Restore, Vec<String>) {
    let mut out = Restore {
        mode: Some(session.mode.into()),
        expanded: session
            .expanded
            .iter()
            .map(|p| session.root.join(p))
            .collect(),
        roots: existing_dirs(&session.roots),
        search: session.search.clone(),
        ..Restore::default()
    };
    let mut warnings = Vec::new();
    // A group number past the saved widths (or past the most groups) is
    // read as the first group.
    let groups = session.groups.len().clamp(1, MAX_GROUPS);
    let mut active = None;
    for t in &session.tabs {
        let group = if t.group < groups { t.group } else { 0 };
        if out
            .tabs
            .iter()
            .any(|r| r.canonical == t.path && r.group == group)
        {
            continue; // the same file twice in one group
        }
        if !t.path.is_file() {
            if !out.dropped.contains(&t.path) {
                out.dropped.push(t.path.clone());
            }
            continue;
        }
        if t.active {
            active = Some(out.tabs.len());
        }
        let mut tab = placeholder(t);
        tab.group = group;
        out.tabs.push(tab);
    }
    let focused = active.map_or(0, |a| out.tabs[a].group);
    // Each group's front tab: the one marked, else the focused group's
    // active tab, else its first.
    for g in 0..groups {
        let marked = out
            .tabs
            .iter()
            .position(|r| r.group == g && r.front)
            .or(active.filter(|_| g == focused))
            .or_else(|| out.tabs.iter().position(|r| r.group == g));
        for (i, r) in out.tabs.iter_mut().enumerate() {
            if r.group == g {
                r.front = Some(i) == marked;
            }
        }
    }
    // Read the front tabs now; one that cannot be read gives its place to
    // the next tab of its group.
    for g in 0..groups {
        while let Some(at) = out.tabs.iter().position(|r| r.group == g && r.front) {
            let path = out.tabs[at].canonical.clone();
            if out
                .tabs
                .iter()
                .any(|r| r.canonical == path && !r.doc.is_placeholder())
            {
                break; // read already for another group: they share it
            }
            match open(&path) {
                Ok(doc) => {
                    out.tabs[at].doc = doc;
                    break;
                }
                Err(e) => {
                    warnings.push(format!("Cannot open {}: {e}", path.display()));
                    out.tabs.remove(at);
                    if let Some(next) = out.tabs[at..].iter_mut().find(|r| r.group == g) {
                        next.front = true;
                    } else if let Some(prev) =
                        out.tabs[..at].iter_mut().rev().find(|r| r.group == g)
                    {
                        prev.front = true;
                    }
                }
            }
        }
    }
    // Number the groups that still have tabs with no gaps.
    let shares = |g: usize| session.groups.get(g).copied().unwrap_or(1.0);
    let used: Vec<usize> = (0..groups)
        .filter(|&g| out.tabs.iter().any(|r| r.group == g))
        .collect();
    for r in &mut out.tabs {
        r.group = used.iter().position(|&g| g == r.group).unwrap_or(0);
    }
    out.shares = used.iter().map(|&g| shares(g)).collect();
    if out.shares.is_empty() {
        out.shares.push(1.0);
    }
    let focused = used.iter().position(|&g| g == focused).unwrap_or(0);
    out.active = out
        .tabs
        .iter()
        .position(|r| r.group == focused && r.front)
        .unwrap_or(0);
    (out, warnings)
}

fn placeholder(t: &SessionTab) -> RestoredTab {
    RestoredTab {
        doc: Document::placeholder(t.path.clone()),
        canonical: t.path.clone(),
        cursor: t.cursor,
        scroll: t.scroll,
        group: t.group,
        front: t.front,
        folds: t.folds.iter().map(|&[a, b]| a..b).collect(),
        disk: t.disk,
    }
}

/// The note for files a restored session no longer finds.
#[must_use]
pub fn dropped_note(dropped: &[PathBuf]) -> Option<String> {
    let name = |p: &PathBuf| {
        p.file_name().map_or_else(
            || p.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    match dropped {
        [] => None,
        [one] => Some(format!(
            "{} no longer exists; left out of the session",
            name(one)
        )),
        [first, rest @ ..] => Some(format!(
            "{} and {} other file(s) no longer exist; left out of the session",
            name(first),
            rest.len()
        )),
    }
}

/// Move a restored cursor or scroll offset onto the text as it is now:
/// clamped to its length and back to the start of a character, so a file
/// that changed since puts it somewhere nearby, never inside a character.
#[must_use]
pub fn clamp_offset(t: &dyn nimbus_core::Text, at: usize) -> usize {
    let mut at = at.min(t.len_bytes());
    while at > 0 && !t.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SidebarMode;
    use nimbus_core::{RopeBuffer, Text};

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let d =
                std::env::temp_dir().join(format!("nimbus-session-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Self(io::canonical(&d))
        }
        fn file(&self, name: &str, text: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, text).unwrap();
            p
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tab(path: &Path, active: bool) -> SessionTab {
        SessionTab {
            path: path.to_path_buf(),
            cursor: 3,
            scroll: 0,
            active,
            ..SessionTab::default()
        }
    }

    #[test]
    fn restore_reads_only_the_front_tab_and_drops_missing_files() {
        let d = Dir::new("restore");
        let a = d.file("a.txt", "aaa");
        let b = d.file("b.txt", "bbb");
        let gone = d.0.join("gone.txt");
        let session = Session {
            root: d.0.clone(),
            mode: SidebarMode::Search,
            expanded: vec![PathBuf::from("src")],
            tabs: vec![
                tab(&a, false),
                tab(&gone, false),
                tab(&b, true),
                tab(&a, false),
            ],
            ..Session::default()
        };
        let mut opened = Vec::new();
        let (r, warnings) = restore(&session, |p| {
            opened.push(p.to_path_buf());
            Document::open(p).map_err(|e| e.to_string())
        });
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(opened, vec![b.clone()], "only the front tab is read");
        assert_eq!(r.tabs.len(), 2, "the gone file and the duplicate left out");
        assert_eq!(r.active, 1);
        assert!(r.tabs[0].doc.is_placeholder() && !r.tabs[1].doc.is_placeholder());
        assert_eq!(r.dropped, vec![gone]);
        assert_eq!(r.expanded, vec![d.0.join("src")]);
        assert_eq!(r.mode, Some(crate::sidebar::Mode::Search));
        assert!(dropped_note(&r.dropped).unwrap().contains("gone.txt"));
    }

    #[test]
    fn groups_come_back_with_a_front_tab_read_in_each() {
        let dir = Dir::new("groups");
        let a = dir.file("a.txt", "aaa");
        let b = dir.file("b.txt", "bbb");
        let c = dir.file("c.txt", "ccc");
        let gone = dir.0.join("gone.txt");
        let in_group = |p: &Path, group, front, active| SessionTab {
            group,
            front,
            ..tab(p, active)
        };
        let session = Session {
            root: dir.0.clone(),
            mode: SidebarMode::Files,
            expanded: Vec::new(),
            tabs: vec![
                in_group(&a, 0, false, false),
                in_group(&b, 0, true, false),
                // Group 1 had only a file that is gone: it goes.
                in_group(&gone, 1, true, false),
                // The same file in two groups; group 2 has the focus.
                in_group(&b, 2, false, false),
                in_group(&c, 2, true, true),
            ],
            groups: vec![0.5, 0.2, 0.3],
            ..Session::default()
        };
        let mut opened = Vec::new();
        let (r, warnings) = restore(&session, |p| {
            opened.push(p.to_path_buf());
            Document::open(p).map_err(|e| e.to_string())
        });
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(opened, vec![b.clone(), c.clone()], "one front per group");
        assert_eq!(r.shares, vec![0.5, 0.3]);
        let groups: Vec<usize> = r.tabs.iter().map(|t| t.group).collect();
        assert_eq!(groups, [0, 0, 1, 1]);
        let fronts: Vec<bool> = r.tabs.iter().map(|t| t.front).collect();
        assert_eq!(fronts, [false, true, false, true]);
        assert_eq!(r.active, 3);
        assert_eq!(r.dropped, vec![gone]);
    }

    #[test]
    fn an_unreadable_front_tab_falls_back_to_the_next() {
        let d = Dir::new("fallback");
        let a = d.file("a.txt", "aaa");
        let b = d.file("b.txt", "bbb");
        let session = Session {
            root: d.0.clone(),
            mode: SidebarMode::Files,
            expanded: Vec::new(),
            tabs: vec![tab(&a, true), tab(&b, false)],
            ..Session::default()
        };
        let (r, warnings) = restore(&session, |p| {
            if p.ends_with("a.txt") {
                Err("denied".into())
            } else {
                Document::open(p).map_err(|e| e.to_string())
            }
        });
        assert_eq!(warnings.len(), 1);
        assert_eq!(r.tabs.len(), 1);
        assert_eq!(r.active, 0);
        assert!(!r.tabs[0].doc.is_placeholder());
    }

    #[test]
    fn command_line_rules() {
        let d = Dir::new("launch");
        let mut state = State::default();
        assert_eq!(launch(&state, Some(&d.0), true, false), Launch::Files);
        assert_eq!(launch(&state, Some(&d.0), false, true), Launch::Files);
        assert_eq!(
            launch(&state, Some(&d.0), false, false),
            Launch::Folder(d.0.clone())
        );
        state.put_session(Session {
            root: d.0.clone(),
            mode: SidebarMode::Files,
            expanded: Vec::new(),
            tabs: Vec::new(),
            ..Session::default()
        });
        assert_eq!(
            launch(&state, None, false, false),
            Launch::Folder(d.0.clone())
        );
    }

    #[test]
    fn offsets_are_clamped_to_a_character() {
        let t = RopeBuffer::from_text("aé");
        assert_eq!(clamp_offset(&t, 2), 1, "inside é goes back to its start");
        assert_eq!(clamp_offset(&t, 99), t.len_bytes());
        assert_eq!(clamp_offset(&t, 0), 0);
    }
}
