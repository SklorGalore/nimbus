//! Parsers for git's machine-readable output (M27).
//!
//! **Plumbing and porcelain.** git has two kinds of commands. *Porcelain*
//! (`git status`, `git log` with its default look) prints for people: its
//! wording changes between versions, follows the user's language and
//! config, and quotes odd file names. *Plumbing* (`git status
//! --porcelain=v2`, `git diff-tree --raw`, `git cat-file --batch`, `git
//! log -z --format=...`) prints a fixed format that git promises to keep.
//! A client parses only plumbing, so it works with any git and any
//! language setting.
//!
//! Every format here is separated by NUL bytes (`-z`), never by spaces or
//! newlines: file names can hold both, and with `-z` git prints them as
//! they are instead of quoting them.
//!
//! These functions only parse; they never run git, so they are tested on
//! fixed bytes.

use std::fmt::Write as _;

/// How one side of a file changed: the index against HEAD, or the working
/// tree against the index. One letter of git's two-letter `XY` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// No change on this side (`.`).
    Unmodified,
    /// Contents changed (`M`).
    Modified,
    /// File type changed, such as a file becoming a symlink (`T`).
    TypeChanged,
    /// New (`A`).
    Added,
    /// Removed (`D`).
    Deleted,
    /// Moved, perhaps with changes (`R`).
    Renamed,
    /// Copied from another file (`C`).
    Copied,
    /// A merge conflict waiting to be resolved (`U`, or any code of an
    /// unmerged entry).
    Conflicted,
    /// Not tracked by git (`?`).
    Untracked,
}

impl Kind {
    fn from_code(c: u8) -> Self {
        match c {
            b'M' => Self::Modified,
            b'T' => Self::TypeChanged,
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'U' => Self::Conflicted,
            b'?' => Self::Untracked,
            _ => Self::Unmodified,
        }
    }

    /// The one-letter mark shown before a file's name.
    #[must_use]
    pub fn letter(self) -> char {
        match self {
            Self::Unmodified => ' ',
            Self::Modified => 'M',
            Self::TypeChanged => 'T',
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Renamed => 'R',
            Self::Copied => 'C',
            Self::Conflicted => '!',
            Self::Untracked => '?',
        }
    }
}

/// One path in `git status`.
///
/// **The index.** git keeps three versions of each file: the last commit
/// (HEAD), the *index* (the staging area: what the next commit will hold)
/// and the working tree (the file on disk). `git add` copies a file from
/// the working tree into the index. So a file has two changes, and status
/// shows two columns: `staged` is the index against HEAD, `unstaged` the
/// working tree against the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// The path from the repository's top folder, with `/` separators.
    pub path: String,
    /// The path before a rename or copy in the index.
    pub orig: Option<String>,
    /// The index against HEAD.
    pub staged: Kind,
    /// The working tree against the index.
    pub unstaged: Kind,
    /// The blob in HEAD, or `None` when HEAD has no such file.
    pub head_oid: Option<String>,
    /// The blob in the index, or `None` when the index has none.
    pub index_oid: Option<String>,
}

impl StatusEntry {
    /// Whether the file has a staged change.
    #[must_use]
    pub fn is_staged(&self) -> bool {
        !matches!(
            self.staged,
            Kind::Unmodified | Kind::Untracked | Kind::Conflicted
        )
    }

    /// Whether the file has an unstaged change (untracked and conflicted
    /// files count here: they wait on the working tree).
    #[must_use]
    pub fn is_unstaged(&self) -> bool {
        self.unstaged != Kind::Unmodified || self.staged == Kind::Conflicted
    }
}

/// What `git status --porcelain=v2 --branch -z` reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// The checked-out branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// The commit HEAD points at, or `None` before the first commit.
    pub head: Option<String>,
    /// The branch's upstream, such as `origin/main`.
    pub upstream: Option<String>,
    /// Commits on the branch and not on its upstream, then the other way.
    pub ahead_behind: Option<(u32, u32)>,
    /// Every path with a change, in git's order (sorted by path).
    pub entries: Vec<StatusEntry>,
}

