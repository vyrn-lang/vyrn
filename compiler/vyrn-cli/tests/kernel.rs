//! RFC-0125 M2 — the named core and the linear judgment, run over the corpus.
//!
//! For every example, every function instance is lowered into the named core
//! (`vyrn_lower::core`) with the ownership plan's releases as explicit drops,
//! and the kernel (`vyrn_lower::kernel`) judges it: every owned name consumed
//! exactly once on every path. Three outcomes per instance:
//!
//!   - accepted: the plan's decisions are linear for this body;
//!   - refused: the kernel found a name released twice, used after release,
//!     or never released — either a leak the plan missed (the finding M2 exists
//!     to make) or a lowering that misread the plan (a bug in `core.rs`); the
//!     message says which name and line, and a person decides which;
//!   - unlowered: a construct this slice does not lower yet, counted by name.
//!
//! The prediction, written before the first run: the kernel refuses nothing
//! in the corpus, because the ratchet holds every example at zero leaks and
//! the plan is what the engines emit. **It was wrong, and the way it was wrong
//! is the finding.** The first slice ended at 5,292 instances accepted and 53
//! refused, in four classes (RFC-0125 §3 M2). One is closed:
//!
//!   1. CLOSED — `push` in expression position whose result escapes while the
//!      plan still releases the receiver: a double free natively, reproduced
//!      by `rfcs/probes-0125/push-in-expression-position.vyrn`. A rebuilding
//!      row takes its receiver now (`movecheck::sinks`), the write-back
//!      statement excepted; the probe runs clean and the refusals are gone.
//!   2. a value bound by `let`, returned or taken on one path and never
//!      released on another — the fall-through after a conditional `return`
//!      (`gqlArgOf`, `rpcPathFor`, `mapSlug` and more), measured by
//!      `rfcs/probes-0125/returned-on-one-path.vyrn`; and an early `return`
//!      inside a loop before the take after it (`std/von`'s readers,
//!      `jsonTopToVon`), measured by
//!      `rfcs/probes-0125/early-return-before-the-take.vyrn`, which leaks
//!      with the compiler as it was before this branch too. The plan's own
//!      fold names the second half (round forty-two: "the in-loop exits keep
//!      their leak until the fold can order across a back edge").
//!   3. a payload binder an arm never reads (`parseErr`'s `Ok(v)`) — the same
//!      class as 2, predicted and not yet probed.
//!   4. the unreachable-arm class went with class 1's fix.
//!
//! plus one instance (`smallarray.vyrn`'s `main`) this lowering misreads.
//! The gate is a ratchet on that count: it may fall, never rise. Each class
//! is closed by fixing the plan, at which point the count drops and the
//! ratchet is lowered with it. The tally of gaps is printed so the work left
//! is a list and not a feeling.
//!
//! **M3's first slice closed classes 2 and 3 from the other side.** With the
//! placer installed (`vyrn_lower::install`), the kernel runs over every body
//! in placement mode inside `own::analyze` and fills the plan's three tables
//! with what it owed: an exit-release row where a name is still held at an
//! exit, an edge row where one edge of a join holds what another took, an
//! arm row where a payload binder was never moved. The judging run then sees
//! the plan it produced. The count fell from 42 to 1, and the three probes
//! under `rfcs/probes-0125/` are flat. `VYRN_NO_PLACER=1` runs this test
//! against the analysis alone, which is how the earlier numbers were taken.
//!
//! **M2's second slice lowered every construct** (RFC-0125 §3 M2, "the
//! second slice"): module state as a borrow, `consume` holes as sub-place
//! takes the kernel tracks, the write-back receiver, map keys the store takes,
//! lambdas as reads of their captures, regions, projections, `?` on a
//! `Fallible` type. Unlowered fell from 1,229 to 0, and the kernel then
//! refused nine instances in five classes, every one a leak the plan cannot
//! express and each measured by a probe under `rfcs/probes-0125/`:
//!
//!   5. a payload binder with a hole (`reply`'s `Some(res) => consume
//!      res.body`): the arm table frees a binder whole or not at all —
//!      `payload-binder-with-a-hole.vyrn`, 53 MB at 200,000 turns, 102 MB at
//!      400,000;
//!   6. a field taken out of a temporary nobody names (`gqlParseQuery(q).sels`):
//!      the rest of the temporary is never released —
//!      `field-out-of-a-temporary.vyrn`, 35 MB and 66 MB;
//!   7. a `consume` of a sub-place on one edge of a join (`recordsFrom`,
//!      `rpcApplyConfig`): the release walk skips the hole on the path that
//!      did not take it — `consume-on-one-edge.vyrn`, 12 MB and 20 MB;
//!   8. a `for` variable one of whose fields the body takes (`twSafelist`):
//!      the handover row frees the buffer alone and the other field leaks —
//!      `for-variable-with-a-hole.vyrn`, 66 MB and 127 MB;
//!   9. a binding returned whole on one path and drained by a `consume` of
//!      one field on the other (`gqlListValue`, `gqlObjectValue`, `gqlSelSet`):
//!      the plan's fate is "moved into the return" and it places no release,
//!      so the untaken field is held at the block's end —
//!      `moved-on-one-path-holed-on-the-other.vyrn`, 20 MB and 35 MB. In the
//!      three corpus instances the untaken field is an empty error String,
//!      which costs no bytes; the probe gives it a value.
//!
//! The placer placed nothing for a name with a hole while the emitters'
//! release walk skipped only the holes the plan's own table lists for a
//! binding; a row for such a name would have freed what left through the
//! hole. The first cut placed such rows and `graphql` died natively of a use
//! after free; parity and the residue ratchet caught it.
//!
//! **M3's second slice placed them** (RFC-0125 §3 M3, "the second slice"):
//! a placed row carries its own hole set, an arm row carries the binder's,
//! an edge row may name a sub-place, a `for` variable has a key, and a taken
//! field's receiver is freed around the hole. A `for` has an exit in the
//! core now (it had none, so the kernel never judged what followed one —
//! the cross-engine generator gate found the row that rewrite placed on a
//! dead edge's word). The count fell from 9 to 3, and the five probes are
//! flat on both engines. The class left:
//!
//!  10. a heap field or element READ off a temporary nobody names
//!      (`gqlIsRecord`'s `gqlSplitDecl(src).rhs.startsWith("{")`,
//!      `arrays.vyrn`'s `weekdayLetters()[1]`, `slots.vyrn`'s
//!      `(people.get(bob) ?? Person { .. }).name`): the analysis puts the
//!      receiver in R1′'s table, both compiled backends run that row after a
//!      scalar read only, and the borrowed field outlives the read —
//!      `field-read-off-a-temporary.vyrn`, 25.9 MB and 58.7 MB natively. The
//!      lowering used to read the dead row as a `drop`; it reads it as
//!      nothing now, which is what the emitters do.
//!
//! **M3's third slice closed class 10** (RFC-0125 §3 M3, "the third
//! slice"): the placer writes an argument-temporary drop keyed by the node
//! that PRODUCED the receiver, which both backends tee and free after the
//! call or operator enclosing the read — after the read's consumer. A
//! lending call (`a[i]`, a projection) whose result owns heap no longer
//! drains its arguments' temporaries, so the tee reaches the consumer
//! above. The count fell from 3 to 0 and the probe is flat: 4.4 MB at
//! 200,000 and at 400,000 turns natively.
//!
//! **Lambda frames are judged in the same slice.** Both compiled backends
//! read a lifted lambda's rows under the enclosing function's name now, the
//! core builds each lambda as a frame of its own (`Body::lambdas`), and
//! this test judges every frame; the tally counts each as an instance.
//! `lambda-holds-on-one-path.vyrn` went from 15.2 / 25.9 MB to 4.8 / 4.8 MB.
//!
//! **Wordings, the same slice.** A refusal carries its line and file and is
//! worded as `movecheck.rs` words a move; `VYRN_KERNEL_STRICT=1` prints it
//! as `file:line:0: message`, and `VYRN_NO_MOVECHECK=1` shows it on a
//! program the checker refuses first (RFC-0125 §3 M3, "wordings").
//!
//! **Borrows bound to a place are judged (2026-09-03).** A `let` that reads
//! a heap field or element out of a place somebody owns is an alias of it;
//! a take of the alias is refused, and a write to the place ends it
//! (RFC-0125 §3 M5, "the take out of a `read` parameter";
//! `take-out-of-a-read-parameter.vyrn`, `alias-then-write-through-the-root.vyrn`).
//! Eleven corpus sites took the `.copy()` the rule names; the tally stayed
//! at 0.
//!
//! The ratchet is 0. `VYRN_KERNEL_GAPS=<substring>`
//! lists where each remaining gap is; `VYRN_KERNEL_TRACE=1` prints what the
//! placer found owed in every body, and `VYRN_KERNEL_TRACE=<fn>` prints that
//! body's core.

