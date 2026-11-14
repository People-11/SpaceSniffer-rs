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
    /// In its folder's block of small tiles: no gutter of its own (the block has one).
    pub solid: bool,
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
/// visible in `prev` keep their tone (see below), so expanding a folder doesn't recolour the
/// blocks around it; a final pass recolours any other tile still clashing.
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
    // A tile that did not move keeps its tone whatever happens around it (so far-off blocks never
    // change colour when a folder opens); one that moved keeps it if that clashes with nothing.
    let old: HashMap<Item, (u8, Rect)> = prev.iter().filter(|t| !t.open).map(|t| (t.item, (t.tone, t.rect))).collect();
    let mut still = vec![false; out.len()];
    for &i in &order {
        if let Some(&(c, r)) = old.get(&out[i].item)
            && r == out[i].rect
        {
            tone[i] = c;
            still[i] = true;
        }
    }
    for &i in &order {
        if let Some(&(c, _)) = old.get(&out[i].item)
            && !still[i]
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
        for &i in order.iter().filter(|&&i| !still[i]) {
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

fn dir(tree: &Tree, d: u32, r: Rect, depth: u16, expanded: &HashSet<u32>, g: &Grid, out: &mut Vec<Tile>) {
    let idx = out.len();
    let x = &tree.dirs[d as usize];
    let has_title = depth == 0 || titled(&x.name, r, g.header); // the view root always opens
    out.push(Tile { rect: r, item: Item::Dir(d), open: false, titled: has_title, tone: 0, solid: false });
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
    // Tiles too small for a gutter (see shader.wgsl) can't be outlined one by one: from the first
    // of them on (the smallest come last), they are gathered into one block that gets the gutter
    // (top row and left column, the folder's dark body showing through) and fill it unoutlined.
    // The block goes last, so it takes the bottom-right corner; whatever comes out too small
    // beside it joins it too, until every tile outside it has room for its own gutter.
    let small = |r: Rect| {
        let r = snap(r, g.pitch);
        (r.width().min(r.height()) / g.pitch).round() < MIN_GUTTER
    };
    let mut placed = Vec::new();
    let mut k = kids.len();
    // A block too small for its own gutter would leave its neighbours open: it is given more room
    // than its files' share (up to MAX_REST of the body), the others shrinking to make way.
    let mut boost = 1.0;
    loop {
        placed.clear();
        let mut top = kids[..k].to_vec();
        if k < kids.len() {
            top.push((kids[k..].iter().map(|t| t.0).sum::<f64>() * boost, REST));
        }
        let total: f64 = top.iter().map(|t| t.0).sum();
        top.iter_mut().for_each(|t| t.0 *= area / total);
        squarify(&top, inner, &mut placed);
        // squarify keeps the order, so the first small tile's index is its index in `kids`.
        match placed.iter().position(|p| p.1 != REST && small(p.0)) {
            Some(j) => k = j,
            None if k > 0 && placed.last().is_some_and(|p| p.1 == REST && small(p.0)) => {
                if top.last().is_some_and(|t| t.0 < area * MAX_REST) {
                    boost *= 1.5;
                } else {
                    // Still too small at that size: too small to show at all, like the sub-dot
                    // tiles above; the rest grow to fill the body.
                    kids.truncate(k);
                    boost = 1.0;
                }
            }
            None => break,
        }
    }
    let mut solid = placed.len();
    if let Some(i) = placed.iter().position(|p| p.1 == REST) {
        let block = snap(placed.swap_remove(i).0, g.pitch);
        solid = placed.len();
        let body = if small(block) { block } else { Rect::from_min_max(block.min + vec2(g.pitch, g.pitch), block.max) };
        let scale = (body.width() * body.height()) as f64 / kids[k..].iter().map(|t| t.0).sum::<f64>();
        let tail: Vec<(f64, Item)> = kids[k..].iter().map(|&(s, it)| (s * scale, it)).collect();
        squarify(&tail, body, &mut placed);
    }
    for (n, (rect, item)) in placed.into_iter().enumerate() {
        let rect = snap(rect, g.pitch);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue; // rounded away on the lattice: its neighbours' snapped edges absorb it
        }
        let at = out.len();
        match item {
            Item::Dir(c) => dir(tree, c, rect, depth + 1, expanded, g, out),
            Item::File(..) => out.push(Tile { rect, item, open: false, titled: false, tone: 0, solid: false }),
        }
        out[at].solid = n >= solid;
    }
}

/// Smallest tile side (dots) that gets a gutter; must match the gutter rule in shader.wgsl.
const MIN_GUTTER: f32 = 3.0;
/// Most of a folder's body its block of small tiles may take to make room for its gutter.
const MAX_REST: f64 = 0.25;
/// Placeholder for the block of small tiles while laying out (no dir has this index).
const REST: Item = Item::Dir(u32::MAX);

/// `items` sorted descending (except a last one, which then takes the bottom-right corner, less
/// square), areas already scaled so their sum == rect area. Keeps their order in `out`.
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
        t.apply(crate::tree::Batch { id: 0, files: vec![("a".into(), 500), ("b".into(), 300), ("c".into(), 120), ("d".into(), 80)], dirs: vec![] });
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
        t.apply(crate::tree::Batch { id: 0, files, dirs: vec![] });
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

    #[test]
    fn expanding_a_folder_keeps_the_tones_around_it() {
        use crate::tree::Batch;
        // root -> x -> A (open by default) holding folder B (closed, depth 3) and 9 files.
        let mut t = Tree::new("R");
        t.apply(Batch { id: 0, files: vec![], dirs: vec![(1, "x".into())] });
        t.apply(Batch { id: 1, files: vec![], dirs: vec![(2, "A".into())] });
        let files = (1..10u64).map(|i| (format!("f{i}").into(), i * 3000)).collect();
        t.apply(Batch { id: 2, files, dirs: vec![(3, "B".into())] });
        let files = (1..20u64).map(|i| (format!("b{i}").into(), i * 1000)).collect();
        t.apply(Batch { id: 3, files, dirs: vec![] });
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
        // No tile that stayed where it was changes colour, however far from the change.
        for a in after.iter().filter(|a| !a.open) {
            if let Some(b) = before.iter().find(|b| b.item == a.item && b.rect == a.rect && !b.open) {
                assert_eq!(a.tone, b.tone, "{:?}", a.item);
            }
        }
    }

    #[test]
    fn children_below_one_dot_are_dropped_and_the_rest_fill_the_body() {
        let mut t = Tree::new("R");
        let mut files = vec![("big".into(), 1_000_000u64)];
        files.extend((0..500).map(|i| (format!("f{i}").into(), 1u64)));
        t.apply(crate::tree::Batch { id: 0, files, dirs: vec![] });
        let mut out = vec![];
        build(&t, 0, Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0)), &HashSet::new(), 3.0, &[], &mut out);
        assert_eq!(out.len(), 2, "root + the big file only");
        let root = out[0].rect;
        let inner = Rect::from_min_max(pos2(root.min.x, root.min.y + header(3.0)), root.max);
        let covered: f32 = out[1..].iter().map(|x| x.rect.area()).sum();
        assert!((covered - inner.area()).abs() < 1e-2, "{covered} vs {}", inner.area());
    }

    /// A remainder too small to outline as a block is given room for its gutter: it stays visible
    /// (in its own outlined block), and the big file keeps its own outline instead of being merged.
    #[test]
    fn a_tiny_remainder_still_gets_an_outlined_block() {
        let mut t = Tree::new("R");
        t.apply(crate::tree::Batch { id: 0, files: vec![("big".into(), 1_000_000), ("tiny".into(), 1_500)], dirs: vec![] });
        let p = 3.0;
        let mut out = vec![];
        build(&t, 0, Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0)), &HashSet::new(), p, &[], &mut out);
        assert_eq!(out.len(), 3, "root + big + tiny");
        let (big, tiny) = (&out[1], &out[2]);
        assert!(!big.solid && tiny.solid);
        // One dot of gutter between them, on the tiny file's top or left.
        let gap = (tiny.rect.min.y - big.rect.max.y).max(tiny.rect.min.x - big.rect.max.x);
        assert!((gap - p).abs() < 0.01, "gap {gap}");
    }

    /// Files too small for their own gutter sit together in one block, and that block has a whole
    /// top row and left column of gutter (nothing covers it: the folder's dark body shows).
    #[test]
    fn small_files_share_one_outlined_block() {
        let mut t = Tree::new("R");
        let mut files: Vec<(Box<str>, u64)> = (0..12).map(|i| (format!("big{i}").into(), 100_000 - i * 5000)).collect();
        files.extend((0..300).map(|i| (format!("f{i}").into(), 200 + i)));
        t.apply(crate::tree::Batch { id: 0, files, dirs: vec![] });
        let p = 3.0;
        let mut out = vec![];
        build(&t, 0, Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0)), &HashSet::new(), p, &[], &mut out);
        let small: Vec<Rect> = out[1..].iter().filter(|x| x.solid).map(|x| x.rect).collect();
        // Everything outside the block has room for its own gutter.
        assert!(out[1..].iter().all(|x| x.solid || x.rect.width().min(x.rect.height()) >= MIN_GUTTER * p - 0.01));
        assert!(small.len() > 50, "the test needs many small files");
        let b = small.iter().fold(Rect::NOTHING, |a, r| a.union(*r));
        let gutter = [Rect::from_min_max(b.min - vec2(p, p), pos2(b.max.x, b.min.y)), Rect::from_min_max(b.min - vec2(p, p), pos2(b.min.x, b.max.y))];
        for x in &out[1..] {
            let r = x.rect.shrink(0.01);
            assert!(!small.contains(&x.rect) || b.contains_rect(x.rect));
            assert!(small.contains(&x.rect) || !r.intersects(b), "a big tile inside the block: {:?}", x.rect);
            assert!(gutter.iter().all(|g| !r.intersects(*g)), "{:?} covers the block's gutter", x.rect);
        }
    }
}