/// The NUL-separated fields of `bytes`, as text (file names that are not
/// UTF-8 are shown with replacement characters).
fn fields(bytes: &[u8]) -> impl Iterator<Item = String> + '_ {
    bytes
        .split(|&b| b == 0)
        .filter(|f| !f.is_empty())
        .map(|f| String::from_utf8_lossy(f).into_owned())
}

/// An object id, or `None` for git's all-zero "no object".
fn oid(s: &str) -> Option<String> {
    (!s.is_empty() && s.bytes().any(|b| b != b'0')).then(|| s.to_owned())
}

/// Parse `git status --porcelain=v2 --branch -z`.
///
/// Each record is one NUL-terminated field, except a rename (`2`), whose
/// original path follows in a field of its own. Lines are:
///
/// - `# branch.oid OID` / `# branch.head NAME` / `# branch.upstream U` /
///   `# branch.ab +A -B`: the header;
/// - `1 XY SUB mH mI mW hH hI PATH`: a changed file;
/// - `2 XY SUB mH mI mW hH hI Xscore PATH` then `ORIG`: a rename or copy;
/// - `u XY SUB m1 m2 m3 mW h1 h2 h3 PATH`: a conflict;
/// - `? PATH`: untracked; `! PATH`: ignored (not asked for).
#[must_use]
pub fn status(bytes: &[u8]) -> Status {
    let mut out = Status::default();
    let mut fields = fields(bytes);
    while let Some(line) = fields.next() {
        if let Some(header) = line.strip_prefix("# ") {
            branch_header(&mut out, header);
            continue;
        }
        let Some((kind, rest)) = line.split_once(' ') else {
            continue;
        };
        match kind {
            "1" => {
                let parts: Vec<&str> = rest.splitn(8, ' ').collect();
                if let [xy, _sub, _mh, _mi, _mw, hh, hi, path] = parts[..] {
                    out.entries.push(entry(xy, (hh, hi), path, None));
                }
            }
            "2" => {
                let parts: Vec<&str> = rest.splitn(9, ' ').collect();
                let orig = fields.next();
                if let [xy, _sub, _mh, _mi, _mw, hh, hi, _score, path] = parts[..] {
                    out.entries.push(entry(xy, (hh, hi), path, orig));
                }
            }
            "u" => {
                let parts: Vec<&str> = rest.splitn(10, ' ').collect();
                if let [_xy, _sub, _m1, _m2, _m3, _mw, h1, h2, _h3, path] = parts[..] {
                    out.entries.push(StatusEntry {
                        path: path.to_owned(),
                        orig: None,
                        staged: Kind::Conflicted,
                        unstaged: Kind::Conflicted,
                        // Stage 1 is the common ancestor, stage 2 ours.
                        head_oid: oid(h1),
                        index_oid: oid(h2),
                    });
                }
            }
            "?" => out.entries.push(StatusEntry {
                path: rest.to_owned(),
                orig: None,
                staged: Kind::Untracked,
                unstaged: Kind::Untracked,
                head_oid: None,
                index_oid: None,
            }),
            _ => {}
        }
    }
    out
}

fn entry(xy: &str, (hh, hi): (&str, &str), path: &str, orig: Option<String>) -> StatusEntry {
    let xy = xy.as_bytes();
    StatusEntry {
        path: path.to_owned(),
        orig,
        staged: Kind::from_code(xy.first().copied().unwrap_or(b'.')),
        unstaged: Kind::from_code(xy.get(1).copied().unwrap_or(b'.')),
        head_oid: oid(hh),
        index_oid: oid(hi),
    }
}

fn branch_header(out: &mut Status, header: &str) {
    let Some((key, value)) = header.split_once(' ') else {
        return;
    };
    match key {
        "branch.oid" if value != "(initial)" => out.head = Some(value.to_owned()),
        "branch.head" if value != "(detached)" => out.branch = Some(value.to_owned()),
        "branch.upstream" => out.upstream = Some(value.to_owned()),
        "branch.ab" => {
            let mut nums = value
                .split(' ')
                .map(|n| n.trim_start_matches(['+', '-']).parse::<u32>().ok());
            if let (Some(Some(a)), Some(Some(b))) = (nums.next(), nums.next()) {
                out.ahead_behind = Some((a, b));
            }
        }
        _ => {}
    }
}

