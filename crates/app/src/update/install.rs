//! Putting a verified release in place of the running one (M31).
//!
//! **Replacing a running program.** On macOS and Linux a running
//! program's files can be renamed: the process keeps the files it has
//! open (they are found by inode, not by name), and the next start finds
//! whatever is at the name then. So the new copy is unpacked *beside* the
//! old one, on the same volume, and two renames swap them:
//! `Nimbus.app` → `Nimbus.app.previous`, then the new one → `Nimbus.app`.
//! Each rename is atomic; if the second fails the first is undone. The
//! previous version stays at `.previous` for Roll Back Update until the
//! next update replaces it.
//!
//! Windows does not let a running `.exe` be replaced, so there the new
//! release's installer runs after Nimbus has quit and does the
//! replacing itself (silently, per user, so with no UAC prompt).
//!
//! **Who owns the install.** Homebrew and apt keep their own record of
//! what they installed; replacing their files behind their back would
//! confuse them. Those installs are handed back to `brew` and `apt`.
//! Nor is a build from source (`cargo build`, `cargo install`) replaced:
//! that is updated with git.
//!
//! **A start that fails.** After a swap, a small file beside the config
//! (`update-pending`) records the new version. Each windowed start of
//! that version counts a try; drawing the first frame deletes the file.
//! If a start finds a try already counted, the previous start never drew
//! a frame, so the old version is put back and started instead.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Which package manager installed Nimbus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Manager {
    /// The Homebrew cask (`brew install --cask`).
    Homebrew,
    /// The `.deb` package (apt or dpkg).
    Apt,
}

/// How this copy of Nimbus is updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// macOS: replace this `Nimbus.app` with the release's zip.
    MacApp(PathBuf),
    /// Linux: replace this `AppImage` file.
    AppImage(PathBuf),
    /// Linux: replace this binary from the release's tarball.
    Tarball(PathBuf),
    /// Windows: run the release's installer after quitting.
    WindowsSetup,
    /// A package manager installed it and should update it.
    Managed(Manager),
    /// Nimbus cannot write the folder it is installed in.
    ReadOnly(PathBuf),
    /// Built from source with cargo: updated with git, not here.
    Source,
    /// No release package fits this install (the Windows zip, a CPU the
    /// releases are not built for).
    Unsupported,
}

impl Plan {
    /// The release file this plan installs, for `version`.
    #[must_use]
    pub fn asset(&self, version: &str) -> Option<String> {
        match self {
            Self::MacApp(_) => Some(format!("Nimbus-{version}-macos.zip")),
            Self::AppImage(_) => Some(format!("Nimbus-{version}-x86_64.AppImage")),
            Self::Tarball(_) => Some(format!("nimbus-{version}-linux-x86_64.tar.gz")),
            Self::WindowsSetup => Some(format!("nimbus-{version}-windows-x86_64-setup.exe")),
            Self::Managed(_) | Self::ReadOnly(_) | Self::Source | Self::Unsupported => None,
        }
    }

    /// What the swap replaces, for the plans that swap.
    #[must_use]
    pub fn target(&self) -> Option<&Path> {
        match self {
            Self::MacApp(p) | Self::AppImage(p) | Self::Tarball(p) => Some(p),
            _ => None,
        }
    }

    /// Why Install Update cannot run for this install, and what to do
    /// instead; `None` when it can.
    #[must_use]
    pub fn refusal(&self, version: &str) -> Option<String> {
        Some(match self {
            Self::Managed(Manager::Homebrew) => {
                "Homebrew installed Nimbus; update it with: brew upgrade --cask nimbus".to_owned()
            }
            Self::Managed(Manager::Apt) => format!(
                "Nimbus came from a .deb; download nimbus_{version}_amd64.deb from the release page and run: sudo apt install ./nimbus_{version}_amd64.deb"
            ),
            Self::ReadOnly(dir) => format!(
                "Nimbus cannot write {}; get {version} from the release page (Open Latest Release Page)",
                dir.display()
            ),
            Self::Source => {
                "This Nimbus was built from source; update it with git pull and cargo".to_owned()
            }
            Self::Unsupported => format!(
                "Nimbus cannot update this install itself; get {version} from the release page (Open Latest Release Page)"
            ),
            Self::MacApp(_) | Self::AppImage(_) | Self::Tarball(_) | Self::WindowsSetup => {
                return None;
            }
        })
    }
}

