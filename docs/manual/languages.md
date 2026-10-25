# Languages and highlighting

Nimbus highlights fifteen languages with full grammars (tree-sitter),
and any language you describe yourself with simple rules. The status bar
shows the language of the active tab.

## Built-in languages

<img src="../../assets/screenshots/languages.png" alt="Python, shell, TOML and YAML files, each highlighted" width="900">

| Language | Name in config | Chosen for |
|---|---|---|
| Rust | `rust` | `.rs` |
| Markdown | `markdown` | `.md`, `.markdown`, `.mdown`, `.mkd` |
| JSON | `json` | `.json`, `.jsonc`, `.json5`, `.geojson` |
| TOML | `toml` | `.toml`, and `Cargo.lock`, `Pipfile`, `poetry.lock`, `uv.lock` |
| YAML | `yaml` | `.yaml`, `.yml` |
| Python | `python` | `.py`, `.pyi`, `.pyw`; `#!` lines naming `python` or `pypy` |
| JavaScript (with JSX) | `javascript` | `.js`, `.mjs`, `.cjs`, `.jsx`; `#!` lines naming `node`, `nodejs` or `bun` |
| TypeScript | `typescript` | `.ts`, `.mts`, `.cts`; `#!` lines naming `deno`, `ts-node` or `tsx` |
| TSX | `tsx` | `.tsx` |
| Shell | `bash` | `.sh`, `.bash`, `.zsh`, `.ksh`, and `.bashrc`, `.bash_profile`, `.profile`, `.zshrc`, `.zprofile`, `.envrc`; `#!` lines naming `sh`, `bash`, `zsh`, `dash` or `ksh` |
| C | `c` | `.c`, `.h` |
| Go | `go` | `.go` |
| HTML | `html` | `.html`, `.htm`, `.xhtml` |
| CSS | `css` | `.css` |
| PowerShell | `powershell` | `.ps1`, `.psm1`, `.psd1`; `#!` lines naming `pwsh` or `powershell` |

Extensions are matched without regard to case. A file that matches
nothing is plain text, with no colours. The *name in config* is what you
write in the `[languages]` table, in per-language settings such as
`[editor.python]`, and in query file names.

## How a file's language is chosen

In order, the first that says wins:

1. The `[languages]` table in `config.toml`, which maps a file name or a
   `*.ext` pattern to a language: `"Justfile" = "bash"`. An exact file
   name beats a pattern, and a longer pattern beats a shorter one
   (`*.d.ts` before `*.ts`).
2. Your own languages' extensions and file names (see
   [customizing](customizing.md#your-own-languages)).
3. The built-in detection above: extension or file name, then the `#!`
   line at the top of the file.

Changes to the table apply to open files after "Reload Config".

## How highlighting behaves

- Colours come from the theme (see [customizing](customizing.md#themes)).
- Parsing happens in the background. A large file may show uncoloured
  for a moment after it opens; a 10 MB file takes about half a second.
- Files over 16 MB, and files in your own languages, get simpler
  line-by-line colours: comments, strings, keywords, numbers and
  constants.
- **Code inside other code** is coloured as its own language: fenced
  code blocks in Markdown (by the language named after the fence: `rust`,
  `py`, `sh`, `js`, `ts`, `tsx`, `c`, `go`, `html`, `css`, `json`,
  `toml`, `yaml`, `markdown`, `powershell` (or `pwsh`, `ps1`) and their
  usual aliases), and `<script>` and
  `<style>` in HTML. A fence naming a language Nimbus has no grammar for
  keeps the Markdown colours.
- **Markdown** prose is coloured too: headings, emphasis, bold, code
  spans and links.

## What the syntax tree adds

When a file has a full grammar and is up to 16 MB, several features use
its syntax tree:

- **Expand Selection** grows through the code's structure (see
  [Editing](editing.md#selecting)).
- **Bracket matching** ignores brackets inside strings and comments.
- **Folding** follows the language's blocks, functions and sections (see
  [Navigating](navigation.md#folding)).
- **Go to Symbol** lists the file's definitions, for Rust, Markdown,
  Python, JavaScript, TypeScript, TSX, C, Go and PowerShell.

## Markdown helpers

- **Enter** on a list item continues the list on the next line: `-`,
  `*`, `+`, numbered items (counting up) and `[ ]` task boxes (the new
  one unticked). A `>` quote continues too. Enter on an item with nothing
  after its marker ends the list.
- **Tab** and **Shift+Tab** indent and outdent the list item under the
  cursor.
- Markdown files open with **soft wrap** on, unless you set
  `wrap = false` in `[editor.markdown]`.
- **Auto-pairs** are off in Markdown unless you turn them on.
- Comments use `<!-- -->`.
- **Open Preview** (`Cmd+Shift+V` / `Ctrl+Shift+V`) shows the file as a
  formatted page beside it. See
  [The Markdown preview](window.md#the-markdown-preview).
