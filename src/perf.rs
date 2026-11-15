//! Frame timing printed to the console, only in `--features perf` builds (no-ops otherwise).
//! Every second: frames and avg / max per section; any frame whose work takes longer than
//! SLOW_MS: its own breakdown.

#[cfg(feature = "perf")]
mod imp {
    use std::cell::RefCell;
    use std::time::Instant;

    const SLOW_MS: f64 = 4.0;

    #[derive(Default)]
    struct State {
        /// This frame: (section, ms or count).
        frame: Vec<(&'static str, f64)>,
        /// This second: (section, sum, max, n).
        acc: Vec<(&'static str, f64, f64, u32)>,
        frames: u32,
        since: Option<Instant>,
    }

    thread_local! { static S: RefCell<State> = RefCell::default(); }

    pub fn add(name: &'static str, v: f64) {
        S.with_borrow_mut(|s| match s.frame.iter_mut().find(|x| x.0 == name) {
            Some(x) => x.1 += v,
            None => s.frame.push((name, v)),
        });
    }

    pub struct Span(&'static str, Instant);

    impl Drop for Span {
        fn drop(&mut self) {
            add(self.0, self.1.elapsed().as_secs_f64() * 1e3);
        }
    }

    pub fn span(name: &'static str) -> Span {
        Span(name, Instant::now())
    }

    fn line(xs: impl Iterator<Item = String>) -> String {
        xs.collect::<Vec<_>>().join(" | ")
    }

    /// End of a frame whose own work took `work` ms.
    pub fn frame(work: f64) {
        S.with_borrow_mut(|s| {
            let now = Instant::now();
            let since = *s.since.get_or_insert(now);
            s.frames += 1;
            let frame = std::mem::take(&mut s.frame);
            if work > SLOW_MS {
                eprintln!("  slow {work:6.2}ms  {}", line(frame.iter().map(|(n, v)| fmt(n, *v))));
            }
            for (n, v) in std::iter::once(("work", work)).chain(frame) {
                match s.acc.iter_mut().find(|x| x.0 == n) {
                    Some(x) => {
                        x.1 += v;
                        x.2 = x.2.max(v);
                        x.3 += 1;
                    }
                    None => s.acc.push((n, v, v, 1)),
                }
            }
            if now.duration_since(since).as_secs_f64() >= 1.0 {
                let acc = std::mem::take(&mut s.acc);
                eprintln!(
                    "{:3} fps  {}",
                    s.frames,
                    line(acc.iter().map(|(n, sum, max, k)| {
                        if is_count(n) { format!("{n} {:.0}/{max:.0}", sum / *k as f64) } else { format!("{n} {:.2}/{max:.2}", sum / *k as f64) }
                    }))
                );
                s.frames = 0;
                s.since = Some(now);
            }
        });
    }

    /// Sections named `#…` are counts, not times.
    fn is_count(n: &str) -> bool {
        n.starts_with('#')
    }

    fn fmt(n: &str, v: f64) -> String {
        if is_count(n) { format!("{n} {v:.0}") } else { format!("{n} {v:.2}") }
    }
}

#[cfg(not(feature = "perf"))]
mod imp {
    pub struct Span;
    impl Drop for Span {
        fn drop(&mut self) {} // so ending a span early with drop() reads the same in both builds
    }
    #[inline(always)]
    pub fn span(_: &'static str) -> Span {
        Span
    }
    #[inline(always)]
    pub fn add(_: &'static str, _: f64) {}
    #[inline(always)]
    pub fn frame(_: f64) {}
}

/// Add `v` to this frame's `name` (a time in ms, or a count if `name` starts with '#').
pub use imp::add;
/// End of the frame; `work` ms of it were ours.
pub use imp::frame;
/// Time the rest of the scope as `name` (summed if used more than once a frame).
pub use imp::span;
