// One instance = one rounded rect. The fragment shader procedurally draws the dot-matrix
// "pixel field" (5 levels, ordered dither), the white-hot discovery flash and additive glow.
//
// Transitions are done by the dot matrix itself (no alpha cross-fades): a wave sweeps from the
// click point; as it passes, each dot cell flips from the old layer to the new one in place, the
// new dot born white-hot and cooling down. A cell is never empty, so the front has no dark gap.

struct U {
    origin: vec2f,
    size: vec2f,
    ppp: f32,
    time: f32,
    _pad: vec2f,
    wave: vec4f,  // origin.xy, max distance, progress K (0..1)
    wrect: vec4f, // region the wave acts on (min.xy, max.xy)
    wopt: vec4f,  // x = progress outside wrect
};
@group(0) @binding(0) var<uniform> u: U;

struct Inst {
    @location(0) rect: vec4f,  // min.xy, max.xy (points, screen space)
    @location(1) color: vec4f, // rgb, intensity
    @location(2) p: vec4f,     // age(s), unused, kind, glow radius(pt)
    @location(3) q: vec4f,     // header h(pt), hover, tone (0..1), unused
    @location(4) o: vec4f,     // role: 0 static, 1 incoming, 2 outgoing; y: hover tone equalisation
    @location(5) t: vec4f,     // label box (min.xy, max.xy); empty = none
};

struct V {
    @builtin(position) pos: vec4f,
    @location(0) pt: vec2f,
    @location(1) @interpolate(flat) rect: vec4f,
    @location(2) @interpolate(flat) color: vec4f,
    @location(3) @interpolate(flat) p: vec4f,
    @location(4) @interpolate(flat) q: vec4f,
    @location(5) @interpolate(flat) o: vec4f,
    @location(6) @interpolate(flat) t: vec4f,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, i: Inst) -> V {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0),
    );
    let g = i.p.w;
    let pt = mix(i.rect.xy - g, i.rect.zw + g, corners[vi]);
    let ndc = (pt - u.origin) / u.size * 2.0 - 1.0;
    var o: V;
    o.pos = vec4f(ndc.x, -ndc.y, 0.0, 1.0);
    o.pt = pt;
    o.rect = i.rect;
    o.color = i.color;
    o.p = i.p;
    o.q = i.q;
    o.o = i.o;
    o.t = i.t;
    return o;
}

fn hash(p: vec2f) -> f32 {
    var x = bitcast<vec2u>(p) * vec2u(1597334673u, 3812015801u);
    let n = (x.x ^ x.y) * 1597334673u;
    return f32(n) / 4294967295.0;
}

// 4x4 Bayer threshold of a dot cell, in (0, 1).
fn bayer4(c: vec2f) -> f32 {
    var m = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
    let i = (u32(c.y) & 3u) * 4u + (u32(c.x) & 3u);
    return (m[i] + 0.5) / 16.0;
}

