//! Where a build spends its time, per phase.
//!
//! This measured an interpreted RUN once, per function, because the tree-walker
//! funnelled every Vyrn call through one place and a span could be charged
//! there. RFC-0125 §3 M5 deleted the tree-walker, and with it the funnel: the
//! compiled route has no per-call hook to charge at, and `vyrn run --profile`
//! reports the phases of the compile and the operations the guest executed
//! instead (`wasmrun`'s meter). So what is left here is the phase half, which is
//! the half that was never the interpreter's.
//!
//! ponytail: a flat table, and no file format. The profiler census
//! (`rfcs/census/profilers.md`) sets out the pprof / speedscope / own-format
//! choice with evidence and leaves it open, and that choice is not this
//! module's to make.
//!
//! **Flat, additive, first-seen order.** A phase is a whole stage of the
//! pipeline, and the stages do not overlap, so a caller/callee split would say
//! nothing a sum does not.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// A duration in the units a reader can compare at a glance. Integer-only above
/// a millisecond, because a profile is read by eye and 1,234 ms sorts wrong
/// against 987 ms when both carry three decimals.
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

// ---- Build phases (RFC-0125 §3 M4) -----------------------------------------
//
// Every program links `std/runtime` and `std/text`, and the suites that emit
// hundreds of modules pay for that hundreds of times. To decide whether the
// front end is the cost you have to see the build split into its phases: a
// name, a count and a total, armed by `VYRN_BUILD_PROFILE=1` and silent
// otherwise.

thread_local! {
    /// `(name, total, count)` in first-seen order — the order a build runs its
    /// phases in, which is the order a reader wants them.
    static PHASES: RefCell<Vec<(&'static str, Duration, u64)>> = const { RefCell::new(Vec::new()) };
}

/// Whether build phases are being timed. Read once: an env lookup per phase on
/// a hot loader path would be measuring the measurement.
pub fn phases_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VYRN_BUILD_PROFILE").is_ok_and(|v| v != "0"))
}

/// One open phase. Charges its span on drop, so an early `return` inside the
/// phase still records it.
pub struct Phase(&'static str, Instant);

impl Drop for Phase {
    fn drop(&mut self) {
        charge(self.0, self.1.elapsed());
    }
}

/// Charge `span` to the phase `name`, from a caller that timed it itself.
///
/// [`phase`] is the pipeline's own hook and answers to `VYRN_BUILD_PROFILE`.
/// This one answers to nobody, because its caller already knows it is
/// profiling: `vyrn run --profile` on the compiled route (RFC-0125 §3 M5, the
/// `run-profile` row). The tree-walker's per-function rows mean nothing there —
/// nothing walks a tree — and the phases of the compile and of the run are what
/// there is to report.
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

/// Start timing `name`, or `None` when nothing is armed — the only cost an
/// ordinary build pays.
pub fn phase(name: &'static str) -> Option<Phase> {
    phases_on().then(|| Phase(name, Instant::now()))
}

/// The phase table, and it clears what it reports. Empty when nothing is armed.
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
    out
}
