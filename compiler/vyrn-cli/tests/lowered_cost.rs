//! What the lowered form costs, owned against borrowed — RFC-0101 M1's two
//! measurements, and the number §2.1 item 1's fallback threshold is written from.
//!
//! **Method, stated because the number is only as good as it.** A counting
//! global allocator wraps the system one and tracks bytes outstanding; the
//! figure reported is the delta in LIVE bytes across building the value, taken
//! while the value is still alive, plus the peak reached while building it.
//! That is heap held, not resident set: it excludes the program's own AST (which
//! both forms share) and it excludes allocator slack. It is the right number for
//! the question, which is "what does a second copy of every body cost", and it
//! is reproducible on any platform, which peak RSS on Windows is not.
//!
//! The OWNED form is modelled the way §2.1 item 1 describes it: one concrete
//! body per instantiation, so each instance clones the `Block` it borrows today.
//! Nothing else changes — same instances, same rows, same recorded types.
//!
//! This is a measurement, so it asserts almost nothing: only that both forms
//! were actually built. The numbers go in the RFC, and `--nocapture` prints them.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use vyrn_frontend::ast::{Block, Program, Type};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(now, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        System.dealloc(p, l);
    }
}

#[global_allocator]
static A: Counting = Counting;

struct Fs;

impl vyrn_frontend::loader::ModuleResolver for Fs {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
}

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn load(path: &std::path::Path) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let root = path.to_string_lossy().replace('\\', "/");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    vyrn_frontend::load(&src, &root, &opts, &Fs).map_err(|_| "load failed".to_string())
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[test]
fn what_a_concrete_body_per_instantiation_costs() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(measure)
        .unwrap()
        .join()
        .unwrap();
}

fn measure() {
    // The largest corpus module, by the size of the program it links — which is
    // the thing the form is built over, not the size of the file.
    let mut names: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    let mut largest: Option<(usize, PathBuf, Program)> = None;
    for path in names {
        let Ok(p) = load(&path) else { continue };
        let n = p.functions.len();
        if largest.as_ref().is_none_or(|(m, ..)| n > *m) {
            largest = Some((n, path, p));
        }
    }
    let (fns, path, program) = largest.expect("the corpus has a largest module");
    let name = path.file_name().unwrap().to_string_lossy().to_string();

    let t = std::time::Instant::now();
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let lowered = vyrn_lower::lower(&program);
    let borrowed = LIVE.load(Relaxed).saturating_sub(before);
    let borrowed_peak = PEAK.load(Relaxed).saturating_sub(before);
    let borrowed_ms = t.elapsed();
    let instances = lowered.instances.len();
    let rows = lowered.rows();

    // One concrete body per instantiation: the shape §2.1 item 1 chose, and the
    // shape whose cost decides whether the fallback (one generic body plus one
    // instance list) is ever needed.
    let t = std::time::Instant::now();
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let owned: Vec<(Vec<Type>, Block)> = lowered
        .instances
        .iter()
        .map(|i| (i.type_args.clone(), i.func.body.clone()))
        .collect();
    let owned_bytes = LIVE.load(Relaxed).saturating_sub(before);
    let owned_peak = PEAK.load(Relaxed).saturating_sub(before);
    let owned_ms = t.elapsed();

    assert!(!owned.is_empty(), "nothing was owned");
    assert!(rows > 0, "nothing was lowered");

    eprintln!(
        "RFC-0101 M1 measurement — largest corpus module: {name} ({fns} linked functions)\n  \
         instances {instances}, rows {rows}\n  \
         BORROWED (what M1 ships): live {:.2} MiB, peak {:.2} MiB, built in {:?}\n  \
         OWNED body per instantiation, ON TOP of that: live {:.2} MiB, peak {:.2} MiB, \
         built in {:?}\n  \
         owned/borrowed live ratio {:.2}x; per instance, borrowed {} B and owned {} B",
        mib(borrowed),
        mib(borrowed_peak),
        borrowed_ms,
        mib(owned_bytes),
        mib(owned_peak),
        owned_ms,
        (borrowed + owned_bytes) as f64 / borrowed.max(1) as f64,
        borrowed / instances.max(1),
        owned_bytes / instances.max(1),
    );
}