/// What the system is, for [`detect_from`]: the parts of the environment
/// that decide the plan, so tests can describe any system.
#[derive(Debug, Clone, Default)]
pub struct System {
    /// `std::env::consts::OS`.
    pub os: &'static str,
    /// `std::env::consts::ARCH`.
    pub arch: &'static str,
    /// `$APPIMAGE`, set by the `AppImage` runtime to the image's own path.
    pub appimage: Option<PathBuf>,
    /// A Homebrew cask record for Nimbus exists.
    pub homebrew_cask: bool,
    /// dpkg lists a `nimbus` package.
    pub dpkg_package: bool,
}

impl System {
    /// The system this is running on.
    #[must_use]
    pub fn current() -> Self {
        let homebrew_cask = cfg!(target_os = "macos")
            && [
                "/opt/homebrew/Caskroom/nimbus",
                "/usr/local/Caskroom/nimbus",
            ]
            .iter()
            .any(|p| Path::new(p).exists());
        let dpkg_package =
            cfg!(target_os = "linux") && Path::new("/var/lib/dpkg/info/nimbus.list").exists();
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            appimage: std::env::var_os("APPIMAGE").map(PathBuf::from),
            homebrew_cask,
            dpkg_package,
        }
    }
}

/// How the running copy is updated.
#[must_use]
pub fn detect() -> Plan {
    let Ok(exe) = std::env::current_exe() else {
        return Plan::Unsupported;
    };
    // Through any symlink (Homebrew's `binary`, a link in /usr/local/bin)
    // to the real file.
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    detect_from(&exe, &System::current(), &writable)
}

/// The plan for the binary at `exe` on `system`; `can_write(dir)` says
/// whether a folder can be written.
pub fn detect_from(exe: &Path, system: &System, can_write: &dyn Fn(&Path) -> bool) -> Plan {
    if is_cargo_build(exe) {
        return Plan::Source;
    }
    match system.os {
        "macos" => {
            if system.homebrew_cask || exe.components().any(|c| c.as_os_str() == "Caskroom") {
                return Plan::Managed(Manager::Homebrew);
            }
            // …/Nimbus.app/Contents/MacOS/nimbus
            let app = exe.ancestors().nth(3).filter(|a| {
                a.extension().is_some_and(|e| e == "app")
                    && exe.parent().is_some_and(|p| p.ends_with("Contents/MacOS"))
            });
            match app {
                Some(app) => writable_plan(app, can_write, || Plan::MacApp(app.to_owned())),
                None => Plan::Source,
            }
        }
        "linux" => {
            if system.arch != "x86_64" {
                return Plan::Unsupported;
            }
            if let Some(image) = &system.appimage {
                return writable_plan(image, can_write, || Plan::AppImage(image.clone()));
            }
            if system.dpkg_package && exe == Path::new("/usr/bin/nimbus") {
                return Plan::Managed(Manager::Apt);
            }
            writable_plan(exe, can_write, || Plan::Tarball(exe.to_owned()))
        }
        "windows" => {
            // The installer leaves its uninstaller beside nimbus.exe; the
            // zip does not, and is not updated in place.
            let Some(dir) = exe.parent() else {
                return Plan::Unsupported;
            };
            if !dir.join("unins000.exe").exists() {
                Plan::Unsupported
            } else if can_write(dir) {
                Plan::WindowsSetup
            } else {
                // Installed for all users under Program Files: the
                // installer would need an administrator.
                Plan::ReadOnly(dir.to_owned())
            }
        }
        _ => Plan::Unsupported,
    }
}

/// `plan()` when the folder holding `path` can be written (the swap
/// renames inside it), else [`Plan::ReadOnly`].
fn writable_plan(
    path: &Path,
    can_write: &dyn Fn(&Path) -> bool,
    plan: impl FnOnce() -> Plan,
) -> Plan {
    match path.parent() {
        Some(dir) if can_write(dir) => plan(),
        Some(dir) => Plan::ReadOnly(dir.to_owned()),
        None => Plan::Unsupported,
    }
}

