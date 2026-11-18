// Bloom "matter": one glossy liquid mass, raymarched as a signed distance
// field, lit by a procedural studio, with a thin-film sheen and a wisp of
// vapour rising from it. Everything the sound and the material change is a
// uniform (see `uniforms.rs`); there are no textures.
//
// Passes: `fs_matter` draws the panel (premultiplied BGRA, sRGB-encoded by
// hand so the bytes are display-ready). On macOS `fs_luma` / `fs_chroma`
// turn that image into the two planes of an NV12 IOSurface, which GPUI shows
// as a video surface without a copy.

struct U {
    res: vec4f,     // width, height (px), time (s), seed
    frame: vec4f,   // centre x, centre y, scale (px per unit), corner radius (px)
    bg: vec4f,      // panel colour (linear), light theme (0/1)
    outer: vec4f,   // colour outside the rounded corners (linear), unused
    shape0: vec4f,  // size, swell, breath, churn
    shape1: vec4f,  // lumps, fold frequency, wobble frequency, wobble amount
    spikes: vec4f,  // amount, sharpness, count, shudder
    cursor: vec4f,  // x, y (units), pull (+ attract, - repel), presence 0..1
    mat0: vec4f,    // albedo (linear), metal
    mat1: vec4f,    // roughness, clearcoat, iridescence, film thickness (µm)
    mat2: vec4f,    // film range (µm), film shift (µm), rim, glass
    mat3: vec4f,    // emission colour, emission amount
    mat4: vec4f,    // diffuse wrap, exposure, smoke tint strength, environment warmth
    smoke0: vec4f,  // amount, puff, rise speed, turbulence
    smoke_a: vec4f, // three vapour colours (linear)
    smoke_b: vec4f,
    smoke_c: vec4f,
    tint: vec4f,    // the sound's own colour (linear), how much it tints the light
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var noise_tex: texture_3d<f32>;
@group(0) @binding(4) var noise_smp: sampler;

const PI: f32 = 3.14159265;
const PHI: f32 = 1.61803399;

struct VsOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

// One triangle covering the target.
@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> VsOut {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var o: VsOut;
    o.pos = vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    o.uv = vec2f(x, y);
    return o;
}

// ───────────────────────────── Noise ─────────────────────────────

fn hash11(x: f32) -> f32 {
    var p = fract(x * 0.1031);
    p *= p + 33.33;
    p *= p + p;
    return fract(p);
}

fn lattice(i: vec3i) -> f32 {
    return textureLoad(noise_tex, i & vec3i(31), 0).r;
}

// Value noise with a quintic fade (C2, so normals stay smooth), -1..1. The
// lattice values live in a 32³ volume (eight loads, blended here in full
// precision: hardware filtering would quantise the weights and grain the
// highlights).
fn vnoise(x: vec3f) -> f32 {
    let fi = floor(x);
    let f = x - fi;
    let i = vec3i(fi);
    let w = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = lattice(i);
    let b = lattice(i + vec3i(1, 0, 0));
    let c = lattice(i + vec3i(0, 1, 0));
    let d = lattice(i + vec3i(1, 1, 0));
    let e = lattice(i + vec3i(0, 0, 1));
    let g = lattice(i + vec3i(1, 0, 1));
    let h = lattice(i + vec3i(0, 1, 1));
    let k = lattice(i + vec3i(1, 1, 1));
    let v = mix(mix(mix(a, b, w.x), mix(c, d, w.x), w.y), mix(mix(e, g, w.x), mix(h, k, w.x), w.y), w.z);
    return v * 2.0 - 1.0;
}

const ROT: mat3x3f = mat3x3f(vec3f(0.00, 0.80, 0.60), vec3f(-0.80, 0.36, -0.48), vec3f(-0.60, -0.48, 0.64));

fn fbm2(p: vec3f) -> f32 {
    return vnoise(p) * 0.66 + vnoise(ROT * p * 2.03) * 0.34;
}

fn fbm3(p: vec3f) -> f32 {
    let q = ROT * p * 2.02;
    return vnoise(p) * 0.55 + vnoise(q) * 0.3 + vnoise(ROT * q * 2.03) * 0.15;
}

// ───────────────────────────── Shape ─────────────────────────────

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = max(k - abs(a - b), 0.0) / k;
    return min(a, b) - h * h * k * 0.25;
}

