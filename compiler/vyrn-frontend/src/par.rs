//! Runs one pass's per-body work on every thread, with output independent of
//! the thread count and the work order. [`in_parallel`] is the one scheduler;
//! the checker's typing, the placer's builds and the emitter's bodies call it.

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// How many threads type, build, place and emit bodies: `VYRN_THREADS`, else the
/// machine's available parallelism. One works on the calling thread, as a
/// target without threads (wasm32) does, and keeps `VYRN_KERNEL_TRACE` in
/// body order.
fn threads() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        (std::env::var("VYRN_THREADS").ok())
            .and_then(|v| v.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
    })
}

/// The summed weight below which [`in_parallel`] works on the calling thread,
/// because handing work to the helpers costs more than they save. Measured
/// with 12 threads started per call, about 0.4 ms, on `examples/fib.vyrn`,
/// `examples/bin/server.vyrn` and `site/export.vyrn`: every call of weight
/// 441 or less ran slower on 12 threads than on 1, and every call of weight
/// 746 or more ran faster but one typing call of 915 (0.47 ms to 0.54 ms).
/// The callers weigh in their own units (expressions, statements, rows,
/// items); the measurements cover each. The helpers persist across calls
/// ([`offer`]), and the cut was not measured again with them.
const SERIAL_BELOW: usize = 600;

/// Returns `work` of each of `items`, in `items`' order, run on the calling
/// thread and up to [`threads`] less one helpers ([`offer`]), or on the
/// calling thread alone when the weights sum below [`SERIAL_BELOW`].
/// Workers take items from one counter, heaviest first by `weight`, so the
/// longest body does not start last; `VYRN_SHUFFLE=<seed>` permutes that
/// order, for the test that holds every output independent of it. Each
/// worker keeps one `S` from `fresh` across its items. A panic in a worker
/// panics the caller.
///
/// A helper outlives the call, so `work` finds a helper's thread-locals as
/// an earlier call left them, and the calling thread's as its caller set
/// them. A thread-local `work` writes is a cache whose key decides its
/// value, or is set and restored around the item (`observe::following` in
/// `vyrn-codegen`). The phases and allocation counts of each helper's share
/// are taken when its share ends.
pub fn in_parallel<T: Sync, S: Send, R: Send>(
    items: &[T],
    weight: impl Fn(&T) -> usize,
    fresh: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, &T) -> R + Sync,
) -> Vec<R> {
    in_parallel_keep(items, weight, fresh, work, drop)
}

/// [`in_parallel`], which also hands each worker's `S` after its last item to
/// `keep`, on the calling thread, in no fixed order.
pub fn in_parallel_keep<T: Sync, S: Send, R: Send>(
    items: &[T],
    weight: impl Fn(&T) -> usize,
    fresh: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, &T) -> R + Sync,
    mut keep: impl FnMut(S),
) -> Vec<R> {
    let weights: Vec<usize> = items.iter().map(weight).collect();
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(weights[i]));
    if let Some(seed) = std::env::var("VYRN_SHUFFLE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    {
        // xorshift64, from a state that is never zero.
        let mut x = seed | 1;
        for i in (1..order.len()).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            order.swap(i, (x % (i as u64 + 1)) as usize);
        }
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    // Measure: `next` only grows, and a worker stops once it passes `order`.
    let worker = || {
        let mut state = fresh();
        let mut done = Vec::new();
        while let Some(&i) = order.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
            done.push((i, work(&mut state, &items[i])));
        }
        (done, state)
    };
    let n = match weights.iter().sum::<usize>() {
        w if w < SERIAL_BELOW => 1,
        _ => threads().min(items.len()),
    };
    let mut done = if n <= 1 {
        let (done, state) = worker();
        keep(state);
        done
    } else {
        let tag = crate::prof::tag();
        let ran: Vec<Mutex<Option<Ran<R, S>>>> = (1..n).map(|_| Mutex::new(None)).collect();
        let offers: Vec<Arc<Offer>> = (ran.iter())
            .map(|slot| {
                let job = || {
                    *lock(slot) = Some(std::panic::catch_unwind(AssertUnwindSafe(|| {
                        crate::prof::set_tag(tag);
                        let done = worker();
                        (done, crate::prof::take_phases(), crate::prof::snapshot())
                    })));
                };
                let job: Box<dyn FnOnce() + Send + '_> = Box::new(job);
                // SAFETY: only the lifetime changes. The job borrows this
                // frame, and `Reclaim` keeps the frame until every offer is
                // taken back or has ended, on return and on unwind alike.
                let job: Box<dyn FnOnce() + Send + 'static> = unsafe { std::mem::transmute(job) };
                Arc::new(Offer {
                    run: Mutex::new(Some(job)),
                    done: Mutex::new(false),
                    ended: Condvar::new(),
                })
            })
            .collect();
        let reclaim = Reclaim(&offers);
        offer(&offers);
        let (mut done, state) = worker();
        keep(state);
        drop(reclaim);
        for slot in ran {
            match slot.into_inner().unwrap_or_else(PoisonError::into_inner) {
                None => {}
                Some(Err(e)) => std::panic::resume_unwind(e),
                Some(Ok(((d, state), phases, counts))) => {
                    crate::prof::absorb(phases);
                    crate::prof::absorb_counts(&counts);
                    done.extend(d);
                    keep(state);
                }
            }
        }
        done
    };
    // Each index was taken once, so the sorted list is one result per item.
    done.sort_unstable_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

