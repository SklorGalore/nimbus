// Solid rectangles: backgrounds, cursor, selection, gutter.

// Viewport size in physical pixels, padded to 16 bytes for uniform layout rules.
// `linear` is 1 when the target is sRGB (--linear-blend): colours are
// turned into linear light so blending happens there.
struct Globals { viewport: vec2<f32>, linear: f32, pad: f32 };
@group(0) @binding(0) var<uniform> globals: Globals;

struct RectIn {
    @location(0) pos: vec2<f32>,   // top-left corner, physical pixels
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>, // straight (not premultiplied) RGBA
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
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
fn vs_main(@builtin(vertex_index) vi: u32, inst: RectIn) -> VsOut {
    var out: VsOut;
    out.clip = to_clip(inst.pos + corner(vi) * inst.size);
    out.color = inst.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(space(in.color.rgb) * in.color.a, in.color.a); // premultiply
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
