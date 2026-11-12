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
    // the first flight's far colour (LT-143): what a polygon's fog blends toward
    fog: vec4<f32>,
    // the water (LT-146): the waves' clock (seconds), on (1/0)
    water: vec4<f32>,
    // a second light without a shadow (battle's, LT-116): the direction it travels, its colour
    fill_dir: vec4<f32>,
    fill: vec4<f32>,
};

@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(0) var<uniform> L: Light;
@group(1) @binding(1) var shadow_map: texture_depth_2d;
@group(1) @binding(2) var shadow_cmp: sampler_comparison;
// The water's ripple map (LT-146, `water::ripple_map`): slope x, slope z, height; tiles.
@group(1) @binding(3) var ripples: texture_2d<f32>;
@group(1) @binding(4) var ripple_sampler: sampler;

// The water's mirror (LT-147, `water::mirror_clip`): world to the clip space of the scene mirrored
// about the water's plane, and the plane: its height, on (1/0), one over the picture's size.
struct Mirror {
    clip: mat4x4<f32>,
    plane: vec4<f32>,
    // LT-148: the shallows, foam and caustics on (1/0)
    shore: vec4<f32>,
};
@group(1) @binding(5) var<uniform> M: Mirror;
// LT-148: each water triangle's three nearest stretches of shore (world x0, z0, x1, z1), the
// triangle's `shore_slot` × 3 on; slot 0 is none (far off).
@group(1) @binding(6) var<storage, read> shores: array<vec4<f32>>;
// The mirrored pass's picture (the lit pass only): linear light, alpha 1 where it drew.
@group(2) @binding(0) var reflection: texture_2d<f32>;
@group(2) @binding(1) var reflection_sampler: sampler;
// How far the ripples push the reflection (picture widths per unit of slope).
const DISTORT: f32 = 0.06;

struct In {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) rgbf: vec4<u32>,
    @location(3) bucket: f32,
    @location(4) world: vec3<f32>,
    @location(5) normal: vec3<f32>,
    // the polygon's fog (LT-143), whether it's the game's water (LT-146: 2 on the mirrored plane,
    // LT-147), its distance to the shore (LT-148)
    @location(6) extra: vec3<f32>,
    // LT-148: where the triangle's corners' nearest stretches of shore are in `shores` (0: none)
    @location(7) shore_slot: u32,
};

struct Out {
    @builtin(position) @invariant pos: vec4<f32>,
    @location(0) @interpolate(linear) uv: vec2<f32>,
    @location(1) @interpolate(linear) rgb: vec3<f32>,
    @location(2) @interpolate(flat) flags: u32,
    @location(3) @interpolate(flat) bucket: f32,
    @location(4) @interpolate(linear) world: vec3<f32>,
    @location(5) @interpolate(linear) normal: vec3<f32>,
    @location(6) @interpolate(flat) fog: f32,
    @location(7) @interpolate(flat) water: f32,
    @location(8) @interpolate(linear) shore: f32,
    @location(9) @interpolate(flat) shore_slot: u32,
};

@vertex fn vs(v: In) -> Out {
    return out_of(v, vec4<f32>(v.pos, 1.0));
}

// The mirrored pass (LT-147): each corner through the mirror; the water isn't in its own
// reflection (its triangles land outside the clip volume).
@vertex fn vs_reflect(v: In) -> Out {
    var pos = M.clip * vec4<f32>(v.world, 1.0);
    if (v.extra.y > 0.5) { pos = vec4<f32>(2.0, 2.0, 2.0, 1.0); }
    return out_of(v, pos);
}

