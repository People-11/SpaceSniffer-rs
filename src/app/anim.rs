use super::*;

/// Transition lengths. An expand/collapse wave crosses the whole view even for a small folder:
/// slow enough to be seen there.
pub const LOCAL_SECS: f32 = 0.65;
pub const ZOOM_SECS: f32 = 0.65;
pub const HOVER_SECS: f32 = 0.15;

pub fn lerp_rect(a: Rect, b: Rect, k: f32) -> Rect {
    Rect::from_min_max(a.min + (b.min - a.min) * k, a.max + (b.max - a.max) * k)
}

// Transitions: the camera moves, tiles never rearrange. A wave from the click flips each dot cell
// from the old layout to the new one while labels erase / type in.

/// Axis-aligned map taking rect `.0` onto rect `.1` (scale and offset per axis).
pub type Map = (Rect, Rect);

pub fn apply(m: Map, p: Pos2) -> Pos2 {
    m.1.min + (p - m.0.min) * (m.1.size() / m.0.size())
}

pub fn apply_rect(m: Map, r: Rect) -> Rect {
    Rect::from_min_max(apply(m, r.min), apply(m, r.max))
}

pub fn inverse(m: Map) -> Map {
    (m.1, m.0)
}

/// `g`, then `f`.
pub fn then(g: Map, f: Map) -> Map {
    (g.0, apply_rect(f, g.1))
}

/// Camera of the top layout at eased progress `e`, from view `v0` to rest. Zooming out is the inverse
/// motion of zooming in, so both keep a log-scale pace (a plain lerp front-loads the zoom).
pub fn top_cam(v0: Rect, area: Rect, e: f32) -> Map {
    if v0.area() > area.area() { (lerp_rect(v0, area, e), area) } else { (v0, lerp_rect(area, v0, e)) }
}

/// Where a new top layout starts on screen (`rel`: the layout under it -> the new one): nothing jumps.
pub fn view_after(cam: Map, rel: Map, area: Rect) -> Rect {
    apply_rect(inverse(then(inverse(rel), cam)), area)
}

/// One transition: brings in the layout above `old` through its own wave.
pub struct Step {
    /// The layout it replaces.
    pub old: Vec<Tile>,
    /// Old layout coords -> new layout coords (zooms: the child's tile <-> the whole view).
    pub rel: Map,
    /// Expand/collapse in place: tiles it leaves unchanged are shared by both layers.
    pub local: bool,
    /// Shared element: its title never erases, it rides the camera.
    pub anchor: Option<Item>,
    pub t0: f32,
    /// Wave origin and region (new layout coords); `outside`: all outside the region is new already.
    pub origin: Pos2,
    pub rect: Rect,
    pub outside: bool,
    /// `old` by item, built once, for expand/collapse steps to find unchanged tiles.
    pub at: Option<At>,
    /// The drive chooser, when that is what this step leaves (`old` is empty then).
    pub chooser: Option<Chooser>,
    /// `old`'s hover light when the step began, kept until the wave flips it.
    pub lit: Lit,
}

impl Step {
    pub fn secs(&self) -> f32 {
        if self.local { LOCAL_SECS } else { ZOOM_SECS }
    }

    pub fn k(&self, now: f32) -> f32 {
        (now - self.t0) / self.secs()
    }
}

/// A view (layout rect on screen) as centre and log size: zooms move it at an even pace.
pub fn params(v: Rect) -> [f32; 4] {
    [v.center().x, v.center().y, v.width().ln(), v.height().ln()]
}

pub fn from_params(p: [f32; 4]) -> Rect {
    Rect::from_center_size(pos2(p[0], p[1]), vec2(p[2].exp(), p[3].exp()))
}

/// Running transitions stack, never reset: layer j is `steps[j].old`, the top one the current tiles;
/// each dot cell shows the layer of the newest wave that has passed it.
pub struct Anim {
    pub steps: Vec<Step>,
    /// Start of the newest step's camera path (see [`top_cam`]).
    pub v0: Rect,
    /// What an interrupted camera still coasts (in [`params`]) before its new path; zero from rest.
    pub coast: [f32; 4],
}

/// Time step over which a camera's velocity is measured.
pub const DT: f32 = 1.0 / 240.0;

