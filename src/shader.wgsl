// One instance = one rect, drawn as a dot-matrix pixel field (5 levels, ordered dither). Transitions:
// a wave from the click flips each dot cell from the old layer to the new one, its front lighting up
// the dots around it. Stacked waves: a cell shows the layer of the newest one passed.

struct U {
    origin: vec2f,
    size: vec2f,
    ppp: f32,
    nw: f32,      // number of waves (0: no transition)
    cell0: vec2f, // first dot cell of the draw's texture (origin/size above cover its cells)
    header: f32,  // title strip height (pt) of open folders
    ngroups: f32,
    // Per wave, 3 vec4s: origin.xy, max distance, progress; region; x = 1: outside it is new (zoom out).
    w: array<vec4f, 12>,
    // Per layer, its camera: layout rect, where that is on screen (min.xy, max.xy each).
    cams: array<vec4f, 10>,
    // Folders lit as a whole: rect (layout coords), then (lift, layer whose light it is, -, -).
    groups: array<vec4f, 32>,
    boxes: array<vec4f, 255>, // label boxes (min.xy, max.xy), indexed by an instance's d.y
};
@group(0) @binding(0) var<uniform> u: U;

// Packed (see gfx::Inst, gfx::Dyn); the vertex shader unpacks it into V.
struct Inst {
    @location(0) rect: vec4f,  // min.xy, max.xy (points, in its first layer's layout)
    @location(1) color: vec4f, // rgb, intensity (particles: level / 4)
    @location(2) p: vec4u,     // kind | 4 no gutter | 8 open; unused; first and last transition layer
    @location(3) q: vec4f,     // hover, tone (0..1), unused, unused
    @location(4) d: vec4u,     // this frame: hover, label box (255: none), label visibility (0..255), unused
};

