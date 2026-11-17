use super::*;

/// One frame of hover easing. `direct`: (item, level 0..1) of tiles lit on their own (`lit`: the
/// hovered tile and its parent); `groups`: folders whose whole block is lit (`group`: the hovered
/// title's folder). Levels move by `step` per frame.
///
/// Moving between nested titles (or a title and a block under it) must never dim or stall what
/// stays lit, however fast and however many levels:
/// - an entry inside the folder now being lit keeps rising until that folder has caught up with
///   it (its tiles are about to be lit by that folder anyway), then fades unseen beneath it;
/// - what is lit is never dimmer than the lit folders around it, so it starts from their level.
pub fn ease_hover(
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

/// Hover light of a layout: directly lit tiles (the hovered one and its parent; a handful), eased
/// 0..1; and the rects of folders whose whole block is lit, with their eased lift.
#[derive(Clone, Default)]
pub struct Lit {
    pub tiles: Vec<(Item, f32)>,
    pub groups: Vec<(Rect, f32)>,
}

/// Paint a layer stack's pixel field (`scene` under `cams`, filling `area`) under its labels, each
/// shown as far as the `waves` let it.
#[allow(clippy::too_many_arguments)]
pub fn paint_field(ui: &egui::Ui, area: Rect, scene: &Scene, cams: &[Map], light: Light, labels: Vec<Label>, waves: &[Wave]) {
    let (mut dyns, groups) = light;
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
        let mut p = if !waves.is_empty() { painter.with_clip_rect(l.clip.intersect(painter.clip_rect())) } else { painter.clone() };
        // Fade through the painter, not the text colour: the colour is part of egui's text layout
        // cache key, so a fading colour laid out every label again each frame (700 labels: ~5 ms).
        p.multiply_opacity(fade);
        let font = FontId::proportional(l.size);
        let white = Color32::from_white_alpha(234); // 0.92
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
        dyns[l.inst][1] = boxes.len() as u8;
        dyns[l.inst][2] = gfx::unorm(f_in * fade);
        boxes.push([bx.min.x, bx.min.y, bx.max.x, bx.max.y]);
        let (name, sub) = if f_in < 1.0 { (wrapped(typed(&l.text)), p.layout_no_wrap(typed(&l.sub), font.clone(), white)) } else { (full.clone(), sub) };
        let top = l.pos.y - (full.size().y + sub.size().y) * 0.5;
        for (g, y) in [(name, top), (sub, top + full.size().y)] {
            p.galley(pos2(l.pos.x, y) - g.rect.center_top().to_vec2(), g, white);
        }
    }
    let waves = waves.iter().map(Wave::gpu).collect();
    let cams = cams.iter().map(|(a, b)| [[a.min.x, a.min.y, a.max.x, a.max.y], [b.min.x, b.min.y, b.max.x, b.max.y]]).collect();
    painter.set(field, gfx::shape(area, 1, gfx::Field { insts: scene.insts.clone(), ver: scene.ver, dyns, boxes, waves, cams, groups }));
}

pub struct Label {
    /// Layers it belongs to; `fixed`: a step's shared title, shown whatever the waves do.
    pub lo: usize,
    pub hi: usize,
    pub fixed: bool,
    pub clip: Rect,
    pub pos: Pos2,
    pub text: String,
    /// File labels: size line under the (up to 2-line) name; empty for folder titles.
    pub sub: String,
    /// File labels: room for the text in layout space (fit is decided there, so zooms don't pop).
    pub room: Vec2,
    pub size: f32,
    /// Index of the tile's instance (gets the label box as its quiet zone).
    pub inst: usize,
}

/// What a layer stack depends on: (layout, its tiles (address, count), each step (start time, its
/// old tiles' address)). The same key means the same instances.
pub type SceneKey = (LayoutKey, usize, usize, Vec<(u32, usize)>);

/// A layer stack's instances, in layout coordinates, built only when the stack changes (a new
/// layout, a step pushed or pruned, the scan going on): they stay on the GPU. Frame to frame only
/// the cameras, the hover light and the labels move, see [`App::labels`] and [`light`].
pub struct Scene {
    /// None: a one-off (see `App::leave`).
    pub key: Option<SceneKey>,
    pub insts: Arc<[Inst]>,
    /// For the GPU (see `gfx::Field::ver`): new with every rebuild, 0 for a one-off.
    pub ver: u64,
    /// Each instance's item (None: a chooser block).
    pub items: Vec<Option<Item>>,
    /// What may carry a label, in layout coordinates.
    pub cands: Vec<Cand>,
    /// Instance of an item in the layers ending at `hi`, looked up once it is first lit.
    pub lit_at: HashMap<(Item, usize), Option<usize>>,
}

