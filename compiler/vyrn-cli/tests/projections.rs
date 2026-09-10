//! RFC-0092 M1/M3 — the regression guard. How many sites the rule "a projection
//! is a borrow of its root, whatever the root is" refuses over the whole corpus.
//!
//! It **links and checks**. `movecheck.rs`'s two earlier measurements parse each
//! file alone, which is the reading Phase 4b got wrong by 81 sites: a file read
//! on its own cannot name an imported type, so `owns_heap` answers "unknown" and
//! the site disappears. Every `.vyrn` under `examples/` and `std/` is loaded as
//! a root, and a site is counted once per (file, line, path, kind) however many
//! roots reach the module it lives in.
//!
//! **A root the compiler REFUSES contributes no site.** The corpus keeps one
//! witness per refusal (`tests/refusals.rs`), and a site inside a witness is the
//! rule working, not a regression.
//!
//! **This is why the guard lives here and not in `vyrn-frontend`.** The rule it
//! guards is the KERNEL's — `vyrn_lower::kernel` writes "`ks[i]` may not be
//! stored into `m`" — and `vyrn-frontend` sits below `vyrn-lower`, so a
//! `--lib` test there cannot install the judgment and cannot tell an accepted
//! program from a refused one. It read one element store back for that reason:
//! `examples/mapkeyborrowed.vyrn`, the witness for this very rule.
//!
//! Three numbers, and the third is inside the first two: stores of a projection,
//! returns of a projection, and how many of each name a type that owns no heap —
//! the rule does not reach those and they cost nothing.
//!
//! Ignored by default: it reads the repository, links it and checks it. Run it
//! with `cargo test -p vyrn-cli --test projections -- --ignored --nocapture`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use vyrn_frontend::loader::DiskResolver;
use vyrn_frontend::movecheck::{projection_sites, ProjectionSite};

#[test]
#[ignore = "walks, links and checks the whole corpus; run explicitly"]
fn rfc0092_projection_sites_over_the_corpus() {
    // The frontend recurses deeply on a realistic program; the CLI runs it on a
    // thread with the interpreter's reserve, and so does this.
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(count)
        .unwrap()
        .join()
        .unwrap();
}

/// Every `.vyrn` under a repo-relative directory, in sorted order.
fn sources(rel: &str, out: &mut Vec<PathBuf>) {
    let mut stack = vec![repo().join(rel)];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "vyrn") {
                out.push(p);
            }
        }
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Canonical, slash-separated, and the same spelling the loader resolves an
/// import to. A root passed in as `<crate>/../../std/x.vyrn` and the same file
/// reached through an import are two strings for one file, and the count would
/// double every module every root reaches.
fn slashed(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .to_string_lossy()
        .replace('\\', "/")
        .replace("//?/", "")
}

