// Living Tool Remaster's lit native 3D (LT-112, research 16 §5.3; `crate::light`).
//
// The same triangles, positions and outputs as the original look's pass (`raster::SHADER` with the
// order output): colour, the coverage mask (red), the polygon's bucket + 1. What changes is the
// colour. The original is texture × baked colour / 128; here that's the albedo, lit by:
// - a hemisphere ambient (sky above, ground below);
// - the rig's main light (sun, or a room's light from above), shadowed by a 2048² map;
// - up to 16 point lights at the lamps the art paints (`light::lamps`).
// Where a real shadow falls, part of the baked darkening is lifted (`delit`), so it isn't dark twice.
// Light is added in linear space and brought back through the ACES curve.

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
@group(1) @binding(0) var<uniform> L: Light;
@group(1) @binding(1) var shadow_map: texture_depth_2d;
@group(1) @binding(2) var shadow_cmp: sampler_comparison;

struct In {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) rgbf: vec4<u32>,
    @location(3) bucket: f32,
    @location(4) world: vec3<f32>,
    @location(5) normal: vec3<f32>,
};

struct Out {
    @builtin(position) @invariant pos: vec4<f32>,
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(linear) rgb: vec3<f32>,
    @location(2) @interpolate(flat) flags: u32,
    @location(3) @interpolate(flat) bucket: f32,
    @location(4) @interpolate(linear) world: vec3<f32>,
    @location(5) @interpolate(linear) normal: vec3<f32>,
};

@vertex fn vs(v: In) -> Out {
    var o: Out;
    o.pos = vec4<f32>(v.pos, 1.0);
    o.uv = v.uv;
    o.rgb = vec3<f32>(v.rgbf.xyz);
    o.flags = v.rgbf.w;
    o.bucket = v.bucket;
    o.world = v.world;
    o.normal = v.normal;
    return o;
}

struct Frag {
    @location(0) colour: vec4<f32>,
    @location(1) mask: vec4<f32>,
    @location(2) order: vec4<f32>,
};

// How much of the main light reaches `world` (1 lit, 0 in shadow).
fn sun_visible(world: vec3<f32>, n: vec3<f32>) -> f32 {
    if L.shadow_params.y < 0.5 {
        return 1.0;
    }
    // Pushed out along the normal by a texel and a half: no acne on surfaces facing the light.
    let p = L.shadow * vec4<f32>(world + n * (1.5 * L.shadow_params.x), 1.0);
    let uv = vec2<f32>(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || p.z > 1.0 {
        return 1.0;
    }
    // Four filtered comparisons (each 2 × 2 texels), half a texel apart.
    let t = L.shadow_params.z * 0.5;
    var sum = 0.0;
    sum += textureSampleCompareLevel(shadow_map, shadow_cmp, uv + vec2<f32>(-t, -t), p.z - 0.0005);
    sum += textureSampleCompareLevel(shadow_map, shadow_cmp, uv + vec2<f32>(t, -t), p.z - 0.0005);
    sum += textureSampleCompareLevel(shadow_map, shadow_cmp, uv + vec2<f32>(-t, t), p.z - 0.0005);
    sum += textureSampleCompareLevel(shadow_map, shadow_cmp, uv + vec2<f32>(t, t), p.z - 0.0005);
    return sum / 4.0;
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment fn fs(i: Out) -> Frag {
    // The albedo and the baked term, as the original look splits them.
    var tex = vec3<f32>(1.0);
    var baked = vec3<f32>(1.0);
    if ((i.flags & 1u) != 0u) {
        let size = vec2<i32>(textureDimensions(atlas));
        let t = textureLoad(atlas, clamp(vec2<i32>(floor(i.uv)), vec2<i32>(0), size - 1), 0);
        // Cut-out texels were left out of the depth pass, so they never reach here.
        tex = t.rgb;
        if ((i.flags & 4u) == 0u) { baked = i.rgb / 128.0; }
    } else {
        tex = i.rgb / 255.0;
    }

    // Two-sided: the normal faces the camera.
    var n = normalize(i.normal);
    let to_eye = L.eye.xyz - i.world;
    if (dot(n, to_eye) < 0.0) { n = -n; }

    let vis = sun_visible(i.world, n);
    let lambert = max(dot(n, -L.sun_dir.xyz), 0.0);
    // Under a real shadow, lift part of the baked darkening (never brighten past neutral).
    let lifted = mix(baked, max(baked, vec3<f32>(1.0)), L.params.y * (1.0 - vis * step(0.05, lambert)));
    let b = mix(vec3<f32>(1.0), lifted, L.params.x);
    let albedo = pow(min(tex * b, vec3<f32>(1.0)), vec3<f32>(2.2));

    // Y is down: up is −Y.
    let up = clamp(-n.y * 0.5 + 0.5, 0.0, 1.0);
    var light = mix(L.ground.rgb, L.sky.rgb, up);
    light += L.sun.rgb * lambert * vis;
    let count = i32(L.params.w);
    var lamps = vec3<f32>(0.0);
    for (var k = 0; k < count; k++) {
        let pr = L.points[2 * k];
        let d = pr.xyz - i.world;
        let dist = length(d);
        let reach = pr.w;
        if (dist < reach) {
            let x = dist / reach;
            let window = (1.0 - x * x) * (1.0 - x * x);
            let falloff = window / (1.0 + dist * dist / (0.08 * reach * reach));
            lamps += L.points[2 * k + 1].rgb * falloff * (0.35 + 0.65 * max(dot(n, d / max(dist, 1e-3)), 0.0));
        }
    }
    light += lamps;
    var c = pow(aces(albedo * light * L.params.z), vec3<f32>(1.0 / 2.2));
    // Debug views (`LT_LIGHT_DEBUG`): 1 the main light's visibility, 2 the normal.
    if (L.shadow_params.w > 0.5 && L.shadow_params.w < 1.5) { c = vec3<f32>(vis * lambert); }
    if (L.shadow_params.w > 1.5 && L.shadow_params.w < 2.5) { c = n * 0.5 + 0.5; }
    if (L.shadow_params.w > 3.5) { c = min(lamps, vec3<f32>(1.0)); }
    if (L.shadow_params.w > 2.5 && L.shadow_params.w < 3.5) {
        let p = L.shadow * vec4<f32>(i.world, 1.0);
        let uv = vec2<f32>(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
        let stored = textureLoad(shadow_map, vec2<i32>(uv * 4096.0), 0);
        c = vec3<f32>(fract(p.z * 20.0), fract(stored * 20.0), select(0.0, 1.0, p.z > stored + 0.001));
    }

    var f: Frag;
    f.colour = vec4<f32>(c, 1.0);
    f.mask = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    f.order = vec4<f32>(i.bucket + 1.0, 0.0, 0.0, 0.0);
    return f;
}

// The depth pass before the lit one: positions as `vs` makes them, cut-out texels dropped.
struct DepthOut {
    @builtin(position) @invariant pos: vec4<f32>,
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(flat) flags: u32,
};

@vertex fn vs_depth(v: In) -> DepthOut {
    var o: DepthOut;
    o.pos = vec4<f32>(v.pos, 1.0);
    o.uv = v.uv;
    o.flags = v.rgbf.w;
    return o;
}

@fragment fn fs_depth(i: DepthOut) {
    if ((i.flags & 1u) != 0u) {
        let size = vec2<i32>(textureDimensions(atlas));
        let t = textureLoad(atlas, clamp(vec2<i32>(floor(i.uv)), vec2<i32>(0), size - 1), 0);
        if (t.a == 0.0) { discard; }
    }
}
