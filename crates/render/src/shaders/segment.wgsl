// Anti-aliased curve segments: the lines and dots of the git graph (M27).
//
// "A quad with a distance shader." Each segment is one instance: a cubic
// Bézier curve (four control points), a stroke width and a colour. The
// vertex shader draws a plain rectangle that is sure to contain the stroke;
// the fragment shader then asks, for every pixel of that rectangle, "how far
// is this pixel's centre from the curve?" and turns the answer into how much
// of the pixel the stroke covers:
//
//     coverage = clamp(width / 2 + 0.5 - distance, 0, 1)
//
// A pixel whose centre is deeper than half a pixel inside the stroke is fully
// covered (1); one more than half a pixel outside is untouched (0); in the
// one-pixel band between, coverage falls off linearly. That band is the
// anti-aliased edge. The distance used is *unsigned* (always >= 0, the same
// on both sides of the curve's centre line); subtracting it from the half
// width gives a *signed* distance to the stroke's edge: positive inside,
// negative outside. Because the edge is worked out per pixel from exact
// geometry rather than from a stored image, it stays one pixel soft at any
// zoom or scale factor, with no jaggies and no blur.
//
// The exact distance to a cubic has no simple formula, so the curve is
// flattened into PIECES straight pieces and the distance is the smallest one
// to any piece. The CPU painter (soft.rs, compatibility mode) uses the same
// count, SEGMENT_PIECES in segment.rs; a test checks the two agree.
//
// The prelude is duplicated from rect.wgsl on purpose: clear over clever.

// `linear` is 1 when the target is sRGB (--linear-blend): colours are
// turned into linear light so blending happens there.
struct Globals { viewport: vec2<f32>, linear: f32, pad: f32 };
@group(0) @binding(0) var<uniform> globals: Globals;

const PIECES: u32 = 16u;

struct SegmentIn {
    @location(0) p0: vec2<f32>,    // start, physical pixels
    @location(1) p1: vec2<f32>,    // first control point
    @location(2) p2: vec2<f32>,    // second control point
    @location(3) p3: vec2<f32>,    // end
    @location(4) color: vec4<f32>, // straight (not premultiplied) RGBA
    @location(5) width: f32,       // stroke width in pixels
};

// Every value is the same for the whole instance, so no interpolation:
// `flat` hands the fragment shader the value as written.
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) p01: vec4<f32>,
    @location(1) @interpolate(flat) p23: vec4<f32>,
    @location(2) @interpolate(flat) color: vec4<f32>,
    @location(3) @interpolate(flat) width: f32,
};

// A triangle strip of 4 vertices: 0=(0,0) 1=(1,0) 2=(0,1) 3=(1,1).
fn corner(vi: u32) -> vec2<f32> {
    return vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
}

// Pixels (origin top-left, y down) to clip space ([-1, 1], y up).
fn to_clip(px: vec2<f32>) -> vec4<f32> {
    let ndc = px / globals.viewport * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: SegmentIn) -> VsOut {
    // A cubic never leaves the box around its four control points, so that
    // box, grown by half the width plus a pixel for the soft edge, holds
    // every pixel the stroke can touch.
    let grow = vec2<f32>(inst.width * 0.5 + 1.0);
    let lo = min(min(inst.p0, inst.p1), min(inst.p2, inst.p3)) - grow;
    let hi = max(max(inst.p0, inst.p1), max(inst.p2, inst.p3)) + grow;
    var out: VsOut;
    out.clip = to_clip(mix(lo, hi, corner(vi)));
    out.p01 = vec4<f32>(inst.p0, inst.p1);
    out.p23 = vec4<f32>(inst.p2, inst.p3);
    out.color = inst.color;
    out.width = inst.width;
    return out;
}

// The point at `t` (0 to 1) along the cubic, in Bernstein form.
fn cubic(p0: vec2<f32>, p1: vec2<f32>, p2: vec2<f32>, p3: vec2<f32>, t: f32) -> vec2<f32> {
    let mt = 1.0 - t;
    return p0 * (mt * mt * mt) + p1 * (3.0 * mt * mt * t) + p2 * (3.0 * mt * t * t)
        + p3 * (t * t * t);
}

// Distance from `p` to the straight piece from `a` to `b`: project `p` onto
// the line, clamp to the piece's ends, measure. A piece of zero length (a
// dot) is a point, so the projection factor is guarded against 0 / 0.
fn piece_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-12), 0.0, 1.0);
    return length(pa - ba * h);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // In a fragment shader the position builtin is the pixel's centre in
    // framebuffer pixels (x + 0.5, y + 0.5), origin top-left: the same
    // space as the control points.
    let p = in.clip.xy;
    let p0 = in.p01.xy;
    let p1 = in.p01.zw;
    let p2 = in.p23.xy;
    let p3 = in.p23.zw;
    var d = 1e30;
    var a = p0;
    for (var i = 1u; i <= PIECES; i = i + 1u) {
        let b = cubic(p0, p1, p2, p3, f32(i) / f32(PIECES));
        d = min(d, piece_distance(p, a, b));
        a = b;
    }
    let coverage = clamp(in.width * 0.5 + 0.5 - d, 0.0, 1.0);
    if coverage <= 0.0 {
        discard;
    }
    let alpha = in.color.a * coverage;
    return vec4<f32>(space(in.color.rgb) * alpha, alpha); // premultiply
}

// The sRGB curve undone, per channel: theme colours are written in sRGB.
fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn space(c: vec3<f32>) -> vec3<f32> {
    return select(c, to_linear(c), globals.linear > 0.5);
}
