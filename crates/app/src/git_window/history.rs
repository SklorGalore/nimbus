//! The history as compact rows, and its graph (M27).
//!
//! **Compact rows.** A large repository has a million commits, and the
//! window keeps all of them so scrolling never waits. A `Vec` of structs
//! holding `String`s would cost several hundred bytes a commit in
//! allocations alone. Instead each commit is one fixed-size [`Row`] of
//! numbers, and everything of variable length lives in a few shared
//! arrays (*arenas*) that rows point into by offset: the hashes in
//! `oids`, parents' hashes in `parents`, subjects in one `String`, the
//! graph in `graph`. Author names repeat, so each is stored once
//! (*interned*) and a row holds its number. Appending a commit is a few
//! pushes onto existing arrays, and memory stays near the size of the
//! data itself (the budget is 150 bytes a commit plus its subject).
//!
//! **A commit graph.** Commits are nodes, and each points at its
//! parents. git lists them newest first, never a parent before all its
//! children (`--date-order`), so the graph can be laid out in one pass as
//! rows arrive, the way `git log --graph` does. Each *lane* is a column
//! waiting for one commit: when a row's commit arrives, the lanes waiting
//! for it end at its dot; its first parent then continues in its lane,
//! and each other parent (a merge) gets a lane of its own, or joins one
//! already waiting for it. A row records which lanes pass straight
//! through it, which come in to its dot from above and which leave it
//! below; that is all the drawing needs, and only rows on screen are
//! drawn.

use std::collections::HashMap;

use crate::git::parse::LogRecord;

/// The most lanes drawn. A history with more parallel branches (a few
/// open-source projects have hundreds) shows the first ones and an
/// overflow mark where a commit sits further right, as other clients do.
pub const MAX_LANES: usize = 32;

/// The longest object id in bytes: SHA-256 repositories use 32, SHA-1
/// ones 20.
const MAX_OID: usize = 32;

/// An object id in binary, padded to [`MAX_OID`].
type Key = [u8; MAX_OID];

/// One commit. 40 bytes; its hash, parents, subject and graph live in the
/// arenas of [`History`].
#[derive(Debug, Clone, Copy)]
struct Row {
    /// Author time, seconds since 1970.
    time: i64,
    /// Offset of the first parent's hash in `History::parents`, in hashes.
    parents: u32,
    /// Author, as an index into `History::authors`.
    author: u32,
    /// Offset of the subject in `History::subjects`.
    subject: u32,
    /// Offset of this row's graph in `History::graph`.
    graph: u32,
    /// The subject's length in bytes (cut at 64 KB).
    subject_len: u16,
    /// Number of parents (capped at 255; octopus merges rarely pass 30).
    parent_count: u8,
}

const _: () = assert!(size_of::<Row>() == 32);

/// What one row's graph shows: lanes are counted from the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphRow<'a> {
    /// The commit's lane, or [`MAX_LANES`] when it is further right than
    /// the lanes drawn.
    pub lane: usize,
    /// Lanes passing straight through, top to bottom.
    pub pass: &'a [u8],
    /// Lanes ending at the commit's dot, from the top edge.
    pub ins: &'a [u8],
    /// Lanes starting at the commit's dot, to the bottom edge (its
    /// parents).
    pub outs: &'a [u8],
}

/// Assigns lanes as commits arrive.
#[derive(Debug, Default)]
struct Lanes {
    /// Each lane's awaited commit, or `None` when the lane is free.
    active: Vec<Option<Key>>,
}