use std::path::PathBuf;

use vyrn_frontend::ast::Program;

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
    vyrn_frontend::load(&src, &root, &opts, &Fs).map_err(|d| {
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

#[test]
fn the_kernel_over_the_corpus() {
    // The frontend recurses deeply on a realistic program; the CLI runs it on
    // a thread with the interpreter's reserve, and so does this.
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::interp::INTERP_STACK_BYTES)
        .spawn(run_corpus)
        .unwrap()
        .join()
        .unwrap();
}

fn run_corpus() {
    // The placer (RFC-0125 M3) is installed unless the run is asked to judge
    // the analysis alone, which is how the first slice's numbers were taken.
    if std::env::var("VYRN_NO_PLACER").is_err() {
        vyrn_lower::install();
    }
    let mut accepted = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut gaps: std::collections::BTreeMap<&'static str, usize> = Default::default();
    let mut details: std::collections::BTreeMap<(&'static str, String), usize> = Default::default();
    let dump = std::env::var("VYRN_KERNEL_DUMP").ok();
    // `VYRN_KERNEL_GAPS=<substring of a gap's name>` prints where each such
    // gap is, one `file:fn:line` per instance, so a construct can be read
    // in the source before it is modelled.
    let show_gaps = std::env::var("VYRN_KERNEL_GAPS").ok();
    let mut unloadable = 0usize;
    let mut programs = 0usize;
    for path in corpus() {
        let program = match load(&path) {
            Ok(p) => p,
            Err(_) => {
                unloadable += 1;
                continue;
            }
        };
        programs += 1;
        // Projections are expanded once per compile (RFC-0123); the lowering
        // and the ownership analysis must see the same expansion.
        let _memo = vyrn_frontend::project::Memo::open();
        let lowered = vyrn_lower::lower(&program);
        let own = vyrn_frontend::own::analyze(&program);
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        // The module-state initializer (RFC-0013) is a body and no instance:
        // every `let` at module scope is a store into the global it names.
        // The kernel judges it and the lambda frames it holds like any other
        // (RFC-0125 §3 M6, third slice).
        if !program.globals.is_empty() {
            match vyrn_lower::core::build_module_state(&program, &own, &lowered.globals) {
                Err(g) => {
                    // A rule the core states is a refusal, not a gap
                    // (RFC-0125 §3 M3, the checker's deletion path).
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
                        match vyrn_lower::kernel::check(body) {
                            Ok(()) => accepted += 1,
                            Err(r) => refused.push(format!(
                                "{file}: <module state> {}: line {}: {}",
                                r.body,
                                r.line,
                                r.message.replace('\n', " / ")
                            )),
                        }
                    }
                }
            }
        }
        for inst in &lowered.instances {
            match vyrn_lower::core::build(&program, inst, &own) {
                Err(g) => {
                    // A rule the core states is a refusal, not a gap: a
                    // corpus program that reaches one moves the ratchet
                    // (RFC-0125 §3 M3, the checker's deletion path).
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
                // The body and every lambda frame under it (RFC-0125 M3,
                // third slice), each judged as a frame of its own.
                Ok(top) => {
                    for body in top.frames() {
                        match vyrn_lower::kernel::check(body) {
                            Ok(()) => accepted += 1,
                            Err(r) => {
                                let tag = format!("{file}:{}", body.name);
                                if dump
                                    .as_deref()
                                    .is_some_and(|d| d.split(',').any(|w| tag.contains(w)))
                                {
                                    eprintln!("{}", body.render());
                                    let arms: Vec<String> = own
                                        .plan
                                        .arm_frees
                                        .iter()
                                        .filter(|((at, _), _)| {
                                            own.plan.owners.get(at) == Some(&inst.func.name)
                                        })
                                        .map(|((_, arm), k)| format!("arm {arm}: {k:?}"))
                                        .collect();
                                    eprintln!("  plan arm frees: {}", arms.join("; "));
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
                                    r.line,
                                    r.message.replace('\n', " / ")
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
        "kernel over the corpus: {programs} programs ({unloadable} not loadable here), \
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
    // The ratchet: 53 at the end of the first slice; 42 once class 1 closed;
    // 1 once the placer (M3's first slice) filled the plan's three tables; 9
    // once every construct lowered and the five hole-family classes above
    // came into view (the one lowering misread, `smallarray`, is fixed); 3
    // once the placer could place a holed name and a `for` had its exit
    // (M3's second slice); 0 once the receiver of a borrowed heap read was
    // an argument temporary of the read's consumer (M3's third slice).
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
