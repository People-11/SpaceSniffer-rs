//! Nested squarified treemap, SpaceSniffer style: every directory gets a header strip
//! ("name - size") and its children are laid out inside.

use crate::tree::{Item, ItemMap, Tree};
use eframe::egui::{Rect, pos2, vec2};
use std::collections::HashSet;

/// Dot pitch in points; must match `cs` in shader.wgsl (2pt rounded down to whole pixels).
pub fn grid(ppp: f32) -> f32 {
    (2.0 * ppp).floor().max(2.0) / ppp
}

/// Folder title strip height: ~15pt, a whole number of dot rows.
pub fn header(pitch: f32) -> f32 {
    (15.0 / pitch).round().max(1.0) * pitch
}

/// Snap a rect onto the dot lattice: whole dots, no hairline gaps between neighbours.
pub fn snap(r: Rect, pitch: f32) -> Rect {
    let s = |v: f32| (v / pitch).round() * pitch;
    Rect::from_min_max(pos2(s(r.min.x), s(r.min.y)), pos2(s(r.max.x), s(r.max.y)))
}

#[derive(Clone)]
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
    /// A file's category (see `tree::CATS`), kept here so drawing never looks the file up.
    pub cat: u8,
}

/// A folder gets a title strip (and children) only if its name fits; otherwise it is a plain field.
fn titled(name: &str, r: Rect, header: f32) -> bool {
    // shortcut: estimated width of the 11.5pt bold title font; measure with egui if titles get cut.
    let w: f32 = name.chars().map(|c| if c.is_ascii() { 6.6 } else { 12.0 }).sum();
    r.width() >= w + 10.0 && r.height() >= 2.0 * header
}

/// Levels below the view root that are open by default; deeper dirs open only when expanded.
const DEFAULT_DEPTH: u16 = 2;

/// `expanded`: dirs the user clicked open (only themselves: their subdirs stay closed).
/// `prev`: the previous layout, whose tones are kept where possible.
pub fn build(tree: &Tree, root: u32, area: Rect, expanded: &HashSet<u32>, pitch: f32, prev: &[Tile], out: &mut Vec<Tile>) {
    {
        let _s = crate::perf::span("dir");
        rects(tree, root, area, expanded, pitch, u32::MAX, out);
    }
    let _s = crate::perf::span("tones");
    tones(out, snap(area, pitch), pitch, prev);
}

/// The rects of [`build`] without tones, with dir `stop` left closed (u32::MAX: none).
pub fn rects(tree: &Tree, root: u32, area: Rect, expanded: &HashSet<u32>, pitch: f32, stop: u32, out: &mut Vec<Tile>) {
    out.clear();
    if tree.dirs[root as usize].size > 0 {
        dir(tree, root, snap(area, pitch), 0, expanded, &Grid { pitch, header: header(pitch), stop }, out);
    }
}

/// Number of brightness tones (4 colours always suffice for a map of rectangles in practice).
pub const TONES: u8 = 4;

