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
//! 28, 06, 20, 21, 07, 19, 25, 13, 14, 26, 10, 11, 29, 01, 02, 03, 27 and 34,
//! RFC-0125 §3 M3 — is refused by the kernel in both runs, and the two must
//! still agree. The row is what stops the sentence moving after the deletion,
//! so it stays in the census.
//!
//! A census row is one program with one error in it, which is what makes it a
//! census and what it cannot see. Accumulation — a file with two kinds of error
//! — is pinned separately, below the rows: see
//! [`a_file_with_a_must_use_error_still_gets_its_ownership_refusals`] and
//! [`the_kernel_does_not_say_again_what_the_checker_said_about_the_same_binding`].

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
            Kernel::Same,
        ),
        row(
            "r02_store_read_parameter_field.vyrn",
            "rule 2: a field of a `read` parameter may not be stored",
            "RFC-0089",
            "`h.meta[0]` may not be stored into `push(..)` — it is a `read` parameter",
            Kernel::Same,
        ),
        row(
            "r03_store_projection.vyrn",
            "rule 2: a projection is a borrow of its root, whatever the root is",
            "RFC-0092",
            "`d.title` may not be stored into `push(..)` — it is read out of a place that owns it",
            Kernel::Same,
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
            "module state `names` may not be passed to a `consume` parameter via `take(..)` — \
             nothing may take ownership of module state (it lives for the whole module and is \
             never dropped)",
            Kernel::Same,
        ),
        row(
            "r11_consume_a_read_parameter.vyrn",
            "rule 2: a prefix `consume` of a `read` parameter",
            "RFC-0089",
            "`ys` may not be passed to a `consume` parameter via `take(..)` — it is a `read` \
             parameter",
            Kernel::Same,
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
            Kernel::Same,
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
            Kernel::Same,
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
            Kernel::Same,
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
            // The kernel gives it too since the walk's deletion (RFC-0125 §3
            // M3): it used to read a record literal of literals as static
            // data and so never judged the `drop`.
            Kernel::Other("is released whole although a `consume` took `.name` out of it"),
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
            Kernel::Same,
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
            "module state `names` may not be consumed by the `for .. in consume` loop — nothing \
             may take ownership of module state (it lives for the whole module and is never \
             dropped)",
            Kernel::Same,
        ),
        row(
            "r30_stream_never_disposed.vyrn",
            "a must-use obligation is discharged on every path",
            "RFC-0075",
            "`s` is a `Stream` and is never disposed",
            Kernel::Elsewhere,
        ),
        row(
            "r31_stream_disposed_twice.vyrn",
            "a must-use obligation is discharged exactly once",
            "RFC-0075",
            "`s` is a `Stream` and is disposed more than once",
            Kernel::Elsewhere,
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
            Kernel::Same,
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

/// The command's WHOLE standard error, menu and all: what a reader sees.
fn whole_refusal_in(dir: PathBuf, file: &str, kernel_mode: bool) -> (bool, String) {
    let mut cmd = vyrn();
    cmd.arg("check").arg(dir.join(file));
    if kernel_mode {
        cmd.env("VYRN_NO_MOVECHECK", "1");
    }
    let out = cmd.output().expect("vyrn check");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
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
        covered(
            "u25b_partial_take_across_iterations.vyrn",
            "25",
            "`er.node` is consumed by `consume` inside a loop, so it would be used again on \
             the next iteration",
            "spelling",
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

/// The shapes row 07's own unit tests pinned, still refused after the rule
/// left `movecheck.rs` (RFC-0125 §3 M3, row 07).
///
/// Rule 1's move — a value moved into a binding, a container, a field, a loop
/// or a hole, and read afterwards — is the kernel's sentence now. The unit
/// tests that pinned it asked `vyrn_frontend::check` alone, and each named a
/// SHAPE rather than the rule: what a builtin's sink takes, what a record
/// literal takes, what a `for .. in consume` leaves behind, what a prefix take
/// leaves a hole in, what a take on one arm does to the other, and what a
/// stream producer takes. The row's own program is the census row `r07`; these
/// are the shapes around it, asked of the whole compiler.
#[test]
fn the_shapes_row_sevens_unit_tests_pinned_are_still_refused() {
    const BAG: &str = "type Bag = { a: String, b: String } \
                       fn make() -> Bag { return Bag { a: \"x\" + \"y\", b: \"p\" + \"q\" } } ";
    let cases: &[(&str, &str, String)] = &[
        (
            "a builtins sink",
            "was moved here into `push(..)`",
            "fn main() -> Int64 { let s = \"a\" + \"b\" let mut xs: Array<String> = [] \
             xs.push(s) return s.byteLength }"
                .to_string(),
        ),
        (
            "a record literals field",
            "was moved here into the field `R.s`",
            "type R = { s: String } \
             fn main() -> Int64 { let s = \"a\" + \"b\" let r = R { s: s } \
             return s.byteLength + r.s.byteLength }"
                .to_string(),
        ),
        (
            "a consuming loops container",
            "`xs` was moved here into the `for .. in consume` loop",
            "fn go() -> Int64 { let xs: Array<String> = [\"a\" + \"b\"] \
             let mut out: Array<String> = [] \
             for x in consume xs { out.push(x) } return xs.length } \
             fn main() -> Int64 { return 0 }"
                .to_string(),
        ),
        (
            "a prefix takes hole",
            "`d.a` was moved here into `consume`",
            format!(
                "{BAG} fn go() -> Int64 {{ let d = make() let mut o: Array<String> = [] \
                 o.push(consume d.a) return d.a.byteLength }} \
                 fn main() -> Int64 {{ return 0 }}"
            ),
        ),
        (
            "a take on one arm",
            "`d.a` was moved here into `consume`",
            format!(
                "{BAG} fn go(n: Int64) -> Int64 {{ let d = make() let mut o: Array<String> = [] \
                 if n > 0 {{ o.push(consume d.a) }} return d.a.byteLength }} \
                 fn main() -> Int64 {{ return 0 }}"
            ),
        ),
        (
            "a stream producer",
            "moved here into `fromArray(..)`",
            "fn main() -> Int64 { let xs: Array<Int64> = [1, 2] \
             let s = fromArray(xs) close(s) return xs.length }"
                .to_string(),
        ),
    ];
    let dir = common::scratch("row-seven-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, needle, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(needle) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "rule 1's move no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// The shapes rule 2's own unit tests pinned, still refused after the rule left
/// `movecheck.rs` (RFC-0125 §3 M3, rows 01, 02, 03, 27 and 34).
///
/// A borrow may not be put anywhere that outlives the call. The checker stated
/// it at one helper — `MoveCheck::store` — and the destination is what made the
/// sentence, so the unit tests that pinned it each named a DESTINATION rather
/// than the rule: module state under an export, a record literal's field, a
/// builtin's sink under a loop variable, a loop over an element read, a map key
/// both ways, a stream producer, and the two menus a projection is offered
/// either side of its root's ownership. They asked `vyrn_frontend::check`
/// alone, so they saw the checker's copy and nothing else; they are asked of
/// the whole compiler here.
///
/// The needles carry the `fix:` lines, because the menu is what these tests
/// were written for, and the licence is measured per program on the WHOLE
/// standard error: the reader loses no line when the checker stands aside.
#[test]
fn the_shapes_rule_twos_unit_tests_pinned_are_still_refused() {
    const END: &str = " fn main() -> Int64 { return 0 }";
    const BAG: &str = "type Bag = { a: String } \
                       fn make() -> Bag { return Bag { a: \"x\" + \"y\" } } ";
    let cases: &[(&str, Vec<&str>, Vec<&str>, String)] = &[
        (
            "an export stores into module state",
            vec![
                "`arg` may not be stored into module state `kept` — it is a `read` parameter",
                "fix: `arg.copy()` — an `export extern fn` may not take ownership",
            ],
            // An export may not take a String its JS caller releases, so the
            // `consume` way out does not exist and is not offered.
            vec!["consume"],
            format!("let mut kept = \"x\" export extern fn set(arg: String) {{ kept = arg }}{END}"),
        ),
        (
            "a record literals field",
            vec![
                "`x` may not be stored into the field `R.s` — it is a `read` parameter",
                "fix: declare the parameter `x: consume ..` if this function should own it",
                "fix: `x.copy()` if both sides need a value",
            ],
            vec![],
            format!(
                "type R = {{ s: String }} \
                 fn keep(x: String) -> R {{ return R {{ s: x }} }}{END}"
            ),
        ),
        (
            "a stored loop variable",
            vec![
                "`x` may not be stored into `push(..)` — it is a loop variable",
                "fix: `for x in consume xs` if the loop should take the elements",
                "fix: `x.copy()` if both sides need a value",
            ],
            vec![],
            format!(
                "fn go(xs: Array<String>) -> Int64 {{ let mut out: Array<String> = [] \
                 for x in xs {{ out.push(x) }} return out.length }}{END}"
            ),
        ),
        (
            "a loop over an element read",
            // An element read is not a temporary: the container still owns it,
            // so the loop borrows and the store is refused.
            vec![
                "`x` may not be stored into `push(..)` — it is a loop variable",
                "fix: `x.copy()` if both sides need a value",
            ],
            vec![],
            format!(
                "fn go(xs: Array<Array<String>>) -> Int64 {{ let mut out: Array<String> = [] \
                 for x in xs[0] {{ out.push(x) }} return out.length }}{END}"
            ),
        ),
        (
            "a map key read inline",
            vec![
                "`ks[0]` may not be stored into `m` — it is a `read` parameter",
                "fix: `ks[0].copy()` if both sides need a value",
            ],
            vec![],
            format!(
                "fn build(ks: Array<String>) -> Map<String, Int64> \
                 {{ let mut m: Map<String, Int64> = [:] m[ks[0]] = 1 return m }}{END}"
            ),
        ),
        (
            "a map key that is a loop variable",
            vec!["`k` may not be stored into `m` — it is a loop variable"],
            vec![],
            format!(
                "fn build(ks: Array<String>) -> Map<String, Int64> \
                 {{ let mut m: Map<String, Int64> = [:] \
                 for k in ks {{ m[k] = 1 }} return m }}{END}"
            ),
        ),
        (
            "a stream producer",
            vec!["`xs` may not be stored into `fromArray(..)` — it is a `read` parameter"],
            vec![],
            format!("fn mk(xs: Array<Int64>) -> Stream<Int64> {{ return fromArray(xs) }}{END}"),
        ),
        (
            "a projection whose root this frame owns",
            vec![
                "`d.a` may not be stored into `push(..)` — it is read out of a place that owns it",
                "fix: `consume d.a` if `d` should give it up",
            ],
            vec![],
            format!(
                "{BAG} fn go() -> Int64 {{ let d = make() let mut o: Array<String> = [] \
                 o.push(d.a) return o.length }}{END}"
            ),
        ),
        (
            "a projection of a read parameter",
            vec!["`d.a` may not be stored into `push(..)` — it is a `read` parameter"],
            // A borrowed root has no take, so the menu must not name one.
            vec!["`consume d.a`"],
            format!(
                "type Bag = {{ a: String }} \
                 fn go(d: read Bag) -> Int64 {{ let mut o: Array<String> = [] \
                 o.push(d.a) return o.length }}{END}"
            ),
        ),
    ];
    let dir = common::scratch("rule-two-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, needles, absent, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = whole_refusal_in(dir.to_path_buf(), &name, false);
        if ok {
            bad.push(format!("{what}: accepted"));
            continue;
        }
        for needle in needles {
            if !text.contains(needle) {
                bad.push(format!("{what}: wanted `{needle}`, got {text}"));
            }
        }
        for needle in absent {
            if text.contains(needle) {
                bad.push(format!("{what}: still offers `{needle}`, got {text}"));
            }
        }
        // The licence, per program: the whole refusal survives the deletion,
        // menu included.
        let (kok, ktext) = whole_refusal_in(dir.to_path_buf(), &name, true);
        if kok || ktext != text {
            bad.push(format!(
                "{what}: the kernel said `{}`",
                if kok { "nothing".to_string() } else { ktext }
            ));
        }
    }
    // The ways out compile, which is the other half of what these tests asked.
    let compiles: &[(&str, String)] = &[
        (
            "the export copies",
            format!(
                "let mut kept = \"x\" export extern fn set(arg: String) \
                 {{ kept = arg.copy() }}{END}"
            ),
        ),
        (
            "the consume signature",
            format!(
                "type R = {{ s: String }} \
                 fn keep(x: consume String) -> R {{ return R {{ s: x }} }}{END}"
            ),
        ),
        (
            "the field copies",
            format!(
                "type R = {{ s: String }} \
                 fn keep(x: String) -> R {{ return R {{ s: x.copy() }} }}{END}"
            ),
        ),
        (
            "the loop takes its container",
            format!(
                "fn go(xs: consume Array<String>) -> Int64 {{ let mut out: Array<String> = [] \
                 for x in consume xs {{ out.push(x) }} return out.length }}{END}"
            ),
        ),
        (
            "the map key copies",
            format!(
                "fn build(ks: Array<String>) -> Map<String, Int64> \
                 {{ let mut m: Map<String, Int64> = [:] m[ks[0].copy()] = 1 return m }}{END}"
            ),
        ),
    ];
    for (what, src) in compiles {
        let name = format!("ok_{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = whole_refusal_in(dir.to_path_buf(), &name, false);
        if !ok {
            bad.push(format!("{what}: refused, {text}"));
        }
    }
    assert!(
        bad.is_empty(),
        "rule 2 no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// A borrow put into a constructor is refused AT the constructor (RFC-0125 §3
/// M3, row 19).
///
/// The rule left `movecheck.rs` with two sites, and the second is why it could
/// not leave before: the pass stated it once at the constructor position and
/// again at a `return` that wraps one, in different words, so cutting the
/// first only handed the program to the second. Both are gone and the kernel
/// states it once.
///
/// The census row `r19` is a whole `read` parameter. This is the other shape,
/// the one the corpus could not see because its own unit test parsed for
/// itself: a loop variable put into `Some(..)`, which is a borrow of the
/// container the loop does not own.
#[test]
fn a_borrow_put_into_a_constructor_is_refused_at_the_constructor() {
    let dir = common::scratch("row-nineteen");
    let src = "type M = { name: String } \
               type C = { members: Array<M> } \
               fn openRule(c: C) -> Option<M> { for m in c.members { return Some(m) } \
               return None } \
               fn main() -> Int64 { let c = C { members: [] } \
               if let Some(r) = openRule(c) { return r.name.byteLength } return 0 }";
    std::fs::write(dir.join("loop_variable.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "loop_variable.vyrn", false);
    assert!(!ok, "the wrapped borrow is accepted");
    assert!(
        text.contains("may not be put into `Some(..)`"),
        "the borrow is refused at the constructor position: {text}"
    );
}

/// The shapes rule 3's own unit tests pinned, still refused after the rule left
/// `movecheck.rs` (RFC-0125 §3 M3, row 17).
///
/// A return is owned. The checker refused one at three exits and worded the
/// export's own no under all three; the kernel states it at one exit now,
/// because the core carries the exit into an `if`'s and a `match`'s arms and
/// does not release the place a returned projection reads out of. The unit
/// tests that pinned the rule asked `vyrn_frontend::check` alone, and each
/// named a SHAPE rather than the rule: a whole `read` parameter, a loop
/// variable, a local name bound to a field, module state and a field of it, a
/// binder yielded by a `match` arm, and the three spellings an export refuses.
/// They are asked of the whole compiler here.
///
/// The needle is the whole sentence, because the wording is what the deletion
/// spends: every one of these is byte-identical with the checker standing
/// aside.
#[test]
fn the_shapes_rule_threes_unit_tests_pinned_are_still_refused() {
    const TAG: &str = "type Tag = | Word(String) | Num(Int64) \
                       let mut tag = Word(\"w\") ";
    const END: &str = " fn main() -> Int64 { return 0 }";
    let cases: &[(&str, &str, String)] = &[
        (
            "a whole read parameter",
            "`s` may not be returned — it is a `read` parameter, and a return is owned",
            format!("fn id(s: String) -> String {{ return s }}{END}"),
        ),
        (
            "a loop variable",
            "`x` may not be returned — it is a loop variable, and a return is owned",
            format!(
                "fn first(xs: Array<String>) -> String \
                 {{ for x in xs {{ return x }} return \"\" }}{END}"
            ),
        ),
        (
            "a name bound to a field",
            "`t` may not be returned — it is a second name for the `read` parameter `r`, \
             and a return is owned",
            format!(
                "type R = {{ s: String }} \
                 fn get(r: R) -> String {{ let t = r.s return t }}{END}"
            ),
        ),
        (
            "module state",
            "`title` may not be returned — it is module state, which nothing may take, \
             and a return is owned",
            format!("let mut title = \"x\" fn get() -> String {{ return title }}{END}"),
        ),
        (
            "a field of module state",
            "`r.s` may not be returned — it is module state, which nothing may take, \
             and a return is owned",
            format!(
                "type R = {{ s: String }} let mut r = R {{ s: \"x\" }} \
                 fn get() -> String {{ return r.s }}{END}"
            ),
        ),
        (
            "a record of module state",
            "`r` may not be returned — it is module state, which nothing may take, \
             and a return is owned",
            format!(
                "type R = {{ s: String }} let mut r = R {{ s: \"x\" }} \
                 fn get() -> R {{ return r }}{END}"
            ),
        ),
        (
            "an arm binder",
            "`s` may not be returned — it is read out of a place that owns it, \
             and a return is owned",
            format!(
                "{TAG} fn text() -> String \
                 {{ return match tag {{ Word(s) => s, Num(n) => \"num\", }} }}{END}"
            ),
        ),
        // The three spellings an export refuses, kept in one test for the
        // reason they were put in one: the `exported` question was asked at one
        // exit, then at two, and `return q` — the plainest spelling there is —
        // kept offering ``declare the parameter `q: consume ..` ``, which the
        // same compiler then refuses at the signature (RFC-0089 M3b).
        (
            "an export returns a read parameter",
            "`q` may not be returned from an exported function — it is a `read` parameter, \
             and the JS caller releases what it is handed",
            format!("export extern fn plain(q: String) -> String {{ return q }}{END}"),
        ),
        (
            "an export returns a projection",
            "`d.s` may not be returned from an exported function — it is read out of a place \
             that owns it, and the JS caller releases what it is handed",
            format!(
                "type D = {{ s: String }} \
                 export extern fn field(q: String) -> String \
                 {{ let d = D {{ s: q.copy() }} return d.s }}{END}"
            ),
        ),
        (
            "an export returns an if arm",
            "`q` may not be returned from an exported function — it is a `read` parameter, \
             and the JS caller releases what it is handed",
            format!(
                "export extern fn pick(p: String, q: String) -> String \
                 {{ return if p == \"\" {{ q }} else {{ p }} }}{END}"
            ),
        ),
        (
            "an export returns a match arm",
            "`s` may not be returned from an exported function — it is read out of a place \
             that owns it, and the JS caller releases what it is handed",
            format!(
                "{TAG} export extern fn text() -> String \
                 {{ return match tag {{ Word(s) => s, Num(n) => \"num\", }} }}{END}"
            ),
        ),
    ];
    let dir = common::scratch("rule-three-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok {
            bad.push(format!("{what}: accepted"));
            continue;
        }
        let (_, msg) = split_head(&text);
        let first = msg.lines().next().unwrap_or_default();
        if first != *says {
            bad.push(format!("{what}: said `{first}`"));
            continue;
        }
        // The licence, per program: the whole refusal survives the deletion.
        let (kok, ktext) = refusal_in(dir.to_path_buf(), &name, true);
        if kok || ktext != text {
            bad.push(format!(
                "{what}: the kernel said `{}`",
                if kok { "nothing".to_string() } else { ktext }
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "rule 3 no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// The shapes row 25's own unit tests pinned, still refused after the rule
/// left `movecheck.rs` (RFC-0125 §3 M3, row 25).
///
/// A consumption inside a loop would run again on the next turn. The kernel
/// judges the loop's BACK EDGE against its entry, which is one rule where the
/// checker had four readings of the body: a `while` body, a `while`
/// CONDITION, a body that ends in `continue`, and a take of a projection whose
/// key is a path no scope frame holds. Each unit test named one of those, and
/// each used the refusal to see it, so they are asked of the whole compiler
/// here.
#[test]
fn the_shapes_row_twenty_fives_unit_tests_pinned_are_still_refused() {
    const T: &str = "type T = { id: Int64 } \
                     fn take(t: consume T) -> Int64 { return t.id } ";
    let cases: &[(&str, String)] = &[
        (
            "a while body",
            format!(
                "{T} fn main() -> Int64 {{ let x = T {{ id: 1 }} let mut i = 0 \
                 while i < 3 {{ let a = take(x) i = i + 1 }} return 0 }}"
            ),
        ),
        (
            "a while condition",
            "type T = { id: Int64 } \
             fn take(t: consume T) -> Bool { return t.id > 0 } \
             fn main() -> Int64 { let x = T { id: 1 } \
             while take(x) { let y = 1 } return 0 }"
                .to_string(),
        ),
        (
            "a trailing continue",
            format!(
                "{T} fn main() -> Int64 {{ let x = T {{ id: 1 }} \
                 for i in [1, 2] {{ let a = take(x) continue }} return 0 }}"
            ),
        ),
        (
            "a take of a projection",
            "type T = { node: String, rest: Int64 } \
             fn main() -> Int64 { let er = T { node: \"n\", rest: 0 } \
             for i in [1, 2] { consume er.node } return 0 }"
                .to_string(),
        ),
    ];
    let dir = common::scratch("row-twenty-five-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains("inside a loop") {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a consumption inside a loop is no longer refused:\n  {}",
        bad.join("\n  ")
    );
}

/// The shapes rows 13 and 14's own unit tests pinned, still refused after the
/// rule left `movecheck.rs` (RFC-0125 §3 M3, rows 13 and 14).
///
/// Rule 2 at the third exit: a borrow may not be handed to a declared
/// `consume` parameter. The pass asked that question of eight spellings of the
/// same argument — a whole parameter, a `read` receiver, a field of one, an
/// element, a name bound to an element, a pattern binder, a loop variable, an
/// `if` arm — and each unit test used the refusal to see a spelling. A ninth,
/// a SPAWNED call, is not here: through the whole compiler the isolation rule
/// refuses `spawn take(ys)` first, because a callee that releases is not pure.
/// The taker's own words for one are the core's (`spawn f(..)`). The
/// kernel asks it of the value, so the spelling is no longer a case; the
/// spellings are pinned here instead, with the menus, because a menu is the
/// surface knowledge a reader acts on.
#[test]
fn the_shapes_rows_thirteen_and_fourteens_unit_tests_pinned_are_still_refused() {
    const TAKE: &str = "type Bag = { xs: Array<Int64> } \
                        let g: Array<Int64> = [1] \
                        let gb: Bag = Bag { xs: [1] } \
                        fn take(xs: consume Array<Int64>) -> Int64 \
                        { let n = xs.length drop xs return n } \
                        fn takeBag(b: consume Bag) -> Int64 { return b.xs.length } ";
    let go = |sig: &str, body: &str| {
        format!("{TAKE} fn go({sig}) -> Int64 {{ {body} }} fn main() -> Int64 {{ return 0 }}")
    };
    let cases: Vec<(&str, Vec<&str>, String)> = vec![
        (
            "a read parameter",
            vec![
                "`ys` may not be passed to a `consume` parameter via `take(..)` — it is a \
                 `read` parameter",
                "fix: declare the parameter `ys: consume ..`",
                "fix: `ys.copy()`",
            ],
            go("ys: read Array<Int64>", "return take(ys)"),
        ),
        (
            "a modify parameter",
            vec!["it is a `modify` parameter"],
            go("ys: modify Array<Int64>", "return take(ys)"),
        ),
        (
            "a read receiver",
            vec!["`self` may not be passed"],
            format!(
                "{TAKE} protocol Giving {{ fn give(read self) -> Int64 }} \
                 impl Giving for Bag {{ fn give(read self) -> Int64 \
                 {{ return takeBag(self) }} }} fn main() -> Int64 {{ return 0 }}"
            ),
        ),
        (
            "a field of a read receiver",
            vec!["`self.xs` may not be passed"],
            format!(
                "{TAKE} protocol Giving {{ fn give(read self) -> Int64 }} \
                 impl Giving for Bag {{ fn give(read self) -> Int64 \
                 {{ return take(self.xs) }} }} fn main() -> Int64 {{ return 0 }}"
            ),
        ),
        (
            "an if arm",
            vec!["may not be passed to a `consume` parameter"],
            "type T = { title: String, id: Int64 } \
             fn sink(s: consume String) -> Int64 { return 0 } \
             fn main() -> Int64 { let d = T { title: \"t\", id: 1 } \
             let r = sink(if 1 > 0 { d.title } else { \"\" }) return r }"
                .to_string(),
        ),
        (
            "a field of a borrowed record",
            vec!["`b.xs` may not be passed"],
            go("b: read Bag", "return take(b.xs)"),
        ),
        (
            "an element of a borrowed container",
            vec!["`ns[0]` may not be passed"],
            go("ns: read Array<Array<Int64>>", "return take(ns[0])"),
        ),
        (
            "a name bound to an element",
            vec!["`n` may not be passed"],
            go(
                "ns: read Array<Array<Int64>>",
                "let n = ns[0] return take(n)",
            ),
        ),
        (
            "a pattern binder",
            vec!["`v` may not be passed"],
            go(
                "o: read Option<Array<Int64>>",
                "return match o { Some(v) => take(v), None => 0 }",
            ),
        ),
        (
            "a loop variable",
            vec!["it is a loop variable", "fix: `for r in consume ns`"],
            go(
                "",
                "let ns: Array<Array<Int64>> = [[1]] let mut t = 0 \
                 for r in ns { t = t + take(r) } return t",
            ),
        ),
        (
            "a field of a place this frame owns",
            vec!["`b.xs` may not be passed", "fix: `consume b.xs`"],
            go("", "let b = Bag { xs: [1] } return take(b.xs)"),
        ),
        (
            "an element of a container this frame owns",
            vec!["`nn[0]` may not be passed", "fix: `nn[0].copy()`"],
            go("", "let nn: Array<Array<Int64>> = [[1]] return take(nn[0])"),
        ),
        (
            "a projection of module state",
            vec![
                "`gb.xs` may not be passed to a `consume` parameter",
                "module state",
            ],
            go("", "return take(gb.xs)"),
        ),
    ];
    let dir = common::scratch("third-exit-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, needles, src) in &cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = whole_refusal_in(dir.to_path_buf(), &name, false);
        if ok {
            bad.push(format!("{what}: accepted"));
            continue;
        }
        for needle in needles {
            if !text.contains(needle) {
                bad.push(format!("{what}: wanted `{needle}`, got {text}"));
            }
        }
    }
    // The ways out compile, and a value this frame owns still moves.
    let compiles: &[(&str, String)] = &[
        (
            "the consume signature",
            go("ys: consume Array<Int64>", "return take(ys)"),
        ),
        (
            "the copy",
            go("ys: read Array<Int64>", "return take(ys.copy())"),
        ),
        (
            "an owned value",
            go("", "let xs: Array<Int64> = [1] return take(xs)"),
        ),
        (
            "the prefix take",
            go(
                "",
                "let b = Bag { xs: [1] } let ys = consume b.xs return take(ys)",
            ),
        ),
    ];
    for (what, src) in compiles {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if !ok {
            bad.push(format!("{what}: refused: {text}"));
        }
    }
    assert!(
        bad.is_empty(),
        "rule 2 at the third exit has moved:\n  {}",
        bad.join("\n  ")
    );
}

/// The shapes rows 10, 11 and 29's own unit tests pinned, still refused after
/// the rule left `movecheck.rs` (RFC-0125 §3 M3, rows 10, 11 and 29).
///
/// The prefix `consume` form. The pass named the FORM a reader wrote — "a
/// take", "a `for` loop" — and the kernel names the TAKER the value reaches:
/// the `consume` parameter a call hands it to, or the loop that took the
/// container. Both are true and the kernel's is the more exact of the two,
/// because it says which taker, so the checker's copy went.
///
/// One case moved as text and it is the third here: `for x in consume r.xs`
/// on a record this frame owns. The unit test asserted it COMPILES, and it
/// does not — it asked `vyrn_frontend::check`, which is the checker alone, and
/// the whole compiler has refused that program since the kernel came in. The
/// pin says what the compiler says.
#[test]
fn the_shapes_rows_ten_eleven_and_twenty_nines_unit_tests_pinned_are_still_refused() {
    const DECLS: &str = "type Bag = { a: String } \
                         type R = { xs: Array<String> } \
                         let g: String = \"m\" \
                         let gs: Array<String> = [] \
                         fn make() -> R { return R { xs: [\"a\"] } } \
                         fn take(xs: consume Array<String>) -> Int64 { return xs.length } ";
    let go = |sig: &str, body: &str| {
        format!(
            "{DECLS} fn go({sig}) -> Int64 {{ let mut o: Array<String> = [] {body} \
             return o.length }} fn main() -> Int64 {{ return 0 }}"
        )
    };
    let cases: Vec<(&str, Vec<&str>, String)> = vec![
        (
            "a consuming loop over a read parameter",
            vec![
                "`xs` may not be stored into the `for .. in consume` loop — it is a `read` \
                 parameter",
                "fix: declare the parameter `xs: consume ..`",
            ],
            go(
                "xs: read Array<String>",
                "for x in consume xs { o.push(x) }",
            ),
        ),
        (
            "a consuming loop over a field of a read parameter",
            vec![
                "`r.xs` may not be stored into the `for .. in consume` loop — it is a `read` \
                 parameter",
                "fix: declare the parameter `r: consume ..`",
            ],
            go("r: read R", "for x in consume r.xs { o.push(x) }"),
        ),
        (
            "a consuming loop over a field this frame owns",
            vec![
                "`r.xs` may not be stored into the `for .. in consume` loop — it is read out of \
                 a place that owns it",
                "fix: `consume r.xs`",
            ],
            go("", "let r = make() for x in consume r.xs { o.push(x) }"),
        ),
        (
            "a consuming loop over module state",
            vec![
                "module state `gs` may not be consumed by the `for .. in consume` loop",
                "nothing may take ownership of module state",
            ],
            go("", "for x in consume gs { o.push(x) }"),
        ),
        (
            "a prefix take of a field of a read parameter",
            vec![
                "`d` may not be consumed — it is a `read` parameter",
                "fix: `d.a.copy()`",
            ],
            go("d: read Bag", "o.push(consume d.a)"),
        ),
        (
            "a prefix take of module state",
            vec![
                "module state `g` may not be passed to a `consume` parameter via `push(..)`",
                "nothing may take ownership of module state",
            ],
            go("", "o.push(consume g)"),
        ),
        (
            "a prefix take of module state at a call",
            vec!["module state `gs` may not be passed to a `consume` parameter via `take(..)`"],
            go("", "let n = take(consume gs) o.push(\"x\")"),
        ),
    ];
    let dir = common::scratch("prefix-consume-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, needles, src) in &cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = whole_refusal_in(dir.to_path_buf(), &name, false);
        if ok {
            bad.push(format!("{what}: accepted"));
            continue;
        }
        for needle in needles {
            if !text.contains(needle) {
                bad.push(format!("{what}: wanted `{needle}`, got {text}"));
            }
        }
    }
    // The ways out compile, and a container this frame owns is still the
    // loop's to take.
    let compiles: &[(&str, String)] = &[
        (
            "the consume signature",
            go(
                "xs: consume Array<String>",
                "for x in consume xs { o.push(x) }",
            ),
        ),
        (
            "a container this frame owns",
            go(
                "",
                "let xs: Array<String> = [\"a\" + \"b\"] for x in consume xs { o.push(x) }",
            ),
        ),
        (
            "the take at the place the field is bound",
            go(
                "",
                "let r = make() let ys = consume r.xs for x in consume ys { o.push(x) }",
            ),
        ),
    ];
    for (what, src) in compiles {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if !ok {
            bad.push(format!("{what}: refused: {text}"));
        }
    }
    assert!(
        bad.is_empty(),
        "the prefix `consume` form has moved:\n  {}",
        bad.join("\n  ")
    );
}

/// A record literal's part names the FIELD it goes into, at both doors a
/// literal is bound through (RFC-0125 §3 M3, row 07).
///
/// `let r = R { s: x }` and `return R { s: x }` are one literal written twice.
/// The core wrote the field names on the binding a reader's `let` makes and
/// not on the temporary an inline literal gets, so the second was told the
/// value went into "the literal" — a word for the machinery, where the first
/// was told the field. No program of the corpus spells the second with a
/// borrow in it, which is why this is a pin and not a fixture: the licence
/// could not see the difference, and the next reader would meet it.
///
/// Both passes are asked, because both state the sentence today.
#[test]
fn a_record_literals_part_names_its_field_at_both_doors() {
    const DECLS: &str = "type R = { s: String } \
                         fn mk() -> String { return \"a\" + \"b\" } \
                         fn take(r: consume R) -> Int64 { return r.s.byteLength } ";
    let cases: &[(&str, &str, &str)] = &[
        (
            "a borrow into a let's literal",
            "the field `R.s`",
            "fn go(x: read String) -> Int64 { let r = R { s: x } return r.s.byteLength } \
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a borrow into an inline literal",
            "the field `R.s`",
            "fn go(x: read String) -> R { return R { s: x } } \
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a move into a let's literal",
            "the field `R.s`",
            "fn go() -> Int64 { let d = mk() let r = R { s: d } \
             return r.s.byteLength + d.byteLength } fn main() -> Int64 { return 0 }",
        ),
        (
            "a move into an inline literal",
            "the field `R.s`",
            "fn go() -> Int64 { let d = mk() let n = take(R { s: d }) \
             return n + d.byteLength } fn main() -> Int64 { return 0 }",
        ),
        // An array has no field names, so neither pass invents one. The two
        // still spell the literal differently — "the array literal" and "the
        // literal" — and that is the store rule's wording, not this row's.
        (
            "an array literal, which has no field to name",
            "literal",
            "fn go(x: read String) -> Int64 { let a: Array<String> = [x] return a.length } \
             fn main() -> Int64 { return 0 }",
        ),
    ];
    let dir = common::scratch("literal-parts");
    let mut bad: Vec<String> = Vec::new();
    for (what, needle, body) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), format!("{DECLS} {body}")).expect("write the program");
        for kernel_mode in [false, true] {
            let (ok, text) = refusal_in(dir.to_path_buf(), &name, kernel_mode);
            let pass = if kernel_mode {
                "the kernel"
            } else {
                "the checker"
            };
            if ok {
                bad.push(format!("{what}: {pass} accepted it"));
            } else if !text.contains(needle) {
                bad.push(format!("{what}: {pass} wanted `{needle}`, got {text}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "a literal's part has lost its field:\n  {}",
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

/// The programs the pass's own unit tests read as ACCEPTED, asked of the whole
/// compiler (RFC-0125 §3 M3, the safety slice).
///
/// They asserted `movecheck::check(..).is_ok()`, and this pass going quiet is
/// not acceptance: the kernel states ownership rules the pass does not, and
/// `vyrn-frontend` does not link the kernel, so the pass's own unit tests never
/// ask it. Thirty-six readings of "this program compiles" were readings of "the
/// checker has nothing to say". Thirty-five were right anyway. The thirty-sixth
/// is the row that carries a sentence: the compiler refuses that program, and
/// has refused it since the kernel came in.
///
/// A row with no sentence compiles. A row with one is refused, and the sentence
/// is what a reader gets.
#[test]
fn the_programs_the_passs_unit_tests_read_as_accepted() {
    let cases: &[(&str, Option<&str>, &str)] = &[
        (
            "a payload binding from module state",
            None,
            "type E = | Tag(Array<Int64>) | Blank\n\
             let mut g: E = Blank\n\
             fn main() -> Int64 {\n\
                 let Tag(xs) = g\n\
                 return xs.length\n\
             }",
        ),
        (
            "a read parameter read twice",
            None,
            "type T = { id: Int64 }\n\
             fn take(t: consume T) -> Int64 { return t.id }\n\
             fn peek(t: read T) -> Int64 { return t.id }\n\
             fn main() -> Int64 { let x = T { id: 1 } return peek(x) + peek(x) }",
        ),
        (
            "a consume with no reuse",
            None,
            "type T = { id: Int64 }\n\
             fn take(t: consume T) -> Int64 { return t.id }\n\
             fn peek(t: read T) -> Int64 { return t.id }\n\
             fn main() -> Int64 { let x = T { id: 1 } return take(x) }",
        ),
        (
            "a reassignment revives",
            None,
            "type T = { id: Int64 }\n\
             fn take(t: consume T) -> Int64 { return t.id }\n\
             fn peek(t: read T) -> Int64 { return t.id }\n\
             fn main() -> Int64 { let mut x = T { id: 1 } let a = take(x) x = T { id: 2 } return a + take(x) }",
        ),
        (
            "a partial take of the loop variable",
            None,
            "type E = { name: String, id: Int64 }\n\
             fn main() -> Int64 { let mut out = 0\n\
              let xs = [E { name: \"a\", id: 1 }, E { name: \"b\", id: 2 }]\n\
              for u in consume xs { consume u.name } return out }",
        ),
        (
            "a local shadowing a global",
            None,
            "type T = { id: Int64 }\n\
             let g = T { id: 1 }\n\
             fn take(t: consume T) -> Int64 { return t.id }\n\
             fn useIt() -> Int64 { let g = T { id: 2 } return take(g) }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a consume on the break branch",
            None,
            "type T = { id: Int64 }\n\
             fn take(t: consume T) -> Int64 { return t.id }\n\
             fn peek(t: read T) -> Int64 { return t.id }\n\
             fn main() -> Int64 { let x = T { id: 1 } let mut s = 0\n\
              for i in [0, 1, 2] { if i == 2 { let a = take(x) break }\n\
              s = s + peek(x) }\n\
              return s }",
        ),
        (
            "a use after an unconditional break",
            None,
            "type T = { id: Int64 }\n\
             fn take(t: consume T) -> Int64 { return t.id }\n\
             fn peek(t: read T) -> Int64 { return t.id }\n\
             fn main() -> Int64 { let x = T { id: 1 }\n\
              while true { break let a = take(x) let b = take(x) }\n\
              return 0 }",
        ),
        (
            "a last use of a string moves",
            None,
            "fn main() -> Int64 { let s = \"a\" + \"b\" let t = s return t.byteLength }",
        ),
        (
            "a scalar alias never moves",
            None,
            "fn main() -> Int64 { let a = 1 let b = a return a + b }",
        ),
        (
            "the consume fix for a returned borrow",
            None,
            "fn id(s: consume String) -> String { return s }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "the copy fix for a returned borrow",
            None,
            "fn id(s: String) -> String { return s.copy() }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a wrapped borrows copy is owned",
            None,
            "type M = { name: String }\n\
             type C = { members: Array<M> }\n\
             fn openRule(c: C) -> Option<M> { for m in c.members { return Some(m.copy()) }\n\
             return None }\n\
             fn main() -> Int64 { let c = C { members: [] }\n\
             if let Some(r) = openRule(c) { return r.name.byteLength } return 0 }",
        ),
        (
            "an if let over a parameter",
            None,
            "fn show(v: Option<String>) -> Int64 {\n\
             if let Some(s) = v { return s.byteLength } return 0 }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a returned scalar parameter",
            None,
            "fn id(n: Int64) -> Int64 { return n }\n\
             fn main() -> Int64 { return id(1) }",
        ),
        (
            "a loop variable copied out",
            None,
            "fn first(xs: Array<String>) -> String { for x in xs { return x.copy() }\n\
             return \"\" }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "two fields read out of one record",
            None,
            "type R = { a: String, b: String }\n\
             fn use2(r: R) -> Int64 { let x = r.a let y = r.b\n\
             return x.byteLength + y.byteLength }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "module state copied out",
            None,
            "let mut title = \"x\"\n\
             fn get() -> String { return title.copy() }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a record of scalars returned",
            None,
            "type R = { n: Int64 }\n\
             let mut r = R { n: 1 }\n\
             fn get() -> R { return r }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "the copy fix in a match arm",
            None,
            "type Tag = | Word(String) | Num(Int64)\n\
             let mut tag: Tag = Num(1)\n\
             fn text() -> String { return match tag { Word(s) => s.copy(), Num(n) => \"num\", } }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "the copy fix in an exports match arm",
            None,
            "type Tag = | Word(String) | Num(Int64)\n\
             let mut tag: Tag = Num(1)\n\
             export extern fn text() -> String { return match tag { Word(s) => s.copy(), Num(n) => \"num\", } }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "an export stores a copy into module state",
            None,
            "let mut kept = \"x\"\n\
             export extern fn set(arg: String) { kept = arg.copy() }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "the consume fix for a stored borrow",
            None,
            "type R = { s: String }\n\
             fn keep(x: consume String) -> R { return R { s: x } }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "the copy fix for a stored borrow",
            None,
            "type R = { s: String }\n\
             fn keep(x: String) -> R { return R { s: x.copy() } }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a consuming loop stores its element",
            None,
            "fn go() -> Int64 { let xs: Array<String> = [\"a\" + \"b\"]\n\
             let mut out: Array<String> = []\n\
             for x in consume xs { out.push(x) } return out.length }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a loop over a temporary stores its element",
            None,
            "fn make() -> Array<String> { return [\"a\" + \"b\"] }\n\
             fn go() -> Int64 { let mut out: Array<String> = []\n\
             for x in make() { out.push(x) } return out.length }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a function called consume",
            None,
            "fn consume(n: Int64) -> Array<Int64> { return [n] }\n\
             fn main() -> Int64 { let mut t = 0 for x in consume(1) { t = t + x }\n\
             return t }",
        ),
        (
            "a call to consume in an argument",
            None,
            "fn consume(n: Int64) -> Array<Int64> { return [n] }\n\
             fn take(xs: consume Array<Int64>) -> Int64 { return xs.length }\n\
             fn main() -> Int64 { return take(consume(1)) }",
        ),
        (
            "a sibling field survives a take",
            None,
            "type Bag = { a: String, b: String }\n\
             fn make() -> Bag { return Bag { a: \"x\" + \"y\", b: \"p\" + \"q\" } }\n\
             fn go() -> Int64 { let d = make() let mut o: Array<String> = [] o.push(consume d.a) o.push(consume d.b) return o.length }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a write fills the hole",
            None,
            "type Bag = { a: String, b: String }\n\
             fn make() -> Bag { return Bag { a: \"x\" + \"y\", b: \"p\" + \"q\" } }\n\
             fn go() -> Int64 { let mut d = make() let mut o: Array<String> = [] o.push(consume d.a) d.a = \"z\" return d.a.byteLength }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "an owned capture at a consume fn parameter",
            None,
            "fn reg(f: consume fn(Int64) -> Int64) -> Int64 { return f(0) }\n\
             fn go() -> Int64 { let s = \"a\" + \"b\" return reg(n -> n + s.byteLength) }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a borrowing capture at a plain fn parameter",
            None,
            "fn apply(f: fn(Int64) -> Int64) -> Int64 { return f(0) }\n\
             fn go(q: read String) -> Int64 { return apply(n -> n + q.byteLength) }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a lender forwarded through an aggregate",
            // Row 17's slice carried the exit into the arm, so the kernel
            // names the loop variable the reader wrote and not the
            // temporary the core minted.
            Some("`x` may not be returned — it is a loop variable, and a return is owned"),
            "type R = { name: String }\n\
             fn pick(xs: Array<String>) -> String\n\
             { for x in xs { return if true { x } else { \"\" } } return \"\" }\n\
             fn g(a: Array<String>) -> R { return R { name: pick(a) } }\n\
             fn h(a: Array<String>) -> Array<String> { return [pick(a)] }\n\
             fn main() -> Int64 { let arr: Array<String> = [\"a\" + \"b\"]\n\
             let r = g(arr) let s2 = h(arr)\n\
             return r.name.byteLength + s2[0].byteLength }",
        ),
        (
            "a copied map key",
            None,
            "fn build(ks: Array<String>) -> Map<String, Int64>\n\
             { let mut m: Map<String, Int64> = [:] m[ks[0].copy()] = 1 return m }\n\
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a fresh map key",
            None,
            "fn main() -> Int64 { let mut m: Map<String, Int64> = [:]\n\
             m[\"a\" + \"b\"] = 1 return 0 }",
        ),
        (
            "a capture in a lambda the callee borrows",
            None,
            "fn apply(f: fn(Int64) -> Int64) -> Int64 { return f(1) }\n\
             fn go(s: String) -> Int64 { return apply(n -> n + s.byteLength) }\n\
             fn main() -> Int64 { return 0 }",
        ),
    ];
    let dir = common::scratch("read-as-accepted");
    let mut bad: Vec<String> = Vec::new();
    for (what, refused, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        match refused {
            None if !ok => bad.push(format!("{what}: refused — {text}")),
            Some(sentence) if ok => bad.push(format!("{what}: accepted, wanted `{sentence}`")),
            Some(sentence) if !text.contains(sentence) => {
                bad.push(format!("{what}: wanted `{sentence}`, got {text}"))
            }
            _ => {}
        }
    }
    assert!(
        bad.is_empty(),
        "a program the pass's unit tests read as accepted no longer reads that way:\n  {}",
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
// Accumulation (RFC-0125 §3 M3, the accumulation slice).
//
// The two passes are one list, and the two programs below are the pair that
// priced it. A file with a must-use error AND an ownership one used to be
// asked of the kernel not at all, so a rule that left took its sentence out of
// that file rather than moving it; and a plain merge said one mistake twice.
// Both are pinned here because both are invisible in a program with one error
// in it, which every census row is.
// ---------------------------------------------------------------------------

/// Every diagnostic a file earns, one per `file:line:col:` head.
fn heads(dir: PathBuf, file: &str) -> Vec<String> {
    let path = dir.join(file);
    let out = vyrn().arg("check").arg(&path).output().expect("vyrn check");
    String::from_utf8_lossy(&out.stderr)
        .replace("\r\n", "\n")
        .lines()
        .filter(|l| !l.starts_with(' '))
        .map(|l| split_head(l).1)
        .collect()
}

/// A file with two kinds of error in it gets both, and a rule that leaves
/// `movecheck.rs` keeps its sentence there.
///
/// `examples/mustuse_abandoned.vyrn` is the only program of the corpus that
/// breaks a must-use obligation AND an ownership rule. The must-use walk is in
/// `movecheck.rs` too, so the load used to fail before the kernel was ever
/// asked: rows 20 and 21 were licensed by the corpus and could not leave,
/// because leaving would have REMOVED their two sentences from this file
/// instead of moving them.
///
/// Both rules have left, and the file keeps FIVE of its six diagnostics. Row
/// 21's sentence about `ops` is the kernel's now, in the same words at the same
/// line, which is what this file was the obstacle to. Row 20's sentence about
/// `b` is gone, and it is gone to the rule that keeps the merge from adding
/// rather than to the deletion: the must-use walk already refuses `b` — a `Txn`
/// disposed twice, on line 59 — so the kernel's second sentence about `b` at
/// line 61 is the same mistake said twice, and is dropped exactly as `r31`'s is
/// (see below). One binding, one sentence, whichever pass states it.
#[test]
fn a_file_with_a_must_use_error_still_gets_its_ownership_refusals() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .unwrap();
    let got = heads(dir, "mustuse_abandoned.vyrn");
    assert_eq!(got.len(), 5, "{got:#?}");
    assert!(
        got.iter().any(|d| d
            == "`ops` may not be dropped — it is a second name for the `read` parameter `self`"),
        "{got:#?}"
    );
    assert!(
        !got.iter()
            .any(|d| d == "`b` is dropped here but was already consumed by `finish(..)` on line 60"),
        "one binding, one sentence: {got:#?}"
    );
}

/// One mistake gets one sentence, whichever pass states it.
///
/// The kernel refuses `r31`'s second `close(s)` as a use after a take, and the
/// must-use walk refuses `s` as an obligation discharged twice. They are one
/// mistake, and the checker never printed both — `examples/expected/*.stderr`
/// was recorded before the first rule left this file and holds one sentence for
/// each such program. So the driver drops a kernel refusal about a binding the
/// file already refuses. Measured over the corpus, this is what six programs
/// turn on, `r31` among them.
#[test]
fn the_kernel_does_not_say_again_what_the_checker_said_about_the_same_binding() {
    let got = heads(dir(), "r31_stream_disposed_twice.vyrn");
    assert_eq!(
        got,
        vec!["`s` is a `Stream` and is disposed more than once".to_string()],
        "{got:#?}"
    );
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
    ///
    /// **The column is empty** (RFC-0125 §3 M3). Rule 2 at a store was the last
    /// of it, and it left with rows 01, 02, 03, 27 and 34. What still refuses
    /// in this file is either the checker's own or an arm of the walk, and the
    /// kind stays so a duplicate that reappears is classified rather than lost.
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
            "pub struct Facts {",
            Rows,
            "what one walk answers, and the closures over the call graph the \
             core reads at a call",
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
            "pub fn arg_caps(program: &Program) -> HashMap<String, Vec<Capability>> {",
            Rows,
            "the capability of a position, the producer screens, and the \
             verdict for one argument temporary — read at a position instead \
             of at a binding, by this pass and by the core alike",
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
            "pub fn refusal(program: &Program) -> String {",
            Shared,
            "the historical string shim, and the door that has no acceptance \
             answer in it",
        ),
        sec(
            "struct MoveCheck<'a> {",
            Shared,
            "the pass's state: the scope stacks, the sinks, the recorded rows",
        ),
        sec(
            "enum Borrow {",
            Checker,
            "what a borrow is, in words. `core::BorrowKind::what` is the same \
             sentence, so nothing here is owed; what reads this one now is \
             `check_take` and the two closure rules, which the kernel does \
             not give",
        ),
        sec(
            "    fn fixes(&self, root: &str, path: &str) -> Vec<String> {",
            Menu,
            "the named ways out of a borrow error",
        ),
        sec(
            "pub fn root_of(path: &str) -> &str {",
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
            Rows,
            "whether a value reads a place that owns it. No refusal exit reads \
             it: every caller writes a row — `Gone::Borrowed`, a temporary's \
             owning flag, an arm's slot, a loop's",
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
            Shared,
            "a rebuilding builtin takes its receiver — a delegation to the free \
             `sinks` below, which reads the declaration",
        ),
        sec(
            "    fn store(",
            Rows,
            "what a store still records once rule 2 is the kernel's (rows 01, \
             02, 03, 27, 34): `Gone::Moved`'s row, the consumed entry, the \
             projection instrument and the retention record",
        ),
        sec(
            "    fn borrow_from(&self, value: &Expr) -> Option<Borrow> {",
            Shared,
            "the borrow status a `let` of a value gives its binding — the \
             borrow table's producer, and no refusal of its own",
        ),
        sec(
            "    fn payload_binding(",
            Shared,
            "what a pattern's binders name, and whether an iterable is a place",
        ),
        sec(
            "    fn callee_keeps(&self, callee: &str, i: usize) -> bool {",
            Shared,
            "whether a callee keeps a `fn` value",
        ),
        sec(
            "    fn note_return(&self, e: &Expr, line: usize) {",
            Rows,
            "what a `return` still records once rule 3 is the kernel's: the              projection instrument, and the lend the call graph is closed over              (rows 15, 16, 17, 18)",
        ),
        sec(
            "    fn note_handover(&self, arg: &Expr, callee: &str, i: usize, line: usize) {",
            Rows,
            "the retention and hand-over records the call graph is closed over",
        ),
        sec(
            "    fn note_arm_aliases(&self, e: &Expr, line: usize, binders: &[String]) {",
            Rows,
            "an arm that yields a place, and what naming one costs",
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
            Rows,
            "the first borrow a returned expression yields. Rule 3 left with \
             row 17, so the readers are RFC-0092's instrument and the lend the \
             call graph is closed over",
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
            "pub fn sub_blocks(s: &Stmt) -> Vec<&Block> {",
            Shared,
            "what an expression names, and on which of its paths — the tree \
             questions the must-use judgment asks from the lowering",
        ),
        sec(
            "fn store_path(e: &Expr) -> Option<String> {",
            Shared,
            "the place an expression names, as the store arms spell it",
        ),
        sec(
            "fn sinks(decl: &Declared, name: &str, i: usize) -> bool {",
            Shared,
            "whether a builtin's parameter takes its argument for good — read \
             off `prelude::signature` and `prelude::rebuilds`, where the rule \
             is stated once for this pass and the core alike",
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
        ("a rule the kernel now gives", 0),
        ("a rule only the checker gives", 126),
        ("placement rows for the engines", 1608),
        ("a fix menu", 73),
        ("shared machinery", 3564),
        ("tests", 703),
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