impl Lanes {
    /// Place a commit with id `key` and parents `parents`; append its row
    /// to `out` and return its lane.
    fn place(&mut self, key: &Key, parents: &[Key], out: &mut Vec<u8>) -> usize {
        let mut ins = Vec::new();
        let mut pass = Vec::new();
        for (l, waiting) in self.active.iter().enumerate() {
            match waiting {
                Some(k) if k == key => ins.push(l),
                Some(_) => pass.push(l),
                None => {}
            }
        }
        let lane = match ins.first() {
            Some(&l) => l,
            None => self.free(),
        };
        for &l in &ins {
            if let Some(slot) = self.active.get_mut(l) {
                *slot = None;
            }
        }
        let mut outs = Vec::new();
        if let Some((first, rest)) = parents.split_first() {
            self.set(lane, *first);
            outs.push(lane);
            for p in rest {
                let l = if let Some(l) = self.active.iter().position(|w| w.as_ref() == Some(p)) {
                    l
                } else {
                    let l = self.free();
                    self.set(l, *p);
                    l
                };
                if !outs.contains(&l) {
                    outs.push(l);
                }
            }
        }
        while self.active.last().is_some_and(Option::is_none) {
            self.active.pop();
        }
        for list in [&pass, &ins, &outs] {
            let kept: Vec<u8> = list
                .iter()
                .filter_map(|&l| u8::try_from(l).ok().filter(|&l| usize::from(l) < MAX_LANES))
                .collect();
            out.push(u8::try_from(kept.len()).unwrap_or(u8::MAX));
            out.extend_from_slice(&kept);
        }
        lane
    }

    /// The first free lane, made if none is.
    fn free(&mut self) -> usize {
        if let Some(l) = self.active.iter().position(Option::is_none) {
            return l;
        }
        self.active.push(None);
        self.active.len() - 1
    }

    fn set(&mut self, lane: usize, key: Key) {
        if let Some(slot) = self.active.get_mut(lane) {
            *slot = Some(key);
        }
    }
}

/// Hex to binary into a [`Key`]; the number of bytes, or `None` for a
/// string that is not an even run of hex digits up to 64 long.
fn decode(hex: &str, key: &mut Key) -> Option<usize> {
    let bytes = hex.as_bytes();
    if !bytes.len().is_multiple_of(2) || bytes.len() > 2 * MAX_OID || bytes.is_empty() {
        return None;
    }
    let digit = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    *key = [0; MAX_OID];
    for (slot, &[hi, lo]) in key.iter_mut().zip(bytes.as_chunks::<2>().0) {
        *slot = digit(hi)? << 4 | digit(lo)?;
    }
    Some(bytes.len() / 2)
}

/// Binary to hex.
fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(HEX[usize::from(b >> 4)]));
        s.push(char::from(HEX[usize::from(b & 15)]));
    }
    s
}

/// The whole history, as rows arrive from `git log`.
#[derive(Debug, Default)]
pub struct History {
    /// Bytes per object id: 20 or 32, set by the first commit.
    width: usize,
    rows: Vec<Row>,
    oids: Vec<u8>,
    parents: Vec<u8>,
    subjects: String,
    authors: Vec<String>,
    author_ids: HashMap<String, u32>,
    graph: Vec<u8>,
    lanes: Lanes,
    /// The widest lane used so far, plus one, capped at [`MAX_LANES`].
    pub lanes_used: usize,
    /// The log ended (`Ok`) or failed (with git's message).
    pub finished: Option<Result<(), String>>,
}

