//! Editor groups: up to three columns of tabs side by side (M13).
//!
//! Every tab belongs to one group, named by its [`crate::app::Tab::group`]
//! number, and the app's list of tabs stays one flat list: a group's tab
//! strip shows its tabs in that list's order. That keeps everything that
//! works on "every open tab" (saving, file watching, background results
//! found by index) unchanged, while the strip, the focus and the layout
//! are per group.
//!
//! The focused group is the one the keyboard talks to. Its active tab is
//! the app's `active` tab and its find bar is the app's find bar; the
//! other groups keep theirs here until they are focused again, so each
//! group has its own find bar state and its own active tab.

use crate::find::FindBar;

/// The most groups side by side; three already makes narrow columns on a
/// laptop screen (the plan's decision, DESIGN.md §9).
pub const MAX_GROUPS: usize = 3;

/// The narrowest a group can be dragged, in cells.
pub const MIN_CELLS: f32 = 20.0;

/// Half the width of a divider's grab area, in logical pixels.
const GRAB: f32 = 4.0;

/// One group's state while another has the focus.
#[derive(Debug, Default)]
pub struct Group {
    /// Its active tab, as an index into the app's tabs. Kept up to date
    /// for the focused group only when focus moves away.
    pub active: usize,
    /// Its find bar while it is not focused (the focused group's is the
    /// app's own, and this holds a closed placeholder).
    pub find: FindBar,
    /// Its share of the editor area's width; the shares add up to 1.
    pub share: f32,
}

/// The groups, left to right, and which one has the focus.
#[derive(Debug)]
pub struct Groups {
    /// The groups in screen order.
    pub list: Vec<Group>,
    /// The group the keyboard talks to.
    pub focused: usize,
    /// A divider being dragged: the index of the group to its left.
    pub dragging: Option<usize>,
}

impl Default for Groups {
    fn default() -> Self {
        Self {
            list: vec![Group {
                share: 1.0,
                ..Group::default()
            }],
            focused: 0,
            dragging: None,
        }
    }
}

impl Groups {
    /// How many groups there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// The left and right edges of each group's column, in physical
    /// pixels, splitting `left..right` (the editor area right of the
    /// sidebar) by the shares. Edges are rounded so columns meet without
    /// a gap.
    #[must_use]
    pub fn columns(&self, left: f32, right: f32) -> Vec<(f32, f32)> {
        let width = (right - left).max(0.0);
        let mut out = Vec::with_capacity(self.list.len());
        let mut x = left;
        let mut acc = 0.0;
        for (i, g) in self.list.iter().enumerate() {
            acc += g.share;
            let end = if i + 1 == self.list.len() {
                right
            } else {
                (left + width * acc).round()
            };
            out.push((x, end));
            x = end;
        }
        out
    }

    /// The group whose column holds `x`.
    #[must_use]
    pub fn at(columns: &[(f32, f32)], x: f32) -> Option<usize> {
        columns.iter().position(|&(a, b)| x >= a && x < b)
    }

    /// The divider under `x`, as the index of the group to its left.
    #[must_use]
    pub fn on_divider(columns: &[(f32, f32)], x: f32, scale: f32) -> Option<usize> {
        let grab = GRAB * scale;
        columns
            .iter()
            .take(columns.len().saturating_sub(1))
            .position(|&(_, b)| (x - b).abs() <= grab)
    }

    /// Move divider `d` (between groups `d` and `d + 1`) to `x`, keeping
    /// both groups at least `min` pixels wide. `columns` are the current
    /// edges.
    pub fn drag_divider(&mut self, columns: &[(f32, f32)], d: usize, x: f32, min: f32) {
        let (Some(&(a, _)), Some(&(_, b))) = (columns.get(d), columns.get(d + 1)) else {
            return;
        };
        let (Some(first), Some(last)) = (columns.first(), columns.last()) else {
            return;
        };
        let total = (last.1 - first.0).max(1.0);
        if b - a < 2.0 * min {
            return;
        }
        let x = x.clamp(a + min, b - min);
        let pair = self.list[d].share + self.list[d + 1].share;
        let left_share = (x - a) / total;
        self.list[d].share = left_share.min(pair);
        self.list[d + 1].share = (pair - left_share).max(0.0);
    }

