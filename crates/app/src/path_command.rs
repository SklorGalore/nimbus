//! Install `nimbus` Command in PATH, and Uninstall (M25).
//!
//! A terminal finds a command by looking through the folders listed in the
//! `PATH` environment variable, in order, for a file of that name. Nimbus's
//! program lives inside the app (`Nimbus.app/Contents/MacOS/nimbus`) or an
//! install folder that is not on `PATH`, so these actions put it there:
//!
//! - **macOS**: a *symbolic link* `/usr/local/bin/nimbus` pointing at the
//!   program inside the app. A link, never a copy: a copy goes stale with
//!   the next update, a link follows the app. `/usr/local/bin` usually
//!   needs an administrator, so the link is then made through `osascript`
//!   `with administrator privileges`, which shows the system's password
//!   prompt (VS Code does the same). Nimbus never sees the password.
//! - **Linux**: a link `~/.local/bin/nimbus`; no password. An `AppImage`
//!   runs from a temporary mount, so the link points at the `.AppImage`
//!   file itself (`$APPIMAGE`).
//! - **Windows**: the folder holding `nimbus.exe` is added to the user's
//!   own `PATH` (in the registry under `HKEY_CURRENT_USER\Environment`);
//!   terminals opened afterwards find it. It is done by a small PowerShell
//!   script, which also tells Windows the environment changed, so no new
//!   crate and no `unsafe` are needed.
//!
//! Something already at the link's place that is not Nimbus is never
//! replaced without asking. Checking what is there ([`plan_install`]) is
//! quick and runs on the event loop, where a question can be asked; the
//! change itself ([`run`]) may wait on a password prompt or PowerShell, so
//! it runs on a worker.

use std::path::{Path, PathBuf};

/// What an install or uninstall comes to, before anything is changed.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Nothing to change; say this.
    Nothing(String),
    /// It cannot be done; say why.
    Refused(String),
    /// Something else is in the way; ask this before doing the step.
    #[cfg_attr(windows, expect(dead_code, reason = "Windows edits PATH instead"))]
    Ask(String, Step),
    /// Do this on a worker.
    Run(Step),
}

/// A change to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Make `link` point at `target`, replacing what is there when
    /// `replace` (only after the user agreed).
    #[cfg_attr(windows, expect(dead_code, reason = "Windows edits PATH instead"))]
    Link {
        /// Where the command goes.
        link: PathBuf,
        /// The program it runs.
        target: PathBuf,
        /// Remove what is at `link` first.
        replace: bool,
    },
    /// Remove the link at this path, which points at Nimbus.
    #[cfg_attr(windows, expect(dead_code, reason = "Windows edits PATH instead"))]
    Unlink(PathBuf),
    /// Add the folder to (`true`) or remove it from (`false`) the user's
    /// `PATH` in the registry (Windows).
    #[cfg_attr(not(windows), expect(dead_code, reason = "only Windows edits PATH"))]
    UserPath(PathBuf, bool),
}

/// What Install `nimbus` Command in PATH would do here.
#[must_use]
pub fn plan_install() -> Plan {
    match target() {
        Ok(target) => install_plan(&target, &search_dirs()),
        Err(why) => Plan::Refused(why),
    }
}

/// What Uninstall `nimbus` Command would do here.
#[must_use]
pub fn plan_uninstall() -> Plan {
    match target() {
        Ok(target) => uninstall_plan(&target),
        Err(why) => Plan::Refused(why),
    }
}

#[cfg(windows)]
fn install_plan(target: &Path, dirs: &[PathBuf]) -> Plan {
    if let Some(found) = on_path(target, dirs) {
        return Plan::Nothing(format!(
            "The nimbus command is already on your PATH: {}",
            found.display()
        ));
    }
    match target.parent() {
        Some(dir) => Plan::Run(Step::UserPath(dir.to_path_buf(), true)),
        None => Plan::Refused("Cannot tell which folder Nimbus is in".to_owned()),
    }
}

#[cfg(windows)]
fn uninstall_plan(target: &Path) -> Plan {
    match target.parent() {
        Some(dir) => Plan::Run(Step::UserPath(dir.to_path_buf(), false)),
        None => Plan::Refused("Cannot tell which folder Nimbus is in".to_owned()),
    }
}

#[cfg(not(windows))]
fn install_plan(target: &Path, dirs: &[PathBuf]) -> Plan {
    match link_path() {
        Some(link) => plan_link(&link, target, dirs),
        None => Plan::Refused("Cannot find your home folder".to_owned()),
    }
}

#[cfg(not(windows))]
fn uninstall_plan(target: &Path) -> Plan {
    match link_path() {
        Some(link) => plan_unlink(&link, target),
        None => Plan::Refused("Cannot find your home folder".to_owned()),
    }
}

