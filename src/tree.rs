//! Directory tree and parallel scanner: worker threads stream one `Batch` per dir to the UI thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

pub const NCAT: usize = 9;

/// (name, sRGB color): deep, saturated shades (Tailwind 600-ish) so the field reads dark by hue.
pub const CATS: [(&str, [u8; 3]); NCAT] = [
    ("Apps", [37, 99, 235]),
    ("Developer", [22, 163, 74]),
    ("Photos", [219, 39, 119]),
    ("Videos", [147, 51, 234]),
    ("Documents", [8, 145, 178]),
    ("Music", [202, 138, 4]),
    ("Other", [107, 114, 128]),
    ("Archives", [234, 88, 12]),
    ("System", [220, 38, 38]),
];

pub fn category(name: &str) -> u8 {
    let Some(dot) = name.rfind('.') else { return 6 };
    let ext = &name[dot + 1..];
    if ext.len() > 8 {
        return 6;
    }
    let mut buf = [0u8; 8];
    for (b, c) in buf.iter_mut().zip(ext.bytes()) {
        *b = c.to_ascii_lowercase();
    }
    match &buf[..ext.len()] {
        b"exe" | b"dll" | b"msi" | b"msix" | b"appx" | b"apk" | b"jar" | b"pak" | b"vpk" | b"asar" | b"ocx" | b"node" | b"so" | b"dylib" | b"bundle" => 0,
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"cc" | b"cs" | b"go" | b"py" | b"pyc" | b"js" | b"mjs" | b"ts" | b"tsx" | b"jsx" | b"java" | b"class"
        | b"kt" | b"swift" | b"rlib" | b"rmeta" | b"o" | b"obj" | b"a" | b"lib" | b"pdb" | b"ilk" | b"json" | b"toml" | b"yaml" | b"yml" | b"xml" | b"lock"
        | b"wasm" | b"d" | b"map" | b"sh" | b"ps1" | b"bat" | b"lua" | b"css" | b"html" | b"vue" | b"pack" | b"idx" => 1,
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"bmp" | b"webp" | b"heic" | b"tif" | b"tiff" | b"psd" | b"raw" | b"cr2" | b"nef" | b"arw" | b"dng" | b"svg"
        | b"ico" | b"avif" => 2,
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"flv" | b"webm" | b"m4v" | b"ts2" | b"mts" | b"m2ts" | b"vob" | b"prproj" | b"aep" => 3,
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"txt" | b"md" | b"rtf" | b"odt" | b"epub" | b"csv" | b"chm" => 4,
        b"mp3" | b"flac" | b"wav" | b"aac" | b"ogg" | b"m4a" | b"wma" | b"opus" | b"ape" => 5,
        b"zip" | b"7z" | b"rar" | b"tar" | b"gz" | b"xz" | b"zst" | b"bz2" | b"iso" | b"img" | b"cab" | b"cache" | b"vhd" | b"vhdx" | b"vmdk" | b"vdi"
        | b"ova" | b"qcow2" | b"wim" | b"esd" | b"sav" => 7,
        b"sys" | b"dat" | b"log" | b"tmp" | b"etl" | b"evtx" | b"db" | b"sqlite" | b"ldb" | b"mui" | b"cat" | b"manifest" | b"ini" | b"bak" | b"old"
        | b"blf" | b"regtrans-ms" | b"edb" | b"pf" | b"dmp" | b"bin" => 8,
        _ => 6,
    }
}

/// 16 bytes; the name lives in the dir's `names` (one allocation per dir: ~15 MB less on 500k files).
pub struct File {
    pub size: u64,
    name_at: u32,
    name_len: u16,
    pub cat: u8,
}

#[derive(Default)]
pub struct Dir {
    pub name: Box<str>,
    pub parent: u32,
    pub size: u64,
    pub nfiles: u64,
    pub dirs: Vec<u32>,
    pub files: Vec<File>,
    /// Names of `files`, back to back.
    names: String,
    /// Gone from the tree (deleted, or a slot whose listing never came): its batches are dropped.
    pub dead: bool,
}

