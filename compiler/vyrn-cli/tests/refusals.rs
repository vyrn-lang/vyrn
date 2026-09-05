//! RFC-0125 §3 M3, the census: every refusal the move checker can give, one
//! minimal program each, and what the kernel says about the same program.
//!
//! The deletion slice after this one takes `movecheck.rs`'s placement code
//! away. It may take away only what something else states. So each row here
//! is one refusal site — a `menu(..)` or a `Diagnostic::error(..)` in
//! `movecheck.rs`, or a guard in `checker.rs` the close-out attributed to the
//! move check — with the program that reaches it, the rule's RFC, and the
//! column the whole census exists for: whether the kernel gives the same
//! answer today.
//!
//! Two runs per row. `vyrn check` is the checker's answer, and it must be the
//! wording in the table. `VYRN_NO_MOVECHECK=1 vyrn check` stands the checker
//! aside so the kernel's own sentence is reachable — the checker refuses each
//! of these first, so without the knob the kernel is never asked (RFC-0125 §3
//! M3, "wordings"). The kernel's refusal needs no flag of its own since the
//! default slice: the second run is the licence the deletion track reads, and
//! a row that says `the same` or `its own words` is a rule `movecheck.rs`
//! may lose without the program it refuses becoming one the compiler accepts.
//!
//! The column has four values, and each is asserted:
//!
//!   - [`Kernel::Same`] — the kernel prints the checker's sentence at the same
//!     file and line, minus the `fix:` menu. The menu names `.copy()` and
//!     write-back as ways out, which is the checker's knowledge of the surface
//!     and not the kernel's; it is the next slice's.
//!   - [`Kernel::Other`] — the kernel refuses the program, in words of its
//!     own. A row here is not closed: the sentence a reader gets would change
//!     the day the checker goes.
//!   - [`Kernel::No`] — the kernel accepts the program. The rule has no kernel
//!     equivalent, and `movecheck.rs` cannot lose this site yet.
//!   - [`Kernel::Elsewhere`] — the refusal is not the move check's at all: it
//!     survives `VYRN_NO_MOVECHECK=1` because another pass gives it. Nothing
//!     is owed here, and the close-out's attribution is corrected.
//!
//! A row whose site has already LEFT `movecheck.rs` — rows 12, 08, 09, 04, 05,
//! 28 and 06, RFC-0125 §3 M3 — is refused by the kernel in both runs, and the
//! two must still agree. The row is what stops the sentence moving after the
//! deletion, so it stays in the census. Rows 20 and 21 read `Kernel::Same`
//! and their sites STAYED: `examples/mustuse_abandoned.vyrn` is a program the
//! two passes word differently, and it is not in this file's corpus.

use std::path::{Path, PathBuf};

mod common;
use common::vyrn;

