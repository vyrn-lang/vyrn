//! Where an interpreted run spends its time, per function.
//!
//! The interpreter already funnels every Vyrn call through one place —
//! `Interp::call_capturing`, where the call-depth counter lives — so this
//! measures there and nowhere else. Two clock reads per call when it is armed,
//! and one thread-local `bool` read when it is not.
//!
//! **This measures the reference semantics, not the shipped binary.** It is the
//! same line `vyrn bench` draws when it refuses to time the interpreter: a
//! number here says what the tree-walker did, and an optimizing backend does
//! something else. It is still the number that matters for the things that only
//! ever run interpreted — every `gen fn`, every `test` block, and `vyrn run`
//! itself, which is what builds this project's own site.
//!
//! ponytail: a flat table, and no file format. The profiler census
//! (`rfcs/census/profilers.md`) sets out the pprof / speedscope / own-format
//! choice with evidence and leaves it open, and that choice is not this
//! module's to make. What it collects — a name, a count, an inclusive time and
//! an exclusive time — is what all three want, so an emitter is an addition
//! rather than a rewrite. Its own experiment found flat text reads as well as
//! anything for the case this serves first.
//!
//! **Recursion inflates the inclusive column and not the exclusive one.** A
//! function that calls itself is on the stack more than once, and each frame
//! counts its own span, so the inclusive time of a recursive function can pass
//! the wall time of the run. Exclusive time never does: it is charged once, to
//! the frame that spent it. Every profiler with a per-frame timer has this, and
//! saying so is cheaper than pretending otherwise.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// One function's share of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    /// How many times it was entered.
    pub calls: u64,
    /// Time from entry to exit, its callees included.
    pub inclusive: Duration,
    /// The same, minus the time its callees spent.
    pub exclusive: Duration,
}

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    static ROWS: RefCell<HashMap<String, (u64, Duration, Duration)>> =
        RefCell::new(HashMap::new());
    /// One accumulator per open frame, holding what that frame's callees have
    /// spent so far. A frame's exclusive time is its own span minus its top.
    static KIDS: RefCell<Vec<Duration>> = const { RefCell::new(Vec::new()) };
}

/// Start recording on this thread, discarding anything already collected.
pub fn start() {
    ROWS.with(|r| r.borrow_mut().clear());
    KIDS.with(|k| k.borrow_mut().clear());
    ON.with(|o| o.set(true));
}

/// Whether this thread is recording.
pub fn on() -> bool {
    ON.with(|o| o.get())
}

/// Stop recording and take what was collected, heaviest exclusive time first.
pub fn take() -> Vec<Row> {
    ON.with(|o| o.set(false));
    let mut rows: Vec<Row> = ROWS.with(|r| {
        std::mem::take(&mut *r.borrow_mut())
            .into_iter()
            .map(|(name, (calls, inclusive, exclusive))| Row {
                name,
                calls,
                inclusive,
                exclusive,
            })
            .collect()
    });
    // Exclusive first, then name, so two runs of the same program print the same
    // order even when two functions tie.
    rows.sort_by(|a, b| b.exclusive.cmp(&a.exclusive).then(a.name.cmp(&b.name)));
    rows
}

/// Take back what a thread of the engine's own collected.
///
/// The interpreter runs its program on a dedicated stack
/// (`interp::on_deep_stack`), so the rows are on that thread and the one that
/// asked for them is elsewhere. Same hand-back `own::trace` does, for the same
/// reason: per-thread on purpose, because two tests in one binary run at once
/// and a global would interleave them.
pub fn adopt(rows: Vec<Row>) {
    ROWS.with(|r| {
        let mut m = r.borrow_mut();
        for row in rows {
            let e = m
                .entry(row.name)
                .or_insert((0, Duration::ZERO, Duration::ZERO));
            e.0 += row.calls;
            e.1 += row.inclusive;
            e.2 += row.exclusive;
        }
    });
    ON.with(|o| o.set(true));
}

/// A call is starting. `None` when nothing is recording, which is the only cost
/// an unprofiled run pays.
pub fn enter() -> Option<Instant> {
    if !on() {
        return None;
    }
    KIDS.with(|k| k.borrow_mut().push(Duration::ZERO));
    Some(Instant::now())
}

