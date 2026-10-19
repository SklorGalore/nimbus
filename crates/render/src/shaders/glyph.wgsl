// Glyph quads textured from the atlas: coverage masks tinted by the text
// colour, or colour images (emoji) drawn as they are.
// The prelude is duplicated from rect.wgsl on purpose: clear over clever.

// `linear` is 1 when the target is sRGB (--linear-blend): colours are
// turned into linear light so blending happens there.
// `top` is the inset kept for a title bar the app draws: every position
// moves down by it, so y = 0 is just below the title bar and the bar
// itself is drawn at negative y.
struct Globals { viewport: vec2<f32>, linear: f32, top: f32 };
@group(0) @binding(0) var<uniform> globals: Globals;

@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;
// The colour page: premultiplied RGBA. A 1x1 stand-in until the first emoji.
@group(1) @binding(2) var color_tex: texture_2d<f32>;

const GLYPH_COLOR: u32 = 1u;

// Above 1 thickens text slightly; light text on a dark background otherwise
// looks thin when blended in gamma space. Tune by eye against native text.
const GAMMA_ADJUST: f32 = 1.4;

struct GlyphIn {
    @location(0) pos: vec2<f32>,   // top-left of the bitmap, whole physical pixels
    @location(1) size: vec2<f32>,  // bitmap size in pixels
    @location(2) uv_min: vec2<f32>,
    @location(3) uv_max: vec2<f32>,
    @location(4) color: vec4<f32>, // straight RGBA
    @location(5) flags: u32,       // bit 0: sample the colour page
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) flags: u32,
};

fn corner(vi: u32) -> vec2<f32> {
    return vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
}

fn to_clip(px: vec2<f32>) -> vec4<f32> {
    let ndc = (px + vec2<f32>(0.0, globals.top)) / globals.viewport * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: GlyphIn) -> VsOut {
    let c = corner(vi);
    var out: VsOut;
    out.clip = to_clip(inst.pos + c * inst.size);
    out.uv = mix(inst.uv_min, inst.uv_max, c);
    out.color = inst.color;
    out.flags = inst.flags;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Sample both pages unconditionally: textureSample must be called in
    // uniform control flow, and choosing afterwards is cheap.
    let cov = textureSample(atlas_tex, atlas_smp, in.uv).r;
    let texel = textureSample(color_tex, atlas_smp, in.uv);
    if (in.flags & GLYPH_COLOR) != 0u {
        // Already premultiplied; the text colour's alpha still fades it.
        return vec4<f32>(space(texel.rgb), texel.a) * in.color.a;
    }
    // pow(0, y) is undefined in WGSL, so keep exact zero as zero.
    // In linear light coverage is used as it is: the adjustment exists to
    // make up for blending in gamma space.
    let gamma = select(GAMMA_ADJUST, 1.0, globals.linear > 0.5);
    let shaped = select(0.0, pow(cov, 1.0 / gamma), cov > 0.0);
    let a = shaped * in.color.a;
    return vec4<f32>(space(in.color.rgb) * a, a); // premultiplied
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
