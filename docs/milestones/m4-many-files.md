# M4 — Many files

**Status: next**

## Goal

Work across a project: tabs, a fuzzy file opener, a command palette that
reaches every action, a config file, and native menus.

## Done when

- [ ] Every operation from M2 and M3 is an `Action`, and the keymap is the
      only place keys are bound.
- [ ] `Primary+Shift+P` opens the command palette; typing filters actions by
      fuzzy match; Enter runs one; each row shows its keybinding.
- [ ] `Primary+P` opens the file opener over the project root. Results stream
      in while the walk runs; `.gitignore`d files are excluded; a 100k-file
      repository shows first results in under 100 ms.
- [ ] Opening a second file adds a tab. `Primary+1..9`, `Ctrl+Tab` and
      `Ctrl+Shift+Tab` switch; `Primary+W` closes, prompting if dirty;
      `Primary+T` or `Primary+N` makes an untitled tab.
- [ ] Each tab keeps its own cursor, scroll and undo history.
- [ ] `Primary+O` and `Primary+Shift+S` use native open and save dialogs.
- [ ] The config file is read at startup; font size, tab width, theme name,
      soft-wrap default and keybinding overrides take effect. Editing the
      file and running "Reload config" applies changes. A syntax error shows
      in the message line with the line number; defaults stay in effect.
- [ ] macOS and Windows have a native menu bar (File, Edit, View, Go) whose
      items run the same actions. Linux has none, by design.
- [ ] Startup stays under 100 ms: the walk, menu build and config parse do
      not delay the first frame.

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

_Fill in when done._
