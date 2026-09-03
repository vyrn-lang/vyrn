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
//! wording in the table. `VYRN_NO_MOVECHECK=1 VYRN_KERNEL_STRICT=1 vyrn check`
//! stands the checker aside so the kernel's own sentence is reachable — the
//! checker refuses each of these first, so without the knob the kernel is
//! never asked (RFC-0125 §3 M3, "wordings").
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
            Kernel::No,
        ),
        row(
            "r09_nothing_to_take.vyrn",
            "`consume` with nothing to take",
            "RFC-0093",
            "`consume` here has nothing to take — the value is already owned, so there is no \
             place to leave a hole in",
            Kernel::No,
        ),
        row(
            "r10_consume_module_state.vyrn",
            "module state may not be taken: a prefix `consume`",
            "RFC-0013",
            "module state `names` may not be consumed by a take — nothing may take ownership of \
             module state (it lives for the whole module and is never dropped)",
            Kernel::No,
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
            Kernel::Other("read out of `names`, a place that owns it"),
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
            Kernel::Other("read out of `names`, a place that owns it"),
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
            Kernel::Other(
                "`s` may not be returned — it is a `read` parameter, and a return is owned",
            ),
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
            Kernel::Other("was already consumed by `take(..)` on line 6"),
        ),
        row(
            "r21_drop_a_borrow.vyrn",
            "rule 4 at the drop: the place that owns a value releases it",
            "RFC-0089",
            "`owned` may not be dropped — it is read out of a place that owns it",
            Kernel::Other("is released although the body does not own it"),
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
            Kernel::No,
        ),
        row(
            "r29_for_in_consume_module_state.vyrn",
            "module state may not be taken: `for .. in consume`",
            "RFC-0013",
            "module state `names` may not be consumed by a `for` loop — nothing may take \
             ownership of module state (it lives for the whole module and is never dropped)",
            Kernel::Other("is released although the body does not own it"),
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

/// The command's standard error, with the `fix:` and `note:` menu lines and
/// the file prefix taken off, so what is left is the sentence.
fn refusal(file: &str, kernel_mode: bool) -> (bool, String) {
    let path = dir().join(file);
    let mut cmd = vyrn();
    cmd.arg("check").arg(&path);
    if kernel_mode {
        cmd.env("VYRN_NO_MOVECHECK", "1")
            .env("VYRN_KERNEL_STRICT", "1");
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
