# Encodings and line endings

A text file is a list of bytes, and its *encoding* says which characters
those bytes stand for. Today most files are UTF-8, which can hold every
character. Older files, CSV exports and files written on Windows are
often in something else: UTF-16, or Windows-1252, which holds 256
characters (Western letters, `€`, curly quotes) in one byte each.

Nimbus works in UTF-8 inside. A file in another encoding is converted
to UTF-8 when it opens and converted back when you save, so editing,
search and highlighting work the same in every file, and the file stays
in its own encoding on disk.

## Which encoding a file is

When a file opens, Nimbus decides its encoding in this order:

1. **Your choice**, if you made one: Reopen with Encoding, the
   `--encoding` option, or a pattern in the `[encodings]` table of
   `config.toml` (see [below](#choosing-an-encoding-in-advance)).
2. **A byte order mark** (BOM): a few invisible bytes some programs put
   at the start of a file to say what follows. A UTF-16 mark (either
   byte order) opens the file as UTF-16; a UTF-8 mark opens it as UTF-8
   with BOM. The mark is kept out of the text, so you cannot type in
   front of it, and it is written back on save.
3. **UTF-8**, if the file is valid UTF-8. A plain ASCII file always is.
4. **Windows-1252**, if the file is not UTF-8 but its first 64 KB look
   like Western text: no NUL or other control bytes beyond tab, line
   breaks, form feed and escape, none of the five byte values
   Windows-1252 leaves undefined, and at least 95 % printable
   characters.
5. Otherwise the file is held **as bytes**, the way Nimbus holds a
   binary file. The status bar says `Not UTF-8`, bytes that are not
   valid UTF-8 show as `�`, and they are written back unchanged unless
   you edit them.

The guess looks at the first 64 KB only, so even a 4 GB file opens at
once. One exception to step 4: if those first 64 KB are UTF-8 with
accented letters in them and the bad bytes come later, the file is UTF-8
with a few stray bytes, and reading it as Windows-1252 would garble
every accent. Such a file is held as bytes instead.

East Asian encodings (Shift_JIS, GBK, Big5, …) are never guessed; see
[East Asian encodings](#east-asian-encodings).

## In the status bar

The status bar shows the encoding and the line ending of the active tab,
for example `CRLF   Windows-1252`:

| Item | Meaning |
|---|---|
| `LF` or `CRLF` | The line ending most lines use. Click it to change the file's line endings. |
| `UTF-8`, `UTF-8 BOM`, `UTF-16 LE`, `UTF-16 BE`, `Windows-1252`, `ISO-8859-1`, `Mac Roman`, … | The encoding the file was read in, and is saved in. Click it for Reopen with Encoding and Save with Encoding. |
| `Not UTF-8` | The file is held as bytes (step 5 above). |
| `Read-only` | A huge file in another encoding; see [Huge files](#huge-files-in-another-encoding). |

A huge memory-mapped UTF-8 file shows no encoding item (or `BOM`, if it
starts with one), because Nimbus never reads it all to check.

A click on either item opens the command palette with the matching
commands already listed.

## Reopen with Encoding

When the guess is wrong (a file shows `Ã©` where it should show `é`, say),
run **Reopen with Encoding…** from the palette or the File menu, or click
the encoding in the status bar. Pick an encoding from the list (type to
narrow it; the current one is marked `current`) and the file is read
again in it.

- If the tab has unsaved changes, Nimbus asks first, since reopening
  drops them.
- The choice sticks for that tab: if the file changes on disk and is
  reloaded, it is read in the same encoding again.
- If some bytes are not valid in the encoding you picked, they show as
  `�` and the message line says so. Saving would write `�` in their
  place, so the first save asks before it does.

An untitled tab has no file to reopen.

## Save with Encoding

**Save with Encoding…** (palette, File menu, or the status bar) saves the
file in another encoding, and keeps saving it in that one. Besides the
encodings Reopen offers, the list has `UTF-8 BOM` (UTF-8 with a byte
order mark), and UTF-16 is written with a mark, as Windows expects. If
the save fails, the file keeps its old encoding.

Two kinds of file cannot be converted:

- Files of 256 MB or more save only as UTF-8.
- A file held as bytes (`Not UTF-8`) saves only as it is.

## Saving

A file is always saved in the encoding it was read in, with its byte
order mark if it had one, so a file you open and save without editing
comes back byte for byte.

Some encodings cannot hold every character: there is no `😀` in
Windows-1252. If the file holds a character its encoding has no bytes
for, the save stops before anything is written. The cursor moves to the
character, the message line names it and its line, and a dialog offers
**Save as UTF-8**, which holds every character. Choosing it makes the
file UTF-8 from then on; Cancel leaves the file untouched and the tab
unsaved, so you can remove the character and save again.

## Line endings

Each line of a text file ends with a line break: `LF` (one byte, used on
macOS and Linux) or `CRLF` (two bytes, the Windows convention). Nimbus
keeps whatever the file uses:

- the status bar shows the ending most lines use (a file with no line
  breaks, or as many of each, shows `LF`);
- Enter inserts that ending, and pasted text is converted to it;
- a file with mixed endings keeps them mixed until you change them.

To change them, run **Change Line Endings to LF** or **Change Line
Endings to CRLF** from the palette, or click `LF` / `CRLF` in the status
bar. Every line ending in the file changes as one edit, so one Undo puts
them back, and Enter inserts the new ending from then on. A lone `CR`
(an old Mac line break) is treated as ordinary text and left alone.
Files over 64 MB are too large to change at once.

Undo puts the old endings back in the text but does not change what
Enter inserts; run the other Change Line Endings command to switch it
back too.

## Choosing an encoding in advance

To skip the guess for some files, give them an encoding in the
`[encodings]` table of `config.toml` (see [Settings](settings.md#encodings)):

```toml
[encodings]
"*.csv" = "windows-1252"
"legacy/*.txt" = "latin1"
```

A pattern without a `/` is matched against the file's name, wherever
the file is. One with a `/` is matched against the end of the file's
path, so `legacy/*.txt` is any `.txt` file directly inside a folder
called `legacy`, such as `/home/you/project/legacy/notes.txt`. A `*`
stays within one folder; use `**` to go through folders
(`legacy/**/*.txt`), and start a pattern with `/` to match from the
root. The table applies to files opened after the
config is loaded or reloaded. A pattern or encoding name Nimbus does not
understand is reported in the message line and skipped.

For a one-off, name the encoding on the command line; it applies to the
files named there:

```sh
nimbus --encoding latin1 old-notes.txt
```

## Encoding names

Names may be written in any case, with `-`, `_` or a space. The usual
ones:

| Name | Encoding |
|---|---|
| `utf-8` | UTF-8 |
| `utf-16le` (or `utf-16`), `utf-16be` | UTF-16, little- or big-endian |
| `windows-1252` (or `cp1252`, `ansi`) | Western European, as Windows writes it |
| `latin1` (or `iso-8859-1`) | ISO-8859-1: each byte is the character with that number |
| `iso-8859-15` | Western European with `€` |
| `windows-1250` … `windows-1258`, `windows-874` | Windows' other regional tables |
| `iso-8859-2` … `iso-8859-16` | The other ISO-8859 tables |
| `koi8-r`, `koi8-u`, `ibm866` | Cyrillic |
| `mac-roman`, `mac-cyrillic` | Old Mac encodings |

Other standard names for these work too (the ones web browsers accept,
such as `cp1251`).

## East Asian encodings

Shift_JIS, EUC-JP, ISO-2022-JP, GBK, GB18030, Big5 and EUC-KR are offered
only in a Nimbus built with the `east-asian` feature:

```sh
cargo build --release -p nimbus --features east-asian
```

The released packages are built without it. In such a build they appear
in the encoding lists and can be named in `--encoding` and
`[encodings]` (`shift_jis`, `euc-jp`, `iso-2022-jp`, `gbk`, `gb18030`,
`big5`, `euc-kr`). They are still never guessed from a file's content,
since that guess is unreliable: choose them yourself.

## Huge files in another encoding

Converting a file means holding all of it in memory, so files of 256 MB
or more are never converted. What happens depends on how Nimbus knows the
encoding:

- **Known** (a UTF-16 byte order mark, `--encoding`, `[encodings]`, or
  Reopen with Encoding): the file opens **read-only**, its bytes shown as
  they are. The status bar shows the encoding and `Read-only`, and
  typing or saving says why. Editing those bytes as UTF-8 would mix two
  encodings in one file.
- **Guessed** (it looks like Windows-1252): the file is held as bytes
  (`Not UTF-8`) and stays editable, as any huge file is. Its bytes are
  written back unchanged.

See [Large files](large-files.md) for the rest of what changes at that
size.

## Find and replace in other encodings

Find in the file works on the converted text, so it finds `é` in a
Windows-1252 file like anywhere else.

Find in Files and Replace in Files read files from disk as they are
(only tabs with unsaved changes are searched as shown). In a file that
is not UTF-8, a search for plain ASCII text works, but accented letters
are other bytes there and are not found. Replace in Files therefore:

- skips open tabs that are not UTF-8 (or are read-only), and says how
  many in its summary;
- refuses a replacement with non-ASCII characters in a file on disk that
  is not UTF-8 ("not UTF-8; open it to replace there"). ASCII
  replacements are the same bytes in every Western encoding, so those are
  made.