/// What the kernel says about a program the checker refuses.
#[derive(PartialEq, Eq, Debug)]
enum Kernel {
    /// The checker's sentence, at the same line, minus the menu.
    Same,
    /// A refusal of its own. The needle is what its message must contain.
    Other(&'static str),
    /// Nothing: the program compiles for the kernel.
    No,
    /// Another pass gives it, so the move check is not its only source.
    Elsewhere,
}

/// One row: the program, the rule, its RFC, the checker's sentence (the whole
/// message, menu excluded), and the kernel's column.
struct Row {
    file: &'static str,
    rule: &'static str,
    rfc: &'static str,
    says: &'static str,
    kernel: Kernel,
}

const fn row(
    file: &'static str,
    rule: &'static str,
    rfc: &'static str,
    says: &'static str,
    kernel: Kernel,
) -> Row {
    Row {
        file,
        rule,
        rfc,
        says,
        kernel,
    }
}

/// The census. One row per refusal site; the table in RFC-0125 §3 M3 is this
/// list, and the two are kept in step by hand — the RFC is prose and this is
/// the assertion.
fn census() -> Vec<Row> {
    vec![
        row(
            "r01_store_element.vyrn",
            "rule 2: an element read may not be stored",
            "RFC-0092",
            "`b.xs[0]` may not be stored into `push(..)` — it is read out of a place that owns it",
            Kernel::Other("read out of `b.xs[..]`, a place that owns it"),
        ),
        row(
            "r02_store_read_parameter_field.vyrn",
            "rule 2: a field of a `read` parameter may not be stored",
            "RFC-0089",
            "`h.meta[0]` may not be stored into `push(..)` — it is a `read` parameter",
            Kernel::Other("read out of `h.meta[..]`, a place that owns it"),
        ),
        row(
            "r03_store_projection.vyrn",
            "rule 2: a projection is a borrow of its root, whatever the root is",
            "RFC-0092",
            "`d.title` may not be stored into `push(..)` — it is read out of a place that owns it",
            Kernel::Other("read out of `d.title`, a place that owns it"),
        ),
        row(
            "r04_whole_after_a_hole.vyrn",
            "a name with a hole may not be used whole",
            "RFC-0093",
            "`p.name` was taken out of `p` here\nline 10: ... and `p` is used as a whole here, \
             with the hole still in it",
            Kernel::Same,
        ),
        row(
            "r05_alias_then_write.vyrn",
            "a write to a place ends every alias that reads out of it",
            "RFC-0090",
            "`t.xs[..]` is written here while `before` still reads out of it\nline 9: ... and \
             `before` is used again here",
            Kernel::Same,
        ),
        row(
            "r06_use_after_consume.vyrn",
            "rule 1: a `consume` parameter takes ownership",
            "RFC-0089",
            "`x` is used here but was already consumed by `take(..)` on line 8\n  (a `consume` \
             parameter takes ownership; the value can't be used afterward)",
            Kernel::Same,
        ),
        row(
            "r07_moved_into_a_binding.vyrn",
            "rule 1: a move into a binding, and a use of the source after it",
            "RFC-0089",
            "`s` was moved here into the binding `t`\nline 5: ... and `s` is used again here",
            Kernel::Same,
        ),
        row(
            "r08_take_an_element.vyrn",
            "`consume` reaches a field, never an element",
            "RFC-0093",
            "`xs[0]` may not be taken — an element is not a place a take reaches",
            Kernel::Same,
        ),
        row(
            "r09_nothing_to_take.vyrn",
            "`consume` with nothing to take",
            "RFC-0093",
            "`consume` here has nothing to take — the value is already owned, so there is no \
             place to leave a hole in",
            Kernel::Same,
        ),
        row(
            "r10_consume_module_state.vyrn",
            "module state may not be taken: a prefix `consume`",
            "RFC-0013",
            "module state `names` may not be consumed by a take — nothing may take ownership of \
             module state (it lives for the whole module and is never dropped)",
            Kernel::Other("module state `names` may not be passed to a `consume` parameter"),
        ),
        row(
            "r11_consume_a_read_parameter.vyrn",
            "rule 2: a prefix `consume` of a `read` parameter",
            "RFC-0089",
            "`ys` may not be consumed — it is a `read` parameter",
            Kernel::Other("via `take(..)` — it is a `read` parameter"),
        ),
        row(
            "r12_module_state_to_a_consume_parameter.vyrn",
            "module state may not be taken: a `consume` parameter",
            "RFC-0013",
            "module state `names` may not be passed to a `consume` parameter via `take(..)` — \
             nothing may take ownership of module state (it lives for the whole module and is \
             never dropped)",
            Kernel::Same,
        ),
        row(
            "r13_read_parameter_to_a_consume_parameter.vyrn",
            "rule 2: a whole `read` parameter to a `consume` parameter",
            "RFC-0089",
            "`ys` may not be passed to a `consume` parameter via `take(..)` — it is a `read` \
             parameter",
            Kernel::Same,
        ),
        row(
            "r14_projection_to_a_consume_parameter.vyrn",
            "rule 2: a projection to a `consume` parameter",
            "RFC-0092",
            "`d.title` may not be passed to a `consume` parameter via `take(..)` — it is read \
             out of a place that owns it",
            Kernel::Other("read out of `d.title`, a place that owns it"),
        ),
        row(
            "r15_return_module_state.vyrn",
            "module state may not be taken: a `return`",
            "RFC-0013",
            "`names` may not be returned — it is module state, which nothing may take, and a \
             return is owned",
            Kernel::Same,
        ),
        row(
            "r16_return_a_field_of_a_read_parameter.vyrn",
            "rule 2 at the return: a field of a `read` parameter",
            "RFC-0089",
            "`d.title` may not be returned — it is a `read` parameter, and a return is owned",
            Kernel::Other("read out of `d.title`, a place that owns it"),
        ),
        row(
            "r17_export_returns_a_borrow.vyrn",
            "an exported function owns its result",
            "RFC-0012",
            "`s` may not be returned from an exported function — it is a `read` parameter, and \
             the JS caller releases what it is handed",
            Kernel::Same,
        ),
        row(
            "r18_return_a_read_parameter.vyrn",
            "rule 2 at the return: a whole `read` parameter",
            "RFC-0089",
            "`ys` may not be returned — it is a `read` parameter, and a return is owned",
            Kernel::Same,
        ),
        row(
            "r19_read_parameter_wrapped_in_the_result.vyrn",
            "rule 2 through a wrapper: a `read` parameter put into the result",
            "RFC-0089",
            "`s` may not be put into `Some(..)` — it is a `read` parameter",
            Kernel::Other("via `Some(..)` — it is a `read` parameter"),
        ),
        row(
            "r20_drop_after_consume.vyrn",
            "rule 1 at the drop: what a `consume` parameter took is gone",
            "RFC-0089",
            "`a` is dropped here but was already consumed by `take(..)` on line 6",
            Kernel::Same,
        ),
        row(
            "r21_drop_a_borrow.vyrn",
            "rule 4 at the drop: the place that owns a value releases it",
            "RFC-0089",
            "`owned` may not be dropped — it is read out of a place that owns it",
            Kernel::Same,
        ),
        row(
            "r22_drop_with_a_hole.vyrn",
            "`drop` releases the whole binding, and a take left a hole",
            "RFC-0093",
            "`p` may not be dropped — `p.name` was taken out of it on line 17, and `drop` \
             releases the whole binding",
            Kernel::No,
        ),
        row(
            "r23_modify_is_exclusive.vyrn",
            "a `modify` borrow is exclusive",
            "RFC-0090",
            "`a` is passed to `bump` as `modify` and read again in the same call — a `modify` \
             borrow is exclusive",
            Kernel::No,
        ),
        row(
            "r24_capture_that_outlives_the_call.vyrn",
            "a closure that outlives the call may not capture a borrow",
            "RFC-0037",
            "`s` may not be captured by a closure that outlives this call — it is a `read` \
             parameter",
            Kernel::No,
        ),
        row(
            "r25_consume_inside_a_loop.vyrn",
            "rule 1 across a back edge",
            "RFC-0089",
            "`x` is consumed by `take(..)` inside a loop, so it would be used again on the next \
             iteration",
            Kernel::Same,
        ),
        row(
            "r26_rebuild_a_borrowed_receiver.vyrn",
            "a rebuilding builtin takes its receiver",
            "RFC-0125",
            "`mt` is read out of `h.meta` here — a place that owns it\nline 7: ... and \
             `push(..)` takes `mt`, so `mt` must be a value of its own",
            Kernel::Same,
        ),
        row(
            "r27_borrow_to_a_builtin_consume.vyrn",
            "rule 2: a `read` parameter to a builtin that declares `consume`",
            "RFC-0089",
            "`xs` may not be stored into `fromArray(..)` — it is a `read` parameter",
            Kernel::Other("via `fromArray(..)` — it is a `read` parameter"),
        ),
        row(
            "r28_return_a_capture_from_a_closure.vyrn",
            "a closure's result is its caller's, and a capture is not its to give",
            "RFC-0037",
            "`s` may not be returned from a closure — it is a captured binding, and the \
             closure's result is its caller's",
            Kernel::Same,
        ),
        row(
            "r29_for_in_consume_module_state.vyrn",
            "module state may not be taken: `for .. in consume`",
            "RFC-0013",
            "module state `names` may not be consumed by a `for` loop — nothing may take \
             ownership of module state (it lives for the whole module and is never dropped)",
            Kernel::Other("module state `names` may not be consumed by a `drop`"),
        ),
        row(
            "r30_stream_never_disposed.vyrn",
            "a must-use obligation is discharged on every path",
            "RFC-0075",
            "`s` is a `Stream` and is never disposed",
            Kernel::No,
        ),
        row(
            "r31_stream_disposed_twice.vyrn",
            "a must-use obligation is discharged exactly once",
            "RFC-0075",
            "`s` is a `Stream` and is disposed more than once",
            Kernel::Other("was already consumed by `close(..)` on line 5"),
        ),
        row(
            "r32_region_store_escapes.vyrn",
            "a value the region allocated may not be stored where it outlives the region",
            "RFC-0004 §4",
            "cannot store a heap value into `kept`, which outlives the enclosing `region` (it \
             would dangle when the region frees). Move `kept` inside the region, or compute a \
             non-heap result to carry out.",
            Kernel::Elsewhere,
        ),
        row(
            "r33_region_consume_escapes.vyrn",
            "a `consume` parameter may not take a value the region frees",
            "RFC-0004 §4",
            "cannot hand a heap value to argument 1 of `take`, which is `consume`, inside a \
             `region`. The region frees the value at its closing brace, so the callee cannot \
             own it. Move the call out of the region, or pass a value that holds no heap.",
            Kernel::Elsewhere,
        ),
        row(
            "r34_read_parameter_into_a_builtin_consume_slot.vyrn",
            "rule 2: a `read` parameter into a builtin's `consume` argument",
            "RFC-0089",
            "`s` may not be stored into `push(..)` — it is a `read` parameter",
            Kernel::Other("via `push(..)` — it is a `read` parameter"),
        ),
    ]
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/refusals")
}

/// Where the counterexamples below live.
fn unlicensed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/unlicensed")
}

