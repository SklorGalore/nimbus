// Glyph quads textured from the coverage atlas.
// The prelude is duplicated from rect.wgsl on purpose: clear over clever.

struct Globals { viewport: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;

@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_smp: sampler;

// Above 1 thickens text slightly; light text on a dark background otherwise
// looks thin when blended in gamma space. Tune by eye against native text.
const GAMMA_ADJUST: f32 = 1.4;

struct GlyphIn {
    @location(0) pos: vec2<f32>,   // top-left of the bitmap, whole physical pixels
    @location(1) size: vec2<f32>,  // bitmap size in pixels
    @location(2) uv_min: vec2<f32>,
    @location(3) uv_max: vec2<f32>,
    @location(4) color: vec4<f32>, // straight RGBA
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

fn corner(vi: u32) -> vec2<f32> {
    return vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
}

fn to_clip(px: vec2<f32>) -> vec4<f32> {
    let ndc = px / globals.viewport * 2.0 - 1.0;
    return vec4<f32>(ndc.x, -ndc.y, 0.0, 1.0);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: GlyphIn) -> VsOut {
    let c = corner(vi);
    var out: VsOut;
    out.clip = to_clip(inst.pos + c * inst.size);
    out.uv = mix(inst.uv_min, inst.uv_max, c);
    out.color = inst.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let cov = textureSample(atlas_tex, atlas_smp, in.uv).r;
    // pow(0, y) is undefined in WGSL, so keep exact zero as zero.
    let shaped = select(0.0, pow(cov, 1.0 / GAMMA_ADJUST), cov > 0.0);
    let a = shaped * in.color.a;
    return vec4<f32>(in.color.rgb * a, a); // premultiplied
}
