//! Shelf packing: deciding where each glyph bitmap goes inside the atlas texture.
//!
//! The atlas is one big texture holding many small glyph images. A shelf
//! packer fills it like a bookcase: it opens a horizontal shelf as tall as
//! the first glyph placed on it, then places later glyphs left to right on
//! the first shelf they fit on. When no shelf fits, it opens a new one below
//! the last. Glyphs from one font at one size have similar heights, so very
//! little space is wasted, and allocation is a short linear scan.
//!
//! This module only does the bookkeeping. It knows nothing about the GPU,
//! which keeps it easy to test.

/// A rectangle inside the atlas, in texels, origin top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub w: u32,
    /// Height.
    pub h: u32,
}

#[derive(Debug, Clone, Copy)]
struct Shelf {
    y: u32,
    height: u32,
    /// Next free x position on this shelf.
    cursor: u32,
}

/// Allocates rectangles in a fixed-size area using shelves.
#[derive(Debug)]
pub struct ShelfPacker {
    width: u32,
    height: u32,
    /// Empty texels left around every allocation, so linear filtering or
    /// rounding at the edge of one glyph never samples its neighbour.
    padding: u32,
    shelves: Vec<Shelf>,
    used_area: u64,
}

/// Shelves may be taller than the glyph by this fraction before we prefer
/// opening a new shelf, so a tiny glyph (a period) does not waste a tall shelf
/// meant for capital letters when room for a fitting shelf remains.
const SHELF_SLACK_NUM: u32 = 3;
const SHELF_SLACK_DEN: u32 = 2;

impl ShelfPacker {
    /// An empty packer for a `width` × `height` area, leaving `padding`
    /// texels between allocations.
    #[must_use]
    pub fn new(width: u32, height: u32, padding: u32) -> Self {
        Self {
            width,
            height,
            padding,
            shelves: Vec::new(),
            used_area: 0,
        }
    }

    /// Reserve a `w` × `h` rectangle. Returns `None` when the area is full;
    /// the caller decides whether to clear and start again.
    ///
    /// Zero-sized requests (a space character has no bitmap) succeed with an
    /// empty rect at the origin and use no space.
    pub fn allocate(&mut self, w: u32, h: u32) -> Option<Rect> {
        if w == 0 || h == 0 {
            return Some(Rect { x: 0, y: 0, w, h });
        }
        let pw = w.checked_add(self.padding)?;
        let ph = h.checked_add(self.padding)?;
        if pw > self.width || ph > self.height {
            return None;
        }

        // Best existing shelf: tall enough, not wastefully tall, with room;
        // among those, the shortest, which keeps tall shelves for tall glyphs.
        let mut best: Option<usize> = None;
        for (i, shelf) in self.shelves.iter().enumerate() {
            let fits_height = shelf.height >= ph;
            let not_wasteful = shelf.height * SHELF_SLACK_DEN <= ph * SHELF_SLACK_NUM;
            let fits_width = self.width - shelf.cursor >= pw;
            let shorter = best.is_none_or(|b| self.shelves[b].height > shelf.height);
            if fits_height && not_wasteful && fits_width && shorter {
                best = Some(i);
            }
        }

        let index = match best {
            Some(i) => i,
            None => match self.open_shelf(ph) {
                Some(i) => i,
                // No room for a fitting new shelf: accept any shelf with room,
                // even a wastefully tall one, before reporting full.
                None => self
                    .shelves
                    .iter()
                    .position(|s| s.height >= ph && self.width - s.cursor >= pw)?,
            },
        };

        let chosen = &mut self.shelves[index];
        let rect = Rect {
            x: chosen.cursor,
            y: chosen.y,
            w,
            h,
        };
        chosen.cursor += pw;
        self.used_area += u64::from(w) * u64::from(h);
        Some(rect)
    }

    fn open_shelf(&mut self, height: u32) -> Option<usize> {
        let y = self.shelves.last().map_or(0, |s| s.y + s.height);
        if self.height - y < height {
            return None;
        }
        self.shelves.push(Shelf {
            y,
            height,
            cursor: 0,
        });
        Some(self.shelves.len() - 1)
    }

    /// Forget every allocation.
    pub fn clear(&mut self) {
        self.shelves.clear();
        self.used_area = 0;
    }

    /// Fraction of the area covered by allocated glyphs, from 0.0 to 1.0.
    /// Padding and shelf slack count as unused.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "an approximate ratio in 0..=1 for logging; f32 precision is plenty"
    )]
    pub fn occupancy(&self) -> f32 {
        let total = u64::from(self.width) * u64::from(self.height);
        if total == 0 {
            return 0.0;
        }
        (self.used_area as f64 / total as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    }

    #[test]
    fn fills_left_to_right_then_opens_a_new_shelf() {
        let mut p = ShelfPacker::new(100, 100, 0);
        let a = p.allocate(40, 10).unwrap();
        let b = p.allocate(40, 10).unwrap();
        let c = p.allocate(40, 10).unwrap();
        assert_eq!((a.x, a.y), (0, 0));
        assert_eq!((b.x, b.y), (40, 0));
        assert_eq!((c.x, c.y), (0, 10));
    }

    #[test]
    fn padding_separates_allocations() {
        let mut p = ShelfPacker::new(100, 100, 2);
        let a = p.allocate(10, 10).unwrap();
        let b = p.allocate(10, 10).unwrap();
        assert_eq!(b.x, a.x + 10 + 2);
    }

    #[test]
    fn rejects_when_full_and_when_too_big() {
        let mut p = ShelfPacker::new(20, 20, 0);
        assert!(p.allocate(21, 1).is_none());
        assert!(p.allocate(20, 20).is_some());
        assert!(p.allocate(1, 1).is_none());
    }

    #[test]
    fn small_glyph_does_not_take_a_tall_shelf_when_space_remains() {
        let mut p = ShelfPacker::new(100, 100, 0);
        let tall = p.allocate(10, 30).unwrap();
        let dot = p.allocate(4, 4).unwrap();
        assert_eq!(tall.y, 0);
        assert_eq!(dot.y, 30, "dot should open its own short shelf");
    }

    #[test]
    fn falls_back_to_a_tall_shelf_when_no_room_for_a_new_one() {
        let mut p = ShelfPacker::new(100, 30, 0);
        p.allocate(10, 30).unwrap();
        let dot = p.allocate(4, 4).unwrap();
        assert_eq!((dot.x, dot.y), (10, 0));
    }

    #[test]
    fn zero_sized_requests_use_no_space() {
        let mut p = ShelfPacker::new(10, 10, 1);
        assert!(p.allocate(0, 5).is_some());
        assert!(p.allocate(9, 9).is_some());
    }

    #[test]
    fn many_random_allocations_never_overlap_or_escape() {
        // A tiny deterministic LCG keeps the test free of dependencies.
        let mut seed: u32 = 12345;
        let mut next = move || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) % 24 + 1
        };
        let mut p = ShelfPacker::new(256, 256, 1);
        let mut placed = Vec::new();
        while let Some(r) = p.allocate(next(), next()) {
            assert!(r.x + r.w <= 256 && r.y + r.h <= 256);
            placed.push(r);
        }
        for (i, a) in placed.iter().enumerate() {
            for b in &placed[i + 1..] {
                assert!(!overlaps(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
        assert!(p.occupancy() > 0.5, "occupancy {}", p.occupancy());
        p.clear();
        assert!(p.occupancy().abs() < f32::EPSILON);
    }
}