// The nearest of `n` points spread evenly over the unit sphere (a spherical
// Fibonacci lattice, inverted in closed form after Keinert et al. 2015):
// returns (index, distance). The ferrofluid spikes stand on these points.
fn inverse_sf(p: vec3f, n: f32) -> vec2f {
    let m = 1.0 - 1.0 / n;
    let phi = min(atan2(p.y, p.x), PI);
    let cos_t = p.z;
    let k = max(2.0, floor(log(n * PI * sqrt(5.0) * (1.0 - cos_t * cos_t)) / log(PHI * PHI)));
    let fk = pow(PHI, k) / sqrt(5.0);
    let f = vec2f(round(fk), round(fk * PHI));
    let ka = 2.0 * f / n;
    let kb = 2.0 * PI * (fract((f + 1.0) * PHI) - (PHI - 1.0));
    let det = ka.y * kb.x - ka.x * kb.y;
    let v = vec2f(phi, cos_t - m);
    let c = floor((vec2f(ka.y, -ka.x) * v.x + vec2f(kb.y, -kb.x) * v.y) / det);
    var best = 8.0;
    var j = 0.0;
    for (var s = 0; s < 4; s++) {
        let uv = vec2f(f32(s % 2), f32(s / 2));
        let i = dot(f, uv + c);
        let ph = 2.0 * PI * fract(i * PHI);
        let ct = m - 2.0 * i / n;
        let st = sqrt(max(0.0, 1.0 - ct * ct));
        let q = vec3f(cos(ph) * st, sin(ph) * st, ct);
        let dd = dot(q - p, q - p);
        if (dd < best) {
            best = dd;
            j = i;
        }
    }
    return vec2f(j, sqrt(best));
}

// Where the magnet (the pointer) sits, as a direction from the mass's centre.
fn magnet_dir() -> vec3f {
    return normalize(vec3f(u.cursor.x * 1.3, u.cursor.y * 1.3, 0.75));
}

// The body: a few warped balls melted together. Cheap, used far from the
// surface and as the base of `map`.
fn body(p: vec3f) -> f32 {
    let t = u.res.z;
    let s = u.shape0.x * (1.0 + 0.10 * u.shape0.y) * (1.0 + 0.012 * sin(t * u.shape0.z));
    let seed = u.res.w;
    let o = vec3f(hash11(seed + 1.0), hash11(seed + 2.0), hash11(seed + 3.0)) - 0.5;
    var q = p;
    q.y *= 1.06;
    var d = length(q - vec3f(0.0, -0.08, 0.0)) - 0.62 * s;
    let c1 = vec3f(0.44 + 0.10 * sin(t * 0.31) + o.x * 0.2, 0.28 + 0.08 * sin(t * 0.23), 0.16 * cos(t * 0.27)) * s;
    let c2 = vec3f(-0.48 + 0.07 * sin(t * 0.41), -0.10 + 0.09 * cos(t * 0.33) + o.y * 0.2, 0.12 * sin(t * 0.21)) * s;
    let c3 = vec3f(0.32 + o.z * 0.3, -0.36 + 0.05 * sin(t * 0.37), -0.20 + 0.08 * cos(t * 0.19)) * s;
    let c4 = vec3f(-0.14 + 0.08 * cos(t * 0.29), 0.40 + 0.06 * sin(t * 0.45 + 1.0), 0.30 + o.x * 0.15) * s;
    let c5 = vec3f(-0.30 + 0.06 * sin(t * 0.27 + 2.0), -0.40 + 0.05 * cos(t * 0.35), 0.28 + o.z * 0.1) * s;
    let c6 = vec3f(0.10 + o.y * 0.2 + 0.05 * sin(t * 0.39), 0.52 + 0.05 * cos(t * 0.31), -0.18) * s;
    d = smin(d, length(q - c1) - (0.38 + 0.06 * o.y) * s, 0.30 * s);
    d = smin(d, length(q - c2) - (0.36 + 0.05 * o.z) * s, 0.30 * s);
    d = smin(d, length(q - c3) - 0.30 * s, 0.26 * s);
    d = smin(d, length(q - c4) - 0.28 * s, 0.24 * s);
    d = smin(d, length(q - c5) - 0.26 * s, 0.24 * s);
    d = smin(d, length(q - c6) - 0.22 * s, 0.22 * s);
    return d;
}

