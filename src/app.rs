use crate::gfx::{self, Inst};
use crate::layout::{self, Tile};
use crate::perf;
use crate::tree::{self, CATS, Item, ItemMap, Tree};
use crate::win;
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Key, Margin, PointerButton, Pos2, Rect, RichText, Sense, Vec2, pos2, vec2};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

mod anim;
mod chooser;
mod particles;
mod scene;
mod treemap;
use anim::*;
use chooser::*;
use particles::*;
use scene::*;
use treemap::*;

const BG: Color32 = Color32::from_rgb(11, 12, 15);
const CARD: Color32 = Color32::from_rgb(21, 22, 26);
const TEXT_DIM: Color32 = Color32::from_rgb(140, 144, 152);

/// All folders share one colour (a warm copper); files are coloured by type.
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

pub struct App {
    tree: Option<Tree>,
    scan: Option<tree::Scan>,
    scan_t0: Instant,
    /// `scan` is only a refresh of what the watch saw change: quiet, and not in the way.
    refreshing: bool,
    /// Dirs a refresh relisted this frame: their files changed index (the tiles hit-tested are older).
    stale: HashSet<u32>,
    watch: Option<crate::watch::Watch>,
    ctx: egui::Context,
    view: u32,
    back: Vec<u32>,
    fwd: Vec<u32>,
    tiles: Vec<Tile>,
    layout_key: LayoutKey,
    /// Recent layouts by `layout_key`, newest first: back / forward between big folders reuses them.
    built: Vec<(LayoutKey, Vec<Tile>)>,
    /// The treemap's layer stack as drawn last frame, and the last `Scene::ver` handed out.
    scene: Option<Scene>,
    scene_gen: u64,
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
    /// Last press on the treemap (time, pos, folder): a second one there soon after zooms in.
    last_press: Option<(f64, Pos2, u32)>,
    /// Dot lattice pitch and folder title height (points) for the current DPI.
    pitch: f32,
    header: f32,
    /// Hover light of the current layout.
    lit: Lit,
    /// Folders lit as a whole (hovered title), 0..1, kept per folder, not per tile; `lit.groups` per frame.
    group_fx: Vec<(Item, f32)>,
    /// Camera of the target layout last frame (layout rect -> screen rect), for hit-testing mid-zoom.
    cam: Map,
    confirm: Option<Item>,
    particles: Particles,
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
            refreshing: false,
            stale: HashSet::new(),
            watch: None,
            ctx: cc.egui_ctx.clone(),
            view: 0,
            back: vec![],
            fwd: vec![],
            tiles: vec![],
            layout_key: (u32::MAX, Rect::NOTHING, 0, 0),
            built: vec![],
            scene: None,
            scene_gen: 0,
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
            particles: Particles::default(),
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
        // From the start: what changes during the scan is caught up on after it.
        let ctx = self.ctx.clone();
        self.watch = crate::watch::watch(&pb, move || ctx.request_repaint());
        self.begin_scan(pb, 0, 1);
    }

    fn begin_scan(&mut self, path: PathBuf, id: u32, next: u32) {
        self.scan = Some(tree::start(vec![(id, path, vec![])], next));
        self.refreshing = false;
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

    /// Forget all that names a dir or file by index (history, hover, menu, delete dialog): it may be gone.
    fn drop_history(&mut self) {
        self.back.clear();
        self.fwd.clear();
        self.hover = None;
        self.menu_item = None;
        self.confirm = None;
    }

    /// Rescan only the current view (SpaceSniffer "refresh").
    fn rescan(&mut self) {
        let Some(t) = self.tree.as_mut() else { return };
        // A refresh is let finish (it is short): cut off, the new dirs it found would stay half listed.
        if let Some(s) = self.scan.take().filter(|_| self.refreshing) {
            while let Ok(b) = s.rx.recv() {
                t.apply(b);
            }
            while t.dirs[self.view as usize].dead {
                self.view = t.dirs[self.view as usize].parent;
            }
        }
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

    /// A scan, not a refresh.
    fn scanning(&self) -> bool {
        self.scan.is_some() && !self.refreshing
    }

    fn pump(&mut self) {
        self.stale.clear();
        let Some(t) = self.tree.as_mut() else { return };
        if let Some(scan) = &self.scan {
            let budget = Instant::now();
            let mut done = false;
            // Bounded so a burst of batches never stalls a frame.
            while budget.elapsed().as_millis() < 8 {
                match scan.rx.try_recv() {
                    Ok(b) => {
                        let id = b.id;
                        if t.apply(b) && self.refreshing {
                            self.stale.insert(id);
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
            if done {
                self.scan = None;
                if !self.refreshing {
                    let secs = self.scan_t0.elapsed().as_secs_f32();
                    self.status = format!("Ready · scanned in {secs:.2}s");
                }
            }
        }
        if self.refreshing && !self.stale.is_empty() {
            // Whatever names a relisted dir's file or a dir now gone may mean something else: forget it.
            let stale = |it: &Item| match *it {
                Item::Dir(d) => t.dirs[d as usize].dead,
                Item::File(d, _) => self.stale.contains(&d) || t.dirs[d as usize].dead,
            };
            self.menu_item = self.menu_item.filter(|x| !stale(x));
            self.confirm = self.confirm.filter(|x| !stale(x));
            self.back.retain(|&d| !t.dirs[d as usize].dead);
            self.fwd.retain(|&d| !t.dirs[d as usize].dead);
            while t.dirs[self.view as usize].dead {
                self.view = t.dirs[self.view as usize].parent;
            }
        }
        if self.scan.is_none() {
            self.refresh();
        }
    }

    /// Relist the dirs the watch saw change (all of them if it lost count), keeping their subdirs.
    fn refresh(&mut self) {
        let (Some(w), Some(t)) = (&self.watch, &self.tree) else { return };
        let mut dirs = HashSet::new();
        let mut all = false;
        while let Ok(c) = w.rx.try_recv() {
            match c {
                Some(paths) => dirs.extend(paths.iter().map(|p| t.find(p))),
                None => all = true,
            }
        }
        if all {
            dirs = (0..t.dirs.len() as u32).filter(|&d| !t.dirs[d as usize].dead).collect();
        }
        if dirs.is_empty() {
            return;
        }
        let jobs = dirs.into_iter().map(|d| (d, t.path(Item::Dir(d)), t.known(d))).collect();
        self.scan = Some(tree::start(jobs, t.dirs.len() as u32));
        self.refreshing = true;
    }

    fn navigate(&mut self, d: u32) {
        if d != self.view {
            self.back.push(self.view);
            self.fwd.clear();
            self.view = d;
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
            self.particles.burst(r, col, self.pitch);
        }
        // Files of that dir changed index (swap_remove): never hit-test those stale tiles.
        if let Item::File(d, _) = item {
            self.tiles.retain(|x| !matches!(x.item, Item::File(e, _) if e == d));
        }
        self.status = format!("Moved to Recycle Bin: {} ({})", path.display(), fmt_size(freed));
    }

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
                self.leave_for_chooser(ui.ctx());
            }
            ui.separator();
            if tool(ui, "\u{E72B}", "Back (Mouse 4)", !self.back.is_empty(), mouse4) {
                step(&mut self.back, &mut self.fwd, &mut self.view);
            }
            if tool(ui, "\u{E72A}", "Forward (Mouse 5)", !self.fwd.is_empty(), mouse5) {
                step(&mut self.fwd, &mut self.back, &mut self.view);
            }
            if tool(ui, "\u{E74A}", "Up one level (Backspace)", has_tree && self.view != 0, bksp) {
                nav = self.tree.as_ref().map(|t| t.dirs[self.view as usize].parent);
            }
            if tool(ui, "\u{E80F}", "Root", has_tree && self.view != 0, false) {
                nav = Some(0);
            }
            ui.separator();
            // Not while scanning: rescanning one folder cancels the scan of everything else.
            if tool(ui, "\u{E72C}", "Rescan current view (F5)", has_tree && !self.scanning(), f5) {
                self.rescan();
            }
            if tool(ui, "\u{E71A}", "Stop scanning", self.scanning(), false) {
                self.scan = None;
                self.status = "Stopped".into();
            }
            if let Some(t) = &self.tree {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Breadcrumb, right aligned: the current folder, then up as far as there is room ("…" for the rest).
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

/// Back / forward: to the last view on `from`, the current one going onto `to`.
fn step(from: &mut Vec<u32>, to: &mut Vec<u32>, view: &mut u32) {
    if let Some(d) = from.pop() {
        to.push(*view);
        *view = d;
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
    // No bundled fonts (exe size): Segoe UI, Microsoft YaHei for CJK names, Segoe UI Symbol; icons from
    // Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets (Windows 10), which share code points.
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
    /// The clear colour is the background, so panels need no fill (a whole extra window of pixels).
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let minimized = ctx.input(|i| i.viewport().minimized.unwrap_or(false));
        // eframe's CPU time for the whole last frame, less swapchain waits (ui() alone misses the upload).
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
                let dot = if self.scanning() { c32(5) } else { c32(1) };
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
            let parts = self.particles.step(dt, self.pitch);
            ui.painter().add(gfx::shape(ui.clip_rect(), 2, gfx::Field { insts: parts.into(), ..Default::default() }));
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
