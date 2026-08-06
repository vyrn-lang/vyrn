//! Keystroke-budget probe for the editor path (RFC-0084).
//!
//! One `analyze_linked` is what an edit costs the LSP, so its mean over ten
//! edits is the budget. RFC-0084 took it from 1579 ms to 97 ms and nothing may
//! put it back; `vyrn check` is not a proxy, because it does different work.
//!
//! ```text
//! cargo run --release --example lspbench -p vyrn-frontend -- <file.vyrn> ...
//! ```
//!
//! `VYRN_STD` names the std root (`std` by default, relative to the current
//! directory). Each iteration appends a distinct comment, because an unchanged
//! document is answered by the per-module diagnostic memo rather than by the
//! checker — which is the editor's behaviour on an idle buffer, not on a
//! keystroke.

struct DiskResolver;

impl vyrn_frontend::loader::ModuleResolver for DiskResolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
}

fn main() {
    let std_root = std::env::var("VYRN_STD").unwrap_or_else(|_| "std".to_string());
    for path in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&path).unwrap();
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some(std_root.clone()),
            ..Default::default()
        };
        let res = DiskResolver;
        let run = |i: usize| {
            let edited = format!("{src}\n// keystroke {i}\n");
            let _ = vyrn_frontend::analyze_linked(&edited, &path, &opts, &res);
        };
        for i in 0..3 {
            run(i);
        }
        let n = 10;
        let t = std::time::Instant::now();
        for i in 0..n {
            run(1000 + i);
        }
        let a = vyrn_frontend::analyze_linked(&src, &path, &opts, &res);
        println!(
            "{:.1} ms  {} symbols, {} diagnostics  {path}",
            t.elapsed().as_secs_f64() * 1000.0 / n as f64,
            a.symbols.len(),
            a.diagnostics.len()
        );
        for d in a.diagnostics.iter().take(2) {
            println!("      {}", d.message);
        }
    }
}