impl History {
    /// Append one commit. Malformed records (an id of another length than
    /// the first commit's) are skipped.
    pub fn push(&mut self, rec: &LogRecord<'_>) {
        let mut key = [0; MAX_OID];
        let Some(width) = decode(rec.oid, &mut key) else {
            return;
        };
        if self.width == 0 {
            self.width = width;
        }
        if width != self.width {
            return;
        }
        let mut parents = Vec::new();
        for p in rec.parents.split(' ').filter(|p| !p.is_empty()) {
            let mut pk = [0; MAX_OID];
            if decode(p, &mut pk) == Some(width) {
                parents.push(pk);
            }
        }
        let graph = u32::try_from(self.graph.len()).unwrap_or(u32::MAX);
        let lane = self.lanes.place(&key, &parents, &mut self.graph);
        self.graph.insert(
            graph as usize,
            u8::try_from(lane.min(MAX_LANES)).unwrap_or(u8::MAX),
        );
        self.lanes_used = self.lanes_used.max((lane + 1).min(MAX_LANES));
        let parents_at = self.parents.len() / width;
        for p in parents.iter().take(usize::from(u8::MAX)) {
            self.parents
                .extend_from_slice(p.get(..width).unwrap_or_default());
        }
        let author = self.intern(rec.author);
        // Subjects are one line; a pathological one is cut, at a
        // character boundary.
        let mut cut = rec.subject.len().min(usize::from(u16::MAX));
        while !rec.subject.is_char_boundary(cut) {
            cut -= 1;
        }
        let subject = rec.subject.get(..cut).unwrap_or_default();
        let row = Row {
            time: rec.time,
            parents: u32::try_from(parents_at).unwrap_or(u32::MAX),
            author,
            subject: u32::try_from(self.subjects.len()).unwrap_or(u32::MAX),
            graph,
            subject_len: u16::try_from(subject.len()).unwrap_or(u16::MAX),
            parent_count: u8::try_from(parents.len()).unwrap_or(u8::MAX),
        };
        self.subjects.push_str(subject);
        self.oids
            .extend_from_slice(key.get(..width).unwrap_or_default());
        self.rows.push(row);
    }

    fn intern(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.author_ids.get(name) {
            return id;
        }
        let id = u32::try_from(self.authors.len()).unwrap_or(u32::MAX);
        self.authors.push(name.to_owned());
        self.author_ids.insert(name.to_owned(), id);
        id
    }