/// The separator between the fields of one log record: ASCII's "unit
/// separator", which no hash, name or date contains. The subject comes
/// last, so even one that holds this byte only loses nothing: the record
/// is split at the first four.
pub const FIELD: u8 = 0x1f;

/// The `--format` that [`LogRecord::parse`] reads: hash, parent hashes
/// (space-separated), author name, author time (seconds since 1970), and
/// subject. `-z` puts a NUL after each commit.
pub const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%at%x1f%s";

/// One commit as `git log -z` prints it with [`LOG_FORMAT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord<'a> {
    /// The commit's hash, in hex.
    pub oid: &'a str,
    /// Its parents' hashes, separated by spaces (empty for a root commit).
    pub parents: &'a str,
    /// The author's name.
    pub author: &'a str,
    /// When it was authored, in seconds since 1970.
    pub time: i64,
    /// The first line of its message.
    pub subject: &'a str,
}

impl<'a> LogRecord<'a> {
    /// Parse one record (without its NUL). `None` when it is malformed;
    /// non-UTF-8 names and subjects come through lossily, via `text`.
    #[must_use]
    pub fn parse(text: &'a str) -> Option<Self> {
        let mut parts = text.splitn(5, char::from(FIELD));
        let oid = parts.next()?.trim_start_matches('\n');
        let parents = parts.next()?;
        let author = parts.next()?;
        let time = parts.next()?.parse().ok()?;
        let subject = parts.next().unwrap_or("");
        if oid.is_empty() || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            oid,
            parents,
            author,
            time,
            subject,
        })
    }
}

/// Splits a stream of `git log -z` output, arriving in chunks of any size,
/// into whole records. A record cut by a chunk boundary is kept until the
/// rest arrives.
#[derive(Debug, Default)]
pub struct Records {
    partial: Vec<u8>,
}

impl Records {
    /// Feed the next chunk; `each` sees every record it completes.
    pub fn feed(&mut self, chunk: &[u8], mut each: impl FnMut(&[u8])) {
        let mut rest = chunk;
        while let Some(end) = memchr::memchr(0, rest) {
            let (head, tail) = rest.split_at(end);
            if self.partial.is_empty() {
                each(head);
            } else {
                self.partial.extend_from_slice(head);
                each(&self.partial);
                self.partial.clear();
            }
            rest = tail.get(1..).unwrap_or_default();
        }
        self.partial.extend_from_slice(rest);
    }

    /// The last record, when the output did not end with a NUL (git's
    /// `-z` output always does).
    #[cfg(test)]
    pub fn finish(&mut self, mut each: impl FnMut(&[u8])) {
        if !self.partial.is_empty() {
            each(&self.partial);
            self.partial.clear();
        }
    }
}

/// What kind of name a ref is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// A local branch (`refs/heads/`).
    Branch,
    /// A remote-tracking branch (`refs/remotes/`).
    Remote,
    /// A tag (`refs/tags/`).
    Tag,
    /// The stash (`refs/stash`).
    Stash,
}

/// A branch, tag or remote-tracking branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    /// The short name, such as `main`, `origin/main` or `v1.0.0`.
    pub name: String,
    /// What kind it is.
    pub kind: RefKind,
    /// The commit it names (for an annotated tag, the commit the tag
    /// points at, not the tag object).
    pub target: String,
}

/// The `--format` that [`refs`] reads: full name, object, the object an
/// annotated tag points at, and the name a symbolic ref points at.
pub const REF_FORMAT: &str = "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(symref)";

