//! The geometry both painters share for curve segments (M27's git graph).
//!
//! A [`SegmentInstance`] is a stroked cubic Bézier curve. The GPU draws it
//! in `shaders/segment.wgsl` and the CPU painter (compatibility mode) in
//! `soft.rs`; both flatten the curve into [`SEGMENT_PIECES`] straight
//! pieces, measure each pixel centre's distance to the nearest piece, and
//! turn that into coverage with [`coverage`]. Keeping the numbers here, in
//! one place, is what makes the two painters agree.
//!
//! **Flattening.** The exact distance from a point to a cubic needs a
//! fifth-degree equation solved per pixel. Sixteen straight pieces follow a
//! graph curve (a lane change one row tall) to well under a tenth of a
//! pixel, which is invisible, and the distance to a straight piece is a
//! couple of multiplications.

use crate::pipeline::SegmentInstance;

/// How many straight pieces a curve is flattened into. `segment.wgsl` has
/// the same number as `PIECES`; a test in `pipeline.rs` checks they agree.
pub(crate) const SEGMENT_PIECES: usize = 16;

/// The flattened curve: [`SEGMENT_PIECES`] + 1 points from `p0` to `p3`.
pub(crate) type Flat = [[f32; 2]; SEGMENT_PIECES + 1];

/// The point at `t` (0 to 1) along the cubic, in Bernstein form: the same
/// expression as `cubic` in the shader.
fn cubic(s: &SegmentInstance, t: f32) -> [f32; 2] {
    let mt = 1.0 - t;
    let w = [mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t];
    let axis = |k: usize| s.p0[k] * w[0] + s.p1[k] * w[1] + s.p2[k] * w[2] + s.p3[k] * w[3];
    [axis(0), axis(1)]
}

/// Flatten `s` into straight pieces, once per segment.
#[expect(
    clippy::cast_precision_loss,
    reason = "i and SEGMENT_PIECES are at most 16, exact in f32"
)]
pub(crate) fn flatten(s: &SegmentInstance) -> Flat {
    std::array::from_fn(|i| cubic(s, i as f32 / SEGMENT_PIECES as f32))
}

/// Distance from `p` to the piece from `a` to `b`: project `p` onto the
/// line, clamp to the ends, measure. A zero-length piece (a dot) is a
/// point; the guard keeps 0 / 0 out, as in the shader.
fn piece_distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let pa = [p[0] - a[0], p[1] - a[1]];
    let ba = [b[0] - a[0], b[1] - a[1]];
    let len2 = (ba[0] * ba[0] + ba[1] * ba[1]).max(1e-12);
    let h = ((pa[0] * ba[0] + pa[1] * ba[1]) / len2).clamp(0.0, 1.0);
    let (dx, dy) = (pa[0] - ba[0] * h, pa[1] - ba[1] * h);
    (dx * dx + dy * dy).sqrt()
}

/// The unsigned distance from `p` to the flattened curve: the nearest of
/// its pieces.
pub(crate) fn distance(flat: &Flat, p: [f32; 2]) -> f32 {
    flat.windows(2)
        .map(|w| piece_distance(p, w[0], w[1]))
        .fold(f32::INFINITY, f32::min)
}

/// How much of a pixel a stroke `width` wide covers when the pixel's
/// centre is `d` from the curve: 1 well inside, 0 well outside, and a
/// linear ramp one pixel wide across the edge, which is the anti-aliasing.
pub(crate) fn coverage(width: f32, d: f32) -> f32 {
    (width * 0.5 + 0.5 - d).clamp(0.0, 1.0)
}

/// The box `[min x, min y, max x, max y]` the GPU quad covers: the control
/// points' bounding box (a cubic never leaves it) grown by half the width
/// plus a pixel for the soft edge. The vertex shader builds the same box.
pub(crate) fn bounds(s: &SegmentInstance) -> [f32; 4] {
    let grow = s.width * 0.5 + 1.0;
    let xs = [s.p0[0], s.p1[0], s.p2[0], s.p3[0]];
    let ys = [s.p0[1], s.p1[1], s.p2[1], s.p3[1]];
    let lo = |v: [f32; 4]| v.into_iter().fold(f32::INFINITY, f32::min);
    let hi = |v: [f32; 4]| v.into_iter().fold(f32::NEG_INFINITY, f32::max);
    [lo(xs) - grow, lo(ys) - grow, hi(xs) + grow, hi(ys) + grow]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(points: [[f32; 2]; 4], width: f32) -> SegmentInstance {
        SegmentInstance {
            p0: points[0],
            p1: points[1],
            p2: points[2],
            p3: points[3],
            color: [1.0; 4],
            width,
            pad: [0.0; 3],
        }
    }

    #[test]
    fn flattening_starts_and_ends_on_the_endpoints() {
        let s = seg([[1.0, 2.0], [5.0, 2.0], [5.0, 9.0], [9.0, 9.0]], 2.0);
        let flat = flatten(&s);
        assert_eq!(flat[0], [1.0, 2.0]);
        assert_eq!(flat[SEGMENT_PIECES], [9.0, 9.0]);
        // Symmetric S-curve: the middle point is the centre.
        assert!((flat[SEGMENT_PIECES / 2][0] - 5.0).abs() < 1e-5);
        assert!((flat[SEGMENT_PIECES / 2][1] - 5.5).abs() < 1e-5);
    }

    #[test]
    fn distances_to_a_line_and_a_dot() {
        let line = flatten(&seg(
            [[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [30.0, 0.0]],
            1.0,
        ));
        assert!((distance(&line, [15.0, 3.0]) - 3.0).abs() < 1e-5);
        assert!((distance(&line, [34.0, 3.0]) - 5.0).abs() < 1e-5);
        let dot = flatten(&seg([[5.0, 5.0]; 4], 4.0));
        assert!((distance(&dot, [8.0, 9.0]) - 5.0).abs() < 1e-5);
        assert!(distance(&dot, [5.0, 5.0]).abs() < 1e-6);
    }

    #[test]
    fn coverage_ramps_across_one_pixel() {
        assert!((coverage(2.0, 0.0) - 1.0).abs() < f32::EPSILON);
        assert!((coverage(2.0, 1.0) - 0.5).abs() < f32::EPSILON);
        assert!(coverage(2.0, 1.5).abs() < f32::EPSILON);
        assert!(coverage(2.0, 9.0).abs() < f32::EPSILON);
    }

    #[test]
    fn bounds_hold_the_whole_stroke() {
        let s = seg([[10.0, 20.0], [0.0, 25.0], [30.0, 5.0], [15.0, 15.0]], 4.0);
        assert_eq!(bounds(&s), [-3.0, 2.0, 33.0, 28.0]);
    }
}
