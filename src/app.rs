use crate::gfx::{self, Inst};
use crate::layout::{self, Tile};
use crate::perf;
use crate::tree::{self, CATS, Item, ItemMap, Tree};
use crate::win;
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Key, Margin, PointerButton, Pos2, Rect, RichText, Sense, Vec2, pos2, vec2};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

const BG: Color32 = Color32::from_rgb(11, 12, 15);
const CARD: Color32 = Color32::from_rgb(21, 22, 26);
const TEXT_DIM: Color32 = Color32::from_rgb(140, 144, 152);
const MAX_PARTICLES: usize = 30000;
/// Transition lengths: in-place expand/collapse must feel instant, zooms carry more motion.
const LOCAL_SECS: f32 = 0.6;
const ZOOM_SECS: f32 = 0.65;
const HOVER_SECS: f32 = 0.15;

/// Every folder shares one color (a warm copper); files are colored by type.
const FOLDER_RGB: [u8; 3] = [184, 116, 66];

fn c32(c: u8) -> Color32 {
    let [r, g, b] = CATS[c as usize].1;
    Color32::from_rgb(r, g, b)
}

pub fn fmt_size(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

fn lerp_rect(a: Rect, b: Rect, k: f32) -> Rect {
    Rect::from_min_max(a.min + (b.min - a.min) * k, a.max + (b.max - a.max) * k)
}

// ------------------------------------------------------------------ transitions
//
// Geometry is a pure camera move: no tile is ever rearranged. The content swap is done by the dot
// matrix itself: a wave from the click point flips each dot cell from the old layout to the new one
// (born white-hot, cooling down) while labels erase / type in. The clicked folder's title is the
// shared element and travels continuously.

fn is_ancestor(t: &Tree, a: u32, mut b: u32) -> bool {
    while b != 0 {
        b = t.dirs[b as usize].parent;
        if b == a {
            return true;
        }
    }
    false
}

/// Rect of `d` (or its nearest ancestor that has a tile) in `tiles`.
fn find_rect(t: &Tree, tiles: &[Tile], mut d: u32) -> Option<Rect> {
    loop {
        if let Some(x) = tiles.iter().find(|x| x.item == Item::Dir(d)) {
            return Some(x.rect);
        }
        if d == 0 {
            return None;
        }
        d = t.dirs[d as usize].parent;
    }
}

/// Rect of dir `d` in the layout of `view`: its tile in `tiles`, or, when it has none (e.g. a
/// back/forward target whose ancestor was folded since), where it would sit with the path down to
/// it opened, so zooms start/end at its real size instead of the folded ancestor's.
fn place(t: &Tree, tiles: &[Tile], view: u32, area: Rect, expanded: &HashSet<u32>, pitch: f32, d: u32) -> Option<Rect> {
    if let Some(x) = tiles.iter().find(|x| x.item == Item::Dir(d)) {
        return Some(x.rect);
    }
    let mut open = expanded.clone();
    let mut a = t.dirs[d as usize].parent;
    while a != view && a != 0 {
        open.insert(a);
        a = t.dirs[a as usize].parent;
    }
    let mut out = vec![];
    layout::rects(t, view, area, &open, pitch, d, &mut out);
    find_rect(t, &out, d)
}

/// One frame of hover easing. `direct`: (item, level 0..1) of tiles lit on their own (`lit`: the
/// hovered tile and its parent); `groups`: folders whose whole block is lit (`group`: the hovered
/// title's folder). Levels move by `step` per frame.
///
/// Moving between nested titles (or a title and a block under it) must never dim or stall what
/// stays lit, however fast and however many levels:
/// - an entry inside the folder now being lit keeps rising until that folder has caught up with
///   it (its tiles are about to be lit by that folder anyway), then fades unseen beneath it;
/// - what is lit is never dimmer than the lit folders around it, so it starts from their level.
fn ease_hover(
    direct: &mut Vec<(Item, f32)>,
    groups: &mut Vec<(Item, f32)>,
    lit: [Option<Item>; 2],
    group: Option<Item>,
    step: f32,
    rect_of: impl Fn(Item) -> Option<Rect>,
) {
    let inside = |it: Item, outer: Rect| rect_of(it).is_some_and(|r| r != outer && outer.contains_rect(r));
    // Inherit first, from the levels as they were shown last frame, then move everything a step.
    for h in lit.into_iter().flatten() {
        if !direct.iter().any(|x| x.0 == h) {
            direct.push((h, 0.0));
        }
    }
    if let Some(g) = group
        && !groups.iter().any(|x| x.0 == g)
    {
        groups.push((g, 0.0));
    }
    let around = |it: Item, groups: &[(Item, f32)]| groups.iter().filter(|g| rect_of(g.0).is_some_and(|r| inside(it, r))).map(|g| g.1).fold(0.0, f32::max);
    for x in direct.iter_mut().filter(|x| lit.contains(&Some(x.0))) {
        x.1 = x.1.max(around(x.0, groups));
    }
    if let Some(i) = groups.iter().position(|x| Some(x.0) == group) {
        groups[i].1 = groups[i].1.max(around(groups[i].0, groups));
    }
    let target = group.and_then(|g| Some((rect_of(g)?, groups.iter().find(|x| x.0 == g)?.1)));
    let carried = |it: Item, v: f32| target.is_some_and(|(r, tv)| tv < v && inside(it, r));
    let ease = |x: &mut (Item, f32), on: bool| x.1 = if on || carried(x.0, x.1) { (x.1 + step).min(1.0) } else { x.1 - step };
    direct.iter_mut().for_each(|x| ease(x, lit.contains(&Some(x.0))));
    groups.iter_mut().for_each(|x| ease(x, group == Some(x.0)));
    direct.retain(|x| x.1 > 0.0);
    groups.retain(|x| x.1 > 0.0);
}

/// Axis-aligned map taking rect `.0` onto rect `.1` (scale and offset per axis).
type Map = (Rect, Rect);

fn apply(m: Map, p: Pos2) -> Pos2 {
    m.1.min + (p - m.0.min) * (m.1.size() / m.0.size())
}

fn apply_rect(m: Map, r: Rect) -> Rect {
    Rect::from_min_max(apply(m, r.min), apply(m, r.max))
}

fn inverse(m: Map) -> Map {
    (m.1, m.0)
}

/// `g`, then `f`.
fn then(g: Map, f: Map) -> Map {
    (g.0, apply_rect(f, g.1))
}

/// Camera of the top layout at eased progress `e`, from `v0` (the part of it on screen when the
/// step began) to rest. Zooming in narrows the view onto `area`; zooming out grows the screen image
/// of `v0` back, the inverse motion, so both have the same (log-scale) pace. A plain lerp of the
/// view when zooming in would spend nearly all the perceived zoom in the first few frames.
fn top_cam(v0: Rect, area: Rect, e: f32) -> Map {
    if v0.area() > area.area() { (lerp_rect(v0, area, e), area) } else { (v0, lerp_rect(area, v0, e)) }
}

/// The part of a new top layout on screen as it takes over: `cam` is the camera of the layout
/// under it, `rel` maps that layout onto the new one. Starting there, nothing jumps.
fn view_after(cam: Map, rel: Map, area: Rect) -> Rect {
    apply_rect(inverse(then(inverse(rel), cam)), area)
}

/// One transition: brings in the layout above `old` through its own wave.
struct Step {
    /// The layout it replaces (the layer under it).
    old: Vec<Tile>,
    /// Old layout coords -> new layout coords (zooms: the child's tile <-> the whole view).
    rel: Map,
    /// Expand/collapse in place: tiles it leaves unchanged are shared by both layers.
    local: bool,
    /// Shared element: its title never erases, it rides the camera.
    anchor: Option<Item>,
    t0: f32,
    /// The wave, in the new layout's coords: origin, the region it flips, and whether everything
    /// outside that region is new already.
    origin: Pos2,
    rect: Rect,
    outside: bool,
    /// `old` by item, when an expand/collapse on either side of it needs to find its unchanged
    /// tiles (`old` never changes, so this is built once).
    at: Option<At>,
    /// The drive chooser, when that is what this step leaves (`old` is empty then).
    chooser: Option<Chooser>,
    /// `old`'s hover light when the step began, kept until the wave flips it (the live light moves
    /// on: what is clicked is no longer hovered once the new layout is under the pointer).
    lit: Lit,
}

/// Hover light of a layout: directly lit tiles (the hovered one and its parent; a handful), eased
/// 0..1; and the rects of folders whose whole block is lit, with their eased lift.
#[derive(Clone, Default)]
struct Lit {
    tiles: Vec<(Item, f32)>,
    groups: Vec<(Rect, f32)>,
}

/// The drive chooser as a layer of a transition: the bottom one of the zoom into a scan (as it was
/// when the drive was picked), the top one of the zoom out of it back to the chooser. Screen coords
/// are its layout coords.
#[derive(Clone)]
struct Chooser {
    /// Drive blocks, and their title strips with the text in them.
    insts: Vec<Inst>,
    titles: Vec<(Rect, String)>,
    /// Each drive's block (as in `App::drives`).
    blocks: Vec<Rect>,
    /// The picked drive's block (None: a typed path, no zoom), and where the wave starts.
    block: Option<Rect>,
    click: Pos2,
    /// Its camera (layout -> screen, see [`Anim::motion`]) and card opacity when the drive was
    /// picked: picked mid-way through a zoom back to it, the zoom into the drive takes over from
    /// there. None: at rest.
    cam: Option<[Map; 2]>,
    card: f32,
}

/// Part of a zoom out of (into) the chooser over which its card (heading, path row) fades away (in).
const CARD_FADE: f32 = 0.3;

/// What a layout depends on: (view, area, tree version, expand version).
type LayoutKey = (u32, Rect, u64, u64);

/// Tiles by item: (rect, open).
type At = ItemMap<(Rect, bool)>;

fn index(tiles: &[Tile]) -> At {
    tiles.iter().map(|x| (x.item, (x.rect, x.open))).collect()
}

impl Step {
    fn secs(&self) -> f32 {
        if self.local { LOCAL_SECS } else { ZOOM_SECS }
    }

    fn k(&self, now: f32) -> f32 {
        (now - self.t0) / self.secs()
    }
}

/// A view (layout rect on screen) as centre and log size: zooms move it at an even pace.
fn params(v: Rect) -> [f32; 4] {
    [v.center().x, v.center().y, v.width().ln(), v.height().ln()]
}

fn from_params(p: [f32; 4]) -> Rect {
    Rect::from_center_size(pos2(p[0], p[1]), vec2(p[2].exp(), p[3].exp()))
}

/// Transitions never reset or skip: one started mid-way stacks on top. Layer j is `steps[j].old`,
/// the top one the current tiles; every step's wave keeps running, and each dot cell shows the
/// layer of the newest wave that has passed it. The camera starts from what is on screen and eases
/// the top layout to rest; the layers under it follow through each step's `rel`.
struct Anim {
    steps: Vec<Step>,
    /// Where the newest step's own path starts (see [`top_cam`]): the top layout's view when it
    /// began, carried on by `coast`.
    v0: Rect,
    /// An interrupted motion first comes to a stop like every zoom does (the [`ease`] tail, over
    /// HOVER_SECS), from the velocity it had: this far on (in [`params`]) from where it was. Zero
    /// when it began at rest.
    coast: [f32; 4],
}

/// Time step over which a camera's velocity is measured.
const DT: f32 = 1.0 / 240.0;

impl Anim {
    /// Stack `step` on the running transition (or start one), its layout taking over from what is
    /// on screen: `from`, the camera of the layout under it now and DT before (see [`Anim::motion`]).
    /// Past MAX_WAVES the oldest layer flips at once.
    fn push(anim: &mut Option<Anim>, from: [Map; 2], area: Rect, step: Step) {
        let (p, q) = (params(view_after(from[0], step.rel, area)), params(view_after(from[1], step.rel, area)));
        // Stopping from velocity v along `ease` (whose slope starts at 3) over HOVER_SECS goes v * HOVER_SECS / 3.
        let coast = std::array::from_fn(|i| (p[i] - q[i]) / DT * HOVER_SECS / 3.0);
        let v0 = from_params(std::array::from_fn(|i| p[i] + coast[i]));
        let a = anim.get_or_insert_with(|| Anim { steps: vec![], v0, coast });
        (a.v0, a.coast) = (v0, coast);
        a.steps.push(step);
        if a.steps.len() > gfx::MAX_WAVES {
            a.steps.remove(0); // shortcut: the oldest layer flips at once; needs 5 clicks inside one transition
        }
    }

    /// Camera of the top layout now (at rest: none).
    fn cam_now(anim: &Option<Anim>, area: Rect, now: f32) -> Map {
        anim.as_ref().map_or((area, area), |a| *a.cams(area, now).last().unwrap())
    }

    /// The top layout's camera now and DT before: what a new step takes over from, and how fast
    /// that is moving.
    fn motion(anim: &Option<Anim>, area: Rect, now: f32) -> [Map; 2] {
        [Anim::cam_now(anim, area, now), Anim::cam_now(anim, area, now - DT)]
    }

    /// Camera (layout -> screen) of every layer, bottom first.
    fn cams(&self, area: Rect, now: f32) -> Vec<Map> {
        let mut c = top_cam(self.v0, area, 1.0);
        if let Some(s) = self.steps.last() {
            // Coast to a stop, then the step's own path in the time left.
            let d = if self.coast == [0.0; 4] { 0.0 } else { HOVER_SECS };
            let t = now - s.t0;
            c = if t < d {
                let p = params(self.v0);
                (from_params(std::array::from_fn(|i| p[i] - self.coast[i] * (1.0 - ease(t / d)))), area)
            } else {
                top_cam(self.v0, area, ease((t - d) / (s.secs() - d)))
            };
        }
        let mut cams = vec![c];
        for s in self.steps.iter().rev() {
            c = then(s.rel, c);
            cams.push(c);
        }
        cams.reverse();
        cams
    }

    /// Every step's wave on screen this frame.
    fn waves(&self, cams: &[Map], now: f32) -> Vec<Wave> {
        let w = |(s, &c): (&Step, &Map)| {
            let (origin, rect) = (apply(c, s.origin), apply_rect(c, s.rect));
            Wave { origin, maxd: far_corner(origin, rect), k: s.k(now).clamp(0.0, 1.0), rect, outside: s.outside }
        };
        self.steps.iter().zip(&cams[1..]).map(w).collect()
    }

    /// Drop the layers no cell can show any more: everything under a finished wave that covers the
    /// screen. False once the newest step is done (nothing left but the top layer).
    fn prune(&mut self, area: Rect, now: f32) -> bool {
        if self.steps.last().is_none_or(|s| s.k(now) >= 1.0) {
            return false;
        }
        let waves = self.waves(&self.cams(area, now), now);
        if let Some(i) = waves.iter().rposition(|w| w.k >= 1.0 && (w.outside || w.rect.expand(0.5).contains_rect(area))) {
            self.steps.drain(..=i);
        }
        true
    }
}

fn ease(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    1.0 - (1.0 - k).powi(3)
}

/// Must match COOL in shader.wgsl: progress span over which a flipped cell cools (and its label types).
const WAVE_COOL: f32 = 0.3;

/// CPU twin of `wave_e()` in the shader (labels use a per-label jitter instead of the Bayer band).
struct Wave {
    origin: Pos2,
    maxd: f32,
    k: f32,
    rect: Rect,
    /// Everything outside `rect` is new already (zoom out). Such a wave is a ring closing in on its
    /// origin from the corners (the old view is drawn back in); the others spread out from it.
    outside: bool,
}

impl Wave {
    /// 0 at the start of the front's path, 1 where it ends (must match `wave_e()` in shader.wgsl).
    fn dist(&self, p: Pos2) -> f32 {
        let d = ((p - self.origin).length() / self.maxd.max(1.0)).clamp(0.0, 1.0);
        if self.outside { 1.0 - d } else { d }
    }

    /// < 0: the front has not reached `p` yet; >= 0: flipped `e` progress units ago.
    fn at(&self, p: Pos2, jitter: f32) -> f32 {
        if !self.rect.contains(p) {
            return if self.outside { WAVE_COOL } else { -1.0 };
        }
        // Jitter fades in with distance so the click point itself reacts on the very first frame.
        let d = self.dist(p);
        let thr = d * 0.8 + jitter * 0.2 * (d * 4.0).min(1.0);
        self.k * (1.0 + WAVE_COOL) - thr
    }

    fn gpu(&self) -> gfx::Wave {
        let r = self.rect;
        [[self.origin.x, self.origin.y, self.maxd, self.k], [r.min.x, r.min.y, r.max.x, r.max.y], [self.outside as u8 as f32, 0.0, 0.0, 0.0]]
    }
}

/// Accordion expand/collapse of `d`: one expanded folder per level. Collapsing also folds
/// everything inside it; expanding keeps only the expansions on the path down to `d`.
/// `open`: `d` is already open by default (shallow level), so there is nothing to expand.
fn toggle(t: &Tree, expanded: &mut HashSet<u32>, d: u32, open: bool) -> bool {
    if expanded.remove(&d) {
        expanded.retain(|&e| !is_ancestor(t, d, e));
        true
    } else if !open {
        expanded.retain(|&e| is_ancestor(t, e, d));
        expanded.insert(d)
    } else {
        false
    }
}

fn far_corner(o: Pos2, r: Rect) -> f32 {
    [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()].iter().map(|c| (*c - o).length()).fold(0.0, f32::max)
}

/// How much shows of a label belonging to layers `lo..=hi`, at a spot with wave values `es` (one
/// per step, see [`Wave::at`]): (typed-in part, opacity), each 0..1; nothing where another layer
/// shows. A label types in while the cells of the wave that brought it cool down behind the front,
/// and fades out over the stretch just ahead of the wave that takes it away (gone as its dots flip).
/// `fixed` (a step's shared title): the wave that brought it does not touch it (it shows even where
/// the layer under it still does), but a later one takes it away like any other.
fn shown(es: &[f32], lo: usize, hi: usize, fixed: bool) -> (f32, f32) {
    let s = es.iter().rposition(|&e| e >= 0.0).map_or(0, |i| i + 1);
    if s > hi || s < lo && !fixed {
        return (0.0, 0.0);
    }
    let typed = if s == lo && lo > 0 && !fixed { (es[lo - 1] / WAVE_COOL).clamp(0.0, 1.0) } else { 1.0 };
    let fade = if hi < es.len() { (-es[hi] / WAVE_COOL).clamp(0.0, 1.0) } else { 1.0 };
    (typed, fade)
}

/// Paint a layer stack's pixel field (`insts`, filling `area`) under its labels, each shown as far
/// as the `waves` let it.
fn paint_field(ui: &egui::Ui, area: Rect, mut insts: Vec<Inst>, labels: Vec<Label>, waves: &[Wave]) {
    let _s = perf::span("labels");
    let mut boxes = vec![];
    let painter = ui.painter().with_clip_rect(area.expand(1.0).intersect(ui.clip_rect()));
    // The pixel layer goes under the labels, but its quiet zones are only known once the labels
    // are laid out: reserve its place in the paint order now, fill it in after.
    let field = painter.add(egui::Shape::Noop);
    for l in labels {
        let jitter = ((l.pos.x * 12.9898 + l.pos.y * 78.233).sin() * 43758.547).fract().abs();
        let es: Vec<f32> = waves.iter().map(|w| w.at(l.pos, jitter)).collect();
        // Outgoing labels fade as a whole, incoming ones type in; the dark patch under a file
        // label follows the same amount, so it never outstays (or precedes) its text.
        let (f_in, fade) = shown(&es, l.lo, l.hi, l.fixed);
        if f_in * fade <= 0.0 {
            continue;
        }
        let typed = |s: &str| typed(s, f_in);
        // Static labels always fit their tile, so they share the treemap's clip and egui draws all
        // text in one batch; only mid-transition (tiles resizing under them) are they clipped
        // per tile. No shadow: labels sit on a dark title strip or a dimmed quiet zone.
        let p = if !waves.is_empty() { painter.with_clip_rect(l.clip.intersect(painter.clip_rect())) } else { painter.clone() };
        let font = FontId::proportional(l.size);
        let white = Color32::from_white_alpha((0.92 * fade * 255.0) as u8);
        if l.sub.is_empty() {
            // Title strip: "name - size", elided at the end when too wide.
            let mut job = egui::text::LayoutJob::simple_singleline(typed(&l.text), font, white);
            job.wrap.max_width = l.clip.max.x - l.pos.x;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let g = p.layout_job(job);
            p.galley(l.pos - vec2(0.0, g.size().y * 0.5), g, white);
            continue;
        }
        // File: name wrapped to at most 2 lines, size below; if that doesn't fit, no label at all.
        let wrapped = |s: String| {
            let mut job = egui::text::LayoutJob::simple(s, font.clone(), white, l.room.x);
            job.wrap.max_rows = 2;
            job.wrap.break_anywhere = true;
            job.halign = egui::Align::Center;
            p.layout_job(job)
        };
        let (full, sub) = (wrapped(l.text.clone()), p.layout_no_wrap(l.sub.clone(), font.clone(), white));
        if full.elided || sub.size().x > l.room.x || full.size().y + sub.size().y > l.room.y {
            continue;
        }
        let bx = Rect::from_center_size(l.pos, vec2(full.size().x.max(sub.size().x), full.size().y + sub.size().y));
        if boxes.len() == gfx::NO_LABEL as usize {
            continue; // shortcut: no dark patch left for it, so no text; only ever this many fit on screen
        }
        insts[l.inst].p[1] = boxes.len() as u8;
        insts[l.inst].q[2] = gfx::unorm(f_in * fade);
        boxes.push([bx.min.x, bx.min.y, bx.max.x, bx.max.y]);
        let (name, sub) = if f_in < 1.0 { (wrapped(typed(&l.text)), p.layout_no_wrap(typed(&l.sub), font.clone(), white)) } else { (full.clone(), sub) };
        let top = l.pos.y - (full.size().y + sub.size().y) * 0.5;
        for (g, y) in [(name, top), (sub, top + full.size().y)] {
            p.galley(pos2(l.pos.x, y) - g.rect.center_top().to_vec2(), g, white);
        }
    }
    painter.set(field, gfx::shape(area, insts, boxes, 1, &waves.iter().map(Wave::gpu).collect::<Vec<_>>()));
}

/// The treemap's rect in `room`: on the dot lattice, shifted by whole dots so what is lit sits
/// centred across (its left column is gutter, dark; its right edge is not).
fn field_rect(room: Rect, pitch: f32) -> Rect {
    let area = layout::snap(room, pitch);
    let off = ((area.min.x + pitch - room.min.x) - (room.max.x - area.max.x)) * 0.5;
    area.translate(vec2(-(off / pitch).round() * pitch, 0.0))
}

/// The first `f` (0..1) of `text`, for typing in.
fn typed(text: &str, f: f32) -> String {
    let n = text.chars().count();
    text.chars().take((n as f32 * f).ceil() as usize).collect()
}

/// One dot of a deleted tile, drifting up on the lattice and dimming level by level.
struct Particle {
    pos: Pos2,
    vel: Vec2,
    life: f32,
    max: f32,
    delay: f32,
    col: [u8; 3],
    /// Level it starts at (of the pixel field's 5).
    level: f32,
}

struct Label {
    /// Layers it belongs to; `fixed`: a step's shared title, shown whatever the waves do.
    lo: usize,
    hi: usize,
    fixed: bool,
    clip: Rect,
    pos: Pos2,
    text: String,
    /// File labels: size line under the (up to 2-line) name; empty for folder titles.
    sub: String,
    /// File labels: room for the text in layout space (fit is decided there, so zooms don't pop).
    room: Vec2,
    size: f32,
    /// Index of the tile's instance (gets the label box as its quiet zone).
    inst: usize,
}

/// A drive root with its (total, free) bytes, if read.
type Drive = (String, Option<(u64, u64)>);

pub struct App {
    tree: Option<Tree>,
    scan: Option<tree::Scan>,
    scan_t0: Instant,
    view: u32,
    back: Vec<u32>,
    fwd: Vec<u32>,
    tiles: Vec<Tile>,
    layout_key: LayoutKey,
    /// Recent layouts by `layout_key`, newest first: back / forward between big folders reuses them.
    built: Vec<(LayoutKey, Vec<Tile>)>,
    expanded: HashSet<u32>,
    expand_ver: u64,
    /// Where the user clicked: origin of the next transition wave.
    click_pos: Option<Pos2>,
    /// Dir whose expand/collapse produced the pending relayout.
    toggled: Option<u32>,
    anim: Option<Anim>,
    /// Going back to the chooser (see `App::leave`); `left`: its zoom has started.
    leaving: bool,
    left: bool,
    gpu_info: String,
    hover: Option<Item>,
    menu_item: Option<Item>,
    /// Last press on the treemap: (time, pos, folder it hit); a second press there soon after
    /// zooms into that folder.
    last_press: Option<(f64, Pos2, u32)>,
    /// Dot lattice pitch and folder title height (points) for the current DPI.
    pitch: f32,
    header: f32,
    /// Hover light of the current layout.
    lit: Lit,
    /// Folders whose whole block is lit (hovered title): lift 0..1. Kept per folder, not per tile,
    /// so lighting 10k blocks costs nothing; `lit.groups` is its per-frame form.
    group_fx: Vec<(Item, f32)>,
    /// Camera of the target layout last frame (layout rect -> screen rect), for hit-testing mid-zoom.
    cam: Map,
    confirm: Option<Item>,
    particles: Vec<Particle>,
    rng: u64,
    t0: Instant,
    path_input: String,
    drives: Vec<Drive>,
    /// Their bytes as each is read (see [`App::refresh_drives`]).
    drives_rx: Option<std::sync::mpsc::Receiver<Drive>>,
    status: String,
    /// Frame timestamps of the last second, and smoothed CPU time per whole frame (ms).
    frames: std::collections::VecDeque<Instant>,
    cpu_ms: f32,
    /// Alt+V (hidden): no vsync and a repaint every frame, to see the highest frame rate.
    uncapped: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            gfx::init(rs);
        }
        setup_style(&cc.egui_ctx);
        win::setup_window(cc);
        let now = Instant::now();
        let mut app = Self {
            tree: None,
            scan: None,
            scan_t0: now,
            view: 0,
            back: vec![],
            fwd: vec![],
            tiles: vec![],
            layout_key: (u32::MAX, Rect::NOTHING, 0, 0),
            built: vec![],
            expanded: HashSet::new(),
            expand_ver: 0,
            click_pos: None,
            toggled: None,
            anim: None,
            leaving: false,
            left: false,
            gpu_info: String::new(),
            hover: None,
            menu_item: None,
            last_press: None,
            pitch: 3.0,
            header: 15.0,
            lit: Lit::default(),
            group_fx: vec![],
            cam: (Rect::from_min_size(Pos2::ZERO, Vec2::splat(1.0)), Rect::from_min_size(Pos2::ZERO, Vec2::splat(1.0))),
            confirm: None,
            particles: vec![],
            rng: 0x2545f4914f6cdd1d,
            t0: now,
            path_input: String::new(),
            drives: vec![],
            drives_rx: None,
            status: "Ready".into(),
            frames: Default::default(),
            cpu_ms: 0.0,
            uncapped: false,
        };
        app.refresh_drives(&cc.egui_ctx);
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            app.gpu_info = rs.adapter.get_info().name;
        }
        if let Some(p) = std::env::args().nth(1) {
            app.start(&p);
        }
        app
    }

    /// List the drives now, their space once read: a slow or offline network drive can take
    /// seconds to answer, which on the UI thread kept the window from showing.
    fn refresh_drives(&mut self, ctx: &egui::Context) {
        let roots = win::drives();
        self.drives = roots.iter().map(|d| (d.clone(), self.drives.iter().find(|o| o.0 == *d).and_then(|o| o.1))).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        // One thread per drive: one that never answers holds up only itself.
        for d in roots {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let space = win::disk_space(std::path::Path::new(&d));
                let _ = tx.send((d, space));
                ctx.request_repaint();
            });
        }
        self.drives_rx = Some(rx);
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 40) as f32 / (1u64 << 24) as f32
    }

    fn now(&self) -> f32 {
        self.t0.elapsed().as_secs_f32()
    }

    fn start(&mut self, path: &str) {
        let mut p = path.trim().trim_matches('"').to_string();
        if p.len() == 2 && p.ends_with(':') {
            p.push('\\');
        }
        let pb = PathBuf::from(&p);
        if !pb.is_dir() {
            self.status = format!("Not a folder: {p}");
            return;
        }
        // Every path shown, opened or recycled is built from the root: make it absolute.
        let pb = std::path::absolute(&pb).unwrap_or(pb);
        self.tree = Some(Tree::new(&pb.to_string_lossy()));
        self.reset_nav();
        self.begin_scan(pb, 0, 1);
    }

    fn begin_scan(&mut self, path: PathBuf, id: u32, next: u32) {
        self.scan = Some(tree::start(path, id, next));
        self.scan_t0 = Instant::now();
        self.status = "Scanning…".into();
    }

    fn reset_nav(&mut self) {
        self.view = 0;
        self.drop_layout(); // a new tree's versions start over: its keys could match old layouts
        self.drop_history();
        self.tiles.clear();
        self.expanded.clear();
        self.leaving = false;
        self.left = false;
    }

    /// Lay out again from scratch, without a transition from the current tiles.
    fn drop_layout(&mut self) {
        self.layout_key.0 = u32::MAX;
        self.built.clear();
        self.anim = None;
    }

    /// Forget back / forward and the hovered item: they name dirs or files that may be gone.
    fn drop_history(&mut self) {
        self.back.clear();
        self.fwd.clear();
        self.hover = None;
    }

    /// Rescan only the current view (SpaceSniffer "refresh").
    fn rescan(&mut self) {
        // An open context menu / delete dialog names its item by index: gone or reused after this.
        self.menu_item = None;
        self.confirm = None;
        let Some(t) = self.tree.as_mut() else { return };
        self.scan = None;
        if self.view == 0 {
            let root = t.dirs[0].name.to_string();
            self.start(&root);
            return;
        }
        let path = t.path(Item::Dir(self.view));
        t.clear_dir(self.view);
        let next = t.dirs.len() as u32;
        // The old tiles index files that are gone, and the history can lead into orphaned dirs.
        self.anim = None;
        self.tiles.clear();
        self.drop_history();
        self.begin_scan(path, self.view, next);
    }

    fn pump(&mut self) {
        let Some(scan) = &self.scan else { return };
        let Some(t) = self.tree.as_mut() else { return };
        let budget = Instant::now();
        let mut done = false;
        // Bounded so a burst of batches never stalls a frame.
        while budget.elapsed().as_millis() < 8 {
            match scan.rx.try_recv() {
                Ok(tree::Msg::Batch(b)) => t.apply(b),
                Ok(tree::Msg::Done) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
                Err(_) => break,
            }
        }
        if done {
            self.scan = None;
            let secs = self.scan_t0.elapsed().as_secs_f32();
            self.status = format!("Ready · scanned in {secs:.2}s");
        }
    }

    fn navigate(&mut self, d: u32) {
        if d != self.view {
            self.back.push(self.view);
            self.fwd.clear();
            self.view = d;
        }
    }

    fn go_back(&mut self) {
        if let Some(d) = self.back.pop() {
            self.fwd.push(self.view);
            self.view = d;
        }
    }

    fn go_fwd(&mut self) {
        if let Some(d) = self.fwd.pop() {
            self.back.push(self.view);
            self.view = d;
        }
    }

    fn go_up(&mut self) {
        if let Some(t) = &self.tree
            && self.view != 0
        {
            let p = t.dirs[self.view as usize].parent;
            self.navigate(p);
        }
    }

    fn do_delete(&mut self, item: Item) {
        let Some(t) = self.tree.as_mut() else { return };
        let path = t.path(item);
        if !win::recycle(&path) {
            self.status = format!("Could not delete {}", path.display());
            return;
        }
        let col = match item {
            Item::Dir(_) => FOLDER_RGB,
            Item::File(d, f) => CATS[t.dirs[d as usize].files[f as usize].cat as usize].1,
        };
        let freed = t.size(item);
        if let Item::Dir(d) = item
            && d == self.view
        {
            self.view = t.dirs[d as usize].parent;
        }
        t.remove(item);
        // File indices shift on delete: never morph across it.
        self.drop_layout();
        self.drop_history();
        if let Some(r) = self.tiles.iter().find(|x| x.item == item).map(|x| x.rect) {
            self.burst(r, col);
        }
        // Files of that dir changed index (swap_remove): never hit-test those stale tiles.
        if let Item::File(d, _) = item {
            self.tiles.retain(|x| !matches!(x.item, Item::File(e, _) if e == d));
        }
        self.status = format!("Moved to Recycle Bin: {} ({})", path.display(), fmt_size(freed));
    }

    /// Dissolve a rect into its dots, rising and dimming, right edge first.
    fn burst(&mut self, r: Rect, col: [u8; 3]) {
        // Whole dots of the lattice, thinned out on big tiles to stay within MAX_PARTICLES.
        let p = self.pitch;
        let step = ((r.area() / (p * p) / 12000.0).sqrt().ceil().max(1.0)) * p;
        let mut y = r.min.y;
        while y < r.max.y {
            let mut x = r.min.x;
            while x < r.max.x {
                if self.rand() < 0.7 && self.particles.len() < MAX_PARTICLES {
                    let (a, b, c, d) = (self.rand(), self.rand(), self.rand(), self.rand());
                    self.particles.push(Particle {
                        pos: pos2(x, y),
                        vel: vec2((a - 0.5) * 50.0, -20.0 - b * 90.0),
                        life: 0.6 + c * 1.2,
                        max: 0.6 + c * 1.2,
                        delay: (r.max.x - x) / r.width().max(1.0) * 0.5,
                        col,
                        level: if d < 0.5 { 4.0 } else { 3.0 },
                    });
                }
                x += step;
            }
            y += step;
        }
    }

    fn step_particles(&mut self, dt: f32) -> Vec<Inst> {
        let mut out = Vec::with_capacity(self.particles.len());
        self.particles.retain_mut(|p| {
            if p.delay > 0.0 {
                p.delay -= dt;
                return true;
            }
            p.life -= dt;
            p.vel.y -= 60.0 * dt;
            p.vel.x *= 1.0 - dt;
            p.pos += p.vel * dt;
            p.life > 0.0
        });
        let pitch = self.pitch;
        for p in self.particles.iter().filter(|p| p.delay <= 0.0) {
            // The dot cell it is over, at a level stepping down as it dies.
            let level = (p.level * (p.life / p.max).clamp(0.0, 1.0)).ceil();
            let c = (p.pos.to_vec2() / pitch).floor() * pitch;
            out.push(Inst {
                rect: [c.x, c.y, c.x + pitch, c.y + pitch],
                color: gfx::rgba(p.col, level / 4.0),
                p: [gfx::PARTICLE, gfx::NO_LABEL, 0, 0],
                q: [0; 4],
            });
        }
        out
    }

    // ---------------------------------------------------------------- UI pieces

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let has_tree = self.tree.is_some() && !self.leaving;
        // Shortcuts go through their button, so they share its enabled state.
        let typing = ui.ctx().egui_wants_keyboard_input();
        let (mouse4, mouse5, bksp, f5) = ui.input(|i| {
            let p = &i.pointer;
            (p.button_pressed(PointerButton::Extra1), p.button_pressed(PointerButton::Extra2), i.key_pressed(Key::Backspace) && !typing, i.key_pressed(Key::F5))
        });
        let mut nav = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if tool(ui, "\u{E838}", "Choose your space", !self.leaving, false) {
                self.scan = None;
                self.status = "Ready".into();
                self.refresh_drives(ui.ctx());
                // Zoom back out to it (the tree goes once that is done); nothing else acts meanwhile.
                self.leaving = self.tree.is_some();
                self.drop_history();
                self.menu_item = None;
                self.confirm = None;
                self.particles.clear();
            }
            ui.separator();
            if tool(ui, "\u{E72B}", "Back (Mouse 4)", !self.back.is_empty(), mouse4) {
                self.go_back();
            }
            if tool(ui, "\u{E72A}", "Forward (Mouse 5)", !self.fwd.is_empty(), mouse5) {
                self.go_fwd();
            }
            if tool(ui, "\u{E74A}", "Up one level (Backspace)", has_tree && self.view != 0, bksp) {
                self.go_up();
            }
            if tool(ui, "\u{E80F}", "Root", has_tree && self.view != 0, false) {
                nav = Some(0);
            }
            ui.separator();
            // Not while scanning: rescanning one folder cancels the scan of everything else.
            if tool(ui, "\u{E72C}", "Rescan current view (F5)", has_tree && self.scan.is_none(), f5) {
                self.rescan();
            }
            if tool(ui, "\u{E71A}", "Stop scanning", self.scan.is_some(), false) {
                self.scan = None;
                self.status = "Stopped".into();
            }
            if let Some(t) = &self.tree {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Breadcrumb, right aligned: the current folder (rightmost) first, then up as far as
                    // there is room ("…" for the rest); the current one itself is cut short if need be.
                    let font = egui::TextStyle::Button.resolve(ui.style());
                    let mut d = self.view;
                    loop {
                        let name = &*t.dirs[d as usize].name;
                        let need = ui.painter().layout_no_wrap(name.into(), font.clone(), TEXT_DIM).size().x + 2.0 * ui.spacing().button_padding.x;
                        if d != self.view && need > ui.available_width() {
                            ui.label(RichText::new("…").color(TEXT_DIM));
                            break;
                        }
                        let txt = RichText::new(name).color(if d == self.view { Color32::WHITE } else { TEXT_DIM });
                        if ui.add(egui::Button::new(txt).frame(false).truncate()).clicked() {
                            nav = Some(d);
                        }
                        if d == 0 {
                            break;
                        }
                        ui.label(RichText::new("›").color(TEXT_DIM));
                        d = t.dirs[d as usize].parent;
                    }
                });
            }
        });
        if let Some(d) = nav {
            self.navigate(d);
        }
    }

    /// `ghost`: drawn over whatever else `ui` holds, at this opacity, during a zoom out of (into)
    /// it (see [`Chooser`]): its card only, no drives, no input.
    fn chooser(&mut self, ui: &mut egui::Ui, ghost: Option<f32>) -> Chooser {
        // Where the treemap of a drive picked here will be (as `treemap` lays it out).
        let area = field_rect(ui.available_rect_before_wrap().shrink(1.0), layout::grid(ui.ctx().pixels_per_point()));
        let mut child;
        let ui = match ghost {
            Some(a) => {
                child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()));
                child.multiply_opacity(a);
                &mut child
            }
            None => ui,
        };
        while let Some((d, space)) = self.drives_rx.as_ref().and_then(|r| r.try_recv().ok()) {
            if let Some(e) = self.drives.iter_mut().find(|e| e.0 == d) {
                e.1 = space;
            }
        }
        let mut go = None;
        let (mut insts, mut titles, mut blocks, mut picked, mut start_at) = (vec![], vec![], vec![], None, Pos2::ZERO);
        let pitch = layout::grid(ui.ctx().pixels_per_point());
        let header = layout::header(pitch);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.5 - 200.0).max(10.0));
            egui::Frame::NONE.fill(CARD).corner_radius(14).inner_margin(22).show(ui, |ui| {
                ui.set_width(520.0);
                ui.label(RichText::new("Choose your space").size(24.0).strong().color(Color32::WHITE));
                ui.add_space(14.0);
                // Drives as pixel-field blocks like the treemap's folders: title strip, used space lit,
                // free space dim; a gap between drives instead of borders.
                let gap = (6.0 / pitch).round() * pitch;
                let cols = self.drives.len().clamp(1, 3) as f32;
                let size = vec2(((ui.available_width() - (cols - 1.0) * gap) / cols / pitch).floor() * pitch, header + (60.0 / pitch).round() * pitch);
                // Pixel fields go under the names: reserve their place in the paint order.
                let field = ui.painter().add(egui::Shape::Noop);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(gap, gap);
                    for (d, space) in &self.drives {
                        let (r, resp) = ui.allocate_exact_size(size, Sense::click());
                        let r = layout::snap(Rect::from_min_size(r.min, size), pitch);
                        // Keyed by drive, not widget: the faded chooser of a transition (another Ui) and
                        // the real one share it, so it never restarts from a stale hover.
                        let hov = ui.ctx().animate_bool_with_time(egui::Id::unique(("drive", d)), resp.hovered(), HOVER_SECS);
                        let block = |rect: Rect, inten: f32, open: bool| Inst {
                            rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                            color: gfx::rgba(FOLDER_RGB, inten),
                            p: [gfx::FIELD | if open { gfx::OPEN } else { 0 }, gfx::NO_LABEL, 0, 0],
                            q: [gfx::unorm(hov), gfx::unorm(0.5), 0, 0],
                        };
                        let body = Rect::from_min_max(pos2(r.min.x, r.min.y + header), r.max);
                        insts.push(block(Rect::from_min_max(r.min, pos2(r.max.x, body.min.y)), 1.0, true));
                        insts.push(block(body, 0.3, false));
                        let name = d.trim_end_matches('\\');
                        let title = match space {
                            Some((tot, free)) => {
                                let used = 1.0 - *free as f32 / (*tot).max(1) as f32;
                                let fill = layout::snap(Rect::from_min_max(body.min, pos2(body.min.x + body.width() * used, body.max.y)), pitch);
                                if fill.width() > 0.0 {
                                    insts.push(block(fill, 1.0, false));
                                }
                                format!("{name} - {} free", fmt_size(*free))
                            }
                            None => name.to_string(),
                        };
                        if ghost.is_none() {
                            let (p, y, font) = (ui.painter(), r.min.y + header * 0.5, FontId::proportional(12.0));
                            p.text(pos2(r.min.x + 4.0, y), Align2::LEFT_CENTER, &title, font, Color32::WHITE);
                        }
                        titles.push((Rect::from_min_max(r.min, pos2(r.max.x, body.min.y)), title));
                        blocks.push(r);
                        if resp.clicked() {
                            go = Some(d.clone());
                            picked = Some((r, resp.interact_pointer_pos().unwrap_or(r.center())));
                        }
                    }
                });
                if ghost.is_none() {
                    ui.painter().set(field, gfx::shape(ui.clip_rect(), insts.clone(), vec![], 0, &[]));
                }
                ui.add_space(14.0);
                // Path field + Start span exactly the row of drives above.
                let row = vec2(cols * size.x + (cols - 1.0) * gap, 28.0);
                ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    let start = ui.add(egui::Button::new("  Start  ").min_size(vec2(0.0, row.y)));
                    start_at = start.rect.center();
                    let start = start.clicked();
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.path_input)
                            .hint_text("C:\\Users")
                            .desired_width(ui.available_width())
                            .align(Align2::LEFT_CENTER)
                            .min_size(vec2(0.0, row.y)),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if start || enter {
                        go = Some(self.path_input.clone());
                    }
                });
            });
        });
        let (block, click) = picked.map_or((None, start_at), |(r, c)| (Some(r), c));
        let snap = Chooser { insts, titles, blocks, block, click, cam: None, card: 1.0 };
        if let Some(p) = go.filter(|_| ghost.is_none()) {
            self.start(&p);
            if self.tree.is_some() {
                self.enter(snap.clone(), area);
            }
        }
        snap
    }

    /// The zoom out of the treemap into its drive's block on the chooser, the inverse of entering
    /// (see [`Chooser`]): the chooser is the new top layer, the treemap shrinks under it. The tree is
    /// dropped once that is done. A drive clicked meanwhile is entered at once, its zoom starting
    /// from the chooser as it is on screen: false then (the caller goes on to draw that).
    fn leave(&mut self, ui: &mut egui::Ui) -> bool {
        let now = self.now();
        let area = field_rect(ui.available_rect_before_wrap().shrink(1.0), self.pitch);
        if self.left && self.anim.as_mut().is_none_or(|a| !a.prune(area, now)) {
            // Done: the real chooser takes over this very frame (no blank frame between).
            self.reset_nav();
            self.tree = None;
            self.chooser(ui, None);
            return true;
        }
        // The chooser, laid out (for where its drives are) and drawn as its card only.
        let card = self.card(now);
        let top = self.chooser(ui, Some(card));
        // The chooser's camera now (the top layer's), or of whatever transition this leaves from.
        let from = Anim::motion(&self.anim, area, now);
        let cam = from[0];
        // Its widgets sit where it will come to rest: hit-test a click through where it is now.
        if self.left
            && let Some(click) = ui
                .input(|i| i.pointer.primary_clicked().then(|| i.pointer.interact_pos()).flatten())
                .filter(|&at| area.contains(at))
                .map(|at| apply(inverse(cam), at))
            && let Some(i) = top.blocks.iter().position(|b| b.contains(click))
        {
            let drive = self.drives[i].0.clone();
            self.start(&drive);
            if !self.leaving {
                // Started (a fresh tree): zoom into the drive from here.
                self.enter(Chooser { block: Some(top.blocks[i]), click, cam: Some(from), card, ..top }, area);
                return false;
            }
        }
        let t = self.tree.as_ref().unwrap();
        if !self.left {
            // Like a zoom out to a parent folder: the old view shrinks into the drive's block (none
            // for a scanned folder: the wave alone), stacked on whatever transition is running.
            self.left = true;
            let root = &*t.dirs[0].name;
            let block = self.drives.iter().position(|d| d.0.eq_ignore_ascii_case(root)).map(|i| top.blocks[i]);
            let old = std::mem::take(&mut self.tiles);
            let (rel, origin, rect, outside) = match block {
                Some(b) => ((area, b), b.center(), b, true),
                None => ((area, area), area.center(), area, false),
            };
            let lit = self.lit.clone();
            Anim::push(&mut self.anim, from, area, Step { old, rel, local: false, anchor: None, t0: now, origin, rect, outside, at: None, chooser: None, lit });
        }
        let a = self.anim.as_ref().unwrap();
        let cams = a.cams(area, now);
        let waves = a.waves(&cams, now);
        let (mut insts, mut labels) = (vec![], vec![]);
        self.layers(t, &a.steps, &cams, area, Some(&top), &mut insts, &mut labels);
        paint_field(ui, area, insts, labels, &waves);
        true
    }

    /// Zoom out of chooser `c` into the scan just started from it, over `area` (the treemap's).
    fn enter(&mut self, c: Chooser, area: Rect) {
        let rel = (c.block.unwrap_or(area), area);
        let (origin, from) = (apply(rel, c.click), c.cam.unwrap_or([(area, area); 2]));
        let now = self.now();
        let step =
            Step { old: vec![], rel, local: false, anchor: None, t0: now, origin, rect: area, outside: false, at: None, chooser: Some(c), lit: Lit::default() };
        Anim::push(&mut self.anim, from, area, step);
    }

    /// Opacity of the chooser's card (heading, path row) during a zoom out of it (it fades away
    /// over the start, from where it was when the drive was picked) or back to it (it fades in
    /// over the end; none until that zoom starts).
    fn card(&self, now: f32) -> f32 {
        let Some(a) = &self.anim else { return 0.0 };
        if self.leaving {
            let k = a.steps.last().filter(|_| self.left).map_or(0.0, |s| s.k(now));
            return ((k - (1.0 - CARD_FADE)) / CARD_FADE).clamp(0.0, 1.0);
        }
        match a.steps.first() {
            Some(s @ Step { chooser: Some(c), .. }) => c.card * (1.0 - s.k(now) / CARD_FADE).max(0.0),
            _ => 0.0,
        }
    }

    fn treemap(&mut self, ui: &mut egui::Ui) {
        let (area, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let pitch = layout::grid(ui.ctx().pixels_per_point());
        if pitch != self.pitch {
            // DPI changed (e.g. moved to another monitor): the lattice moved, relayout from scratch.
            self.pitch = pitch;
            self.header = layout::header(pitch);
            // Not before a first layout (the default pitch on a scan's first frame): nothing to
            // drop, and its transition is the zoom out of the chooser.
            if self.layout_key.0 != u32::MAX {
                self.drop_layout();
            }
        }
        let area = field_rect(area.shrink(1.0), pitch);
        let now = self.now();
        if self.anim.as_mut().is_some_and(|a| !a.prune(area, now)) {
            self.anim = None;
        }

        // Clicks are handled before the relayout below so the transition starts this very frame.
        // Hit-testing uses the target layout, seen through its current camera, so hover and clicks
        // track what is on screen mid-transition.
        let hit = resp.hover_pos().and_then(|p| {
            let p = apply(inverse(self.cam), p);
            self.tiles.iter().rev().find(|x| x.rect.contains(p)).map(|x| (x, p))
        });
        let hover = hit.map(|(x, _)| x.item);
        let hit_open = hit.is_some_and(|(x, _)| x.open);
        let on_title = hit.is_some_and(|(x, p)| x.open && p.y < x.rect.min.y + self.header);
        // A press on any folder expands/collapses it at once (mouse *down*); a second press at the
        // same spot within the double-click time zooms into the folder the first press hit (for a
        // file: its folder), stacking on the expand animation.
        if let Some(pos) = resp.hover_pos().filter(|_| ui.input(|i| i.pointer.primary_pressed())) {
            let time = ui.input(|i| i.time);
            match self.last_press.take() {
                Some((t0, p0, d)) if time - t0 < 0.3 && (pos - p0).length() < 6.0 => {
                    if d != self.view {
                        self.click_pos = Some(pos);
                        self.navigate(d);
                    }
                }
                _ => {
                    if let Some(Item::Dir(d)) = hover
                        && d != self.view
                        && !on_title // the title strip only zooms (double-click), never collapses
                        && toggle(self.tree.as_ref().unwrap(), &mut self.expanded, d, hit_open)
                    {
                        self.expand_ver += 1;
                        self.toggled = Some(d);
                        self.click_pos = Some(pos);
                    }
                    if let Some(Item::Dir(d) | Item::File(d, _)) = hover {
                        self.last_press = Some((time, pos, d));
                    }
                }
            }
        }
        {
            let t = self.tree.as_ref().unwrap();
            let key = (self.view, area, t.version, self.expand_ver);
            let prev = self.layout_key;
            let structural = key.0 != prev.0 || key.1 != prev.1 || key.3 != prev.3;
            // Relayout costs ~0.2 ms: follow the scan every frame so new tiles grow smoothly.
            if structural || key.2 != prev.2 {
                let lit = self.lit.clone();
                if key.0 != prev.0 {
                    // New view: drop hover afterglow. A lit folder's rect is looked up in the current
                    // layout, so the folder just entered (now the whole view) would light everything.
                    self.lit.tiles.clear();
                    self.group_fx.clear();
                }
                let old = std::mem::take(&mut self.tiles);
                {
                    let _s = perf::span("build");
                    match self.built.iter().position(|b| b.0 == key) {
                        Some(i) => self.tiles = self.built[i].1.clone(),
                        None => {
                            layout::build(t, self.view, area, &self.expanded, self.pitch, &old, &mut self.tiles);
                            // Not while scanning: every frame is a new version then.
                            if self.scan.is_none() {
                                self.built.insert(0, (key, self.tiles.clone()));
                                self.built.truncate(4);
                            }
                        }
                    }
                }
                let _s = perf::span("anim-setup");
                let animate = prev.0 != u32::MAX && key.1 == prev.1 && (key.0 != prev.0 || key.3 != prev.3);
                if animate {
                    let (from, to) = (prev.0, self.view);
                    // The old layout's camera right now (mid-transition it is still moving), and the
                    // click in that layout's coords.
                    let motion = Anim::motion(&self.anim, area, now);
                    let cam = motion[0];
                    let click = self.click_pos.take().map(|p| apply(inverse(cam), p));
                    let toggled = self.toggled.take();
                    let whole = (area, area);
                    // (old -> new layout, wave origin and region in the new layout, outside is new, shared element)
                    let (rel, origin, rect, outside, anchor) = if from == to {
                        let d = toggled.unwrap_or(to);
                        let at = find_rect(t, &self.tiles, d).unwrap_or(area).center();
                        (whole, click.unwrap_or(at), area, false, Some(Item::Dir(d)))
                    } else if is_ancestor(t, from, to)
                        && let Some(small) = place(t, &old, from, area, &self.expanded, self.pitch, to)
                    {
                        // Zoom in: the child's tile becomes the view; the wave flips inside it only.
                        let rel = (small, area);
                        (rel, apply(rel, click.filter(|p| small.contains(*p)).unwrap_or(small.center())), area, false, Some(Item::Dir(to)))
                    } else if is_ancestor(t, to, from)
                        && let Some(small) = place(t, &self.tiles, to, area, &self.expanded, self.pitch, from)
                    {
                        // Zoom out: the old view shrinks to its tile; everything around it is new at once.
                        ((area, small), small.center(), small, true, Some(Item::Dir(from)))
                    } else {
                        (whole, area.center(), area, false, None)
                    };
                    let local = from == to;
                    let at = (local || self.anim.as_ref().and_then(|a| a.steps.last()).is_some_and(|s| s.local)).then(|| index(&old));
                    Anim::push(&mut self.anim, motion, area, Step { old, rel, local, anchor, t0: now, origin, rect, outside, at, chooser: None, lit });
                } else if structural && prev.0 != u32::MAX {
                    self.anim = None;
                }
                self.layout_key = key;
            }
        }
        self.hover = hover;
        // Hovering a block also lights its parent folder (whose visible part is the title strip),
        // as if that title were hovered. Not for titles themselves.
        let t = self.tree.as_ref().unwrap();
        let parent = match hover {
            _ if on_title => None,
            Some(Item::File(d, _)) => Some(Item::Dir(d)),
            Some(Item::Dir(d)) if d != 0 => Some(Item::Dir(t.dirs[d as usize].parent)),
            _ => None,
        };
        // Targets: the hovered tile and that parent go to the top level; a hovered title also lights
        // all the blocks inside its folder the same way.
        let lit = [hover, parent];
        let group = hover.filter(|_| on_title);
        // Hover brightness eases in/out over HOVER_SECS; tiles just left keep a short afterglow.
        let step = ui.input(|i| i.stable_dt).min(0.1) / HOVER_SECS;
        let tiles = &self.tiles;
        ease_hover(&mut self.lit.tiles, &mut self.group_fx, lit, group, step, |it| tiles.iter().find(|x| x.item == it).map(|x| x.rect));
        self.lit.groups = self.group_fx.iter().filter_map(|&(g, v)| Some((tiles.iter().find(|x| x.item == g)?.rect, v * v * (3.0 - 2.0 * v)))).collect();

        let t = self.tree.as_ref().unwrap();
        let mut insts = Vec::with_capacity(self.tiles.len() * 2 + 8);
        let mut labels = vec![];
        let (steps, cams, waves) = match &self.anim {
            Some(a) => {
                let cams = a.cams(area, now);
                let waves = a.waves(&cams, now);
                (&a.steps[..], cams, waves)
            }
            None => (&[][..], vec![(area, area)], vec![]),
        };
        {
            let _s = perf::span("layers");
            self.layers(t, steps, &cams, area, None, &mut insts, &mut labels);
        }
        perf::add("#tiles", self.tiles.len() as f64);
        perf::add("#insts", insts.len() as f64);
        perf::add("#labels", labels.len() as f64);
        self.cam = *cams.last().unwrap();
        paint_field(ui, area, insts, labels, &waves);

        // Own tooltip instead of egui's: shown at once and kept up while the tiles change under a
        // still pointer (egui's waits for movement after a click / relayout).
        if let (Some(item), Some(at)) = (self.hover, resp.hover_pos())
            && !egui::Popup::is_any_open(ui.ctx())
        {
            let size = t.size(item);
            let (name, extra) = match item {
                Item::Dir(d) => (&*t.dirs[d as usize].name, format!("{} files", t.dirs[d as usize].nfiles)),
                Item::File(d, f) => {
                    let x = &t.dirs[d as usize];
                    (x.file_name(f), CATS[x.files[f as usize].cat as usize].0.to_string())
                }
            };
            egui::Area::new(egui::Id::unique("tile_tip")).order(egui::Order::Tooltip).fixed_pos((at + vec2(14.0, 18.0)).round()).interactable(false).show(
                ui.ctx(),
                |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        ui.label(RichText::new(name).strong());
                        ui.label(format!("{}  ·  {}", fmt_size(size), extra));
                    });
                },
            );
        }
        if resp.secondary_clicked() {
            self.menu_item = hover;
        }
        // Zoom and delete need `&mut self`, so they are applied after the menu; the rest act at once.
        let (mut zoom, mut delete) = (None, None);
        let scanning = self.scan.is_some();
        resp.context_menu(|ui| {
            let Some(item) = self.menu_item else {
                ui.close();
                return;
            };
            let Some(t) = self.tree.as_ref() else { return };
            let path = t.path(item);
            if let Item::Dir(d) = item
                && ui.button("🔍  Zoom in").clicked()
            {
                zoom = Some(d);
            }
            if ui.button("📄  Open").clicked() {
                win::open(&path);
            }
            if ui.button("📁  Show in Explorer").clicked() {
                win::reveal(&path);
            }
            if ui.button("📋  Copy path").clicked() {
                ui.ctx().copy_text(path.display().to_string());
            }
            ui.separator();
            let can_del = !scanning && item != Item::Dir(0);
            if ui
                .add_enabled(can_del, egui::Button::new(RichText::new("🗑  Move to Recycle Bin").color(c32(8))))
                .on_disabled_hover_text("Not available while scanning")
                .clicked()
            {
                delete = Some(item);
            }
        });
        if let Some(d) = zoom {
            self.navigate(d);
        }
        if delete.is_some() {
            self.confirm = delete;
        }
    }

    /// Draw the layer stack: layer j (`steps[j].old`, the top one the current tiles) under camera
    /// `cams[j]`. A tile an expand/collapse left unchanged belongs to both layers around it: drawn
    /// once, untouched by that wave. `chooser_top`: the chooser is the top layer (leaving).
    #[allow(clippy::too_many_arguments)]
    fn layers(&self, t: &Tree, steps: &[Step], cams: &[Map], area: Rect, chooser_top: Option<&Chooser>, insts: &mut Vec<Inst>, labels: &mut Vec<Label>) {
        let n = steps.len();
        let layer = |j: usize| if j < n { &steps[j].old[..] } else { &self.tiles[..] };
        let local = |j: usize| j < n && steps[j].local;
        // Only the top layer can change (the scan goes on): index it per frame, and only if needed.
        let top = (n > 0 && local(n - 1)).then(|| index(&self.tiles));
        let at = |j: usize| if j < n { steps[j].at.as_ref() } else { top.as_ref() };
        let same = |j: usize, x: &Tile| at(j).is_some_and(|m| m.get(&x.item) == Some(&(x.rect, x.open)));
        for j in 0..=n {
            if let Some(c) = if j < n { steps[j].chooser.as_ref() } else { chooser_top } {
                let cam = cams[j];
                for i in &c.insts {
                    let r = apply_rect(cam, Rect::from_min_max(pos2(i.rect[0], i.rect[1]), pos2(i.rect[2], i.rect[3])));
                    insts.push(Inst { rect: [r.min.x, r.min.y, r.max.x, r.max.y], p: [i.p[0], i.p[1], j as u8, j as u8], ..*i });
                }
                for (strip, text) in &c.titles {
                    let r = apply_rect(cam, *strip);
                    let pos = pos2(r.min.x + 4.0, r.center().y);
                    labels.push(Label {
                        lo: j,
                        hi: j,
                        fixed: false,
                        clip: r,
                        pos,
                        text: text.clone(),
                        sub: String::new(),
                        room: Vec2::ZERO,
                        size: 12.0,
                        inst: 0,
                    });
                }
            }
            for x in layer(j) {
                if j > 0 && local(j - 1) && same(j - 1, x) {
                    continue; // drawn with the layer below
                }
                let mut hi = j;
                while local(hi) && same(hi + 1, x) {
                    hi += 1;
                }
                // A step's shared title is drawn once, by the layer it leads to, untouched by its wave.
                let label = if hi < n && steps[hi].anchor == Some(x.item) { None } else { Some(j > 0 && steps[j - 1].anchor == Some(x.item)) };
                self.draw(t, x, cams[j], area, (j, hi), if hi < n { &steps[hi].lit } else { &self.lit }, label, insts, labels);
            }
        }
    }

    /// One tile, camera-mapped, belonging to layers `lo..=hi`, lit as layer `hi` was (shown until a
    /// wave flips it); `label`: none, or whether it is fixed (see [`Label`]). Label fit is decided in
    /// layout space so zooms never make labels pop. Tolerates stale items. Only what covers a dot
    /// cell's centre inside `area` is drawn (one texel per cell, see gfx.rs): the specks of a layer
    /// zoomed away cost nothing.
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        t: &Tree,
        tile: &Tile,
        cam: Map,
        area: Rect,
        (lo, hi): (usize, usize),
        lit: &Lit,
        label: Option<bool>,
        insts: &mut Vec<Inst>,
        labels: &mut Vec<Label>,
    ) {
        let r = apply_rect(cam, tile.rect);
        let v = r.intersect(area);
        let p = self.pitch;
        // Cell centres (k + 0.5) * p within [a, b), as the rasterizer samples them.
        let any = |a: f32, b: f32| (b / p - 0.5).ceil() > (a / p - 0.5).ceil();
        if !any(v.min.x, v.max.x) || !any(v.min.y, v.max.y) {
            return;
        }
        let item = tile.item;
        let is_dir = matches!(item, Item::Dir(_));
        let col = if is_dir { FOLDER_RGB } else { CATS[tile.cat as usize].1 };
        let mut hov = lit.tiles.iter().find(|x| x.0 == item).map_or(0.0, |x| x.1 * x.1 * (3.0 - 2.0 * x.1));
        // Inside a lit folder (its rect strictly contains ours): lit the same way.
        for &(g, v) in &lit.groups {
            if g != tile.rect && g.contains_rect(tile.rect) {
                hov = hov.max(v);
            }
        }
        let flags = gfx::FIELD | if tile.solid { gfx::SOLID } else { 0 } | if tile.open { gfx::OPEN } else { 0 };
        insts.push(Inst {
            rect: [r.min.x, r.min.y, r.max.x, r.max.y],
            color: gfx::rgba(col, 1.0),
            p: [flags, gfx::NO_LABEL, lo as u8, hi as u8],
            q: [if hov > 0.0 { gfx::unorm(hov) } else { 0 }, (tile.tone as u16 * 255 / (layout::TONES - 1) as u16) as u8, 0, 0],
        });
        let inst = insts.len() - 1;
        let Some(fixed) = label else { return };
        let lr = tile.rect;
        let info = || match item {
            Item::Dir(d) => t.dirs.get(d as usize).map(|x| (x.size, &*x.name)),
            Item::File(d, f) => t.dirs.get(d as usize).and_then(|x| Some((x.files.get(f as usize)?.size, x.file_name(f)))),
        };
        if tile.open {
            let Some((size, name)) = info() else { return };
            labels.push(Label {
                lo,
                hi,
                fixed,
                clip: Rect::from_min_max(r.min, pos2(r.max.x - 2.0, (r.min.y + self.header).min(r.max.y))),
                pos: pos2(r.min.x + 4.0, r.min.y + self.header * 0.5),
                text: format!("{} - {}", name, fmt_size(size)),
                sub: String::new(),
                room: Vec2::ZERO,
                size: 11.5,
                inst,
            });
        } else if (!is_dir || tile.titled) && lr.width() > 24.0 && lr.height() > 24.0 {
            let Some((size, name)) = info() else { return };
            // Files and closed folders: name (up to 2 lines) and size inside.
            labels.push(Label {
                lo,
                hi,
                fixed,
                clip: r.shrink(2.0),
                // Centred on what shows of the tile: its own gutter (top row, left column) is not.
                pos: r.center() + Vec2::splat(if tile.solid { 0.0 } else { self.pitch * 0.5 }),
                text: name.to_string(),
                sub: fmt_size(size),
                room: lr.shrink(3.0).size(),
                size: if lr.height() > 60.0 && lr.width() > 110.0 { 13.0 } else { 11.0 },
                inst,
            });
        }
    }

    fn delete_modal(&mut self, ctx: &egui::Context) {
        let Some(item) = self.confirm else { return };
        let Some(t) = self.tree.as_ref() else { return };
        let path = t.path(item);
        let size = t.size(item);
        let mut answer = None;
        egui::Modal::new(egui::Id::unique("confirm_delete")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Move to Recycle Bin?").size(18.0).strong());
            ui.add_space(6.0);
            ui.label(path.display().to_string());
            ui.label(RichText::new(fmt_size(size)).color(TEXT_DIM));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("  Move to Recycle Bin  ").color(c32(8))).clicked() {
                    answer = Some(true);
                }
                if ui.button("  Cancel  ").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    answer = Some(false);
                }
            });
        });
        if let Some(yes) = answer {
            self.confirm = None;
            if yes {
                self.do_delete(item);
            }
        }
    }
}