/// Parse `git for-each-ref` with [`REF_FORMAT`]: one ref per line, fields
/// separated by NUL. Symbolic refs (`origin/HEAD`) are left out: they
/// repeat a branch already listed. Sorted by kind, then by name.
#[must_use]
pub fn refs(bytes: &[u8]) -> Vec<Ref> {
    let mut out = Vec::new();
    for line in bytes.split(|&b| b == b'\n') {
        let text = String::from_utf8_lossy(line);
        let mut parts = text.split('\0');
        let (Some(full), Some(object)) = (parts.next(), parts.next()) else {
            continue;
        };
        let peeled = parts.next().unwrap_or("");
        let symref = parts.next().unwrap_or("");
        if !symref.is_empty() || object.is_empty() {
            continue;
        }
        let (kind, name) = if let Some(n) = full.strip_prefix("refs/heads/") {
            (RefKind::Branch, n)
        } else if let Some(n) = full.strip_prefix("refs/remotes/") {
            (RefKind::Remote, n)
        } else if let Some(n) = full.strip_prefix("refs/tags/") {
            (RefKind::Tag, n)
        } else if full == "refs/stash" {
            (RefKind::Stash, "stash")
        } else {
            continue;
        };
        let target = if peeled.is_empty() { object } else { peeled };
        out.push(Ref {
            name: name.to_owned(),
            kind,
            target: target.to_owned(),
        });
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| natural(&a.name, &b.name)));
    out
}

/// One file in a commit's change list (`git diff-tree --raw`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// What happened to it.
    pub kind: Kind,
    /// Its path after the commit.
    pub path: String,
    /// Its path before, for a rename or copy.
    pub old_path: Option<String>,
    /// The blob before, `None` for an added file.
    pub old_oid: Option<String>,
    /// The blob after, `None` for a deleted file.
    pub new_oid: Option<String>,
    /// Whether this is a submodule (mode `160000`), whose "blob" is a
    /// commit in another repository and cannot be read here.
    pub submodule: bool,
}

/// Parse `git diff-tree -r -z --raw --no-abbrev`: for each file a header
/// field `:OLDMODE NEWMODE OLD NEW STATUS`, then its path, then for a
/// rename or copy (`R086`, `C100`) its new path.
#[must_use]
pub fn diff_tree(bytes: &[u8]) -> Vec<FileChange> {
    let mut out = Vec::new();
    let mut fields = bytes.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(header) = fields.next() {
        let header = String::from_utf8_lossy(header);
        let Some(header) = header.strip_prefix(':') else {
            // A commit id line (without --no-commit-id); not a file.
            continue;
        };
        let parts: Vec<&str> = header.split(' ').collect();
        let [old_mode, new_mode, old, new, status] = parts[..] else {
            continue;
        };
        let code = status.as_bytes().first().copied().unwrap_or(b'M');
        let Some(first) = fields.next() else { break };
        let first = String::from_utf8_lossy(first).into_owned();
        let (path, old_path) = if matches!(code, b'R' | b'C') {
            let Some(second) = fields.next() else { break };
            (String::from_utf8_lossy(second).into_owned(), Some(first))
        } else {
            (first, None)
        };
        out.push(FileChange {
            kind: Kind::from_code(code),
            path,
            old_path,
            old_oid: oid(old),
            new_oid: oid(new),
            submodule: old_mode == "160000" || new_mode == "160000",
        });
    }
    out
}

/// One object's contents from `git cat-file --batch`, or `None` when the
/// object is missing.
pub type Blob = Option<Vec<u8>>;

/// Parse `git cat-file --batch` output for `count` requests: each object
/// is a header line `OID TYPE SIZE`, then `SIZE` bytes and a newline; a
/// missing one is the line `NAME missing`.
#[must_use]
pub fn batch(mut bytes: &[u8], count: usize) -> Vec<Blob> {
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let Some(end) = memchr::memchr(b'\n', bytes) else {
            break;
        };
        let header = String::from_utf8_lossy(bytes.get(..end).unwrap_or_default());
        bytes = bytes.get(end + 1..).unwrap_or_default();
        let parts: Vec<&str> = header.split(' ').collect();
        match parts[..] {
            [_, _, size] => {
                let Ok(size) = size.parse::<usize>() else {
                    out.push(None);
                    continue;
                };
                let Some(content) = bytes.get(..size) else {
                    out.push(None);
                    break;
                };
                out.push(Some(content.to_vec()));
                bytes = bytes.get(size + 1..).unwrap_or_default();
            }
            _ => out.push(None),
        }
    }
    out.resize(count, None);
    out
}

