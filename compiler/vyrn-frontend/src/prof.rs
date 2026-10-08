//! Where a build spends its time, per phase: a flat, additive table in
//! first-seen order. Phases are whole pipeline stages that do not overlap, so
//! a caller and callee split would add nothing.
//!
//! ponytail: a flat table, no file format. The pprof, speedscope or
//! own-format choice is open.
//!
//! `VYRN_BUILD_PROFILE=allocs` adds a table of allocation counts per phase.
//! [`Counting`] wraps the global allocator and bumps thread-local counters, so
//! the hot path touches no shared line. A worker hands its counters to the
//! caller as it hands over its phases.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

/// Formats a duration in units a reader compares by eye.
fn ms(d: Duration) -> String {
    let ns = d.as_nanos();
    if ns < 1_000 {
        format!("{ns} ns")
    } else if ns < 1_000_000 {
        format!("{:.2} µs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.2} ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.3} s", ns as f64 / 1_000_000_000.0)
    }
}

// Build phases: a name, a count and a total per phase, armed by
// `VYRN_BUILD_PROFILE=1` and silent otherwise.

thread_local! {
    /// `(name, total, count)` in first-seen order, the order a build runs them.
    static PHASES: RefCell<Vec<(&'static str, Duration, u64)>> = const { RefCell::new(Vec::new()) };
    static LINES: Cell<u64> = const { Cell::new(0) };
}

/// Adds `n` source lines to the count `phase_table` prints as `lines read`.
/// `scripts/check-speed.sh` divides the check time by it.
pub fn read_lines(n: usize) {
    if phases_on() {
        LINES.with(|l| l.set(l.get() + n as u64));
    }
}

/// Whether build phases are timed: [`arm`] or `VYRN_BUILD_PROFILE`. Read once,
/// so the hot loader path does not pay an env lookup per phase.
pub fn phases_on() -> bool {
    *PHASES_ON.get_or_init(|| std::env::var("VYRN_BUILD_PROFILE").is_ok_and(|v| v != "0"))
}

static PHASES_ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Times build phases from here on, as `VYRN_BUILD_PROFILE=1` does. Call it
/// before the first phase; after one, the answer is already fixed.
pub fn arm() {
    let _ = PHASES_ON.set(true);
}

/// One open phase. It charges its span on drop, so an early `return` still
/// records it.
pub struct Phase(&'static str, Instant, u8);

impl Drop for Phase {
    fn drop(&mut self) {
        TAG.with(|t| t.set(self.2));
        charge(self.0, self.1.elapsed());
    }
}

/// Charges `span` to the phase `name`, for a caller that timed it and already
/// knows it is profiling (`vyrn run --profile`). [`phase`] is the pipeline's
/// hook and obeys `VYRN_BUILD_PROFILE`.
pub fn charge(name: &'static str, span: Duration) {
    add(name, span, 1);
}

fn add(name: &'static str, span: Duration, count: u64) {
    PHASES.with(|p| {
        let mut p = p.borrow_mut();
        match p.iter_mut().find(|(n, _, _)| *n == name) {
            Some(row) => {
                row.1 += span;
                row.2 += count;
            }
            None => p.push((name, span, count)),
        }
    });
}

/// This thread's phases, taken, for [`absorb`] on the thread that prints the
/// table.
pub fn take_phases() -> Vec<(&'static str, Duration, u64)> {
    PHASES.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Adds another thread's [`take_phases`] to this thread's table. A phase run
/// on many threads is charged the sum of their spans.
pub fn absorb(rows: Vec<(&'static str, Duration, u64)>) {
    for (name, span, count) in rows {
        add(name, span, count);
    }
}

/// Starts timing `name`, or returns `None` when nothing is armed: the only
/// cost an ordinary build pays.
pub fn phase(name: &'static str) -> Option<Phase> {
    phases_on().then(|| Phase(name, Instant::now(), enter(name)))
}

/// Returns the phase table and clears it. Empty when nothing is armed.
pub fn phase_table() -> String {
    let counts = snapshot();
    let rows = take_phases();
    if rows.is_empty() {
        return String::new();
    }
    let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(5).max(5);
    let mut out = format!("{:<width$}  {:>8}  {:>12}\n", "phase", "count", "total");
    for (name, total, count) in &rows {
        out.push_str(&format!(
            "{:<width$}  {:>8}  {:>12}\n",
            name,
            count,
            ms(*total)
        ));
    }
    let lines = LINES.with(|l| l.take());
    out.push_str(&format!("lines read  {lines}\n"));
    if ALLOCS_ON.load(Relaxed) {
        out.push_str(&alloc_table(&counts));
    }
    out
}

/// The slots of [`Counts`] per phase: allocations, reallocations, frees and
/// bytes requested.
const KINDS: usize = 4;
/// Phase tags; a phase past the last shares it.
const TAGS: usize = 96;
/// One thread's counts: `KINDS` consecutive slots per phase tag, tag 0 being
/// the code outside any phase.
pub type Counts = [u64; KINDS * TAGS];

static ALLOCS_ON: AtomicBool = AtomicBool::new(false);
static TAG_NAMES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

thread_local! {
    static TAG: Cell<u8> = const { Cell::new(0) };
    static COUNTS: [Cell<u64>; KINDS * TAGS] = const { [const { Cell::new(0) }; KINDS * TAGS] };
}

/// Starts counting allocations on every thread if `VYRN_BUILD_PROFILE` is
/// `allocs`. Call it first on the thread that runs the build: what the
/// process allocated earlier is not counted.
pub fn start_allocs() {
    ALLOCS_ON.store(
        std::env::var("VYRN_BUILD_PROFILE").is_ok_and(|v| v == "allocs"),
        Relaxed,
    );
}

/// Makes `name` the phase that owns allocations and returns the one it
/// replaces.
fn enter(name: &'static str) -> u8 {
    let prev = TAG.with(Cell::get);
    if ALLOCS_ON.load(Relaxed) {
        let mut names = TAG_NAMES.lock().unwrap();
        let i = names.iter().position(|n| *n == name).unwrap_or_else(|| {
            names.push(name);
            names.len() - 1
        });
        TAG.with(|t| t.set((i + 1).min(TAGS - 1) as u8));
    }
    prev
}

/// The phase tag of the running thread, for a worker to adopt with [`set_tag`].
pub fn tag() -> u8 {
    TAG.with(Cell::get)
}

/// Makes a worker thread count into the phase that spawned it.
pub fn set_tag(t: u8) {
    TAG.with(|c| c.set(t));
}

/// This thread's counts, taken, for [`absorb_counts`].
pub fn snapshot() -> Counts {
    COUNTS.with(|c| std::array::from_fn(|i| c[i].replace(0)))
}

/// Adds a worker's [`snapshot`] to this thread's counts.
pub fn absorb_counts(other: &Counts) {
    COUNTS.with(|c| {
        for (slot, n) in c.iter().zip(other) {
            slot.set(slot.get() + n);
        }
    });
}

fn alloc_table(counts: &Counts) -> String {
    let names = TAG_NAMES.lock().unwrap();
    let mut out = format!(
        "\n{:<28} {:>10} {:>10} {:>10} {:>12}\n",
        "alloc phase", "allocs", "reallocs", "frees", "bytes"
    );
    for (t, row) in counts.chunks(KINDS).enumerate() {
        if row.iter().all(|&n| n == 0) {
            continue;
        }
        let name = match t {
            0 => "(outside)",
            _ => names.get(t - 1).copied().unwrap_or("(overflow)"),
        };
        out.push_str(&format!(
            "{name:<28} {:>10} {:>10} {:>10} {:>12}\n",
            row[0], row[1], row[2], row[3]
        ));
    }
    out
}

fn bump(kind: usize, bytes: usize) {
    if ALLOCS_ON.load(Relaxed) {
        let t = TAG.with(Cell::get) as usize * KINDS;
        COUNTS.with(|c| {
            c[t + kind].set(c[t + kind].get() + 1);
            c[t + 3].set(c[t + 3].get() + bytes as u64);
        });
    }
}

/// A global allocator that counts into the running phase when
/// `VYRN_BUILD_PROFILE=allocs`, and otherwise adds one relaxed load.
pub struct Counting<A>(pub A);

// SAFETY: every method forwards to `A` unchanged and only bumps counters.
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        bump(0, l.size());
        self.0.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        bump(0, l.size());
        self.0.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        bump(1, n);
        self.0.realloc(p, l, n)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        bump(2, 0);
        self.0.dealloc(p, l)
    }
}