/// Whether `exe` is cargo's output (`target/debug`, `target/release`,
/// `target/<triple>/release`) or a `cargo install` (`~/.cargo/bin`).
fn is_cargo_build(exe: &Path) -> bool {
    let names: Vec<&std::ffi::OsStr> = exe
        .components()
        .map(std::path::Component::as_os_str)
        .collect();
    names.windows(2).any(|w| {
        (w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
            || (w[0] == ".cargo" && w[1] == "bin")
    }) || names
        .windows(3)
        .any(|w| w[0] == "target" && (w[2] == "debug" || w[2] == "release"))
}

/// Whether a file can be made in `dir`: the only sure test, since
/// permissions, ACLs and read-only volumes all have a say.
#[must_use]
pub fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".nimbus-write-test-{}", std::process::id()));
    match fs::File::create(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Where the replaced version waits, for Roll Back Update:
/// `Nimbus.app.previous`, `nimbus.previous`.
#[must_use]
pub fn previous_of(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_owned();
    name.push(".previous");
    target.with_file_name(name)
}

/// The folder a package is unpacked into, beside `target` so the swap is
/// a rename on one volume. Hidden, and named for the version.
fn staging_dir(target: &Path, version: &str) -> PathBuf {
    target.with_file_name(format!(".nimbus-update-{version}"))
}

/// A change waiting to be made when Nimbus quits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staged {
    /// Swap `new` in at `target`; afterwards delete `cleanup` (the
    /// staging folder), if any.
    Replace {
        /// The version being installed.
        version: String,
        /// What is replaced.
        target: PathBuf,
        /// The verified new copy, beside it.
        new: PathBuf,
        /// A folder to delete after the swap.
        cleanup: Option<PathBuf>,
    },
    /// Run this verified Windows installer.
    Installer {
        /// The version being installed.
        version: String,
        /// The installer, in the cache folder.
        setup: PathBuf,
    },
    /// Put `target`'s `.previous` copy back.
    RollBack {
        /// What is replaced by its previous version.
        target: PathBuf,
    },
}

impl Staged {
    /// The version this installs, or `None` for a roll back.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Replace { version, .. } | Self::Installer { version, .. } => Some(version),
            Self::RollBack { .. } => None,
        }
    }

    /// Forget a staged update without installing it: delete what was
    /// unpacked.
    pub fn discard(&self) {
        match self {
            Self::Replace { new, cleanup, .. } => {
                let _ = remove(cleanup.as_deref().unwrap_or(new));
            }
            Self::Installer { setup, .. } => {
                let _ = fs::remove_file(setup);
            }
            Self::RollBack { .. } => {}
        }
    }
}

/// Unpack the verified `package` for `plan` beside what it replaces, so
/// that quitting only has to rename. Runs on the worker.
///
/// # Errors
///
/// When unpacking fails (no space, `ditto` or `tar` missing) or the
/// package does not hold what it should.
pub fn stage(plan: &Plan, package: &Path, version: &str) -> io::Result<Staged> {
    match plan {
        Plan::MacApp(app) => {
            let dir = staging_dir(app, version);
            let _ = remove(&dir);
            fs::create_dir_all(&dir)?;
            // ditto keeps the bundle's permissions and symlinks; a file
            // Nimbus downloaded itself has no quarantine mark, so neither
            // does the app, and Gatekeeper does not stop it.
            if let Err(e) = run(Command::new("ditto")
                .arg("-x")
                .arg("-k")
                .arg(package)
                .arg(&dir))
            {
                let _ = remove(&dir);
                return Err(e);
            }
            let new = dir.join(app.file_name().unwrap_or_default());
            let new = if new.exists() {
                new
            } else {
                dir.join("Nimbus.app")
            };
            if !new.join("Contents/MacOS/nimbus").is_file() {
                let _ = remove(&dir);
                return Err(io::Error::other("the zip has no Nimbus.app in it"));
            }
            Ok(Staged::Replace {
                version: version.to_owned(),
                target: app.clone(),
                new,
                cleanup: Some(dir),
            })
        }
        Plan::AppImage(image) => {
            let mut name = image.file_name().unwrap_or_default().to_owned();
            name.push(".new");
            let new = image.with_file_name(name);
            fs::copy(package, &new)?;
            #[cfg(unix)]
            make_executable(&new)?;
            let _ = fs::remove_file(package);
            Ok(Staged::Replace {
                version: version.to_owned(),
                target: image.clone(),
                new,
                cleanup: None,
            })
        }
        Plan::Tarball(binary) => {
            let dir = staging_dir(binary, version);
            let _ = remove(&dir);
            fs::create_dir_all(&dir)?;
            if let Err(e) = run(Command::new("tar")
                .arg("-xzf")
                .arg(package)
                .arg("-C")
                .arg(&dir))
            {
                let _ = remove(&dir);
                return Err(e);
            }
            let new = dir
                .join(format!("nimbus-{version}-linux-x86_64"))
                .join("nimbus");
            if !new.is_file() {
                let _ = remove(&dir);
                return Err(io::Error::other("the tarball has no nimbus binary in it"));
            }
            let _ = fs::remove_file(package);
            Ok(Staged::Replace {
                version: version.to_owned(),
                target: binary.clone(),
                new,
                cleanup: Some(dir),
            })
        }
        Plan::WindowsSetup => Ok(Staged::Installer {
            version: version.to_owned(),
            setup: package.to_owned(),
        }),
        _ => Err(io::Error::other("this install is not updated by Nimbus")),
    }
}