/// Who made a commit and when.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ident {
    /// Their name.
    pub name: String,
    /// Their email address.
    pub email: String,
    /// Seconds since 1970.
    pub time: i64,
    /// Their time zone, in minutes east of UTC.
    pub offset: i32,
}

/// A commit object, as `git cat-file commit OID` prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commit {
    /// Its parents' hashes.
    pub parents: Vec<String>,
    /// Who wrote the change.
    pub author: Ident,
    /// Who made the commit (differs after a rebase or cherry-pick).
    pub committer: Ident,
    /// The whole message.
    pub message: String,
}

/// Parse a raw commit object: header lines (`tree`, `parent`, `author`,
/// `committer`, and others such as `gpgsig`, whose continuation lines
/// start with a space), a blank line, then the message.
#[must_use]
pub fn commit(bytes: &[u8]) -> Commit {
    let text = String::from_utf8_lossy(bytes);
    let (head, message) = text.split_once("\n\n").unwrap_or((&text, ""));
    let mut out = Commit {
        message: message.to_owned(),
        ..Commit::default()
    };
    for line in head.lines() {
        if line.starts_with(' ') {
            continue;
        }
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        match key {
            "parent" => out.parents.push(value.to_owned()),
            "author" => out.author = ident(value),
            "committer" => out.committer = ident(value),
            _ => {}
        }
    }
    out
}

/// Parse `Name <email> 1700000000 +0100`.
fn ident(value: &str) -> Ident {
    let (name, rest) = value.split_once(" <").unwrap_or((value, ""));
    let (email, when) = rest.split_once("> ").unwrap_or((rest, ""));
    let mut when = when.split(' ');
    let time = when.next().and_then(|t| t.parse().ok()).unwrap_or(0);
    let offset = when.next().map_or(0, |z| {
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let digits = z.trim_start_matches(['+', '-']);
        let hours: i32 = digits.get(..2).and_then(|h| h.parse().ok()).unwrap_or(0);
        let minutes: i32 = digits.get(2..4).and_then(|m| m.parse().ok()).unwrap_or(0);
        sign * (hours * 60 + minutes)
    });
    Ident {
        name: name.to_owned(),
        email: email.trim_end_matches('>').to_owned(),
        time,
        offset,
    }
}

/// `time` (seconds since 1970) in the time zone `offset` minutes east of
/// UTC, as `2026-10-19 14:05 +0200`.
///
/// Dates are worked out here rather than with a date library: a day count
/// to a calendar date is a few lines of integer arithmetic (Howard
/// Hinnant's `civil_from_days`), and nothing else in Nimbus needs dates.
#[must_use]
pub fn date(time: i64, offset: i32) -> String {
    // A commit can carry any timestamp (git only checks that it is a
    // number), so saturate rather than overflow on one near the i64 limits.
    let local = time.saturating_add(i64::from(offset) * 60);
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let sign = if offset < 0 { '-' } else { '+' };
    let off = offset.unsigned_abs();
    let mut s = String::new();
    let _ = write!(
        s,
        "{y:04}-{m:02}-{d:02} {:02}:{:02} {sign}{:02}{:02}",
        secs / 3600,
        secs % 3600 / 60,
        off / 60,
        off % 60
    );
    s
}

