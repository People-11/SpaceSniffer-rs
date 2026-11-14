//! Nested squarified treemap, SpaceSniffer style: every directory gets a header strip
//! ("name - size") and its children are laid out inside.

use crate::tree::{Item, Tree};
use std::collections::{HashMap, HashSet};
use eframe::egui::{Rect, pos2, vec2};

/// Dot pitch in points; must match `cs` in shader.wgsl (2pt rounded down to whole pixels).
pub fn grid(ppp: f32) -> f32 {
    (2.0 * ppp).floor().max(2.0) / ppp
}

/// Folder title strip height: ~15pt, a whole number of dot rows.
pub fn header(pitch: f32) -> f32 {
    (15.0 / pitch).round().max(1.0) * pitch
}

/// Snap a rect onto the dot lattice so tiles hold whole dots and neighbours meet on a lattice
/// line (no half-cut dots, no hairline gaps).
pub fn snap(r: Rect, pitch: f32) -> Rect {
    let s = |v: f32| (v / pitch).round() * pitch;
    Rect::from_min_max(pos2(s(r.min.x), s(r.min.y)), pos2(s(r.max.x), s(r.max.y)))
}

pub struct Tile {
    pub rect: Rect,
    pub item: Item,
    /// Directory whose children were laid out.
    pub open: bool,
    /// Directory with a title strip (its name fits); smaller ones are plain pixel fields.
    pub titled: bool,
    /// Brightness tone 0..TONES: differs from every edge-adjacent visible tile (see `tones`).
    pub tone: u8,
}

/// Does a folder named `name` at `r` get a title strip (and children)? Only if its name fits;
/// otherwise it is a plain pixel field (not opened) until zoomed in enough to read it.
fn titled(name: &str, r: Rect, header: f32) -> bool {
    // shortcut: estimated width of the 11.5pt bold title font; measure with egui if titles get cut.
    let w: f32 = name.chars().map(|c| if c.is_ascii() { 6.6 } else { 12.0 }).sum();
    r.width() >= w + 10.0 && r.height() >= 2.0 * header
}

/// Levels below the view root that are open by default; deeper dirs open only when expanded.
const DEFAULT_DEPTH: u16 = 2;

/// `expanded`: dirs the user clicked open; each opens itself and its direct subdirs.
/// `pitch`: dot lattice pitch in points (see [`grid`]); every rect is snapped to it.
/// `prev`: the previous layout, whose tones are kept where possible.
pub fn build(tree: &Tree, root: u32, area: Rect, expanded: &HashSet<u32>, pitch: f32, prev: &[Tile], out: &mut Vec<Tile>) {
    out.clear();
    if tree.dirs[root as usize].size > 0 {
        let g = Grid { pitch, header: header(pitch) };
        let area = snap(area, pitch);
        dir(tree, root, area, 0, expanded, &g, out);
        tones(out, area, pitch, prev);
    }
}

/// Number of brightness tones (4 colours always suffice for a map of rectangles in practice).
pub const TONES: u8 = 4;

