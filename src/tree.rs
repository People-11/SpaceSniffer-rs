//! Directory tree + incremental parallel scanner.
//!
//! Worker threads walk the disk and stream one `Batch` per directory to the UI thread,
//! which merges it into the arena. Every file/dir remembers when it was discovered so the
//! renderer can flash new tiles white-hot and let them cool down to their category color.

use std::path::PathBuf;
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
        b"exe" | b"dll" | b"msi" | b"msix" | b"appx" | b"apk" | b"jar" | b"pak" | b"vpk" | b"asar"
        | b"ocx" | b"node" | b"so" | b"dylib" | b"bundle" => 0,
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"cc" | b"cs" | b"go" | b"py" | b"pyc" | b"js"
        | b"mjs" | b"ts" | b"tsx" | b"jsx" | b"java" | b"class" | b"kt" | b"swift" | b"rlib"
        | b"rmeta" | b"o" | b"obj" | b"a" | b"lib" | b"pdb" | b"ilk" | b"json" | b"toml"
        | b"yaml" | b"yml" | b"xml" | b"lock" | b"wasm" | b"d" | b"map" | b"sh" | b"ps1"
        | b"bat" | b"lua" | b"css" | b"html" | b"vue" | b"pack" | b"idx" => 1,
        b"jpg" | b"jpeg" | b"png" | b"gif" | b"bmp" | b"webp" | b"heic" | b"tif" | b"tiff"
        | b"psd" | b"raw" | b"cr2" | b"nef" | b"arw" | b"dng" | b"svg" | b"ico" | b"avif" => 2,
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"flv" | b"webm" | b"m4v" | b"ts2" | b"mts"
        | b"m2ts" | b"vob" | b"prproj" | b"aep" => 3,
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"txt" | b"md"
        | b"rtf" | b"odt" | b"epub" | b"csv" | b"chm" => 4,
        b"mp3" | b"flac" | b"wav" | b"aac" | b"ogg" | b"m4a" | b"wma" | b"opus" | b"ape" => 5,
        b"zip" | b"7z" | b"rar" | b"tar" | b"gz" | b"xz" | b"zst" | b"bz2" | b"iso" | b"img"
        | b"cab" | b"cache" | b"vhd" | b"vhdx" | b"vmdk" | b"vdi" | b"ova" | b"qcow2" | b"wim"
        | b"esd" | b"sav" => 7,
        b"sys" | b"dat" | b"log" | b"tmp" | b"etl" | b"evtx" | b"db" | b"sqlite" | b"ldb"
        | b"mui" | b"cat" | b"manifest" | b"ini" | b"bak" | b"old" | b"blf" | b"regtrans-ms"
        | b"edb" | b"pf" | b"dmp" | b"bin" => 8,
        _ => 6,
    }
}

pub struct File {
    pub name: Box<str>,
    pub size: u64,
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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Item {
    Dir(u32),
    File(u32, u32),
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
            Item::File(d, f) => (d, Some(&*self.dirs[d as usize].files[f as usize].name)),
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

    pub fn apply(&mut self, b: Batch) {
        let files: Vec<File> = b.files.into_iter().map(|(name, size)| File { cat: category(&name), name, size }).collect();
        let (sz, n) = (files.iter().map(|f| f.size).sum(), files.len() as u64);
        let max = b.dirs.iter().map(|d| d.0).max().unwrap_or(0) as usize;
        if max >= self.dirs.len() {
            self.dirs.resize_with(max + 1, Dir::default);
        }
        let mut ids = Vec::with_capacity(b.dirs.len());
        for (cid, name) in b.dirs {
            self.dirs[cid as usize] = Dir { name, parent: b.id, ..Default::default() };
            ids.push(cid);
        }
        let d = &mut self.dirs[b.id as usize];
        d.files = files;
        d.dirs = ids;
        self.add_up(b.id, sz, n, true);
        self.version += 1;
    }

    /// Add (or subtract) `sz` bytes and `nfiles` files on `d` and all its ancestors.
    fn add_up(&mut self, mut d: u32, sz: u64, nfiles: u64, add: bool) {
        loop {
            let x = &mut self.dirs[d as usize];
            if add {
                x.size += sz;
                x.nfiles += nfiles;
            } else {
                x.size -= sz;
                x.nfiles -= nfiles;
            }
            if d == 0 {
                break;
            }
            d = x.parent;
        }
    }

    /// Empty a directory before rescanning it (old descendant slots are simply orphaned).
    pub fn clear_dir(&mut self, d: u32) {
        let x = &self.dirs[d as usize];
        self.add_up(d, x.size, x.nfiles, false);
        let x = &mut self.dirs[d as usize];
        x.dirs.clear();
        x.files.clear();
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
                self.add_up(p, sz, n, false);
                self.dirs[p as usize].dirs.retain(|&c| c != d);
            }
            Item::File(d, f) => {
                let file = self.dirs[d as usize].files.swap_remove(f as usize);
                self.add_up(d, file.size, 1, false);
            }
        }
        self.version += 1;
    }
}

