// One instance = one rounded rect. The fragment shader procedurally draws the dot-matrix
// "pixel field" (5 levels, ordered dither); particles are single dots of it.
//
// Transitions are done by the dot matrix itself (no alpha cross-fades): a wave sweeps from the
// click point; as it passes, each dot cell flips from the old layer to the new one in place. The
// front carries light: it lifts the levels of the dots around it, which then cool back down.
// A cell is never empty, so the front has no dark gap. A transition started mid-way stacks: each
// cell shows the layer of the newest wave that has passed it, while the older waves run on.

struct U {
    origin: vec2f,
    size: vec2f,
    ppp: f32,
    nw: f32,      // number of waves (0: no transition)
    _pad: vec2f,
    // Per wave, 3 vec4s: origin.xy, max distance, progress K (0..1); region it acts on (min.xy,
    // max.xy); x = 1 if outside that region is new already.
    w: array<vec4f, 12>,
};
@group(0) @binding(0) var<uniform> u: U;

struct Inst {
    @location(0) rect: vec4f,  // min.xy, max.xy (points, screen space)
    @location(1) color: vec4f, // rgb, intensity (particles: level 0..4)
    @location(2) p: vec4f,     // kind, field: 1 = no gutter, first and last transition layer
    @location(3) q: vec4f,     // header h(pt), hover, tone (0..1), label visibility (0..1)
    @location(4) t: vec4f,     // label box (min.xy, max.xy); empty = none
};

struct V {
    @builtin(position) pos: vec4f,
    @location(0) pt: vec2f,
    @location(1) @interpolate(flat) rect: vec4f,
    @location(2) @interpolate(flat) color: vec4f,
    @location(3) @interpolate(flat) p: vec4f,
    @location(4) @interpolate(flat) q: vec4f,
    @location(5) @interpolate(flat) t: vec4f,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, i: Inst) -> V {
    var corners = array<vec2f, 6>(
        vec2f(0.0, 0.0), vec2f(1.0, 0.0), vec2f(0.0, 1.0),
        vec2f(0.0, 1.0), vec2f(1.0, 0.0), vec2f(1.0, 1.0),
    );
    let pt = mix(i.rect.xy, i.rect.zw, corners[vi]);
    let ndc = (pt - u.origin) / u.size * 2.0 - 1.0;
    var o: V;
    o.pos = vec4f(ndc.x, -ndc.y, 0.0, 1.0);
    o.pt = pt;
    o.rect = i.rect;
    o.color = i.color;
    o.p = i.p;
    o.q = i.q;
    o.t = i.t;
    return o;
}

// 4x4 Bayer threshold of a dot cell, in (0, 1).
fn bayer4(c: vec2f) -> f32 {
    var m = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
    let i = (u32(c.y) & 3u) * 4u + (u32(c.x) & 3u);
    return (m[i] + 0.5) / 16.0;
}

// Colour of a dot at `level` (0..4) of a tile coloured `col`.
fn shade(col: vec3f, level: f32) -> vec3f {
    return col * (0.07 + 0.93 * pow(level / 4.0, 1.6));
}

// Must match WAVE_COOL in app.rs: how long (in progress units) a flipped cell stays hot.
const COOL: f32 = 0.3;

// Wave i at screen point p: how far past this spot the front is. < 0: still old; >= 0: new, and
// e / COOL is how far it has cooled. Distance from the origin plus the dot's Bayer threshold gives
// each spot its delay, so the front is an ordered-dither band, not random grain.
fn wave_e(i: u32, p: vec2f, cs: f32) -> f32 {
    let w = u.w[i * 3u];
    let r = u.w[i * 3u + 1u];
    let x = u.w[i * 3u + 2u];
    if (any(p < r.xy) || any(p > r.zw)) { return select(-1.0, COOL, x.x > 0.5); }
    // Where p lies along the front's path, 0..1 (Shape in app.rs): a ring spreading out from the
    // origin, or (1) closing in on it from the corners.
    let d0 = clamp(length(p - w.xy) / max(w.z, 1.0), 0.0, 1.0);
    let d = select(d0, 1.0 - d0, x.y > 0.5);
    // The band fades in with distance so the click point itself reacts on the very first frame.
    let thr = d * 0.8 + bayer4(floor(p * u.ppp / cs)) * 0.2 * min(d * 4.0, 1.0);
    return w.w * (1.0 + COOL) - thr;
}