/// The roll back for `plan`, when a previous version is kept.
#[must_use]
pub fn roll_back(plan: &Plan) -> Option<Staged> {
    let target = plan.target()?;
    previous_of(target).exists().then(|| Staged::RollBack {
        target: target.to_owned(),
    })
}

/// Make the staged change, as Nimbus quits. With `relaunch`, start the
/// result with those arguments (the session to restore).
///
/// # Errors
///
/// When a rename fails (after undoing the first), or the installer or
/// the new version cannot be started.
pub fn apply(staged: &Staged, relaunch: Option<&[OsString]>) -> io::Result<()> {
    match staged {
        Staged::Replace {
            version,
            target,
            new,
            cleanup,
        } => {
            swap(target, new)?;
            if let Some(dir) = cleanup {
                let _ = remove(dir);
            }
            record_pending(version);
            if let Some(args) = relaunch {
                start(target, args)?;
            }
            Ok(())
        }
        Staged::Installer { setup, .. } => {
            // /CLOSEAPPLICATIONS closes this process if it is still on its
            // way out; /RELAUNCH=1 is ours (nimbus.iss): start Nimbus
            // again when done. Detached, so it outlives us.
            let mut cmd = Command::new(setup);
            cmd.args([
                "/VERYSILENT",
                "/SUPPRESSMSGBOXES",
                "/CLOSEAPPLICATIONS",
                "/NORESTART",
            ]);
            if relaunch.is_some() {
                cmd.arg("/RELAUNCH=1");
            }
            detach(&mut cmd).spawn().map(drop)
        }
        Staged::RollBack { target } => {
            exchange_with_previous(target)?;
            clear_pending();
            if let Some(args) = relaunch {
                start(target, args)?;
            }
            Ok(())
        }
    }
}

/// Rename `target` to its `.previous` (replacing an older one) and `new`
/// to `target`. If the second rename fails, the first is undone.
fn swap(target: &Path, new: &Path) -> io::Result<()> {
    let previous = previous_of(target);
    if previous.exists() || previous.is_symlink() {
        remove(&previous)?;
    }
    fs::rename(target, &previous)?;
    if let Err(e) = fs::rename(new, target) {
        let _ = fs::rename(&previous, target);
        return Err(e);
    }
    Ok(())
}

/// Put `target`'s `.previous` back at `target`; the version that was
/// there is deleted.
fn exchange_with_previous(target: &Path) -> io::Result<()> {
    let previous = previous_of(target);
    let mut name = target.file_name().unwrap_or_default().to_owned();
    name.push(".rolled-back");
    let aside = target.with_file_name(name);
    if aside.exists() {
        remove(&aside)?;
    }
    fs::rename(target, &aside)?;
    if let Err(e) = fs::rename(&previous, target) {
        let _ = fs::rename(&aside, target);
        return Err(e);
    }
    let _ = remove(&aside);
    Ok(())
}