/// The command's standard error, with the `fix:` and `note:` menu lines and
/// the file prefix taken off, so what is left is the sentence.
fn refusal(file: &str, kernel_mode: bool) -> (bool, String) {
    refusal_in(dir(), file, kernel_mode)
}

fn refusal_in(dir: PathBuf, file: &str, kernel_mode: bool) -> (bool, String) {
    let path = dir.join(file);
    let mut cmd = vyrn();
    cmd.arg("check").arg(&path);
    if kernel_mode {
        cmd.env("VYRN_NO_MOVECHECK", "1");
    }
    let out = cmd.output().expect("vyrn check");
    let err = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    let text: Vec<&str> = err
        .lines()
        .filter(|l| !l.trim_start().starts_with("fix:") && !l.trim_start().starts_with("note:"))
        .collect();
    (out.status.success(), text.join("\n"))
}

/// The `file:line:col: ` prefix of the first diagnostic, and the rest.
fn split_head(text: &str) -> (String, String) {
    // A Windows path carries a drive colon, so the prefix is found from the
    // end of the file name: `<path>:<line>:<col>: <message>`.
    match text.find(": ") {
        Some(_) => {
            let mut best = None;
            for (i, _) in text.match_indices(": ") {
                let head = &text[..i];
                if head.rsplit(':').take(2).all(|p| p.parse::<u32>().is_ok()) {
                    best = Some(i);
                    break;
                }
            }
            match best {
                Some(i) => (text[..i].to_string(), text[i + 2..].to_string()),
                None => (String::new(), text.to_string()),
            }
        }
        None => (String::new(), text.to_string()),
    }
}

