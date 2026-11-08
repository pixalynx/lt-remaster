// The shadow map for the lit native 3D (LT-112): casters in the main light's clip space; cut-out
// texels cast nothing. `Light` is `lit.wgsl`'s.

struct Light {
    sun_dir: vec4<f32>,
    sun: vec4<f32>,
    sky: vec4<f32>,
    ground: vec4<f32>,
    // baked strength, de-light, exposure, point lights
    params: vec4<f32>,
    eye: vec4<f32>,
    shadow: mat4x4<f32>,
    // world units a texel, map on (1/0), 1 / map size, debug view
    shadow_params: vec4<f32>,
    // per light: position + reach, colour
    points: array<vec4<f32>, 32>,
};

@group(0) @binding(0) var atlas: texture_2d<f32>;
struct CasterIn {
    @location(0) world: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) flags: u32,
};

struct CasterOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(flat) flags: u32,
};

@group(1) @binding(0) var<uniform> LS: Light;

// The draw's map to world space: a chunk's, or the identity for the characters (posed already).
@group(2) @binding(0) var<uniform> M: mat4x4<f32>;

@vertex fn vs_shadow(v: CasterIn) -> CasterOut {
    var o: CasterOut;
    o.pos = LS.shadow * (M * vec4<f32>(v.world, 1.0));
    o.uv = v.uv;
    o.flags = v.flags;
    return o;
}

@fragment fn fs_shadow(i: CasterOut) {
    if ((i.flags & 1u) != 0u) {
        let size = vec2<i32>(textureDimensions(atlas));
        let t = textureLoad(atlas, clamp(vec2<i32>(floor(i.uv)), vec2<i32>(0), size - 1), 0);
        if (t.a == 0.0) { discard; }
    }
}
