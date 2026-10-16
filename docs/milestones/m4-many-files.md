# M4 — Many files

**Status: built on branch `m4-wip` (draft PR #2); owner checks pending** (2026-10-16)

## Goal

Work across a project: tabs, a fuzzy file opener, a command palette that
reaches every action, a config file, and native menus.

## Done when

Ticked items were verified here by tests or headless renders. **Owner**
items need a person at the keyboard; each says what to try.

- [x] Every operation is an `Action` with a stable id, and keys are bound
      only through the registry plus config overrides. A test checks that
      the default bindings have no conflicts on each platform.
- [x] Primary+Shift+P opens the command palette; typing filters by fuzzy
      match; rows show key bindings (headless render).
- [x] Primary+P opens the file opener; results stream in while the walk
      runs; `.gitignore`d files are excluded. First match in about 5 ms on
      a 62,000-file folder.
- [ ] **Owner:** use both pickers by keyboard and mouse: arrows, Enter,
      Escape, clicking a row; clicking outside closes, inside does not.
- [ ] **Owner:** tabs: open several files (`nimbus a.rs b.rs`, Primary+O,
      the file opener); switch with Primary+1..9, Ctrl+Tab, Ctrl+Shift+Tab
      and by clicking; Primary+W closes with a prompt if dirty; Primary+N
      opens an untitled tab; each tab keeps its cursor, scroll and undo.
- [ ] **Owner:** Primary+O and Primary+Shift+S show native dialogs; saving
      an untitled tab asks for a name.
- [ ] **Owner:** "Open Config File" from the palette creates the file from a
      commented template; change `font_size` or a key binding, save, run
      "Reload Config", and see the change (menus too). A typo shows a
      warning in the message line and the other settings still apply.
- [ ] **Owner:** macOS menu bar (Nimbus, File, Edit, View) runs the same
      actions and shows the shortcuts; holding Cmd+W closes one tab, not
      all of them. Windows builds the same menus (CI compiles them); Linux
      has none, by design.
- [ ] **Owner:** startup with several files and a config file stays well
      under 500 ms (`--smoke`; the screen was locked when this was built,
      so it is unmeasured).

## Scope

**In**: action registry, keymap, palette, opener, tabs, dialogs, config,
menus, the overlay list widget.

**Deferred**: file tree (M6), themes beyond font and colours (M5),
recently-opened files (post-M8).

## Design

**Actions** (`app/action.rs`): one enum, one metadata table.

```rust
pub enum Action { Save, Undo, Redo, Find, OpenPalette, OpenFile, CloseTab, GoToTab(u8), /* … */ }

pub struct ActionInfo {
    pub id: &'static str,          // stable, used in config: "edit.undo"
    pub title: &'static str,       // shown in palette and menus: "Undo"
    pub default_keys: &'static [&'static str], // "Primary+Z"
}
```

A test asserts every variant has metadata, ids are unique, and no two
default bindings collide. Parameterised actions such as `GoToTab(n)` get one
metadata entry per bound value.

**Keymap** (`app/keymap.rs`): parses `"Primary+Shift+P"` into a `Chord`
(modifiers plus logical key), resolves `Primary` per platform, and holds
`HashMap<Chord, Action>`. Config overrides replace defaults per action id.
Single chords only; multi-key sequences like `Ctrl+K Ctrl+C` are deferred.

**Overlay list** (`app/widgets/picker.rs`): a `text_input` from M3 over a
scrolling list, used by both palette and opener. Sources implement
`PickerSource` with `items()` and `on_pick()`.

**Fuzzy matching**: `nucleo::Nucleo` holds the item list, accepts new items
from any thread through its injector, and matches incrementally on its own
thread pool. The opener's walker (`ignore::WalkBuilder::build_parallel`)
pushes paths into the injector as it finds them. Each frame calls `tick` with
a 1 ms budget and renders the current snapshot.

**Config** (`app/config.rs`): a `serde` struct with `#[serde(default)]` on
every field so a partial file works. Unknown keys are reported as warnings,
not errors, so a typo is visible but harmless. Parsed on the startup thread
from M1 alongside GPU setup.

**Menus**: `muda` builds the menu; on macOS it attaches to the app with
`init_for_nsapp`, on Windows to the window handle with `init_for_hwnd`. Menu
events arrive on muda's channel; a forwarding thread wakes the event loop
with the proxy. Menu items show the accelerator from the keymap.

**Tabs** (`app/tabs.rs`): `Vec<Document>` plus active index. The tab bar is
one row of rects and text with the dirty dot and a close affordance on
hover. Overflowing tabs scroll horizontally; there is no wrapping.

## Tasks

1. `Action`, `ActionInfo`, metadata tests; convert M2/M3 key handling.
2. `Keymap` with chord parsing and platform resolution.
3. `config.rs` and the "Reload config" action.
4. Picker widget and the command palette.
5. Walker plus nucleo; the file opener.
6. Tabs: model, tab bar, switching and closing actions.
7. Native dialogs.
8. Menus on macOS, then Windows; confirm the Linux build has no muda dependency.
9. Startup timing check with a config file present.

## Concepts

- **Data-driven commands**: why one table of actions gives you the palette,
  menus and rebindable keys for free.
- **Producer and consumer threads**: the walker feeding nucleo while you type.
- **Platform-conditional dependencies**: `[target.'cfg(...)'.dependencies]`.
- **Serde defaults** and forgiving config parsing.

## Risks

| Risk | Fallback |
|---|---|
| muda and winit disagree about the macOS menu or event loop | Build the menu after `resumed`; if still broken, ship palette-only on macOS for now |
| nucleo's API is more than we need | Use `nucleo-matcher` directly with our own filtering on the main thread for under 50k items |
| Too many default bindings collide across platforms | The collision test fails CI; resolve per platform in `default_keys` |

## Results

| Metric | Value |
|---|---|
| Tests | 164 passing, plus 3 opt-in benchmarks or diagnostics |
| File opener on 62,244 files | first match 4.6 ms; walk and match done in 147 ms |
| Menus installed | about 90 ms after start (window ready at about 80 ms) |
| Release binary | 8.7 MB |

**Design changes**

- `main.rs` is startup only; the application moved to `app.rs`, with
  `action`, `keymap`, `config`, `picker` and `menu` modules beside it.
- Actions carry a `When` context. A chord bound to an action that does not
  apply right now types its text instead: Option+C types "ç" unless the
  find bar is open.
- Key lookup tries the logical key first, then the physical key, but only
  when Option/Alt or a non-ASCII layout changed the character. Without that
  limit, Dvorak's Cmd+, would have closed a tab.
- Config overrides always win over defaults, regardless of action order;
  labels and menu shortcuts follow what each action still owns.
- Each config setting is read separately, so one mistake costs one setting.
- Menu shortcuts come from the keymap and update on Reload Config. Held
  menu shortcuts are de-duplicated (macOS sends a menu event per repeat).
- The tab bar is always shown, and scrolls to keep the active tab visible.
- A lone untouched untitled tab is replaced when a file is opened.
- `--picker palette|files[:query]` with `--headless` renders a picker for
  checks without a window.

**Surprises**

- The bundled font has no ⌘ ⌥ ⌃ ⌫ glyphs, so shortcut labels read
  `Cmd+Shift+P` on macOS until M8's font fallback (noted in the M8 plan).
- The screen locked during the work again; `--smoke` now exits once, with
  one message, instead of repeating it while winit shuts down.
- Linux CI caught a method that only the native menus use.

**End-of-milestone review** (code-review skill, high) found ten issues, all
fixed: overrides losing to later defaults; physical-key fallback on
letter-moving layouts; menus not updated on reload; menu dropped after the
window it is attached to (Windows); held menu shortcuts repeating actions;
a continuous redraw during the file walk; one bad config value resetting
everything; modified Backspace and Delete doing nothing; failed Save As
changing the tab's path; clicks inside the picker closing it.

**Known limitations**

- Quitting from the Dock or at logout still skips the unsaved prompt; the
  new menus do not change that (winit has no `applicationShouldTerminate`).
- Multi-key chord sequences (Ctrl+K Ctrl+C) are not supported.
- Recently opened files are not remembered.
