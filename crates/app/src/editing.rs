//! The everyday editing operations of M11 on a [`Document`]: lines,
//! comments, selection growth, text transforms, indentation, auto-pairs
//! and the on-save tidying.
//!
//! The pure work is in `nimbus_core` (`lines`, `comment`, `expand`,
//! `transform`, `indent`); this module feeds it the document's text and
//! selections and applies what comes back as one undo step through
//! [`Document::apply_batch`]. Operations that read the selected lines are
//! refused over [`LINE_OP_LIMIT`] bytes, so on a 4 GB file a mistaken
//! Select All followed by Sort Lines says so instead of reading the file.

use std::borrow::Cow;
use std::ops::Range;

use nimbus_core::comment;
use nimbus_core::expand::expand;
use nimbus_core::indent::Indent;
use nimbus_core::lines::{self, Batch, LineError};
use nimbus_core::transform::{self, Case, Lines};
use nimbus_core::{Bias, EditError, Selection, Selections, Text, brackets, movement};

use crate::config::Indenting;
use crate::document::{Document, MAX_COPY_BYTES};

/// Line operations refuse selections whose lines hold more than this:
/// the same 64 MiB as Copy, for the same reason.
pub const LINE_OP_LIMIT: usize = MAX_COPY_BYTES;

/// How far bracket matching and expansion look when there is no syntax
/// tree: enough for any function, cheap on a huge file.
const BRACKET_LIMIT: usize = 64 * 1024;

/// The pairs auto-pairing completes.
const PAIRS: [(u8, u8); 6] = [
    (b'(', b')'),
    (b'[', b']'),
    (b'{', b'}'),
    (b'"', b'"'),
    (b'\'', b'\''),
    (b'`', b'`'),
];

/// What an operation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The text or the selections changed.
    Done,
    /// There was nothing to do (moving the first line up, say).
    Nothing,
    /// Refused: the selected lines are over [`LINE_OP_LIMIT`].
    TooLarge,
    /// Refused: the language has no such comment.
    NoComment,
}

impl From<LineError> for Outcome {
    fn from(e: LineError) -> Self {
        match e {
            LineError::TooLarge => Self::TooLarge,
        }
    }
}