fn sd_box(p: vec2f, half: vec2f, r: f32) -> f32 {
    let q = abs(p) - half + r;
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Must match WAVE_COOL in app.rs: how long (in progress units) a flipped cell stays hot.
const COOL: f32 = 0.3;

// Wave at screen point p: how far past this spot the front is. < 0: still old; >= 0: new, and
// e / COOL is how far it has cooled. Distance from the origin plus clumped noise (3x3-dot clumps)
// gives each spot its delay.
fn wave_e(p: vec2f, cs: f32) -> f32 {
    if (any(p < u.wrect.xy) || any(p > u.wrect.zw)) { return select(-1.0, COOL, u.wopt.x > 0.5); }
    let clump = floor(p * u.ppp / (cs * 3.0));
    let d = clamp(length(p - u.wave.xy) / max(u.wave.z, 1.0), 0.0, 1.0);
    // Jitter fades in with distance so the click point itself reacts on the very first frame.
    let thr = d * 0.8 + hash(clump + vec2f(7.0, 13.0)) * 0.2 * min(d * 4.0, 1.0);
    return u.wave.w * (1.0 + COOL) - thr;
}

@fragment
fn fs(v: V) -> @location(0) vec4f {
    let age = v.p.x;
    let kind = u32(v.p.z);
    let glow = v.p.w;
    let col = v.color.rgb;
    let inten = v.color.a;
    let hover = v.q.y;
    let role = u32(v.o.x);

    // Dot lattice fixed to the screen, in physical pixels: like an LED matrix, the dots never
    // move or scale; zooming tiles slide their colours across it. Tiles at rest sit exactly on it.
    // ~3pt dot pitch (4px at 150% scaling), dots fill their cell with no gap.
    let cs = max(3.0, floor(3.0 * u.ppp)); // = layout::grid() * ppp
    let gp = v.pt * u.ppp;
    let cell = floor(gp / cs);
    // Transition: each screen cell belongs to exactly one layer (both layers evaluate the same
    // cell centre), so the two never overlap or leave a hole, even while the camera zooms.
    var born = 0.0;
    if (role != 0u) {
        let sc = (floor(v.pt * u.ppp / cs) + 0.5) * cs / u.ppp;
        let e = wave_e(sc, cs);
        if ((role == 2u) == (e >= 0.0)) { discard; }
        let c = 1.0 - clamp(e / COOL, 0.0, 1.0);
        born = select(0.0, c * c, role == 1u);
    }

    let half = (v.rect.zw - v.rect.xy) * 0.5;
    let d = sd_box(v.pt - (v.rect.xy + v.rect.zw) * 0.5, half, 0.0);
    // Freshly discovered: a soft white flash, cooling over ~0.8s.
    let hot = exp(-age * 4.0);

    // Glow outside the rect: alpha 0 => additive under premultiplied blending.
    if (d > 0.0) {
        if (glow <= 0.0) { discard; }
        let a = (0.55 + hot * 0.5) * inten * exp(-d / (glow * 0.33)) * (1.0 - smoothstep(glow * 0.7, glow, d));
        return vec4f(mix(col, vec3f(1.0), hot * 0.45) * a, 0.0);
    }

    if (kind == 2u) { // particle: additive solid square
        return vec4f(col * inten, 0.0);
    }

    // Quiet zone: dots under a label and up to 2 dots around it are dimmed, so the text always
    // sits on a dark patch.
    var quiet = 0.0;
    if (v.t.z > v.t.x) {
        let cc = (cell + 0.5) * cs / u.ppp;
        let out = max(v.t.xy - cc, cc - v.t.zw);
        quiet = clamp(1.0 - max(out.x, out.y) / (2.0 * cs / u.ppp), 0.0, 1.0);
    }

    // Ordered dither: every dot takes one of 5 levels. A tile's value picks the mix of two adjacent
    // levels through a 4x4 Bayer matrix, so tones read as regular patterns instead of noise.
    // Layout gives edge neighbours different tones; a directly hovered tile eases to one target.
    let tone = v.q.z;
    let eq = v.o.y;
    let org = round(v.rect.xy * u.ppp / cs);
    let ts = max(round((v.rect.zw - v.rect.xy) * u.ppp / cs), vec2f(1.0));
    let rd = cell - org;
    // The 4 tones sit between two levels (25 % / 75 % mixes: 1.25, 1.75, 2.25, 2.75), never on
    // one, so no tile turns into a solid block; a slight top-left light varies the pattern.
    let g = 1.0 - (rd.x / ts.x + rd.y / ts.y) * 0.5;
    var n = 1.25 + 0.5 * round(tone * 3.0) + (g - 0.5) * 0.2;
    n = mix(n, 3.5, eq) + 0.8 * max(hover - eq, 0.0);
    n *= 1.0 - 0.8 * quiet;
    // Directory header strip: sparse and dark under its title.
    if (v.pt.y < v.rect.y + v.q.x) { n = 0.4 + 0.6 * hover; }
    let fl = min(floor(n), 3.0);
    var level = fl + step(bayer4(cell), clamp(n - fl, 0.15, 0.85));
    // Gutter: the top row and left column of every tile big enough to spare them, so each shared
    // edge is one dark line; smaller tiles are told apart by their pattern alone.
    if (min(ts.x, ts.y) >= 4.0 && min(rd.x, rd.y) < 0.5) { level = 0.0; }
    var rgb = col * (0.07 + 0.93 * pow(level / 4.0, 1.6));
    rgb *= inten;

    // White-hot: discovery flash and newborn transition dots.
    rgb = mix(rgb, vec3f(1.0), max(hot * 0.55, born * 0.5));
    return vec4f(rgb, 1.0);
}