/// Map colouring of the visible tiles: big ones first, each taking the tone that shares the least
/// edge with same-toned neighbours. Tiles from `prev` keep theirs, so expanding never recolours far
/// blocks; a final pass repairs any clash left.
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
    // Every tile cell just outside tile i (one per dot of shared boundary).
    let edge = |i: usize, f: &mut dyn FnMut(usize)| {
        let (x0, y0, x1, y1) = cells(&out[i]);
        let mut look = |x: isize, y: isize| {
            if (0..cols).contains(&x) && (0..rows).contains(&y) {
                let j = map[(y * cols + x) as usize];
                if j != u32::MAX {
                    f(j as usize);
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
    };
    // Shared boundary length (in dots) with already-coloured neighbours, per neighbour tone.
    let near = |i: usize, tone: &[u8]| {
        let mut w = [0.0f32; TONES as usize];
        edge(i, &mut |j| {
            if tone[j] != u8::MAX {
                w[tone[j] as usize] += 1.0;
            }
        });
        w
    };
    // Same-tone edge first (any clash-free tone wins), then how close the other neighbours' tones are.
    let cost = |w: &[f32; TONES as usize], c: u8| (w[c as usize], (0..TONES).map(|n| w[n as usize] * NEAR[n.abs_diff(c) as usize]).sum::<f32>());
    let best = |w: &[f32; TONES as usize]| (0..TONES).min_by(|&a, &b| cost(w, a).partial_cmp(&cost(w, b)).unwrap()).unwrap();
    // A tile that did not move keeps its tone; one that moved keeps it if that clashes with nothing.
    let mut old: Vec<Option<(u8, Rect)>> = vec![None; out.len()];
    if prev.len() <= order.len() {
        let m: ItemMap<(u8, Rect)> = prev.iter().filter(|t| !t.open).map(|t| (t.item, (t.tone, t.rect))).collect();
        order.iter().for_each(|&i| old[i] = m.get(&out[i].item).copied());
    } else {
        let m: ItemMap<usize> = order.iter().map(|&i| (out[i].item, i)).collect();
        for t in prev.iter().filter(|t| !t.open) {
            if let Some(&i) = m.get(&t.item) {
                old[i] = Some((t.tone, t.rect));
            }
        }
    }
    let mut still = vec![false; out.len()];
    for &i in &order {
        if let Some((c, r)) = old[i]
            && r == out[i].rect
        {
            tone[i] = c;
            still[i] = true;
        }
    }
    for &i in &order {
        if let Some((c, _)) = old[i]
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
    // Local repair until nothing changes; each pass revisits only changed tiles and their neighbours.
    let mut rank = vec![0; out.len()];
    order.iter().enumerate().for_each(|(k, &i)| rank[i] = k);
    let mut work: Vec<usize> = order.iter().copied().filter(|&i| !still[i]).collect();
    for _ in 0..4 {
        let mut next = vec![];
        for &i in &work {
            let w = near(i, &tone);
            if w[tone[i] as usize] == 0.0 {
                continue; // no clash: nothing to repair
            }
            let c = best(&w);
            if cost(&w, c) < cost(&w, tone[i]) {
                tone[i] = c;
                next.push(i);
                edge(i, &mut |j| next.push(j));
            }
        }
        if next.is_empty() {
            break;
        }
        next.retain(|&i| !still[i]);
        next.sort_unstable_by_key(|&i| rank[i]);
        next.dedup();
        work = next;
    }
    for &i in &order {
        out[i].tone = tone[i];
    }
}

struct Grid {
    pitch: f32,
    header: f32,
    /// Dir left closed (see [`rects`]); u32::MAX: none.
    stop: u32,
}

fn dir(tree: &Tree, d: u32, r: Rect, depth: u16, expanded: &HashSet<u32>, g: &Grid, out: &mut Vec<Tile>) {
    let idx = out.len();
    let x = &tree.dirs[d as usize];
    let has_title = depth == 0 || titled(&x.name, r, g.header); // the view root always opens
    out.push(Tile { rect: r, item: Item::Dir(d), open: false, titled: has_title, tone: 0, solid: false, cat: 0 });
    // Children fill everything below the title strip: no padding gutters.
    let inner = Rect::from_min_max(pos2(r.min.x, r.min.y + g.header), r.max);
    // Opens by default only if its name fits; a folder the user clicked opens regardless.
    let wanted = has_title && depth <= DEFAULT_DEPTH || expanded.contains(&d);
    if !wanted || d == g.stop || inner.width() < 12.0 || inner.height() < g.pitch {
        return;
    }
    out[idx].open = true;
    // Drop children under one dot before sorting (a folder may hold a million files), then scale the
    // rest up to fill the body.
    let area = (inner.width() * inner.height()) as f64;
    let total = x.size as f64; // = its children's sizes
    let dot = (g.pitch * g.pitch) as f64;
    let mut kids: Vec<(f64, Item)> = x
        .dirs
        .iter()
        .map(|&c| (tree.dirs[c as usize].size as f64, Item::Dir(c)))
        .chain(x.files.iter().enumerate().map(|(i, f)| (f.size as f64, Item::File(d, i as u32))))
        .filter(|k| k.0 > 0.0 && k.0 * area / total >= dot)
        .collect();
    kids.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    let kept: f64 = kids.iter().map(|k| k.0).sum();
    for k in &mut kids {
        k.0 *= area / kept;
    }
    // Tiles too small for their own gutter are gathered into one block (last: bottom-right) that gets
    // the gutter; whatever beside it comes out too small joins it too.
    let small = |r: Rect| {
        let r = snap(r, g.pitch);
        (r.width().min(r.height()) / g.pitch).round() < MIN_GUTTER
    };
    let mut placed = Vec::new();
    let mut k = kids.len();
    // A block too small for its own gutter gets more room (up to MAX_REST of the body).
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
                    // Still too small: dropped like the sub-dot tiles; the rest fill the body.
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
            Item::File(_, f) => out.push(Tile { rect, item, open: false, titled: false, tone: 0, solid: false, cat: x.files[f as usize].cat }),
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
        let mut b = crate::tree::Batch::new(0);
        [("a", 500), ("b", 300), ("c", 120), ("d", 80)].iter().for_each(|&(n, s)| b.push_file(n, s));
        t.apply(b);
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
        let mut b = crate::tree::Batch::new(0);
        (1..60u64).for_each(|i| b.push_file(&format!("f{i}"), i * i * 1000));
        t.apply(b);
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
        t.apply(Batch::of(0, vec![], vec![(1, "x".into())]));
        t.apply(Batch::of(1, vec![], vec![(2, "A".into())]));
        let files: Vec<(Box<str>, u64)> = (1..10u64).map(|i| (format!("f{i}").into(), i * 3000)).collect();
        t.apply(Batch::of(2, files, vec![(3, "B".into())]));
        let files: Vec<(Box<str>, u64)> = (1..20u64).map(|i| (format!("b{i}").into(), i * 1000)).collect();
        t.apply(Batch::of(3, files, vec![]));
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
        t.apply(crate::tree::Batch::of(0, files, vec![]));
        let mut out = vec![];
        build(&t, 0, Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0)), &HashSet::new(), 3.0, &[], &mut out);
        assert_eq!(out.len(), 2, "root + the big file only");
        let root = out[0].rect;
        let inner = Rect::from_min_max(pos2(root.min.x, root.min.y + header(3.0)), root.max);
        let covered: f32 = out[1..].iter().map(|x| x.rect.area()).sum();
        assert!((covered - inner.area()).abs() < 1e-2, "{covered} vs {}", inner.area());
    }

    /// A remainder too small for a block gets room for its gutter; the big file keeps its own outline.
    #[test]
    fn a_tiny_remainder_still_gets_an_outlined_block() {
        let mut t = Tree::new("R");
        t.apply(crate::tree::Batch::of(0, vec![("big".into(), 1_000_000), ("tiny".into(), 1_500)], vec![]));
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

    /// Files too small for their own gutter share one block with a whole gutter row and column.
    #[test]
    fn small_files_share_one_outlined_block() {
        let mut t = Tree::new("R");
        let mut files: Vec<(Box<str>, u64)> = (0..12).map(|i| (format!("big{i}").into(), 100_000 - i * 5000)).collect();
        files.extend((0..300).map(|i| (format!("f{i}").into(), 200 + i)));
        t.apply(crate::tree::Batch::of(0, files, vec![]));
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
