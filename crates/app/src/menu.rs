//! Menus (DESIGN.md §4.4): the lists of actions they show, and on macOS
//! the system menu bar at the top of the screen.
//!
//! Menu items are actions from the registry. Their shortcuts come from the
//! keymap, so a rebinding in the config file shows in the menus too.
//!
//! Everywhere a menu belongs to the window, Nimbus draws it itself, in the
//! theme's colours ([`drawn`]): the menu bar and its drop-downs on Windows
//! and right-click menus on every system. Only macOS keeps a system menu
//! bar, because it lives at the top of the screen rather than in the
//! window; there a menu shortcut takes the key press before the window sees
//! it, and the action arrives as a menu event instead. Either way it runs
//! the same command. Linux has no menu bar: `muda` needs GTK there, which
//! Nimbus avoids, and the command palette covers every action.

pub mod drawn;

/// Menus by title, each a list of action ids; `"-"` is a separator.
pub const LAYOUT: &[(&str, &[&str])] = &[
    (
        "File",
        &[
            "file.new",
            "file.open",
            "file.openRecent",
            "file.quickOpen",
            "-",
            "file.reopenClosed",
            "session.reopenLast",
            "-",
            "workspace.addFolder",
            "workspace.open",
            "workspace.saveAs",
            "workspace.saveHere",
            "-",
            "file.save",
            "file.saveAs",
            "file.saveWithEncoding",
            "file.reopenWithEncoding",
            "-",
            "file.close",
        ],
    ),
    (
        "Edit",
        &[
            "edit.undo",
            "edit.redo",
            "-",
            "edit.cut",
            "edit.copy",
            "edit.paste",
            "edit.selectAll",
            "-",
            "edit.toggleLineComment",
            "edit.toggleBlockComment",
            "-",
            "edit.duplicate",
            "edit.moveLineUp",
            "edit.moveLineDown",
            "edit.deleteLine",
            "edit.joinLines",
            "edit.insertLineBelow",
            "edit.insertLineAbove",
            "-",
            "edit.upperCase",
            "edit.lowerCase",
            "edit.titleCase",
            "edit.sortLines",
            "edit.removeDuplicateLines",
            "edit.trimWhitespace",
            "-",
            "find.open",
            "find.replace",
            "search.inFiles",
            "search.replaceInFiles",
        ],
    ),
    (
        "Selection",
        &[
            "select.line",
            "select.expand",
            "select.shrink",
            "-",
            "cursor.matchingBracket",
            "select.toMatchingBracket",
            "-",
            "selection.addNextOccurrence",
            "selection.addCursorAbove",
            "selection.addCursorBelow",
        ],
    ),
    (
        "View",
        &[
            "app.palette",
            "view.toggleSidebar",
            "view.toggleWrap",
            "view.toggleTheme",
            "view.toggleWhitespace",
            "view.toggleIndentGuides",
            "view.toggleStickyScroll",
            "view.toggleMinimap",
            "view.openPreview",
            "-",
            "view.zoomIn",
            "view.zoomOut",
            "view.resetZoom",
            "-",
            "view.splitRight",
            "view.focusLeftGroup",
            "view.focusRightGroup",
            "tab.moveToNextGroup",
            "view.closeGroup",
            "-",
            "fold.fold",
            "fold.unfold",
            "fold.foldAll",
            "fold.unfoldAll",
            "-",
            "tab.next",
            "tab.previous",
        ],
    ),
    (
        "Help",
        &[
            "app.checkUpdates",
            "app.installUpdate",
            "app.restartToUpdate",
            "app.openRelease",
        ],
    ),
];

/// The tree's context menu (M12), by action id.
pub const TREE_MENU: &[&str] = &[
    "tree.newFile",
    "tree.newFolder",
    "-",
    "tree.rename",
    "tree.delete",
    "tree.undoMove",
    "-",
    "tree.searchInFolder",
    "tree.reveal",
    "-",
    "workspace.addFolder",
    "workspace.removeFolder",
];

#[cfg(target_os = "macos")]
mod native {
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

    use super::LAYOUT;
    use crate::action;
    use crate::keymap::Keymap;

    /// The built menu bar. Keep it alive for as long as the app runs.
    pub struct MenuBar {
        _menu: Menu,
        items: Vec<(&'static str, MenuItem)>,
    }

    impl MenuBar {
        /// Show the keymap's current shortcuts, after the config changed.
        pub fn update(&self, keymap: &Keymap) {
            for (id, item) in &self.items {
                let accel = keymap.accelerator(id).and_then(|s| s.parse().ok());
                if let Err(e) = item.set_accelerator(accel) {
                    log::warn!("menu shortcut for {id}: {e}");
                }
            }
        }
    }

    impl std::fmt::Debug for MenuBar {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MenuBar").finish_non_exhaustive()
        }
    }

    fn item(
        id: &str,
        keymap: &Keymap,
        items: &mut Vec<(&'static str, MenuItem)>,
    ) -> Option<MenuItem> {
        let info = action::by_id(id)?;
        let accel = keymap.accelerator(id).and_then(|s| s.parse().ok());
        let it = MenuItem::with_id(id, info.title, true, accel);
        items.push((info.id, it.clone()));
        Some(it)
    }

    /// Build the menu bar and give it to the app. `on_action` receives the
    /// action id of each chosen item, on muda's thread.
    pub fn install(
        keymap: &Keymap,
        on_action: impl Fn(String) + Send + Sync + 'static,
    ) -> Option<MenuBar> {
        let menu = Menu::new();
        let mut items = Vec::new();
        let quit = item("app.quit", keymap, &mut items)?;
        let app = Submenu::with_items(
            "Nimbus",
            true,
            &[
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::separator(),
                &quit,
            ],
        )
        .ok()?;
        menu.append(&app).ok()?;
        for (title, ids) in LAYOUT {
            let sub = Submenu::new(*title, true);
            for id in *ids {
                let result = if *id == "-" {
                    sub.append(&PredefinedMenuItem::separator())
                } else if let Some(it) = item(id, keymap, &mut items) {
                    sub.append(&it)
                } else {
                    Ok(())
                };
                if let Err(e) = result {
                    log::warn!("menu item {id}: {e}");
                }
            }
            menu.append(&sub).ok()?;
        }
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| on_action(e.id.0)));
        menu.init_for_nsapp();
        Some(MenuBar { _menu: menu, items })
    }
}

#[cfg(target_os = "macos")]
pub use native::{MenuBar, install};

/// No system menu bar here: Windows gets the one Nimbus draws.
#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub struct MenuBar;

#[cfg(not(target_os = "macos"))]
impl MenuBar {
    /// Nothing to update without a system menu bar.
    #[expect(
        clippy::unused_self,
        reason = "same signature as the system menu bar's update"
    )]
    pub fn update(&self, _keymap: &crate::keymap::Keymap) {}
}

/// No system menu bar on this platform.
#[cfg(not(target_os = "macos"))]
pub fn install(
    _keymap: &crate::keymap::Keymap,
    _on_action: impl Fn(String) + Send + Sync + 'static,
) -> Option<MenuBar> {
    None
}