impl Anim {
    /// Stack `step` on the running transition, taking over from camera `from` (now and DT before).
    pub fn push(anim: &mut Option<Anim>, from: [Map; 2], area: Rect, step: Step) {
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
    pub fn cam_now(anim: &Option<Anim>, area: Rect, now: f32) -> Map {
        anim.as_ref().map_or((area, area), |a| *a.cams(area, now).last().unwrap())
    }

    /// The top layout's camera now and DT before: where a new step starts, and how fast.
    pub fn motion(anim: &Option<Anim>, area: Rect, now: f32) -> [Map; 2] {
        [Anim::cam_now(anim, area, now), Anim::cam_now(anim, area, now - DT)]
    }

    /// Camera (layout -> screen) of every layer, bottom first.
    pub fn cams(&self, area: Rect, now: f32) -> Vec<Map> {
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
    pub fn waves(&self, cams: &[Map], now: f32) -> Vec<Wave> {
        let w = |(s, &c): (&Step, &Map)| {
            let (origin, rect) = (apply(c, s.origin), apply_rect(c, s.rect));
            Wave { origin, maxd: far_corner(origin, rect), k: s.k(now).clamp(0.0, 1.0), rect, outside: s.outside }
        };
        self.steps.iter().zip(&cams[1..]).map(w).collect()
    }

    /// Drop layers under a finished full-screen wave; false once the newest step is done.
    pub fn prune(&mut self, area: Rect, now: f32) -> bool {
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

pub fn ease(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    1.0 - (1.0 - k).powi(3)
}

// Wave band widths, along the path where its far corner is 1: see the twins in shader.wgsl. The dots'
// glow ahead of the front (AHEAD there) does not concern labels.
/// Glow behind the front; also how far ahead of it a label fades out.
pub const WAVE_COOL: f32 = 0.15;
/// The front's ordered-dither band.
pub const WAVE_DITHER: f32 = 0.04;
/// Labels type in over this much travel behind the front, which goes on until the last one has.
pub const WAVE_TYPE: f32 = 0.3;

/// A transition wave this frame, in screen coords; CPU twin of `wave_e()` in shader.wgsl (labels use
/// a per-label jitter for the Bayer threshold).
pub struct Wave {
    pub origin: Pos2,
    /// Distance to the far corner of `rect`: the path's end.
    pub maxd: f32,
    /// Progress 0..1.
    pub k: f32,
    /// The region it flips.
    pub rect: Rect,
    /// Zoom out: all outside `rect` is new already, and the ring closes in on `origin`.
    pub outside: bool,
}

impl Wave {
    /// Where `p` lies along the front's path, 0..1.
    pub fn dist(&self, p: Pos2) -> f32 {
        let d = ((p - self.origin).length() / self.maxd.max(1.0)).clamp(0.0, 1.0);
        if self.outside { 1.0 - d } else { d }
    }

    /// How far the front is past `p` (< 0: not there yet), for `jitter` (0..1) as its dither threshold.
    pub fn at(&self, p: Pos2, jitter: f32) -> f32 {
        if !self.rect.contains(p) {
            return if self.outside { 1.0 } else { -1.0 };
        }
        let d = self.dist(p);
        self.k * (1.0 + WAVE_DITHER + WAVE_TYPE) - d - WAVE_DITHER * jitter * (d / WAVE_DITHER).min(1.0)
    }

    pub fn gpu(&self) -> gfx::Wave {
        let r = self.rect;
        [[self.origin.x, self.origin.y, self.maxd, self.k], [r.min.x, r.min.y, r.max.x, r.max.y], [self.outside as u8 as f32, 0.0, 0.0, 0.0]]
    }
}

pub fn far_corner(o: Pos2, r: Rect) -> f32 {
    [r.left_top(), r.right_top(), r.left_bottom(), r.right_bottom()].iter().map(|c| (*c - o).length()).fold(0.0, f32::max)
}

/// (typed-in part, opacity) of a label of layers `lo..=hi` where the waves are at `es`: it types in
/// behind the wave that brought it and fades just ahead of the one that takes it away. `fixed` (a
/// shared title) ignores the wave that brought it.
pub fn shown(es: &[f32], lo: usize, hi: usize, fixed: bool) -> (f32, f32) {
    let s = es.iter().rposition(|&e| e >= 0.0).map_or(0, |i| i + 1);
    if s > hi || s < lo && !fixed {
        return (0.0, 0.0);
    }
    let typed = if s == lo && lo > 0 && !fixed { (es[lo - 1] / WAVE_TYPE).clamp(0.0, 1.0) } else { 1.0 };
    let fade = if hi < es.len() { (-es[hi] / WAVE_COOL).clamp(0.0, 1.0) } else { 1.0 };
    (typed, fade)
}

/// The first `f` (0..1) of `text`, for typing in.
pub fn typed(text: &str, f: f32) -> String {
    let n = text.chars().count();
    text.chars().take((n as f32 * f).ceil() as usize).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wave sweeps out from its origin, leaves the outside of its rect alone, and drives the labels.
    #[test]
    fn wave_and_typing() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let o = pos2(0.0, 0.0);
        let w = |k| Wave { origin: o, maxd: far_corner(o, r), k, rect: r, outside: false };
        assert!(w(0.0).at(pos2(50.0, 50.0), 0.5) < 0.0);
        // Everything has flipped, cooled and typed in by the end, even the far corner.
        assert!(w(1.0).at(pos2(99.0, 99.0), 1.0) >= WAVE_TYPE);
        assert!(w(0.0).at(pos2(0.0, 0.0), 0.0) >= 0.0, "the click point flips on the first frame");
        assert!(w(0.4).at(pos2(5.0, 5.0), 0.0) > w(0.4).at(pos2(90.0, 90.0), 0.0));
        assert!(w(0.9).at(pos2(150.0, 0.0), 0.0) < 0.0);
        // One band shape all the way: the front moves at one speed and its dither band (past the
        // first few dots) has one width, near the origin or far.
        let delay = |x: f32, j| -w(0.0).at(pos2(x, 0.0), j);
        let (near, far) = (20.0, 90.0);
        assert!(((delay(far, 0.0) - delay(near, 0.0)) / (far - near) - 1.0 / far_corner(o, r)).abs() < 1e-6);
        assert!(((delay(near, 1.0) - delay(near, 0.0)) - (delay(far, 1.0) - delay(far, 0.0))).abs() < 1e-6);
        // A zoom out's front starts at the corners and ends at its origin.
        let c = r.center();
        let g = Wave { origin: c, maxd: far_corner(c, r), k: 0.0, rect: r, outside: true };
        assert!(g.dist(pos2(0.0, 0.0)) < 0.01 && g.dist(c) > 0.99);
        // Incoming (layer 1 of 2) types in behind the front.
        assert_eq!(typed("abcd", shown(&[0.0], 1, 1, false).0), "");
        assert_eq!(typed("abcd", shown(&[WAVE_TYPE * 0.5], 1, 1, false).0), "ab");
        assert_eq!(typed("abcd", shown(&[WAVE_TYPE], 1, 1, false).0), "abcd");
        // Outgoing: whole until the fade starts, half gone halfway, gone as the front arrives.
        assert_eq!(shown(&[-1.0], 0, 0, false), (1.0, 1.0));
        assert_eq!(shown(&[-WAVE_COOL * 0.5], 0, 0, false), (1.0, 0.5));
        assert_eq!(shown(&[0.0], 0, 0, false).1, 0.0);
        // Shared by both layers (unchanged by an expand): untouched. No transition: full.
        assert_eq!(shown(&[0.1], 0, 1, false), (1.0, 1.0));
        assert_eq!(shown(&[], 0, 0, false), (1.0, 1.0));
        // Stacked: each spot shows the layer of the newest wave that has passed it.
        assert_eq!(shown(&[-0.5, WAVE_TYPE], 2, 2, false), (1.0, 1.0));
        assert_eq!(shown(&[-0.5, WAVE_TYPE], 0, 0, false).1, 0.0);
        assert_eq!(shown(&[WAVE_TYPE, -1.0], 1, 1, false), (1.0, 1.0));
        assert_eq!(shown(&[WAVE_TYPE, -1.0], 2, 2, false).1, 0.0);
        // A shared title survives its own wave, not the next one.
        assert_eq!(shown(&[-1.0, -1.0], 1, 1, true), (1.0, 1.0));
        assert_eq!(shown(&[0.0, -1.0], 1, 1, true), (1.0, 1.0));
        assert_eq!(shown(&[WAVE_COOL, 0.0], 1, 1, true).1, 0.0);
        assert_eq!(shown(&[-1.0, 0.0], 1, 1, true).1, 0.0);
    }

    /// Zoom in, out at 0.2 s, in at 0.4 s: no layer jumps, velocity carries over, it ends at rest.
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
        // Back out: both layers stay put, the parent's view keeps its velocity.
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
}
