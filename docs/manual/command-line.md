# Command line

```
nimbus [FILE | FOLDER]... [OPTIONS]
```

Files and folders may come in any order, mixed with options. Each file
opens in a tab; a file that does not exist opens empty and is created
when you save. The first folder becomes the project folder (see
[Getting started](getting-started.md#opening-files-and-folders)).

What you name also decides whether Nimbus uses a
[session](getting-started.md#sessions-picking-up-where-you-left-off):

| You run | Nimbus opens | Session |
|---|---|---|
| `nimbus FOLDER` | That folder as the project, with the tabs it had last time. | Restored and saved. |
| `nimbus` | The last project you worked in, with its tabs. With no earlier session (or if its folder is gone), the current folder. | Restored and saved. |
| `nimbus FILE...` (with or without a folder), `--diff`, `--merge` | Just those files. | Left alone. |
| `nimbus --no-session` | An empty untitled tab, with the current folder (or the folder you name) as the project. | Neither restored nor saved. |

The testing options below (`--headless`, `--screenshot`, `--smoke`,
`--bench-typing`) never save anything Nimbus remembers, so measuring
does not overwrite your session.

## Options

| Option | What it does |
|---|---|
| `--no-session` | Start without reopening the last session, and do not save this run as one. |
| `--find PATTERN` | Open with the find bar searching the first tab for `PATTERN`. |
| `--search PATTERN` | Open with the sidebar's Search panel searching the project folder for `PATTERN`. |
| `--wrap` | Start every tab with soft wrap on, whatever the config says. |
| `--theme NAME` | Use this theme instead of the config's `theme`: `dark`, `light`, or one of your own. |
| `--diff OLD NEW` | Open `NEW` in a tab, compared with `OLD` (see [Compare and merge](compare-and-merge.md#comparing)). |
| `--merge BASE LOCAL REMOTE` | Open a three-way merge of the three files. The result saves to `LOCAL` unless `-o` says otherwise. The exit code is 0 only if the result was saved with no conflicts left, 1 otherwise. |
| `-o FILE`, `--output FILE` | With `--merge`: where the result saves. |
| `--version`, `-V` | Print the version (`nimbus 0.5.0`) and exit. |
| `--help`, `-h` | Print a one-line usage summary and exit. |

An unknown option, or an option missing its value, prints an error and
exits with code 2. A file that cannot be opened (a device, a file you
may not read) prints the reason, shows it in a dialog, and exits with
code 1.

## Examples

```sh
nimbus .                                  # this folder, with its tabs from last time
nimbus src/main.rs --find TODO            # open a file, searching for TODO
nimbus . --search 'fn \w+_test'           # search the project on start
nimbus --diff old/config.toml config.toml # what changed?
nimbus --merge base.rs mine.rs theirs.rs -o merged.rs; echo $?
```

## For testing and measuring

These options exist to check rendering and speed, mostly for working on
Nimbus itself. They are listed so that you recognise them; you do not
need them for everyday use.

| Option | What it does |
|---|---|
| `--headless WxH` | Render without a window at this size in pixels, for example `1600x900`. Use with `--screenshot`. Works with the screen locked. |
| `--screenshot FILE.bmp` | Save the first frame as a BMP image and exit. |
| `--smoke` | Open the window, print the time to the first frame (`first-frame-ms=…`) and exit. |
| `--bench-typing N` | Type `N` characters through the normal editing path, print keystroke latency figures and exit. |
| `--backend rope\|mapped` | Hold every file in memory (`rope`) or memory-mapped (`mapped`), whatever its size. |
| `--wait-index` | With `--headless`: count all of a huge file's lines before drawing. |
| `--debug-overlay` | Start with the debug overlay (`F12`) showing. |
| `--cursors N,N,…` | With `--headless`: put a cursor at each byte offset; the last is the main one. |
| `--picker KIND[:QUERY]` | With `--headless`: draw an open picker, `palette`, `files`, `line` or `symbols`, optionally with a query. |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Normal exit. With `--merge`: the result was saved with no conflicts left. |
| 1 | A file could not be opened; with `--headless`, the GPU could not start; with `--screenshot` or `--smoke`, no frame could be drawn or saved; with `--merge`, the result was not saved or still had conflicts. |
| 2 | A mistake on the command line. |

## Environment variables

| Variable | Effect |
|---|---|
| `NIMBUS_CONFIG_DIR` | Use this folder instead of the usual config folder, for `config.toml`, `state.toml`, themes, languages and queries. Handy for trying settings without touching your own. |
| `RUST_LOG` | How much Nimbus logs to the terminal. The default is `nimbus=info`; `RUST_LOG=nimbus=debug` adds detail such as startup times. |
| `WGPU_BACKEND` | Which graphics API to use, such as `vulkan`, `gl`, `metal` or `dx12`. Useful if Nimbus fails to start on your graphics driver. |