impl Dir {
    pub fn file_name(&self, f: u32) -> &str {
        let x = &self.files[f as usize];
        &self.names[x.name_at as usize..][..x.name_len as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Item {
    Dir(u32),
    File(u32, u32),
}

/// Item-keyed map with FxHash: layouts hash tens of thousands of our own indices, no DoS concern.
pub type ItemMap<V> = std::collections::HashMap<Item, V, std::hash::BuildHasherDefault<Fx>>;

#[derive(Default)]
pub struct Fx(u64);

impl Fx {
    fn add(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        bytes.iter().for_each(|&b| self.add(b as u64));
    }
    fn write_u32(&mut self, x: u32) {
        self.add(x as u64);
    }
    fn write_u64(&mut self, x: u64) {
        self.add(x);
    }
    fn write_usize(&mut self, x: usize) {
        self.add(x as u64);
    }
}

pub struct Tree {
    pub dirs: Vec<Dir>,
    /// Bumped on every mutation; drives relayout.
    pub version: u64,
}

impl Tree {
    pub fn new(root: &str) -> Self {
        let root = Dir { name: root.into(), ..Default::default() };
        Self { dirs: vec![root], version: 0 }
    }

    pub fn path(&self, item: Item) -> PathBuf {
        let (mut d, file) = match item {
            Item::Dir(d) => (d, None),
            Item::File(d, f) => (d, Some(self.dirs[d as usize].file_name(f))),
        };
        let mut parts = vec![];
        loop {
            parts.push(&*self.dirs[d as usize].name);
            if d == 0 {
                break;
            }
            d = self.dirs[d as usize].parent;
        }
        let mut p: PathBuf = parts.iter().rev().collect();
        if let Some(f) = file {
            p.push(f);
        }
        p
    }

    pub fn size(&self, item: Item) -> u64 {
        match item {
            Item::Dir(d) => self.dirs[d as usize].size,
            Item::File(d, f) => self.dirs[d as usize].files[f as usize].size,
        }
    }

    /// Install a dir's listing over what it held. Subdirs already in the tree keep their contents;
    /// those no longer listed go. False if the dir itself is gone.
    pub fn apply(&mut self, b: Batch) -> bool {
        if self.dirs.get(b.id as usize).is_none_or(|d| d.dead) {
            return false;
        }
        let max = b.dirs.iter().map(|d| d.0).max().unwrap_or(0) as usize;
        if max >= self.dirs.len() {
            self.dirs.resize_with(max + 1, || Dir { dead: true, ..Default::default() });
        }
        let d = &self.dirs[b.id as usize];
        let old: u64 = d.files.iter().map(|f| f.size).sum();
        let mut sz = b.files.iter().map(|f| f.size).sum::<u64>().wrapping_sub(old);
        let mut n = (b.files.len() as u64).wrapping_sub(d.files.len() as u64);
        let before = std::mem::take(&mut self.dirs[b.id as usize].dirs);
        let mut ids = Vec::with_capacity(b.dirs.len());
        for (cid, name) in b.dirs {
            // New ids are fresh (dead) slots; a live one is a subdir it already had (see `known`).
            if self.dirs[cid as usize].dead {
                self.dirs[cid as usize] = Dir { name, parent: b.id, ..Default::default() };
            }
            ids.push(cid);
        }
        if !before.is_empty() {
            let mut kept = ids.clone();
            kept.sort_unstable();
            for c in before.into_iter().filter(|c| kept.binary_search(c).is_err()) {
                let x = &self.dirs[c as usize];
                (sz, n) = (sz.wrapping_sub(x.size), n.wrapping_sub(x.nfiles));
                self.bury(c);
            }
        }
        let d = &mut self.dirs[b.id as usize];
        d.files = b.files;
        d.names = b.names;
        d.dirs = ids;
        self.add_up(b.id, sz, n);
        self.version += 1;
        true
    }

    /// Its subdirs by name, sorted: what a relisting of `d` keeps (see [`Job`]).
    pub fn known(&self, d: u32) -> Vec<(Box<str>, u32)> {
        let mut k: Vec<_> = self.dirs[d as usize].dirs.iter().map(|&c| (self.dirs[c as usize].name.clone(), c)).collect();
        k.sort_unstable();
        k
    }

    /// The dir at `rel` (components below the root), or its deepest ancestor in the tree.
    pub fn find(&self, rel: &Path) -> u32 {
        let mut d = 0;
        for c in rel.components() {
            let Some(c) = c.as_os_str().to_str() else { break };
            let kids = &self.dirs[d as usize].dirs;
            let Some(&k) = kids.iter().find(|&&k| self.dirs[k as usize].name.eq_ignore_ascii_case(c)) else { break };
            d = k;
        }
        d
    }

    /// Mark `d` and all below it dead and free their contents (their slots stay, empty).
    fn bury(&mut self, d: u32) {
        let mut stack = vec![d];
        while let Some(d) = stack.pop() {
            let x = &mut self.dirs[d as usize];
            x.dead = true;
            stack.append(&mut x.dirs);
            x.dirs = vec![];
            x.files = vec![];
            x.names = String::new();
        }
    }

    /// Add `sz` bytes and `nfiles` files on `d` and all its ancestors (wrapping: pass `0 - n` to
    /// take `n` away).
    fn add_up(&mut self, mut d: u32, sz: u64, nfiles: u64) {
        loop {
            let x = &mut self.dirs[d as usize];
            x.size = x.size.wrapping_add(sz);
            x.nfiles = x.nfiles.wrapping_add(nfiles);
            if d == 0 {
                break;
            }
            d = x.parent;
        }
    }

    /// Empty a directory before rescanning it.
    // shortcut: dead slots are never reused (~120 bytes each, contents freed); compact if long
    // sessions on churning trees make that add up.
    pub fn clear_dir(&mut self, d: u32) {
        let x = &self.dirs[d as usize];
        self.add_up(d, x.size.wrapping_neg(), x.nfiles.wrapping_neg());
        std::mem::take(&mut self.dirs[d as usize].dirs).into_iter().for_each(|c| self.bury(c));
        let x = &mut self.dirs[d as usize];
        x.files.clear();
        x.names.clear();
        self.version += 1;
    }

    pub fn remove(&mut self, item: Item) {
        match item {
            Item::Dir(d) => {
                if d == 0 {
                    return;
                }
                let x = &self.dirs[d as usize];
                let (sz, n, p) = (x.size, x.nfiles, x.parent);
                self.add_up(p, sz.wrapping_neg(), n.wrapping_neg());
                self.dirs[p as usize].dirs.retain(|&c| c != d);
                self.bury(d);
            }
            Item::File(d, f) => {
                let file = self.dirs[d as usize].files.swap_remove(f as usize);
                self.add_up(d, file.size.wrapping_neg(), u64::MAX);
            }
        }
        self.version += 1;
    }
}

pub struct Batch {
    pub id: u32,
    pub files: Vec<File>,
    names: String,
    pub dirs: Vec<(u32, Box<str>)>,
}

impl Batch {
    pub fn new(id: u32) -> Self {
        Self { id, files: vec![], names: String::new(), dirs: vec![] }
    }

    pub fn push_file(&mut self, name: &str, size: u64) {
        let (name_at, name_len) = (self.names.len() as u32, name.len() as u16); // NTFS names: <= 765 UTF-8 bytes
        self.files.push(File { size, name_at, name_len, cat: category(name) });
        self.names.push_str(name);
    }

    #[cfg(test)]
    pub fn of(id: u32, files: impl IntoIterator<Item = (Box<str>, u64)>, dirs: Vec<(u32, Box<str>)>) -> Self {
        let mut b = Self { dirs, ..Self::new(id) };
        files.into_iter().for_each(|(n, s)| b.push_file(&n, s));
        b
    }
}

/// A running scan: one batch per listed dir, then the channel closes (every worker is done).
pub struct Scan {
    pub rx: Receiver<Batch>,
    pub cancel: Arc<AtomicBool>,
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A dir to list: its slot, path, and the subdirs it already has (by name, sorted). Those keep their
/// slot and are not listed again; any other subdir is new and scanned whole.
pub type Job = (u32, PathBuf, Vec<(Box<str>, u32)>);

/// List `jobs`; new dirs get ids from `next` upward.
pub fn start(jobs: Vec<Job>, next: u32) -> Scan {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        let next = AtomicU32::new(next);
        // Work stack of dirs still to list + count of dirs being listed right now.
        let work = Mutex::new((jobs, 0usize));
        let cv = Condvar::new();
        // Beyond about half the cores the threads only contend on file system locks: same speed
        // (warm cache, C:\ on 20 cores), several times the CPU.
        let threads = std::thread::available_parallelism().map_or(8, |n| n.get() / 2).clamp(4, 16);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    crate::win::below_normal();
                    worker(&work, &cv, &tx, &next, &c)
                });
            }
        });
    });
    Scan { rx, cancel }
}

