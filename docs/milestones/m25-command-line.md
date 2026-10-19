# M25 — Command line

**Status: planned (2026-10-18).** Written on 2026-10-18 at the owner's
request: an "Install `nimbus` Command" palette action like VS Code's,
grouped with the two backlog items it pairs with, opening files in the
Nimbus that is already running and `--wait` for git. Together they make
`nimbus FILE` from a terminal behave the way people expect from an
editor.

## Goal

Typing `nimbus FILE` in any terminal opens the file in the Nimbus window
that is already open, and `git config core.editor "nimbus --wait"`
works, without the user editing their `PATH` by hand.

## Done when

- [ ] **Install the command**: "Install `nimbus` Command in PATH" in the
      palette puts a `nimbus` command on the user's `PATH`, and
      "Uninstall `nimbus` Command" removes it. On macOS it links
      `/usr/local/bin/nimbus` to the running app's binary, asking for an
      administrator password through the system prompt when the folder
      needs it; on Linux it links `~/.local/bin/nimbus`, no password; on
      Windows it adds the folder holding `nimbus.exe` to the user's
      `PATH`, no password. The message line says what was done, or why
      not (for example, a link there already points somewhere else,
      which is never overwritten without asking).
- [ ] **Open in the running window**: with Nimbus open, `nimbus a.rs b.rs`
      adds two tabs to that window and brings it to the front, and the
      second process exits at once. With no Nimbus open, it starts as
      today. There is no `--new-window` flag, since one window stays the
      rule (`DESIGN.md` §2); measurement and test flags (`--headless`,
      `--smoke`, `--screenshot`, `--bench-typing`) always start a
      process of their own.
- [ ] **`--wait`**: `nimbus --wait FILE` returns only when that file's
      tab is closed (or Nimbus quits), with exit code 0, so git can use
      it as `core.editor`. It works whether the file opens in a new
      Nimbus or in the running one.
- [ ] Budgets: `--smoke` unchanged (the check for a running Nimbus costs
      under 5 ms before the window is created); handing files to a
      running Nimbus takes under 100 ms from the command to the new tab;
      no new `unsafe`.

## Scope

**In**: the install and uninstall actions on all three platforms, the
hand-off to a running Nimbus, `--wait`, the manual pages
(`getting-started.md` loses its manual `ln -s` step on macOS).

**Deferred**: opening a file at a line (`nimbus FILE:42`), which the
command line does not support today and would be a separate small
change; remote or cross-machine hand-off.

## Design

**Installing is a link or a `PATH` entry, never a copy.** A copy goes
stale on the next update; a link follows the app. The Homebrew cask
already links the binary (`packaging/homebrew/nimbus.rb.in`), so on a
Homebrew install the action finds the command present and says so.
On macOS the link needs administrator rights when `/usr/local/bin` is
not writable; the action then runs the `ln` through `osascript` with
`with administrator privileges`, the same system prompt VS Code uses,
so Nimbus never sees the password and needs no `unsafe`. On Windows
the user `PATH` lives in `HKEY_CURRENT_USER\Environment`; it is changed
with the `reg` tool or the `winreg` crate (measured for size) and new
terminals pick it up.

**One Nimbus per user, found by a socket.** At startup, before the
window exists, `nimbus` tries to connect to a socket in the state
directory. If something answers, it sends its file paths (made absolute
first) and exits, or with `--wait` stays connected until it is told the
tabs closed. If nothing answers, it becomes the running Nimbus and
listens on that socket on a worker thread, which posts each request to
the event loop as a user event, so opening the tabs goes through the
same code as the command line. On macOS and Linux this is a Unix domain
socket from `std`; Windows needs a named pipe, which `std` lacks, so it
is either a small crate (`interprocess`, measured) or a loopback TCP
port written to a file readable only by the user.

**`--wait` is a reply on the same connection.** The running Nimbus keeps
the connection with the tabs it opened for it, and writes one line when
the last of them closes. A `--wait` that starts a new Nimbus has no
connection to wait on, so it simply runs until those tabs close and
then exits, leaving the window open only if other tabs remain.

## Tasks

1. Install and Uninstall actions in `action.rs`; macOS link with the
   administrator prompt; Linux `~/.local/bin`; Windows user `PATH`.
2. The socket check at startup and the listener; requests as user
   events; tests for path handling with a fake listener.
3. `--wait` over the connection and in a fresh process.
4. Windows named pipe or loopback choice, measured.
5. Manual pages (`getting-started.md`, `command-line.md`); measurements;
   Results.

## Concepts

- **`PATH` and symbolic links**: how a terminal finds `nimbus`, and why
  a link beats a copy.
- **Single-instance apps**: a socket as a doorbell, and why the check
  must happen before the window is made.
- **An editor for git**: what `core.editor` expects, and why the process
  has to wait.

## Risks

| Risk | Fallback |
|---|---|
| The socket check slows startup | It is one connect attempt on a local socket; if it ever costs over 5 ms, it moves behind a flag in the config |
| A stale socket file after a crash makes every start think Nimbus is running | A failed connect removes the stale file and starts normally |
| `/usr/local/bin` does not exist on a fresh Apple Silicon Mac | The privileged command creates it first, as Homebrew's installer does |
| Another program already owns `/usr/local/bin/nimbus` | Never overwritten silently; the message names what is there |

## Results

_To be filled in when the milestone lands._