@fragment
fn fs(v: V) -> @location(0) vec4f {
    let kind = u32(v.p.x);
    let col = v.color.rgb;
    let inten = v.color.a;
    let hover = v.q.y;
    let lo = u32(v.p.z);
    let hi = u32(v.p.w);

    // Dot lattice fixed to the screen, in physical pixels: like an LED matrix, the dots never
    // move or scale; zooming tiles slide their colours across it. Tiles at rest sit exactly on it.
    // ~2pt dot pitch (3px at 150% scaling), dots fill their cell with no gap.
    let cs = max(2.0, floor(2.0 * u.ppp)); // = layout::grid() * ppp
    let gp = v.pt * u.ppp;
    let cell = floor(gp / cs);
    // Transition: each screen cell belongs to exactly one layer (every layer evaluates the same
    // cell centre), so they never overlap or leave a hole, even while the camera zooms.
    // Light of a wave front: rises on the layer it takes away just ahead of it, cools on the one it
    // brings. A tile shared by several layers is only lit by the waves at its ends.
    var born = 0.0;
    let nw = u32(u.nw);
    if (nw > 0u) {
        let sc = (floor(v.pt * u.ppp / cs) + 0.5) * cs / u.ppp;
        var es = array<f32, 4>(-1.0, -1.0, -1.0, -1.0);
        var s = 0u; // layer shown here: the newest wave passed
        for (var i = 0u; i < nw; i++) {
            es[i] = wave_e(i, sc, cs);
            if (es[i] >= 0.0) { s = i + 1u; }
        }
        if (s < lo || s > hi) { discard; }
        if (s == lo && lo > 0u) {
            let c = 1.0 - clamp(es[lo - 1u] / COOL, 0.0, 1.0);
            born = c * c;
        }
        if (s == hi && hi < nw) {
            let ahead = clamp(1.0 + es[hi] / (COOL * 0.25), 0.0, 1.0);
            born = max(born, ahead * ahead);
        }
    }

    if (kind == 2u) { // particle: one dot, at its level
        return vec4f(shade(col, inten), 1.0);
    }

    // Quiet zone: the dots under a label and one dot around it turn into title strip dots (below),
    // so the text always sits on a dark patch; it fades with the label during transitions.
    var quiet = 0.0;
    if (v.t.z > v.t.x) {
        let cc = (cell + 0.5) * cs / u.ppp;
        let out = max(v.t.xy - cc, cc - v.t.zw);
        quiet = select(0.0, v.q.w, max(out.x, out.y) < cs / u.ppp);
    }

    // Ordered dither: every dot takes one of 5 levels. A tile's value picks the mix of two adjacent
    // levels through a 4x4 Bayer matrix, so tones read as regular patterns instead of noise.
    // Layout gives edge neighbours different tones. Hovered tiles (and every block of a hovered
    // title's folder) ease to the same top level.
    let tone = v.q.z;
    let org = round(v.rect.xy * u.ppp / cs);
    let ts = max(round((v.rect.zw - v.rect.xy) * u.ppp / cs), vec2f(1.0));
    let rd = cell - org;
    // The 4 tones sit between two levels (around 30 % / 70 % mixes: 1.3, 1.7, 2.3, 2.7), never on
    // one, so no tile turns into a solid block. A top-left light sweeps each tile through ~6 Bayer
    // steps (+-0.175 of a level), staying inside its own half so tones never meet. The bands
    // always run at 45 degrees whatever the tile's shape, spread evenly from corner to corner.
    let g = 1.0 - (rd.x + rd.y) / (ts.x + ts.y);
    let k = round(tone * 3.0);
    var n = 1.3 + 0.4 * k + 0.2 * floor(k * 0.5) + (g - 0.5) * 0.35;
    n = mix(n, 3.9, hover);
    // The wave's light lifts the tile's own levels (no white overlay): the pattern itself glows
    // and settles back as it cools.
    n = min(n + 2.0 * born, 3.9);
    // Directory header strip: sparse and dark under its title, lit on hover. A label's quiet zone
    // takes the level of a lit strip (a fading label's mixes it with the tile's, still pure dither).
    let strip = 0.4 + 2.0 * born;
    n = mix(n, strip + 0.6, quiet);
    if (v.pt.y < v.rect.y + v.q.x) { n = strip + 0.6 * hover; }
    let fl = min(floor(n), 3.0);
    var level = fl + step(bayer4(cell), clamp(n - fl, 0.1, 0.9));
    // Gutter: the top row and left column of every tile big enough to spare them, so each shared
    // edge is one dark line; smaller tiles are told apart by their pattern alone.
    // An open folder's body is gutter too: its children cover it, except the gutter around its
    // block of small tiles (see layout.rs).
    let body = v.q.x > 0.0 && v.pt.y >= v.rect.y + v.q.x;
    // Tiles in a folder's block of small ones (p.y = 1) have none: the block has it.
    let own = kind == 0u && v.p.y < 0.5 && min(ts.x, ts.y) >= 3.0;
    if ((own && min(rd.x, rd.y) < 0.5) || body) { level = 0.0; }
    return vec4f(shade(col, level) * inten, 1.0);
}
