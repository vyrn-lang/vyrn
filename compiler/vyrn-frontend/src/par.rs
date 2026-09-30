//! Runs one pass's per-body work on every thread, with output independent of
//! the thread count and the work order. [`in_parallel`] is the one scheduler;
//! the checker's typing and the placer's builds call it.

/// How many threads type, build and place bodies: `VYRN_THREADS`, else the
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

/// Returns `work` of each of `items`, in `items`' order, run on up to
/// [`threads`] threads. Workers take items from one counter, heaviest first
/// by `weight`, so the longest body does not start last; `VYRN_SHUFFLE=<seed>`
/// permutes that order, for the test that holds every output independent of
/// it. Each worker keeps one `S` from `fresh` across its items. A panic in a
/// worker panics the caller.
pub fn in_parallel<T: Sync, S, R: Send>(
    items: &[T],
    weight: impl Fn(&T) -> usize,
    fresh: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, &T) -> R + Sync,
) -> Vec<R> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_cached_key(|&i| std::cmp::Reverse(weight(&items[i])));
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
        done
    };
    let n = threads().min(items.len());
    let mut done = if n <= 1 {
        worker()
    } else {
        std::thread::scope(|s| {
            // A worker recurses as deep as the calling thread, so it gets the
            // CLI's deep stack, not the platform default (a debug build
            // overflowed on `limits.rs`'s wide frames).
            let workers: Vec<_> = (0..n)
                .map(|_| {
                    std::thread::Builder::new()
                        .stack_size(crate::trap::DEEP_STACK_BYTES)
                        .spawn_scoped(s, || (worker(), crate::prof::take_phases()))
                        .expect("spawn a worker")
                })
                .collect();
            let mut done = Vec::with_capacity(items.len());
            for w in workers {
                let (d, phases) = w.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
                crate::prof::absorb(phases);
                done.extend(d);
            }
            done
        })
    };
    // Each index was taken once, so the sorted list is one result per item.
    done.sort_unstable_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}