fn count() {
    // A corpus example may import through a generator, and generation is the
    // DRIVER's engine (RFC-0125 §3 M5). The placer is what states the rule this
    // guard measures. Both installations are idempotent.
    vyrn_genwasm::install();
    vyrn_lower::install();

    let std_root = slashed(&repo().join("std"));
    let repo_prefix = format!("{}/", slashed(&repo()));

    let mut files = Vec::new();
    sources("examples", &mut files);
    sources("std", &mut files);
    files.sort();

    let mut seen: HashSet<(String, usize, String, &'static str)> = HashSet::new();
    let mut rows: Vec<(String, ProjectionSite)> = Vec::new();
    let (mut accepted, mut unlinkable, mut refused) = (0, Vec::new(), Vec::new());
    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        let root_key = slashed(path);
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some(std_root.clone()),
            ..Default::default()
        };
        // The sites are read off the LINKED program, before the synthesis: the
        // JSON encoders `check_and_synthesize` appends are the compiler's own
        // Vyrn and not a site anybody wrote. The accepted answer comes from the
        // full load beside it, which is the whole compiler's.
        let Ok(program) = vyrn_frontend::loader::load(&src, &root_key, &opts, &DiskResolver) else {
            unlinkable.push(root_key);
            continue;
        };
        if vyrn_frontend::load(&src, &root_key, &opts, &DiskResolver).is_err() {
            refused.push(root_key);
            continue;
        }
        accepted += 1;
        for s in projection_sites(&program) {
            let file = s.module.clone().unwrap_or_else(|| root_key.clone());
            let key = (file.clone(), s.line, s.path.clone(), s.kind);
            if seen.insert(key) {
                rows.push((file, s));
            }
        }
    }
    rows.sort_by(|a, b| (&a.0, a.1.line).cmp(&(&b.0, b.1.line)));

    let tally = |kind: &str, heap: bool| {
        rows.iter()
            .filter(|(_, s)| s.kind == kind && s.owns_heap == heap)
            .count()
    };
    let unknown = |kind: &str| {
        rows.iter()
            .filter(|(_, s)| s.kind == kind && s.ty == "?")
            .count()
    };
    let (stores, returns) = (tally("store", true), tally("return", true));
    println!(
        "corpus: {} files, {accepted} accepted ({} would not link, {} refused)",
        files.len(),
        unlinkable.len(),
        refused.len()
    );
    for f in &unlinkable {
        println!("    not linked: {f}");
    }
    for f in &refused {
        println!("    refused: {f}");
    }
    println!("RFC-0092 projection sites over the corpus");
    println!("  stores:  {stores}  (+{} scalar)", tally("store", false));
    println!("  returns: {returns}  (+{} scalar)", tally("return", false));
    println!("  total:   {}", stores + returns);
    println!(
        "  unnameable even linked, so counted as scalar: {} store, {} return",
        unknown("store"),
        unknown("return")
    );
    // M1 widened `store` and `returned_borrow` to see an element read, so these
    // are refused like the field they are. Still counted apart, because M0
    // counted them apart and the two numbers have to stay comparable.
    println!(
        "  element reads: {} store (+{} scalar), {} return (+{} scalar)",
        tally("elem-store", true),
        tally("elem-store", false),
        tally("elem-return", true),
        tally("elem-return", false)
    );
    for (file, s) in &rows {
        let name = file.strip_prefix(&repo_prefix).unwrap_or(file);
        let cost = if s.owns_heap { "*" } else { " " };
        println!(
            "  {cost} {:<7} {name}:{} {}: `{}` -> {} [{}]",
            s.kind, s.line, s.func, s.path, s.into, s.ty
        );
    }
    // M1's regression guard. Every store the rule refuses is migrated, and the
    // corpus compiles, so the instrument reads zero for all three store classes
    // and for an element return. A site that reappears fails here with its file
    // and line already printed above.
    assert_eq!(stores, 0, "RFC-0092 M1: a projection store came back");
    assert_eq!(
        tally("elem-store", true),
        0,
        "RFC-0092 M1: an element store came back"
    );
    assert_eq!(
        tally("elem-return", true),
        0,
        "RFC-0092 M1: an element return came back"
    );
    // **M1 left seven of these and M3 closed all seven**, in the change that gave
    // the row — which is what M1 said would happen and why it asserted the number
    // rather than migrating them early.
    //
    // They were all one shape: `return match hit { Some(r) => r, .. }` on an
    // owned `Option<Response>` or `Option<Cargo>`. `check_return` refuses an
    // arm-yielded projection only where the caller RELEASES the result (Phase
    // 4b's guard, which this RFC does not move), and a record had no release
    // rule, so they could not dangle. M3 gives `Type::Record` its row and they
    // can.
    //
    // The fix is not the copy M1 priced and refused to pay. RFC-0093 M1 shipped
    // the take in between, so each of them reads `match consume hit { .. }`: the
    // arm yields a value the frame gave up, and nothing is copied at all. Six
    // are one line of the `pages` generator.
    assert_eq!(
        returns, 0,
        "RFC-0092 M3: a projection return came back — see the list above"
    );
}