/// Map colouring of the visible tiles (everything not open) on a dot-lattice grid of tile indices.
/// Big tiles go first; each takes the tone sharing the least edge length with same-toned coloured
/// neighbours (ties: the one farthest from the other neighbours' tones). Tiles
/// visible in `prev` keep their tone when it clashes with nothing, so expanding a folder doesn't
/// recolour the blocks around it; a final pass recolours any tile still clashing.
fn tones(out: &mut [Tile], area: Rect, pitch: f32, prev: &[Tile]) {
    const NEAR: [f32; TONES as usize] = [0.0, 0.3, 0.08, 0.0]; // secondary cost by tone distance
    let cols = (area.width() / pitch).round() as isize;
    let rows = (area.height() / pitch).round() as isize;
    let at = |v: f32, o: f32| ((v - o) / pitch).round() as isize;
    let cells = |t: &Tile| {
        let (x0, y0) = (at(t.rect.min.x, area.min.x).max(0), at(t.rect.min.y, area.min.y).max(0));
        (x0, y0, at(t.rect.max.x, area.min.x).min(cols), at(t.rect.max.y, area.min.y).min(rows))
    };
    let mut order: Vec<usize> = (0..out.len()).filter(|&i| !out[i].open).collect();
    order.sort_by(|&a, &b| out[b].rect.area().total_cmp(&out[a].rect.area()));
    let mut map = vec![u32::MAX; (cols * rows).max(0) as usize];
    for &i in &order {
        let (x0, y0, x1, y1) = cells(&out[i]);
        for y in y0..y1 {
            map[(y * cols + x0) as usize..(y * cols + x1) as usize].fill(i as u32);
        }
    }
    let mut tone = vec![u8::MAX; out.len()];
    // Shared boundary length (in dots) with already-coloured neighbours, per neighbour tone.
    let near = |i: usize, tone: &[u8]| {
        let (x0, y0, x1, y1) = cells(&out[i]);
        let mut w = [0.0f32; TONES as usize];
        let mut look = |x: isize, y: isize| {
            if (0..cols).contains(&x) && (0..rows).contains(&y) {
                let j = map[(y * cols + x) as usize];
                if j != u32::MAX && tone[j as usize] != u8::MAX {
                    w[tone[j as usize] as usize] += 1.0;
                }
            }
        };
        for x in x0..x1 {
            look(x, y0 - 1);
            look(x, y1);
        }
        for y in y0..y1 {
            look(x0 - 1, y);
            look(x1, y);
        }
        w
    };
    // Same-tone edge first (any clash-free tone wins), then how close the other neighbours' tones are.
    let cost = |w: &[f32; TONES as usize], c: u8| (w[c as usize], (0..TONES).map(|n| w[n as usize] * NEAR[n.abs_diff(c) as usize]).sum::<f32>());
    let best = |w: &[f32; TONES as usize]| (0..TONES).min_by(|&a, &b| cost(w, a).partial_cmp(&cost(w, b)).unwrap()).unwrap();
    let old: HashMap<Item, u8> = prev.iter().filter(|t| !t.open).map(|t| (t.item, t.tone)).collect();
    for &i in &order {
        if let Some(&c) = old.get(&out[i].item)
            && near(i, &tone)[c as usize] == 0.0
        {
            tone[i] = c;
        }
    }
    for &i in &order {
        if tone[i] == u8::MAX {
            tone[i] = best(&near(i, &tone));
        }
    }
    // Local repair: recolour wherever another tone clashes less, until nothing changes.
    for _ in 0..4 {
        let mut changed = false;
        for &i in &order {
            let w = near(i, &tone);
            let c = best(&w);
            if w[tone[i] as usize] > 0.0 && cost(&w, c) < cost(&w, tone[i]) {
                tone[i] = c;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for &i in &order {
        out[i].tone = tone[i];
    }
}

struct Grid {
    pitch: f32,
    header: f32,
}

#[allow(clippy::too_many_arguments)]
fn dir(tree: &Tree, d: u32, r: Rect, depth: u16, expanded: &HashSet<u32>, g: &Grid, out: &mut Vec<Tile>) {
    let idx = out.len();
    let x = &tree.dirs[d as usize];
    let has_title = depth == 0 || titled(&x.name, r, g.header); // the view root always opens
    out.push(Tile { rect: r, item: Item::Dir(d), open: false, titled: has_title, tone: 0 });
    // Children fill everything below the title strip: no padding gutters.
    let inner = Rect::from_min_max(pos2(r.min.x, r.min.y + g.header), r.max);
    // Opens by default only if its name fits; a folder the user clicked opens regardless.
    let wanted = has_title && (depth <= DEFAULT_DEPTH || expanded.contains(&x.parent)) || expanded.contains(&d);
    if !wanted || inner.width() < 12.0 || inner.height() < g.pitch {
        return;
    }
    out[idx].open = true;
    let mut kids: Vec<(f64, Item)> = x
        .dirs
        .iter()
        .map(|&c| (tree.dirs[c as usize].size as f64, Item::Dir(c)))
        .chain(x.files.iter().enumerate().map(|(i, f)| (f.size as f64, Item::File(d, i as u32))))
        .filter(|k| k.0 > 0.0)
        .collect();
    kids.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    // Children smaller than one dot are dropped; the rest are scaled up to fill the whole body, so
    // no leftover patch of the folder's own field shows.
    let area = (inner.width() * inner.height()) as f64;
    let total: f64 = kids.iter().map(|k| k.0).sum();
    let dot = (g.pitch * g.pitch) as f64;
    kids.truncate(kids.partition_point(|k| k.0 * area / total >= dot));
    let kept: f64 = kids.iter().map(|k| k.0).sum();
    for k in &mut kids {
        k.0 *= area / kept;
    }
    let mut placed = Vec::new();
    squarify(&kids, inner, &mut placed);
    for (rect, item) in placed {
        let rect = snap(rect, g.pitch);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue; // rounded away on the lattice: its neighbours' snapped edges absorb it
        }
        match item {
            Item::Dir(c) => dir(tree, c, rect, depth + 1, expanded, g, out),
            Item::File(..) => out.push(Tile { rect, item, open: false, titled: false, tone: 0 }),
        }
    }
}

/// `items` sorted descending, areas already scaled so their sum == rect area.
fn squarify(items: &[(f64, Item)], mut r: Rect, out: &mut Vec<(Rect, Item)>) {
    let mut i = 0;
    while i < items.len() {
        let short = r.width().min(r.height()) as f64;
        if short <= 0.0 {
            return;
        }
        let worst = |sum: f64, big: f64, small: f64| {
            let s2 = sum * sum;
            let w2 = short * short;
            (w2 * big / s2).max(s2 / (w2 * small))
        };
        let mut j = i + 1;
        let mut sum = items[i].0;
        let mut best = worst(sum, items[i].0, items[i].0);
        while j < items.len() {
            let s = sum + items[j].0;
            let w = worst(s, items[i].0, items[j].0);
            if w > best {
                break;
            }
            best = w;
            sum = s;
            j += 1;
        }
        let thick = (sum / short) as f32;
        let horizontal = r.width() >= r.height(); // row is a column on the left when wide
        let mut off = 0.0f32;
        for it in &items[i..j] {
            let len = (it.0 / sum * short) as f32;
            let rect = if horizontal {
                Rect::from_min_size(pos2(r.min.x, r.min.y + off), vec2(thick, len))
            } else {
                Rect::from_min_size(pos2(r.min.x + off, r.min.y), vec2(len, thick))
            };
            out.push((rect, it.1));
            off += len;
        }
        if horizontal {
            r.min.x += thick;
        } else {
            r.min.y += thick;
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squarify_fills_area_without_overlap() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0));
        let sizes = [24000.0, 12000.0, 9000.0, 6000.0, 4000.0, 3000.0, 2000.0];
        let items: Vec<_> = sizes.iter().enumerate().map(|(i, &s)| (s, Item::Dir(i as u32))).collect();
        let mut out = vec![];
        squarify(&items, r, &mut out);
        assert_eq!(out.len(), sizes.len());
        let area: f32 = out.iter().map(|(r, _)| r.area()).sum();
        assert!((area - 60000.0).abs() < 1.0, "area {area}");
        for (a, _) in &out {
            assert!(r.expand(0.01).contains_rect(*a));
            for (b, _) in &out {
                if a != b {
                    let i = a.intersect(*b);
                    assert!(i.width() < 0.01 || i.height() < 0.01);
                }
            }
        }
    }

    #[test]
    fn tiles_snap_to_lattice_without_gaps() {
        let mut t = Tree::new("R");
        t.apply(crate::tree::Batch { id: 0, files: vec![("a".into(), 500), ("b".into(), 300), ("c".into(), 120), ("d".into(), 80)], dirs: vec![] }, 0.0);
        let pitch = 8.0 / 3.0; // 150% scaling: 4px dots
        let area = Rect::from_min_size(pos2(10.3, 7.9), vec2(401.0, 299.0));
        let mut out = vec![];
        build(&t, 0, area, &HashSet::new(), pitch, &[], &mut out);
        let on = |v: f32| ((v / pitch) - (v / pitch).round()).abs() < 1e-3;
        for x in &out {
            assert!(on(x.rect.min.x) && on(x.rect.min.y) && on(x.rect.max.x) && on(x.rect.max.y), "{:?}", x.rect);
        }
        let root = out[0].rect;
        let inner = Rect::from_min_max(pos2(root.min.x, root.min.y + header(pitch)), root.max);
        let kids: f32 = out[1..].iter().map(|x| x.rect.area()).sum();
        assert!((kids - inner.area()).abs() < 1e-2, "children cover the folder body exactly: {kids} vs {}", inner.area());
    }

    #[test]
    fn edge_neighbours_never_share_a_tone() {
        let mut t = Tree::new("R");
        let files = (1..60u64).map(|i| (format!("f{i}").into(), i * i * 1000)).collect();
        t.apply(crate::tree::Batch { id: 0, files, dirs: vec![] }, 0.0);
        let mut out = vec![];
        build(&t, 0, Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0)), &HashSet::new(), 3.0, &[], &mut out);
        let vis: Vec<_> = out.iter().filter(|x| !x.open).collect();
        assert!(vis.len() > 30);
        for a in &vis {
            for b in &vis {
                let i = a.rect.intersect(b.rect);
                // touching along an edge (not just a corner)
                if !std::ptr::eq(*a, *b) && (i.width() > 0.0 && i.height() == 0.0 || i.height() > 0.0 && i.width() == 0.0) {
                    assert_ne!(a.tone, b.tone, "{:?} {:?}", a.rect, b.rect);
                }
            }
        }
    }

    /// Total edge length shared by visible tiles of the same tone, and by the larger ones (> 2000pt²).
    fn clashes(out: &[Tile]) -> (f32, f32) {
        let vis: Vec<_> = out.iter().filter(|x| !x.open).collect();
        let (mut all, mut big) = (0.0, 0.0);
        for (i, a) in vis.iter().enumerate() {
            for b in &vis[i + 1..] {
                let r = a.rect.intersect(b.rect);
                let len = if r.width() == 0.0 { r.height() } else if r.height() == 0.0 { r.width() } else { 0.0 };
                if len > 0.0 && a.tone == b.tone {
                    all += len;
                    if a.rect.area() > 2000.0 && b.rect.area() > 2000.0 {
                        big += len;
                    }
                }
            }
        }
        (all, big)
    }

    /// `SS_DIR=<folder> cargo test --release tone_clash_real -- --ignored --nocapture`:
    /// same-tone shared edges on a real folder.
    #[test]
    #[ignore]
    fn tone_clash_real() {
        let root = std::env::var("SS_DIR").unwrap();
        let scan = crate::tree::start(root.clone().into(), 0, 1);
        // Relayout after every batch, keeping tones from the previous layout, as the app does while scanning.
        let mut t = Tree::new(&root);
        let (mut out, mut prev) = (vec![], vec![]);
        while let Ok(crate::tree::Msg::Batch(b)) = scan.rx.recv() {
            t.apply(b, 0.0);
            std::mem::swap(&mut out, &mut prev);
            build(&t, 0, Rect::from_min_size(pos2(0.0, 0.0), vec2(1260.0, 680.0)), &HashSet::new(), 8.0 / 3.0, &prev, &mut out);
        }
        let (all, big) = clashes(&out);
        eprintln!("{} visible tiles, same-tone shared edge: {all:.0}pt total, {big:.0}pt between big tiles", out.iter().filter(|x| !x.open).count());
    }

    #[test]
    fn expanding_a_folder_keeps_the_tones_around_it() {
        use crate::tree::Batch;
        // root -> x -> A (open by default) holding folder B (closed, depth 3) and 9 files.
        let mut t = Tree::new("R");
        t.apply(Batch { id: 0, files: vec![], dirs: vec![(1, "x".into())] }, 0.0);
        t.apply(Batch { id: 1, files: vec![], dirs: vec![(2, "A".into())] }, 0.0);
        let files = (1..10u64).map(|i| (format!("f{i}").into(), i * 3000)).collect();
        t.apply(Batch { id: 2, files, dirs: vec![(3, "B".into())] }, 0.0);
        let files = (1..20u64).map(|i| (format!("b{i}").into(), i * 1000)).collect();
        t.apply(Batch { id: 3, files, dirs: vec![] }, 0.0);
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 600.0));
        let (mut before, mut after) = (vec![], vec![]);
        build(&t, 0, area, &HashSet::new(), 3.0, &[], &mut before);
        assert!(before.iter().any(|x| x.item == Item::Dir(3) && !x.open));
        build(&t, 0, area, &HashSet::from([3]), 3.0, &before, &mut after);
        assert!(after.iter().any(|x| x.item == Item::Dir(3) && x.open));
        let tone = |v: &[Tile], i: Item| v.iter().find(|x| x.item == i).map(|x| x.tone);
        for f in 0..9 {
            let i = Item::File(2, f);
            assert_eq!(tone(&before, i), tone(&after, i), "{i:?}");
        }
    }

    #[test]
    fn children_below_one_dot_are_dropped_and_the_rest_fill_the_body() {
        let mut t = Tree::new("R");
        let mut files = vec![("big".into(), 1_000_000u64)];
        files.extend((0..500).map(|i| (format!("f{i}").into(), 1u64)));
        t.apply(crate::tree::Batch { id: 0, files, dirs: vec![] }, 0.0);
        let mut out = vec![];
        build(&t, 0, Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0)), &HashSet::new(), 3.0, &[], &mut out);
        assert_eq!(out.len(), 2, "root + the big file only");
        let root = out[0].rect;
        let inner = Rect::from_min_max(pos2(root.min.x, root.min.y + header(3.0)), root.max);
        let covered: f32 = out[1..].iter().map(|x| x.rect.area()).sum();
        assert!((covered - inner.area()).abs() < 1e-2, "{covered} vs {}", inner.area());
    }
}