// How tall the spikes grow in direction `dir`: on the top, and towards the
// magnet when it attracts.
fn spike_grow(dir: vec3f, facing: f32, pull: f32) -> f32 {
    return u.spikes.x * (0.25 + 0.75 * smoothstep(-0.35, 0.85, dir.y)) + max(pull, 0.0) * 0.8 * pow(facing, 4.0);
}

// The steepest the surface can get (spikes), for the marching step.
fn steepness() -> f32 {
    let r = 1.5 / sqrt(max(u.spikes.z, 4.0));
    let g = u.spikes.x + max(u.cursor.z * u.cursor.w, 0.0) * 0.8;
    return 0.30 * u.shape0.x * g * max(u.spikes.y, 1.0) / r;
}

// The full mass: body, folds, the FM wobble, the shudder of a hit, the
// magnet's pull and the spikes.
fn map(p: vec3f) -> f32 {
    return map2(p).x;
}

// How far any detail (folds, wobble, shake, magnet, spikes) can move the
// surface out of the body.
fn detail_reach() -> f32 {
    let pull = max(u.cursor.z * u.cursor.w, 0.0);
    return (1.5 * u.shape1.x + u.shape1.w + 0.03 * u.spikes.w + 0.2 * pull + 0.3 * (u.spikes.x + 0.8 * pull)) * u.shape0.x;
}

// The distance, and a bound that ignores the spikes' steep cones (the body
// and folds less the tallest spike): far from the spikes the march may step
// by the bound, near them by the careful distance.
fn map2(p: vec3f) -> vec2f {
    let t = u.res.z;
    let s = u.shape0.x;
    var d = body(p);
    // Far from the surface the details cannot matter: skip them.
    let reach = detail_reach();
    if (d > reach + 0.1 * s) {
        let b = d - reach;
        return vec2f(b, b);
    }
    let churn = u.shape0.w;
    let q = p * u.shape1.y + vec3f(0.0, -t * churn, t * churn * 0.6);
    // A cheap analytic warp bends the noise into folds.
    let w = sin(q.yzx * 1.3 + vec3f(1.7, 0.3, 2.9) + t * 0.11);
    let folds = vnoise(q + w * 0.55);
    // Ridges and creases: a softened |noise| folds the surface into lines
    // the studio lights run along.
    let n2 = vnoise(q * 1.9 + w * 0.7 + vec3f(1.3, 0.0, 2.1));
    let crease = 0.45 - sqrt(n2 * n2 + 0.02);
    d -= (folds * 1.1 + crease * 0.5) * u.shape1.x * s;
    // FM wobble: a slow standing wave over the surface.
    let wf = u.shape1.z;
    d -= u.shape1.w * s * sin(p.x * wf + t * 1.9) * sin(p.y * wf * 1.13 - t * 1.6) * sin(p.z * wf * 0.91 + t * 1.1);
    // A transient shakes the whole mass for an instant.
    d -= u.spikes.w * 0.03 * s * sin(t * 37.0 + p.y * 2.5 + p.x * 1.7);
    let dir = normalize(p - vec3f(0.0, -0.06, 0.0));
    let md = magnet_dir();
    let facing = max(dot(dir, md), 0.0);
    let pull = u.cursor.z * u.cursor.w;
    d -= pull * 0.20 * s * pow(facing, 5.0);
    // Ferrofluid spikes: cones on an even lattice, grown on the top and
    // towards the magnet, taller when the sound is excited.
    let grow = spike_grow(dir, facing, pull);
    let tallest = grow * 0.30 * s;
    let bound = d - tallest;
    if (grow > 0.002 && d >= tallest + 0.05 * s) {
        // Too far for any spike to reach: the bound will do.
        d = bound;
    } else if (grow > 0.002) {
        let n = u.spikes.z;
        let f = inverse_sf(dir, n);
        let r = 1.5 / sqrt(n);
        let x = clamp(1.0 - f.y / r, 0.0, 1.0);
        let h = 0.6 + 0.4 * hash11(f.x + u.res.w * 7.0);
        let sway = 0.85 + 0.15 * sin(t * 3.0 + f.x);
        d -= pow(x, u.spikes.y) * h * sway * tallest;
    }
    return vec2f(d, bound);
}

