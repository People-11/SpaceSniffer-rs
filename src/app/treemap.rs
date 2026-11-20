use super::*;

pub fn is_ancestor(t: &Tree, a: u32, mut b: u32) -> bool {
    while b != 0 {
        b = t.dirs[b as usize].parent;
        if b == a {
            return true;
        }
    }
    false
}

/// Rect of `d` (or its nearest ancestor that has a tile) in `tiles`.
pub fn find_rect(t: &Tree, tiles: &[Tile], mut d: u32) -> Option<Rect> {
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

/// Rect of dir `d` in `view`'s layout; with no tile (an ancestor folded since), where it would sit
/// opened, so zooms use its real size.
pub fn place(t: &Tree, tiles: &[Tile], view: u32, area: Rect, expanded: &HashSet<u32>, pitch: f32, d: u32) -> Option<Rect> {
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

/// What a layout depends on: (view, area, tree version, expand version).
pub type LayoutKey = (u32, Rect, u64, u64);

/// Tiles by item: (rect, open).
pub type At = ItemMap<(Rect, bool)>;

pub fn index(tiles: &[Tile]) -> At {
    tiles.iter().map(|x| (x.item, (x.rect, x.open))).collect()
}

/// Collapse every expansion inside `d`, which itself stays open (a press on its title strip).
pub fn fold(t: &Tree, expanded: &mut HashSet<u32>, d: u32) -> bool {
    let n = expanded.len();
    expanded.retain(|&e| !is_ancestor(t, d, e));
    expanded.len() != n
}

/// Accordion expand/collapse: one expanded folder per level; collapsing folds what is inside.
/// `open`: `d` is open by default, nothing to expand.
pub fn toggle(t: &Tree, expanded: &mut HashSet<u32>, d: u32, open: bool) -> bool {
    if expanded.remove(&d) {
        fold(t, expanded, d);
        true
    } else if !open {
        expanded.retain(|&e| is_ancestor(t, e, d));
        expanded.insert(d)
    } else {
        false
    }
}

/// The treemap's rect in `room`: on the dot lattice, centred by whole dots.
pub fn field_rect(room: Rect, pitch: f32) -> Rect {
    let area = layout::snap(room, pitch);
    let off = ((area.min.x + pitch - room.min.x) - (room.max.x - area.max.x)) * 0.5;
    area.translate(vec2(-(off / pitch).round() * pitch, 0.0))
}

/// What the pointer is over: a tile of the target layout, and whether that is a folder's title strip.
#[derive(Clone, Copy)]
pub struct Hit {
    pub item: Item,
    pub open: bool,
    pub title: bool,
}

impl App {
    /// Input first, so a click's transition starts this very frame.
    pub fn treemap(&mut self, ui: &mut egui::Ui) {
        let (room, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click());
        let area = self.fit(ui, room);
        let now = self.now();
        if self.anim.as_mut().is_some_and(|a| !a.prune(area, now)) {
            self.anim = None;
        }
        // Hit-test the target layout through its current camera: hover tracks the screen mid-transition.
        let hit = resp.hover_pos().and_then(|p| {
            let p = apply(inverse(self.cam), p);
            let x = self.tiles.iter().rev().find(|x| x.rect.contains(p))?;
            if let Item::File(d, _) = x.item
                && self.stale.contains(&d)
            {
                return None; // relisted since these tiles were laid out
            }
            Some(Hit { item: x.item, open: x.open, title: x.open && p.y < x.rect.min.y + self.header })
        });
        self.press(ui, &resp, hit);
        self.relayout(area, now);
        self.hover = hit.map(|h| h.item);
        self.ease_hover(ui, hit);
        self.draw(ui, area, None);
        self.tooltip(ui, &resp);
        self.menu(&resp);
    }

    /// The treemap's area in `room`, on the dot lattice of the current DPI.
    fn fit(&mut self, ui: &egui::Ui, room: Rect) -> Rect {
        let pitch = layout::grid(ui.ctx().pixels_per_point());
        if pitch != self.pitch {
            // DPI changed (e.g. moved to another monitor): the lattice moved, relayout from scratch.
            self.pitch = pitch;
            self.header = layout::header(pitch);
            // Not before the first layout: its transition is the zoom out of the chooser.
            if self.layout_key.0 != u32::MAX {
                self.drop_layout();
            }
        }
        field_rect(room.shrink(1.0), pitch)
    }

    /// A press on a folder expands/collapses it (on mouse down), on a title strip folds what is inside;
    /// a second press there within the double-click time zooms into the folder.
    fn press(&mut self, ui: &egui::Ui, resp: &egui::Response, hit: Option<Hit>) {
        let Some(pos) = resp.hover_pos().filter(|_| ui.input(|i| i.pointer.primary_pressed())) else { return };
        let time = ui.input(|i| i.time);
        if let Some((t0, p0, d)) = self.last_press.take()
            && time - t0 < 0.3
            && (pos - p0).length() < 6.0
        {
            if d != self.view {
                self.click_pos = Some(pos);
                self.navigate(d);
            }
            return;
        }
        let Some(hit) = hit else { return };
        let t = self.tree.as_ref().unwrap();
        if let Item::Dir(d) = hit.item
            && (if hit.title { fold(t, &mut self.expanded, d) } else { d != self.view && toggle(t, &mut self.expanded, d, hit.open) })
        {
            self.expand_ver += 1;
            self.toggled = Some(d);
            self.click_pos = Some(pos);
            // A lit block that opens stays lit as a whole, so what is under the pointer now starts
            // lit (see `ease_hover`) instead of easing in from dark behind the wave.
            if !hit.title
                && !hit.open
                && let Some(&(_, v)) = self.lit.tiles.iter().find(|x| x.0 == hit.item)
                && !self.group_fx.iter().any(|g| g.0 == hit.item)
            {
                self.group_fx.push((hit.item, v));
            }
        }
        let (Item::Dir(d) | Item::File(d, _)) = hit.item;
        self.last_press = Some((time, pos, d));
    }

    /// Lay out again when view, area, tree or expansions changed (every frame of a scan, ~0.2 ms); a
    /// new view or expansion becomes a transition.
    fn relayout(&mut self, area: Rect, now: f32) {
        let t = self.tree.as_ref().unwrap();
        let key = (self.view, area, t.version, self.expand_ver);
        let prev = std::mem::replace(&mut self.layout_key, key);
        if key == prev {
            return;
        }
        let lit = self.lit.clone();
        if key.0 != prev.0 {
            // New view: drop hover afterglow (the folder just entered would light everything).
            self.lit.tiles.clear();
            self.group_fx.clear();
        }
        let old = std::mem::take(&mut self.tiles);
        {
            let _s = perf::span("build");
            match self.built.iter().find(|b| b.0 == key) {
                Some(b) => self.tiles = b.1.clone(),
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
        if prev.0 == u32::MAX {
            return; // the first layout: its transition is the zoom out of the chooser
        }
        if key.1 != prev.1 {
            self.anim = None; // resized: no transition
            return;
        }
        if (key.0, key.3) == (prev.0, prev.3) {
            return; // the scan going on
        }
        let (from, to) = (prev.0, key.0);
        // The old layout's camera now (it may still be moving), and the click in its coords.
        let motion = Anim::motion(&self.anim, area, now);
        let click = self.click_pos.take().map(|p| apply(inverse(motion[0]), p));
        let toggled = self.toggled.take();
        let whole = (area, area);
        // (old -> new layout, wave origin and region in the new layout, outside is new, shared element)
        let (rel, origin, rect, outside, anchor) = if from == to {
            let d = toggled.unwrap_or(to);
            let at = find_rect(t, &self.tiles, d).unwrap_or(area).center();
            // No shared element: unchanged tiles are shared by both layers, a new title types in.
            (whole, click.unwrap_or(at), area, false, None)
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
    }

    /// Ease the hover light toward what `hit` is over, by one frame.
    fn ease_hover(&mut self, ui: &egui::Ui, hit: Option<Hit>) {
        let hover = hit.map(|h| h.item);
        let on_title = hit.is_some_and(|h| h.title);
        // A hovered block also lights its parent's title strip (a hovered title does not).
        let t = self.tree.as_ref().unwrap();
        let parent = match hover {
            _ if on_title => None,
            Some(Item::File(d, _)) => Some(Item::Dir(d)),
            Some(Item::Dir(d)) if d != 0 => Some(Item::Dir(t.dirs[d as usize].parent)),
            _ => None,
        };
        // A hovered title lights every block of its folder.
        let group = hover.filter(|_| on_title);
        // Hover brightness eases in/out over HOVER_SECS; tiles just left keep a short afterglow.
        let step = ui.input(|i| i.stable_dt).min(0.1) / HOVER_SECS;
        let tiles = &self.tiles;
        let rect_of = |it| tiles.iter().find(|x| x.item == it).map(|x| x.rect);
        ease_hover(&mut self.lit.tiles, &mut self.group_fx, [hover, parent], group, step, rect_of);
        self.lit.groups = self.group_fx.iter().filter_map(|&(g, v)| Some((rect_of(g)?, v * v * (3.0 - 2.0 * v)))).collect();
    }

    /// Own tooltip: egui's waits for pointer movement after a click or relayout.
    fn tooltip(&self, ui: &egui::Ui, resp: &egui::Response) {
        let (Some(item), Some(at), Some(t)) = (self.hover, resp.hover_pos(), &self.tree) else { return };
        if egui::Popup::is_any_open(ui.ctx()) {
            return;
        }
        let (name, extra) = match item {
            Item::Dir(d) => (&*t.dirs[d as usize].name, format!("{} files", t.dirs[d as usize].nfiles)),
            Item::File(d, f) => {
                let x = &t.dirs[d as usize];
                (x.file_name(f), CATS[x.files[f as usize].cat as usize].0.to_string())
            }
        };
        let tip = egui::Area::new(egui::Id::unique("tile_tip")).order(egui::Order::Tooltip).fixed_pos((at + vec2(14.0, 18.0)).round()).interactable(false);
        tip.show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.label(RichText::new(name).strong());
                ui.label(format!("{}  ·  {}", fmt_size(t.size(item)), extra));
            });
        });
    }

    fn menu(&mut self, resp: &egui::Response) {
        if resp.secondary_clicked() {
            self.menu_item = self.hover;
        }
        // Zoom and delete need `&mut self`, so they are applied after the menu; the rest act at once.
        let (mut zoom, mut delete) = (None, None);
        let scanning = self.scanning();
        resp.context_menu(|ui| {
            let (Some(item), Some(t)) = (self.menu_item, self.tree.as_ref()) else {
                ui.close();
                return;
            };
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Title strip: folds what is inside the folder; it stays open, and so does what is above.
        ex = HashSet::from([1, 3]);
        assert!(!fold(&t, &mut ex, 3), "nothing expanded inside 3");
        assert_eq!(ex, HashSet::from([1, 3]));
        assert!(fold(&t, &mut ex, 1));
        assert_eq!(ex, HashSet::from([1]));
        ex = HashSet::from([1, 3]);
        assert!(fold(&t, &mut ex, 0), "the view's own title folds everything");
        assert!(ex.is_empty());
        assert!(!fold(&t, &mut ex, 2), "nothing expanded: no change");
    }
}