/// Toolbar button with a Segoe Fluent / MDL2 glyph; `key`: its shortcut was pressed (only acts when enabled).
fn tool(ui: &mut egui::Ui, icon: &str, tip: &str, enabled: bool, key: bool) -> bool {
    let icon = RichText::new(icon).size(15.0).family(egui::FontFamily::Name("icons".into()));
    let clicked = ui.add_enabled(enabled, egui::Button::new(icon).min_size(vec2(32.0, 28.0))).on_hover_text(tip).clicked();
    enabled && (clicked || key)
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.visuals.panel_fill = BG;
        s.visuals.window_fill = CARD;
        s.visuals.extreme_bg_color = Color32::from_rgb(16, 17, 21);
        s.visuals.text_edit_bg_color = Some(Color32::from_rgb(26, 27, 33));
        s.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(30, 32, 38);
        s.visuals.widgets.inactive.corner_radius = CornerRadius::same(7);
        s.visuals.widgets.hovered.corner_radius = CornerRadius::same(7);
        s.visuals.widgets.active.corner_radius = CornerRadius::same(7);
        s.visuals.window_corner_radius = CornerRadius::same(12);
        s.spacing.button_padding = vec2(8.0, 4.0);
        // Text is display only (text fields still select).
        s.interaction.selectable_labels = false;
    });
    // No bundled fonts (keeps the exe small): Segoe UI for Latin, Microsoft YaHei for CJK file
    // names, Segoe UI Symbol for menu symbols; toolbar icons from Segoe Fluent Icons (Windows 11)
    // or Segoe MDL2 Assets (Windows 10), which share code points.
    let mut fonts = egui::FontDefinitions::empty();
    let mut list = vec![];
    for file in ["segoeui.ttf", "msyh.ttc", "seguisym.ttf"] {
        if let Some(bytes) = win::map_file(&format!(r"C:\Windows\Fonts\{file}")) {
            fonts.font_data.insert(file.into(), egui::FontData::from_static(bytes).into());
            list.push(file.to_string());
        }
    }
    let mut icons = vec![];
    if let Some((file, bytes)) = ["SegoeIcons.ttf", "segmdl2.ttf"].iter().find_map(|f| Some((f, win::map_file(&format!(r"C:\Windows\Fonts\{f}"))?))) {
        fonts.font_data.insert(file.to_string(), egui::FontData::from_static(bytes).into());
        icons.push(file.to_string());
    }
    fonts.families.insert(egui::FontFamily::Name("icons".into()), icons.into_iter().chain(list.clone()).collect());
    fonts.families.insert(egui::FontFamily::Proportional, list.clone());
    fonts.families.insert(egui::FontFamily::Monospace, list);
    ctx.set_fonts(fonts);
}