/// A label of a scene: a tile's name (and size), or a chooser drive's title.
pub struct Cand {
    /// Its tile's instance (a title: none).
    pub inst: usize,
    /// In its first layer's layout.
    pub rect: Rect,
    pub lo: usize,
    pub hi: usize,
    pub fixed: bool,
    pub what: CandKind,
}

pub enum CandKind {
    Tile { item: Item, open: bool, solid: bool },
    Title(String),
}

/// Per instance hover and label state, and the folders lit as a whole.
pub type Light = (Vec<gfx::Dyn>, Vec<gfx::Group>);

/// Hover light of a scene this frame: each lit tile's level, and the folders lit as a whole. A tile
/// is lit as the layer it ends at (`hi`) was: `steps[hi].lit`, the top one `top`.
pub fn light(scene: &mut Scene, steps: &[Step], top: &Lit) -> Light {
    let (items, insts) = (&scene.items, &scene.insts);
    let mut dyns = vec![gfx::NO_DYN; insts.len()];
    let mut groups = vec![];
    for (hi, lit) in steps.iter().map(|s| &s.lit).chain([top]).enumerate() {
        for &(item, v) in &lit.tiles {
            let at = scene.lit_at.entry((item, hi)).or_insert_with(|| (0..items.len()).find(|&i| items[i] == Some(item) && insts[i].p[3] as usize == hi));
            if let Some(i) = *at {
                dyns[i][0] = gfx::unorm(v * v * (3.0 - 2.0 * v));
            }
        }
        groups.extend(lit.groups.iter().map(|&(g, v)| ([g.min.x, g.min.y, g.max.x, g.max.y], v, hi as u8)));
    }
    (dyns, groups)
}

impl App {
    /// Draw the layer stack filling `area`: the running transition's layers under the current tiles,
    /// or under `chooser` (leaving for it). Its scene is rebuilt only when the stack changes; with
    /// the chooser on top (its blocks light up on hover) it is a one-off every frame.
    pub fn draw(&mut self, ui: &egui::Ui, area: Rect, chooser: Option<&Chooser>) {
        let now = self.now();
        let (steps, cams, waves) = match &self.anim {
            Some(a) => {
                let cams = a.cams(area, now);
                let waves = a.waves(&cams, now);
                (&a.steps[..], cams, waves)
            }
            None => (&[][..], vec![(area, area)], vec![]),
        };
        let key = chooser.is_none().then(|| {
            (self.layout_key, self.tiles.as_ptr() as usize, self.tiles.len(), steps.iter().map(|s| (s.t0.to_bits(), s.old.as_ptr() as usize)).collect())
        });
        let mut scene = match self.scene.take() {
            Some(s) if key.is_some() && s.key == key => s,
            _ => {
                let _s = perf::span("scene");
                self.scene_gen += 1;
                let ver = if key.is_some() { self.scene_gen } else { 0 };
                self.scene(steps, chooser, key, ver)
            }
        };
        let labels = {
            let _s = perf::span("cands");
            self.labels(self.tree.as_ref().unwrap(), &scene.cands, &cams, area)
        };
        let light = light(&mut scene, steps, &self.lit);
        perf::add("#tiles", self.tiles.len() as f64);
        perf::add("#insts", scene.insts.len() as f64);
        perf::add("#labels", labels.len() as f64);
        self.cam = *cams.last().unwrap();
        paint_field(ui, area, &scene, &cams, light, labels, &waves);
        if scene.key.is_some() {
            self.scene = Some(scene);
        }
    }