/// A call has returned — by any edge, including a trap the caller catches.
pub fn exit(name: &str, started: Instant) {
    let span = started.elapsed();
    let kids = KIDS.with(|k| {
        let mut s = k.borrow_mut();
        let mine = s.pop().unwrap_or(Duration::ZERO);
        // Charge the whole span to the caller's accumulator, so the caller's
        // exclusive time excludes this frame and everything under it.
        if let Some(top) = s.last_mut() {
            *top += span;
        }
        mine
    });
    ROWS.with(|r| {
        let mut m = r.borrow_mut();
        let e = m
            .entry(name.to_string())
            .or_insert((0, Duration::ZERO, Duration::ZERO));
        e.0 += 1;
        e.1 += span;
        e.2 += span.saturating_sub(kids);
    });
}

/// The table, as the CLI prints it. `limit` rows, or all of them at zero.
pub fn table(rows: &[Row], limit: usize) -> String {
    let shown = if limit == 0 {
        rows
    } else {
        &rows[..rows.len().min(limit)]
    };
    let total: Duration = rows.iter().map(|r| r.exclusive).sum();
    let width = shown.iter().map(|r| r.name.len()).max().unwrap_or(8).max(8);
    let mut out = format!(
        "{:<width$}  {:>10}  {:>12}  {:>12}  {:>6}\n",
        "function", "calls", "self", "total", "share"
    );
    for r in shown {
        let share = if total.is_zero() {
            0.0
        } else {
            r.exclusive.as_secs_f64() / total.as_secs_f64() * 100.0
        };
        out.push_str(&format!(
            "{:<width$}  {:>10}  {:>12}  {:>12}  {:>5.1}%\n",
            r.name,
            r.calls,
            ms(r.exclusive),
            ms(r.inclusive),
            share
        ));
    }
    if limit > 0 && rows.len() > limit {
        out.push_str(&format!(
            "... {} more function(s), {} of self time\n",
            rows.len() - limit,
            ms(rows[limit..].iter().map(|r| r.exclusive).sum())
        ));
    }
    out.push_str(&format!(
        "\n{} function(s), {} calls, {} of self time\n",
        rows.len(),
        rows.iter().map(|r| r.calls).sum::<u64>(),
        ms(total)
    ));
    out
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_time_excludes_a_callee() {
        start();
        let outer = enter().unwrap();
        std::thread::sleep(Duration::from_millis(10));
        let inner = enter().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        exit("inner", inner);
        exit("outer", outer);
        let rows = take();
        let by = |n: &str| rows.iter().find(|r| r.name == n).unwrap().clone();
        let (o, i) = (by("outer"), by("inner"));
        // The callee's whole span belongs to the callee. Lower bounds only:
        // a sleep can overshoot on a loaded runner, never undershoot.
        assert!(i.exclusive >= Duration::from_millis(25), "{i:?}");
        assert!(o.inclusive >= Duration::from_millis(35), "{o:?}");
        // And none of it to the caller: the two exclusives PARTITION the outer
        // span, so their sum cannot exceed it. Arithmetic on one clock rather
        // than a wall bound, because a loaded runner (macOS CI stretched the
        // 10 ms sleep past 25) stretches every number together — a caller
        // charged its callee's time fails this however slow the machine.
        assert!(
            o.exclusive + i.exclusive <= o.inclusive + Duration::from_millis(5),
            "the caller was charged its callee's time: {o:?} {i:?}"
        );
    }

    #[test]
    fn nothing_is_recorded_when_it_is_not_armed() {
        // `take` leaves it off, and `enter` then answers `None` — which is the
        // whole cost an ordinary run pays.
        let _ = take();
        assert!(!on());
        assert!(enter().is_none());
        assert!(take().is_empty());
    }

    #[test]
    fn a_recursive_frame_is_charged_once() {
        start();
        let a = enter().unwrap();
        let b = enter().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        exit("rec", b);
        exit("rec", a);
        let rows = take();
        let r = rows.iter().find(|r| r.name == "rec").unwrap();
        assert_eq!(r.calls, 2);
        // Inclusive counts both frames; exclusive is charged to the inner
        // frame alone, so it is at most HALF of inclusive plus the outer
        // frame's own sliver — a ratio, not a wall bound, for the reason the
        // test above states (macOS CI stretched the 20 ms sleep to 110).
        // Charged twice, exclusive equals inclusive and this fails at any
        // speed.
        assert!(
            r.exclusive * 2 <= r.inclusive + Duration::from_millis(5),
            "{r:?}"
        );
    }
}