fn normal_at(p: vec3f, e: f32) -> vec3f {
    let k = vec2f(1.0, -1.0);
    return normalize(k.xyy * map(p + k.xyy * e) + k.yyx * map(p + k.yyx * e) + k.yxy * map(p + k.yxy * e) + k.xxx * map(p + k.xxx * e));
}

// Occlusion from a few steps along the normal (creases go dark).
fn occlusion(p: vec3f, n: vec3f) -> f32 {
    var o = 0.0;
    var w = 1.0;
    for (var i = 1; i <= 2; i++) {
        let h = 0.07 * f32(i) * u.shape0.x;
        o += w * (h - map(p + n * h));
        w *= 0.6;
    }
    return clamp(1.0 - 2.6 * o / u.shape0.x, 0.0, 1.0);
}

// ─────────────────────────── The studio ───────────────────────────

// A soft rectangular light seen along `r`: `n` is where it faces from,
// `half` its half size on a plane at distance 1, `soft` the edge blur.
fn softbox(r: vec3f, n: vec3f, up: vec3f, half: vec2f, soft: f32) -> f32 {
    let c = dot(r, n);
    if (c <= 0.02) {
        return 0.0;
    }
    let q = r / c - n;
    let right = normalize(cross(up, n));
    let up2 = cross(n, right);
    let x = abs(dot(q, right));
    let y = abs(dot(q, up2));
    let bx = 1.0 - smoothstep(half.x - soft, half.x + soft, x);
    let by = 1.0 - smoothstep(half.y - soft, half.y + soft, y);
    // A softbox is brighter in its middle.
    let falloff = 1.0 - 0.35 * clamp(length(vec2f(x / max(half.x, 1e-3), y / max(half.y, 1e-3))) * 0.6, 0.0, 1.0);
    return bx * by * falloff * smoothstep(0.02, 0.2, c);
}