/// The program the command should run: this one, or on Linux the
/// `AppImage` it was started from.
fn target() -> Result<PathBuf, String> {
    #[cfg(target_os = "linux")]
    if let Some(image) = std::env::var_os("APPIMAGE").filter(|a| !a.is_empty()) {
        return Ok(PathBuf::from(image));
    }
    let exe = std::env::current_exe().map_err(|e| format!("Cannot find Nimbus's program: {e}"))?;
    // On Windows the canonical form starts with `\\?\`, which does not
    // belong in PATH; there the folder is used as it is.
    #[cfg(not(windows))]
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if exe.to_string_lossy().contains("/AppTranslocation/") {
        // Gatekeeper runs a downloaded app from a random read-only copy
        // until it is moved; a link to that copy would break.
        return Err(
            "macOS is running Nimbus from a temporary copy: move Nimbus.app into Applications, open it from there, and try again"
                .to_owned(),
        );
    }
    Ok(exe)
}

/// Where the link goes on macOS and Linux.
#[cfg(not(windows))]
fn link_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(PathBuf::from("/usr/local/bin/nimbus"))
    } else {
        Some(dirs::home_dir()?.join(".local/bin/nimbus"))
    }
}

/// The folders a terminal may search: `PATH` as Nimbus got it, plus the
/// usual places an app started from the Dock or a menu does not see in
/// its `PATH` (Homebrew's folders, `~/.local/bin`).
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if !cfg!(windows) {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
        if let Some(home) = dirs::home_dir() {
            dirs.push(home.join(".local/bin"));
        }
    }
    dirs
}

/// The first `nimbus` in `dirs` that is this program, if any.
fn on_path(target: &Path, dirs: &[PathBuf]) -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "nimbus.exe"
    } else {
        "nimbus"
    };
    let want = std::fs::canonicalize(target).ok()?;
    dirs.iter()
        .map(|d| d.join(name))
        .find(|c| std::fs::canonicalize(c).is_ok_and(|c| c == want))
}

/// Whether `link` leads to `target`, through any number of links.
#[cfg(not(windows))]
fn reaches(link: &Path, target: &Path) -> bool {
    match (std::fs::canonicalize(link), std::fs::canonicalize(target)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Whether the link at `link` is one of Nimbus's: it reaches `target`, or
/// it points at a Nimbus app that was moved or deleted since.
#[cfg(not(windows))]
fn is_ours(link: &Path, target: &Path) -> bool {
    reaches(link, target)
        || std::fs::read_link(link).is_ok_and(|to| {
            to == target || (to.ends_with("Nimbus.app/Contents/MacOS/nimbus") && !to.exists())
        })
}

/// What is at `path`, in words, for a message.
#[cfg(not(windows))]
fn describe(path: &Path) -> String {
    match std::fs::read_link(path) {
        Ok(to) => format!("{} links to {}", path.display(), to.display()),
        Err(_) => format!("{} is a file that is not Nimbus", path.display()),
    }
}

/// Install as a link at `link` to `target`, given the folders `dirs` a
/// terminal searches.
#[cfg(not(windows))]
fn plan_link(link: &Path, target: &Path, dirs: &[PathBuf]) -> Plan {
    if let Some(found) = on_path(target, dirs).filter(|f| f != link) {
        return Plan::Nothing(format!(
            "The nimbus command is already on your PATH: {}",
            found.display()
        ));
    }
    match std::fs::symlink_metadata(link) {
        Err(_) => Plan::Run(Step::Link {
            link: link.to_path_buf(),
            target: target.to_path_buf(),
            replace: false,
        }),
        Ok(_) if reaches(link, target) => Plan::Nothing(format!(
            "The nimbus command is already installed: {} links to {}",
            link.display(),
            target.display()
        )),
        Ok(meta) if meta.is_dir() => Plan::Refused(format!(
            "{} is a folder; nothing was changed",
            link.display()
        )),
        Ok(_) => Plan::Ask(
            format!("{}. Replace it with a link to Nimbus?", describe(link)),
            Step::Link {
                link: link.to_path_buf(),
                target: target.to_path_buf(),
                replace: true,
            },
        ),
    }
}

/// Uninstall the link at `link`, if it is Nimbus's.
#[cfg(not(windows))]
fn plan_unlink(link: &Path, target: &Path) -> Plan {
    match std::fs::symlink_metadata(link) {
        Err(_) => Plan::Nothing(format!("There is no nimbus command at {}", link.display())),
        Ok(meta) if meta.file_type().is_symlink() && is_ours(link, target) => {
            Plan::Run(Step::Unlink(link.to_path_buf()))
        }
        Ok(_) => Plan::Refused(format!("{}, so it was left as it is", describe(link))),
    }
}

/// Make the change, on a worker. Returns the message to show.
///
/// # Errors
///
/// What went wrong, as a message for the message line.
pub fn run(step: &Step) -> Result<String, String> {
    match step {
        Step::Link {
            link,
            target,
            replace,
        } => make_link(link, target, *replace),
        Step::Unlink(link) => remove_link(link),
        Step::UserPath(dir, add) => user_path(dir, *add),
    }
}

/// The note added after installing when a terminal would not find the
/// link's folder yet.
#[cfg(unix)]
fn path_note(link: &Path) -> String {
    let Some(dir) = link.parent() else {
        return String::new();
    };
    let listed =
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir));
    if listed || cfg!(target_os = "macos") {
        // macOS terminals always search /usr/local/bin.
        String::new()
    } else {
        format!(
            "; {} is not on your PATH yet, so add it in your shell's profile",
            dir.display()
        )
    }
}

