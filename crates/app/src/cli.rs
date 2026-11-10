//! What `nimbus --help` prints, and the hints a command line mistake gets.
//!
//! The help is built from one table, [`SECTIONS`], rather than written out
//! by hand, so every option is listed once, in a group, with its value and
//! a short description. The parser in `main.rs` stays a plain `match` on
//! the option's name; a test below reads that `match` and fails if it
//! accepts an option the table does not list, so `--help` cannot fall
//! behind the parser again.
//!
//! The text fits an 80-column terminal: names on the left, descriptions
//! lined up in a second column and wrapped inside it. Headings are bold
//! when the output is a terminal that understands the escape code for it,
//! and plain otherwise (see [`styled`]).

use std::io::IsTerminal;

/// One option: what to type, and what it does.
struct Opt {
    /// The option's names and value, as typed: `-o, --output FILE`.
    usage: &'static str,
    /// One sentence, no full stop; wrapped to fit.
    help: &'static str,
}

/// A group of options under a heading.
struct Section {
    /// The heading, such as `Opening`.
    title: &'static str,
    /// Shown only by `--help all`: options for working on Nimbus itself.
    testing: bool,
    /// The options, in the order they are listed.
    opts: &'static [Opt],
}

/// Shorthand for the table below.
const fn opt(usage: &'static str, help: &'static str) -> Opt {
    Opt { usage, help }
}

/// Every option Nimbus accepts, grouped as `--help` lists them.
const SECTIONS: &[Section] = &[
    Section {
        title: "Opening",
        testing: false,
        opts: &[
            opt("--workspace NAME", "Open the workspace saved as NAME"),
            opt(
                "--no-session",
                "Start fresh and remember nothing from this run",
            ),
            opt(
                "--wait",
                "Return when the files' tabs close (for git and $EDITOR)",
            ),
            opt(
                "--encoding NAME",
                "Read and save files in this encoding (utf-8, latin1, ...)",
            ),
        ],
    },
    Section {
        title: "On start",
        testing: false,
        opts: &[
            opt("--find PATTERN", "Search the first tab for PATTERN"),
            opt("--search PATTERN", "Search the project folder for PATTERN"),
            opt("--wrap", "Turn soft wrap on in every tab"),
            opt(
                "--theme NAME",
                "Use this theme (dark, light, or one of your own)",
            ),
            opt("--compat", "Draw on the CPU instead of the graphics card"),
        ],
    },
    Section {
        title: "Compare and merge",
        testing: false,
        opts: &[
            opt("--diff OLD NEW", "Compare two files, or two folders"),
            opt(
                "--merge BASE LOCAL REMOTE",
                "Three-way merge; saves to LOCAL unless -o says otherwise",
            ),
            opt("-o, --output FILE", "Where --merge saves its result"),
        ],
    },
    Section {
        title: "Git",
        testing: false,
        opts: &[opt(
            "--git [DIR]",
            "Open the git window for DIR's repository",
        )],
    },
    Section {
        title: "Testing and measuring",
        testing: true,
        opts: &[
            opt(
                "--headless WxH",
                "Render without a window at this size, such as 1600x900",
            ),
            opt(
                "--screenshot FILE.bmp",
                "Save the first frame as a BMP image and exit",
            ),
            opt("--smoke", "Print the time to the first frame and exit"),
            opt(
                "--bench-typing N",
                "Type N characters and print keystroke latency",
            ),
            opt(
                "--soak",
                "With --bench-typing: also undo, find, save; check memory",
            ),
            opt(
                "--backend rope|mapped",
                "Keep files in memory, or memory-mapped, at any size",
            ),
            opt(
                "--wait-index",
                "With --headless: count a huge file's lines before drawing",
            ),
            opt(
                "--debug-overlay",
                "Start with the F12 debug overlay showing",
            ),
            opt(
                "--cursors N,N",
                "With --headless: a cursor at each byte offset",
            ),
            opt(
                "--preview",
                "With --headless: draw the Markdown preview, not the text",
            ),
            opt(
                "--picker KIND[:QUERY]",
                "With --headless: show a palette|files|line|symbols picker",
            ),
            opt(
                "--prompt",
                "With --headless: draw the \"Save changes?\" question",
            ),
            opt(
                "--title-bar",
                "With --headless: draw the macOS title bar, on any system",
            ),
            opt(
                "--menu NAME",
                "With --headless: draw the Windows menu bar, NAME open",
            ),
            opt(
                "--git-select WHAT",
                "With --git: select changes, or a commit by hash prefix",
            ),
            opt("--linear-blend", "Blend text in linear light (experiment)"),
        ],
    },
    Section {
        title: "Other",
        testing: false,
        opts: &[
            opt(
                "-h, --help",
                "Show this help (--help all adds the testing options)",
            ),
            opt("-V, --version", "Show the version"),
        ],
    },
];

