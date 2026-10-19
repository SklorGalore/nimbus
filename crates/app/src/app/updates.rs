//! The app's side of updates (M31): the actions, the worker that
//! prepares an update, and what happens when Nimbus quits with one
//! ready. The steps themselves are in `crate::update`.
//!
//! The states, in order: a check finds a newer release (`available`);
//! Install Update starts a worker (`cancel` is its flag) that downloads,
//! verifies and unpacks it, reporting progress on the message line; when
//! it finishes the change is `staged`. Restart to Update quits the usual
//! way (asking about unsaved tabs) with `restart` set, and `main` makes
//! the change after the event loop ends ([`App::take_update`]). Quitting
//! without restarting makes the change too, so the next start is the new
//! version.

use std::ffi::OsString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use winit::event_loop::{ActiveEventLoop, EventLoopProxy};

use super::{App, AppEvent};
use crate::update::{self, Release, UpdateError, install, verify};

/// Updates in this run.
#[derive(Debug, Default)]
pub(super) struct Updater {
    /// The newer release the last check found.
    pub(super) available: Option<Release>,
    /// A worker preparing an update; setting the flag cancels it.
    cancel: Option<Arc<AtomicBool>>,
    /// An update (or a roll back) ready to be made when Nimbus quits.
    staged: Option<install::Staged>,
    /// Start Nimbus again once the staged change is made.
    pub(super) restart: bool,
}

/// What `main` does after the event loop ends: make `staged`, then start
/// the result with `relaunch` as its arguments if asked.
#[derive(Debug)]
pub struct UpdateOnQuit {
    /// The change to make.
    pub staged: install::Staged,
    /// The arguments to start the new version with, when restarting.
    pub relaunch: Option<Vec<OsString>>,
}

/// What a worker preparing an update reports.
#[derive(Debug)]
pub enum UpdateEvent {
    /// Bytes downloaded so far, and the package's size when known.
    Progress(u64, Option<u64>),
    /// It finished: the staged update, or why not.
    Done(Result<install::Staged, UpdateError>),
}

/// Prepare release `version` on its own thread.
fn spawn_prepare(proxy: &EventLoopProxy<AppEvent>, version: String, cancel: Arc<AtomicBool>) {
    let proxy = proxy.clone();
    let spawned = std::thread::Builder::new()
        .name("nimbus-update-install".to_owned())
        .spawn(move || {
            let result = prepare(&proxy, &version, &cancel);
            let _ = proxy.send_event(AppEvent::Installing(UpdateEvent::Done(result)));
        });
    if let Err(e) = spawned {
        log::error!("could not start the update download: {e}");
    }
}

/// The worker's body: [`update::prepare`] for this install, from GitHub,
/// trusting the keys built in.
fn prepare(
    proxy: &EventLoopProxy<AppEvent>,
    version: &str,
    cancel: &AtomicBool,
) -> Result<install::Staged, UpdateError> {
    let plan = install::detect();
    let dir = update::download_dir()
        .ok_or_else(|| UpdateError::Io("no cache folder for the download".to_owned()))?;
    update::clear_downloads(&dir);
    if let Some(target) = plan.target() {
        log::info!("update: installing {version} over {}", target.display());
    }
    update::prepare(
        &update::download::Source::github(),
        verify::TRUSTED_KEYS,
        &plan,
        version,
        env!("CARGO_PKG_VERSION"),
        &dir,
        cancel,
        &mut |got, total| {
            let _ = proxy.send_event(AppEvent::Installing(UpdateEvent::Progress(got, total)));
        },
    )
}

/// `n` bytes in megabytes, one decimal.
fn mb(n: u64) -> String {
    // Display only: a package is far below 2^52 bytes.
    #[expect(clippy::cast_precision_loss, reason = "display of a size")]
    let m = n as f64 / (1024.0 * 1024.0);
    format!("{m:.1} MB")
}

