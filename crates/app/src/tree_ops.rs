//! File operations from the sidebar's tree: New File, New Folder, Rename,
//! Delete (to the trash) and Reveal in the system's file manager.
//!
//! Each operation runs on a worker, because a rename on a network share can
//! block for seconds. The tree does not edit its own model afterwards: the
//! folder is listed again and the listing (what the disk says) is what the
//! tree shows. This is the M6 rule that the disk is the truth, applied to
//! our own changes too.

use std::path::{Component, Path, PathBuf};

/// What the inline field in the tree is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Name a new file.
    NewFile,
    /// Name a new folder.
    NewFolder,
    /// Rename the selected file or folder.
    Rename,
}

/// An operation for the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Create an empty file (and any folders on the way to it).
    NewFile(PathBuf),
    /// Create a folder (and any on the way to it).
    NewFolder(PathBuf),
    /// Move `from` to `to`; `to` may already exist when the user agreed to
    /// replace it.
    Rename {
        /// The old path.
        from: PathBuf,
        /// The new path.
        to: PathBuf,
    },
    /// Move a file or folder to the system's trash (the Recycle Bin on
    /// Windows), where it can be put back from.
    Trash(PathBuf),
    /// Copy a file or folder (and everything in it) to `to`, which must not
    /// exist yet (M19: a drag with `Alt` held).
    Copy {
        /// What is copied.
        from: PathBuf,
        /// The copy.
        to: PathBuf,
    },
    /// Put the most recently trashed item that was at this path back
    /// (Undo Move after a Move to Trash). Windows and Linux only: macOS
    /// gives no way to find a trashed item again.
    Restore(PathBuf),
}

impl Op {
    /// The path the operation leaves behind, to select in the tree.
    #[must_use]
    pub fn result_path(&self) -> &Path {
        match self {
            Self::NewFile(p)
            | Self::NewFolder(p)
            | Self::Rename { to: p, .. }
            | Self::Copy { to: p, .. }
            | Self::Trash(p)
            | Self::Restore(p) => p,
        }
    }
}

/// Where a name typed into the tree's field goes, inside `dir`. A name may
/// hold folders (`src/new.rs`), which New creates and Rename moves into;
/// it may not climb out (`..`), be absolute, or be empty.
///
/// # Errors
///
/// Returns why the name cannot be used, for the message line.
pub fn target(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name.trim().is_empty() {
        return Err("A name is needed".into());
    }
    let rel = Path::new(name);
    let mut out = dir.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => return Err("A name cannot go up a folder (..)".into()),
            Component::RootDir | Component::Prefix(_) => {
                return Err("A name cannot be a full path".into());
            }
        }
    }
    if out == dir {
        return Err("A name is needed".into());
    }
    Ok(out)
}

/// Run `op`.
///
/// # Errors
///
/// Returns the reason it failed, with the path, for the message line.
pub fn run(op: &Op) -> Result<(), String> {
    let at = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    match op {
        Op::NewFile(p) => {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
            }
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(p)
                .map(drop)
                .map_err(|e| at(p, e))
        }
        Op::NewFolder(p) => {
            if p.exists() {
                return Err(format!("{} already exists", p.display()));
            }
            std::fs::create_dir_all(p).map_err(|e| at(p, e))
        }
        Op::Rename { from, to } => {
            if let Some(dir) = to.parent() {
                std::fs::create_dir_all(dir).map_err(|e| at(dir, e))?;
            }
            std::fs::rename(from, to).map_err(|e| at(from, e))
        }
        Op::Trash(p) => trash_context()
            .delete(p)
            .map_err(|e| format!("{}: {e}", p.display())),
        Op::Copy { from, to } => {
            if to.exists() {
                return Err(format!("{} already exists", to.display()));
            }
            if to.starts_with(from) {
                return Err(format!("cannot copy {} into itself", from.display()));
            }
            copy_all(from, to).map_err(|e| at(from, e))
        }
        Op::Restore(p) => restore(p),
    }
}