    /// Insert a group right of `at`, taking half of that group's width,
    /// and return its index.
    pub fn insert_after(&mut self, at: usize) -> usize {
        let half = self.list.get(at).map_or(1.0, |g| g.share / 2.0);
        if let Some(g) = self.list.get_mut(at) {
            g.share = half;
        }
        let i = (at + 1).min(self.list.len());
        self.list.insert(
            i,
            Group {
                share: half,
                ..Group::default()
            },
        );
        i
    }

    /// Tab `i` of group `g` left the app's list of tabs; `tabs` holds the
    /// group of each tab that remains, in order. Every group's active tab
    /// stays the same tab, and when `i` itself was active, the tab before
    /// it in `g`'s strip (else the one after) takes over. A group left
    /// empty closes: the tabs right of it are renumbered and the focus
    /// moves left if it was there. The focused group's state must be
    /// parked in `list` first.
    pub fn tab_removed(&mut self, tabs: &mut [usize], i: usize, g: usize) {
        let before = (0..i).rev().find(|&t| tabs[t] == g);
        let after = (i..tabs.len()).find(|&t| tabs[t] == g);
        for (h, slot) in self.list.iter_mut().enumerate() {
            if h == g && slot.active == i {
                slot.active = before.or(after).unwrap_or(0);
            } else if slot.active > i {
                slot.active -= 1;
            }
        }
        if before.is_none() && after.is_none() {
            self.close_empty(tabs, g);
        }
    }

    /// Move tab `i` to group `to`, where it becomes the active tab; the
    /// group it leaves behaves as in [`Groups::tab_removed`], and the
    /// focus goes with the tab. The focused group's state must be parked
    /// in `list` first.
    pub fn tab_moved(&mut self, tabs: &mut [usize], i: usize, to: usize) {
        let from = tabs[i];
        if from == to || to >= self.list.len() {
            return;
        }
        let before = (0..i).rev().find(|&t| tabs[t] == from);
        let after = (i + 1..tabs.len()).find(|&t| tabs[t] == from);
        tabs[i] = to;
        self.list[to].active = i;
        self.focused = to;
        match before.or(after) {
            Some(n) => self.list[from].active = n,
            None => self.close_empty(tabs, from),
        }
    }

    /// Remove group `g`, which no tab is in any more.
    fn close_empty(&mut self, tabs: &mut [usize], g: usize) {
        if self.remove(g).is_none() {
            return;
        }
        for t in tabs.iter_mut() {
            if *t > g {
                *t -= 1;
            }
        }
        if self.focused > g || (self.focused == g && g > 0) {
            self.focused -= 1;
        }
    }