impl App {
    /// Install Update: start preparing the newer release the last check
    /// found. With none found yet, it says to run Check for Updates.
    pub(super) fn install_update(&mut self) {
        if self.updater.cancel.is_some() {
            self.say(
                "The update is already downloading; Cancel Update stops it",
                false,
            );
            return;
        }
        if let Some(staged) = &self.updater.staged {
            let note = match staged.version() {
                Some(v) => format!("Nimbus {v} is ready; run Restart to Update to finish"),
                None => "A roll back is waiting; run Restart to Update to finish".to_owned(),
            };
            self.say(note, false);
            return;
        }
        let Some(release) = self.updater.available.clone() else {
            self.say(
                "No newer release known yet; run Check for Updates first",
                false,
            );
            return;
        };
        // Said before downloading anything: a package manager's install,
        // a folder Nimbus cannot write.
        if let Some(why) = install::detect().refusal(&release.version) {
            self.say(why, false);
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.updater.cancel = Some(Arc::clone(&cancel));
        self.say(format!("Downloading Nimbus {}…", release.version), false);
        spawn_prepare(&self.proxy, release.version, cancel);
    }

    /// Cancel Update: stop a download, or forget a staged update.
    pub(super) fn cancel_update(&mut self) {
        if let Some(cancel) = &self.updater.cancel {
            cancel.store(true, Ordering::Relaxed);
            self.say("Cancelling the update…", false);
        } else if let Some(staged) = self.updater.staged.take() {
            staged.discard();
            self.updater.restart = false;
            self.say("Update cancelled; nothing was changed", false);
        } else {
            self.say("No update is under way", false);
        }
    }

    /// Roll Back Update: put back the version the last update replaced,
    /// at the next restart.
    pub(super) fn roll_back_update(&mut self) {
        if self.updater.cancel.is_some() || self.updater.staged.is_some() {
            self.say("An update is under way; Cancel Update first", false);
            return;
        }
        let plan = install::detect();
        match install::roll_back(&plan) {
            Some(staged) => {
                self.updater.staged = Some(staged);
                self.say(
                    "The previous version comes back at the next start; run Restart to Update to do it now",
                    false,
                );
            }
            None => self.say(
                if matches!(plan, install::Plan::WindowsSetup) {
                    "Windows keeps no previous version; install an older release from the release page"
                } else {
                    "No previous version is kept here to roll back to"
                },
                false,
            ),
        }
    }

    /// Restart to Update: quit as usual, asking about unsaved tabs, then
    /// start again once the staged change is made.
    pub(super) fn restart_to_update(&mut self, event_loop: &ActiveEventLoop) {
        if self.updater.staged.is_none() {
            let note = if self.updater.cancel.is_some() {
                "The update is still downloading"
            } else {
                "No update is ready; run Install Update first"
            };
            self.say(note, false);
            return;
        }
        self.updater.restart = true;
        self.quit(event_loop);
    }

    /// A message from the worker preparing an update.
    pub(super) fn update_event(&mut self, event: UpdateEvent) {
        match event {
            UpdateEvent::Progress(got, total) => {
                let Some(release) = &self.updater.available else {
                    return;
                };
                if self.updater.cancel.is_none() {
                    return; // a late chunk after the end
                }
                let size = match total {
                    Some(t) => format!("{} of {}", mb(got), mb(t)),
                    None => mb(got),
                };
                let text = format!(
                    "Downloading Nimbus {}: {size} (Cancel Update stops it)",
                    release.version
                );
                self.say(text, false);
            }
            UpdateEvent::Done(result) => {
                self.updater.cancel = None;
                match result {
                    Ok(staged) => {
                        let version = staged.version().unwrap_or_default().to_owned();
                        self.updater.staged = Some(staged);
                        self.say(
                            format!(
                                "Nimbus {version} is ready; run Restart to Update to finish (or it installs when you quit)"
                            ),
                            false,
                        );
                    }
                    Err(UpdateError::Cancelled) => {
                        self.say("Update cancelled; nothing was changed", false);
                    }
                    Err(e) => {
                        log::warn!("update: {e}");
                        self.say(e.to_string(), true);
                    }
                }
            }
        }
    }

    /// A quit was cancelled at the unsaved-changes prompt: it no longer
    /// restarts.
    pub(super) fn quit_cancelled(&mut self) {
        self.updater.restart = false;
    }

    /// After the first frame: the version just installed has started, so
    /// it is kept; and if an automatic roll back brought this version
    /// back, say so.
    pub(super) fn update_started(&mut self) {
        if self.options.unattended() {
            return;
        }
        install::clear_pending();
        if let Some(failed) = install::take_rolled_back() {
            self.say(
                format!("Nimbus {failed} did not start, so the previous version was put back"),
                true,
            );
        }
    }

    /// For `main`, once the event loop has ended: the change to make, and
    /// the arguments to restart with.
    pub fn take_update(&mut self) -> Option<UpdateOnQuit> {
        let staged = self.updater.staged.take()?;
        if let Some(cancel) = &self.updater.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        let relaunch = self.updater.restart.then(|| self.relaunch_args());
        Some(UpdateOnQuit { staged, relaunch })
    }

    /// The arguments that bring this window back: the named workspace,
    /// else the session's folder (whose session was just saved), else
    /// the open files.
    fn relaunch_args(&self) -> Vec<OsString> {
        if let Some(name) = &self.options.workspace {
            return vec!["--workspace".into(), name.into()];
        }
        if let Some(root) = &self.ws.session_root {
            return vec![root.into()];
        }
        let mut args: Vec<OsString> = Vec::new();
        for tab in &self.tabs {
            if let Some(path) = &self.docs[tab.doc].path
                && !args.iter().any(|a| a == path.as_os_str())
            {
                args.push(path.into());
            }
        }
        args
    }
}
