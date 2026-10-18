# Themes, your own languages and queries

This chapter covers the files you can add to the config folder, beside
`config.toml`, to change how Nimbus looks and which languages it knows.
[Settings](settings.md) says where the config folder is and covers
`config.toml` itself, including key bindings.

After adding or changing one of these files, run "Reload Config" from
the palette, except for query files, which need a restart. Mistakes show
in the message line and cost only the setting or entry that has them.

## Themes

<img src="../../assets/screenshots/light.png" alt="Nimbus with the built-in light theme" width="800">

```
themes/
  mine.toml
```

Set `theme = "mine"` in `config.toml`. A theme's name may use letters,
digits, `-` and `_`. A theme lists only what it changes from its `base`,
which must be one of the built-in themes, `"dark"` (the default) or
`"light"`. The built-in dark theme, with every name you can set, is
`crates/app/themes/dark.toml` in the repository; copying it is a good
start.

Colours are `"#rrggbb"`, or `"#rrggbbaa"` where the last two digits are
the opacity.

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

The `[ui]` keys are:

| Key | Colours |
|---|---|
| `background`, `text` | the text area and its text |
| `gutter`, `current_line_number` | line numbers and other secondary text, and the cursor's line number |
| `cursor`, `preedit` | the cursor, and the underline of text being composed with an input method |
| `selection`, `bracket` | behind selected text, and behind the bracket at the cursor and its partner |
| `scrollbar` | the scrollbar thumb |
| `find_match`, `current_match` | find bar matches, and the current one |
| `bar`, `field`, `field_focus`, `toggle_on`, `error` | the find bar and message line, text fields, the border of the focused field, an option toggle that is on, and error messages |
| `tab_bar`, `tab_active`, `tab_accent` | the tab strip, the active tab, and its accent line |
| `picker`, `picker_border`, `picker_selected` | the palette and other pickers |
| `search_match`, `search_file` | the matched text in Find in Files results, and each file's row there |
| `diff_added`, `diff_removed`, `diff_changed`, `diff_word`, `diff_filler` | compare rows that were added, removed or changed, the changed words within a line, and rows one side has no line for |
| `merge_conflict` | conflicts in the merge view |
| `fold_chip` | behind the `… 12 lines` chip after a folded line |

"Toggle Light/Dark Theme" in the palette switches between the two
built-in themes for the current session; it does not change your config.

Markdown prose uses `text.title` for headings, `text.emphasis` (italic in
the built-in themes) and `text.strong` (bold), `text.literal` for code
spans, `text.uri` and `text.reference` for links. Code inside a fenced
block is coloured by that language's own captures, so `keyword` colours a
Rust keyword in a Markdown file too.

## Which language a file is

Map file names or `*.ext` patterns in `config.toml`:

```toml
[languages]
"Justfile" = "bash"
"*.jsonc" = "json"
"*.conf" = "lua"          # a language of your own (below)
```

Built-in languages: `rust`, `markdown`, `json`, `toml`, `yaml`, `python`,
`javascript`, `typescript`, `tsx`, `bash`, `c`, `go`, `html`, `css`. An
exact file name beats a pattern, and a longer pattern beats a shorter one
(`*.d.ts` before `*.ts`). The table beats everything else. Next come your
own languages' extensions, then the built-in detection (extension, file
name, `#!` line); see [Languages](languages.md) for that list.

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
`string`, `keyword`, `number` and `constant` colours. The comment
markers are also what Toggle Line Comment and Toggle Block Comment use,
and the language's name (the file name without `.toml`) is what
`[editor.NAME]` and `[indent.NAME]` in `config.toml` refer to.

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

Markdown has two grammars: `markdown.scm` is the block grammar (headings,
lists, fences) and `markdown_inline.scm` the inline one (emphasis, code
spans, links) that runs inside each paragraph.

### The other query kinds

Each language can have three more files beside its highlight query, all
with the same `;; extends` rule:

```
queries/
  rust.scm                 highlighting
  rust.injections.scm      which parts are another language
  rust.tags.scm            what "Go to Symbol" lists
  rust.folds.scm           what can be folded
```

**Injections** name regions that belong to another language. Markdown's
built-in query sends each paragraph to the inline grammar and each fenced
block to the language its fence names (`rust`, `py`, `sh`, `js`, `ts`,
`tsx`, `c`, `go`, `html`, `css`, `json`, `toml`, `yaml`, `markdown` and
their usual aliases; a name Nimbus has no grammar for is left in the
host's colours). HTML sends `<script>` to JavaScript and `<style>` to CSS.
A pattern captures the region as `@injection.content` and gives the
language either as `@injection.language` (a node whose text is the name)
or fixed, with `(#set! injection.language "css")`. Rust ships no
injections by default: parsing every macro argument as a second Rust
tree costs frames for no visible gain, so `rust.injections.scm` would
start from nothing.

**Tags** (Go to Symbol, `Cmd+Shift+R` / `Ctrl+Shift+R`) list definitions: a pattern
captures the definition node as `@definition.function`, `.method`,
`.class`, `.type`, `.interface`, `.module`, `.macro` or `.constant`, and
the name as `@name`. Markdown's built-in query captures headings. For
example, to list Rust `static` items:

```scheme
;; extends
(static_item name: (identifier) @name) @definition.constant
```

**Folds** mark the nodes a fold can hide, as `@fold`; only nodes spanning
more than one line count. The built-in files are in
`crates/syntax/queries/folds/`.

Adding a whole new tree-sitter grammar means building Nimbus from
source with that grammar added; for anything else, a language of your
own (above) is the way.
