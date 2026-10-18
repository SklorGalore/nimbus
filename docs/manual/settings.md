# Settings

## The config folder

Nimbus keeps its settings in one folder:

| System | Folder |
|---|---|
| macOS | `~/Library/Application Support/nimbus/` |
| Linux | `~/.config/nimbus/` (or `$XDG_CONFIG_HOME/nimbus/`) |
| Windows | `%APPDATA%\nimbus\` |

What can be in it:

| File or folder | What it is | Written by |
|---|---|---|
| `config.toml` | Your settings. | You. Nimbus creates it once from a template when you run "Open Config File", and never writes to it after that. |
| `state.toml` | What Nimbus remembers between runs: sessions, recent files, the window and sidebar. | Nimbus. |
| `themes/NAME.toml` | Your colour themes. | You. See [customizing](customizing.md#themes). |
| `languages/NAME.toml` | Your own simple languages. | You. See [customizing](customizing.md#your-own-languages). |
| `queries/` | Changes to how built-in languages are highlighted, folded and listed in Go to Symbol. | You. See [customizing](customizing.md#changing-a-built-in-languages-highlighting). |

To try settings without touching your own, point the `NIMBUS_CONFIG_DIR`
environment variable at another folder; Nimbus then uses that folder for
everything above:

```sh
NIMBUS_CONFIG_DIR=/tmp/try nimbus notes.md
```

## Editing config.toml

Run **Open Config File** from the command palette. The first time, it
creates `config.toml` with every setting commented out, and opens it.
Edit, save, then run **Reload Config**. Nimbus never needs a restart for
settings, except for query files.

Every setting is optional. A mistake (a misspelt name, a wrong type, a
value out of range) shows as a warning in the message line and costs
only that one setting: the rest still apply, and the faulty one keeps
its default. If the file is not valid TOML at all, every setting takes
its default.

The file uses [TOML](https://toml.io): `name = value` lines at the top,
then `[tables]`.

## Every setting

### Top level

| Setting | Type | Default | Meaning |
|---|---|---|---|
| `font_size` | number, 6 to 72 | `14.0` | Text size in points. |
| `tab_width` | whole number, 1 to 16 | `4` | How many columns a tab character is drawn as. This does not decide what the Tab key inserts; see `[indent]`. |
| `soft_wrap` | `true` or `false` | `false` | Whether new tabs start with soft wrap on. Languages can override it with `wrap` in `[editor.LANGUAGE]`; Markdown wraps unless told not to. |
| `theme` | text | `"dark"` | `"dark"`, `"light"`, or the name of your own theme in `themes/NAME.toml`. An unknown name falls back to dark with a warning. |
| `check_updates` | `true` or `false` | `false` | Ask GitHub once at each start whether a newer release exists. Off by default, so Nimbus never contacts the network unless you ask. See [Updates](troubleshooting.md#checking-for-updates). |
| `font` | text | `""` (empty) | The editor font: an installed font's name (`"JetBrains Mono"`) or a font file's path (`.ttf`, `.otf`, `.ttc`). Empty is the bundled Hack Nerd Font Mono. It loads just after the window appears; a name that is not found keeps Hack and says so. |
| `ligatures` | `true` or `false` | `false` | Join `->`, `!=`, `=>` and friends into one glyph, in a font that has ligatures (Hack has none). Columns stay the same, and a ligature splits apart where the cursor or a selection edge falls inside it. |
| `remember_zoom` | `true` or `false` | `true` | Keep the size Zoom In and Zoom Out left (in `state.toml`) for the next start. `false` starts every session at `font_size`. Changing `font_size` in the file always wins. |
| `render_whitespace` | `"selection"`, `"all"` or `"none"` | `"selection"` | Draw spaces as `·` and tabs as `→`: only inside a selection, everywhere, or never. Toggle Render Whitespace in the palette cycles it for the session. |
| `indent_guides` | `true` or `false` | `false` | Draw a thin line at each indentation level, the one around the cursor brighter. Toggle Indent Guides in the palette switches it for the session. |

### `[keys]`

Changes key bindings. See [Key bindings](#key-bindings) below.

```toml
[keys]
"view.toggleWrap" = ["Alt+W"]
"file.quickOpen" = ["Primary+P", "Primary+E"]
"app.quit" = []
```

### `[languages]`

Maps a file name, or a `*.ext` pattern, to a language name: a built-in
one (`rust`, `markdown`, `json`, `toml`, `yaml`, `python`, `javascript`,
`typescript`, `tsx`, `bash`, `c`, `go`, `html`, `css`) or one of your
own. See [Languages](languages.md#how-a-files-language-is-chosen).

```toml
[languages]
"Justfile" = "bash"
"*.jsonc" = "json"
"*.conf" = "lua"       # a language of your own
```

### `[encodings]`

Maps a file name or pattern to the encoding such files are read and
saved in, skipping Nimbus's guess. A pattern without a `/` is matched
against the file name. One with a `/` is matched against the file's
whole path, usually a full path, so start it with `**/`. Encoding names
are listed in [Encodings](encodings.md#encoding-names).

```toml
[encodings]
"*.csv" = "windows-1252"
"**/legacy/*.txt" = "latin1"
"export.dat" = "utf-16le"
```

The table applies to files opened after Reload Config; tabs already
open keep their encoding (use Reopen with Encoding for those). An
unknown encoding name or a broken pattern is reported in the message
line and that entry is skipped. A file of 256 MB or more given an
encoding other than UTF-8 here opens read-only (see
[Encodings](encodings.md#huge-files-in-another-encoding)).

### `[editor]`

Editing behaviour. Each of these can also be set for one language in a
table named after it, such as `[editor.markdown]` or `[editor.lua]`; the
language's table wins over `[editor]`, which wins over the default.

| Setting | Type | Default | Meaning |
|---|---|---|---|
| `auto_pairs` | `true` or `false` | on for code, off for Markdown and plain text | Typing `(`, `[`, `{`, `"`, `'` or a backtick inserts its closer (see [Editing](editing.md#typing-and-deleting)). |
| `trim_trailing_whitespace` | `true` or `false` | `false` | Remove spaces and tabs at the ends of lines when saving. |
| `final_newline` | `true` or `false` | `false` | Make sure the file ends with a line break when saving. |
| `wrap` | `true` or `false` | on for Markdown, otherwise the top-level `soft_wrap` | Open files of this language with soft wrap on. |

Both save settings are off by default so that a file you did not edit
saves back byte for byte. Neither applies to huge memory-mapped files.
Toggle Soft Wrap still changes one tab at a time.

```toml
[editor]
trim_trailing_whitespace = true

[editor.markdown]
wrap = false
auto_pairs = true
```

### `[indent]`

How Tab, Shift+Tab and Enter indent. Each can also be set per language
in `[indent.LANGUAGE]`, such as `[indent.python]`, or
`[indent.makefile]` for a language of your own.

| Setting | Type | Default | Meaning |
|---|---|---|---|
| `width` | whole number, 1 to 16 | `4` | Columns per indentation level. |
| `tabs` | `true` or `false` | `false` | Indent with a tab character rather than spaces. |
| `detect` | `true` or `false` | `true` | Let the file's own indentation win over `width` and `tabs`. Nimbus looks at the first 4,000 lines when the file opens. |

With `detect = true`, `width` and `tabs` apply only to files that do not
show their own indentation, such as new files. The status bar shows the
result for each file, and clicking it changes it (see
[Editing](editing.md#indentation)).

```toml
[indent]
width = 2

[indent.python]
width = 4

[indent.go]
tabs = true
detect = false
```

## The template

This is the file "Open Config File" writes:

```toml
# Nimbus configuration. Every setting is optional.

# font_size = 14.0
# tab_width = 4
# soft_wrap = false
# theme = "dark"            # or "light", or NAME for themes/NAME.toml here
# check_updates = false     # true: ask GitHub for a newer release at startup

# The editor font: an installed font's name or a font file's path. Empty is
# the bundled Hack Nerd Font Mono. ligatures joins -> != => and friends in
# fonts that have them (Fira Code, JetBrains Mono, Cascadia Code).
# font = "JetBrains Mono"
# ligatures = false
# Zoom (View > Zoom In/Out, Primary+= and Primary+-) is remembered between
# sessions; false starts every session at font_size.
# remember_zoom = true
# Show spaces as · and tabs as →: "selection", "all" or "none".
# render_whitespace = "selection"
# indent_guides = false     # true: a hairline at each indentation level

# Key bindings: action id = list of chords. "Primary" is Cmd on macOS and
# Ctrl elsewhere. An empty list unbinds the action. The command palette
# (Primary+Shift+P) shows every action's name.
# [keys]
# "view.toggleWrap" = ["Alt+W"]
# "file.quickOpen" = ["Primary+P", "Primary+E"]

# Languages: a file name or "*.ext" pattern = language. Built-in languages
# are rust, markdown, json, toml, yaml, python, javascript, typescript and
# bash. Add your own as languages/NAME.toml here (simple colouring rules),
# and change a built-in's colours with queries/NAME.scm (restart to apply).
# [languages]
# "Justfile" = "bash"
# "*.jsonc" = "json"

# Editing. auto_pairs: typing ( [ { " ' or ` inserts the closer; on for
# code and off for Markdown and plain text unless set. The two save
# settings are off so files round-trip byte for byte. wrap: soft wrap for
# files of a language; Markdown wraps unless told not to. A language table
# ([editor.markdown], [editor.lua]) overrides any of the four.
# [editor]
# auto_pairs = true
# trim_trailing_whitespace = false
# final_newline = false
# [editor.markdown]
# wrap = false

# Indentation. With detect = true (the default) the file's own
# indentation wins over width and tabs. [indent.python] and the like
# override per language.
# [indent]
# width = 4
# tabs = false
# detect = true
```

The template's list of built-in languages is out of date: `tsx`, `c`,
`go`, `html` and `css` work too.

## Key bindings

Every command has an id, such as `edit.toggleLineComment`; the full list
with the default keys is in
[Keyboard shortcuts and commands](keybindings.md). In the `[keys]` table,
give a command's id and the list of keys you want for it:

```toml
[keys]
"edit.toggleLineComment" = ["Primary+/", "Ctrl+K Ctrl+C"]
"view.toggleWrap" = ["Alt+W"]
"edit.moveLineUp" = ["Primary+Shift+Up"]
"app.quit" = []
```

- The list **replaces** the command's default keys. An empty list leaves
  the command with no key (it stays in the palette and menus).
- If a key you assign already belongs to another command, it moves to
  yours, and the message line says so after Reload Config.
- The menus show your keys after Reload Config.

### Writing a key

A key is written as modifiers and a key joined by `+`, such as
`Primary+Shift+P`.

| Modifier | Means |
|---|---|
| `Primary` | `Cmd` on macOS, `Ctrl` on Windows and Linux. Use it for keys that should feel native on both. |
| `Ctrl` (or `Control`) | The Control key on every platform. |
| `Alt` (or `Option`) | Alt, or Option on macOS. |
| `Shift` | Shift. |
| `Meta` (or `Cmd`, `Command`, `Super`, `Win`) | Command on macOS, the Windows or Super key elsewhere. |

The key is a single character (`P`, `/`, `[`, `1`; letters are not case
sensitive, and `Primary++` means the plus key) or one of these names:
`Left`, `Right`, `Up`, `Down`, `Home`, `End`, `PageUp`, `PageDown`,
`Enter`, `Tab`, `Escape`, `Backspace`, `Delete`, `Space`, `F1` to `F12`.

Keys are matched by the character your keyboard layout produces, and
fall back to the key's position on a US keyboard when a modifier changes
the character, so `Alt+Z` still works on a Mac where Option+Z types `Ω`.

### Two-key shortcuts

A binding may be two keys separated by a space, such as `"Ctrl+K
Ctrl+C"`: press the first, let go, then press the second. After the
first, the status bar shows it and waits two seconds for the second;
Escape, or any key that does not complete a binding, cancels.

The defaults use this for folding: `Primary+K` followed by `Primary+0`,
`Primary+J`, `Primary+L` or `Primary+1` to `3`. A key cannot both do
something on its own and start a two-key shortcut, so binding
`Primary+K` alone would clash with those; Reload Config reports such a
clash in the message line.

### Keys only the find bar uses

Match case (`Alt+C`), Regular expression (`Alt+R`) and Replace All
(`Primary+Enter`) apply only while the find bar is open. While it is
closed, `Alt+C` and `Alt+R` type their usual characters, and
`Primary+Enter` is Insert Line Below.

## state.toml

`state.toml`, beside `config.toml`, is where Nimbus remembers things by
itself. It holds only paths, positions and sizes, never any text from
your files, so it stays small. It is written as you open and close
tabs and when Nimbus quits, through a temporary file and a rename like
any save. It is not written by `--smoke`, `--screenshot`, `--headless`
or `--bench-typing` runs.

| Key | Meaning |
|---|---|
| `sidebar_visible` | Whether the sidebar was shown (default `true`). |
| `sidebar_width` | The sidebar's width in points (default `240`, at least `120`). |
| `[window]` | The window's size and position in points, the name of the display it was on, and whether it was maximized. |
| `[[recent]]` | The files opened lately, most recent first, at most 100. |
| `[[sessions]]` | One session per project folder, most recent first, at most 20: the folder, the sidebar mode, the folders open in the tree, the editor groups' widths, and each tab's file, cursor and scroll position (as byte offsets), its group, whether it was in front, and its folds with the file's size and time when they were saved. |

You can delete it at any time to go back to the defaults: Nimbus forgets
its sessions, recent files, window position and sidebar size. A broken
file is treated the same way. Nimbus keeps all this separate so that it
never has to rewrite your hand-edited `config.toml`.