impl eframe::App for App {
    /// The background is the clear colour, so the panels need no fill: under the treemap it was a
    /// whole window of pixels drawn every frame only to be covered by the pixel field.
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        // The whole previous frame on the CPU as eframe saw it (incl. tessellation and painting), less
        // the waits for the swapchain: also the status bar's figure, as `ui` alone misses the upload.
        let prev_cpu = frame.info().cpu_usage.unwrap_or(0.0) * 1e3;
        perf::add("prev-cpu", prev_cpu as f64);
        self.cpu_ms = self.cpu_ms * 0.9 + prev_cpu * 0.1;
        let frame_t0 = Instant::now();
        self.frames.push_back(frame_t0);
        while self.frames.front().is_some_and(|t| frame_t0.duration_since(*t).as_secs_f32() > 1.0) {
            self.frames.pop_front();
        }

        {
            let _s = perf::span("pump");
            self.pump();
        }
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::ALT, Key::V)) {
            self.uncapped = !self.uncapped;
            if let Some(mut c) = frame.wgpu_surface_config() {
                // Immediate where the driver has it (all the big Windows Vulkan ones), else whatever skips vsync.
                c.present_mode = if self.uncapped { wgpu::PresentMode::AutoNoVsync } else { wgpu::PresentMode::Fifo };
                frame.set_wgpu_surface_config(c);
            }
        }

        egui::Panel::top("toolbar").frame(egui::Frame::NONE.inner_margin(Margin::symmetric(10, 6))).show(ui, |ui| {
            let _s = perf::span("toolbar");
            self.toolbar(ui)
        });

        egui::Panel::bottom("status").frame(egui::Frame::NONE.inner_margin(Margin::symmetric(12, 4))).show(ui, |ui| {
            ui.horizontal(|ui| {
                let dot = if self.scan.is_some() { c32(5) } else { c32(1) };
                ui.label(RichText::new("●").color(dot));
                ui.label(RichText::new(&self.status).color(TEXT_DIM));
                if let Some(t) = &self.tree {
                    let v = &t.dirs[self.view as usize];
                    ui.label(RichText::new(format!("{} · {} files", fmt_size(v.size), v.nfiles)).color(TEXT_DIM));
                }
                let mode = if self.uncapped { " · Uncapped" } else { "" };
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} · {} FPS · {:.1} ms{mode}", self.gpu_info, self.frames.len(), self.cpu_ms))
                            .color(TEXT_DIM.gamma_multiply(0.6)),
                    );
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE.inner_margin(Margin { left: 10, right: 10, top: 2, bottom: 4 })).show(ui, |ui| {
            if self.tree.is_none() {
                self.chooser(ui, None);
                return;
            }
            if self.leaving && self.leave(ui) {
                return;
            }
            // Just left the chooser: its card fades over the start of the zoom out of it.
            let card = self.card(self.now());
            if card > 0.0 {
                self.chooser(ui, Some(card));
            }
            {
                let _s = perf::span("treemap");
                self.treemap(ui);
            }
            let parts = self.step_particles(dt);
            gfx::paint(ui.painter(), ui.clip_rect(), parts, 2, &[]);
        });

        self.delete_modal(&ctx);

        perf::frame(frame_t0.elapsed().as_secs_f64() * 1e3);
        // The pixel field is static: keep drawing only while something moves (input repaints anyway).
        let easing = self.lit.tiles.iter().chain(&self.group_fx).any(|x| x.1 < 1.0);
        let busy = self.uncapped || self.scan.is_some() || self.anim.is_some() || !self.particles.is_empty() || easing;
        if busy {
            if minimized {
                ctx.request_repaint_after(std::time::Duration::from_millis(250)); // keep the scan flowing
            } else {
                ctx.request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transition wave starts at 0, finishes at 1, sweeps outward from its origin, leaves
    /// the outside of its rect alone, and drives the typewriter labels.
    #[test]
    fn wave_and_typing() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let o = pos2(0.0, 0.0);
        let w = |k| Wave { origin: o, maxd: far_corner(o, r), k, rect: r, outside: false };
        assert!(w(0.0).at(pos2(50.0, 50.0), 0.5) < 0.0);
        // Everything has flipped and fully cooled by the end, even the far corner.
        assert!(w(1.0).at(pos2(99.0, 99.0), 1.0) >= WAVE_COOL);
        assert!(w(0.0).at(pos2(0.0, 0.0), 0.0) >= 0.0, "the click point flips on the first frame");
        assert!(w(0.4).at(pos2(5.0, 5.0), 0.0) > w(0.4).at(pos2(90.0, 90.0), 0.0));
        assert!(w(0.9).at(pos2(150.0, 0.0), 0.0) < 0.0);
        // A zoom out's front starts at the corners and ends at its origin.
        let c = r.center();
        let g = Wave { origin: c, maxd: far_corner(c, r), k: 0.0, rect: r, outside: true };
        assert!(g.dist(pos2(0.0, 0.0)) < 0.01 && g.dist(c) > 0.99);
        // Incoming (layer 1 of 2) types in as its cells cool.
        assert_eq!(typed("abcd", shown(&[0.0], 1, 1, false).0), "");
        assert_eq!(typed("abcd", shown(&[WAVE_COOL * 0.5], 1, 1, false).0), "ab");
        assert_eq!(typed("abcd", shown(&[WAVE_COOL], 1, 1, false).0), "abcd");
        // Outgoing: whole until the fade starts, half gone halfway, gone as the front arrives.
        assert_eq!(shown(&[-1.0], 0, 0, false), (1.0, 1.0));
        assert_eq!(shown(&[-WAVE_COOL * 0.5], 0, 0, false), (1.0, 0.5));
        assert_eq!(shown(&[0.0], 0, 0, false).1, 0.0);
        // Shared by both layers (unchanged by an expand): untouched. No transition: full.
        assert_eq!(shown(&[0.1], 0, 1, false), (1.0, 1.0));
        assert_eq!(shown(&[], 0, 0, false), (1.0, 1.0));
        // Stacked: where the newer wave has passed, only the top layer shows, even if the older
        // one has not got there yet; where only the older one has, the middle layer does.
        assert_eq!(shown(&[-0.5, WAVE_COOL], 2, 2, false), (1.0, 1.0));
        assert_eq!(shown(&[-0.5, WAVE_COOL], 0, 0, false).1, 0.0);
        assert_eq!(shown(&[WAVE_COOL, -1.0], 1, 1, false), (1.0, 1.0));
        assert_eq!(shown(&[WAVE_COOL, -1.0], 2, 2, false).1, 0.0);
        // A shared title: whole before and through the wave that brought it, but the next wave
        // (a second quick click) still erases it.
        assert_eq!(shown(&[-1.0, -1.0], 1, 1, true), (1.0, 1.0));
        assert_eq!(shown(&[0.0, -1.0], 1, 1, true), (1.0, 1.0));
        assert_eq!(shown(&[WAVE_COOL, 0.0], 1, 1, true).1, 0.0);
        assert_eq!(shown(&[-1.0, 0.0], 1, 1, true).1, 0.0);
    }

    /// Zoom into a child, back out at 0.2 s, in again at 0.4 s: nothing jumps (every layer stays
    /// where it was on screen), the view keeps its velocity, comes to a stop the way every zoom
    /// ends before it turns, and ends at rest.
    #[test]
    fn interrupted_zooms() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0));
        let small = Rect::from_min_size(pos2(500.0, 100.0), vec2(200.0, 100.0)); // child's tile in the parent
        let tile = Rect::from_min_size(pos2(520.0, 120.0), vec2(30.0, 20.0)); // a tile in the parent
        let close = |a: Rect, b: Rect| (a.min - b.min).length() + (a.max - b.max).length() < 1e-2;
        let step = |rel: Map, t0: f32| Step {
            old: vec![],
            rel,
            local: false,
            anchor: None,
            t0,
            origin: Pos2::ZERO,
            rect: area,
            outside: false,
            at: None,
            chooser: None,
            lit: Lit::default(),
        };
        let (rel_in, rel_out, h) = ((small, area), (area, small), 1e-3);
        let mut anim = None;
        Anim::push(&mut anim, [(area, area); 2], area, step(rel_in, 0.0));
        assert_eq!(anim.as_ref().unwrap().coast, [0.0; 4], "from rest: no coast");
        let cams = |a: &Option<Anim>, t| a.as_ref().unwrap().cams(area, t);
        assert!(close(apply_rect(cams(&anim, 0.0)[0], tile), tile), "starts where it is");
        // Back out: the parent's tile and the child layer stay put, the parent's view (the new
        // top layout) keeps its velocity.
        let t = 0.2;
        let parent = |a: &Option<Anim>, t| params(view_after(Anim::cam_now(a, area, t), rel_out, area));
        let (p0, p1) = (parent(&anim, t - h), parent(&anim, t));
        let (tile_at, child_at) = (apply_rect(cams(&anim, t)[0], tile), apply_rect(cams(&anim, t)[1], area));
        let from = Anim::motion(&anim, area, t);
        Anim::push(&mut anim, from, area, step(rel_out, t));
        assert!(close(apply_rect(cams(&anim, t)[2], tile), tile_at) && close(apply_rect(cams(&anim, t)[1], area), child_at));
        let view = |a: &Option<Anim>, t| params(apply_rect(inverse(Anim::cam_now(a, area, t)), area));
        let (v1, c0, c1) = (view(&anim, t + h), view(&anim, t + HOVER_SECS - h), view(&anim, t + HOVER_SECS));
        for i in 0..4 {
            let (a, b) = ((p1[i] - p0[i]) / h, (v1[i] - p1[i]) / h);
            assert!((a - b).abs() <= 0.05 * a.abs().max(1.0), "param {i}: {a} -> {b}");
            assert!((c1[i] - c0[i]).abs() / h <= 0.05 * a.abs().max(1.0), "param {i} stops before turning");
        }
        // In again: the parent's tile stays put; it all ends at rest.
        let t = 0.4;
        let tile_at = apply_rect(cams(&anim, t)[2], tile);
        let from = Anim::motion(&anim, area, t);
        Anim::push(&mut anim, from, area, step(rel_in, t));
        assert!(close(apply_rect(cams(&anim, t)[2], tile), tile_at));
        assert!(close(apply_rect(Anim::cam_now(&anim, area, t + ZOOM_SECS), area), area), "ends at rest");
    }

    /// Root (1) > mid (2) > inner (3) title strips: whichever way and however fast the pointer
    /// crosses them, a block that ends up lit never dims and never stalls on the way to full.
    #[test]
    fn hover_across_nested_titles_never_dims_or_stalls() {
        let rect = |i: f32| Rect::from_min_max(pos2(i, i), pos2(100.0 - i, 100.0 - i));
        // dirs 1..=3 nested; "tiles" 11, 12, 13 sit directly inside each of them
        let rect_of = |it: Item| match it {
            Item::Dir(d @ 1..=3) => Some(rect(d as f32 * 10.0)),
            Item::Dir(d @ 11..=13) => Some(Rect::from_min_size(pos2((d - 10) as f32 * 10.0 + 1.0, (d - 10) as f32 * 10.0 + 1.0), vec2(2.0, 2.0))),
            _ => None,
        };
        // brightness of a tile, as `App::draw` combines it
        let level = |tile: u32, direct: &[(Item, f32)], groups: &[(Item, f32)]| {
            let r = rect_of(Item::Dir(tile)).unwrap();
            let own = direct.iter().find(|x| x.0 == Item::Dir(tile)).map_or(0.0, |x| x.1);
            groups.iter().filter(|g| rect_of(g.0).is_some_and(|o| o != r && o.contains_rect(r))).map(|g| g.1).fold(own, f32::max)
        };
        let step = 0.1;
        // (titles hovered in turn, frames spent on each but the last, tiles that must behave)
        let cases: [(&[u32], usize, &[u32]); 5] = [
            (&[3, 2, 1], 3, &[13, 12]), // outwards, each left a third of the way
            (&[1, 2, 3], 3, &[13]),     // inwards
            (&[3, 2, 3, 2, 1], 2, &[13]),
            (&[1, 2, 1, 2, 3], 2, &[13]),
            (&[2, 3, 1], 4, &[13, 12]),
        ];
        for (path, dwell, watch) in cases {
            let (mut direct, mut groups) = (vec![], vec![]);
            let frame =
                |g: u32, direct: &mut Vec<_>, groups: &mut Vec<_>| ease_hover(direct, groups, [Some(Item::Dir(g)), None], Some(Item::Dir(g)), step, rect_of);
            (0..20).for_each(|_| frame(path[0], &mut direct, &mut groups)); // first title fully lit
            let mut last: Vec<f32> = watch.iter().map(|&t| level(t, &direct, &groups)).collect();
            let frames = path[1..].iter().enumerate().flat_map(|(i, &g)| std::iter::repeat_n(g, if i + 2 == path.len() { 20 } else { dwell }));
            for g in frames {
                frame(g, &mut direct, &mut groups);
                for (t, last) in watch.iter().zip(&mut last) {
                    let now = level(*t, &direct, &groups);
                    // lit by the hovered folder: must rise every frame until full (tiles outside it may fade)
                    let lit = rect_of(Item::Dir(g)).unwrap().contains_rect(rect_of(Item::Dir(*t)).unwrap());
                    if lit {
                        assert!(now >= (*last + step * 0.99).min(1.0), "tile {t} on title {g} in {path:?}: {last} -> {now}");
                    }
                    *last = now;
                }
            }
            assert!(last.iter().all(|&v| v == 1.0), "{path:?} ends lit: {last:?}");
        }
    }

    #[test]
    fn accordion_keeps_one_branch() {
        // 0 ─ 1 ─ 3
        //   │   └ 4
        //   └ 2 ─ 5
        let mut t = Tree::new("R");
        t.apply(tree::Batch::of(0, [], vec![(1, "a".into()), (2, "b".into())]));
        t.apply(tree::Batch::of(1, [], vec![(3, "c".into()), (4, "d".into())]));
        t.apply(tree::Batch::of(2, [], vec![(5, "e".into())]));
        let mut ex = HashSet::new();
        assert!(toggle(&t, &mut ex, 1, false));
        assert!(toggle(&t, &mut ex, 3, false));
        assert_eq!(ex, HashSet::from([1, 3]));
        assert!(toggle(&t, &mut ex, 4, false), "sibling replaces 3");
        assert_eq!(ex, HashSet::from([1, 4]));
        assert!(toggle(&t, &mut ex, 2, false), "other branch folds 1 and 4");
        assert_eq!(ex, HashSet::from([2]));
        assert!(toggle(&t, &mut ex, 5, false));
        assert!(toggle(&t, &mut ex, 2, false), "collapse folds the inside too");
        assert!(ex.is_empty());
        assert!(!toggle(&t, &mut ex, 2, true), "default-open dir has nothing to expand");
    }
}