/// The ways to start Nimbus, shown under `Usage`.
const USAGE: &[&str] = &[
    "nimbus [FILE | FOLDER]... [OPTIONS]",
    "nimbus --diff OLD NEW",
    "nimbus --merge BASE LOCAL REMOTE [-o RESULT]",
    "nimbus --git [DIR]",
];

/// Example commands and what they do (empty: the command speaks for itself).
const EXAMPLES: &[(&str, &str)] = &[
    ("nimbus .", "This folder, with its tabs from last time"),
    (
        "nimbus src/main.rs --find TODO",
        "Open a file, searching for TODO",
    ),
    ("git config --global core.editor \"nimbus --wait\"", ""),
];

/// The whole page width the help is laid out for. Terminals are at least
/// this wide, and reading the real width would take a system call per OS
/// for little gain.
const WIDTH: usize = 80;
/// Where descriptions start: two spaces of indent, the widest common
/// option, and two spaces of gap.
const COLUMN: usize = 23;

/// The help text. `all` adds the testing options; `bold` makes the
/// headings bold with an ANSI escape code.
#[must_use]
pub fn help(all: bool, bold: bool) -> String {
    let heading = |t: &str| {
        if bold {
            format!("\x1b[1m{t}:\x1b[0m\n")
        } else {
            format!("{t}:\n")
        }
    };
    let mut out = format!(
        "Nimbus {}, a fast text editor\n\n",
        env!("CARGO_PKG_VERSION")
    );
    out.push_str(&heading("Usage"));
    for u in USAGE {
        out.push_str("  ");
        out.push_str(u);
        out.push('\n');
    }
    for section in SECTIONS.iter().filter(|s| all || !s.testing) {
        out.push('\n');
        out.push_str(&heading(section.title));
        for o in section.opts {
            push_row(&mut out, o.usage, o.help, COLUMN);
        }
    }
    out.push('\n');
    out.push_str(&heading("Examples"));
    for (cmd, what) in EXAMPLES {
        push_row(&mut out, cmd, what, 35);
    }
    out.push_str(
        "\nThe manual's Command line page has the details, exit codes and\n\
         environment variables.",
    );
    out
}

/// One `  name   description` row, the description starting at `column`
/// and wrapped to [`WIDTH`]. A name too long for its column puts the
/// description on the next line, as `--merge BASE LOCAL REMOTE` does.
fn push_row(out: &mut String, name: &str, text: &str, column: usize) {
    out.push_str("  ");
    out.push_str(name);
    let mut at = 2 + name.chars().count();
    if text.is_empty() {
        out.push('\n');
        return;
    }
    if at + 2 > column {
        out.push('\n');
        at = 0;
    }
    let mut first = true;
    for word in text.split(' ') {
        let len = word.chars().count();
        if !first && at + 1 + len > WIDTH {
            out.push('\n');
            at = 0;
            first = true;
        }
        if first {
            out.push_str(&" ".repeat(column - at));
            at = column;
        } else {
            out.push(' ');
            at += 1;
        }
        out.push_str(word);
        at += len;
        first = false;
    }
    out.push('\n');
}