/// Days since 1970-01-01 to (year, month, day), in the proleptic
/// Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (
        y,
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// How long ago `time` was, seen from `now` (both seconds since 1970),
/// in the short form the history shows: `now`, `5 min`, `3 h`, `2 days`,
/// `4 weeks`, `7 months`, `2 years`.
pub fn ago(out: &mut String, time: i64, now: i64) {
    out.clear();
    // Saturating, as in `date`: the time comes from the commit.
    let secs = now.saturating_sub(time).max(0);
    let (n, unit) = match secs {
        s if s < 60 => {
            out.push_str("now");
            return;
        }
        s if s < 3600 => (s / 60, "min"),
        s if s < 86_400 => (s / 3600, "h"),
        s if s < 14 * 86_400 => (s / 86_400, "day"),
        s if s < 60 * 86_400 => (s / (7 * 86_400), "week"),
        s if s < 365 * 86_400 => (s / (30 * 86_400), "month"),
        s => (s / (365 * 86_400), "year"),
    };
    let plural = n != 1 && !matches!(unit, "min" | "h");
    let _ = write!(out, "{n} {unit}{}", if plural { "s" } else { "" });
}

/// The version `git --version` reports, as numbers: `git version
/// 2.43.0.windows.1` is `(2, 43)`.
#[must_use]
pub fn version(text: &str) -> Option<(u32, u32)> {
    let v = text.trim().strip_prefix("git version ")?;
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    Some((major, minor))
}

