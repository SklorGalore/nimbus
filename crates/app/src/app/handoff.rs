//! The running Nimbus's side of M25: files handed over by a later
//! `nimbus` command, the `--wait` commands waiting for their tabs to
//! close, and the Install / Uninstall `nimbus` Command actions.

use std::path::{Path, PathBuf};

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

use super::{App, AppEvent};
use crate::install::{self, Plan};
use crate::instance::Waiter;

/// One `nimbus --wait` command: the files it is waiting on and the
/// connection to answer when none of them is open any more.
#[derive(Debug)]
pub struct Waiting {
    /// Each file as it was named and, when it exists, its canonical path,
    /// so a tab matches however the file was spelt.
    files: Vec<(PathBuf, Option<PathBuf>)>,
    waiter: Waiter,
}

impl App {
    /// Open what a later `nimbus` command handed over, bring the window
    /// to the front, and with `--wait` remember to answer when the files
    /// close.
    pub(super) fn handed_off(&mut self, paths: &[PathBuf], waiter: Option<Waiter>) {
        self.open_from_system(paths);
        if let Some(window) = &self.window {
            window.set_minimized(false);
            window.focus_window();
        }
        if let Some(waiter) = waiter {
            let files = paths
                .iter()
                .filter(|p| !p.is_dir())
                .map(|p| (p.clone(), canonical(p)))
                .collect();
            self.waiting.push(Waiting { files, waiter });
        }
    }

    /// Answer every `--wait` whose files are all closed. Called before the
    /// event loop sleeps, only while someone waits.
    pub(super) fn check_waiting(&mut self) {
        let mut i = 0;
        while i < self.waiting.len() {
            let open = self.waiting[i].files.iter().any(|(named, canon)| {
                self.docs.iter().any(|(_, d)| {
                    d.path.as_deref() == Some(named.as_path())
                        || (canon.is_some() && d.canonical == *canon)
                })
            });
            if open {
                i += 1;
            } else {
                self.waiting.swap_remove(i).waiter.done();
            }
        }
    }

    /// Install (`true`) or Uninstall `nimbus` Command: look at what is
    /// there now, ask before replacing anything that is not Nimbus's, and
    /// make the change on a worker.
    pub(super) fn install_command(&mut self, install: bool) {
        let plan = if install {
            install::plan_install()
        } else {
            install::plan_uninstall()
        };
        let step = match plan {
            Plan::Nothing(note) => return self.say(note, false),
            Plan::Refused(why) => return self.say(why, true),
            Plan::Run(step) => step,
            Plan::Ask(question, step) => {
                let mut dialog = MessageDialog::new()
                    .set_level(MessageLevel::Warning)
                    .set_title("Replace?")
                    .set_description(question)
                    .set_buttons(MessageButtons::OkCancelCustom(
                        "Replace".into(),
                        "Cancel".into(),
                    ));
                if let Some(window) = &self.window {
                    dialog = dialog.set_parent(window.as_ref());
                }
                // Some platforms report the custom OK button as plain Ok.
                let replace = match dialog.show() {
                    MessageDialogResult::Ok => true,
                    MessageDialogResult::Custom(label) => label == "Replace",
                    _ => false,
                };
                if !replace {
                    return self.say("Left as it is", false);
                }
                step
            }
        };
        self.say(
            if install {
                "Installing the nimbus command…"
            } else {
                "Removing the nimbus command…"
            },
            false,
        );
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("nimbus-install".to_owned())
            .spawn(move || {
                let _ = proxy.send_event(AppEvent::Installed(install::run(&step)));
            });
        if let Err(e) = spawned {
            self.say(format!("Cannot start the install: {e}"), true);
        }
    }
}

/// `path` with links and `..` resolved, or, for a file not created yet,
/// its folder resolved and the name added.
fn canonical(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok().or_else(|| {
        let folder = std::fs::canonicalize(path.parent()?).ok()?;
        Some(folder.join(path.file_name()?))
    })
}