/// Copy `from` to `to`: a file, or a folder and everything in it. A link
/// is copied as what it points to, as Finder and Explorer do.
fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.is_dir() {
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        return std::fs::copy(from, to).map(drop);
    }
    // A stack, not recursion, so a deep tree cannot overflow the thread.
    let mut stack = vec![(from.to_path_buf(), to.to_path_buf())];
    while let Some((src, dst)) = stack.pop() {
        std::fs::create_dir(&dst)?;
        for entry in std::fs::read_dir(&src)? {
            let entry = entry?;
            let (s, d) = (entry.path(), dst.join(entry.file_name()));
            if s.is_dir() {
                stack.push((s, d));
            } else {
                std::fs::copy(&s, &d)?;
            }
        }
    }
    Ok(())
}

/// Put back the item most recently trashed from `path`.
#[cfg(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
))]
fn restore(path: &Path) -> Result<(), String> {
    let at = |e: trash::Error| format!("{}: {e}", path.display());
    if path.exists() {
        return Err(format!("{} exists again; nothing put back", path.display()));
    }
    let item = trash::os_limited::list()
        .map_err(at)?
        .into_iter()
        .filter(|i| i.original_path() == path)
        .max_by_key(|i| i.time_deleted)
        .ok_or_else(|| format!("{} is no longer in the trash", path.display()))?;
    trash::os_limited::restore_all([item]).map_err(at)
}

/// macOS: the trash crate cannot find an item in the Trash again, so it
/// cannot be put back from here.
#[cfg(not(any(
    target_os = "windows",
    all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android")
    )
)))]
fn restore(path: &Path) -> Result<(), String> {
    Err(format!(
        "{} is in the Trash; use Put Back in Finder to restore it",
        path.display()
    ))
}

/// Whether trashed items can be put back from Nimbus on this system.
#[must_use]
pub const fn can_restore() -> bool {
    cfg!(any(
        target_os = "windows",
        all(
            unix,
            not(target_os = "macos"),
            not(target_os = "ios"),
            not(target_os = "android")
        )
    ))
}

/// A free name for a copy of `name` in `dir`: `a copy.rs`, then
/// `a copy 2.rs` and so on, as Finder names them.
#[must_use]
pub fn copy_name(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let stem = stem_range(name, is_dir);
    let (base, ext) = (&name[stem.clone()], &name[stem.end..]);
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            format!("{base} copy{ext}")
        } else {
            format!("{base} copy {n}{ext}")
        };
        let path = dir.join(candidate);
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

/// How the `trash` crate moves files to the trash. On macOS it asks
/// `NSFileManager` directly, not Finder through an Apple script (its default),
/// which would ask the user to let Nimbus control Finder first.
fn trash_context() -> trash::TrashContext {
    #[cfg_attr(
        not(target_os = "macos"),
        expect(unused_mut, reason = "set on macOS only")
    )]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
}

/// Show `path` in Finder, Explorer, or the Linux file manager, selected
/// where the platform allows. The file manager runs on its own; a thread
/// waits for the launcher so it does not linger as a zombie process.
///
/// # Errors
///
/// Returns why the file manager could not be started.
pub fn reveal(path: &Path) -> Result<(), String> {
    let mut cmd = reveal_command(path);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let _ = std::thread::Builder::new()
        .name("nimbus-reveal".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

#[cfg(target_os = "macos")]
fn reveal_command(path: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("open");
    cmd.arg("-R").arg(path);
    cmd
}

#[cfg(target_os = "windows")]
fn reveal_command(path: &Path) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    // Explorer reads its own command line: `/select,"path"` as one piece.
    let mut cmd = std::process::Command::new("explorer");
    cmd.raw_arg(format!("/select,\"{}\"", path.display()));
    cmd
}

/// Linux and the rest: no portable way to select a file, so the folder
/// holding it opens (the folder itself, for a folder).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn reveal_command(path: &Path) -> std::process::Command {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(dir);
    cmd
}