/// Compare names with runs of digits as numbers, so `v2` sorts before
/// `v10` and `release-9` before `release-10`.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let db = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (na, ra) = a.split_at(da);
                let (nb, rb) = b.split_at(db);
                // Leading zeros aside, a longer run is a larger number.
                let trim = |n: &[u8]| -> usize { n.iter().take_while(|&&c| c == b'0').count() };
                let (na, nb) = (&na[trim(na)..], &nb[trim(nb)..]);
                let order = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
                (a, b) = (ra, rb);
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                (a, b) = (&a[1..], &b[1..]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "1111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222";
    const Z: &str = "0000000000000000000000000000000000000000";

    #[test]
    fn status_reads_the_header_and_every_kind_of_entry() {
        let raw = format!(
            "# branch.oid {H1}\0# branch.head main\0# branch.upstream origin/main\0\
             # branch.ab +2 -1\0\
             1 M. N... 100644 100644 100644 {H1} {H2} src/a b.rs\0\
             1 .D N... 100644 100644 000000 {H1} {H1} gone.txt\0\
             2 R. N... 100644 100644 100644 {H1} {H1} R100 new name.txt\0old name.txt\0\
             u UU N... 100644 100644 100644 100644 {H1} {H2} {H1} both.txt\0\
             1 A. N... 000000 100644 100644 {Z} {H2} added.txt\0\
             ? untracked dir/\0"
        );
        let s = status(raw.as_bytes());
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.head.as_deref(), Some(H1));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        assert_eq!(s.ahead_behind, Some((2, 1)));
        let e = &s.entries;
        assert_eq!(e.len(), 6);
        assert_eq!(e[0].path, "src/a b.rs");
        assert_eq!(
            (e[0].staged, e[0].unstaged),
            (Kind::Modified, Kind::Unmodified)
        );
        assert!(e[0].is_staged() && !e[0].is_unstaged());
        assert_eq!(e[0].index_oid.as_deref(), Some(H2));
        assert_eq!(e[1].unstaged, Kind::Deleted);
        assert!(!e[1].is_staged() && e[1].is_unstaged());
        assert_eq!(e[2].path, "new name.txt");
        assert_eq!(e[2].orig.as_deref(), Some("old name.txt"));
        assert_eq!(e[2].staged, Kind::Renamed);
        assert_eq!(e[3].staged, Kind::Conflicted);
        assert!(!e[3].is_staged() && e[3].is_unstaged());
        assert_eq!(e[4].head_oid, None, "the all-zero id means no object");
        assert_eq!(e[5].path, "untracked dir/");
        assert_eq!(e[5].unstaged, Kind::Untracked);
    }

    #[test]
    fn status_of_a_new_repository_and_a_detached_head() {
        let s = status(b"# branch.oid (initial)\0# branch.head main\0");
        assert_eq!((s.head, s.branch.as_deref()), (None, Some("main")));
        let raw = format!("# branch.oid {H1}\0# branch.head (detached)\0");
        let s = status(raw.as_bytes());
        assert_eq!((s.head.as_deref(), s.branch), (Some(H1), None));
    }

    #[test]
    fn log_records_split_at_the_first_four_separators() {
        let text = format!("{H1}\x1f{H2} {Z}\x1fAda Lovelace\x1f1700000000\x1fFix: a\x1fb");
        let r = LogRecord::parse(&text).unwrap();
        assert_eq!(r.oid, H1);
        assert_eq!(r.parents, format!("{H2} {Z}"));
        assert_eq!(r.author, "Ada Lovelace");
        assert_eq!(r.time, 1_700_000_000);
        assert_eq!(r.subject, "Fix: a\x1fb");
        // git puts a newline between -z records with some formats.
        let lead = format!("\n{H1}\x1f\x1fA\x1f1\x1f");
        assert_eq!(LogRecord::parse(&lead).unwrap().subject, "");
        assert!(LogRecord::parse("not a hash\x1f\x1fa\x1f1\x1fs").is_none());
        assert!(LogRecord::parse(&format!("{H1}\x1f\x1fa\x1fsoon\x1fs")).is_none());
    }

    #[test]
    fn records_survive_any_chunking() {
        let stream = b"one\0two\0three\0four";
        for size in 1..stream.len() {
            let mut r = Records::default();
            let mut got = Vec::new();
            for chunk in stream.chunks(size) {
                r.feed(chunk, |rec| got.push(rec.to_vec()));
            }
            r.finish(|rec| got.push(rec.to_vec()));
            assert_eq!(
                got,
                [&b"one"[..], b"two", b"three", b"four"],
                "chunk size {size}"
            );
        }
    }

    #[test]
    fn names_sort_with_numbers_as_numbers() {
        let mut names = ["v10", "v2", "v1", "v100", "v1.10", "v1.9", "a", "v02"];
        names.sort_by(|a, b| natural(a, b));
        assert_eq!(
            names,
            ["a", "v1", "v1.9", "v1.10", "v2", "v02", "v10", "v100"]
        );
    }

    #[test]
    fn refs_are_sorted_peeled_and_skip_symbolic_ones() {
        let raw = format!(
            "refs/tags/v1\0{H2}\0{H1}\0\nrefs/heads/main\0{H1}\0\0\n\
             refs/remotes/origin/HEAD\0{H1}\0\0refs/remotes/origin/main\n\
             refs/remotes/origin/main\0{H1}\0\0\nrefs/stash\0{H2}\0\0\nrefs/notes/x\0{H1}\0\0\n"
        );
        let r = refs(raw.as_bytes());
        let names: Vec<(&str, RefKind)> = r.iter().map(|r| (r.name.as_str(), r.kind)).collect();
        assert_eq!(
            names,
            [
                ("main", RefKind::Branch),
                ("origin/main", RefKind::Remote),
                ("v1", RefKind::Tag),
                ("stash", RefKind::Stash),
            ]
        );
        assert_eq!(r[2].target, H1, "an annotated tag names its commit");
    }

    #[test]
    fn diff_tree_reads_renames_and_new_files() {
        let raw = format!(
            ":100644 100644 {H1} {H2} M\0a.rs\0\
             :100644 100644 {H1} {H1} R090\0old.rs\0new.rs\0\
             :000000 100644 {Z} {H2} A\0dir/with space.txt\0\
             :100644 000000 {H1} {Z} D\0gone\0\
             :160000 160000 {H1} {H2} M\0sub\0"
        );
        let f = diff_tree(raw.as_bytes());
        assert_eq!(f.len(), 5);
        assert_eq!((f[0].kind, f[0].path.as_str()), (Kind::Modified, "a.rs"));
        assert_eq!(f[1].kind, Kind::Renamed);
        assert_eq!(
            (f[1].old_path.as_deref(), f[1].path.as_str()),
            (Some("old.rs"), "new.rs")
        );
        assert_eq!((f[2].old_oid.as_deref(), f[2].kind), (None, Kind::Added));
        assert_eq!(f[2].path, "dir/with space.txt");
        assert_eq!(f[3].new_oid, None);
        assert!(f[4].submodule && !f[0].submodule);
    }

    #[test]
    fn batch_reads_objects_with_newlines_inside_and_missing_ones() {
        let raw = format!("{H1} blob 6\nab\ncd\n\n{H2} missing\n{H2} blob 0\n\n");
        let b = batch(raw.as_bytes(), 3);
        assert_eq!(b, [Some(b"ab\ncd\n".to_vec()), None, Some(Vec::new())]);
        // Cut short: what is missing is None.
        assert_eq!(
            batch(format!("{H1} blob 99\nab").as_bytes(), 2),
            [None, None]
        );
    }

    #[test]
    fn commits_keep_parents_people_and_the_whole_message() {
        let raw = format!(
            "tree {H1}\nparent {H1}\nparent {H2}\n\
             author Ada Lovelace <ada@example.com> 1700000000 +0130\n\
             committer C B <c@b> 1700000060 -0800\n\
             gpgsig -----BEGIN-----\n line\n -----END-----\n\n\
             Subject\n\nBody line\n"
        );
        let c = commit(raw.as_bytes());
        assert_eq!(c.parents, [H1, H2]);
        assert_eq!(c.author.name, "Ada Lovelace");
        assert_eq!(c.author.email, "ada@example.com");
        assert_eq!((c.author.time, c.author.offset), (1_700_000_000, 90));
        assert_eq!(c.committer.offset, -480);
        assert_eq!(c.message, "Subject\n\nBody line\n");
    }

    #[test]
    fn dates_are_calendar_dates_in_the_commit_time_zone() {
        assert_eq!(date(0, 0), "1970-01-01 00:00 +0000");
        assert_eq!(date(1_700_000_000, 90), "2023-11-14 23:43 +0130");
        assert_eq!(date(1_700_000_000, -480), "2023-11-14 14:13 -0800");
        // A leap day.
        assert_eq!(date(951_782_400, 0), "2000-02-29 00:00 +0000");
        assert_eq!(date(-86_400, 0), "1969-12-31 00:00 +0000");
    }

    /// A commit's timestamp is whatever number its author wrote, so the
    /// limits of `i64` must not overflow (found by the `git_output` fuzz
    /// target: `author A <a> 9223372036854775807 +9959`).
    #[test]
    fn extreme_timestamps_do_not_overflow() {
        let c = commit(
            b"author A <a> 9223372036854775807 +9959\ncommitter B <b> -9223372036854775808 -9959\n\n",
        );
        assert_eq!(c.author.time, i64::MAX);
        assert_eq!(c.committer.time, i64::MIN);
        assert_ne!(date(c.author.time, c.author.offset), "");
        assert_ne!(date(c.committer.time, c.committer.offset), "");
        let mut s = String::new();
        ago(&mut s, c.committer.time, c.author.time);
        assert!(s.ends_with("years"), "{s}");
        ago(&mut s, c.author.time, c.committer.time);
        assert_eq!(s, "now");
    }

    #[test]
    fn ago_rounds_down_to_one_unit() {
        let mut s = String::new();
        let now = 1_000_000_000;
        for (secs, want) in [
            (5, "now"),
            (120, "2 min"),
            (3 * 3600, "3 h"),
            (86_400, "1 day"),
            (3 * 86_400, "3 days"),
            (21 * 86_400, "3 weeks"),
            (90 * 86_400, "3 months"),
            (800 * 86_400, "2 years"),
            (-50, "now"),
        ] {
            ago(&mut s, now - secs, now);
            assert_eq!(s, want, "{secs} s");
        }
    }

    #[test]
    fn versions_parse_with_platform_suffixes() {
        assert_eq!(version("git version 2.43.0\n"), Some((2, 43)));
        assert_eq!(version("git version 2.39.3 (Apple Git-146)"), Some((2, 39)));
        assert_eq!(version("git version 2.47.0.windows.2"), Some((2, 47)));
        assert_eq!(version("hg 6"), None);
    }
}
