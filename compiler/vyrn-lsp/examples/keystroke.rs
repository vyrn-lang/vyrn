//! Keystroke-budget probe for the editor path: `analyze_judged` with
//! `vyrn_lower::JUDGE`, the wasm generation engine and the judgment memo
//! armed, as `vyrn-lsp` runs them. `vyrn check` is not a proxy, because it does
//! different work.
//!
//! ```text
//! cargo run --release --manifest-path vyrn-lsp/Cargo.toml --example keystroke -- <file.vyrn> ...
//! ```
//!
//! Each edit appends a distinct comment to the root, so the root is checked and
//! judged again while every other module is reused, as on a keystroke. Prints
//! the best and the median of `VYRN_RUNS` edits (5 by default) after three
//! warm-up edits. `VYRN_BUILD_PROFILE=1` adds the phase table of one more.

use vyrn_frontend::loader::{DiskResolver, LoadOptions, ModuleResolver};
use vyrn_frontend::manifest::{pinned_blob, Lock};

/// The disk, and a remote import pinned in the project's lock, as the server
/// reads them.
struct Resolver(Option<String>);

impl ModuleResolver for Resolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if !vyrn_frontend::loader::is_remote(resolved) {
            return DiskResolver.read(resolved);
        }
        let dir = self.0.as_deref().ok_or("no project")?;
        let (_, sha) = Lock::in_project(dir)?
            .entries
            .get(resolved)
            .cloned()
            .ok_or("not pinned")?;
        pinned_blob(Some(dir), &sha).unwrap_or(Err("not cached".to_string()))
    }
    fn list_kinds(&self, resolved: &str) -> Result<Vec<String>, String> {
        DiskResolver.list_kinds(resolved)
    }
    fn gen_cache_get(&self, key: &str) -> Option<String> {
        DiskResolver.gen_cache_get(key)
    }
    fn gen_cache_put(&self, key: &str, value: &str) {
        DiskResolver.gen_cache_put(key, value)
    }
}

fn main() {
    let runs: usize = std::env::var("VYRN_RUNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let files: Vec<String> = std::env::args().skip(1).collect();
    // The server's analysis thread: its stack, and the memo it arms.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            vyrn_frontend::movecheck::reuse_judgments();
            for path in files {
                probe(&path, runs);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

fn probe(path: &str, runs: usize) {
    let path = std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    let src = std::fs::read_to_string(&path).unwrap();
    let mut opts = LoadOptions {
        std_root: vyrn_frontend::manifest::std_root(),
        ..Default::default()
    };
    let mut resolver = Resolver(None);
    let engine = vyrn_genwasm::engine();
    let dir = std::path::Path::new(&path).parent().unwrap();
    if let Ok(Some(m)) = vyrn_frontend::manifest::find(dir) {
        opts.aliases = m.dependencies.into_iter().collect();
        opts.alias_base = m.dir.clone();
        opts.audience = m.audience;
        opts.artifacts = m.artifacts;
        resolver.0 = Some(m.dir);
    }
    let analyze = |text: &str| {
        vyrn_frontend::analyze_judged(
            text,
            Some((&path, &opts, &resolver)),
            Some(&*engine),
            &vyrn_lower::JUDGE,
        )
    };
    for i in 0..3 {
        analyze(&format!("{src}\n// warm {i}\n"));
    }
    let mut ms: Vec<f64> = (0..runs)
        .map(|i| {
            let edited = format!("{src}\n// keystroke {i}\n");
            let t = std::time::Instant::now();
            analyze(&edited);
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(|a, b| a.total_cmp(b));
    let _ = vyrn_frontend::prof::phase_table();
    analyze(&format!("{src}\n// profiled\n"));
    eprint!("{}", vyrn_frontend::prof::phase_table());
    let a = analyze(&src);
    println!(
        "best {:.1} ms  median {:.1} ms  {} diagnostics, {} memory notes  {path}",
        ms[0],
        ms[ms.len() / 2],
        a.diagnostics.len(),
        a.memory.len()
    );
    for d in a.diagnostics.iter().take(3) {
        println!("      {}:{} {}", d.line, d.col, d.message);
    }
}