/// Where `path` is after `from` moved to `to`: itself, or under `to` if it
/// was `from` or inside it. `None` when the move did not touch it.
#[must_use]
pub fn moved(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

/// The part of a file name Rename selects at first: the name without its
/// extension, as Finder and most editors do, so typing replaces the name
/// and keeps `.rs`. Byte offsets into `name`.
#[must_use]
pub fn stem_range(name: &str, is_dir: bool) -> std::ops::Range<usize> {
    if is_dir {
        return 0..name.len();
    }
    match name.rfind('.') {
        Some(0) | None => 0..name.len(),
        Some(dot) => 0..dot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_stay_inside_their_folder() {
        let d = Path::new("/p/src");
        assert_eq!(target(d, "a.rs").unwrap(), PathBuf::from("/p/src/a.rs"));
        assert_eq!(target(d, "x/y.rs").unwrap(), PathBuf::from("/p/src/x/y.rs"));
        assert_eq!(target(d, "./a").unwrap(), PathBuf::from("/p/src/a"));
        assert!(target(d, "").is_err());
        assert!(target(d, "  ").is_err());
        assert!(target(d, ".").is_err());
        assert!(target(d, "../a").is_err());
        assert!(target(d, "/etc/passwd").is_err());
    }

    #[test]
    fn moves_carry_paths_inside_a_folder() {
        let (from, to) = (Path::new("/p/a"), Path::new("/p/b"));
        assert_eq!(
            moved(Path::new("/p/a"), from, to),
            Some(PathBuf::from("/p/b"))
        );
        assert_eq!(
            moved(Path::new("/p/a/x.rs"), from, to),
            Some(PathBuf::from("/p/b/x.rs"))
        );
        assert_eq!(
            moved(Path::new("/p/ab"), from, to),
            None,
            "only whole names"
        );
        assert_eq!(moved(Path::new("/q/a"), from, to), None);
    }

    #[test]
    fn rename_selects_the_name_without_its_extension() {
        assert_eq!(stem_range("main.rs", false), 0..4);
        assert_eq!(stem_range("archive.tar.gz", false), 0..11);
        assert_eq!(stem_range(".gitignore", false), 0..10);
        assert_eq!(stem_range("Makefile", false), 0..8);
        assert_eq!(stem_range("v1.2", true), 0..4);
    }

    #[test]
    fn operations_on_disk() {
        let dir = std::env::temp_dir().join(format!("nimbus-treeops-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("sub/new.txt");
        run(&Op::NewFile(f.clone())).unwrap();
        assert!(f.is_file());
        assert!(run(&Op::NewFile(f.clone())).is_err(), "never overwrites");
        let d = dir.join("folder");
        run(&Op::NewFolder(d.clone())).unwrap();
        assert!(run(&Op::NewFolder(d.clone())).is_err());
        let to = dir.join("folder/inner/renamed.txt");
        run(&Op::Rename {
            from: f.clone(),
            to: to.clone(),
        })
        .unwrap();
        assert!(to.is_file() && !f.exists());
        // Copying a folder copies what is inside; never over something.
        let copy = dir.join("copied");
        run(&Op::Copy {
            from: d.clone(),
            to: copy.clone(),
        })
        .unwrap();
        assert!(copy.join("inner/renamed.txt").is_file() && to.is_file());
        assert!(
            run(&Op::Copy {
                from: d.clone(),
                to: copy.clone(),
            })
            .is_err()
        );
        assert!(
            run(&Op::Copy {
                from: d.clone(),
                to: d.join("inside"),
            })
            .is_err()
        );
        assert_eq!(
            copy_name(&d.join("inner"), "renamed.txt", false),
            d.join("inner/renamed copy.txt")
        );
        std::fs::write(d.join("inner/renamed copy.txt"), "").unwrap();
        assert_eq!(
            copy_name(&d.join("inner"), "renamed.txt", false),
            d.join("inner/renamed copy 2.txt")
        );
        assert_eq!(copy_name(&dir, "folder", true), dir.join("folder copy"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