/// What a helper's worker returns: its results and state, with its phases
/// and counts for the calling thread to absorb, or its panic.
type Ran<R, S> = std::thread::Result<(
    (Vec<(usize, R)>, S),
    Vec<(&'static str, std::time::Duration, u64)>,
    crate::prof::Counts,
)>;

/// One worker an [`in_parallel`] call offers the helpers. The first helper to
/// take `run` runs it, then sets `done`. The call takes back each offer no
/// helper took, so it waits only for a helper running its work, and a call
/// made inside a worker finishes on its own thread when every helper is busy.
struct Offer {
    run: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    done: Mutex<bool>,
    ended: Condvar,
}

/// The offers no helper took yet, oldest first.
static OFFERS: Mutex<VecDeque<Arc<Offer>>> = Mutex::new(VecDeque::new());
static OFFERED: Condvar = Condvar::new();

/// Queues `offers`. The first call starts the helpers, [`threads`] less one,
/// which live as long as the process, because starting them per call costs
/// about 0.4 ms. A helper that fails to start leaves its share to the
/// calling threads.
fn offer(offers: &[Arc<Offer>]) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        for _ in 1..threads() {
            // A worker recurses as deep as the calling thread, so it gets the
            // CLI's deep stack, not the platform default (a debug build
            // overflowed on `limits.rs`'s wide frames).
            let _ = std::thread::Builder::new()
                .stack_size(crate::trap::DEEP_STACK_BYTES)
                .spawn(help);
        }
    });
    lock(&OFFERS).extend(offers.iter().cloned());
    for _ in offers {
        OFFERED.notify_one();
    }
}

/// A helper's loop: takes the oldest offer and runs it, unless its call took
/// it back first.
fn help() -> ! {
    loop {
        let mut queue = lock(&OFFERS);
        let offer = loop {
            match queue.pop_front() {
                Some(o) => break o,
                None => queue = OFFERED.wait(queue).unwrap_or_else(PoisonError::into_inner),
            }
        };
        drop(queue);
        let run = lock(&offer.run).take();
        if let Some(run) = run {
            run();
            *lock(&offer.done) = true;
            offer.ended.notify_all();
        }
    }
}

/// On drop, takes back each offer no helper took and waits for each one a
/// helper took to end.
struct Reclaim<'a>(&'a [Arc<Offer>]);

impl Drop for Reclaim<'_> {
    fn drop(&mut self) {
        for o in self.0 {
            if lock(&o.run).take().is_some() {
                continue;
            }
            let mut done = lock(&o.done);
            while !*done {
                done = o.ended.wait(done).unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

/// Locks `m`. No code that can panic runs under these locks, so a poisoned
/// one still holds a whole value.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