fn environment(r: vec3f, rough: f32) -> vec3f {
    let light = u.bg.w;
    let warm = u.mat4.w;
    let panel = u.bg.rgb;
    // The room: a dark floor that picks up the panel, a dim horizon, a soft top.
    let floor_c = mix(vec3f(0.006), panel * 0.55, light * 0.85 + 0.15);
    let horizon = mix(vec3f(0.03, 0.032, 0.036), panel * 0.9 + vec3f(0.02), light);
    let top = mix(vec3f(0.07, 0.075, 0.085), vec3f(0.55, 0.57, 0.6), light);
    var c = mix(floor_c, horizon, smoothstep(-0.45, 0.02, r.y));
    c = mix(c, top, smoothstep(0.05, 0.95, r.y));
    let soft = 0.04 + rough * 0.55;
    let key = mix(vec3f(1.0), vec3f(1.0, 0.93, 0.84), warm) * 22.0;
    let up = vec3f(0.0, 1.0, 0.0);
    // Key: a big softbox high on the left, in front.
    c += key * softbox(r, normalize(vec3f(-0.50, 0.62, 0.60)), up, vec2f(0.55, 0.36), soft);
    // A wide panel overhead.
    c += vec3f(14.0) * softbox(r, normalize(vec3f(0.10, 0.98, 0.15)), vec3f(0.0, 0.0, 1.0), vec2f(0.95, 0.16), soft);
    // A tall rim strip behind on the right, a little cool.
    c += vec3f(8.0, 8.8, 10.0) * softbox(r, normalize(vec3f(0.86, 0.20, -0.46)), up, vec2f(0.08, 0.75), soft * 0.8);
    // A second strip in front on the right: streaks down the right lobes.
    c += vec3f(6.0) * softbox(r, normalize(vec3f(0.70, 0.30, 0.65)), up, vec2f(0.05, 0.55), soft);
    // A faint kicker on the left.
    c += vec3f(3.0, 2.9, 2.8) * softbox(r, normalize(vec3f(-0.92, -0.08, -0.25)), up, vec2f(0.05, 0.5), soft);
    // A fill card low in front: a thin line along the bottom lobes.
    c += vec3f(2.5) * softbox(r, normalize(vec3f(0.20, -0.38, 0.9)), up, vec2f(0.6, 0.035), soft);
    // A tall strip in front on the left.
    c += vec3f(7.0) * softbox(r, normalize(vec3f(-0.78, 0.15, 0.6)), up, vec2f(0.045, 0.6), soft);
    // The sound tints the studio a little.
    return c * mix(vec3f(1.0), 0.6 + u.tint.rgb * 1.6, u.tint.w * 0.25);
}

// Thin-film interference: the reflectance per channel of a film `d` µm thick
// (index 1.38) seen at `cos_i`. 0..1.
fn thin_film(cos_i: f32, d: f32) -> vec3f {
    let n = 1.38;
    let sin_t2 = (1.0 - cos_i * cos_i) / (n * n);
    let cos_t = sqrt(max(0.0, 1.0 - sin_t2));
    let opd = 2.0 * n * d * cos_t;
    let lambda = vec3f(0.650, 0.532, 0.450);
    let phase = 2.0 * PI * opd / lambda + PI;
    return 0.5 + 0.5 * cos(phase);
}

fn tonemap(x: vec3f) -> vec3f {
    // A filmic curve (Narkowicz's ACES fit).
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), vec3f(0.0), vec3f(1.0));
}