/// Whether to make headings bold: only when stdout is a terminal, the user
/// has not asked for plain output with `NO_COLOR` (no-color.org), and the
/// terminal understands escape codes. On Windows that means Windows
/// Terminal or another terminal that says so through `TERM`; the old
/// console window would print the codes as text.
#[must_use]
pub fn styled() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
    if set("NO_COLOR") || !std::io::stdout().is_terminal() {
        return false;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term == "dumb" {
        return false;
    }
    !cfg!(windows) || set("WT_SESSION") || !term.is_empty()
}

/// Every option name in the table, such as `--output` and `-o`.
fn names() -> impl Iterator<Item = &'static str> {
    SECTIONS
        .iter()
        .flat_map(|s| s.opts)
        .flat_map(|o| o.usage.split(", "))
        .filter_map(|u| u.split(' ').next())
}

/// The option the user most likely meant by `typed`, if one is close: at
/// most two letters added, removed or changed (so `--hepl` finds `--help`
/// and `--themes` finds `--theme`).
#[must_use]
pub fn suggest(typed: &str) -> Option<&'static str> {
    names()
        .filter(|n| n.starts_with("--"))
        .map(|n| (distance(typed, n), n))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, n)| n)
}

/// Edit distance (Levenshtein): how many single letters must be added,
/// removed or changed to turn `a` into `b`. Two rows of the usual table
/// are enough, since each row only needs the one before it.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut row = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let change = prev[j] + usize::from(ca != *cb);
            row[j + 1] = change.min(prev[j + 1] + 1).min(row[j] + 1);
        }
        std::mem::swap(&mut prev, &mut row);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_parsed_option_is_listed() {
        // The option names in `main.rs`'s `match` arms: string literals
        // starting with `-` that are followed by `=>` or `|`.
        let source = include_str!("main.rs");
        let listed: Vec<&str> = names().collect();
        let mut parsed = 0;
        for line in source.lines() {
            let line = line.trim();
            if !(line.starts_with("\"-") && (line.contains("\" =>") || line.contains("\" |"))) {
                continue;
            }
            for part in line.split('"').skip(1).step_by(2) {
                if part.starts_with('-') {
                    parsed += 1;
                    assert!(listed.contains(&part), "{part} is missing from --help");
                }
            }
        }
        assert!(parsed > 30, "found only {parsed} options in main.rs");
    }

    #[test]
    fn fits_eighty_columns() {
        for line in help(true, false).lines() {
            assert!(line.chars().count() <= WIDTH, "too wide: {line}");
        }
    }

    #[test]
    fn testing_options_only_with_all() {
        assert!(!help(false, false).contains("--headless"));
        assert!(help(true, false).contains("--headless"));
        assert!(help(false, false).contains("--wait"));
    }

    #[test]
    fn descriptions_line_up() {
        let text = help(false, false);
        let row = text
            .lines()
            .find(|l| l.starts_with("  --wrap"))
            .unwrap_or_default();
        assert_eq!(row.find("Turn"), Some(COLUMN));
        let long = text
            .lines()
            .skip_while(|l| !l.starts_with("  --merge"))
            .nth(1)
            .unwrap_or_default();
        assert!(long.starts_with(&" ".repeat(COLUMN)));
        assert!(long.trim_start().starts_with("Three-way"));
    }

    #[test]
    fn bold_only_when_asked() {
        assert!(help(false, true).contains("\x1b[1mUsage:"));
        assert!(!help(false, false).contains('\x1b'));
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest("--hepl"), Some("--help"));
        assert_eq!(suggest("--themes"), Some("--theme"));
        assert_eq!(suggest("--nosession"), Some("--no-session"));
        assert_eq!(suggest("--frobnicate"), None);
        assert_eq!(distance("", "abc"), 3);
        assert_eq!(distance("kitten", "sitting"), 3);
    }
}