/// Every program in the census is refused by the checker with the wording the
/// table records, and the kernel's column is what the table says.
#[test]
fn the_census_is_what_the_two_passes_say() {
    let mut bad: Vec<String> = Vec::new();
    for r in census() {
        let (ok, text) = refusal(r.file, false);
        if ok {
            bad.push(format!("{}: the checker accepted it", r.file));
            continue;
        }
        let (head, msg) = split_head(&text);
        // One program may draw a second diagnostic once the first is out of
        // the way; the row is about the first.
        let first = msg
            .split('\n')
            .take(r.says.lines().count())
            .collect::<Vec<_>>()
            .join("\n");
        if first != r.says {
            bad.push(format!(
                "{} ({} {}): the checker said\n    {}\n  and the table says\n    {}",
                r.file,
                r.rfc,
                r.rule,
                first.replace('\n', "\n    "),
                r.says.replace('\n', "\n    ")
            ));
            continue;
        }
        let (kok, ktext) = refusal(r.file, true);
        match &r.kernel {
            Kernel::No => {
                if !kok {
                    bad.push(format!(
                        "{}: the table says the kernel gives nothing, and it said\n    {}",
                        r.file,
                        ktext.replace('\n', "\n    ")
                    ));
                }
            }
            Kernel::Elsewhere => {
                if kok || ktext != text {
                    bad.push(format!(
                        "{}: the table says another pass gives this, so the two runs must \
                         agree; they said\n    {}\n  and\n    {}",
                        r.file,
                        text.replace('\n', "\n    "),
                        ktext.replace('\n', "\n    ")
                    ));
                }
            }
            Kernel::Same => {
                let (khead, kmsg) = split_head(&ktext);
                if kok || khead != head || kmsg != r.says {
                    bad.push(format!(
                        "{}: the table says the kernel gives the checker's sentence at \
                         {head}; it said\n    {}",
                        r.file,
                        ktext.replace('\n', "\n    ")
                    ));
                }
            }
            Kernel::Other(needle) => {
                if kok || !ktext.contains(needle) {
                    bad.push(format!(
                        "{}: the table says the kernel refuses it with `{needle}`; it said\n    \
                         {}",
                        r.file,
                        ktext.replace('\n', "\n    ")
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "the census has moved:\n  {}",
        bad.join("\n  ")
    );
}

/// Every program under `tests/refusals/` is a row, and every row is a file.
#[test]
fn the_census_covers_the_directory() {
    let mut on_disk: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|f| f.ends_with(".vyrn"))
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = census().iter().map(|r| r.file.to_string()).collect();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "a program with no row, or a row with no program"
    );
}

/// The table for RFC-0125 §3 M3, printed from the rows above so the prose and
/// the assertion cannot drift apart by a transcription:
/// `cargo test -p vyrn-cli --test refusals -- --ignored --nocapture`.
#[test]
#[ignore]
fn the_census_as_a_table() {
    println!("| # | rule | RFC | the checker's sentence | the kernel |");
    println!("|---|---|---|---|---|");
    for r in census() {
        let n = &r.file[1..3];
        let says = r.says.replace('\n', " / ").replace('|', r"|");
        let k = match r.kernel {
            Kernel::Same => "the same".to_string(),
            Kernel::Other(_) => "its own words".to_string(),
            Kernel::No => "nothing".to_string(),
            Kernel::Elsewhere => "not the move check's".to_string(),
        };
        println!("| {n} | {} | {} | {says} | {k} |", r.rule, r.rfc);
    }
}

// ---------------------------------------------------------------------------
// The licence, measured per PROGRAM (RFC-0125 §3 M3, the deletion slice).
//
// The census above runs ONE program per rule, so its kernel column is a
// measurement of that program and not of the rule. This table is the
// correction: for seven of the rows the column says the kernel gives, here is
// a second program the SAME rule refuses and the kernel accepts. A deletion
// licensed by the column alone would ship each of these as a program the
// compiler compiles.
//
// The two reasons were separate, and the `why` column says which one the row
// was found by.
//
//   - **heap**. The kernel placed releases, so a value that owned no heap had
//     no release and the kernel had no opinion about it. RFC-0089 rule 1 and
//     RFC-0013 are rules about OWNERSHIP, which the checker applies to every
//     value. `while i < 3 { take(x) }` over a record of `Int64`s was the
//     smallest of them.
//   - **spelling**. The value owned heap and the kernel still said nothing,
//     because it did not read that spelling as a borrow: a prefix `consume`
//     of a `read` parameter's field, a match arm's payload binder, a
//     `for .. in consume` whose iterable names no place.
//
// **The pin has flipped** (RFC-0125 §3 M3, the two-questions slice): all seven
// are refused by both passes now, so the test asserts the containment instead
// of the gap. The two texts are compared WHOLE — the head, the sentence and
// its second line — because rule 4 of the deletion is that a refusal must be
// the same refusal after it. A row that starts to differ fails here.
// ---------------------------------------------------------------------------

/// One program a census row's rule refuses, and what the kernel says about it.
struct Uncovered {
    file: &'static str,
    row: &'static str,
    says: &'static str,
    why: &'static str,
    /// `None`: the kernel refuses it in the checker's words, whole. `Some(s)`:
    /// the kernel refuses it and says `s` instead, so the row's rule may NOT
    /// leave `movecheck.rs` — the sentence a reader gets would move (RFC-0125
    /// §3 M3, the containment slice).
    kernel: Option<&'static str>,
}

const fn covered(
    file: &'static str,
    row: &'static str,
    says: &'static str,
    why: &'static str,
) -> Uncovered {
    Uncovered {
        file,
        row,
        says,
        why,
        kernel: None,
    }
}

/// The same, for a program whose two passes do not agree on the wording.
const fn worded(
    file: &'static str,
    row: &'static str,
    says: &'static str,
    why: &'static str,
    kernel: &'static str,
) -> Uncovered {
    Uncovered {
        file,
        row,
        says,
        why,
        kernel: Some(kernel),
    }
}

/// The counterexamples, one per rule the census column got wrong.
fn counterexamples() -> Vec<Uncovered> {
    vec![
        covered(
            "u04_whole_after_a_hole_heapless.vyrn",
            "04",
            "`p.name` was taken out of `p` here",
            "heap",
        ),
        covered(
            "u06_use_after_consume_heapless.vyrn",
            "06, 07",
            "`x.id` is used here but was already consumed by `take(..)` on line 8",
            "heap",
        ),
        covered(
            "u09_for_in_consume_with_nothing_to_take.vyrn",
            "09",
            "`consume` here has nothing to take — the loop already owns a container that is \
             not a binding",
            "spelling",
        ),
        covered(
            "u11_prefix_consume_of_a_read_parameters_field.vyrn",
            "11",
            "`d` may not be consumed — it is a `read` parameter",
            "spelling",
        ),
        covered(
            "u12_module_state_to_a_consume_parameter_heapless.vyrn",
            "12",
            "module state `g` may not be passed to a `consume` parameter via `take(..)` — \
             nothing may take ownership of module state (it lives for the whole module and is \
             never dropped)",
            "heap",
        ),
        covered(
            "u13_an_arm_binder_to_a_consume_parameter.vyrn",
            "13",
            "`v` may not be passed to a `consume` parameter via `take(..)` — it is a second \
             name for the `read` parameter `o`",
            "spelling",
        ),
        covered(
            "u12b_module_state_to_a_spawned_consume_parameter.vyrn",
            "12",
            "module state `names` may not be passed to a `consume` parameter via `spawn \
             take(..)` — nothing may take ownership of module state (it lives for the whole \
             module and is never dropped)",
            "spelling",
        ),
        covered(
            "u25_consume_inside_a_loop_heapless.vyrn",
            "25",
            "`x` is consumed by `take(..)` inside a loop, so it would be used again on the \
             next iteration",
            "heap",
        ),
        worded(
            "u25b_partial_take_across_iterations.vyrn",
            "25",
            "`er.node` is consumed by `consume` inside a loop, so it would be used again on \
             the next iteration",
            "spelling",
            "`er` (line 9) has a `consume` hole at a loop's back edge it did not have at entry",
        ),
    ]
}

/// Each counterexample is refused by the checker AND by the kernel. A row with
/// no `kernel` wording is refused in the same words at the same line, which is
/// the licence the census column could not give on its own; a row that names
/// one is refused in DIFFERENT words, which is why its census row's rule stays
/// (RFC-0125 §3 M3, the containment slice).
#[test]
fn the_licence_is_per_program_and_these_are_the_counterexamples() {
    let mut bad: Vec<String> = Vec::new();
    for u in counterexamples() {
        let (ok, text) = refusal_in(unlicensed_dir(), u.file, false);
        if ok {
            bad.push(format!("{}: the checker accepted it", u.file));
            continue;
        }
        let (_, msg) = split_head(&text);
        let first = msg.lines().next().unwrap_or_default();
        if first != u.says {
            bad.push(format!(
                "{} (row {}): the checker said `{first}` and the table says `{}`",
                u.file, u.row, u.says
            ));
            continue;
        }
        let (kok, ktext) = refusal_in(unlicensed_dir(), u.file, true);
        if kok {
            bad.push(format!(
                "{} (row {}, found by {}): the kernel is supposed to refuse it and it said                  nothing",
                u.file, u.row, u.why
            ));
            continue;
        }
        let (_, kmsg) = split_head(&ktext);
        match u.kernel {
            // The licence: the same refusal, whole.
            None => {
                if ktext != text {
                    bad.push(format!(
                        "{} (row {}): the checker said `{text}` and the kernel said `{ktext}`",
                        u.file, u.row
                    ));
                }
            }
            // The rule stays: the two passes refuse and word it differently.
            Some(k) => {
                let first = kmsg.lines().next().unwrap_or_default();
                if first != k {
                    bad.push(format!(
                        "{} (row {}): the kernel is supposed to say `{k}` and it said `{first}`",
                        u.file, u.row
                    ));
                }
            }
        }
    }
    assert!(bad.is_empty(), "the licence has moved: {}", bad.join("; "));
}

/// The shapes rule 1's own unit tests pinned, still refused after the rule
/// left `movecheck.rs` (RFC-0125 §3 M3, row 06).
///
/// They lived in `movecheck.rs`'s test module and asked `vyrn_frontend::check`
/// alone, which no longer states this rule. Each names something OTHER than
/// the rule — a region's shadowing `let`, a lambda block's, a `break` path, a
/// `spawn`, a method's `consume` parameter, a `test` body, a `drop` — and each
/// used the refusal to see it, so the deletion would have taken seven readings
/// of the walk with it. They are asked of the whole compiler here instead.
#[test]
fn the_shapes_rule_ones_unit_tests_pinned_are_still_refused() {
    const T: &str = "type T = { id: Int64 } \
                     fn take(t: consume T) -> Int64 { return t.id } ";
    let cases: &[(&str, String)] = &[
        (
            "a second call",
            format!(
                "{T} fn main() -> Int64 {{ let x = T {{ id: 1 }} let a = take(x) return take(x) }}"
            ),
        ),
        (
            "a test body",
            format!(
                "{T} test \"consumes twice\" {{ let x = T {{ id: 1 }} \
                 let a = take(x) let b = take(x) assert(a == b) }} \
                 fn main() -> Int64 {{ return 0 }}"
            ),
        ),
        (
            "a lambda blocks shadowing let",
            format!(
                "{T} fn main() -> Int64 {{ let s = T {{ id: 1 }} let n = take(s) \
                 let g: fn(Int64) -> Int64 = x -> {{ let s = 0 return x + s }} \
                 return g(n) + take(s) }}"
            ),
        ),
        (
            "a regions shadowing let",
            "fn sink(t: consume String) -> Int64 { drop t return 0 } \
             fn main() -> Int64 { let x = \"a\" + \"b\" let n = sink(x) \
             region { let x = 9 } return x.byteLength + n }"
                .to_string(),
        ),
        (
            "a break path",
            format!(
                "{T} fn main() -> Int64 {{ let x = T {{ id: 1 }} \
                 for i in [0, 1] {{ let a = take(x) break }} return take(x) }}"
            ),
        ),
        (
            "a spawn",
            format!(
                "{T} fn main() -> Int64 {{ let x = T {{ id: 1 }} \
                 let t = spawn take(x) let z = take(x) return t.join() + z }}"
            ),
        ),
        (
            "a methods consume parameter",
            "type T = { n: Int64 } \
             protocol Taking { fn take(modify self, s: consume String) -> Unit } \
             impl Taking for T { fn take(modify self, s: consume String) -> Unit \
             { self.n = self.n + s.byteLength } } \
             fn main() -> Int64 { let mut t = T { n: 1 } let s = \"a\" \
             t.take(s) return s.byteLength }"
                .to_string(),
        ),
        (
            "a drop",
            "fn main() -> Int64 { let mut xs: SmallArray<Int64, 4> = [] xs.push(1) \
             drop xs return xs.length }"
                .to_string(),
        ),
    ];
    let dir = common::scratch("rule-one-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains("already consumed") {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "rule 1 no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// A nullary constructor is a value with no owner, not a name (RFC-0126 §8.8).
///
/// `take(None)` twice hands the callee two values. The checker keyed rule 1 on
/// the bare name a nullary variant parses as, so it reported the second as a
/// use of the first — of `None`, of a user enum's `Nothing`, at a call, across
/// a branch, around a loop. Every engine runs these programs; the kernel never
/// had the defect, because a constructor lowers to a value and not to a name.
///
/// Asked of both passes, because the checker is the one that was wrong and the
/// answer has to stay right when the next rule leaves it.
#[test]
fn a_nullary_constructor_is_a_value_and_not_a_name() {
    const M: &str = "type Maybe = | Nothing | Just(String) \
                     fn takeM(m: consume Maybe) -> Int64 { return 0 } ";
    const O: &str = "fn takeO(o: consume Option<String>) -> Int64 { return 0 } ";
    let cases: &[(&str, String)] = &[
        (
            "a builtin variant twice",
            format!("{O} fn main() -> Int64 {{ let a = takeO(None) let b = takeO(None) return a + b }}"),
        ),
        (
            "a declared variant twice",
            format!("{M} fn main() -> Int64 {{ let a = takeM(Nothing) let b = takeM(Nothing) return a + b }}"),
        ),
        (
            "one on each branch and one after",
            format!(
                "{O} fn main() -> Int64 {{ let mut t = 0 \
                 if t > 0 {{ t = t + takeO(None) }} else {{ t = t + takeO(None) }} \
                 return t + takeO(None) }}"
            ),
        ),
        (
            "one every turn of a loop and one after",
            format!(
                "{M} fn main() -> Int64 {{ let mut t = 0 let mut i = 0 \
                 while i < 2 {{ t = t + takeM(Nothing) i = i + 1 }} \
                 return t + takeM(Nothing) }}"
            ),
        ),
    ];
    let dir = common::scratch("nullary-constructors");
    let mut bad: Vec<String> = Vec::new();
    for (what, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        for kernel_only in [false, true] {
            let (ok, text) = refusal_in(dir.to_path_buf(), &name, kernel_only);
            if !ok {
                let by = if kernel_only {
                    "the kernel"
                } else {
                    "the compiler"
                };
                bad.push(format!(
                    "{what}, {by}:\n    {}",
                    text.replace('\n', "\n    ")
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "a nullary constructor is read as a name:\n  {}",
        bad.join("\n  ")
    );
}

/// Every program under `tests/unlicensed/` is a counterexample, and every
/// counterexample is a program.
#[test]
fn the_counterexamples_cover_their_directory() {
    let mut on_disk: Vec<String> = std::fs::read_dir(unlicensed_dir())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|f| f.ends_with(".vyrn"))
        .collect();
    on_disk.sort();
    let mut listed: Vec<String> = counterexamples()
        .iter()
        .map(|u| u.file.to_string())
        .collect();
    listed.sort();
    assert_eq!(
        on_disk, listed,
        "a program with no row, or a row with no program"
    );
}

/// The table for RFC-0125 §3 M3, printed from the rows above:
/// `cargo test -p vyrn-cli --test refusals -- --ignored --nocapture
/// the_counterexamples_as_a_table`.
#[test]
#[ignore]
fn the_counterexamples_as_a_table() {
    println!("| census row | the program | the checker's sentence | found by | the kernel |");
    println!("|---|---|---|---|---|");
    for u in counterexamples() {
        let k = match u.kernel {
            None => "the same, whole".to_string(),
            Some(k) => format!("its own words: {k}"),
        };
        println!(
            "| {} | `{}` | {} | {} | {k} |",
            u.row, u.file, u.says, u.why
        );
    }
}

// ---------------------------------------------------------------------------
// The structural census of `movecheck.rs` (RFC-0125 §3 M3, the checker's
// deletion path).
//
// The census above is rule by rule. This one is line by line: every section of
// `compiler/vyrn-frontend/src/movecheck.rs`, what kind of code it is, and how
// many lines it holds. The point is to say what the deletion is worth and what
// stands in its way, in a number rather than in an impression.
//
// A section is one item — a `fn`, a `struct`, an `enum`, an `impl`, a `mod` —
// together with every item after it up to the next section's anchor. The span
// runs from the anchor's own doc comment to the line before the next anchor's,
// so every line of the file belongs to exactly one section and the counts add
// up to the file. The test computes the spans; the table below records only the
// anchor and the kind, so an edit to the file moves the numbers and the
// classification stays where a reader put it.
// ---------------------------------------------------------------------------

/// What a section of `movecheck.rs` is.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Kind {
    /// A refusal rule the kernel gives today, in the same sentence: the census
    /// above says `Same` for it. The checker's copy is what the deletion takes.
    Kernel,
    /// A refusal rule only the checker gives. The census above says `nothing`
    /// or `its own words`, so nothing may take this yet.
    Checker,
    /// Placement rows for the engines: what `own.rs` reads and the plan
    /// carries. It is not a rule, and the kernel does not replace it — the
    /// own-side deletion track does.
    Rows,
    /// A `fix:` menu. Surface knowledge the kernel has no source for.
    Menu,
    /// Shared machinery: the walk itself, the scope stacks, the path algebra,
    /// the entry points, the recorded measurements.
    Shared,
    /// The file's own unit tests.
    Tests,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Kernel => "a rule the kernel now gives",
            Kind::Checker => "a rule only the checker gives",
            Kind::Rows => "placement rows for the engines",
            Kind::Menu => "a fix menu",
            Kind::Shared => "shared machinery",
            Kind::Tests => "tests",
        }
    }
}

/// One section: the exact source line that starts it, its kind, and what it is.
struct Section {
    at: &'static str,
    kind: Kind,
    what: &'static str,
}

const fn sec(at: &'static str, kind: Kind, what: &'static str) -> Section {
    Section { at, kind, what }
}

/// The sections, in file order. The first one starts at line 1.
fn sections() -> Vec<Section> {
    use Kind::*;
    vec![
        sec(
            "pub struct OwningSite {",
            Shared,
            "the module's own statement of the rules, and the two recorded \
             measurements (RFC-0089 rule 1's sites, RFC-0092's projections)",
        ),
        sec(
            "pub enum Gone {",
            Rows,
            "why a binding does not hold its value at its block's end, and the \
             row `own.rs` reads it from",
        ),
        sec(
            "pub enum ArgVerdict {",
            Rows,
            "what a callee does with the temporary at a call-argument position",
        ),
        sec(
            "pub struct ExitEv {",
            Rows,
            "the event records: exits, reads, consuming matches, arm payloads, \
             stores, Rule N edges, place stores",
        ),
        sec(
            "pub fn facts(program: &Program) -> Facts {",
            Rows,
            "the two facts out of one walk, and the lender and retention \
             post-passes over them",
        ),
        sec(
            "enum Want {",
            Shared,
            "what a run is for, and one run's outputs",
        ),
        sec(
            "fn arg_verdict(",
            Rows,
            "the verdict for one argument temporary, read at a position instead \
             of at a binding",
        ),
        sec(
            "fn let_id(s: &Stmt) -> usize {",
            Rows,
            "the key of a `let`, the lending builtins, and the projection names",
        ),
        sec(
            "pub fn check_accum(program: &Program) -> Vec<Diagnostic> {",
            Shared,
            "the entry points a caller uses",
        ),
        sec(
            "fn run(program: &Program, want: Want) -> Run {",
            Shared,
            "the one walk: the capability tables, every body, the drains and the \
             stamps",
        ),
        sec(
            "pub fn check(program: &Program) -> Result<(), String> {",
            Shared,
            "the historical string shim",
        ),
        sec(
            "struct MoveCheck<'a> {",
            Shared,
            "the pass's state: the scope stacks, the sinks, the recorded rows",
        ),
        sec(
            "enum Borrow {",
            Kernel,
            "what a borrow is, in words — `core::BorrowKind::what` is this \
             sentence",
        ),
        sec(
            "    fn fixes(&self, root: &str, path: &str) -> Vec<String> {",
            Menu,
            "the named ways out of a borrow error",
        ),
        sec(
            "enum TakeForm {",
            Kernel,
            "which form wrote the `consume`, and how a refusal names it",
        ),
        sec(
            "fn root_of(path: &str) -> &str {",
            Shared,
            "the path algebra and the consumed table: overlap, reach, revival",
        ),
        sec(
            "impl MoveCheck<'_> {",
            Shared,
            "one body, with its parameters and its return type",
        ),
        sec(
            "    fn enter(&self) {",
            Shared,
            "the three scope stacks, read as one environment",
        ),
        sec(
            "    fn place_key(&self, e: &Expr) -> usize {",
            Rows,
            "the key a row is written under",
        ),
        sec(
            "    fn note_temporary(&self, s: &Stmt, value: &Expr) -> usize {",
            Rows,
            "the recording: temporaries, store events, branches, reads, exits, \
             takes, holes, place stores, hand-overs at a `return`",
        ),
        sec(
            "    fn is_bound_name(&self, e: &Expr) -> bool {",
            Rows,
            "whether a `let` names storage somebody else owns, for reclamation",
        ),
        sec(
            "    fn names_a_place(&self, value: &Expr) -> Option<&'static str> {",
            Kernel,
            "whether a value reads a place that owns it — the kernel's alias \
             table",
        ),
        sec(
            "    fn fixes_here(&self, b: &Borrow, root: &str, path: &str) -> Vec<String> {",
            Menu,
            "the ways out that exist in THIS function",
        ),
        sec(
            "    fn is_module_state(&self, name: &str) -> bool {",
            Shared,
            "module state, the borrow table, and the type reading",
        ),
        sec(
            "    fn sinks(&self, name: &str, i: usize) -> bool {",
            Kernel,
            "a rebuilding builtin takes its receiver, and the write-back \
             statement excepted (row 26)",
        ),
        sec(
            "    fn store(",
            Kernel,
            "rule 1's move and rule 2's refusal at a store (rows 01, 02, 03, 27, \
             34)",
        ),
        sec(
            "    fn borrow_from(&self, value: &Expr) -> Option<Borrow> {",
            Kernel,
            "the borrow status a `let` of a value gives its binding",
        ),
        sec(
            "    fn payload_binding(",
            Shared,
            "what a pattern's binders name, and whether an iterable is a place",
        ),
        sec(
            "    fn check_use(&self, path: &str, line: usize, consumed: &Consumed) \
             -> Result<(), Diagnostic> {",
            Kernel,
            "rule 1 asked of a path: is the storage still all there (rows 04, 06, \
             07)",
        ),
        sec(
            "    fn check_take(",
            Kernel,
            "a take's refusals: an element, and nothing to take — \
             `core::take_prefix` states both (rows 08, 09)",
        ),
        sec(
            "    fn check_handover(&self, arg: &Expr, callee: &str, line: usize) \
             -> Result<(), Diagnostic> {",
            Kernel,
            "rule 2 at the third exit: a borrow may not be consumed (rows 11, 12, \
             13, 14)",
        ),
        sec(
            "    fn refuse_projected_arg(",
            Kernel,
            "the refusal a projected argument to a `consume` parameter gets",
        ),
        sec(
            "    fn arm_binder(&self, name: &str) -> bool {",
            Shared,
            "an arm's binders, and whether a callee keeps a `fn` value",
        ),
        sec(
            "    fn check_return(&self, e: &Expr, line: usize) -> Result<(), Diagnostic> {",
            Kernel,
            "rule 3: a return is owned (rows 15, 16, 18, 19, 28)",
        ),
        sec(
            "    fn refuse_return(&self, b: &Borrow, root: &str, path: &str, line: usize) \
             -> Diagnostic {",
            Kernel,
            "the one exit every returned borrow leaves by, the exported \
             function's own sentence with it (row 17)",
        ),
        sec(
            "    fn note_handover(&self, arg: &Expr, callee: &str, i: usize, line: usize) {",
            Rows,
            "the retention and hand-over records the call graph is closed over",
        ),
        sec(
            "    fn note_arg_temp(&self, arg: &Expr, callee: &str, ix: usize, line: usize) {",
            Rows,
            "the argument-temporary row: its producer, its type and its release \
             kind",
        ),
        sec(
            "    fn ctor_valued(&self, e: &Expr) -> bool {",
            Rows,
            "what an expression builds: a variant, a String, a concatenation",
        ),
        sec(
            "    fn note_arm_aliases(&self, e: &Expr, line: usize, binders: &[String]) {",
            Rows,
            "an arm that yields a place, and what naming one costs",
        ),
        sec(
            "    fn value_cannot_alias(&self, e: &Expr, root: &str) -> bool {",
            Rows,
            "Rule N's edge guard, the mention guard, and what a call may forward",
        ),
        sec(
            "    fn carries_param_storage(&self, e: &Expr) -> bool {",
            Rows,
            "the escape screen: storage flow rather than mention",
        ),
        sec(
            "    fn lends(&self) {",
            Rows,
            "the lending record, and the lend a wrapper hides",
        ),
        sec(
            "    fn returned_borrow(&self, e: &Expr) -> Option<(Borrow, String, String)> {",
            Kernel,
            "the first borrow a returned expression yields",
        ),
        sec(
            "    fn note_returned_projection(&self, e: &Expr, line: usize) {",
            Shared,
            "RFC-0092's instrument",
        ),
        sec(
            "    fn lends_through_a_wrapper(&self, e: &Expr) -> Option<(Borrow, String, String)> {",
            Rows,
            "the same question through a constructor, to record a lend and never \
             to refuse one",
        ),
        sec(
            "    fn site(&self, kind: &'static str, line: usize, e: &Expr, declared: Option<&Type>) {",
            Shared,
            "RFC-0089 rule 1's instrument",
        ),
        sec(
            "    fn block(&self, b: &Block, consumed: &mut Consumed, scope: &mut Vec<HashSet<String>>) -> bool {",
            Shared,
            "a block, and whether it diverges",
        ),
        sec(
            "    fn stmt(",
            Shared,
            "the walk over statements: it calls the refusal helpers and writes \
             the plan's rows in the same arm",
        ),
        sec(
            "    fn capture_site(&self, name: &str, line: usize) {",
            Rows,
            "a lambda's captures, recorded for the enclosing block",
        ),
        sec(
            "    fn check_exclusive(&self, callee: &str, args: &[Expr], line: usize) \
             -> Result<(), Diagnostic> {",
            Checker,
            "a `modify` borrow is exclusive (row 23)",
        ),
        sec(
            "    fn check_capture(&self, name: &str, line: usize) -> Result<(), Diagnostic> {",
            Checker,
            "a closure that outlives the call may not capture a borrow (row 24)",
        ),
        sec(
            "    fn check_loop_reuse(",
            Kernel,
            "rule 1 across a back edge (row 25)",
        ),
        sec(
            "    fn expr(",
            Shared,
            "the walk over expressions: the same traversal does both jobs",
        ),
        sec(
            "pub fn mentions_place(e: &Expr, base: &str) -> bool {",
            Shared,
            "whether a stored value mentions the place it is stored into",
        ),
        sec(
            "mod linear {",
            Checker,
            "the must-use obligation: acquired once, disposed exactly once (rows \
             30, 31)",
        ),
        sec(
            "fn store_path(e: &Expr) -> Option<String> {",
            Shared,
            "the place an expression names, as the store arms spell it",
        ),
        sec(
            "fn sinks(decl: &Declared, name: &str, i: usize) -> bool {",
            Kernel,
            "whether a builtin's parameter takes its argument for good",
        ),
        sec(
            "fn reads(e: &Expr) -> Vec<String> {",
            Shared,
            "the names an expression reads, and the calls in it",
        ),
        sec(
            "pub fn element_path(e: &Expr) -> Option<(String, String)> {",
            Shared,
            "the place spellings every rule above compares",
        ),
        sec(
            "fn menu(line: usize, message: String, fixes: Vec<String>) -> Diagnostic {",
            Menu,
            "one diagnostic with its menu of fixes",
        ),
        sec(
            "fn declared_in(block: &crate::ast::Block, out: &mut std::collections::HashSet<String>) {",
            Shared,
            "the names a block declares, and a pattern's binders",
        ),
        sec("mod tests {", Tests, "the pass's own unit tests"),
    ]
}

/// `movecheck.rs`, as lines.
fn movecheck() -> Vec<String> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vyrn-frontend/src/movecheck.rs");
    std::fs::read_to_string(&p)
        .expect("read movecheck.rs")
        .replace("\r\n", "\n")
        .lines()
        .map(str::to_string)
        .collect()
}

/// Where a section's doc comment starts: the run of comment and attribute lines
/// straight above the anchor.
fn doc_start(lines: &[String], anchor: usize) -> usize {
    let mut i = anchor;
    while i > 0 {
        let t = lines[i - 1].trim_start();
        if t.starts_with("//") || t.starts_with("#[") {
            i -= 1;
        } else {
            break;
        }
    }
    i
}

/// The sections, with the span each holds: `(index, first line, last line)`,
/// one-based and inclusive. Every line of the file is in exactly one span.
fn spans(lines: &[String]) -> Vec<(usize, usize, usize)> {
    let secs = sections();
    let mut anchors = Vec::new();
    for s in &secs {
        let want: String = s.at.split_whitespace().collect::<Vec<_>>().join(" ");
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.split_whitespace().collect::<Vec<_>>().join(" ") == want)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "the anchor `{}` names {} lines of movecheck.rs; a section's anchor must name one",
            s.at,
            hits.len()
        );
        anchors.push(doc_start(lines, hits[0]));
    }
    let mut out = Vec::new();
    for i in 0..secs.len() {
        let first = if i == 0 { 0 } else { anchors[i] };
        let last = if i + 1 == secs.len() {
            lines.len()
        } else {
            anchors[i + 1]
        };
        assert!(
            first < last,
            "section `{}` of movecheck.rs is empty or out of order",
            secs[i].at
        );
        out.push((i, first + 1, last));
    }
    out
}

/// The sections tile `movecheck.rs`: every line is in one, in file order.
#[test]
fn the_structural_census_covers_the_file() {
    let lines = movecheck();
    let spans = spans(&lines);
    let mut next = 1;
    for (_, a, b) in &spans {
        assert_eq!(*a, next, "a gap or an overlap at line {a} of movecheck.rs");
        next = b + 1;
    }
    assert_eq!(
        next - 1,
        lines.len(),
        "the last section does not reach the end of movecheck.rs"
    );
}

/// The line count per kind, as RFC-0125 §3 M3 records it. The prose quotes
/// these numbers, so they are asserted rather than described: a change to
/// `movecheck.rs` moves one, and the RFC's table moves with it.
#[test]
fn the_structural_census_is_what_the_rfc_records() {
    let lines = movecheck();
    let secs = sections();
    let mut by_kind = std::collections::BTreeMap::new();
    for (i, a, b) in spans(&lines) {
        *by_kind.entry(secs[i].kind as usize).or_insert(0usize) += b - a + 1;
    }
    let got: Vec<(&'static str, usize)> = [
        Kind::Kernel,
        Kind::Checker,
        Kind::Rows,
        Kind::Menu,
        Kind::Shared,
        Kind::Tests,
    ]
    .iter()
    .map(|k| (k.label(), by_kind.get(&(*k as usize)).copied().unwrap_or(0)))
    .collect();
    let want = vec![
        ("a rule the kernel now gives", 916),
        ("a rule only the checker gives", 723),
        ("placement rows for the engines", 2362),
        ("a fix menu", 73),
        ("shared machinery", 3647),
        ("tests", 2069),
    ];
    assert_eq!(got, want, "the structural census has moved");
    assert_eq!(
        got.iter().map(|(_, n)| n).sum::<usize>(),
        lines.len(),
        "the kinds do not add up to the file"
    );
}

/// The table for RFC-0125 §3 M3, printed from the sections above:
/// `cargo test -p vyrn-cli --test refusals -- --ignored --nocapture
/// the_structural_census_as_a_table`.
#[test]
#[ignore]
fn the_structural_census_as_a_table() {
    let lines = movecheck();
    let secs = sections();
    println!("| section | lines | kind | what it is |");
    println!("|---|---|---|---|");
    for (i, a, b) in spans(&lines) {
        let name = secs[i]
            .at
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches(" {")
            .trim_end_matches('(')
            .to_string();
        println!(
            "| `{}` | {} | {} | {} |",
            name,
            b - a + 1,
            secs[i].kind.label(),
            secs[i].what
        );
    }
}
