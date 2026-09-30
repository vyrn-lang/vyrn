//! The named core and the linear judgment over the corpus.
//!
//! Every function instance, module-state initializer, `test` and `bench` body, and
//! lambda frame is lowered into the named core (`vyrn_lower::core`) with the plan's
//! releases as explicit drops, and the kernel (`vyrn_lower::kernel`) judges it: every
//! owned name is consumed exactly once on every path. A refusal is either a leak the
//! plan missed or a lowering that misread the plan; a person decides which. The
//! refusal count is a ratchet: it may fall, never rise. An example the load refuses is
//! left out only when `examples/expected/<name>.stderr` records that refusal; any other
//! load failure fails the suite. `VYRN_KERNEL_GAPS=<substring>`
//! lists where each unlowered construct is; `VYRN_KERNEL_TRACE=1` prints what the
//! placer found owed in every body, and `VYRN_KERNEL_TRACE=<fn>` prints that body's core.

use vyrn_frontend::loader::DiskResolver;

use std::path::PathBuf;

use vyrn_frontend::ast::Program;

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
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    // Without the engine, an example that imports through a generator fails to
    // load, and the gate measures a smaller corpus.
    let engine = vyrn_genwasm::engine();
    vyrn_lower::load(&src, &root, &opts, &DiskResolver, Some(&*engine)).map_err(|d| {
        d.first()
            .map(|d| d.render())
            .unwrap_or_else(|| "load failed".into())
    })
}

fn corpus() -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found");
    names
}

/// Whether `examples/expected/<stem>.stderr` records the refusal `err` names: the
/// sentence after `line N: ` in the load's first diagnostic.
fn fixture_records(path: &std::path::Path, err: &str) -> bool {
    let first = err.lines().next().unwrap_or("");
    let sentence = first
        .strip_prefix("line ")
        .and_then(|rest| rest.split_once(": "))
        .map_or(first, |(_, s)| s);
    let stem = path.file_stem().unwrap().to_string_lossy();
    let expected = repo_root()
        .join("examples/expected")
        .join(format!("{stem}.stderr"));
    std::fs::read_to_string(expected).is_ok_and(|s| !sentence.is_empty() && s.contains(sentence))
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test kernel -- --ignored"]
fn the_kernel_over_the_corpus() {
    // The frontend recurses deeply; run it on a thread with the CLI's stack reserve.
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run_corpus)
        .unwrap()
        .join()
        .unwrap();
}