    /// The layer stack's scene: layer j (`steps[j].old`, the top one the current tiles) in its own
    /// layout's coords, each tile tagged with the layers it belongs to. A tile an expand/collapse
    /// left unchanged belongs to both layers around it: drawn once, untouched by that wave.
    /// `chooser_top`: the chooser is the top layer (leaving).
    pub fn scene(&self, steps: &[Step], chooser_top: Option<&Chooser>, key: Option<SceneKey>, ver: u64) -> Scene {
        let n = steps.len();
        let layer = |j: usize| if j < n { &steps[j].old[..] } else { &self.tiles[..] };
        let local = |j: usize| j < n && steps[j].local;
        // Only the top layer can change (the scan goes on): index it per scene, and only if needed.
        let top = (n > 0 && local(n - 1)).then(|| index(&self.tiles));
        let at = |j: usize| if j < n { steps[j].at.as_ref() } else { top.as_ref() };
        let same = |j: usize, x: &Tile| at(j).is_some_and(|m| m.get(&x.item) == Some(&(x.rect, x.open)));
        let (mut insts, mut items, mut cands) = (Vec::with_capacity(self.tiles.len() + 8), Vec::with_capacity(self.tiles.len() + 8), vec![]);
        for j in 0..=n {
            if let Some(c) = if j < n { steps[j].chooser.as_ref() } else { chooser_top } {
                for i in &c.insts {
                    insts.push(Inst { p: [i.p[0], i.p[1], j as u8, j as u8], ..*i });
                    items.push(None);
                }
                for (strip, text) in &c.titles {
                    cands.push(Cand { inst: 0, rect: *strip, lo: j, hi: j, fixed: false, what: CandKind::Title(text.clone()) });
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
                let is_dir = matches!(x.item, Item::Dir(_));
                let col = if is_dir { FOLDER_RGB } else { CATS[x.cat as usize].1 };
                let flags = gfx::FIELD | if x.solid { gfx::SOLID } else { 0 } | if x.open { gfx::OPEN } else { 0 };
                let r = x.rect;
                insts.push(Inst {
                    rect: [r.min.x, r.min.y, r.max.x, r.max.y],
                    color: gfx::rgba(col, 1.0),
                    p: [flags, gfx::NO_LABEL, j as u8, hi as u8],
                    q: [0, (x.tone as u16 * 255 / (layout::TONES - 1) as u16) as u8, 0, 0],
                });
                items.push(Some(x.item));
                // Folder titles; files and closed folders big enough for their name (up to 2 lines)
                // and size inside.
                if let Some(fixed) = label
                    && (x.open || (!is_dir || x.titled) && r.width() > 24.0 && r.height() > 24.0)
                {
                    let what = CandKind::Tile { item: x.item, open: x.open, solid: x.solid };
                    cands.push(Cand { inst: insts.len() - 1, rect: r, lo: j, hi, fixed, what });
                }
            }
        }
        Scene { key, insts: insts.into(), ver, items, cands, lit_at: HashMap::new() }
    }

    /// This frame's labels of a scene under `cams`: those whose tile shows on any dot cell of `area`.
    pub fn labels(&self, t: &Tree, cands: &[Cand], cams: &[Map], area: Rect) -> Vec<Label> {
        let p = self.pitch;
        // Cell centres (k + 0.5) * p within [a, b), as the rasterizer samples them.
        let any = |a: f32, b: f32| (b / p - 0.5).ceil() > (a / p - 0.5).ceil();
        let mut labels = vec![];
        for c in cands {
            let (lo, hi, fixed, lr) = (c.lo, c.hi, c.fixed, c.rect);
            let r = apply_rect(cams[lo], lr);
            let (item, open, solid) = match &c.what {
                CandKind::Title(text) => {
                    let pos = pos2(r.min.x + 4.0, r.center().y);
                    labels.push(Label { lo, hi, fixed, clip: r, pos, text: text.clone(), sub: String::new(), room: Vec2::ZERO, size: 12.0, inst: 0 });
                    continue;
                }
                &CandKind::Tile { item, open, solid } => (item, open, solid),
            };
            let v = r.intersect(area);
            if !any(v.min.x, v.max.x) || !any(v.min.y, v.max.y) {
                continue;
            }
            let info = match item {
                Item::Dir(d) => t.dirs.get(d as usize).map(|x| (x.size, &*x.name)),
                Item::File(d, f) => t.dirs.get(d as usize).and_then(|x| Some((x.files.get(f as usize)?.size, x.file_name(f)))),
            };
            let Some((size, name)) = info else { continue };
            labels.push(if open {
                Label {
                    lo,
                    hi,
                    fixed,
                    clip: Rect::from_min_max(r.min, pos2(r.max.x - 2.0, (r.min.y + self.header).min(r.max.y))),
                    pos: pos2(r.min.x + 4.0, r.min.y + self.header * 0.5),
                    text: format!("{} - {}", name, fmt_size(size)),
                    sub: String::new(),
                    room: Vec2::ZERO,
                    size: 11.5,
                    inst: c.inst,
                }
            } else {
                Label {
                    lo,
                    hi,
                    fixed,
                    clip: r.shrink(2.0),
                    // Centred on what shows of the tile: its own gutter (top row, left column) is not.
                    pos: r.center() + Vec2::splat(if solid { 0.0 } else { self.pitch * 0.5 }),
                    text: name.to_string(),
                    sub: fmt_size(size),
                    room: lr.shrink(3.0).size(),
                    size: if lr.height() > 60.0 && lr.width() > 110.0 { 13.0 } else { 11.0 },
                    inst: c.inst,
                }
            });
        }
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