type Work = Mutex<(Vec<Job>, usize)>;

fn worker(work: &Work, cv: &Condvar, tx: &Sender<Batch>, next: &AtomicU32, cancel: &AtomicBool) {
    loop {
        let mut w = work.lock().unwrap();
        let job = loop {
            if let Some(job) = w.0.pop() {
                w.1 += 1;
                break job;
            }
            if w.1 == 0 {
                return; // nothing queued and nobody listing: the walk is complete
            }
            w = cv.wait(w).unwrap();
        };
        drop(w);
        let sub = list(job, tx, next, cancel);
        let mut w = work.lock().unwrap();
        w.1 -= 1;
        let n = sub.len();
        w.0.extend(sub);
        if w.0.is_empty() && w.1 == 0 {
            cv.notify_all(); // done: release every waiting worker
        } else {
            // Wake only as many workers as there is new work (no thundering herd on the lock).
            (0..n).for_each(|_| cv.notify_one());
        }
    }
}

// FILE_ATTRIBUTE_*: not stored on this disk.
const OFFLINE: u32 = 0x1000;
const RECALL_ON_OPEN: u32 = 0x4_0000;
const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

/// List one directory, send its batch and return its subdirectories (empty once cancelled).
fn list((id, path, known): Job, tx: &Sender<Batch>, next: &AtomicU32, cancel: &AtomicBool) -> Vec<Job> {
    if cancel.load(Ordering::Relaxed) {
        return vec![];
    }
    let mut b = Batch::new(id);
    let mut sub = vec![];
    if let Ok(rd) = std::fs::read_dir(&path) {
        for e in rd.flatten() {
            // On Windows DirEntry::file_type/metadata come from FindNextFile data: no extra syscalls.
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue; // symlinks + junctions: avoid double counting and cycles
            }
            // Cloud files (OneDrive Files On Demand) count only what is on this disk, and listing an online
            // folder would fetch it. The recall bits show even though Windows disguises placeholders from us.
            let Ok(m) = e.metadata() else { continue };
            if std::os::windows::fs::MetadataExt::file_attributes(&m) & (RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS | OFFLINE) != 0 {
                continue;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if ft.is_dir() {
                let cid = match known.binary_search_by(|k| (*k.0).cmp(&name)) {
                    Ok(i) => known[i].1,
                    Err(_) => {
                        let cid = next.fetch_add(1, Ordering::Relaxed);
                        sub.push((cid, e.path(), vec![]));
                        cid
                    }
                };
                b.dirs.push((cid, name.into()));
            } else {
                b.push_file(&name, m.len());
            }
        }
    }
    // Kept for the life of the tree: drop the growth slack (up to half of each buffer).
    b.files.shrink_to_fit();
    b.names.shrink_to_fit();
    // Parent batch must be sent before any child batch (children reference the parent's slots).
    if tx.send(b).is_err() {
        return vec![];
    }
    sub
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_counts_a_real_tree() {
        let root = std::env::temp_dir().join(format!("ss-rs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // 3 levels x 4 dirs, 2 files of 10 bytes in every dir (1 + 4 + 16 + 64 = 85 dirs).
        let (mut all, mut level) = (vec![root.clone()], vec![root.clone()]);
        for _ in 0..3 {
            level = level.iter().flat_map(|d| (0..4).map(move |i| d.join(format!("d{i}")))).collect();
            all.extend(level.iter().cloned());
        }
        for d in &all {
            std::fs::create_dir_all(d).unwrap();
            for f in ["a.txt", "b.bin"] {
                std::fs::write(d.join(f), [0u8; 10]).unwrap();
            }
        }
        let scan = start(vec![(0, root.clone(), vec![])], 1);
        let mut t = Tree::new(root.to_str().unwrap());
        while let Ok(b) = scan.rx.recv() {
            t.apply(b);
        }
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(t.dirs[0].nfiles, 85 * 2);
        assert_eq!(t.dirs[0].size, 85 * 2 * 10);
        assert_eq!(t.dirs.len(), 85);
    }

    #[test]
    fn sizes_stay_consistent_through_delete_and_clear() {
        let mut t = Tree::new("R");
        let batch = |id, files: &[(&str, u64)], dirs: &[(u32, &str)]| {
            let mut b = Batch::new(id);
            files.iter().for_each(|&(n, s)| b.push_file(n, s));
            b.dirs = dirs.iter().map(|&(c, n)| (c, n.into())).collect();
            b
        };
        t.apply(batch(0, &[("a", 10), ("b.jpg", 20)], &[(1, "s"), (2, "u")]));
        t.apply(batch(1, &[("c", 100), ("d", 200)], &[]));
        t.apply(batch(2, &[("e", 1000)], &[]));
        assert_eq!((t.dirs[0].file_name(1), t.dirs[0].files[1].cat), ("b.jpg", category("b.jpg")));
        let root = |t: &Tree| (t.dirs[0].size, t.dirs[0].nfiles);
        assert_eq!(root(&t), (1330, 5));
        // The path the Recycle Bin gets: root first, every level in order.
        assert_eq!(t.path(Item::File(1, 1)), PathBuf::from("R/s/d"));
        t.remove(Item::File(1, 0));
        assert_eq!((root(&t), t.dirs[1].size, t.dirs[1].file_name(0)), ((1230, 4), 200, "d"));
        t.remove(Item::Dir(2));
        assert_eq!((root(&t), t.dirs[0].dirs.clone()), ((230, 3), vec![1]));
        t.clear_dir(1);
        assert_eq!((root(&t), t.dirs[1].size), ((30, 2), 0));
        t.apply(batch(1, &[("f", 5)], &[])); // its rescan
        assert_eq!(t.dirs[1].file_name(0), "f");
        assert_eq!(root(&t), (35, 3));
    }

    #[test]
    fn relisting_keeps_known_dirs_and_drops_gone_ones() {
        let mut t = Tree::new("R");
        t.apply(Batch::of(0, [("a".into(), 10)], vec![(1, "s".into()), (2, "u".into())]));
        t.apply(Batch::of(1, [("b".into(), 100)], vec![]));
        t.apply(Batch::of(2, [("c".into(), 1000)], vec![(3, "deep".into())]));
        t.apply(Batch::of(3, [("d".into(), 5)], vec![]));
        assert_eq!((t.dirs[0].size, t.dirs[0].nfiles), (1115, 4));
        assert_eq!(t.find(Path::new("U/deep")), 3);
        assert_eq!(t.find(Path::new("u/nope/x")), 2);
        assert_eq!(t.known(0), vec![("s".into(), 1), ("u".into(), 2)]);
        // Relisted: a grew, z is new, s kept (not relisted), u gone, n new.
        t.apply(Batch::of(0, [("a".into(), 12), ("z".into(), 1)], vec![(1, "s".into()), (4, "n".into())]));
        assert_eq!((t.dirs[0].size, t.dirs[0].nfiles), (113, 3));
        assert_eq!((t.dirs[1].size, t.dirs[2].dead, t.dirs[3].dead, t.dirs[3].files.len()), (100, true, true, 0));
        assert!(t.apply(Batch::of(4, [("e".into(), 7)], vec![])));
        assert!(!t.apply(Batch::of(3, [("late".into(), 9)], vec![]))); // its dir is gone
        assert_eq!((t.dirs[0].size, t.dirs[0].nfiles, t.dirs[0].dirs.clone()), (120, 4, vec![1, 4]));
    }

    /// `cargo test --release scan_bench -- --ignored --nocapture`: time a full scan of C:\.
    #[test]
    #[ignore]
    fn scan_bench() {
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            let scan = start(vec![(0, PathBuf::from("C:\\"), vec![])], 1);
            let mut n = 0;
            while scan.rx.recv().is_ok() {
                n += 1;
            }
            eprintln!("{n} dirs in {:?}", t0.elapsed());
        }
    }
}