fn out_of(v: In, pos: vec4<f32>) -> Out {
    var o: Out;
    o.pos = pos;
    o.uv = v.uv;
    o.rgb = vec3<f32>(v.rgbf.xyz);
    o.flags = v.rgbf.w;
    o.bucket = v.bucket;
    o.world = v.world;
    o.normal = v.normal;
    o.fog = v.extra.x;
    o.water = v.extra.y;
    o.shore = v.extra.z;
    o.shore_slot = v.shore_slot;
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

// One layer of ripples: the ripple map laid over the water's plane (x, z) at `size` world units a
// tile, turned by `turn` (cos, sin), drifting `drift` tiles a second. Its slope in world units (xy)
// and its height (z, -1..1). `dx`, `dy`: the plane's change across a pixel (the mip it's read at).
fn ripple(p: vec2<f32>, dx: vec2<f32>, dy: vec2<f32>, size: f32, turn: vec2<f32>, drift: vec2<f32>, strength: f32, t: f32) -> vec3<f32> {
    let r = mat2x2<f32>(vec2<f32>(turn.x, turn.y), vec2<f32>(-turn.y, turn.x));
    let uv = (r * p) / size + drift * t;
    let s = textureSampleGrad(ripples, ripple_sampler, uv, (r * dx) / size, (r * dy) / size);
    let slope = transpose(r) * (s.xy * 2.0 - 1.0) * strength;
    return vec3<f32>(slope, s.z * 2.0 - 1.0);
}

// The water's surface at `world` and time `t`: four layers of the ripple map at unlike sizes,
// angles and drifts. Its normal (Y is down: level water faces -Y) and its height (-1..1).
fn waves(world: vec3<f32>, t: f32, dx: vec2<f32>, dy: vec2<f32>) -> vec4<f32> {
    let p = world.xz;
    var d = vec3<f32>(0.0);
    d += ripple(p, dx, dy, 900.0, vec2<f32>(1.0, 0.0), vec2<f32>(0.020, 0.008), 0.06, t) * vec3<f32>(1.0, 1.0, 0.4);
    d += ripple(p, dx, dy, 420.0, vec2<f32>(0.80, 0.60), vec2<f32>(-0.030, 0.021), 0.08, t) * vec3<f32>(1.0, 1.0, 0.3);
    d += ripple(p, dx, dy, 170.0, vec2<f32>(-0.37, 0.93), vec2<f32>(0.050, -0.031), 0.07, t) * vec3<f32>(1.0, 1.0, 0.2);
    d += ripple(p, dx, dy, 70.0, vec2<f32>(-0.91, -0.42), vec2<f32>(-0.071, -0.052), 0.05, t) * vec3<f32>(1.0, 1.0, 0.1);
    return vec4<f32>(normalize(vec3<f32>(-d.x, -1.0, -d.y)), d.z);
}

// The game's water as water (LT-146), in linear light:
// - its own colour lit as before (`base`), shaded by the waves: their slope against the sun
//   (around level water's, so they shade and don't brighten), crests lighter than troughs;
// - the sky reflected over it by the Fresnel term (little looking down, more at a grazing angle,
//   held to 0.55 so the water's colour stays). The sky it reflects is the water's own colour,
//   lighter toward the horizon (never past 0.9 in its brightest channel; a quarter of the scene's
//   far colour there when it has one): the art's own blue, not a stock sky;
// - the sun's glitter on the ripple facets turned toward the eye (`vis`: the sun not shadowed), a
//   tight lobe: the field's camera looks near the sun's mirror angle.
// - LT-147: where the mirrored pass drew (`mirrored`: this water lies on its plane), the scene
//   reflected instead of that sky, read at the pixel (`screen`) pushed by the ripples' slope.
// - LT-148, by the shore (`shore` units away): the shallows lighter and greener, the caustic web
//   on their bed, and a broken line of foam where the water meets the land.
// `dx`, `dy`: the plane's change across a pixel.
fn water(world: vec3<f32>, lit: vec3<f32>, vis: f32, dx: vec2<f32>, dy: vec2<f32>, screen: vec2<f32>, mirrored: bool, shore: f32) -> vec3<f32> {
    let w = waves(world, L.water.x, dx, dy);
    let n = w.xyz;
    var base = lit;
    let s = shallow(shore);
    if (s > 0.0) {
        base = mix(base, base * SHALLOW_TINT, 0.75 * s);
        base += L.sun.rgb * caustics(world, n, L.water.x, dx, dy) * s * s * vis * CAUSTIC_LIGHT;
    }
    let v = normalize(L.eye.xyz - world);
    let cosv = clamp(dot(n, v), 0.0, 1.0);
    let fresnel = min(0.04 + 0.96 * pow(1.0 - cosv, 5.0), 0.55);
    let r = reflect(-v, n);
    let up = clamp(-r.y, 0.0, 1.0);
    let peak = max(max(base.r, base.g), max(base.b, 1e-4));
    var horizon = max(min(base * 1.4, base / peak * 0.9), base);
    if (dot(L.fog.rgb, vec3<f32>(1.0)) > 0.0) { horizon = mix(horizon, pow(L.fog.rgb, vec3<f32>(2.2)), 0.25); }
    let sky = mix(horizon, base, sqrt(up));
    // The waves' slope against the sun, around level water's (so they shade, not brighten).
    let level = max(L.sun_dir.y, 0.0);
    let shaded = base * clamp(1.0 + 1.0 * (max(dot(n, -L.sun_dir.xyz), 0.0) - level), 0.75, 1.2) * (1.0 + 0.15 * w.w);
    let h = normalize(-L.sun_dir.xyz + v);
    let nh = max(dot(n, h), 0.0);
    let glint = pow(nh, 3000.0) * 0.5;
    var surface = mix(shaded, sky, fresnel);
    if (mirrored) {
        // The scene's reflection over that, by its own Fresnel term: from 0.15 looking down (the
        // art's bright blue would hide a real 0.04 from the field's steep camera) to 0.6 grazing.
        let m = mirror_at(screen, n);
        surface = mix(surface, m.rgb, mirror_fresnel(cosv) * m.a);
    }
    var out = surface + L.sun.rgb * glint * vis;
    let f = foam(world, shore, w.w, L.water.x, dx, dy);
    if (f > 0.0) {
        // White, facing up: the sky's light and the sun's.
        out = mix(out, (L.sky.rgb + L.sun.rgb * level * vis) * FOAM_ALBEDO, f);
    }
    return out;
}

// LT-148. How far the shallows reach from the shore, and the foam line's width (world units).
const SHALLOW: f32 = 220.0;
const FOAM: f32 = 34.0;
// The shallows' colour against the water's own (lighter, greener), the caustics' share of the
// sun, the foam's whiteness.
const SHALLOW_TINT: vec3<f32> = vec3<f32>(1.1, 1.8, 1.3);
const CAUSTIC_LIGHT: f32 = 0.3;
const FOAM_ALBEDO: f32 = 0.75;

// A pixel's distance across the ground to the shore (LT-148): to the nearest of its triangle's
// corners' nearest stretches, at most 1024 (`water::SHORE_MAX`). Exact inside the triangle, so the
// foam keeps its width where every corner lies by the shore.
fn shore_at(i: Out) -> f32 {
    let p = i.world.xz;
    let k = i.shore_slot * 3u;
    return min(min(min(stretch(p, shores[k]), stretch(p, shores[k + 1u])), stretch(p, shores[k + 2u])), 1024.0);
}

fn stretch(p: vec2<f32>, s: vec4<f32>) -> f32 {
    let d = s.zw - s.xy;
    let e = p - s.xy;
    let len = dot(d, d);
    let t = select(0.0, clamp(dot(e, d) / max(len, 1e-6), 0.0, 1.0), len > 0.0);
    return length(e - t * d);
}

// How shallow the water is `shore` units from the shore: 1 at it, 0 from `SHALLOW` on (and with
// the shore's effects off).
fn shallow(shore: f32) -> f32 {
    if (M.shore.x < 0.5) { return 0.0; }
    return 1.0 - smoothstep(0.0, SHALLOW, shore);
}

// The caustic web on the shallows' bed (0..1): the ripple map's web (alpha) twice, at unlike sizes
// and drifts, each wobbled by the waves' slope (`n`), the brighter of the two.
fn caustics(world: vec3<f32>, n: vec3<f32>, t: f32, dx: vec2<f32>, dy: vec2<f32>) -> f32 {
    let p = world.xz + n.xz * 40.0;
    let a = textureSampleGrad(ripples, ripple_sampler, p / 330.0 + vec2<f32>(0.021, 0.013) * t, dx / 330.0, dy / 330.0).a;
    let r = mat2x2<f32>(vec2<f32>(0.8, 0.6), vec2<f32>(-0.6, 0.8));
    let b = textureSampleGrad(ripples, ripple_sampler, (r * p) / 250.0 + vec2<f32>(-0.017, 0.024) * t, (r * dx) / 250.0, (r * dy) / 250.0).a;
    // Faded where a pixel covers much ground (seen from high up, a web would be a crackle).
    let foot = max(length(dx), length(dy));
    return pow(max(a, b), 1.5) * (1.0 - smoothstep(6.0, 30.0, foot));
}

// The foam where the water meets the shore (0..1): `FOAM` wide, its edge lapping in and out (every
// 3.7 seconds, in waves running along the shore), pushed by the waves' height (`height`, -1..1) and
// a drifting swell (the ripple map's height, larger), and broken where that swell is low; never
// past 1.6 × `FOAM`; 0 with the shore's effects off.
fn foam(world: vec3<f32>, shore: f32, height: f32, t: f32, dx: vec2<f32>, dy: vec2<f32>) -> f32 {
    if (M.shore.x < 0.5) { return 0.0; }
    let swell = textureSampleGrad(ripples, ripple_sampler, world.xz / 280.0 + vec2<f32>(-0.04, 0.06) * t, dx / 280.0, dy / 280.0).z;
    let lap = sin(t * 1.7 + dot(world.xz, vec2<f32>(0.013, 0.009)));
    let edge = FOAM * clamp(0.55 + 0.35 * lap + 0.3 * height + 0.6 * swell, 0.2, 1.6);
    let line = 1.0 - smoothstep(edge * 0.5, edge, shore);
    return line * smoothstep(0.2, 0.45, swell) * 0.9;
}

// The scene reflection's Fresnel term for the eye at `cosv` to the water's normal: 0.15 looking
// straight down, up to 0.6 grazing.
fn mirror_fresnel(cosv: f32) -> f32 {
    return min(0.15 + 0.85 * pow(1.0 - cosv, 5.0), 0.6);
}

// The mirrored scene at a pixel (`screen`, the picture's pixels), pushed by the ripples' slope
// (`n`, the water's normal): linear light, alpha where the mirrored pass drew.
fn mirror_at(screen: vec2<f32>, n: vec3<f32>) -> vec4<f32> {
    let uv = screen * M.plane.zw + n.xz * DISTORT;
    return textureSampleLevel(reflection, reflection_sampler, uv, 0.0);
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

// [`aces`] undone (below its white): the linear light that tone-maps to `y`.
fn aces_inverse(y: vec3<f32>) -> vec3<f32> {
    let v = min(y, vec3<f32>(0.999));
    return (-0.59 * v + 0.03 - sqrt(-1.0127 * v * v + 1.3702 * v + 0.0009)) / (2.0 * (2.43 * v - 2.51));
}

// The first flight's depth cue (LT-143) on a tone-mapped colour `c`: the polygon's (`fog`), toward
// what the game fogs to. It fogs the colour that modulates the texture, so a textured polygon's
// far colour is its texture (`tex`) brightened by the far colour (× 0xF0 / 128); an untextured
// one's is the far colour; a raw texture isn't modulated, so isn't fogged.
fn fogged(c: vec3<f32>, tex: vec3<f32>, flags: u32, fog: f32) -> vec3<f32> {
    var haze = L.fog.rgb;
    if ((flags & 1u) != 0u) {
        haze = select(min(tex * L.fog.rgb * (255.0 / 128.0), vec3<f32>(1.0)), c, (flags & 4u) != 0u);
    }
    return mix(c, haze, fog);
}

// A corner's surface lit, in linear light (before the exposure), as seen from `eye`: what the lit
// pass draws and the mirrored pass reflects.
struct Shaded {
    lin: vec3<f32>,
    // the texture's colour (the fog's), the facing normal, the main light's visibility and
    // Lambert term, the lamps' light
    tex: vec3<f32>,
    n: vec3<f32>,
    vis: f32,
    lambert: f32,
    lamps: vec3<f32>,
};

fn shade(i: Out, eye: vec3<f32>) -> Shaded {
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
    let to_eye = eye - i.world;
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
    light += L.fill.rgb * max(dot(n, -L.fill_dir.xyz), 0.0);
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
            var falloff = window / (1.0 + dist * dist / (0.08 * reach * reach));
            var facing = 0.35 + 0.65 * max(dot(n, d / max(dist, 1e-3)), 0.0);
            // The battle arena's light (LT-116): full within its inner radius, then linear in the
            // distance squared, whichever way a surface faces, as the game's (a vertex colour).
            let inner = L.points[2 * k + 1].w;
            if (inner > 0.0) {
                falloff = 1.0 - clamp((dist * dist - inner * inner) / max(reach * reach - inner * inner, 1.0), 0.0, 1.0);
                facing = 1.0;
            }
            lamps += L.points[2 * k + 1].rgb * falloff * facing;
        }
    }
    light += lamps;
    return Shaded(albedo * light, tex, n, vis, lambert, lamps);
}

