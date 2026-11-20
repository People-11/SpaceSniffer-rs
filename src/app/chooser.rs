use super::*;

/// The drive chooser as a transition layer (bottom when zooming into a scan, top when leaving for
/// it). Its layout coords are screen coords.
#[derive(Clone)]
pub struct Chooser {
    /// Drive blocks, and their title strips with the text in them.
    pub insts: Vec<Inst>,
    pub titles: Vec<(Rect, String)>,
    /// Each drive's block (as in `App::drives`).
    pub blocks: Vec<Rect>,
    /// The picked drive's block (None: a typed path, no zoom), and where the wave starts.
    pub block: Option<Rect>,
    pub click: Pos2,
    /// Camera and card opacity when the drive was picked (mid-zoom back to it: the new zoom starts there).
    pub cam: Option<[Map; 2]>,
    pub card: f32,
}

/// Part of a zoom out of (into) the chooser over which its card (heading, path row) fades away (in).
pub const CARD_FADE: f32 = 0.3;

/// A drive root with its (total, free) bytes, if read.
pub type Drive = (String, Option<(u64, u64)>);

impl App {
    /// List the drives now, their space later: a slow network drive must not hold up the window.
    pub fn refresh_drives(&mut self, ctx: &egui::Context) {
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

    /// `ghost`: only its card, at this opacity, over a transition (no drives, no input).
    pub fn chooser(&mut self, ui: &mut egui::Ui, ghost: Option<f32>) -> Chooser {
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
                // Drives as pixel-field blocks like folders: used space lit, free space dim.
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
                        // Keyed by drive: the ghost chooser (another Ui) shares the hover, so it never restarts.
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
                    ui.painter().set(field, gfx::shape(ui.clip_rect(), 0, gfx::Field { insts: insts.clone().into(), ..Default::default() }));
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

    /// Back to the chooser: zoom out to it, then drop the tree (see [`App::leave`]).
    pub fn leave_for_chooser(&mut self, ctx: &egui::Context) {
        self.scan = None;
        self.watch = None;
        self.status = "Ready".into();
        self.refresh_drives(ctx);
        self.leaving = self.tree.is_some();
        self.drop_history();
        self.particles.clear();
    }

    /// Zoom out of the treemap into its drive's block, then drop the tree. A drive clicked meanwhile is
    /// entered at once from where the chooser is on screen: false then (the caller draws that).
    pub fn leave(&mut self, ui: &mut egui::Ui) -> bool {
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
        // The chooser's camera now (or the running transition's).
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
        if !self.left {
            // Like zooming out to a parent: the view shrinks into the drive's block (none for a folder).
            self.left = true;
            let root = &*self.tree.as_ref().unwrap().dirs[0].name;
            let block = self.drives.iter().position(|d| d.0.eq_ignore_ascii_case(root)).map(|i| top.blocks[i]);
            let old = std::mem::take(&mut self.tiles);
            let (rel, origin, rect, outside) = match block {
                Some(b) => ((area, b), b.center(), b, true),
                None => ((area, area), area.center(), area, false),
            };
            let lit = self.lit.clone();
            Anim::push(&mut self.anim, from, area, Step { old, rel, local: false, anchor: None, t0: now, origin, rect, outside, at: None, chooser: None, lit });
        }
        self.draw(ui, area, Some(&top));
        true
    }

    /// Zoom out of chooser `c` into the scan just started from it, over `area` (the treemap's).
    pub fn enter(&mut self, c: Chooser, area: Rect) {
        let rel = (c.block.unwrap_or(area), area);
        let (origin, from) = (apply(rel, c.click), c.cam.unwrap_or([(area, area); 2]));
        let now = self.now();
        let step =
            Step { old: vec![], rel, local: false, anchor: None, t0: now, origin, rect: area, outside: false, at: None, chooser: Some(c), lit: Lit::default() };
        Anim::push(&mut self.anim, from, area, step);
    }

    /// Card opacity: fades out over the start of a zoom out of the chooser, in over the end of one back.
    pub fn card(&self, now: f32) -> f32 {
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
}
