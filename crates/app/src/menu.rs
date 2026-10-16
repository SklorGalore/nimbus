//! Native menu bar on macOS and Windows (DESIGN.md §4.4). Linux has none:
//! `muda` needs GTK there, which Nimbus avoids; the command palette covers
//! every action instead.
//!
//! Menu items are actions from the registry. Their shortcuts come from the
//! keymap, so a rebinding in the config file shows in the menus too. On
//! macOS a menu shortcut takes the key press before the window sees it, and
//! the action arrives as a menu event instead; either way it runs the same
//! command.

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native {
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use winit::window::Window;

    use crate::action;
    use crate::keymap::Keymap;

    /// Menus by title, each a list of action ids; `"-"` is a separator.
    const LAYOUT: &[(&str, &[&str])] = &[
        (
            "File",
            &[
                "file.new",
                "file.open",
                "file.quickOpen",
                "-",
                "file.save",
                "file.saveAs",
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
                "find.open",
                "find.replace",
            ],
        ),
        (
            "View",
            &[
                "app.palette",
                "view.toggleWrap",
                "-",
                "tab.next",
                "tab.previous",
            ],
        ),
    ];

    /// The built menu bar. Keep it alive for as long as the window.
    pub struct MenuBar {
        _menu: Menu,
    }

    impl std::fmt::Debug for MenuBar {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MenuBar").finish_non_exhaustive()
        }
    }

    fn item(id: &str, keymap: &Keymap) -> Option<MenuItem> {
        let info = action::by_id(id)?;
        let accel = keymap.accelerator(id).and_then(|s| s.parse().ok());
        Some(MenuItem::with_id(id, info.title, true, accel))
    }

    /// Build the menu bar and attach it. `on_action` receives the action id
    /// of each chosen item, on muda's thread.
    pub fn install(
        window: &Window,
        keymap: &Keymap,
        on_action: impl Fn(String) + Send + Sync + 'static,
    ) -> Option<MenuBar> {
        let menu = Menu::new();
        #[cfg(target_os = "macos")]
        {
            let quit = item("app.quit", keymap)?;
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
        }
        for (title, ids) in LAYOUT {
            let sub = Submenu::new(*title, true);
            for id in *ids {
                let result = if *id == "-" {
                    sub.append(&PredefinedMenuItem::separator())
                } else if let Some(it) = item(id, keymap) {
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
        attach(&menu, window)?;
        Some(MenuBar { _menu: menu })
    }

    #[cfg(target_os = "macos")]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "same signature as the Windows version, which can fail"
    )]
    fn attach(menu: &Menu, _window: &Window) -> Option<()> {
        menu.init_for_nsapp();
        Some(())
    }

    #[cfg(target_os = "windows")]
    #[expect(unsafe_code, reason = "muda attaches to a window through its raw HWND")]
    fn attach(menu: &Menu, window: &Window) -> Option<()> {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let RawWindowHandle::Win32(handle) = window.window_handle().ok()?.as_raw() else {
            return None;
        };
        // SAFETY: the handle comes from a live winit window that outlives
        // the menu (both are owned by the app until exit), which is what
        // `init_for_hwnd` requires.
        unsafe { menu.init_for_hwnd(handle.hwnd.get()) }.ok()
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::{MenuBar, install};

/// No native menus on this platform.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Debug)]
pub struct MenuBar;

/// No native menus on this platform; the command palette covers them.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn install(
    _window: &winit::window::Window,
    _keymap: &crate::keymap::Keymap,
    _on_action: impl Fn(String) + Send + Sync + 'static,
) -> Option<MenuBar> {
    None
}