/// Start the program at `target` (an app bundle or a binary) with `args`.
fn start(target: &Path, args: &[OsString]) -> io::Result<()> {
    let mut cmd = if target.extension().is_some_and(|e| e == "app") {
        // Through Launch Services, as the Dock or Finder would: -n for a
        // new instance even though this one is still exiting.
        let mut c = Command::new("open");
        c.arg("-n").arg(target);
        if !args.is_empty() {
            c.arg("--args").args(args);
        }
        c
    } else {
        let mut c = Command::new(target);
        c.args(args);
        c
    };
    detach(&mut cmd).spawn().map(drop)
}

/// No inherited terminal: the child outlives this process.
fn detach(cmd: &mut Command) -> &mut Command {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
}

/// Run `cmd` to completion, failing unless it exits with 0.
fn run(cmd: &mut Command) -> io::Result<()> {
    let out = cmd.stdin(Stdio::null()).output()?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(io::Error::other(format!(
            "{} failed: {}",
            cmd.get_program().to_string_lossy(),
            err.trim()
        )))
    }
}

/// Delete a file, a symlink or a whole folder.
fn remove(path: &Path) -> io::Result<()> {
    if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// An `AppImage` must be executable to run (Windows has no such bit).
#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

/// `update-pending`, beside the config file.
fn pending_path() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("update-pending")))
}

/// `update-rolled-back`, beside the config file: what the old version
/// says when it starts after an automatic roll back.
fn rolled_back_path() -> Option<PathBuf> {
    crate::config::path().and_then(|p| p.parent().map(|d| d.join("update-rolled-back")))
}

/// Note that `version` was just swapped in and has not drawn yet.
fn record_pending(version: &str) {
    if let Some(path) = pending_path() {
        let _ = fs::write(path, format!("version={version}\ntries=0\n"));
    }
}

/// Forget the note: the new version drew its first frame, or was rolled
/// back.
pub fn clear_pending() {
    if let Some(path) = pending_path() {
        let _ = fs::remove_file(path);
    }
}

/// What a windowed start should do about an update just installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// Carry on.
    Normal,
    /// The last start of this version never drew a frame: put the
    /// previous version back and start it instead.
    RollBack(Staged),
}

/// Called early in a windowed start of `running`: count this try, or
/// decide to roll back. The text is `update-pending`'s.
fn start_from(text: &str, running: &str) -> (Start, Option<String>) {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    if field("version") != Some(running) {
        // Another version runs (a roll back, an older copy): not ours.
        return (Start::Normal, None);
    }
    let tries: u32 = field("tries").and_then(|t| t.parse().ok()).unwrap_or(0);
    if tries >= 1 {
        let Some(target) = detect().target().map(Path::to_owned) else {
            return (Start::Normal, None);
        };
        if previous_of(&target).exists() {
            return (Start::RollBack(Staged::RollBack { target }), None);
        }
        return (Start::Normal, None);
    }
    (
        Start::Normal,
        Some(format!("version={running}\ntries={}\n", tries + 1)),
    )
}

/// The start check of [`Start`], reading and updating `update-pending`.
#[must_use]
pub fn on_start(running: &str) -> Start {
    let Some(path) = pending_path() else {
        return Start::Normal;
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return Start::Normal;
    };
    let (start, write) = start_from(&text, running);
    match &write {
        Some(text) => {
            let _ = fs::write(&path, text);
        }
        None => {
            let _ = fs::remove_file(&path);
        }
    }
    if matches!(start, Start::RollBack(_))
        && let Some(note) = rolled_back_path()
    {
        let _ = fs::write(note, running);
    }
    start
}