pub struct Batch {
    pub id: u32,
    pub files: Vec<(Box<str>, u64)>,
    pub dirs: Vec<(u32, Box<str>)>,
}

pub enum Msg {
    Batch(Batch),
    Done,
}

pub struct Scan {
    pub rx: Receiver<Msg>,
    pub cancel: Arc<AtomicBool>,
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Scan `root` into dir slot `id`; new dirs get ids from `next` upward.
pub fn start(root: PathBuf, id: u32, next: u32) -> Scan {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    std::thread::spawn(move || {
        let next = AtomicU32::new(next);
        // Work stack of dirs still to list + count of dirs being listed right now.
        let work = Mutex::new((vec![(id, root)], 0usize));
        let cv = Condvar::new();
        // Directory listing is IO-latency bound: oversubscribe the cores.
        let threads = std::thread::available_parallelism().map_or(8, |n| n.get() * 2).max(8);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| worker(&work, &cv, &tx, &next, &c));
            }
        });
        let _ = tx.send(Msg::Done);
    });
    Scan { rx, cancel }
}

type Work = Mutex<(Vec<(u32, PathBuf)>, usize)>;

fn worker(work: &Work, cv: &Condvar, tx: &Sender<Msg>, next: &AtomicU32, cancel: &AtomicBool) {
    loop {
        let mut w = work.lock().unwrap();
        let (id, path) = loop {
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
        let sub = list(path, id, tx, next, cancel);
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

/// List one directory, send its batch and return its subdirectories (empty once cancelled).
fn list(path: PathBuf, id: u32, tx: &Sender<Msg>, next: &AtomicU32, cancel: &AtomicBool) -> Vec<(u32, PathBuf)> {
    if cancel.load(Ordering::Relaxed) {
        return vec![];
    }
    let mut b = Batch { id, files: vec![], dirs: vec![] };
    let mut sub = vec![];
    if let Ok(rd) = std::fs::read_dir(&path) {
        for e in rd.flatten() {
            // On Windows DirEntry::file_type/metadata come from FindNextFile data: no extra syscalls.
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue; // symlinks + junctions: avoid double counting and cycles
            }
            let name: Box<str> = e.file_name().to_string_lossy().into();
            if ft.is_dir() {
                let cid = next.fetch_add(1, Ordering::Relaxed);
                sub.push((cid, e.path()));
                b.dirs.push((cid, name));
            } else {
                let size = e.metadata().map_or(0, |m| m.len());
                b.files.push((name, size));
            }
        }
    }
    // Parent batch must be sent before any child batch (children reference the parent's slots).
    if tx.send(Msg::Batch(b)).is_err() {
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
        let scan = start(root.clone(), 0, 1);
        let mut t = Tree::new(root.to_str().unwrap());
        while let Ok(Msg::Batch(b)) = scan.rx.recv() {
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
        t.apply(Batch { id: 0, files: vec![("a".into(), 10), ("b".into(), 20)], dirs: vec![(1, "s".into()), (2, "u".into())] });
        t.apply(Batch { id: 1, files: vec![("c".into(), 100), ("d".into(), 200)], dirs: vec![] });
        t.apply(Batch { id: 2, files: vec![("e".into(), 1000)], dirs: vec![] });
        let root = |t: &Tree| (t.dirs[0].size, t.dirs[0].nfiles);
        assert_eq!(root(&t), (1330, 5));
        t.remove(Item::File(1, 0));
        assert_eq!((root(&t), t.dirs[1].size, &*t.dirs[1].files[0].name), ((1230, 4), 200, "d"));
        t.remove(Item::Dir(2));
        assert_eq!((root(&t), t.dirs[0].dirs.clone()), ((230, 3), vec![1]));
        t.clear_dir(1);
        assert_eq!((root(&t), t.dirs[1].size), ((30, 2), 0));
        t.apply(Batch { id: 1, files: vec![("f".into(), 5)], dirs: vec![] }); // its rescan
        assert_eq!(root(&t), (35, 3));
    }

    /// `cargo test --release scan_bench -- --ignored --nocapture`: time a full scan of C:\.
    #[test]
    #[ignore]
    fn scan_bench() {
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            let scan = start(PathBuf::from("C:\\"), 0, 1);
            let mut n = 0;
            while let Ok(Msg::Batch(_)) = scan.rx.recv() {
                n += 1;
            }
            eprintln!("{n} dirs in {:?}", t0.elapsed());
        }
    }
}
