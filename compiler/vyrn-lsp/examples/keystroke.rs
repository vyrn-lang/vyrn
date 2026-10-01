//! Keystroke-budget probe for the editor path: `analyze_judged` with
//! `vyrn_lower::JUDGE`, the wasm generation engine, and a session with the
//! judgment memo and the watched disk, as `vyrn-lsp` runs them for a client
//! that sends file events. `vyrn check` is not a proxy, because it does
//! different work.
//!
//! ```text
//! cargo run --release --manifest-path vyrn-lsp/Cargo.toml --example keystroke -- <file.vyrn> ...
//! ```
//!
//! `VYRN_EDIT` picks the edit, each distinct per run:
//! - `comment` (the default) appends a comment to the root;
//! - `body` adds a `let` after the `{` of the root's last `fn` line;
//! - `sig` toggles `mut` on the root function other than `main` whose name the
//!   root spells most often, a signature edit every reader of it sees;
//! - `param` changes the type of the first parameter of the function `sig`
//!   picks, from its own to `Int32` and back (`Int64` where it is `Int32`);
//! - `line` puts `1 + i` blank lines before the root, moving every line;
//! - `doc` appends a protocol whose doc comment and member's doc comment
//!   name the edit.
//!
//! Only `line` moves a line. Prints the best and the median of `VYRN_RUNS` edits
//! (5 by default) after three warm-up edits, then how many bodies one more edit
//! typed and replayed (`checker::recheck`) and judged and served (the judgment
//! memo). `VYRN_BUILD_PROFILE=1` adds the phase table of that edit.

use std::sync::Arc;

use vyrn_frontend::loader::{DiskResolver, LoadOptions, ModuleResolver};
use vyrn_frontend::manifest::{pinned_blob, Lock};
use vyrn_frontend::session::Session;

/// The session's disk, and a remote import pinned in the project's lock, as
/// the server reads them.
struct Resolver(Option<String>, Arc<Session>);

impl ModuleResolver for Resolver {
    fn read(&self, resolved: &str) -> Result<String, String> {
        if !vyrn_frontend::loader::is_remote(resolved) {
            return self.1.read(resolved);
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
        self.1.list_kinds(resolved)
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
    // The server's analysis thread: its stack.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
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
    let session = Session::new(true);
    let mut opts = LoadOptions {
        std_root: vyrn_frontend::manifest::std_root(),
        session: Some(session.clone()),
        ..Default::default()
    };
    let mut resolver = Resolver(None, session.clone());
    let engine = vyrn_genwasm::engine();
    let dir = std::path::Path::new(&path).parent().unwrap();
    if let Ok(Some(m)) = vyrn_frontend::manifest::find(dir) {
        opts.aliases = m.dependencies.into_iter().collect();
        opts.alias_base = m.dir.clone();
        opts.audience = m.audience;
        opts.artifacts = m.artifacts;
        resolver.0 = Some(m.dir);
    }
    // The server watches its workspace folders and the std root.
    let project = resolver
        .0
        .clone()
        .unwrap_or_else(|| dir.to_string_lossy().into_owned());
    let roots: Vec<String> = std::iter::once(project)
        .chain(opts.std_root.clone())
        .collect();
    session.watch(&roots);
    let analyze = |text: &str| {
        vyrn_frontend::analyze_judged(
            text,
            Some((&path, &opts, &resolver)),
            Some(&*engine),
            &vyrn_lower::JUDGE,
        )
    };
    for i in 0..3 {
        analyze(&edit(&src, i));
    }
    let mut ms: Vec<f64> = (0..runs)
        .map(|i| {
            let edited = edit(&src, i + 3);
            let t = std::time::Instant::now();
            analyze(&edited);
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    ms.sort_by(|a, b| a.total_cmp(b));
    let _ = vyrn_frontend::prof::phase_table();
    let _ = session.recheck_tally();
    vyrn_frontend::movecheck::reset_judgment_tally();
    analyze(&edit(&src, runs + 3));
    let (checked, replayed) = session.recheck_tally();
    let (judged, served) = vyrn_frontend::movecheck::judgment_tally();
    eprint!("{}", vyrn_frontend::prof::phase_table());
    let a = analyze(&src);
    println!(
        "best {:.1} ms  median {:.1} ms  {} diagnostics, {} memory notes, {checked} bodies checked, {replayed} replayed, {judged} judged, {served} served  {path}",
        ms[0],
        ms[ms.len() / 2],
        a.diagnostics.len(),
        a.memory.len()
    );
    for d in a.diagnostics.iter().take(3) {
        println!("      {}:{} {}", d.line, d.col, d.message);
    }
}

/// The root after edit `i` of the kind `VYRN_EDIT` names.
fn edit(src: &str, i: usize) -> String {
    match std::env::var("VYRN_EDIT").as_deref() {
        Ok("body") => {
            let at = src.rfind("\nfn ").expect("the root declares a function") + 1;
            let brace = at + src[at..].find("{\n").expect("the body opens on its line");
            format!("{} let _k{i} = {i}{}", &src[..=brace], &src[brace + 1..])
        }
        Ok("sig") if i % 2 == 0 => {
            let name = most_read(src);
            src.replacen(&format!("\nfn {name}("), &format!("\nmut fn {name}("), 1)
        }
        Ok("sig") => src.to_string(),
        Ok("param") if i % 2 == 0 => {
            let head = format!("\nfn {}(", most_read(src));
            let at = src.find(&head).expect("the function") + head.len();
            let colon = at + src[at..].find(": ").expect("a parameter") + 2;
            let end = colon + src[colon..].find([',', ')']).expect("its type ends");
            let to = if &src[colon..end] == "Int32" {
                "Int64"
            } else {
                "Int32"
            };
            format!("{}{to}{}", &src[..colon], &src[end..])
        }
        Ok("param") => src.to_string(),
        Ok("line") => format!("{}{src}", "\n".repeat(1 + i)),
        Ok("doc") => format!(
            "{src}\n/// Edit {i}.\nprotocol KeystrokeDoc {{\n  /// Edit {i}.\n  \
             fn keystrokeDoc(self) -> Int64\n}}\n"
        ),
        _ => format!("{src}\n// keystroke {i}\n"),
    }
}

/// The root function other than `main` whose name the root spells most often.
fn most_read(src: &str) -> &str {
    let names = src
        .split("\nfn ")
        .skip(1)
        .filter_map(|s| s.split('(').next());
    (names.filter(|n| *n != "main"))
        .max_by_key(|n| src.matches(&format!("{n}(")).count())
        .expect("the root declares a function other than `main`")
}