fn encode(x: vec3f) -> vec3f {
    let c = clamp(x, vec3f(0.0), vec3f(1.0));
    return select(1.055 * pow(c, vec3f(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3f(0.0031308));
}

fn decode(x: vec3f) -> vec3f {
    return select(pow((x + 0.055) / 1.055, vec3f(2.4)), x / 12.92, x <= vec3f(0.04045));
}

// ───────────────────────────── Vapour ─────────────────────────────

fn smoke_palette(x: f32) -> vec3f {
    let t = fract(x) * 3.0;
    let a = u.smoke_a.rgb;
    let b = u.smoke_b.rgb;
    let c = u.smoke_c.rgb;
    let s = smoothstep(vec3f(0.0), vec3f(1.0), fract(vec3f(t)));
    if (t < 1.0) {
        return mix(a, b, s);
    } else if (t < 2.0) {
        return mix(b, c, s);
    }
    return mix(c, a, s);
}

// A wisp rising from the top of the mass (2.5D: noise advected upwards in
// the panel's plane). Returns colour (linear) and coverage.
fn vapour(p: vec2f) -> vec4f {
    let amount = u.smoke0.x * (1.0 + 1.6 * u.smoke0.y);
    if (amount < 0.002) {
        return vec4f(0.0);
    }
    let t = u.res.z;
    let s = u.shape0.x;
    let h = p.y - 0.22 * s;
    if (h < -0.15 || h > 1.8) {
        return vec4f(0.0);
    }
    let sway = 0.06 * sin(h * 2.4 - t * 0.55) + 0.035 * sin(h * 4.3 + t * 0.85);
    let lean = u.cursor.x * u.cursor.w * 0.12 * max(h, 0.0);
    let x = p.x - sway * (0.4 + h * 1.4) - lean;
    let width = 0.05 + 0.20 * max(h, 0.0) + 0.04 * u.smoke0.y;
    let core = exp(-(x * x) / (width * width));
    if (core < 0.01) {
        return vec4f(0.0);
    }
    let reach = 0.5 + 0.45 * u.smoke0.y + 0.4 * u.smoke0.x;
    let fade = smoothstep(-0.12, 0.10, h) * exp(-(h * h) / (reach * reach));
    let rise = u.smoke0.z;
    let w = vec3f(x * 2.4, h * 1.7 - t * rise, t * 0.09);
    let warp = vec2f(vnoise(w * 1.1 + vec3f(0.0, 0.0, 4.0)), vnoise(w * 1.1 + vec3f(5.2, 1.3, 0.0)));
    let turb = u.smoke0.w;
    let q = w + vec3f(warp * turb * 0.8, 0.0);
    let dens = fbm3(q * 1.5);
    let dens2 = fbm2(q * 2.4 + vec3f(7.0, 0.0, 3.0));
    // Curling ribbons rather than fog: thin bands where the noise crosses a level.
    let sheet = 1.0 - smoothstep(0.0, 0.14, abs(dens - 0.05));
    let sheet2 = 1.0 - smoothstep(0.0, 0.09, abs(dens2 + 0.1));
    let veil = smoothstep(0.0, 0.6, dens + 0.2) * 0.22;
    // Pale vapour on a pale panel needs more body to read.
    let boost = mix(1.6, 2.6, u.bg.w);
    let a = clamp((sheet * 0.65 + sheet2 * 0.35 + veil) * core * fade * amount * boost, 0.0, 0.85);
    let band = h * 0.9 + dens * 0.5 + x * 0.8 + t * 0.025 + u.mat2.y;
    var col = smoke_palette(band);
    // Lit from above: the ribbons' edges catch the light.
    col *= 0.9 + 0.3 * sheet;
    return vec4f(col, a);
}

// ───────────────────────────── Panel ─────────────────────────────

struct Hit {
    t: f32,
    hit: bool,
    near_t: f32,
    near_ratio: f32,
};

fn march(ro: vec3f, rd: vec3f, pixel: f32) -> Hit {
    var h: Hit;
    h.hit = false;
    h.t = 0.0;
    h.near_ratio = 1e9;
    h.near_t = 0.0;
    let s = u.shape0.x;
    let reach = s * (1.25 + 0.15 * u.shape0.y) + u.shape1.x * s * 0.9 + u.spikes.x * 0.45 * s + 0.3 * s * max(u.cursor.z * u.cursor.w, 0.0) + u.shape1.w * s;
    // Bounding sphere.
    let b = dot(ro, rd);
    let c = dot(ro, ro) - reach * reach;
    let disc = b * b - c;
    if (disc < 0.0) {
        return h;
    }
    let sq = sqrt(disc);
    var t = max(-b - sq, 0.0);
    let t_end = -b + sq;
    let k = clamp(0.9 / (1.0 + steepness() * 0.6), 0.25, 0.9);
    for (var i = 0; i < 80; i++) {
        let p = ro + rd * t;
        let m = map2(p);
        let d = m.x;
        let ratio = d / (t * pixel);
        if (ratio < h.near_ratio) {
            h.near_ratio = ratio;
            h.near_t = t;
        }
        if (d < 0.0007 * t) {
            h.hit = true;
            h.t = t;
            return h;
        }
        t += max(d * k, m.y * 0.85);
        if (t > t_end) {
            break;
        }
    }
    return h;
}

fn shade(p: vec3f, rd: vec3f, n: vec3f) -> vec3f {
    let t = u.res.z;
    let v = -rd;
    let ndv = clamp(dot(n, v), 0.0, 1.0);
    let rough = clamp(u.mat1.x, 0.0, 1.0);
    let albedo = u.mat0.rgb;
    let metal = u.mat0.w;
    let ao = occlusion(p, n);
    let r = reflect(rd, n);
    // The film: thickness wanders over the surface; the sound shifts it.
    let film = u.mat1.w + u.mat2.y + u.mat2.x * (0.5 + 0.5 * vnoise(p * 2.2 + vec3f(0.0, t * 0.05, 0.0)));
    let irid = thin_film(ndv, film);
    let grazing = pow(1.0 - ndv, 1.5);
    let iw = u.mat1.z * (0.22 + 0.78 * grazing);
    let f0 = mix(vec3f(0.04), albedo, metal);
    let fres = f0 + (vec3f(1.0) - f0) * pow(1.0 - ndv, 5.0);
    let fcol = mix(fres, fres * (0.15 + 2.2 * irid) + irid * 0.05, iw);
    var col = environment(r, rough) * fcol * mix(0.55, 1.0, ao) * u.mat1.y;

    // A sharp clearcoat hotspot from the key light (GGX).
    let l = normalize(vec3f(-0.55, 0.60, 0.58));
    let hv = normalize(l + v);
    let ndh = max(dot(n, hv), 0.0);
    let ndl = max(dot(n, l), 0.0);
    let a = mix(0.035, 0.32, rough);
    let a2 = a * a;
    let dn = ndh * ndh * (a2 - 1.0) + 1.0;
    let ggx = a2 / (PI * dn * dn);
    col += vec3f(2.2) * ggx * ndl * fres.g * u.mat1.y * 0.12;

    // Diffuse (near nothing for the black materials).
    let wrap = u.mat4.x;
    let diff = clamp((dot(n, l) + wrap) / (1.0 + wrap), 0.0, 1.0);
    let sky = 0.5 + 0.5 * n.y;
    let amb = mix(vec3f(0.05), u.bg.rgb * 0.5 + vec3f(0.12), u.bg.w);
    let base = albedo * (1.0 - metal) * (1.0 - u.mat2.w) * (vec3f(1.15) * diff + amb * sky * 1.6) * ao;
    col += base * (vec3f(1.0) - fres * 0.6) * (1.0 + 0.6 * irid * u.mat1.z * (1.0 - metal));

    // A thin-film sheen over the dark body, strongest in the creases.
    col += irid * u.mat1.z * (0.03 + 0.10 * grazing) * (1.2 - ao * 0.6) * (0.6 + 0.6 * u.bg.w);

    // The rim: a bright edge on the silhouette, tinted by the film.
    let rim = pow(1.0 - ndv, 4.0) * u.mat2.z;
    col += rim * mix(vec3f(0.9, 0.92, 1.0), irid * 1.6, 0.55 * u.mat1.z) * mix(1.0, 0.7, u.bg.w);

    // Glass: what lies behind, bent and tinted, plus the reflections.
    if (u.mat2.w > 0.0) {
        let rr = refract(rd, n, 1.0 / 1.47);
        let thick = 1.0 - ndv;
        // The panel seen through the body: bent, darker and tinted where it is thick.
        let bent = environment(rr, rough + 0.08);
        let through = mix(u.bg.rgb * 1.05 + vec3f(0.01), bent * 0.6, 0.3 + 0.4 * thick) * mix(vec3f(1.0), albedo * albedo, 0.25 + 0.75 * thick);
        let caustic = pow(max(dot(rr, vec3f(0.0, -1.0, 0.0)), 0.0), 6.0) * 0.5;
        col = mix(col, col + through * (vec3f(1.0) - fres) + caustic * albedo, u.mat2.w);
    }

    // Emission: something molten in the creases, flaring on a hit.
    if (u.mat3.w > 0.0) {
        let crease = 1.0 - ao;
        // Veins: thin lines where a noise crosses zero, like cracks in a crust.
        let vn = vnoise(p * 2.6 + vec3f(0.0, t * 0.05, 0.0));
        let vein = 1.0 - smoothstep(0.0, 0.07 + 0.05 * u.spikes.w, abs(vn));
        let flicker = 0.75 + 0.25 * vnoise(p * 3.5 + vec3f(0.0, t * 0.6, 0.0));
        let glow = (vein * (0.9 + 1.5 * u.spikes.w) + pow(crease, 1.5) * 1.6 + pow(1.0 - ndv, 3.0) * 0.12) * flicker;
        col += u.mat3.rgb * u.mat3.w * glow;
    }
    return col * u.mat4.y;
}

// The panel's coverage at `px` (rounded corners).
fn corner_mask(px: vec2f) -> f32 {
    let r = u.frame.w;
    if (r <= 0.0) {
        return 1.0;
    }
    let half = u.res.xy * 0.5;
    let q = abs(px - half) - (half - vec2f(r));
    let d = length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
    return clamp(0.5 - d, 0.0, 1.0);
}

@fragment
fn fs_matter(in: VsOut) -> @location(0) vec4f {
    let px = in.uv * u.res.xy;
    let scale = u.frame.z;
    let p = vec2f(px.x - u.frame.x, u.frame.y - px.y) / scale;
    let light = u.bg.w;
    let s = u.shape0.x;

    // The panel: the theme's colour with a soft lift behind the mass.
    var col = u.bg.rgb;
    let lift = exp(-dot(p * vec2f(0.55, 0.75), p * vec2f(0.55, 0.75)) * 1.4);
    col *= 1.0 + lift * mix(0.10, 0.045, light);
    // The shadow it casts on the panel below.
    let sy = -0.70 * s;
    let sd = (p - vec2f(0.0, sy)) / vec2f(0.62 * s, 0.10 * s);
    let shadow = exp(-dot(sd, sd)) * 0.7 + exp(-dot(sd * 0.45, sd * 0.45)) * 0.25;
    col *= 1.0 - shadow * mix(0.55, 0.32, light);

    // The vapour behind the mass.
    let vap = vapour(p);
    col = mix(col, vap.rgb, vap.a);

    // The mass.
    let ro = vec3f(0.0, 0.0, 4.0);
    let focal = 2.8;
    let rd = normalize(vec3f(p, -focal));
    let pixel = 1.0 / (scale * focal);
    let h = march(ro, rd, pixel);
    var cover = 0.0;
    var t_at = h.t;
    if (h.hit) {
        cover = 1.0;
    } else if (h.near_ratio < 1.5) {
        cover = clamp(1.0 - h.near_ratio / 1.5, 0.0, 1.0);
        cover = cover * cover * (3.0 - 2.0 * cover);
        t_at = h.near_t;
    }
    if (cover > 0.0) {
        let pos = ro + rd * t_at;
        let n = normal_at(pos, 0.0015 * s);
        let m = tonemap(shade(pos, rd, n));
        col = mix(col, decode(m), cover);
        // A little vapour in front where it leaves the surface.
        col = mix(col, vap.rgb, vap.a * 0.35 * smoothstep(0.0, 0.25, p.y - 0.15 * s));
    }
    let a = corner_mask(px);
    return vec4f(encode(col) * a, a);
}

// ─────────────────────── NV12 (macOS surfaces) ───────────────────────

// Full-range BT.601, what GPUI's surface shader converts back.
fn rgb_of(uv: vec2f) -> vec3f {
    let c = textureSampleLevel(src, src_sampler, uv, 0.0);
    return c.rgb + u.outer.rgb * (1.0 - c.a);
}

@fragment
fn fs_luma(in: VsOut) -> @location(0) vec4f {
    let c = rgb_of(in.uv);
    return vec4f(dot(c, vec3f(0.299, 0.587, 0.114)), 0.0, 0.0, 1.0);
}

@fragment
fn fs_chroma(in: VsOut) -> @location(0) vec4f {
    // One chroma sample per 2×2 block: the bilinear sample at its centre is
    // their average.
    let c = rgb_of(in.uv);
    let cb = -0.168736 * c.r - 0.331264 * c.g + 0.5 * c.b + 0.5;
    let cr = 0.5 * c.r - 0.418688 * c.g - 0.081312 * c.b + 0.5;
    return vec4f(cb, cr, 0.0, 1.0);
}
