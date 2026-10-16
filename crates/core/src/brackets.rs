//! Finding the bracket that pairs with the one at the cursor (M8).
//!
//! The bracket just after the cursor, or failing that just before it, is
//! paired with its partner by counting nesting: `(` adds a level, `)`
//! removes one, and the partner is where the count returns to zero. Only
//! brackets of the same kind count, so `( [ )` still pairs the round ones.
//!
//! This scan does not know about strings or comments, so a `)` inside a
//! string can mislead it. Where there is a syntax tree, the editor asks the
//! tree instead (`nimbus_syntax`), and uses this only for files without one:
//! huge files, plain text, languages with simple rules. It reads at most
//! `limit` bytes each way, so a missing partner in a 4 GB file costs a few
//! kilobytes, not a scan of the file.

use crate::Text;

/// The bracket pairs that are matched.
const PAIRS: [(u8, u8); 3] = [(b'(', b')'), (b'[', b']'), (b'{', b'}')];

/// For the bracket byte `b`: its partner, and whether it opens.
#[must_use]
pub fn partner(b: u8) -> Option<(u8, bool)> {
    PAIRS.iter().find_map(|&(open, close)| {
        if b == open {
            Some((close, true))
        } else if b == close {
            Some((open, false))
        } else {
            None
        }
    })
}

/// Where the brackets to match could be for a cursor at `pos`: the byte
/// after it first, then the byte before.
#[must_use]
pub fn candidates(t: &dyn Text, pos: usize) -> [Option<usize>; 2] {
    let at = |p: usize| t.byte_at(p).and_then(partner).map(|_| p);
    [at(pos), pos.checked_sub(1).and_then(at)]
}

/// The bracket next to `pos` and its partner, as two byte offsets in text
/// order, by counting nesting within `limit` bytes. `None` if there is no
/// bracket next to the cursor or no partner within reach.
#[must_use]
pub fn matching(t: &dyn Text, pos: usize, limit: usize) -> Option<(usize, usize)> {
    candidates(t, pos)
        .into_iter()
        .flatten()
        .find_map(|at| partner_of(t, at, limit).map(|other| (at.min(other), at.max(other))))
}

/// The partner of the bracket at `at`, within `limit` bytes.
fn partner_of(t: &dyn Text, at: usize, limit: usize) -> Option<usize> {
    let me = t.byte_at(at)?;
    let (other, opens) = partner(me)?;
    let mut window = Vec::new();
    let mut depth = 0usize;
    if opens {
        let end = (at + 1 + limit).min(t.len_bytes());
        t.read_into(at + 1..end, &mut window).ok()?;
        for (i, &b) in window.iter().enumerate() {
            if b == me {
                depth += 1;
            } else if b == other {
                if depth == 0 {
                    return Some(at + 1 + i);
                }
                depth -= 1;
            }
        }
    } else {
        let start = at.saturating_sub(limit);
        t.read_into(start..at, &mut window).ok()?;
        for (i, &b) in window.iter().enumerate().rev() {
            if b == me {
                depth += 1;
            } else if b == other {
                if depth == 0 {
                    return Some(start + i);
                }
                depth -= 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RopeBuffer;

    #[test]
    fn brackets_pair_by_nesting_from_either_side() {
        let t = RopeBuffer::from_text("f(a[1], (b)) {x}");
        // After the cursor: the "(" at 1 pairs with the ")" at 11.
        assert_eq!(matching(&t, 1, 1024), Some((1, 11)));
        // Before the cursor: the ")" at 11 pairs back to 1.
        assert_eq!(matching(&t, 12, 1024), Some((1, 11)));
        assert_eq!(matching(&t, 3, 1024), Some((3, 5)), "square ones");
        assert_eq!(matching(&t, 13, 1024), Some((13, 15)), "curly ones");
        assert_eq!(matching(&t, 7, 1024), None, "not next to a bracket");
    }

    #[test]
    fn a_missing_or_distant_partner_is_none() {
        let t = RopeBuffer::from_text("((x)");
        assert_eq!(matching(&t, 0, 1024), None);
        let far = format!("({})", "x".repeat(100));
        let t = RopeBuffer::from_text(&far);
        assert_eq!(matching(&t, 0, 50), None, "beyond the limit");
        assert_eq!(matching(&t, 0, 200), Some((0, 101)));
    }
}