@fragment fn fs(i: Out) -> Frag {
    // The water's plane across a pixel (its ripples are read at that mip; derivatives need uniform
    // control flow, so they're taken here).
    let dwx = dpdx(i.world.xz);
    let dwy = dpdy(i.world.xz);
    let s = shade(i, L.eye.xyz);
    let tex = s.tex;
    let n = s.n;
    let vis = s.vis;
    let lambert = s.lambert;
    let lamps = s.lamps;
    var lin = s.lin;
    // Water 2: on the mirrored plane (LT-147).
    let mirrored = i.water > 1.5 && M.plane.y > 0.5;
    if (i.water > 0.5 && L.water.y > 0.5) { lin = water(i.world, lin, vis, dwx, dwy, i.pos.xy, mirrored, shore_at(i)); }
    var c = fogged(pow(aces(lin * L.params.z), vec3<f32>(1.0 / 2.2)), tex, i.flags, i.fog);
    // Debug views (`LT_LIGHT_DEBUG`): 1 the main light's visibility, 2 the normal, 3 the shadow
    // map's depth, 4 the lamps, 5 the water.
    if (L.shadow_params.w > 0.5 && L.shadow_params.w < 1.5) { c = vec3<f32>(vis * lambert); }
    if (L.shadow_params.w > 1.5 && L.shadow_params.w < 2.5) { c = n * 0.5 + 0.5; }
    if (L.shadow_params.w > 3.5 && L.shadow_params.w < 4.5) { c = min(lamps, vec3<f32>(1.0)); }
    // 5: the water (LT-146), white.
    if (L.shadow_params.w > 4.5 && L.shadow_params.w < 5.5) { c = vec3<f32>(min(i.water, 1.0)); }
    // 6, 7, 8 (LT-147): on water, where the mirrored scene lies (white), its colour as the ripples
    // push it, and the Fresnel term it's blended by (0..0.6); black elsewhere.
    if (L.shadow_params.w > 5.5 && L.shadow_params.w < 8.5) {
        c = vec3<f32>(0.0);
        if (mirrored) {
            let wn = waves(i.world, L.water.x, dwx, dwy).xyz;
            let m = mirror_at(i.pos.xy, wn);
            c = select(pow(aces(m.rgb * L.params.z), vec3<f32>(1.0 / 2.2)), vec3<f32>(m.a), L.shadow_params.w < 6.5);
            if (L.shadow_params.w > 7.5) { c = vec3<f32>(mirror_fresnel(clamp(dot(wn, normalize(L.eye.xyz - i.world)), 0.0, 1.0))); }
        }
    }
    // 9, 10, 11 (LT-148): on water, how near the shore (white at it, black 512 units off), the foam,
    // and the caustics' light (as the shallows give it); black elsewhere.
    if (L.shadow_params.w > 8.5 && L.shadow_params.w < 11.5) {
        c = vec3<f32>(0.0);
        if (i.water > 0.5) {
            let wv = waves(i.world, L.water.x, dwx, dwy);
            let d = shore_at(i);
            if (L.shadow_params.w < 9.5) { c = vec3<f32>(1.0 - min(d / 512.0, 1.0)); }
            else if (L.shadow_params.w < 10.5) { c = vec3<f32>(foam(i.world, d, wv.w, L.water.x, dwx, dwy)); }
            else { let s = shallow(d); c = vec3<f32>(caustics(i.world, wv.xyz, L.water.x, dwx, dwy) * s * s); }
        }
    }
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

// The mirrored pass's colour (LT-147): what's above the water lit as the lit pass lights it, in
// linear light; what's under the plane and cut-out texels left out.
@fragment fn fs_reflect(i: Out) -> @location(0) vec4<f32> {
    // Y is down: under the water is below the plane.
    if (i.world.y > M.plane.x + 1.0) { discard; }
    if ((i.flags & 1u) != 0u) {
        let size = vec2<i32>(textureDimensions(atlas));
        let t = textureLoad(atlas, clamp(vec2<i32>(floor(i.uv)), vec2<i32>(0), size - 1), 0);
        if (t.a == 0.0) { discard; }
    }
    // Lit on the side the camera sees: every polygon here passed the game's back-face test from
    // the camera, so that's its front (the mirrored eye would turn smoothed normals at an edge
    // toward the sun).
    let s = shade(i, L.eye.xyz);
    var lin = s.lin;
    // Fogged as the lit pass fogs it (after the tone curve), brought back to linear light.
    if (i.fog > 0.0) {
        let c = fogged(pow(aces(lin * L.params.z), vec3<f32>(1.0 / 2.2)), s.tex, i.flags, i.fog);
        lin = aces_inverse(pow(c, vec3<f32>(2.2))) / L.params.z;
    }
    return vec4<f32>(lin, 1.0);
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