struct V {
    @builtin(position) pos: vec4f,
    // color: rgb, intensity (particles: level 0..4); p: kind, 1 = no gutter, first and last
    // transition layer; q: title strip height (pt), hover, tone, label visibility; t: label box.
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
    // Its first layer's camera (as app.rs `apply`, in the same order, so tiles land on the same cells).
    let src = u.cams[i.p.z * 2u];
    let dst = u.cams[i.p.z * 2u + 1u];
    let k = (dst.zw - dst.xy) / (src.zw - src.xy);
    let rect = vec4f(dst.xy + (i.rect.xy - src.xy) * k, dst.xy + (i.rect.zw - src.xy) * k);
    let pt = mix(rect.xy, rect.zw, corners[vi]);
    let ndc = (pt - u.origin) / u.size * 2.0 - 1.0;
    var o: V;
    o.pos = vec4f(ndc.x, -ndc.y, 0.0, 1.0);
    o.pt = pt;
    o.rect = rect;
    let kind = i.p.x & 3u;
    o.color = vec4f(i.color.rgb, i.color.a * select(1.0, 4.0, kind == 2u));
    o.p = vec4f(f32(kind), f32((i.p.x >> 2u) & 1u), f32(i.p.z), f32(i.p.w));
    // Inside a folder lit as a whole by its last layer's light (strictly inside): lit as much.
    var hover = max(i.q.x, f32(i.d.x) / 255.0);
    for (var g = 0u; g < u32(u.ngroups); g++) {
        let gr = u.groups[g * 2u];
        let gv = u.groups[g * 2u + 1u];
        if (u32(gv.y) == i.p.w && any(gr != i.rect) && all(gr.xy <= i.rect.xy) && all(i.rect.zw <= gr.zw)) {
            hover = max(hover, gv.x);
        }
    }
    o.q = vec4f(select(0.0, u.header, (i.p.x & 8u) != 0u), hover, i.q.y, f32(i.d.z) / 255.0);
    o.t = select(vec4f(0.0, 0.0, -1.0, -1.0), u.boxes[min(i.d.y, 254u)], i.d.y < 255u);
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

// Transition waves (`Wave` in anim.rs is the CPU twin: keep them in step). Along a wave's path its
// far corner is 1; the band has fixed widths there, so it keeps one shape all the way.
const COOL: f32 = 0.15;   // glow behind the front, on the layer it brings, fading out
const AHEAD: f32 = 0.04;  // glow ahead of the front, on the layer it takes away, fading in
const DITHER: f32 = 0.04; // the front's ordered-dither band (narrowed at the origin: it flips at once)
const TYPE: f32 = 0.3;    // labels type in behind the front; it travels on until the last one has

// How far wave i's front is past p (< 0: not there yet), for a cell at dither threshold b. Outside
// the wave's region: long past if all of it is new already (zoom out), else never.
fn wave_e(i: u32, p: vec2f, b: f32) -> f32 {
    let w = u.w[i * 3u]; // origin.xy, reach, progress
    let r = u.w[i * 3u + 1u];
    let out = u.w[i * 3u + 2u].x > 0.5;
    if (any(p < r.xy) || any(p > r.zw)) { return select(-1.0, 1.0, out); }
    let d0 = clamp(length(p - w.xy) / max(w.z, 1.0), 0.0, 1.0);
    let d = select(d0, 1.0 - d0, out); // a zoom out's ring closes in on the origin
    return w.w * (1.0 + DITHER + TYPE) - d - DITHER * b * min(d / DITHER, 1.0);
}

@fragment
fn fs(v: V) -> @location(0) vec4f {
    let kind = u32(v.p.x);
    let col = v.color.rgb;
    let inten = v.color.a;
    let hover = v.q.y;
    let lo = u32(v.p.z);
    let hi = u32(v.p.w);
    if (kind == 2u) { // particle: one dot, at its level
        return vec4f(shade(col, inten), 1.0);
    }

    // Dot lattice fixed to the screen in physical pixels (~2pt pitch): zooming tiles slide across it.
    let cs = max(2.0, floor(2.0 * u.ppp)); // = layout::grid() * ppp
    let cell = floor(v.pt * u.ppp / cs);
    let cc = (cell + 0.5) * cs / u.ppp; // its centre, in points
    // Waves: every layer evaluates the same cell centre, so each cell shows exactly one layer, that of
    // the newest wave passed. `glow`: the share of its dots the fronts around it light up.
    var glow = 0.0;
    let nw = u32(u.nw);
    if (nw > 0u) {
        let b = bayer4(cell);
        var es = array<f32, 4>(-1.0, -1.0, -1.0, -1.0);
        var s = 0u;
        for (var i = 0u; i < nw; i++) {
            es[i] = wave_e(i, cc, b);
            if (es[i] >= 0.0) { s = i + 1u; }
        }
        if (s < lo || s > hi) { discard; }
        if (s == lo && lo > 0u) { glow = 1.0 - clamp(es[lo - 1u] / COOL, 0.0, 1.0); }
        if (s == hi && hi < nw) { glow = max(glow, clamp(1.0 + es[hi] / AHEAD, 0.0, 1.0)); }
    }

    // Quiet zone: a dark patch under a label (and one dot around it), fading with the label.
    var quiet = 0.0;
    if (v.t.z > v.t.x) {
        let out = max(v.t.xy - cc, cc - v.t.zw);
        quiet = select(0.0, v.q.w, max(out.x, out.y) < cs / u.ppp);
    }

    // Ordered dither: a tile's value mixes two adjacent levels through the Bayer matrix.
    let tone = v.q.z;
    let org = round(v.rect.xy * u.ppp / cs);
    let ts = max(round((v.rect.zw - v.rect.xy) * u.ppp / cs), vec2f(1.0));
    let rd = cell - org;
    // The 4 tones sit between levels (1.3, 1.7, 2.3, 2.7), never on one, so no tile turns solid; a
    // 45-degree light sweeps each tile by +-0.175 of a level, staying within its tone.
    let g = 1.0 - (rd.x + rd.y) / (ts.x + ts.y);
    let k = round(tone * 3.0);
    var n = 1.3 + 0.4 * k + 0.2 * floor(k * 0.5) + (g - 0.5) * 0.35;
    n = mix(n, 3.4, hover); // a level below the wave's light, which then still shows on a lit tile
    // Title strip: sparse and dark, lit on hover; a label's quiet zone takes a lit strip's level.
    let strip = 0.4;
    n = mix(n, strip + 0.6, quiet);
    if (v.pt.y < v.rect.y + v.q.x) { n = strip + 0.6 * hover; }
    let fl = min(floor(n), 3.0);
    var level = fl + step(bayer4(cell), clamp(n - fl, 0.1, 0.9));
    // Wave glow: that share of the dots goes to the top level, in a Bayer order of its own (transposed
    // and offset from the tone's). A density, not a lift of the tile's level, so the band has one
    // shape on every tile, dark or bright, whatever its colour.
    if (bayer4(cell.yx + vec2f(1.0, 2.0)) < glow) { level = 4.0; }
    // Gutter: top row and left column of every tile big enough, so shared edges are one dark line. An
    // open folder's body is gutter too (its children cover it).
    let body = v.q.x > 0.0 && v.pt.y >= v.rect.y + v.q.x;
    // Tiles in a folder's block of small ones (p.y = 1) have none: the block has it.
    let own = v.p.y < 0.5 && min(ts.x, ts.y) >= 3.0;
    if ((own && min(rd.x, rd.y) < 0.5) || body) { level = 0.0; }
    return vec4f(shade(col, level) * inten, 1.0);
}

// Blit: the draw's one-texel-per-cell texture scaled up onto the callback's viewport.
@group(1) @binding(0) var field: texture_2d<f32>;

@vertex
fn vs_blit(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let c = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(c * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs_blit(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let cs = max(2.0, floor(2.0 * u.ppp));
    let c = vec2i(floor(pos.xy / cs) - u.cell0);
    return textureLoad(field, clamp(c, vec2i(0), vec2i(textureDimensions(field)) - 1), 0);
}