    /// Number of commits so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether no commit has arrived.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn oid_bytes(&self, i: usize) -> &[u8] {
        self.oids
            .get(i * self.width..(i + 1) * self.width)
            .unwrap_or_default()
    }

    /// Row `i`'s full hash.
    #[must_use]
    pub fn oid(&self, i: usize) -> String {
        encode(self.oid_bytes(i))
    }

    #[cfg(test)]
    /// Row `i`'s hash, shortened to 8 digits for the list.
    #[must_use]
    pub fn short_oid(&self, i: usize) -> String {
        encode(self.oid_bytes(i).get(..4).unwrap_or_default())
    }

    /// Row `i`'s subject.
    #[must_use]
    pub fn subject(&self, i: usize) -> &str {
        self.rows.get(i).map_or("", |r| {
            let start = r.subject as usize;
            self.subjects
                .get(start..start + usize::from(r.subject_len))
                .unwrap_or("")
        })
    }

    /// Row `i`'s author.
    #[must_use]
    pub fn author(&self, i: usize) -> &str {
        self.rows
            .get(i)
            .and_then(|r| self.authors.get(r.author as usize))
            .map_or("", String::as_str)
    }

    /// Row `i`'s author time, seconds since 1970.
    #[must_use]
    pub fn time(&self, i: usize) -> i64 {
        self.rows.get(i).map_or(0, |r| r.time)
    }

    /// Row `i`'s parents' hashes, first parent first.
    #[must_use]
    pub fn parents(&self, i: usize) -> Vec<String> {
        let Some(r) = self.rows.get(i) else {
            return Vec::new();
        };
        (0..usize::from(r.parent_count))
            .map(|k| {
                let at = (r.parents as usize + k) * self.width;
                encode(self.parents.get(at..at + self.width).unwrap_or_default())
            })
            .collect()
    }

    /// Row `i`'s graph.
    #[must_use]
    pub fn graph(&self, i: usize) -> GraphRow<'_> {
        let empty = GraphRow {
            lane: 0,
            pass: &[],
            ins: &[],
            outs: &[],
        };
        let Some(r) = self.rows.get(i) else {
            return empty;
        };
        let at = r.graph as usize;
        let mut rest = self.graph.get(at..).unwrap_or_default();
        let Some((&lane, tail)) = rest.split_first() else {
            return empty;
        };
        rest = tail;
        let mut take = || {
            let (&n, tail) = rest.split_first()?;
            let (list, tail) = tail.split_at_checked(usize::from(n))?;
            rest = tail;
            Some(list)
        };
        let (Some(pass), Some(ins), Some(outs)) = (take(), take(), take()) else {
            return empty;
        };
        GraphRow {
            lane: usize::from(lane),
            pass,
            ins,
            outs,
        }
    }

    /// The row of the commit with hash `hex`, searched from the top: a
    /// scan of the packed hashes, about 20 MB for a million commits,
    /// which takes milliseconds and saves keeping a map of them.
    #[must_use]
    pub fn find(&self, hex: &str) -> Option<usize> {
        let mut key = [0; MAX_OID];
        let width = decode(hex, &mut key)?;
        if width != self.width || width == 0 {
            return None;
        }
        let want = key.get(..width)?;
        self.oids.chunks_exact(width).position(|c| c == want)
    }

    /// The log ended (`Ok`) or failed: record it and give back the
    /// arrays' spare room. Growing by doubling leaves up to half of each
    /// unused, which for a million commits is tens of megabytes.
    pub fn finish(&mut self, outcome: Result<(), String>) {
        self.finished = Some(outcome);
        self.rows.shrink_to_fit();
        self.oids.shrink_to_fit();
        self.parents.shrink_to_fit();
        self.subjects.shrink_to_fit();
        self.graph.shrink_to_fit();
    }

    #[cfg(test)]
    /// Bytes held, for the budget check: every array's capacity, the
    /// authors and their map.
    #[must_use]
    pub fn bytes(&self) -> usize {
        let authors: usize = self.authors.iter().map(|a| 2 * a.capacity() + 48).sum();
        self.rows.capacity() * size_of::<Row>()
            + self.oids.capacity()
            + self.parents.capacity()
            + self.subjects.capacity()
            + self.graph.capacity()
            + authors
            + self.author_ids.capacity() * (size_of::<String>() + 4)
    }

    #[cfg(test)]
    /// The subjects' share of [`History::bytes`], which the budget
    /// leaves out.
    #[must_use]
    pub fn subject_bytes(&self) -> usize {
        self.subjects.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(n: u8) -> String {
        encode(&[n; 20])
    }

    /// Push commits given as (id, parent ids) in log order.
    fn history(commits: &[(u8, &[u8])]) -> History {
        let mut h = History::default();
        for (id, parents) in commits {
            let oid = hex(*id);
            let parents: Vec<String> = parents.iter().map(|p| hex(*p)).collect();
            let parents = parents.join(" ");
            let subject = format!("commit {id}");
            h.push(&LogRecord {
                oid: &oid,
                parents: &parents,
                author: if id % 2 == 0 { "Even" } else { "Odd" },
                time: i64::from(*id),
                subject: &subject,
            });
        }
        h
    }

    fn lanes(h: &History, i: usize) -> (usize, Vec<u8>, Vec<u8>, Vec<u8>) {
        let g = h.graph(i);
        (g.lane, g.pass.to_vec(), g.ins.to_vec(), g.outs.to_vec())
    }

    #[test]
    fn a_straight_line_stays_in_one_lane() {
        let h = history(&[(3, &[2]), (2, &[1]), (1, &[])]);
        assert_eq!(lanes(&h, 0), (0, vec![], vec![], vec![0]));
        assert_eq!(lanes(&h, 1), (0, vec![], vec![0], vec![0]));
        assert_eq!(lanes(&h, 2), (0, vec![], vec![0], vec![]));
        assert_eq!(h.lanes_used, 1);
    }

    #[test]
    fn a_merge_opens_a_lane_and_the_fork_closes_it() {
        // 5 merges 4 (main) and 3 (side); both come from 1.
        //   5 = merge(4, 3); 4 -> 2 -> 1; 3 -> 1
        let h = history(&[(5, &[4, 3]), (4, &[2]), (3, &[1]), (2, &[1]), (1, &[])]);
        // The merge: its first parent below in lane 0, the second in 1.
        assert_eq!(lanes(&h, 0), (0, vec![], vec![], vec![0, 1]));
        // 4 in lane 0, the side lane passes by.
        assert_eq!(lanes(&h, 1), (0, vec![1], vec![0], vec![0]));
        // 3 on the side lane; its parent 1 continues there.
        assert_eq!(lanes(&h, 2), (1, vec![0], vec![1], vec![1]));
        assert_eq!(lanes(&h, 3), (0, vec![1], vec![0], vec![0]));
        // 1: both lanes come in, the side one curving to lane 0.
        assert_eq!(lanes(&h, 4), (0, vec![], vec![0, 1], vec![]));
        assert_eq!(h.lanes_used, 2);
    }

    #[test]
    fn a_new_branch_tip_takes_the_first_free_lane() {
        // Two unrelated tips: 4 -> 2, 3 -> 1, then the roots.
        let h = history(&[(4, &[2]), (3, &[1]), (2, &[]), (1, &[])]);
        assert_eq!(lanes(&h, 0).0, 0);
        assert_eq!(lanes(&h, 1), (1, vec![0], vec![], vec![1]));
        assert_eq!(lanes(&h, 2), (0, vec![1], vec![0], vec![]));
        // Lane 0 is free again but 1 still waits for its commit.
        assert_eq!(lanes(&h, 3), (1, vec![], vec![1], vec![]));
    }

    #[test]
    fn rows_keep_their_fields_and_authors_are_shared() {
        let h = history(&[(2, &[1]), (1, &[])]);
        assert_eq!(h.len(), 2);
        assert_eq!(h.oid(0), hex(2));
        assert_eq!(h.short_oid(0), "02020202");
        assert_eq!(h.subject(1), "commit 1");
        assert_eq!(h.author(0), "Even");
        assert_eq!(h.time(1), 1);
        assert_eq!(h.parents(0), [hex(1)]);
        assert_eq!(h.parents(1).len(), 0);
        assert_eq!(h.find(&hex(1)), Some(1));
        assert_eq!(h.find(&hex(9)), None);
        assert_eq!(h.find("zz"), None);
        assert_eq!(h.authors.len(), 2);
        // Out of range reads are empty, not panics.
        assert_eq!(h.subject(5), "");
        assert_eq!(h.graph(5).lane, 0);
    }

    #[test]
    fn lanes_past_the_cap_are_not_stored() {
        // MAX_LANES + 4 tips, each with its own root below.
        let n = u8::try_from(MAX_LANES + 4).unwrap();
        let mut commits: Vec<(u8, Vec<u8>)> = (0..n).map(|i| (100 + i, vec![i + 1])).collect();
        commits.extend((0..n).map(|i| (i + 1, vec![])));
        let as_slices: Vec<(u8, &[u8])> = commits.iter().map(|(i, p)| (*i, p.as_slice())).collect();
        let h = history(&as_slices);
        let last_tip = lanes(&h, usize::from(n) - 1);
        assert_eq!(last_tip.0, MAX_LANES, "an overflowing commit is marked");
        assert!(last_tip.1.iter().all(|&l| usize::from(l) < MAX_LANES));
        assert_eq!(last_tip.1.len(), MAX_LANES);
        assert_eq!(h.lanes_used, MAX_LANES);
    }

    #[test]
    fn rows_stay_within_the_memory_budget() {
        // A long line of commits with a merge every tenth: well under the
        // budget of 150 bytes a commit, subjects aside.
        let mut h = History::default();
        let n = 20_000u32;
        let id = |i: u32| format!("{i:040x}");
        for i in (1..=n).rev() {
            let parents = if i % 10 == 0 && i > 2 {
                format!("{} {}", id(i - 1), id(i - 2))
            } else if i > 1 {
                id(i - 1)
            } else {
                String::new()
            };
            let oid = id(i);
            h.push(&LogRecord {
                oid: &oid,
                parents: &parents,
                author: if i % 3 == 0 { "A" } else { "B" },
                time: i64::from(i),
                subject: "a subject of a typical length, forty-ish",
            });
        }
        h.finish(Ok(()));
        let per = (h.bytes() - h.subject_bytes()) / h.len();
        assert!(per < 150, "{per} bytes per commit");
        assert!(per > 60, "{per}: the count misses something");
    }
}