/// After an automatic roll back, the version that failed to start, once:
/// the note is deleted as it is read.
#[must_use]
pub fn take_rolled_back() -> Option<String> {
    let path = rolled_back_path()?;
    let text = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);
    Some(text.trim().to_owned()).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("nimbus-install-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mac() -> System {
        System {
            os: "macos",
            arch: "aarch64",
            ..System::default()
        }
    }

    fn linux() -> System {
        System {
            os: "linux",
            arch: "x86_64",
            ..System::default()
        }
    }

    fn yes(_: &Path) -> bool {
        true
    }

    fn no(_: &Path) -> bool {
        false
    }

    #[test]
    fn mac_installs_are_told_apart() {
        let exe = Path::new("/Applications/Nimbus.app/Contents/MacOS/nimbus");
        assert_eq!(
            detect_from(exe, &mac(), &yes),
            Plan::MacApp("/Applications/Nimbus.app".into())
        );
        assert_eq!(
            detect_from(exe, &mac(), &no),
            Plan::ReadOnly("/Applications".into())
        );
        let brew = System {
            homebrew_cask: true,
            ..mac()
        };
        assert_eq!(
            detect_from(exe, &brew, &yes),
            Plan::Managed(Manager::Homebrew)
        );
        assert_eq!(
            detect_from(
                Path::new("/Users/s/scripts/nimbus/target/release/nimbus"),
                &mac(),
                &yes
            ),
            Plan::Source
        );
        assert_eq!(
            detect_from(Path::new("/Users/s/.cargo/bin/nimbus"), &mac(), &yes),
            Plan::Source
        );
        // A bare binary outside a bundle (copied by hand): not ours to swap.
        assert_eq!(
            detect_from(Path::new("/usr/local/bin/nimbus"), &mac(), &yes),
            Plan::Source
        );
    }

    #[test]
    fn linux_installs_are_told_apart() {
        let image = System {
            appimage: Some("/home/s/Apps/Nimbus.AppImage".into()),
            ..linux()
        };
        // The AppImage runtime runs the binary from a mount; $APPIMAGE is
        // the file to replace.
        assert_eq!(
            detect_from(
                Path::new("/tmp/.mount_NimbusX/usr/bin/nimbus"),
                &image,
                &yes
            ),
            Plan::AppImage("/home/s/Apps/Nimbus.AppImage".into())
        );
        let deb = System {
            dpkg_package: true,
            ..linux()
        };
        assert_eq!(
            detect_from(Path::new("/usr/bin/nimbus"), &deb, &no),
            Plan::Managed(Manager::Apt)
        );
        assert_eq!(
            detect_from(Path::new("/opt/nimbus/nimbus"), &linux(), &yes),
            Plan::Tarball("/opt/nimbus/nimbus".into())
        );
        assert_eq!(
            detect_from(Path::new("/opt/nimbus/nimbus"), &linux(), &no),
            Plan::ReadOnly("/opt/nimbus".into())
        );
        assert_eq!(
            detect_from(
                Path::new("/home/s/nimbus/target/x86_64-unknown-linux-gnu/release/nimbus"),
                &linux(),
                &yes
            ),
            Plan::Source
        );
        let arm = System {
            arch: "aarch64",
            ..linux()
        };
        assert_eq!(
            detect_from(Path::new("/opt/nimbus/nimbus"), &arm, &yes),
            Plan::Unsupported
        );
    }

    #[test]
    fn assets_are_the_release_workflow_names() {
        // .github/workflows/release.yml names the packages; these must match.
        let v = "2.0.0";
        assert_eq!(
            Plan::MacApp(PathBuf::new()).asset(v).unwrap(),
            "Nimbus-2.0.0-macos.zip"
        );
        assert_eq!(
            Plan::AppImage(PathBuf::new()).asset(v).unwrap(),
            "Nimbus-2.0.0-x86_64.AppImage"
        );
        assert_eq!(
            Plan::Tarball(PathBuf::new()).asset(v).unwrap(),
            "nimbus-2.0.0-linux-x86_64.tar.gz"
        );
        assert_eq!(
            Plan::WindowsSetup.asset(v).unwrap(),
            "nimbus-2.0.0-windows-x86_64-setup.exe"
        );
        assert!(Plan::Source.asset(v).is_none());
        assert!(
            Plan::Managed(Manager::Homebrew)
                .refusal(v)
                .unwrap()
                .contains("brew upgrade")
        );
        assert!(
            Plan::Managed(Manager::Apt)
                .refusal(v)
                .unwrap()
                .contains("nimbus_2.0.0_amd64.deb")
        );
        assert!(Plan::WindowsSetup.refusal(v).is_none());
    }

    #[test]
    fn a_tarball_install_swaps_and_rolls_back() {
        let d = Dir::new("tarball");
        let bin = d.0.join("nimbus");
        fs::write(&bin, "old").unwrap();
        // A tarball laid out as the release workflow makes it.
        let src = d.0.join("src");
        let inner = src.join("nimbus-2.0.0-linux-x86_64");
        fs::create_dir_all(&inner).unwrap();
        fs::write(inner.join("nimbus"), "new").unwrap();
        let package = d.0.join("pkg.tar.gz");
        let tarred = Command::new("tar")
            .arg("-czf")
            .arg(&package)
            .arg("-C")
            .arg(&src)
            .arg("nimbus-2.0.0-linux-x86_64")
            .status()
            .unwrap();
        assert!(tarred.success());

        let plan = Plan::Tarball(bin.clone());
        let staged = stage(&plan, &package, "2.0.0").unwrap();
        assert_eq!(
            fs::read_to_string(&bin).unwrap(),
            "old",
            "nothing replaced yet"
        );
        swap(
            &bin,
            match &staged {
                Staged::Replace { new, .. } => new,
                _ => unreachable!(),
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&bin).unwrap(), "new");
        assert_eq!(fs::read_to_string(previous_of(&bin)).unwrap(), "old");

        let back = roll_back(&plan).unwrap();
        assert_eq!(
            back,
            Staged::RollBack {
                target: bin.clone()
            }
        );
        exchange_with_previous(&bin).unwrap();
        assert_eq!(fs::read_to_string(&bin).unwrap(), "old");
        assert!(!previous_of(&bin).exists());
        assert!(roll_back(&plan).is_none());
    }

    #[test]
    fn a_bad_tarball_leaves_nothing_behind() {
        let d = Dir::new("bad-tarball");
        let bin = d.0.join("nimbus");
        fs::write(&bin, "old").unwrap();
        let package = d.0.join("pkg.tar.gz");
        fs::write(&package, "not a tarball").unwrap();
        assert!(stage(&Plan::Tarball(bin.clone()), &package, "2.0.0").is_err());
        assert!(!staging_dir(&bin, "2.0.0").exists());
        assert_eq!(fs::read_to_string(&bin).unwrap(), "old");
    }

    #[test]
    fn an_appimage_is_copied_beside_and_swapped() {
        let d = Dir::new("appimage");
        let image = d.0.join("Nimbus.AppImage");
        fs::write(&image, "old").unwrap();
        let package = d.0.join("download");
        fs::write(&package, "new").unwrap();
        let staged = stage(&Plan::AppImage(image.clone()), &package, "2.0.0").unwrap();
        let Staged::Replace { new, .. } = &staged else {
            panic!("{staged:?}")
        };
        assert_eq!(new, &d.0.join("Nimbus.AppImage.new"));
        swap(&image, new).unwrap();
        assert_eq!(fs::read_to_string(&image).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&image).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "executable");
        }
    }

    #[test]
    fn a_failed_swap_puts_the_old_version_back() {
        let d = Dir::new("failed-swap");
        let bin = d.0.join("nimbus");
        fs::write(&bin, "old").unwrap();
        // The new copy is gone (deleted by hand, a full disk): the second
        // rename fails and the first is undone.
        assert!(swap(&bin, &d.0.join("missing")).is_err());
        assert_eq!(fs::read_to_string(&bin).unwrap(), "old");
        assert!(!previous_of(&bin).exists());
    }

    #[test]
    fn writable_folders_are_found_by_trying() {
        let d = Dir::new("writable");
        assert!(writable(&d.0));
        assert!(!writable(&d.0.join("no such folder")));
        assert_eq!(
            fs::read_dir(&d.0).unwrap().count(),
            0,
            "the probe is removed"
        );
    }

    #[test]
    fn a_start_counts_tries_for_the_new_version_only() {
        let (start, write) = start_from("version=2.0.0\ntries=0\n", "2.0.0");
        assert_eq!(start, Start::Normal);
        assert_eq!(write.as_deref(), Some("version=2.0.0\ntries=1\n"));
        // Another version is running: the note is dropped.
        assert_eq!(
            start_from("version=2.0.0\ntries=0\n", "1.1.0"),
            (Start::Normal, None)
        );
        assert_eq!(start_from("", "1.1.0"), (Start::Normal, None));
    }
}
