use crate::gfx::{self, Inst};
use crate::layout::{self, Tile};
use crate::tree::{self, CATS, Item, Tree};
use crate::win;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Margin, PointerButton, Pos2, Rect, RichText, Sense, Vec2,
    pos2, vec2,
};
use eframe::egui_wgpu::SurfaceConfig;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

const BG: Color32 = Color32::from_rgb(11, 12, 15);
const CARD: Color32 = Color32::from_rgb(21, 22, 26);
const TEXT_DIM: Color32 = Color32::from_rgb(140, 144, 152);
const DIM: f32 = 0.22;
const MAX_PARTICLES: usize = 8000;
/// Transition lengths: in-place expand/collapse must feel instant, zooms carry more motion.
const LOCAL_SECS: f32 = 0.6;
const ZOOM_SECS: f32 = 0.65;
const HOVER_SECS: f32 = 0.15;

/// Every folder shares one color (SpaceSniffer-style sand); files are colored by type.
const FOLDER_RGB: [f32; 3] = [180.0 / 255.0, 128.0 / 255.0, 70.0 / 255.0];

fn rgb(c: u8) -> [f32; 3] {
    let [r, g, b] = CATS[c as usize].1;
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0]
}

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

fn seed(item: Item) -> f32 {
    let (a, b) = match item {
        Item::Dir(d) => (d, 0x9e37),
        Item::File(d, f) => (d, f.wrapping_mul(0x85eb) ^ 0x51ed),
    };
    let h = (a.wrapping_mul(0x27d4eb2d) ^ b.wrapping_mul(0x165667b1)).wrapping_mul(0x9e3779b1);
    (h >> 8) as f32 / (1u32 << 24) as f32
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

#[derive(Clone, Copy)]
enum Kind {
    /// Zoom into a child whose tile was `small` in the old layout.
    In { small: Rect, click: Pos2 },
    /// Zoom out; the old view sits at `small` in the new layout.
    Out { small: Rect },
    /// Expand/collapse in place (the wave crosses the whole view; unchanged tiles stay still).
    Local { click: Pos2 },
    /// Unrelated jump (back/forward across branches).
    Swap,
}

struct Anim {
    old: Vec<Tile>,
    kind: Kind,
    /// Shared element: its title never erases, it rides the camera.
    anchor: Option<Item>,
    t0: f32,
}

impl Anim {
    fn k(&self, now: f32) -> f32 {
        let secs = if matches!(self.kind, Kind::Local { .. }) { LOCAL_SECS } else { ZOOM_SECS };
        (now - self.t0) / secs
    }
}

fn ease(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    1.0 - (1.0 - k).powi(3)
}

/// Must match COOL in shader.wgsl: progress span over which a flipped cell cools (and its label types).
const WAVE_COOL: f32 = 0.3;

/// CPU twin of `wave_e()` in the shader (labels use a per-label jitter instead of clump noise).
struct Wave {
    origin: Pos2,
    maxd: f32,
    k: f32,
    rect: Rect,
    outside: f32,
}

impl Wave {
    /// < 0: the front has not reached `p` yet; >= 0: flipped `e` progress units ago.
    fn at(&self, p: Pos2, jitter: f32) -> f32 {
        if !self.rect.contains(p) {
            return if self.outside > 0.5 { WAVE_COOL } else { -1.0 };
        }
        // Jitter fades in with distance so the click point itself reacts on the very first frame.
        let d = ((p - self.origin).length() / self.maxd.max(1.0)).clamp(0.0, 1.0);
        let thr = d * 0.8 + jitter * 0.2 * (d * 4.0).min(1.0);
        self.k * (1.0 + WAVE_COOL) - thr
    }

    fn gpu(&self) -> gfx::Wave {
        let r = self.rect;
        [[self.origin.x, self.origin.y, self.maxd, self.k], [r.min.x, r.min.y, r.max.x, r.max.y], [self.outside, 0.0, 0.0, 0.0]]
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

#[derive(Clone, Copy, PartialEq)]
enum LMode {
    Full,
    TypeIn,
    Erase,
}

/// Visible prefix of a label for wave value `e` (see [`Wave::at`]): erases just ahead of the
/// front, types in while its cells cool down behind it.
fn typed(text: &str, e: f32, mode: LMode) -> String {
    let f = match mode {
        LMode::Full => return text.to_string(),
        LMode::TypeIn => e / WAVE_COOL,
        LMode::Erase => -e / 0.12,
    }
    .clamp(0.0, 1.0);
    let n = text.chars().count();
    text.chars().take((n as f32 * f).ceil() as usize).collect()
}

// ------------------------------------------------------------------ filter

/// SpaceSniffer-like filter: `*.mp4; >100mb; <2gb; name`. Name terms are OR-ed, size terms AND-ed.
#[derive(Default)]
pub struct Filter {
    min: Option<u64>,
    max: Option<u64>,
    pats: Vec<String>,
}

impl Filter {
    pub fn parse(s: &str) -> Self {
        let mut f = Filter::default();
        for t in s.split([';', ' ', ',']).map(str::trim).filter(|t| !t.is_empty()) {
            if let Some(v) = t.strip_prefix('>').and_then(parse_size) {
                f.min = Some(v);
            } else if let Some(v) = t.strip_prefix('<').and_then(parse_size) {
                f.max = Some(v);
            } else {
                f.pats.push(t.to_lowercase());
            }
        }
        f
    }

    pub fn active(&self) -> bool {
        self.min.is_some() || self.max.is_some() || !self.pats.is_empty()
    }

    pub fn matches(&self, name: &str, size: u64) -> bool {
        if self.min.is_some_and(|m| size < m) || self.max.is_some_and(|m| size > m) {
            return false;
        }
        if self.pats.is_empty() {
            return true;
        }
        let n = name.to_lowercase();
        self.pats.iter().any(|p| if p.contains(['*', '?']) { glob(p.as_bytes(), n.as_bytes()) } else { n.contains(p.as_str()) })
    }
}

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let i = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let n: f64 = s[..i].parse().ok()?;
    let mul = match s[i..].trim() {
        "" | "b" => 1u64,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return None,
    };
    Some((n * mul as f64) as u64)
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

struct Particle {
    pos: Pos2,
    vel: Vec2,
    life: f32,
    max: f32,
    delay: f32,
    col: [f32; 3],
    size: f32,
}

enum Action {
    Zoom(u32),
    Open(PathBuf),
    Reveal(PathBuf),
    Copy(String),
    AskDelete(Item),
}

struct Label {
    mode: LMode,
    clip: Rect,
    pos: Pos2,
    align: Align2,
    text: String,
    /// File labels: size line under the (up to 2-line) name; empty for folder titles.
    sub: String,
    /// File labels: room for the text in layout space (fit is decided there, so zooms don't pop).
    room: Vec2,
    size: f32,
    alpha: f32,
    /// Index of the tile's instance (gets the label box as its quiet zone).
    inst: usize,
}

pub struct App {
    tree: Option<Tree>,
    scan: Option<tree::Scan>,
    scan_t0: Instant,
    root: PathBuf,
    disk: Option<(u64, u64)>,
    view: u32,
    back: Vec<u32>,
    fwd: Vec<u32>,
    tiles: Vec<Tile>,
    layout_key: (u32, Rect, u64, u64),
    expanded: HashSet<u32>,
    expand_ver: u64,
    /// Where the user clicked: origin of the next transition wave.
    click_pos: Option<Pos2>,
    /// Dir whose expand/collapse produced the pending relayout.
    toggled: Option<u32>,
    anim: Option<Anim>,
    /// Surface supports Mailbox: switch to it on the first frame and pace with DwmFlush.
    mailbox: bool,
    mailbox_set: bool,
    gpu_info: String,
    hover: Option<Item>,
    menu_item: Option<Item>,
    /// Dot lattice pitch and folder title height (points) for the current DPI.
    pitch: f32,
    header: f32,
    /// Per-tile hover brightness 0..1 (current hover rising, previous ones fading).
    /// [lift, tone equalisation], each easing 0..1.
    hover_fx: HashMap<Item, [f32; 2]>,
    /// Camera of the target layout last frame (layout rect -> screen rect), for hit-testing mid-zoom.
    cam: (Rect, Rect),
    confirm: Option<Item>,
    filter_text: String,
    filter: Filter,
    particles: Vec<Particle>,
    rng: u64,
    t0: Instant,
    path_input: String,
    drives: Vec<(String, Option<(u64, u64)>)>,
    status: String,
    title: String,
    /// Frame timestamps of the last second, and smoothed CPU time per frame (ms).
    frames: std::collections::VecDeque<Instant>,
    cpu_ms: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            gfx::init(rs);
        }
        setup_style(&cc.egui_ctx);
        let now = Instant::now();
        let mut app = Self {
            tree: None,
            scan: None,
            scan_t0: now,
            root: PathBuf::new(),
            disk: None,
            view: 0,
            back: vec![],
            fwd: vec![],
            tiles: vec![],
            layout_key: (u32::MAX, Rect::NOTHING, 0, 0),
            expanded: HashSet::new(),
            expand_ver: 0,
            click_pos: None,
            toggled: None,
            anim: None,
            mailbox: cc.wgpu_render_state.as_ref().is_some_and(|rs| supports_mailbox(cc, rs)),
            mailbox_set: false,
            gpu_info: String::new(),
            hover: None,
            menu_item: None,
            pitch: 3.0,
            header: 15.0,
            hover_fx: HashMap::new(),
            cam: (Rect::from_min_size(Pos2::ZERO, Vec2::splat(1.0)), Rect::from_min_size(Pos2::ZERO, Vec2::splat(1.0))),
            confirm: None,
            filter_text: String::new(),
            filter: Filter::default(),
            particles: vec![],
            rng: 0x2545f4914f6cdd1d,
            t0: now,
            path_input: String::new(),
            drives: win::drives()
                .into_iter()
                .map(|d| {
                    let s = win::disk_space(Path::new(&d));
                    (d, s)
                })
                .collect(),
            status: "Ready".into(),
            title: String::new(),
            frames: Default::default(),
            cpu_ms: 0.0,
        };
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let i = rs.adapter.get_info();
            let mode = if app.mailbox { "Mailbox + DwmFlush" } else { "Fifo" };
            app.gpu_info = format!("{:?} · {} · {}", i.backend, mode, i.name);
        }
        if let Some(p) = std::env::args().nth(1) {
            app.start(&p);
        }
        app
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
        self.disk = win::disk_space(&pb);
        self.tree = Some(Tree::new(&p));
        self.root = pb.clone();
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
        self.back.clear();
        self.fwd.clear();
        self.hover = None;
        self.tiles.clear();
        self.layout_key.0 = u32::MAX;
        self.expanded.clear();
        self.anim = None;
    }

    /// Rescan only the current view (SpaceSniffer "refresh").
    fn rescan(&mut self) {
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
        self.disk = win::disk_space(&self.root);
        self.anim = None;
        self.begin_scan(path, self.view, next);
    }

    fn pump(&mut self) {
        let now = self.now();
        let Some(scan) = &self.scan else { return };
        let Some(t) = self.tree.as_mut() else { return };
        let budget = Instant::now();
        let mut done = false;
        // Bounded so a burst of batches never stalls a frame.
        while budget.elapsed().as_millis() < 8 {
            match scan.rx.try_recv() {
                Ok(tree::Msg::Batch(b)) => t.apply(b, now),
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
            self.disk = win::disk_space(&self.root);
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
            Item::File(d, f) => rgb(t.dirs[d as usize].files[f as usize].cat),
        };
        let freed = t.size(item);
        if let Item::Dir(d) = item
            && d == self.view
        {
            self.view = t.dirs[d as usize].parent;
        }
        t.remove(item);
        // File indices shift on delete: never morph across it.
        self.layout_key.0 = u32::MAX;
        self.anim = None;
        self.back.clear();
        self.fwd.clear();
        self.hover = None;
        if let Some(r) = self.tiles.iter().find(|x| x.item == item).map(|x| x.rect) {
            self.burst(r, col);
        }
        if let Some((tot, free)) = self.disk.as_mut() {
            *free = (*free + freed).min(*tot);
        }
        self.status = format!("Moved to Recycle Bin: {} ({})", path.display(), fmt_size(freed));
    }

    /// Dissolve a rect into rising particles, right edge first (like the reference "clear caches").
    fn burst(&mut self, r: Rect, col: [f32; 3]) {
        let step = (r.area() / 2500.0).sqrt().max(3.0);
        let mut y = r.min.y;
        while y < r.max.y {
            let mut x = r.min.x;
            while x < r.max.x {
                if self.rand() < 0.7 && self.particles.len() < MAX_PARTICLES {
                    let (a, b, c, d) = (self.rand(), self.rand(), self.rand(), self.rand());
                    let w = (0.3 + 0.7 * d) * 0.45;
                    self.particles.push(Particle {
                        pos: pos2(x, y),
                        vel: vec2((a - 0.5) * 50.0, -20.0 - b * 90.0),
                        life: 0.6 + c * 1.2,
                        max: 0.6 + c * 1.2,
                        delay: (r.max.x - x) / r.width().max(1.0) * 0.5,
                        col: col.map(|v| v + (1.0 - v) * w),
                        size: 1.5 + d * 2.0,
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
        for p in self.particles.iter().filter(|p| p.delay <= 0.0) {
            let k = (p.life / p.max).clamp(0.0, 1.0);
            let s = p.size * 0.5;
            out.push(Inst {
                rect: [p.pos.x - s, p.pos.y - s, p.pos.x + s, p.pos.y + s],
                color: [p.col[0], p.col[1], p.col[2], k * k * 1.4],
                p: [99.0, 0.0, gfx::PARTICLE, 3.0],
                q: [0.0; 4],
                o: gfx::STATIC,
                t: gfx::NO_TEXT,
            });
        }
        out
    }

    // ---------------------------------------------------------------- UI pieces

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let has_tree = self.tree.is_some();
        let mut nav = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if tool(ui, "📂", "Choose your space", true) {
                self.tree = None;
                self.scan = None;
                self.status = "Ready".into();
            }
            ui.separator();
            if tool(ui, "⬅", "Back (Mouse 4)", !self.back.is_empty()) {
                self.go_back();
            }
            if tool(ui, "➡", "Forward (Mouse 5)", !self.fwd.is_empty()) {
                self.go_fwd();
            }
            if tool(ui, "⬆", "Up one level (Backspace)", has_tree && self.view != 0) {
                self.go_up();
            }
            if tool(ui, "🏠", "Root", has_tree && self.view != 0) {
                nav = Some(0);
            }
            ui.separator();
            if tool(ui, "🔄", "Rescan current view (F5)", has_tree) {
                self.rescan();
            }
            if tool(ui, "⏹", "Stop scanning", self.scan.is_some()) {
                self.scan = None;
                self.status = "Stopped".into();
            }
            ui.separator();
            ui.label(RichText::new("Filter").color(TEXT_DIM));
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.filter_text)
                    .hint_text("*.mp4; >100mb; <2gb; name")
                    .desired_width(240.0),
            );
            if r.changed() {
                self.filter = Filter::parse(&self.filter_text);
            }
            if self.filter.active() && tool(ui, "✖", "Clear filter", true) {
                self.filter_text.clear();
                self.filter = Filter::default();
            }
            // Breadcrumb, right aligned, deepest last.
            if let Some(t) = &self.tree {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut d = self.view;
                    loop {
                        let txt = RichText::new(&*t.dirs[d as usize].name)
                            .color(if d == self.view { Color32::WHITE } else { TEXT_DIM });
                        if ui.add(egui::Button::new(txt).frame(false)).clicked() {
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

    fn chooser(&mut self, ui: &mut egui::Ui) {
        let mut go = None;
        let now = self.now();
        let pitch = layout::grid(ui.ctx().pixels_per_point());
        let header = layout::header(pitch);
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.5 - 200.0).max(10.0));
            egui::Frame::NONE.fill(CARD).corner_radius(14).inner_margin(22).show(ui, |ui| {
                ui.set_width(520.0);
                ui.label(RichText::new("Choose your space").size(24.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("Pick a drive or type a folder path").color(TEXT_DIM));
                ui.add_space(14.0);
                // Drives as pixel-field blocks like the treemap's folders: title strip, used space lit,
                // free space dim; a gap between drives instead of borders.
                let gap = 2.0 * pitch;
                let cols = self.drives.len().clamp(1, 3) as f32;
                let size = vec2(((ui.available_width() - (cols - 1.0) * gap) / cols / pitch).floor() * pitch, header + 20.0 * pitch);
                let mut insts = vec![];
                let mut texts = vec![];
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(gap, gap);
                    for (i, (d, space)) in self.drives.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(size, Sense::click());
                        let r = layout::snap(Rect::from_min_size(r.min, size), pitch);
                        let hov = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), HOVER_SECS);
                        let seed = (i as f32 * 0.618 + 0.1).fract();
                        let block = |rect: Rect, inten: f32, head: f32| Inst {
                            rect: [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                            color: [FOLDER_RGB[0], FOLDER_RGB[1], FOLDER_RGB[2], inten],
                            p: [99.0, seed, gfx::FIELD, 0.0],
                            q: [head, hov, 0.5, 0.3],
                            o: gfx::STATIC,
                            t: gfx::NO_TEXT,
                        };
                        let body = Rect::from_min_max(pos2(r.min.x, r.min.y + header), r.max);
                        insts.push(block(Rect::from_min_max(r.min, pos2(r.max.x, body.min.y)), 1.0, header));
                        insts.push(block(body, 0.3, 0.0));
                        let name = d.trim_end_matches('\\');
                        let (title, sub) = match space {
                            Some((tot, free)) => {
                                let used = 1.0 - *free as f32 / (*tot).max(1) as f32;
                                let fill = layout::snap(Rect::from_min_max(body.min, pos2(body.min.x + body.width() * used, body.max.y)), pitch);
                                if fill.width() > 0.0 {
                                    insts.push(block(fill, 1.0, 0.0));
                                }
                                (format!("{name} - {} free", fmt_size(*free)), format!("{:.0}% of {}", used * 100.0, fmt_size(*tot)))
                            }
                            None => (name.to_string(), String::new()),
                        };
                        texts.push((r, title, sub));
                        if resp.clicked() {
                            go = Some(d.clone());
                        }
                    }
                });
                gfx::paint(ui.painter(), ui.clip_rect(), insts, now, 0, gfx::NO_WAVE);
                let p = ui.painter();
                for (r, title, sub) in texts {
                    p.text(pos2(r.min.x + 4.0, r.min.y + header * 0.5), Align2::LEFT_CENTER, title, FontId::proportional(12.0), Color32::WHITE);
                    p.text(pos2(r.max.x - 4.0, r.min.y + header * 0.5), Align2::RIGHT_CENTER, sub, FontId::proportional(12.0), TEXT_DIM);
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.path_input).hint_text("C:\\Users").desired_width(400.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    if ui.button("  Start  ").clicked() || enter {
                        go = Some(self.path_input.clone());
                    }
                });
            });
        });
        if let Some(p) = go {
            self.start(&p);
        }
    }

    fn treemap(&mut self, ui: &mut egui::Ui) {
        let (area, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let pitch = layout::grid(ui.ctx().pixels_per_point());
        if pitch != self.pitch {
            // DPI changed (e.g. moved to another monitor): the lattice moved, relayout from scratch.
            self.pitch = pitch;
            self.header = layout::header(pitch);
            self.layout_key.0 = u32::MAX;
            self.anim = None;
        }
        let area = layout::snap(area.shrink(1.0), pitch);
        let now = self.now();
        if self.anim.as_ref().is_some_and(|a| a.k(now) >= 1.0) {
            self.anim = None;
        }

        // Clicks are handled before the relayout below so the transition starts this very frame.
        // Hit-testing uses the target layout, seen through its current camera, so hover and clicks
        // track what is on screen mid-transition.
        let (cf, ct) = self.cam;
        let hit = resp.hover_pos().and_then(|p| {
            let p = cf.min + (p - ct.min) * (cf.size() / ct.size());
            self.tiles.iter().rev().find(|x| x.rect.contains(p)).map(|x| (x, p))
        });
        let hover = hit.map(|(x, _)| x.item);
        let hit_open = hit.is_some_and(|(x, _)| x.open);
        // Double-click zooms in on a folder's title strip, or anywhere on a folder too small to have
        // one; a press anywhere else on a titled folder expands/collapses it. Only open folders
        // have a title strip (closed ones show their name inside the block).
        let on_title = hit.is_some_and(|(x, p)| x.open && p.y < x.rect.min.y + self.header);
        let untitled = hit.is_some_and(|(x, _)| matches!(x.item, Item::Dir(_)) && !x.titled);
        // Expand reacts on mouse *down* (no click/double-click ambiguity in the body).
        let pressed = resp.contains_pointer() && ui.input(|i| i.pointer.primary_pressed());
        if resp.double_clicked() && (on_title || untitled) {
            if let Some(Item::Dir(d)) = hover {
                self.click_pos = resp.interact_pointer_pos();
                self.navigate(d);
            }
        } else if pressed
            && let Some(Item::Dir(d)) = hover
            && d != self.view
            && !on_title
            && !untitled
            && toggle(self.tree.as_ref().unwrap(), &mut self.expanded, d, hit_open)
        {
            self.expand_ver += 1;
            self.toggled = Some(d);
            self.click_pos = resp.hover_pos();
        }
        {
            let t = self.tree.as_ref().unwrap();
            let key = (self.view, area, t.version, self.expand_ver);
            let prev = self.layout_key;
            let structural = key.0 != prev.0 || key.1 != prev.1 || key.3 != prev.3;
            // Relayout costs ~0.2 ms: follow the scan every frame so new tiles grow smoothly.
            if structural || key.2 != prev.2 {
                let old = std::mem::take(&mut self.tiles);
                layout::build(t, self.view, area, &self.expanded, self.pitch, &old, &mut self.tiles);
                let animate = prev.0 != u32::MAX && key.1 == prev.1 && (key.0 != prev.0 || key.3 != prev.3);
                if animate {
                    let (from, to) = (prev.0, self.view);
                    let click = self.click_pos.take();
                    let toggled = self.toggled.take();
                    let (kind, anchor) = if from == to {
                        let d = toggled.unwrap_or(to);
                        let at = find_rect(t, &self.tiles, d).unwrap_or(area).center();
                        (Kind::Local { click: click.unwrap_or(at) }, Some(Item::Dir(d)))
                    } else if is_ancestor(t, from, to)
                        && let Some(small) = find_rect(t, &old, to)
                    {
                        (Kind::In { small, click: click.filter(|p| small.contains(*p)).unwrap_or(small.center()) }, Some(Item::Dir(to)))
                    } else if is_ancestor(t, to, from)
                        && let Some(small) = find_rect(t, &self.tiles, from)
                    {
                        (Kind::Out { small }, Some(Item::Dir(from)))
                    } else {
                        (Kind::Swap, None)
                    };
                    self.anim = Some(Anim { old, kind, anchor, t0: now });
                } else if structural {
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
        // Targets: the hovered tile and that parent are lifted *and* eased to the top tone (so every
        // hovered tile reaches the same brightness); a hovered title also lifts all the blocks
        // inside its folder, keeping their tones so they stay distinguishable.
        let mut lit: HashMap<Item, [f32; 2]> = [hover, parent].into_iter().flatten().map(|i| (i, [1.0, 1.0])).collect();
        if on_title && let Some(i) = self.tiles.iter().position(|x| Some(x.item) == hover) {
            // Tiles are in depth-first order: the folder's blocks directly follow it, inside its rect.
            let r = self.tiles[i].rect;
            for x in self.tiles[i + 1..].iter().take_while(|x| r.contains_rect(x.rect)) {
                lit.entry(x.item).or_insert([1.0, 0.0]);
            }
        }
        // Hover brightness eases in/out over HOVER_SECS; tiles just left keep a short afterglow.
        let step = ui.input(|i| i.stable_dt).min(0.1) / HOVER_SECS;
        for &item in lit.keys() {
            self.hover_fx.entry(item).or_insert([0.0; 2]);
        }
        for (item, v) in &mut self.hover_fx {
            let goal = lit.get(item).copied().unwrap_or([0.0; 2]);
            for k in 0..2 {
                v[k] = if goal[k] > v[k] { (v[k] + step).min(goal[k]) } else { (v[k] - step).max(goal[k]) };
            }
        }
        self.hover_fx.retain(|_, v| v[0] > 0.0 || v[1] > 0.0);

        let t = self.tree.as_ref().unwrap();
        let mut insts = Vec::with_capacity(self.tiles.len() * 2 + 8);
        let mut labels = vec![];
        let mut wave = None;
        if let Some(a) = &self.anim {
            let k = a.k(now).clamp(0.0, 1.0);
            let e = ease(k);
            // r: where the zooming view currently sits; old/new: camera maps for each layer.
            let (r, origin, outside, old_cam, new_cam) = match a.kind {
                Kind::In { small, click } => {
                    // Camera = inverse of Out's camera at the same eased progress, so zooming in has
                    // the same (log-scale) pace as zooming out. A plain lerp of `small` would spend
                    // nearly all the perceived zoom in the first few frames.
                    let m = lerp_rect(area, small, e);
                    let inv = |p: Pos2| area.min + (p - m.min) * (area.size() / m.size());
                    let r = Rect::from_min_max(inv(small.min), inv(small.max));
                    (r, r.min + (click - small.min) * (r.size() / small.size()), 0.0, (small, r), (area, r))
                }
                Kind::Out { small } => {
                    let r = lerp_rect(area, small, e);
                    (r, r.center(), 1.0, (area, r), (small, r))
                }
                Kind::Local { click } => (area, click, 0.0, (area, area), (area, area)),
                Kind::Swap => (area, area.center(), 0.0, (area, area), (area, area)),
            };
            let w = Wave { origin, maxd: far_corner(origin, r), k, rect: r, outside };
            if let Kind::Local { .. } = a.kind {
                // Only tiles whose rect or open-state changed go through the wave; the rest are
                // drawn untouched underneath (they are the ancestors/neighbours of what changed).
                type At = HashMap<Item, (Rect, bool)>;
                let at = |tiles: &[Tile]| -> At { tiles.iter().map(|x| (x.item, (x.rect, x.open))).collect() };
                let (old_at, new_at) = (at(&a.old), at(&self.tiles));
                let kept = |x: &Tile, other: &At| other.get(&x.item) == Some(&(x.rect, x.open));
                self.layer(t, self.tiles.iter().filter(|x| kept(x, &old_at)), new_cam, 0.0, None, now, &mut insts, &mut labels);
                self.layer(t, a.old.iter().filter(|x| !kept(x, &new_at)), old_cam, gfx::OUTGOING, a.anchor, now, &mut insts, &mut labels);
                self.layer(t, self.tiles.iter().filter(|x| !kept(x, &old_at)), new_cam, gfx::INCOMING, a.anchor, now, &mut insts, &mut labels);
            } else if matches!(a.kind, Kind::Out { .. }) {
                // Zooming out the parent (new) surrounds the shrinking child (old): draw it first.
                self.layer(t, &self.tiles, new_cam, gfx::INCOMING, a.anchor, now, &mut insts, &mut labels);
                self.layer(t, &a.old, old_cam, gfx::OUTGOING, a.anchor, now, &mut insts, &mut labels);
            } else {
                self.layer(t, &a.old, old_cam, gfx::OUTGOING, a.anchor, now, &mut insts, &mut labels);
                self.layer(t, &self.tiles, new_cam, gfx::INCOMING, a.anchor, now, &mut insts, &mut labels);
            }
            wave = Some(w);
            self.cam = new_cam;
        } else {
            self.layer(t, &self.tiles, (area, area), 0.0, None, now, &mut insts, &mut labels);
            self.cam = (area, area);
        }
        let painter = ui.painter().with_clip_rect(area.expand(1.0).intersect(ui.clip_rect()));
        // The pixel layer goes under the labels, but its quiet zones are only known once the labels
        // are laid out: reserve its place in the paint order now, fill it in after.
        let field = painter.add(egui::Shape::Noop);
        for l in labels {
            let e = wave.as_ref().map(|w| w.at(l.pos, ((l.pos.x * 12.9898 + l.pos.y * 78.233).sin() * 43758.547).fract().abs()));
            let typed = |s: &str| e.map_or_else(|| s.to_string(), |e| typed(s, e, l.mode));
            let p = painter.with_clip_rect(l.clip.intersect(painter.clip_rect()));
            let font = FontId::proportional(l.size);
            let (shadow, white) = (Color32::from_black_alpha((l.alpha * 220.0) as u8), Color32::from_white_alpha((l.alpha * 255.0) as u8));
            if l.sub.is_empty() {
                let text = typed(&l.text);
                p.text(l.pos + vec2(1.0, 1.0), l.align, &text, font.clone(), shadow);
                p.text(l.pos, l.align, &text, font, white);
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
            insts[l.inst].t = [bx.min.x, bx.min.y, bx.max.x, bx.max.y];
            let (name, sub) = (wrapped(typed(&l.text)), p.layout_no_wrap(typed(&l.sub), font.clone(), white));
            let top = l.pos.y - (full.size().y + sub.size().y) * 0.5;
            for (g, y) in [(name, top), (sub, top + full.size().y)] {
                let at = pos2(l.pos.x, y) - g.rect.center_top().to_vec2();
                p.galley_with_override_text_color(at + vec2(1.0, 1.0), g.clone(), shadow);
                p.galley(at, g, white);
            }
        }
        painter.set(field, gfx::shape(area, insts, now, 1, wave.as_ref().map_or(gfx::NO_WAVE, Wave::gpu)));

        // Interaction.
        // Own tooltip instead of egui's: shown at once and kept up while the tiles change under a
        // still pointer (egui's waits for movement after a click / relayout).
        if let (Some(item), Some(at)) = (self.hover, resp.hover_pos())
            && !egui::Popup::is_any_open(ui.ctx())
        {
            let size = t.size(item);
            let (name, extra) = match item {
                Item::Dir(d) => (&t.dirs[d as usize].name, format!("{} files", t.dirs[d as usize].nfiles)),
                Item::File(d, f) => {
                    let x = &t.dirs[d as usize].files[f as usize];
                    (&x.name, CATS[x.cat as usize].0.to_string())
                }
            };
            egui::Area::new(egui::Id::unique("tile_tip"))
                .order(egui::Order::Tooltip)
                .fixed_pos(at + vec2(14.0, 18.0))
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        ui.label(RichText::new(&**name).strong());
                        ui.label(format!("{}  ·  {}", fmt_size(size), extra));
                    });
                });
        }
        if resp.secondary_clicked() {
            self.menu_item = hover;
        }
        let mut action = None;
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
                action = Some(Action::Zoom(d));
            }
            if ui.button("📄  Open").clicked() {
                action = Some(Action::Open(path.clone()));
            }
            if ui.button("📁  Show in Explorer").clicked() {
                action = Some(Action::Reveal(path.clone()));
            }
            if ui.button("📋  Copy path").clicked() {
                action = Some(Action::Copy(path.display().to_string()));
            }
            ui.separator();
            let can_del = !scanning && item != Item::Dir(0);
            if ui
                .add_enabled(can_del, egui::Button::new(RichText::new("🗑  Move to Recycle Bin").color(c32(8))))
                .on_disabled_hover_text("Not available while scanning")
                .clicked()
            {
                action = Some(Action::AskDelete(item));
            }
            if action.is_some() {
                ui.close();
            }
        });
        match action {
            Some(Action::Zoom(d)) => self.navigate(d),
            Some(Action::Open(p)) => win::open(&p),
            Some(Action::Reveal(p)) => win::reveal(&p),
            Some(Action::Copy(s)) => ui.ctx().copy_text(s),
            Some(Action::AskDelete(i)) => self.confirm = Some(i),
            None => {}
        }
    }

    /// Draw `tiles` under the camera map `cam` (layout rect -> screen rect) with a transition role.
    #[allow(clippy::too_many_arguments)]
    fn layer<'a>(&self, t: &Tree, tiles: impl IntoIterator<Item = &'a Tile>, cam: (Rect, Rect), role: f32, anchor: Option<Item>, now: f32, insts: &mut Vec<Inst>, labels: &mut Vec<Label>) {
        let (from, to) = cam;
        let s = to.size() / from.size();
        let mode = if role == gfx::INCOMING {
            LMode::TypeIn
        } else if role == gfx::OUTGOING {
            LMode::Erase
        } else {
            LMode::Full
        };
        for tile in tiles {
            let shared = anchor == Some(tile.item);
            // The shared title is drawn once, by the incoming layer, untouched by the wave.
            let mode = match (shared, mode) {
                (true, LMode::Erase) => None,
                (true, _) => Some(LMode::Full),
                (false, m) => Some(m),
            };
            self.draw(t, tile, from, to, s, role, mode, now, insts, labels);
        }
    }

    /// One tile, camera-mapped. Label visibility is decided in layout space so the camera
    /// never makes labels pop. Tolerates stale items.
    #[allow(clippy::too_many_arguments)]
    fn draw(&self, t: &Tree, tile: &Tile, from: Rect, to: Rect, s: Vec2, role: f32, mode: Option<LMode>, now: f32, insts: &mut Vec<Inst>, labels: &mut Vec<Label>) {
        let map = |p: Pos2| to.min + (p - from.min) * s;
        let r = Rect::from_min_max(map(tile.rect.min), map(tile.rect.max));
        if r.width() <= 0.0 || r.height() <= 0.0 {
            return;
        }
        let item = tile.item;
        let (is_dir, col, born, size, name) = match item {
            Item::Dir(d) => {
                let Some(x) = t.dirs.get(d as usize) else { return };
                (true, FOLDER_RGB, x.born, x.size, &*x.name)
            }
            Item::File(d, f) => {
                let Some(x) = t.dirs.get(d as usize).and_then(|x| x.files.get(f as usize)) else { return };
                (false, rgb(x.cat), x.born, x.size, &*x.name)
            }
        };
        let dim = self.filter.active() && !is_dir && !self.filter.matches(name, size);
        let age = now - born;
        let [hov, eq] = self.hover_fx.get(&item).map_or([0.0; 2], |v| v.map(|x| x * x * (3.0 - 2.0 * x)));
        let glow = if age < 0.8 && r.area() > 30.0 { 6.0 * (1.0 - age / 0.8) } else { 0.0 };
        insts.push(Inst {
            rect: [r.min.x, r.min.y, r.max.x, r.max.y],
            color: [col[0], col[1], col[2], if dim { DIM } else { 1.0 }],
            p: [age, seed(item), gfx::FIELD, glow],
            q: [if tile.open { self.header } else { 0.0 }, hov, tile.tone as f32 / (layout::TONES - 1) as f32, 0.3],
            o: [role, eq, 0.0, 0.0],
            t: gfx::NO_TEXT,
        });
        let inst = insts.len() - 1;
        let Some(mode) = mode else { return };
        let alpha = if dim { 0.35 } else { 0.92 };
        let lr = tile.rect;
        if tile.open {
            labels.push(Label {
                mode,
                clip: Rect::from_min_max(r.min, pos2(r.max.x - 2.0, (r.min.y + self.header).min(r.max.y))),
                pos: pos2(r.min.x + 4.0, r.min.y + self.header * 0.5),
                align: Align2::LEFT_CENTER,
                text: format!("{} - {}", name, fmt_size(size)),
                sub: String::new(),
                room: Vec2::ZERO,
                size: 11.5,
                alpha,
                inst,
            });
        } else if (!is_dir || tile.titled) && lr.width() > 24.0 && lr.height() > 24.0 {
            // Files and closed folders: name (up to 2 lines) and size inside.
            labels.push(Label {
                mode,
                clip: r.shrink(2.0),
                pos: r.center(),
                align: Align2::CENTER_CENTER,
                text: name.to_string(),
                sub: fmt_size(size),
                room: lr.shrink(3.0).size(),
                size: if lr.height() > 60.0 && lr.width() > 110.0 { 13.0 } else { 11.0 },
                alpha,
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

/// Probe the window's surface for Mailbox support on the adapter eframe picked.
fn supports_mailbox(cc: &eframe::CreationContext, rs: &eframe::egui_wgpu::RenderState) -> bool {
    // SAFETY: the window outlives this temporary surface, which is dropped before returning.
    let Ok(target) = (unsafe { wgpu::SurfaceTargetUnsafe::from_window(cc) }) else { return false };
    let Ok(surface) = (unsafe { rs.instance.create_surface_unsafe(target) }) else { return false };
    surface.get_capabilities(&rs.adapter).present_modes.contains(&wgpu::PresentMode::Mailbox)
}

fn tool(ui: &mut egui::Ui, icon: &str, tip: &str, enabled: bool) -> bool {
    ui.add_enabled(enabled, egui::Button::new(RichText::new(icon).size(17.0)).min_size(vec2(32.0, 28.0)))
        .on_hover_text(tip)
        .clicked()
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
    });
    // No bundled fonts (keeps the exe small): Segoe UI for Latin, Microsoft YaHei for CJK file
    // names, Segoe UI Symbol for the toolbar/menu icons.
    let mut fonts = egui::FontDefinitions::empty();
    let mut list = vec![];
    for file in ["segoeui.ttf", "msyh.ttc", "seguisym.ttf"] {
        if let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) {
            fonts.font_data.insert(file.into(), egui::FontData::from_owned(bytes).into());
            list.push(file.to_string());
        }
    }
    fonts.families.insert(egui::FontFamily::Proportional, list.clone());
    fonts.families.insert(egui::FontFamily::Monospace, list);
    ctx.set_fonts(fonts);
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.mailbox && !self.mailbox_set {
            self.mailbox_set = true;
            frame.set_wgpu_surface_config(SurfaceConfig { present_mode: wgpu::PresentMode::Mailbox, desired_maximum_frame_latency: Some(1) });
        }
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        // Fifo already blocks in present; DwmFlush there would only add a second wait.
        if self.mailbox && !minimized {
            // Mailbox never blocks; DwmFlush paces us to the compositor without a GPU busy-wait.
            win::dwm_flush();
        }
        let frame_t0 = Instant::now();
        self.frames.push_back(frame_t0);
        while self.frames.front().is_some_and(|t| frame_t0.duration_since(*t).as_secs_f32() > 1.0) {
            self.frames.pop_front();
        }

        self.pump();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);

        // Global navigation input.
        let (back, fwd, bksp, f5) = ctx.input(|i| {
            (
                i.pointer.button_pressed(PointerButton::Extra1),
                i.pointer.button_pressed(PointerButton::Extra2),
                i.key_pressed(Key::Backspace),
                i.key_pressed(Key::F5),
            )
        });
        let typing = ctx.egui_wants_keyboard_input();
        if back {
            self.go_back();
        }
        if fwd {
            self.go_fwd();
        }
        if bksp && !typing {
            self.go_up();
        }
        if f5 && self.tree.is_some() {
            self.rescan();
        }

        egui::Panel::top("toolbar")
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin::symmetric(10, 6)))
            .show(ui, |ui| self.toolbar(ui));

        egui::Panel::bottom("status")
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin::symmetric(12, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let dot = if self.scan.is_some() { c32(5) } else { c32(1) };
                    ui.label(RichText::new("●").color(dot));
                    ui.label(RichText::new(&self.status).color(TEXT_DIM));
                    if let Some(t) = &self.tree {
                        let v = &t.dirs[self.view as usize];
                        ui.label(RichText::new(format!("{} · {} files", fmt_size(v.size), v.nfiles)).color(TEXT_DIM));
                    }
                    ui.label(RichText::new(format!("{} · {} fps · {:.1} ms", self.gpu_info, self.frames.len(), self.cpu_ms)).color(TEXT_DIM.gamma_multiply(0.6)));
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BG).inner_margin(Margin { left: 10, right: 10, top: 2, bottom: 4 }))
            .show(ui, |ui| {
                if self.tree.is_none() {
                    self.chooser(ui);
                    return;
                }
                self.treemap(ui);
                let parts = self.step_particles(dt);
                gfx::paint(ui.painter(), ui.clip_rect(), parts, self.now(), 2, gfx::NO_WAVE);
            });

        self.delete_modal(&ctx);

        let title = match (&self.tree, self.disk) {
            (Some(t), Some((_, free))) => format!(
                "SpaceSniffer-rs - [{} ({} free)] - {}",
                t.dirs[0].name,
                fmt_size(free),
                if self.scan.is_some() { "Scanning…" } else { "Ready" }
            ),
            _ => "SpaceSniffer-rs".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        self.cpu_ms = self.cpu_ms * 0.9 + frame_t0.elapsed().as_secs_f32() * 1000.0 * 0.1;
        if minimized {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        } else {
            // The pixel fields (treemap and drive picker) always twinkle.
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_terms() {
        let f = Filter::parse("*.mp4; >1mb");
        assert!(f.matches("Movie.MP4", 5 << 20));
        assert!(!f.matches("Movie.mp4", 10));
        assert!(!f.matches("a.mkv", 5 << 20));
        assert!(Filter::parse("cache").matches("npm-cache.bin", 1));
        assert!(Filter::parse("<1kb").matches("x", 100));
        assert!(glob(b"a*b?c", b"axxbyc"));
        assert!(!glob(b"a*b?c", b"axxbc"));
        assert_eq!(parse_size("1.5gb"), Some((1.5 * (1u64 << 30) as f64) as u64));
    }

    /// The transition wave starts at 0, finishes at 1, sweeps outward from its origin, leaves
    /// the outside of its rect alone, and drives the typewriter labels.
    #[test]
    fn wave_and_typing() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0));
        let o = pos2(0.0, 0.0);
        let w = |k| Wave { origin: o, maxd: far_corner(o, r), k, rect: r, outside: 0.0 };
        assert!(w(0.0).at(pos2(50.0, 50.0), 0.5) < 0.0);
        // Everything has flipped and fully cooled by the end, even the far corner.
        assert!(w(1.0).at(pos2(99.0, 99.0), 1.0) >= WAVE_COOL);
        assert!(w(0.0).at(pos2(0.0, 0.0), 0.0) >= 0.0, "the click point flips on the first frame");
        assert!(w(0.4).at(pos2(5.0, 5.0), 0.0) > w(0.4).at(pos2(90.0, 90.0), 0.0));
        assert!(w(0.9).at(pos2(150.0, 0.0), 0.0) < 0.0);
        assert_eq!(typed("abcd", 0.0, LMode::TypeIn), "");
        assert_eq!(typed("abcd", WAVE_COOL * 0.5, LMode::TypeIn), "ab");
        assert_eq!(typed("abcd", WAVE_COOL, LMode::TypeIn), "abcd");
        assert_eq!(typed("abcd", -1.0, LMode::Erase), "abcd");
        assert_eq!(typed("abcd", 0.0, LMode::Erase), "");
        assert_eq!(typed("abcd", 0.3, LMode::Full), "abcd");
    }

    #[test]
    fn accordion_keeps_one_branch() {
        // 0 ─ 1 ─ 3
        //   │   └ 4
        //   └ 2 ─ 5
        let mut t = Tree::new("R");
        t.apply(tree::Batch { id: 0, files: vec![], dirs: vec![(1, "a".into()), (2, "b".into())] }, 0.0);
        t.apply(tree::Batch { id: 1, files: vec![], dirs: vec![(3, "c".into()), (4, "d".into())] }, 0.0);
        t.apply(tree::Batch { id: 2, files: vec![], dirs: vec![(5, "e".into())] }, 0.0);
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