    /// Remove group `g`, giving its width to a neighbour. The caller
    /// renumbers the tabs and moves the focus.
    pub fn remove(&mut self, g: usize) -> Option<Group> {
        if g >= self.list.len() || self.list.len() == 1 {
            return None;
        }
        let gone = self.list.remove(g);
        // The left neighbour, or the new first group when the first went.
        let to = g.saturating_sub(1);
        if let Some(n) = self.list.get_mut(to) {
            n.share += gone.share;
        }
        Some(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(n: usize) -> Groups {
        let mut g = Groups::default();
        while g.len() < n {
            g.insert_after(g.len() - 1);
        }
        g
    }

    #[test]
    fn one_group_spans_the_editor_area() {
        let g = Groups::default();
        assert_eq!(g.columns(200.0, 1000.0), vec![(200.0, 1000.0)]);
        assert_eq!(Groups::at(&g.columns(200.0, 1000.0), 500.0), Some(0));
        assert_eq!(Groups::at(&g.columns(200.0, 1000.0), 100.0), None);
    }

    #[test]
    fn splitting_halves_the_group_and_dividers_drag() {
        let mut g = groups(2);
        let cols = g.columns(0.0, 1000.0);
        assert_eq!(cols, vec![(0.0, 500.0), (500.0, 1000.0)]);
        assert_eq!(Groups::on_divider(&cols, 502.0, 1.0), Some(0));
        assert_eq!(Groups::on_divider(&cols, 520.0, 1.0), None);
        g.drag_divider(&cols, 0, 700.0, 100.0);
        assert_eq!(g.columns(0.0, 1000.0), vec![(0.0, 700.0), (700.0, 1000.0)]);
        // Not narrower than the minimum.
        let cols = g.columns(0.0, 1000.0);
        g.drag_divider(&cols, 0, 990.0, 100.0);
        assert_eq!(g.columns(0.0, 1000.0), vec![(0.0, 900.0), (900.0, 1000.0)]);
    }

    #[test]
    fn closing_a_tab_keeps_each_group_on_its_tab() {
        let mut g = groups(2);
        // Tabs 0, 1, 3 in group 0; 2 and 4 in group 1.
        g.list[0].active = 1;
        g.list[1].active = 4;
        let mut tabs = vec![0, 0, 1, 0, 1];
        // Close tab 1, group 0's active: the one before it takes over,
        // and group 1's active index follows its tab down.
        tabs.remove(1);
        g.tab_removed(&mut tabs, 1, 0);
        assert_eq!((g.list[0].active, g.list[1].active), (0, 3));
        // Close tab 0, the first: the next in the strip (old index 2).
        tabs.remove(0);
        g.tab_removed(&mut tabs, 0, 0);
        assert_eq!((g.list[0].active, g.list[1].active), (1, 2));
        assert_eq!(tabs, [1, 0, 1]);
        // Close group 0's last tab: the group goes, group 1 becomes 0.
        g.focused = 1;
        tabs.remove(1);
        g.tab_removed(&mut tabs, 1, 0);
        assert_eq!(g.len(), 1);
        assert_eq!(tabs, [0, 0]);
        assert_eq!((g.focused, g.list[0].active), (0, 1));
    }

    #[test]
    fn moving_the_last_tab_out_closes_its_group() {
        let mut g = groups(3);
        let mut tabs = vec![0, 1, 2, 2];
        g.list[2].active = 3;
        g.focused = 1;
        // Tab 1 is group 1's only tab: moving it right empties group 1,
        // so its new group (2) becomes 1.
        g.tab_moved(&mut tabs, 1, 2);
        assert_eq!(g.len(), 2);
        assert_eq!(tabs, [0, 1, 1, 1]);
        assert_eq!((g.focused, g.list[1].active), (1, 1));
        // Moving tab 2 to group 0: group 1 keeps a tab, the neighbour.
        g.tab_moved(&mut tabs, 2, 0);
        assert_eq!(tabs, [0, 1, 0, 1]);
        assert_eq!((g.focused, g.list[0].active, g.list[1].active), (0, 2, 1));
    }

    #[test]
    fn three_groups_and_removing_one_gives_its_width_away() {
        let mut g = groups(2);
        // Split the right one: it halves.
        g.insert_after(1);
        assert_eq!(
            g.columns(0.0, 1000.0),
            vec![(0.0, 500.0), (500.0, 750.0), (750.0, 1000.0)]
        );
        assert!(g.remove(1).is_some());
        assert_eq!(g.columns(0.0, 1000.0), vec![(0.0, 750.0), (750.0, 1000.0)]);
        assert!(g.remove(0).is_some());
        assert_eq!(g.columns(0.0, 1000.0), vec![(0.0, 1000.0)]);
        assert!(g.remove(0).is_none(), "the last group stays");
        let total: f32 = g.list.iter().map(|x| x.share).sum();
        assert!((total - 1.0).abs() < 1e-6);
    }
}