fn run_corpus() {
    let mut accepted = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut gaps: std::collections::BTreeMap<&'static str, usize> = Default::default();
    let mut details: std::collections::BTreeMap<(&'static str, String), usize> = Default::default();
    let dump = std::env::var("VYRN_KERNEL_DUMP").ok();
    let show_gaps = std::env::var("VYRN_KERNEL_GAPS").ok();
    let mut unloadable = 0usize;
    let mut unexpected: Vec<String> = Vec::new();
    let mut programs = 0usize;
    for path in corpus() {
        let program = match load(&path) {
            Ok(p) => p,
            Err(e) => {
                // A program the load refuses is left out only when its fixture
                // records that refusal: otherwise a new refusal would shrink the
                // corpus instead of failing the suite.
                if !fixture_records(&path, &e) {
                    unexpected.push(format!(
                        "{}: {}",
                        path.file_name().unwrap().to_string_lossy(),
                        e.lines().next().unwrap_or("")
                    ));
                }
                unloadable += 1;
                continue;
            }
        };
        programs += 1;
        let lowered = vyrn_lower::lower(&program);
        let world = vyrn_lower::analyze(&program);
        let own = &world.ownership;
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        // The module-state initializer is a body but no instance: every `let` at
        // module scope is a store into the global it names.
        if !program.globals.is_empty() {
            match vyrn_lower::core::build_module_state(
                &program,
                &own,
                &Default::default(),
                &lowered.globals,
            ) {
                Err(g) => {
                    // A rule the core states is a refusal, not a gap.
                    if let Some(m) = &g.rule {
                        refused.push(format!("{file}: <module state>: line {}: {m}", g.line));
                        continue;
                    }
                    if show_gaps.as_deref().is_some_and(|w| g.what.contains(w)) {
                        eprintln!(
                            "  gap: {file} <module state>:{} {} {}",
                            g.line, g.what, g.detail
                        );
                    }
                    *gaps.entry(g.what).or_default() += 1;
                }
                Ok(top) => {
                    for body in top.frames() {
                        match vyrn_lower::kernel::check(body, &Default::default()) {
                            Ok(()) => accepted += 1,
                            Err(r) => refused.push(format!(
                                "{file}: <module state> {}: line {}: {}",
                                r.body,
                                r.diagnostic.line,
                                r.diagnostic.message.replace('\n', " / ")
                            )),
                        }
                    }
                }
            }
        }
        // A `test` or `bench` body is a body but no instance: neither is a function.
        for ob in &lowered.bodies {
            match vyrn_lower::core::build_outside(
                &program,
                &own,
                &Default::default(),
                &mut Default::default(),
                ob,
            ) {
                Err(g) => {
                    if let Some(m) = &g.rule {
                        refused.push(format!("{file}: {}: line {}: {m}", ob.name, g.line));
                        continue;
                    }
                    if show_gaps.as_deref().is_some_and(|w| g.what.contains(w)) {
                        eprintln!(
                            "  gap: {file} {}:{} {} {}",
                            ob.name, g.line, g.what, g.detail
                        );
                    }
                    *gaps.entry(g.what).or_default() += 1;
                }
                Ok(top) => {
                    for body in top.frames() {
                        match vyrn_lower::kernel::check(body, &Default::default()) {
                            Ok(()) => accepted += 1,
                            Err(r) => refused.push(format!(
                                "{file}: {}: line {}: {}",
                                r.body,
                                r.diagnostic.line,
                                r.diagnostic.message.replace('\n', " / ")
                            )),
                        }
                    }
                }
            }
        }
        for inst in &lowered.instances {
            match vyrn_lower::core::build(&program, inst, &own) {
                Err(g) => {
                    if let Some(m) = &g.rule {
                        refused.push(format!("{file}: {}: line {}: {m}", inst.spelling(), g.line));
                        continue;
                    }
                    if show_gaps.as_deref().is_some_and(|w| g.what.contains(w)) {
                        eprintln!(
                            "  gap: {file} {}:{}:{} {} {}",
                            inst.module(),
                            inst.spelling(),
                            g.line,
                            g.what,
                            g.detail
                        );
                    }
                    *gaps.entry(g.what).or_default() += 1;
                    if !g.detail.is_empty() {
                        *details.entry((g.what, g.detail)).or_default() += 1;
                    }
                }
                Ok(top) => {
                    for body in top.frames() {
                        match vyrn_lower::kernel::check(body, &Default::default()) {
                            Ok(()) => accepted += 1,
                            Err(r) => {
                                let tag = format!("{file}:{}", body.name);
                                if dump
                                    .as_deref()
                                    .is_some_and(|d| d.split(',').any(|w| tag.contains(w)))
                                {
                                    eprintln!("{}", body.render());
                                    let rel: Vec<String> = inst
                                        .releases
                                        .iter()
                                        .map(|r| format!("{}@{:?}:{}", r.name, r.exit, r.line))
                                        .collect();
                                    eprintln!("  plan releases: {}", rel.join("; "));
                                }
                                refused.push(format!(
                                    "{file}: {}: line {}: {}",
                                    r.body,
                                    r.diagnostic.line,
                                    r.diagnostic.message.replace('\n', " / ")
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    let total_gaps: usize = gaps.values().sum();
    eprintln!(
        "kernel over the corpus: {programs} programs (and {unloadable} refused at load, as \
         their fixtures record), \
         {accepted} instances accepted, {} refused, {total_gaps} unlowered",
        refused.len()
    );
    for (what, n) in &gaps {
        eprintln!("  unlowered: {n:5}  {what}");
    }
    let mut top: Vec<(&(&'static str, String), &usize)> = details.iter().collect();
    top.sort_by(|a, b| b.1.cmp(a.1));
    for ((what, detail), n) in top.iter().take(25) {
        eprintln!("    {n:5}  {what}: {detail}");
    }
    for r in refused.iter().take(40) {
        eprintln!("  refused: {r}");
    }
    assert!(
        unexpected.is_empty(),
        "{} examples failed to load, and their fixtures in examples/expected/ record \
         no such refusal:\n  {}",
        unexpected.len(),
        unexpected.join("\n  ")
    );
    const RATCHET: usize = 0;
    assert!(
        refused.len() <= RATCHET,
        "{} instances refused by the kernel, more than the {RATCHET} recorded; the first new          one is worth reading before the number is raised: {}",
        refused.len(),
        refused[0]
    );
    assert!(
        accepted > 0,
        "the kernel accepted nothing, so it judged nothing"
    );
}

/// A judgment is cached under the body's module, that module's content hash and the
/// instance's spelling, under a fingerprint of every declaration with function bodies
/// left out; so an edit inside one function re-judges only its module's bodies.
#[test]
fn one_edit_re_judges_one_body() {
    vyrn_frontend::movecheck::reuse_judgments();
    let dir = std::env::temp_dir().join(format!("vyrn-judgmemo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).expect("write");
    write(
        "b.vyrn",
        "export fn bOne(x: Int64) -> Int64 { return x + 2 }\n",
    );
    write(
        "main.vyrn",
        "import { aOne } from \"./a\"\nimport { bOne } from \"./b\"\n\
         fn main() -> Int64 { return aOne(1) + bOne(2) }\n",
    );
    // The load states the refusals itself (`check_and_synthesize`), so a load is
    // the keystroke this counts.
    let run = || {
        vyrn_frontend::movecheck::reset_judgment_tally();
        load(&dir.join("main.vyrn")).expect("the program loads");
        vyrn_frontend::movecheck::judgment_tally()
    };

    write(
        "a.vyrn",
        "export fn aOne(x: Int64) -> Int64 { return x + 1 }\n",
    );
    let (cold, served_cold) = run();
    assert!(cold > 0, "the first run judges every body it can key");
    assert_eq!(served_cold, 0, "nothing is served on the first run");

    let (idle, served_idle) = run();
    assert!(
        served_idle > 0,
        "a run that edits nothing is served its bodies"
    );

    // The same signature and the same line: only the body moves, so the
    // fingerprint stands and `a`'s content hash does not.
    write(
        "a.vyrn",
        "export fn aOne(x: Int64) -> Int64 { return x + 1 + 0 }\n",
    );
    let (edited, _) = run();
    assert_eq!(
        edited,
        idle + 1,
        "an edit inside one function re-judges that function's body and no other \
         (idle {idle}, edited {edited}, cold {cold})"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Serving a body skips its placement as well as its judgment; that is sound because
/// placed rows have no reader in a host that armed the memo
/// ([`vyrn_frontend::movecheck::reuse_judgments`]). The string interpolation injects
/// `std/text`, whose `decodeUtf8` and `test` block are imported bodies the placer
/// writes a row for.
#[test]
fn a_placed_row_does_not_stop_a_body_being_served() {
    vyrn_frontend::movecheck::reuse_judgments();
    let dir = std::env::temp_dir().join(format!("vyrn-judgrows-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).expect("write");
    write(
        "b.vyrn",
        "export fn bTwo(x: Int64) -> Int64 { return x + 2 }\n",
    );
    let run = || {
        vyrn_frontend::movecheck::reset_judgment_tally();
        load(&dir.join("main.vyrn")).expect("the program loads");
        vyrn_frontend::movecheck::judgment_tally()
    };
    let root = |tail: &str| {
        write(
            "main.vyrn",
            &format!(
                "import {{ bTwo }} from \"./b\"\nfn main() -> Int64 {{\n  let s = \
                 \"n=${{bTwo(2)}}{tail}\"\n  return s.byteLength\n}}\n"
            ),
        )
    };

    root("");
    let (cold, served_cold) = run();
    assert!(cold > 0, "the first run judges every body it can key");
    assert_eq!(served_cold, 0, "nothing is served on the first run");

    // The root is the module a keystroke changes; its own bodies have no key and
    // are built every time.
    for tail in ["!", "!!"] {
        root(tail);
        let (judged, served) = run();
        assert_eq!(
            judged, 0,
            "an edit in the root re-judges no imported body (served {served})"
        );
        assert_eq!(
            served, cold,
            "every body the cold run judged is served (cold {cold})"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// One body of `n` blocks. Each binds a `String` that one edge of an `if`
/// consumes and a counter a `while` walks, so no fact outlives its block.
fn copies(n: usize) -> String {
    let mut src = String::from(
        "fn take(v: consume String) -> Int64 {\n    drop v\n    return 0\n}\n\n\
         fn f(flag: Bool) -> Int64 {\n    let mut acc = 0\n",
    );
    for k in 0..n {
        src += &format!(
            "    if flag {{\n        let s{k} = \"ab\" + \"cd\"\n        if flag {{\n            \
             acc = acc + take(consume s{k})\n        }} else {{\n            \
             acc = acc + s{k}.byteLength\n        }}\n        let mut i{k} = 0\n        \
             while i{k} < 2 {{\n            acc = acc + i{k}\n            i{k} = i{k} + 1\n        \
             }}\n    }}\n"
        );
    }
    src + "    return acc\n}\n\nfn main() -> Int64 {\n    print(\"\\{f(true)}\")\n    return 0\n}\n"
}

/// The kernel's row of `vyrn check --profile <file>`, in seconds.
fn kernel_secs(file: &std::path::Path) -> f64 {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .args(["check", "--profile"])
        .arg(file)
        .output()
        .expect("run vyrn check");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    let row = (err.lines())
        .find(|l| l.starts_with("placer: kernel::placement"))
        .expect("a kernel row");
    let mut words = row.split_whitespace().rev();
    let unit = words.next().expect("a unit");
    let value: f64 = words.next().expect("a value").parse().expect("a number");
    value
        * match unit {
            "s" => 1.0,
            "ms" => 1e-3,
            "\u{b5}s" => 1e-6,
            "ns" => 1e-9,
            u => panic!("unit {u}"),
        }
}

/// The kernel's cost follows the facts at each join, not the names the body
/// declares: eight times the blocks costs about eight times the time, where a
/// state sized to every name costs sixty-four.
#[test]
#[ignore = "times the kernel; run explicitly: cargo test --release -p vyrn-cli --test kernel -- --ignored copies"]
fn the_kernel_is_linear_in_copies() {
    let dir = std::env::temp_dir().join(format!("vyrn-copies-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    let best = |n: usize| {
        let file = dir.join(format!("copies{n}.vyrn"));
        std::fs::write(&file, copies(n)).expect("write");
        (0..3).map(|_| kernel_secs(&file)).fold(f64::MAX, f64::min)
    };
    let (small, large) = (best(100), best(800));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        large < 16.0 * small,
        "100 blocks: {small:.4} s, 800 blocks: {large:.4} s"
    );
}
