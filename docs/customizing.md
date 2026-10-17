# Customizing Nimbus

Everything here lives in the config folder, beside `config.toml`:

| System | Folder |
|---|---|
| macOS | `~/Library/Application Support/nimbus/` |
| Linux | `~/.config/nimbus/` |
| Windows | `%APPDATA%\nimbus\` |

"Open Config File" in the command palette opens `config.toml`, creating it
from a commented template. After editing, run "Reload Config", except for
highlight queries, which need a restart. Mistakes show in the message line
and cost only the setting that has them.

To try a config without touching your own, point `NIMBUS_CONFIG_DIR` at
another folder: `NIMBUS_CONFIG_DIR=/tmp/try nimbus file.lua`.

## Themes

```
themes/
  mine.toml
```

Set `theme = "mine"` in `config.toml`. A theme lists only what it changes
from its `base` (`"dark"` unless given). See the built-in
`crates/app/themes/dark.toml` for every name.

```toml
base = "dark"

[ui]
background = "#101418"
cursor = "#ff6ac1"
selection = "#3e4a5c99"     # last two digits: opacity

[syntax]
keyword = "#ff6ac1"
string = "#9ece6a"
"function.method" = "#7dcfff"
```

Syntax keys are tree-sitter capture names. The longest matching prefix
wins: `function` also colours `function.method`, unless that has its own
entry.

A syntax entry can also be bold or italic. Write it as a table; any of the
three settings can be left out (no `color` keeps the text colour):

```toml
[syntax]
keyword = { color = "#ff6ac1", bold = true }
comment = { italic = true }
```

The compare view (M9) has its own `[ui]` keys: `diff_added`,
`diff_removed` and `diff_changed` for the row backgrounds, `diff_word` for
the changed words within a line, `diff_filler` for the rows one side has
no line for, and `merge_conflict` for the merge view.

## Which language a file is

Map file names or `*.ext` patterns in `config.toml`:

```toml
[languages]
"Justfile" = "bash"
"*.jsonc" = "json"
"*.conf" = "lua"          # a language of your own (below)
```

Built-in languages: `rust`, `markdown`, `json`, `toml`, `yaml`, `python`,
`javascript`, `typescript`, `bash`. An exact file name beats a pattern, and
a longer pattern beats a shorter one (`*.d.ts` before `*.ts`). The table
beats everything else. Next come your own languages' extensions, then the
built-in detection (extension, file name, `#!` line).

## Your own languages

```
languages/
  lua.toml
```

A language of your own uses simple rules, coloured line by line: comments,
strings, keywords, numbers and constants. It has no grammar, so it does not
tell functions or types apart, but it costs nothing and works at any file
size.

```toml
extensions = ["lua"]
filenames = []
line_comments = ["--"]
block_comment = ["--[[", "]]"]
nested_comments = false
hash_needs_space = false       # "#" only after a space (shells, YAML)
strings = [
    { open = "[[", close = "]]", escapes = false, multiline = true },
    { open = "\"" },           # close defaults to open, escapes to true
    { open = "'" },
]
keywords = ["and", "do", "else", "end", "for", "function", "if", "local",
            "return", "then", "while"]
constants = ["true", "false", "nil"]
```

Every field is optional. Strings are tried in order, so put longer openers
first (`"""` before `"`). They are coloured with the theme's `comment`,
`string`, `keyword`, `number` and `constant` colours.

A file named after a built-in language (`rust.toml`) is ignored. To change
how a built-in language is coloured, use a query.

## Changing a built-in language's highlighting

```
queries/
  rust.scm
```

A query file replaces the built-in highlight query for that language. If
its first line is `;; extends`, it is added after the built-in one
instead, and later patterns win. For example, to colour `main` as a
keyword:

```scheme
;; extends
((identifier) @keyword (#eq? @keyword "main"))
```

Queries use tree-sitter's query language; the built-in ones are in each
grammar's `queries/highlights.scm`, and Rust's in
`crates/syntax/queries/rust.scm`. A query that does not compile is reported
in the message line, and the built-in one is used. Queries load once, the
first time a file of that language opens, so a change needs a restart.

Adding a new tree-sitter grammar still means adding its crate to
`crates/syntax` and rebuilding (DESIGN.md §4.2).