impl Document {
    /// Apply a [`Batch`] from `nimbus_core` as one undo step: its items in
    /// one pass, then its selections, or the current ones moved through
    /// the edits with `bias` when it gives none. Sealed on both sides so
    /// typing never joins it.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub(crate) fn apply_batch(
        &mut self,
        batch: Batch<'_>,
        bias: (Bias, Bias),
    ) -> Result<Outcome, EditError> {
        let Batch { items, after } = batch;
        if items.is_empty() {
            return Ok(match after {
                Some(after) if after != self.carets.selections => {
                    self.set_selections(after);
                    Outcome::Done
                }
                _ => Outcome::Nothing,
            });
        }
        self.history.seal();
        let changed = self.apply_each(items, bias)?;
        if let Some(after) = after {
            self.set_selections(after);
        }
        self.history.seal();
        Ok(if changed {
            Outcome::Done
        } else {
            Outcome::Nothing
        })
    }

    /// Replace the selections, snapped to the text, as an operation leaves
    /// them.
    fn set_selections(&mut self, after: Selections) {
        let t = &self.buffer;
        let mut after = after;
        after.transform(|_, s| Selection {
            anchor: movement::clamp(t, s.anchor),
            head: movement::clamp(t, s.head),
        });
        self.carets.selections = after;
        self.carets.goals.clear();
        self.carets.preferred_column = None;
        self.carets.pairs = None;
    }

    /// Whether the selected lines are small enough for a line operation.
    fn within_limit(&self) -> Result<(), Outcome> {
        Batch::check_size(&self.buffer, &self.carets.selections, LINE_OP_LIMIT)
            .map_err(Outcome::from)
    }

    /// Duplicate the selection, or the cursor's line.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn duplicate(&mut self) -> Result<Outcome, EditError> {
        if let Err(o) = self.within_limit() {
            return Ok(o);
        }
        let batch = lines::duplicate(
            &self.buffer,
            &self.carets.selections,
            self.line_ending.as_str(),
        )
        .into_owned();
        self.apply_batch(batch, (Bias::After, Bias::After))
    }

    /// Move the selected lines up or down by one.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn move_lines(&mut self, down: bool) -> Result<Outcome, EditError> {
        if let Err(o) = self.within_limit() {
            return Ok(o);
        }
        let batch = lines::move_lines(&self.buffer, &self.carets.selections, down).into_owned();
        self.apply_batch(batch, (Bias::After, Bias::After))
    }

    /// Delete the selected lines.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn delete_lines(&mut self) -> Result<Outcome, EditError> {
        if let Err(o) = self.within_limit() {
            return Ok(o);
        }
        let batch = lines::delete_lines(&self.buffer, &self.carets.selections).into_owned();
        self.apply_batch(batch, (Bias::After, Bias::After))
    }

    /// Join the selected lines.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn join_lines(&mut self) -> Result<Outcome, EditError> {
        if let Err(o) = self.within_limit() {
            return Ok(o);
        }
        let batch = lines::join_lines(&self.buffer, &self.carets.selections).into_owned();
        self.apply_batch(batch, (Bias::After, Bias::After))
    }

    /// Open a line above or below each cursor's line.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert_line(&mut self, above: bool) -> Result<Outcome, EditError> {
        let batch = lines::insert_line(
            &self.buffer,
            &self.carets.selections,
            self.line_ending.as_str(),
            above,
        )
        .into_owned();
        self.apply_batch(batch, (Bias::After, Bias::After))
    }

    /// Select the cursor's line; pressed again, the next line too.
    pub fn select_line(&mut self) {
        let after = lines::select_lines(&self.buffer, &self.carets.selections);
        self.set_selections(after);
        self.history.seal();
    }

    /// Comment or uncomment the selected lines with the language's line
    /// comment, or its block comment when it has no line comment.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn toggle_line_comment(&mut self) -> Result<Outcome, EditError> {
        if let Err(o) = self.within_limit() {
            return Ok(o);
        }
        let Some(token) = self.comment_tokens.line else {
            return self.toggle_block_comment();
        };
        let batch = comment::toggle_line(&self.buffer, &self.carets.selections, token).into_owned();
        self.apply_batch(batch, (Bias::Before, Bias::After))
    }

    /// Wrap each selection in the language's block comment, or unwrap it.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn toggle_block_comment(&mut self) -> Result<Outcome, EditError> {
        if let Err(o) = self.within_limit() {
            return Ok(o);
        }
        let Some(tokens) = self.comment_tokens.block else {
            return Ok(Outcome::NoComment);
        };
        let batch =
            comment::toggle_block(&self.buffer, &self.carets.selections, tokens).into_owned();
        self.apply_batch(batch, (Bias::After, Bias::After))
    }

    /// Grow every selection one step: word, pair, line, everything.
    pub fn expand_selection(&mut self) -> Outcome {
        let t = &self.buffer;
        let before = self.carets.selections.clone();
        let mut after = before.clone();
        after.transform(|_, s| expand(t, s, BRACKET_LIMIT).unwrap_or(s));
        if after == before {
            return Outcome::Nothing;
        }
        self.carets.expand_stack.push((before, after.clone()));
        self.set_selections(after);
        self.history.seal();
        Outcome::Done
    }

    /// Undo the last Expand Selection, if nothing else moved since.
    pub fn shrink_selection(&mut self) -> Outcome {
        match self.carets.expand_stack.pop() {
            Some((before, after)) if after == self.carets.selections => {
                self.set_selections(before);
                self.history.seal();
                Outcome::Done
            }
            _ => {
                self.carets.expand_stack.clear();
                Outcome::Nothing
            }
        }
    }

    /// The bracket next to `pos` and its partner: from the syntax tree
    /// when it is up to date, else by counting.
    fn bracket_pair(&self, pos: usize) -> Option<(usize, usize)> {
        let t = &self.buffer;
        self.syntax
            .matching_bracket(t, pos)
            .unwrap_or_else(|| brackets::matching(t, pos, BRACKET_LIMIT))
    }

    /// Move each cursor to the bracket pairing with the one next to it,
    /// or with `select`, select from one bracket to the other.
    pub fn matching_bracket(&mut self, select: bool) -> Outcome {
        let before = self.carets.selections.clone();
        let mut after = before.clone();
        let mut moved = false;
        for i in 0..after.count() {
            let s = after.as_slice()[i];
            let Some((a, b)) = self.bracket_pair(s.head) else {
                continue;
            };
            moved = true;
            let target = match s.head {
                p if p == a => b,
                p if p == a + 1 => b + 1,
                p if p == b => a,
                _ => a + 1,
            };
            let new = if select {
                Selection {
                    anchor: if target > s.head { a } else { b + 1 },
                    head: if target > s.head { b + 1 } else { a },
                }
            } else {
                Selection::cursor(target)
            };
            after.transform(|j, s| if j == i { new } else { s });
        }
        if !moved || after == before {
            return Outcome::Nothing;
        }
        self.set_selections(after);
        self.history.seal();
        Outcome::Done
    }

    /// Change the case of the selected text, or the word under a cursor.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn change_case(&mut self, case: Case) -> Result<Outcome, EditError> {
        if self.selected_len() > LINE_OP_LIMIT {
            return Ok(Outcome::TooLarge);
        }
        let batch =
            transform::change_case(&self.buffer, &self.carets.selections, case).into_owned();
        self.apply_batch(batch, (Bias::Before, Bias::After))
    }

    /// Sort, reverse-sort or deduplicate the selected lines, or all lines
    /// when nothing is selected.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn rewrite_lines(&mut self, op: Lines) -> Result<Outcome, EditError> {
        if let Err(o) = self.whole_or_selected_within_limit() {
            return Ok(o);
        }
        let batch = transform::rewrite_lines(
            &self.buffer,
            &self.carets.selections,
            op,
            self.line_ending.as_str(),
        )
        .into_owned();
        self.apply_batch(batch, (Bias::Before, Bias::After))
    }

    /// Remove trailing whitespace from the selected lines, or every line
    /// when nothing is selected.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn trim_whitespace(&mut self) -> Result<Outcome, EditError> {
        if let Err(o) = self.whole_or_selected_within_limit() {
            return Ok(o);
        }
        let batch = transform::trim_trailing(&self.buffer, &self.carets.selections).into_owned();
        self.apply_batch(batch, (Bias::Before, Bias::Before))
    }

    /// The limit check for operations that take the whole text when
    /// nothing is selected.
    fn whole_or_selected_within_limit(&self) -> Result<(), Outcome> {
        let whole =
            self.carets.selections.count() == 1 && self.carets.selections.primary().is_empty();
        if whole {
            if self.buffer.len_bytes() > LINE_OP_LIMIT {
                return Err(Outcome::TooLarge);
            }
            Ok(())
        } else {
            self.within_limit()
        }
    }

    /// Set the indentation from the config (`settings`) or, when it says
    /// to detect, from the file itself: at open, and again on request.
    /// Returns whether the file decided.
    pub fn configure_indent(&mut self, settings: Indenting) -> bool {
        if settings.detect
            && let Some(found) = Indent::detect(&self.buffer)
        {
            self.indent = found;
            return true;
        }
        self.indent = settings.indent;
        false
    }

    /// Tab with no selection spanning lines: insert one level of
    /// indentation at each cursor, in this file's style, up to the next
    /// stop when it is spaces.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert_indent(&mut self) -> Result<(), EditError> {
        let indent = self.indent;
        let items = self.each(|t, s| {
            let r = s.range();
            let col = movement::column_of(t, r.start, indent.width);
            Some((r, Cow::Owned(indent.to_next_stop(col).into_owned())))
        });
        self.carets.pairs = None;
        self.apply_each(items, (Bias::After, Bias::After)).map(drop)
    }

    /// Enter: insert the line ending, then the current line's indentation,
    /// so a new line starts where the previous one did. After an opening
    /// bracket the new line is one level deeper; and when its closer
    /// follows the cursor, the closer goes on a line of its own below.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn insert_newline(&mut self) -> Result<(), EditError> {
        let ending = self.line_ending.as_str();
        let unit = self.indent.unit();
        let mut after = Vec::new();
        let mut grown = 0usize;
        let mut items: Vec<(Range<usize>, Cow<'_, str>)> = Vec::new();
        for s in self.carets.selections.iter() {
            let t: &dyn Text = &self.buffer;
            let r = s.range();
            let line_start = movement::line_start(t, r.start);
            // Only the indentation before the cursor carries over, so only
            // the line up to the cursor (and at most a few KB of it) is read.
            let head = t.slice(line_start..r.start.min(line_start + nimbus_core::long_line::NEAR));
            let indent_len = head.len() - head.trim_start_matches([' ', '\t']).len();
            let indent = &head[..indent_len];
            let opens = r.start > 0
                && t.byte_at(r.start - 1)
                    .and_then(brackets::partner)
                    .is_some_and(|(_, opens)| opens);
            let closes_next = opens
                && t.byte_at(r.start - 1)
                    .and_then(brackets::partner)
                    .is_some_and(|(close, _)| t.byte_at(r.end) == Some(close));
            let mut text = format!("{ending}{indent}");
            if opens {
                text.push_str(&unit);
            }
            let cursor = r.start + grown + text.len() - r.len();
            if closes_next {
                text.push_str(ending);
                text.push_str(indent);
            }
            grown += text.len();
            grown = grown.wrapping_sub(r.len());
            after.push(Selection::cursor(cursor));
            items.push((r, Cow::Owned(text)));
        }
        let after = Selections::from_vec(after, self.carets.selections.primary_index());
        self.carets.pairs = None;
        self.apply_each(items, (Bias::After, Bias::After))?;
        self.set_selections(after);
        Ok(())
    }

    /// Type `text` at every cursor. With `auto_pairs`, an opening bracket
    /// or quote brings its closer: typed with a selection it wraps the
    /// selection; typed before whitespace, a closer or the end of the
    /// line it inserts the pair and leaves the cursor between; typed
    /// where its closer already waits (because an earlier keystroke put
    /// it there) it steps over it. A closer typed first on a line lines
    /// the line up with its opener's.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn type_text(&mut self, text: &str, auto_pairs: bool) -> Result<(), EditError> {
        let mut bytes = text.bytes();
        let (Some(c), None) = (bytes.next(), bytes.next()) else {
            self.carets.pairs = None;
            return self.insert(text);
        };
        let pair = PAIRS.iter().find(|&&(open, _)| open == c);
        let closes = PAIRS.iter().any(|&(_, close)| close == c);
        if auto_pairs && closes && self.step_over(c) {
            return Ok(());
        }
        if brackets::partner(c).is_some_and(|(_, opens)| !opens) && self.dedent_closer(c)? {
            return Ok(());
        }
        let Some(&(open, close)) = pair.filter(|_| auto_pairs) else {
            // Plain typing: a pair typed into stays a pair.
            let inside = self.pairs_valid();
            self.insert(text)?;
            if let Some(depth) = inside {
                self.carets.pairs = Some((self.carets.selections.clone(), depth));
            }
            return Ok(());
        };
        let t: &dyn Text = &self.buffer;
        let all_selected = self.carets.selections.iter().all(|s| !s.is_empty());
        if all_selected {
            return self.wrap_selections(open, close);
        }
        let quote = open == close;
        let fits = |s: &Selection| {
            let next = t.byte_at(s.head);
            let prev = s.head.checked_sub(1).and_then(|p| t.byte_at(p));
            let next_ok = match next {
                None | Some(b'\n' | b'\r' | b' ' | b'\t' | b')' | b']' | b'}' | b';' | b',') => {
                    true
                }
                Some(n) => quote && n == close,
            };
            let prev_ok = !quote || prev.is_none_or(|p| !(p.is_ascii_alphanumeric() || p == close));
            s.is_empty() && next_ok && prev_ok
        };
        if !self.carets.selections.iter().all(fits) {
            self.carets.pairs = None;
            return self.insert(text);
        }
        let depth = self.pairs_valid().unwrap_or(0) + 1;
        let both = [open, close];
        let both = std::str::from_utf8(&both).unwrap_or(text);
        let items = self.each(|_, s| Some((s.range(), Cow::Borrowed(both))));
        let mut after = Vec::new();
        for (i, s) in self.carets.selections.iter().enumerate() {
            after.push(Selection::cursor(s.head + 1 + 2 * i));
        }
        let after = Selections::from_vec(after, self.carets.selections.primary_index());
        self.apply_each(items, (Bias::After, Bias::After))?;
        self.set_selections(after);
        self.carets.pairs = Some((self.carets.selections.clone(), depth));
        Ok(())
    }

    /// How many auto-pairs the cursors are inside, if the selections are
    /// still the ones the last typing step left and each closer is still
    /// there.
    fn pairs_valid(&self) -> Option<usize> {
        let (recorded, depth) = self.carets.pairs.as_ref()?;
        if *depth == 0 || *recorded != self.carets.selections {
            return None;
        }
        let t = &self.buffer;
        let closers = self.carets.selections.iter().all(|s| {
            s.is_empty()
                && t.byte_at(s.head)
                    .is_some_and(|b| PAIRS.iter().any(|&(_, close)| close == b))
        });
        closers.then_some(*depth)
    }

    /// Typing the closer that auto-pairing put after the cursor: step
    /// over it. Returns whether that happened.
    fn step_over(&mut self, c: u8) -> bool {
        let Some(depth) = self.pairs_valid() else {
            return false;
        };
        let t = &self.buffer;
        if !self
            .carets
            .selections
            .iter()
            .all(|s| t.byte_at(s.head) == Some(c))
        {
            return false;
        }
        let mut after = self.carets.selections.clone();
        after.transform(|_, s| Selection::cursor(s.head + 1));
        self.carets.selections = after;
        self.carets.pairs = (depth > 1).then(|| (self.carets.selections.clone(), depth - 1));
        self.history.seal();
        true
    }

    /// Wrap each selection in `open` and `close`, keeping it selected.
    fn wrap_selections(&mut self, open: u8, close: u8) -> Result<(), EditError> {
        let (open, close) = (
            std::str::from_utf8(std::slice::from_ref(&open)).unwrap_or(""),
            std::str::from_utf8(std::slice::from_ref(&close)).unwrap_or(""),
        );
        let mut items: Vec<(Range<usize>, Cow<'_, str>)> = Vec::new();
        let mut after = Vec::new();
        for (i, s) in self.carets.selections.iter().enumerate() {
            let r = s.range();
            items.push((r.start..r.start, Cow::Borrowed(open)));
            items.push((r.end..r.end, Cow::Borrowed(close)));
            let start = r.start + 2 * i + 1;
            let end = start + r.len();
            after.push(if s.head < s.anchor {
                Selection {
                    anchor: end,
                    head: start,
                }
            } else {
                Selection {
                    anchor: start,
                    head: end,
                }
            });
        }
        let after = Selections::from_vec(after, self.carets.selections.primary_index());
        self.history.seal();
        self.apply_each(items, (Bias::After, Bias::After))?;
        self.set_selections(after);
        self.history.seal();
        Ok(())
    }

    /// A closer typed on a line that holds only whitespace so far: put it
    /// at its opener's indentation, when the line is deeper than that.
    /// Returns whether the closer was typed this way.
    fn dedent_closer(&mut self, c: u8) -> Result<bool, EditError> {
        let t: &dyn Text = &self.buffer;
        let closer = std::str::from_utf8(std::slice::from_ref(&c)).unwrap_or("");
        let mut items: Vec<(Range<usize>, Cow<'_, str>)> = Vec::new();
        for s in self.carets.selections.iter() {
            if !s.is_empty() {
                return Ok(false);
            }
            let line_start = movement::line_start(t, s.head);
            if s.head - line_start > nimbus_core::long_line::NEAR {
                return Ok(false);
            }
            let before = t.slice(line_start..s.head);
            if before.is_empty() || !before.trim_matches([' ', '\t']).is_empty() {
                return Ok(false);
            }
            let Some(open_at) = brackets::opener_before(t, line_start, c, BRACKET_LIMIT) else {
                return Ok(false);
            };
            let open_line = t.line_span(open_at);
            let open_head = t.slice(
                open_line.start..open_at.min(open_line.start + nimbus_core::long_line::NEAR),
            );
            let want =
                &open_head[..open_head.len() - open_head.trim_start_matches([' ', '\t']).len()];
            if want.len() >= before.len() {
                return Ok(false);
            }
            items.push((line_start..s.head, Cow::Owned(format!("{want}{closer}"))));
        }
        self.carets.pairs = None;
        self.apply_each(items, (Bias::After, Bias::After))?;
        Ok(true)
    }

    /// Backspace: between the two halves of a pair auto-pairing just made,
    /// remove both; otherwise as [`Document::delete_back`].
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn backspace(&mut self) -> Result<bool, EditError> {
        if let Some(depth) = self.pairs_valid() {
            let t = &self.buffer;
            let empty_pair = self.carets.selections.iter().all(|s| {
                let prev = s.head.checked_sub(1).and_then(|p| t.byte_at(p));
                let next = t.byte_at(s.head);
                PAIRS
                    .iter()
                    .any(|&(o, c)| prev == Some(o) && next == Some(c))
            });
            if empty_pair {
                let items = self.each(|_, s| Some((s.head - 1..s.head + 1, Cow::Borrowed(""))));
                let changed = self.apply_each(items, (Bias::After, Bias::After))?;
                self.carets.pairs =
                    (depth > 1).then(|| (self.carets.selections.clone(), depth - 1));
                return Ok(changed);
            }
        }
        self.carets.pairs = None;
        self.delete_back()
    }

    /// Before saving: trim trailing whitespace and add a final line break,
    /// as the config asks, as one undo step. A memory-mapped document is
    /// left alone: either would mean reading all of it.
    ///
    /// # Errors
    ///
    /// See [`Document::replace`].
    pub fn tidy_for_save(&mut self, trim: bool, final_newline: bool) -> Result<(), EditError> {
        if (!trim && !final_newline) || self.buffer.is_mapped() {
            return Ok(());
        }
        let t: &dyn Text = &self.buffer;
        let all = Selections::single(Selection::cursor(0));
        let mut batch = if trim {
            transform::trim_trailing(t, &all).into_owned()
        } else {
            Batch::default()
        };
        let len = t.len_bytes();
        if final_newline && len > 0 && t.byte_at(len - 1) != Some(b'\n') {
            batch
                .items
                .push((len..len, Cow::Borrowed(self.line_ending.as_str())));
        }
        self.apply_batch(batch, (Bias::Before, Bias::Before))
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text)
    }

    fn text(d: &Document) -> String {
        d.buffer.slice(0..d.buffer.len_bytes()).into_owned()
    }

    fn cursors(d: &mut Document, at: &[usize]) {
        d.carets.selections = Selections::from_vec(
            at.iter().map(|&p| Selection::cursor(p)).collect(),
            at.len() - 1,
        );
    }

    fn heads(d: &Document) -> Vec<usize> {
        d.carets.selections.iter().map(|s| s.head).collect()
    }

    fn rust(d: &mut Document) {
        d.comment_tokens = nimbus_core::comment::Tokens {
            line: Some("//"),
            block: Some(("/*", "*/")),
        };
    }

    #[test]
    fn line_operations_are_one_undo_step_each_with_one_and_three_cursors() {
        let mut d = doc("a\nb\nc\nd");
        cursors(&mut d, &[2]);
        assert_eq!(d.duplicate().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\nb\nb\nc\nd");
        assert_eq!(d.move_lines(true).unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\nb\nc\nb\nd");
        assert_eq!(d.delete_lines().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\nb\nc\nd");
        assert_eq!(
            d.cursor(),
            6,
            "on the line that took the deleted one's place"
        );
        assert_eq!(d.join_lines().unwrap(), Outcome::Nothing, "last line");
        d.click(4, false);
        assert_eq!(d.join_lines().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\nb\nc d");
        assert_eq!(d.insert_line(false).unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\nb\nc d\n");
        for expected in [
            "a\nb\nc d",
            "a\nb\nc\nd",
            "a\nb\nc\nb\nd",
            "a\nb\nb\nc\nd",
            "a\nb\nc\nd",
        ] {
            assert!(d.undo().unwrap());
            assert_eq!(text(&d), expected);
        }
        cursors(&mut d, &[0, 2, 4]);
        assert_eq!(d.duplicate().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\na\nb\nb\nc\nc\nd");
        assert_eq!(heads(&d), [2, 6, 10]);
        assert!(d.undo().unwrap());
        assert_eq!(text(&d), "a\nb\nc\nd");
        cursors(&mut d, &[0]);
        assert_eq!(
            d.move_lines(false).unwrap(),
            Outcome::Nothing,
            "top line stays"
        );
        d.select_line();
        assert_eq!(d.selection(), Selection { anchor: 0, head: 2 });
        d.select_line();
        assert_eq!(d.selection(), Selection { anchor: 0, head: 4 });
    }

    #[test]
    fn comments_use_the_language_tokens() {
        let mut d = doc("a\nb");
        assert_eq!(
            d.toggle_line_comment().unwrap(),
            Outcome::NoComment,
            "plain text has none"
        );
        rust(&mut d);
        cursors(&mut d, &[0, 2]);
        assert_eq!(d.toggle_line_comment().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "// a\n// b");
        assert_eq!(heads(&d), [3, 8]);
        assert_eq!(d.toggle_line_comment().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\nb");
        d.select(0..1);
        assert_eq!(d.toggle_block_comment().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "/* a */\nb");
        // Markdown: no line comment, so the block form is used.
        let mut m = doc("x");
        m.comment_tokens = nimbus_core::comment::Tokens {
            line: None,
            block: Some(("<!--", "-->")),
        };
        m.select(0..1);
        assert_eq!(m.toggle_line_comment().unwrap(), Outcome::Done);
        assert_eq!(text(&m), "<!-- x -->");
    }

    #[test]
    fn expand_and_shrink_retrace_each_other_and_brackets_match() {
        let mut d = doc("f(a, b)");
        d.click(2, false);
        assert_eq!(d.expand_selection(), Outcome::Done);
        assert_eq!(d.selection().range(), 2..3);
        assert_eq!(d.expand_selection(), Outcome::Done);
        assert_eq!(d.selection().range(), 2..6);
        assert_eq!(d.shrink_selection(), Outcome::Done);
        assert_eq!(d.selection().range(), 2..3);
        d.click(0, false);
        assert_eq!(
            d.shrink_selection(),
            Outcome::Nothing,
            "moved: the stack is gone"
        );
        d.click(1, false);
        assert_eq!(d.matching_bracket(false), Outcome::Done);
        assert_eq!(d.cursor(), 6);
        assert_eq!(d.matching_bracket(false), Outcome::Done);
        assert_eq!(d.cursor(), 1);
        assert_eq!(d.matching_bracket(true), Outcome::Done);
        assert_eq!(d.selection().range(), 1..7);
        d.click(3, false);
        assert_eq!(d.matching_bracket(false), Outcome::Nothing);
    }

    #[test]
    fn transforms_and_limits() {
        let mut d = doc("c\na \nb\nb");
        cursors(&mut d, &[0]);
        // No selection: the whole text.
        assert_eq!(d.rewrite_lines(Lines::Sort).unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a \nb\nb\nc");
        assert_eq!(d.rewrite_lines(Lines::Dedupe).unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a \nb\nc");
        assert_eq!(d.trim_whitespace().unwrap(), Outcome::Done);
        assert_eq!(text(&d), "a\nb\nc");
        assert_eq!(d.trim_whitespace().unwrap(), Outcome::Nothing);
        cursors(&mut d, &[0]);
        assert_eq!(d.change_case(Case::Upper).unwrap(), Outcome::Done);
        assert_eq!(text(&d), "A\nb\nc");
        cursors(&mut d, &[0, 2, 4]);
        assert_eq!(d.change_case(Case::Upper).unwrap(), Outcome::Done);
        assert_eq!(text(&d), "A\nB\nC");
        assert_eq!(heads(&d), [0, 2, 4]);
        assert!(d.undo().unwrap());
        assert_eq!(text(&d), "A\nb\nc", "one step for three cursors");
    }

    #[test]
    fn tab_enter_and_outdent_follow_the_file_indentation() {
        let mut d = doc("fn f() {}");
        d.indent = Indent {
            width: 2,
            tabs: false,
        };
        d.click(8, false);
        d.insert_newline().unwrap();
        assert_eq!(text(&d), "fn f() {\n  \n}");
        assert_eq!(d.cursor(), 11, "on the indented middle line");
        d.insert_indent().unwrap();
        assert_eq!(text(&d), "fn f() {\n    \n}");
        d.type_text("x", true).unwrap();
        d.insert_newline().unwrap();
        assert_eq!(text(&d), "fn f() {\n    x\n    \n}");
        d.type_text("}", true).unwrap();
        assert_eq!(
            text(&d),
            "fn f() {\n    x\n}\n}",
            "the closer lines up with its opener"
        );
        // Tab goes to the next stop.
        let mut d = doc("abc");
        d.indent = Indent {
            width: 4,
            tabs: false,
        };
        d.click(3, false);
        d.insert_indent().unwrap();
        assert_eq!(text(&d), "abc ");
        let mut d = doc("a\n  b");
        d.indent = Indent {
            width: 2,
            tabs: false,
        };
        d.select(0..5);
        d.indent_lines().unwrap();
        assert_eq!(text(&d), "  a\n    b");
        d.outdent_lines().unwrap();
        assert_eq!(text(&d), "a\n  b");
        assert!(d.configure_indent(Indenting {
            indent: Indent::default(),
            detect: true
        }));
        assert_eq!(d.indent.width, 2);
    }

    #[test]
    fn auto_pairs_insert_step_over_wrap_and_delete_together() {
        let mut d = doc("");
        d.type_text("(", true).unwrap();
        assert_eq!(text(&d), "()");
        assert_eq!(d.cursor(), 1);
        d.type_text("a", true).unwrap();
        d.type_text("[", true).unwrap();
        assert_eq!(text(&d), "(a[])");
        d.type_text("]", true).unwrap();
        d.type_text(")", true).unwrap();
        assert_eq!(text(&d), "(a[])", "closers are stepped over");
        assert_eq!(d.cursor(), 5);
        d.type_text(")", true).unwrap();
        assert_eq!(text(&d), "(a[]))", "a closer typed on purpose stays");
        // Backspace inside an empty pair removes both.
        d.type_text("{", true).unwrap();
        assert_eq!(text(&d), "(a[])){}");
        assert!(d.backspace().unwrap());
        assert_eq!(text(&d), "(a[]))");
        // Off: nothing of the sort.
        d.type_text("(", false).unwrap();
        assert_eq!(text(&d), "(a[]))(");
        // Quotes do not pair after a letter (don't), and a selection is
        // wrapped.
        let mut d = doc("don");
        d.click(3, false);
        d.type_text("'", true).unwrap();
        assert_eq!(text(&d), "don'");
        d.select(0..4);
        d.type_text("\"", true).unwrap();
        assert_eq!(text(&d), "\"don'\"");
        assert_eq!(d.selection().range(), 1..5);
        // Before a word nothing pairs: the opener is typed alone.
        let mut d = doc("x");
        d.type_text("(", true).unwrap();
        assert_eq!(text(&d), "(x");
        // Three cursors.
        let mut d = doc("\n\n");
        cursors(&mut d, &[0, 1, 2]);
        d.type_text("\"", true).unwrap();
        assert_eq!(text(&d), "\"\"\n\"\"\n\"\"");
        assert_eq!(heads(&d), [1, 4, 7]);
        d.type_text("\"", true).unwrap();
        assert_eq!(heads(&d), [2, 5, 8]);
    }

    #[test]
    fn saving_can_trim_and_add_a_final_newline_as_one_step() {
        let mut d = doc("a  \nb\t\n\nc");
        d.click(1, false);
        d.tidy_for_save(false, false).unwrap();
        assert_eq!(text(&d), "a  \nb\t\n\nc", "both off: untouched");
        d.tidy_for_save(true, true).unwrap();
        assert_eq!(text(&d), "a\nb\n\nc\n");
        assert_eq!(d.cursor(), 1);
        assert!(d.undo().unwrap());
        assert_eq!(text(&d), "a  \nb\t\n\nc");
        let mut e = doc("a\n");
        e.tidy_for_save(true, true).unwrap();
        assert_eq!(text(&e), "a\n", "nothing to do");
    }
}