#[cfg(unix)]
fn make_link(link: &Path, target: &Path, replace: bool) -> Result<String, String> {
    let done = || {
        format!(
            "Installed the nimbus command: {} links to {}{}",
            link.display(),
            target.display(),
            path_note(link)
        )
    };
    let attempt = (|| {
        if let Some(dir) = link.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if replace {
            std::fs::remove_file(link)?;
        }
        std::os::unix::fs::symlink(target, link)
    })();
    match attempt {
        Ok(()) => Ok(done()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && cfg!(target_os = "macos") => {
            as_administrator(
                "mkdir -p \" & quoted form of (item 1 of argv) & \" && ln -sfn \" & quoted form of (item 2 of argv) & \" \" & quoted form of (item 3 of argv)",
                &[
                    link.parent().unwrap_or(Path::new("/usr/local/bin")),
                    target,
                    link,
                ],
            )
            .map(|()| done())
        }
        Err(e) => Err(format!("Cannot make {}: {e}", link.display())),
    }
}

#[cfg(unix)]
fn remove_link(link: &Path) -> Result<String, String> {
    let done = || format!("Removed the nimbus command at {}", link.display());
    match std::fs::remove_file(link) {
        Ok(()) => Ok(done()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && cfg!(target_os = "macos") => {
            as_administrator("rm -f \" & quoted form of (item 1 of argv)", &[link]).map(|()| done())
        }
        Err(e) => Err(format!("Cannot remove {}: {e}", link.display())),
    }
}

/// Run a shell command as an administrator through the system's password
/// prompt. `command` is the middle of an `AppleScript` string expression
/// (`"mkdir -p " & quoted form of (item 1 of argv)`), so the paths, passed
/// as arguments in `args`, are quoted by `AppleScript` and never pasted
/// into the shell command as text.
#[cfg(unix)]
fn as_administrator(command: &str, args: &[&Path]) -> Result<(), String> {
    let script = format!("do shell script \"{command} with administrator privileges");
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", "on run argv", "-e", &script, "-e", "end run"])
        .args(args)
        .output()
        .map_err(|e| format!("Cannot ask for an administrator password: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&output.stderr);
    if why.contains("-128") {
        // The user pressed Cancel in the password prompt.
        Err("Cancelled; nothing was changed".to_owned())
    } else {
        Err(format!("Could not change /usr/local/bin: {}", why.trim()))
    }
}

#[cfg(not(unix))]
fn make_link(link: &Path, _target: &Path, _replace: bool) -> Result<String, String> {
    Err(format!("Cannot make {} here", link.display()))
}

#[cfg(not(unix))]
fn remove_link(link: &Path) -> Result<String, String> {
    Err(format!("Cannot remove {} here", link.display()))
}

/// The PowerShell script that edits the user's `PATH`. It reads the
/// registry value unexpanded (so entries such as `%USERPROFILE%\bin` stay
/// as written), adds or removes the folder in `NIMBUS_DIR`, writes it back
/// as `REG_EXPAND_SZ`, then sets and clears a variable through .NET, which
/// broadcasts the "environment changed" message so new terminals see it.
/// It prints `added`, `present`, `removed` or `absent`.
#[cfg_attr(not(windows), expect(dead_code, reason = "only Windows edits PATH"))]
const PATH_SCRIPT: &str = r"$ErrorActionPreference = 'Stop'
$key = 'HKCU:\Environment'
$old = (Get-Item -LiteralPath $key).GetValue('Path', '', 'DoNotExpandEnvironmentNames')
$dir = $env:NIMBUS_DIR.TrimEnd('\')
$parts = @($old -split ';' | Where-Object { $_ -ne '' })
$has = @($parts | Where-Object { $_.TrimEnd('\') -eq $dir }).Count -gt 0
if ($env:NIMBUS_ADD -eq '1') {
  if ($has) { 'present'; exit 0 }
  $new = (@($parts) + $dir) -join ';'
} else {
  if (-not $has) { 'absent'; exit 0 }
  $new = @($parts | Where-Object { $_.TrimEnd('\') -ne $dir }) -join ';'
}
Set-ItemProperty -LiteralPath $key -Name 'Path' -Value $new -Type ExpandString
[Environment]::SetEnvironmentVariable('NIMBUS_PATH_CHANGED', '1', 'User')
[Environment]::SetEnvironmentVariable('NIMBUS_PATH_CHANGED', $null, 'User')
if ($env:NIMBUS_ADD -eq '1') { 'added' } else { 'removed' }
";

#[cfg(windows)]
fn user_path(dir: &Path, add: bool) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`: no console flashes up while it runs.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // By full path, so a `powershell.exe` in some other PATH folder is
    // never the one run.
    let powershell = std::env::var_os("SystemRoot").map_or_else(
        || PathBuf::from("powershell.exe"),
        |r| PathBuf::from(r).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    );
    let output = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
        ])
        .args(["-EncodedCommand", &encoded_command(PATH_SCRIPT)])
        .env("NIMBUS_DIR", dir)
        .env("NIMBUS_ADD", if add { "1" } else { "0" })
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Cannot run PowerShell: {e}"))?;
    let said = String::from_utf8_lossy(&output.stdout);
    let shown = dir.display();
    match said.trim() {
        "added" => Ok(format!(
            "Added {shown} to your PATH: terminals opened from now on find nimbus"
        )),
        "present" => Ok(format!("{shown} is already on your PATH")),
        "removed" => Ok(format!("Removed {shown} from your PATH")),
        "absent" => Ok(format!("{shown} was not on your PATH")),
        _ => Err(format!(
            "Could not change your PATH: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

#[cfg(not(windows))]
fn user_path(dir: &Path, _add: bool) -> Result<String, String> {
    Err(format!("Cannot change PATH for {} here", dir.display()))
}

/// A script for PowerShell's `-EncodedCommand`: UTF-16LE, then Base64.
/// Passing it this way means no quote in it is ever reinterpreted by the
/// command line.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "only Windows edits PATH")
)]
fn encoded_command(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_encoded_for_powershell() {
        // `[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('ab'))`
        assert_eq!(encoded_command("ab"), "YQBiAA==");
        assert_eq!(encoded_command("abc"), "YQBiAGMA");
        assert_eq!(encoded_command(""), "");
    }

    #[cfg(unix)]
    mod links {
        use super::super::*;
        use std::os::unix::fs::symlink;

        fn temp(tag: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("nimbus-install-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(dir.join("nimbus-exe"), "").unwrap();
            dir
        }

        #[test]
        fn install_makes_and_finds_the_link() {
            let dir = temp("make");
            let (link, target) = (dir.join("bin/nimbus"), dir.join("nimbus-exe"));
            let plan = plan_link(&link, &target, &[]);
            let Plan::Run(step) = plan else {
                panic!("{plan:?}")
            };
            assert!(run(&step).is_ok());
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            // Again: nothing to do.
            assert!(matches!(plan_link(&link, &target, &[]), Plan::Nothing(_)));
            // Found through PATH somewhere else: nothing to do either.
            let other = dir.join("elsewhere/nimbus");
            assert!(matches!(
                plan_link(&other, &target, &[dir.join("bin")]),
                Plan::Nothing(_)
            ));
            // Uninstall removes it.
            let Plan::Run(step) = plan_unlink(&link, &target) else {
                panic!()
            };
            assert!(run(&step).is_ok());
            assert!(std::fs::symlink_metadata(&link).is_err());
            assert!(matches!(plan_unlink(&link, &target), Plan::Nothing(_)));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn something_else_is_never_replaced_silently() {
            let dir = temp("other");
            let (link, target) = (dir.join("bin/nimbus"), dir.join("nimbus-exe"));
            symlink("/usr/bin/true", &link).unwrap();
            assert!(matches!(plan_link(&link, &target, &[]), Plan::Ask(..)));
            assert!(matches!(plan_unlink(&link, &target), Plan::Refused(_)));
            std::fs::remove_file(&link).unwrap();
            std::fs::write(&link, "#!/bin/sh\n").unwrap();
            let Plan::Ask(_, step) = plan_link(&link, &target, &[]) else {
                panic!()
            };
            assert!(matches!(plan_unlink(&link, &target), Plan::Refused(_)));
            // Agreed to: replaced.
            assert!(run(&step).is_ok());
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn a_link_to_a_moved_app_is_ours() {
            let dir = temp("moved");
            let (link, target) = (dir.join("bin/nimbus"), dir.join("nimbus-exe"));
            symlink("/gone/Nimbus.app/Contents/MacOS/nimbus", &link).unwrap();
            assert!(matches!(plan_unlink(&link, &target), Plan::Run(_)));
            // Installing over it asks, since it is broken rather than ours now.
            assert!(matches!(plan_link(&link, &target, &[]), Plan::Ask(..)));
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
