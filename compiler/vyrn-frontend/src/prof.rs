//! Where a build spends its time, per phase: a flat, additive table in
//! first-seen order. Phases are whole pipeline stages that do not overlap, so
//! a caller and callee split would add nothing.
//!
//! ponytail: a flat table, no file format. The pprof, speedscope or
//! own-format choice is open.

use std::cell::{Cell, RefCell};
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

/// Whether build phases are timed. Read once, so the hot loader path does not
/// pay an env lookup per phase.
pub fn phases_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VYRN_BUILD_PROFILE").is_ok_and(|v| v != "0"))
}

/// One open phase. It charges its span on drop, so an early `return` still
/// records it.
pub struct Phase(&'static str, Instant);

impl Drop for Phase {
    fn drop(&mut self) {
        charge(self.0, self.1.elapsed());
    }
}

/// Charges `span` to the phase `name`, for a caller that timed it and already
/// knows it is profiling (`vyrn run --profile`). [`phase`] is the pipeline's
/// hook and obeys `VYRN_BUILD_PROFILE`.
pub fn charge(name: &'static str, span: Duration) {
    PHASES.with(|p| {
        let mut p = p.borrow_mut();
        match p.iter_mut().find(|(n, _, _)| *n == name) {
            Some(row) => {
                row.1 += span;
                row.2 += 1;
            }
            None => p.push((name, span, 1)),
        }
    });
}

/// Starts timing `name`, or returns `None` when nothing is armed: the only
/// cost an ordinary build pays.
pub fn phase(name: &'static str) -> Option<Phase> {
    phases_on().then(|| Phase(name, Instant::now()))
}

/// Returns the phase table and clears it. Empty when nothing is armed.
pub fn phase_table() -> String {
    let rows: Vec<(&'static str, Duration, u64)> =
        PHASES.with(|p| std::mem::take(&mut *p.borrow_mut()));
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
    out
}
