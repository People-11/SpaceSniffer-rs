// One instance = one rounded rect. The fragment shader procedurally draws the dot-matrix
// "pixel field", twinkling lit dots, the white-hot discovery flash and additive glow.
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
    @location(2) p: vec4f,     // age(s), seed, kind, glow radius(pt)
    @location(3) q: vec4f,     // header h(pt), hover, tone (0..1), lit density
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
    let seed = v.p.y;
    let kind = u32(v.p.z);
    let glow = v.p.w;
    let col = v.color.rgb;
    let inten = v.color.a;
    let hover = v.q.y;
    let role = u32(v.o.x);

    // Dot lattice fixed to the screen, in physical pixels: like an LED matrix, the dots never
    // move or scale; zooming tiles slide their colours across it. Tiles at rest sit exactly on it.
    // ~3pt dot pitch: a 3px dot + 1px gap at 150% scaling.
    let cs = max(3.0, floor(3.0 * u.ppp)); // = layout::grid() * ppp
    let gp = v.pt * u.ppp;
    let cell = floor(gp / cs);
    let f = gp - cell * cs;

    let hd = (cs - 1.0) * 0.5;
    let dq = abs(f - hd);
    let dot = clamp(hd + 0.5 - max(dq.x, dq.y), 0.0, 1.0);
    // Transition: each screen cell belongs to exactly one layer (both layers evaluate the same
    // cell centre), so the two never overlap or leave a hole, even while the camera zooms.
    var born = 0.0;
    if (role != 0u) {
        let sc = (floor(v.pt * u.ppp / cs) + 0.5) * cs / u.ppp;
        let e = wave_e(sc, cs);
        if ((role == 2u) == (e >= 0.0)) { discard; }
        let c = 1.0 - clamp(e / COOL, 0.0, 1.0);
        born = select(0.0, c * c, role == 1u) * dot;
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

    let h = hash(cell + vec2f(seed * 131.0, seed * 71.0));
    let h2 = hash(cell.yx + vec2f(seed * 17.0 + 3.0, 911.0));

    // Lit dots: uniformly random over the tile, twinkling.
    let density = v.q.w * 0.9;
    // Every dot flickers on its own clock: time is cut into per-dot slots and each slot re-rolls
    // whether the dot lights, so over time any dot can light up (no fixed lit set).
    // Slow, so labels on top stay readable.
    let tt = u.time * (0.15 + h2 * 0.3) + h * 17.0;
    let slot = floor(tt);
    let roll = hash(cell + vec2f(slot * 1.618 + seed * 13.0, slot * 2.414 + 5.0));
    // Quiet zone: whole dots under a label and up to 2 dots around it are dimmed (they still
    // twinkle), so the text always sits on a dark patch.
    var quiet = 0.0;
    if (v.t.z > v.t.x) {
        let cc = (cell + 0.5) * cs / u.ppp;
        let out = max(v.t.xy - cc, cc - v.t.zw);
        quiet = clamp(1.0 - max(out.x, out.y) / (2.0 * cs / u.ppp), 0.0, 1.0);
    }
    let lit = step(roll, density * 1.3) * sin(3.14159265 * fract(tt));

    // Per-tile tone (colour, background and unlit dot level together): with no outlines this is
    // what splits neighbouring tiles (layout gives edge neighbours different tones). Open folders use the same
    // field, so space left by children too small to lay out still reads as dots, not a hole.
    // A directly hovered tile eases to the top tone first, so all of them reach the same brightness.
    let tone = mix(v.q.z, 1.0, v.o.y);
    let base = col * (0.55 + 0.45 * tone);
    var rgb = base * (0.01 + 0.05 * tone + dot * mix(0.07 + 0.17 * tone, 1.0, lit) * (1.0 - 0.6 * quiet));
    // Directory header strip.
    if (v.pt.y < v.rect.y + v.q.x) { rgb = base * 0.16 + vec3f(0.03); }
    rgb *= inten;

    // No outlines: neighbouring tiles are told apart by their (per-tile varied) colour alone.

    // White-hot: discovery flash and newborn transition dots.
    rgb = mix(rgb, vec3f(1.0), max(hot * mix(0.2, 0.55, dot), born * 0.5));
    // Hover: the whole tile (dots and background) lifts in brightness.
    rgb = rgb * (1.0 + hover * 0.6) + col * hover * 0.07;
    return vec4f(rgb, 1.0);
}
