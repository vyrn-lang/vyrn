//! The refusal census: one minimal program per ownership refusal, the
//! sentence it gets, and which pass states it. It pins the sentences so they do
//! not move.
//!
//! Each row runs twice: `vyrn check` must print the table's wording, and
//! `VYRN_NO_KERNEL=1 vyrn check` stands the kernel aside to attribute it.
//!
//! A row holds one error, so accumulation is pinned by fixtures instead:
//! `examples/expected/mustuse_abandoned.stderr` and
//! `refusals/r31_stream_disposed_twice.vyrn`.

use std::path::{Path, PathBuf};

mod common;
use common::vyrn;

/// Which pass states a row's refusal, measured by standing the kernel aside.
#[derive(PartialEq, Eq, Debug)]
enum Kernel {
    /// The kernel's own. `VYRN_NO_KERNEL=1` accepts the program.
    Its,
    /// Another pass's: the refusal survives `VYRN_NO_KERNEL=1`, word for word.
    Elsewhere,
}

/// One row. `says` is the whole message without the `fix:`/`note:` menu.
struct Row {
    file: &'static str,
    rule: &'static str,
    says: &'static str,
    kernel: Kernel,
}

const fn row(file: &'static str, rule: &'static str, says: &'static str, kernel: Kernel) -> Row {
    Row {
        file,
        rule,
        says,
        kernel,
    }
}

/// One row per refusal site.
fn census() -> Vec<Row> {
    vec![
        row(
            "r01_store_element.vyrn",
            "rule 2: an element read may not be stored",
            "`b.xs[0]` may not be stored into `push(..)` — it is read out of a place that owns it",
            Kernel::Its,
        ),
        row(
            "r02_store_read_parameter_field.vyrn",
            "rule 2: a field of a `read` parameter may not be stored",
            "`h.meta[0]` may not be stored into `push(..)` — it is a `read` parameter",
            Kernel::Its,
        ),
        row(
            "r03_store_projection.vyrn",
            "rule 2: a projection is a borrow of its root, whatever the root is",
            "`d.title` may not be stored into `push(..)` — it is read out of a place that owns it",
            Kernel::Its,
        ),
        row(
            "r04_whole_after_a_hole.vyrn",
            "a name with a hole may not be used whole",
            "`p.name` was taken out of `p` here\nline 10: ... and `p` is used as a whole here, \
             with the hole still in it",
            Kernel::Its,
        ),
        row(
            "r05_alias_then_write.vyrn",
            "a write to a place ends every alias that reads out of it",
            "`t.xs[..]` is written here while `before` still reads out of it\nline 9: ... and \
             `before` is used again here",
            Kernel::Its,
        ),
        row(
            "r06_use_after_consume.vyrn",
            "rule 1: a `consume` parameter takes ownership",
            "`x` is used here but was already consumed by `take(..)` on line 8\n  (a `consume` \
             parameter takes ownership; the value can't be used afterward)",
            Kernel::Its,
        ),
        row(
            "r07_moved_into_a_binding.vyrn",
            "rule 1: a move into a binding, and a use of the source after it",
            "`s` was moved here into the binding `t`\nline 5: ... and `s` is used again here",
            Kernel::Its,
        ),
        row(
            "r08_take_an_element.vyrn",
            "`consume` reaches a field, never an element",
            "`xs[0]` may not be taken — an element is not a place a take reaches",
            Kernel::Its,
        ),
        row(
            "r09_nothing_to_take.vyrn",
            "`consume` with nothing to take",
            "`consume` here has nothing to take — the value is already owned, so there is no \
             place to leave a hole in",
            Kernel::Its,
        ),
        row(
            "r10_consume_module_state.vyrn",
            "module state may not be taken: a prefix `consume`",
            "module state `names` may not be passed to a `consume` parameter via `take(..)` — \
             nothing may take ownership of module state (it lives for the whole module and is \
             never dropped)",
            Kernel::Its,
        ),
        row(
            "r11_consume_a_read_parameter.vyrn",
            "rule 2: a prefix `consume` of a `read` parameter",
            "`ys` may not be passed to a `consume` parameter via `take(..)` — it is a `read` \
             parameter",
            Kernel::Its,
        ),
        row(
            "r12_module_state_to_a_consume_parameter.vyrn",
            "module state may not be taken: a `consume` parameter",
            "module state `names` may not be passed to a `consume` parameter via `take(..)` — \
             nothing may take ownership of module state (it lives for the whole module and is \
             never dropped)",
            Kernel::Its,
        ),
        row(
            "r13_read_parameter_to_a_consume_parameter.vyrn",
            "rule 2: a whole `read` parameter to a `consume` parameter",
            "`ys` may not be passed to a `consume` parameter via `take(..)` — it is a `read` \
             parameter",
            Kernel::Its,
        ),
        row(
            "r14_projection_to_a_consume_parameter.vyrn",
            "rule 2: a projection to a `consume` parameter",
            "`d.title` may not be passed to a `consume` parameter via `take(..)` — it is read \
             out of a place that owns it",
            Kernel::Its,
        ),
        row(
            "r15_return_module_state.vyrn",
            "module state may not be taken: a `return`",
            "`names` may not be returned — it is module state, which nothing may take, and a \
             return is owned",
            Kernel::Its,
        ),
        row(
            "r16_return_a_field_of_a_read_parameter.vyrn",
            "rule 2 at the return: a field of a `read` parameter",
            "`d.title` may not be returned — it is a `read` parameter, and a return is owned",
            Kernel::Its,
        ),
        row(
            "r17_export_returns_a_borrow.vyrn",
            "an exported function owns its result",
            "`s` may not be returned from an exported function — it is a `read` parameter, and \
             the JS caller releases what it is handed",
            Kernel::Its,
        ),
        row(
            "r18_return_a_read_parameter.vyrn",
            "rule 2 at the return: a whole `read` parameter",
            "`ys` may not be returned — it is a `read` parameter, and a return is owned",
            Kernel::Its,
        ),
        row(
            "r19_read_parameter_wrapped_in_the_result.vyrn",
            "rule 2 through a wrapper: a `read` parameter put into the result",
            "`s` may not be put into `Some(..)` — it is a `read` parameter",
            Kernel::Its,
        ),
        row(
            "r20_drop_after_consume.vyrn",
            "rule 1 at the drop: what a `consume` parameter took is gone",
            "`a` is dropped here but was already consumed by `take(..)` on line 6",
            Kernel::Its,
        ),
        row(
            "r21_drop_a_borrow.vyrn",
            "rule 4 at the drop: the place that owns a value releases it",
            "`owned` may not be dropped — it is read out of a place that owns it",
            Kernel::Its,
        ),
        row(
            "r22_drop_with_a_hole.vyrn",
            "`drop` releases the whole binding, and a take left a hole",
            "`p` may not be dropped — `p.name` was taken out of it on line 17, and `drop` \
             releases the whole binding",
            Kernel::Its,
        ),
        row(
            "r23_modify_is_exclusive.vyrn",
            "a `modify` borrow is exclusive",
            "`a` is passed to `bump` as `modify` and read again in the same call — a `modify` \
             borrow is exclusive",
            // The checker states it in `check_modify_arg`.
            Kernel::Elsewhere,
        ),
        row(
            "r24_capture_that_outlives_the_call.vyrn",
            "a closure that outlives the call may not capture a borrow",
            // The kernel reads which captures a lambda body reads from
            // `NameInfo::closure_reads`.
            "`s` may not be captured by a closure that outlives this call — it is a `read` \
             parameter",
            Kernel::Its,
        ),
        row(
            "r25_consume_inside_a_loop.vyrn",
            "rule 1 across a back edge",
            "`x` is consumed by `take(..)` inside a loop, so it would be used again on the next \
             iteration",
            Kernel::Its,
        ),
        row(
            "r26_rebuild_a_borrowed_receiver.vyrn",
            "a rebuilding builtin takes its receiver",
            "`mt` is read out of `h.meta` here — a place that owns it\nline 7: ... and \
             `push(..)` takes `mt`, so `mt` must be a value of its own",
            Kernel::Its,
        ),
        row(
            "r27_borrow_to_a_builtin_consume.vyrn",
            "rule 2: a `read` parameter to a builtin that declares `consume`",
            "`xs` may not be stored into `fromArray(..)` — it is a `read` parameter",
            Kernel::Its,
        ),
        row(
            "r28_return_a_capture_from_a_closure.vyrn",
            "a closure's result is its caller's, and a capture is not its to give",
            "`s` may not be returned from a closure — it is a captured binding, and the \
             closure's result is its caller's",
            Kernel::Its,
        ),
        row(
            "r29_for_in_consume_module_state.vyrn",
            "module state may not be taken: `for .. in consume`",
            "module state `names` may not be consumed by the `for .. in consume` loop — nothing \
             may take ownership of module state (it lives for the whole module and is never \
             dropped)",
            Kernel::Its,
        ),
        row(
            "r30_stream_never_disposed.vyrn",
            "a must-use obligation is discharged on every path",
            "`s` is a `Stream` and is never disposed",
            Kernel::Elsewhere,
        ),
        row(
            "r31_stream_disposed_twice.vyrn",
            "a must-use obligation is discharged exactly once",
            "`s` is a `Stream` and is disposed more than once",
            Kernel::Elsewhere,
        ),
        row(
            "r32_region_store_escapes.vyrn",
            "a value the region allocated may not be stored where it outlives the region",
            "cannot store a heap value into `kept`, which outlives the enclosing `region` (it \
             would dangle when the region frees). Move `kept` inside the region, or compute a \
             non-heap result to carry out.",
            Kernel::Elsewhere,
        ),
        row(
            "r33_region_consume_escapes.vyrn",
            "a `consume` parameter may not take a value the region frees",
            "cannot hand a heap value to argument 1 of `take`, which is `consume`, inside a \
             `region`. The region frees the value at its closing brace, so the callee cannot \
             own it. Move the call out of the region, or pass a value that holds no heap.",
            Kernel::Elsewhere,
        ),
        row(
            "r34_read_parameter_into_a_builtin_consume_slot.vyrn",
            "rule 2: a `read` parameter into a builtin's `consume` argument",
            "`s` may not be stored into `push(..)` — it is a `read` parameter",
            Kernel::Its,
        ),
        row(
            "r35_write_into_a_second_name_for_a_read_parameter.vyrn",
            "rule 2: a store into a part of a second name for a parameter",
            "`c` may not be written through the field `name` — it is a second name for the \
             `read` parameter `r`",
            Kernel::Its,
        ),
        row(
            "r36_modify_a_second_name_for_a_read_parameter.vyrn",
            "rule 2: a second name for a parameter into a `modify` argument",
            "`c` may not be passed to a `modify` parameter via `lengthen(..)` — it is a second \
             name for the `read` parameter `r`",
            Kernel::Its,
        ),
        row(
            "r37_join_of_a_borrow_then_an_owned_if_arm.vyrn",
            "rule 2: a join an arm owns may not hold another arm's borrow (#518)",
            "`xs[0]` may not be stored into a store — it is a `read` parameter",
            Kernel::Its,
        ),
        row(
            "r38_join_of_an_owned_then_a_borrowed_if_arm.vyrn",
            "rule 2: the same, with the owned arm first (#518)",
            "`xs[0]` may not be stored into a store — it is a `read` parameter",
            Kernel::Its,
        ),
        row(
            "r39_join_of_a_borrowed_then_an_owned_match_arm.vyrn",
            "rule 2: a match join an arm owns may not hold a borrowed payload binder (#518)",
            "`s` may not be stored into a store — it is a second name for the `read` parameter `o`",
            Kernel::Its,
        ),
        row(
            "r40_join_of_an_owned_then_a_borrowed_match_arm.vyrn",
            "rule 2: the same, with the owned arm first (#518)",
            "`s` may not be stored into a store — it is a second name for the `read` parameter `o`",
            Kernel::Its,
        ),
        row(
            "r41_read_parameter_of_a_type_parameter_into_consume_self.vyrn",
            "rule 2: a generic as written judges `T` as owning heap",
            "`x` may not be passed to a `consume` parameter via `eat(..)` — it is a `read` parameter",
            Kernel::Its,
        ),
        row(
            "r42_fn_value_used_after_a_move_into_a_literal.vyrn",
            "rule 1: a literal part takes a `fn` value, which can own its captures",
            "`f` was moved here into a literal\nline 7: ... and `f` is used again here",
            Kernel::Its,
        ),
    ]
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/refusals")
}

fn unlicensed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/unlicensed")
}

/// The command's standard error without the `fix:` and `note:` menu lines.
fn refusal(file: &str, no_kernel: bool) -> (bool, String) {
    refusal_in(dir(), file, no_kernel)
}

/// The command's whole standard error, menu included.
fn whole_refusal_in(dir: PathBuf, file: &str, no_kernel: bool) -> (bool, String) {
    let mut cmd = vyrn();
    cmd.arg("check").arg(dir.join(file));
    if no_kernel {
        cmd.env("VYRN_NO_KERNEL", "1");
    }
    let out = cmd.output().expect("vyrn check");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
}

fn refusal_in(dir: PathBuf, file: &str, no_kernel: bool) -> (bool, String) {
    let path = dir.join(file);
    let mut cmd = vyrn();
    cmd.arg("check").arg(&path);
    if no_kernel {
        cmd.env("VYRN_NO_KERNEL", "1");
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
        // A program may draw a second diagnostic; the row is about the first.
        let first = msg
            .split('\n')
            .take(r.says.lines().count())
            .collect::<Vec<_>>()
            .join("\n");
        if first != r.says {
            bad.push(format!(
                "{} ({}): the checker said\n    {}\n  and the table says\n    {}",
                r.file,
                r.rule,
                first.replace('\n', "\n    "),
                r.says.replace('\n', "\n    ")
            ));
            continue;
        }
        let (kok, ktext) = refusal(r.file, true);
        match &r.kernel {
            Kernel::Elsewhere => {
                if kok || ktext != text {
                    bad.push(format!(
                        "{}: the table says another pass gives this, so standing the kernel \
                         aside may not change it; the two runs said\n    {}\n  and\n    {}",
                        r.file,
                        text.replace('\n', "\n    "),
                        ktext.replace('\n', "\n    ")
                    ));
                }
            }
            Kernel::Its => {
                if !kok {
                    bad.push(format!(
                        "{}: the table says the sentence at {head} is the kernel's, so \
                         standing it aside must accept the program; it said\n    {}",
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

/// A second program for a census row's rule. The census's kernel column
/// measures one program per rule, not the rule, so these pin the shapes that
/// program misses.
///
/// `why` names the class: `heap` is a value that owns no heap, which ownership
/// rules still cover; `spelling` is a borrow spelled in a form the row's
/// program does not use (a prefix `consume` of a `read` parameter's field, a
/// match arm's payload binder, a `for .. in consume` over no place).
struct Uncovered {
    file: &'static str,
    row: &'static str,
    says: &'static str,
    why: &'static str,
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
    }
}

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

/// Each is refused in the table's words, and by the kernel: `VYRN_NO_KERNEL=1`
/// accepts it.
#[test]
fn the_licence_is_per_program_and_these_are_the_counterexamples() {
    let mut bad: Vec<String> = Vec::new();
    for u in counterexamples() {
        let (ok, text) = refusal_in(unlicensed_dir(), u.file, false);
        if ok {
            bad.push(format!("{}: it is accepted", u.file));
            continue;
        }
        let (_, msg) = split_head(&text);
        let first = msg.lines().next().unwrap_or_default();
        if first != u.says {
            bad.push(format!(
                "{} (row {}): it said `{first}` and the table says `{}`",
                u.file, u.row, u.says
            ));
            continue;
        }
        let (kok, _) = refusal_in(unlicensed_dir(), u.file, true);
        if !kok {
            bad.push(format!(
                "{} (row {}, found by {}): the sentence is supposed to be the kernel's, and \
                 standing the kernel aside still refused it",
                u.file, u.row, u.why
            ));
        }
    }
    assert!(bad.is_empty(), "the licence has moved: {}", bad.join("; "));
}

/// Rule 1 (census row 06) around shapes other than the rule: a region's or a
/// lambda block's shadowing `let`, a `break` path, a method's `consume`
/// parameter, a `test` body, a `drop`. Asked of the whole compiler, because
/// `vyrn_frontend::check` alone does not state the rule.
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

/// "A store needs `mut`", asked of the whole compiler: the typed judgment
/// states it, not `vyrn_frontend::check`.
#[test]
fn the_checkers_mut_unit_tests_are_still_refused() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "an assign",
            "cannot assign to `x`",
            "fn main() -> Int64 { let x = 1 x = 2 return x }",
        ),
        (
            "a field store",
            "cannot mutate a field of `p`",
            "type P = { x: Int64 } fn main() -> Int64 { let p = P { x: 1 } p.x = 2 return p.x }",
        ),
        (
            "an index store",
            "cannot store into `a`",
            "fn main() -> Int64 { let a: Array<Int64> = [1, 2] a[0] = 9 return 0 }",
        ),
        (
            "module state",
            "cannot assign to `banner`",
            "let banner = \"hi\"
             fn f() -> Int64 { banner = \"bye\" return 0 }
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a local shadowing module state",
            "cannot assign to `hits`",
            "let mut hits = 0
             fn f() -> Int64 { let hits = 1 hits = 2 return hits }
             fn main() -> Int64 { return 0 }",
        ),
        (
            "an element field store",
            "cannot store into `a`",
            "type P = { x: Int64 }
             fn main() -> Int64 { let a: Array<P> = [P { x: 1 }]  a[0].x = 9  return 0 }",
        ),
        (
            "a projected element store",
            "cannot store into `b`",
            "type P = { x: Int64 } type Bag = { items: Array<P> }
             impl Index for Bag {
                 fn at(read self, i: Int64) -> read P { return self.items[i] }
                 fn atSet(modify self, i: Int64) -> modify P { return self.items[i] }
             }
             fn main() -> Int64 { let b = Bag { items: [P { x: 1 }] }  b[0] = P { x: 2 }  return 0 }",
        ),
        (
            "a projected element field store",
            "cannot store into `b`",
            "type P = { x: Int64 } type Bag = { items: Array<P> }
             impl Index for Bag {
                 fn at(read self, i: Int64) -> read P { return self.items[i] }
                 fn atSet(modify self, i: Int64) -> modify P { return self.items[i] }
             }
             fn main() -> Int64 { let b = Bag { items: [P { x: 1 }] }  b[0].x = 9  return 0 }",
        ),
    ];
    let dir = common::scratch("mut-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(&format!("{says} (declared without `mut`)")) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a store needs `mut` no longer refuses:
  {}",
        bad.join(
            "
  "
        )
    );
}

/// `break` and `continue` outside a loop, asked of the whole compiler: the typed
/// judgment states the rule.
#[test]
fn the_checkers_loop_unit_test_is_still_refused() {
    let cases: &[(&str, &str, &str)] = &[
        ("a break", "`break` outside a loop", "fn main() -> Int64 { break return 0 }"),
        (
            "a continue",
            "`continue` outside a loop",
            "fn main() -> Int64 { continue return 0 }",
        ),
        (
            "a break in a lambda in a loop",
            "`break` outside a loop",
            "fn main() -> Int64 { while true {              let f: fn(Int64) -> Unit = x -> { break } } return 0 }",
        ),
    ];
    let dir = common::scratch("loop-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(says) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a loop exit outside a loop no longer refuses:
  {}",
        bad.join(
            "
  "
        )
    );
}

/// A literal's constant rules, asked of the whole compiler: the typed judgment
/// states them.
#[test]
fn the_checkers_literal_unit_tests_are_still_refused() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "a sized slot",
            ":3:0: integer literal 300 does not fit Int8 (its range is -128..=127)",
            "fn main() -> Int64 {\n    let q = 0\n    let x: Int8 = 300\n    return 0\n}",
        ),
        (
            "a sized sibling",
            "integer literal 300 does not fit UInt8",
            "fn main() -> Int64 { let x: UInt8 = 200 if x < 300 { return 1 } return 0 }",
        ),
        (
            "a negative sibling past the minimum",
            "integer literal -2147483649 does not fit Int32",
            "fn main() -> Int64 { let mut x: Int32 = 0 if x == -2147483649 { x = 1 } return 0 }",
        ),
        (
            "a negated slot past the minimum",
            "integer literal 2147483649 does not fit Int32",
            "fn main() -> Int64 { let a: Int32 = -2147483649 return 0 }",
        ),
        (
            "a literal pattern",
            "the right side of `=~` must be a string-literal pattern",
            "fn f(s: String, p: String) -> Bool { return s =~ p } fn main() -> Int64 { return 0 }",
        ),
        (
            "a reversed class",
            "invalid regex `[z-a]`",
            "fn f(s: String) -> Bool { return s =~ \"[z-a]\" } fn main() -> Int64 { return 0 }",
        ),
        (
            "a SmallArray slot",
            "this literal has 3 elements but the slot is SmallArray<_, 2>",
            "fn main() -> Int64 { let xs: SmallArray<Int64, 2> = [1, 2, 3] return xs.length }",
        ),
    ];
    let dir = common::scratch("literal-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(says) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a literal rule no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// A non-exhaustive `match`, asked of the whole compiler: the core's switch
/// states the rule.
#[test]
fn the_checkers_match_unit_tests_are_still_refused() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "an option",
            "missing variant `None`",
            "fn main() -> Int64 { let o: Option<Int64> = Some(1) return match o { Some(x) => x } }",
        ),
        (
            "an enum",
            "missing variant `B`",
            "type E = | A | B fn f(e: E) -> Int64 { return match e { A => 1 } }              fn main() -> Int64 { return f(A) }",
        ),
    ];
    let dir = common::scratch("match-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(says) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a non-exhaustive match no longer refuses:
  {}",
        bad.join(
            "
  "
        )
    );
}

/// A checker rule in a block lambda of a module-state initializer is refused
/// in its own sentence, and not as the core's internal error (#554).
#[test]
fn a_block_lambda_in_module_state_is_checked() {
    let src = "type Op = fn(Int64) -> Int64
               let ops: Array<Op> = [
               x -> {
               let a = 1 + \"s\"
               return x
               },
               ]
               fn main() -> Int64 {
               return ops.length
               }
";
    let dir = common::scratch("state-lambda");
    std::fs::write(dir.join("state.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "state.vyrn", false);
    assert!(!ok, "accepted");
    assert!(
        text.contains("state.vyrn:4:0: `+` concatenates two Strings, found Int64 and String")
            && !text.contains("internal error"),
        "{text}"
    );
}

/// A `fn` argument that does not fit its slot, asked of the whole compiler: the
/// core states the rule.
#[test]
fn the_checkers_fn_slot_unit_tests_are_still_refused() {
    let twice = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
                 let mut out: Array<Int64> = []\n\
                 for x in xs { out.push(f(x)) }\n\
                 return out }\n";
    let cases: &[(&str, &str, String)] = &[
        (
            "an arity",
            "this lambda takes 2 parameter(s)",
            format!(
                "{twice}fn main() -> Int64 {{ let a = twice([1], (x, y) -> x + y)  return 0 }}"
            ),
        ),
        (
            "a return type",
            "this lambda returns Bool",
            format!("{twice}fn main() -> Int64 {{ let a = twice([1], x -> x > 0)  return 0 }}"),
        ),
        (
            "a field",
            "an expression of `fn` type; found Int64",
            format!(
                "{twice}type B = {{ n: Int64 }}\n\
                 fn main() -> Int64 {{ let b = B {{ n: 1 }}  let a = twice([1], b.n)  return 0 }}"
            ),
        ),
        (
            "a rigid parameter",
            "`widthOf` expects a String argument, but `fn(T) -> Int64` will pass it T",
            "type G<T> = { width: fn(T) -> Int64 }
             fn widthOf(s: String) -> Int64 { return 1 }
             fn make<T>() -> G<T> { return G { width: widthOf } }
             fn main() -> Int64 { let g: G<String> = make()
 return 0 }"
                .to_string(),
        ),
        (
            "a narrower record",
            "`h` expects a Q argument, but `apply` will pass it P",
            "type P = { x: Int64 } type Q = { x: Int64, y: Int64 } \
             fn g(v: Q) -> Int64 { return v.y } \
             fn apply(f: fn(P) -> Int64, p: P) -> Int64 { return f(p) } \
             fn main() -> Int64 { let h: fn(Q) -> Int64 = g return apply(h, P { x: 1 }) }"
                .to_string(),
        ),
    ];
    let dir = common::scratch("fn-slot-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(says) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a fn argument that does not fit its slot no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// The `drop` rules, asked of the whole compiler: the typed judgment states
/// them, and reads a generic body with its parameters as written.
#[test]
fn the_checkers_drop_unit_test_is_still_refused() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "module state",
            "cannot `drop` module state `s`",
            "let s = \"hi\"
             fn f() -> Int64 { drop s return 0 }
             fn main() -> Int64 { return 0 }",
        ),
        (
            "a called generic",
            "cannot `drop` `v`: its type `T` is a type parameter",
            "fn give<T>(v: consume T) -> Int64 { drop v return 0 }
             fn main() -> Int64 { return give(\"a\" + \"b\") }",
        ),
    ];
    let dir = common::scratch("drop-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(says) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a `drop` rule no longer refuses:
  {}",
        bad.join(
            "
  "
        )
    );
}

/// A removal's receiver type, asked of the whole compiler: the typed judgment
/// states it.
#[test]
fn the_checkers_shrink_unit_test_is_still_refused() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "pop",
            "`pop` is not available on a fixed-size array",
            "fn main() -> Int64 { let mut a: Array<Int64, 3> = [1, 2, 3] let p = a.pop() return 0 }",
        ),
        (
            "swapRemove",
            "`swapRemove` is not available on a fixed-size array",
            "fn main() -> Int64 { let mut a: Array<Int64, 3> = [1, 2, 3] let g = a.swapRemove(0) return g }",
        ),
        (
            "a String",
            "`pop` needs an `Array<T>`, found String",
            "fn main() -> Int64 { let mut s = \"abc\" let p = s.pop() return 0 }",
        ),
    ];
    let dir = common::scratch("shrink-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(says) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a removal's receiver rule no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// A declared call's arity, type-argument count and argument types, asked of the
/// whole compiler: the typed judgment states them. Both counts start after the
/// dot where the call writes one (#577), and a wrong argument reads the same
/// through dispatch, a bound or by name.
#[test]
fn the_checkers_call_unit_tests_are_still_refused() {
    const P: &str = "protocol P { fn m(self, k: Int64) -> Int64 }
         impl P for Int64 { fn m(self, k: Int64) -> Int64 { return self + k } }";
    let cases: &[(&str, &str, String)] = &[
        (
            "readFile",
            "`readFile` argument 1 expects String",
            "fn main() -> Int64 { let r = readFile(5) return 0 }".into(),
        ),
        (
            "writeFile",
            "`writeFile` expects 2 argument(s)",
            "fn main() -> Int64 { let r = writeFile(\"p\") return 0 }".into(),
        ),
        (
            "args",
            "`args` expects 0 argument(s)",
            "fn main() -> Int64 { let a = args(1) return 0 }".into(),
        ),
        (
            "readLine",
            "`readLine` expects 0 argument(s)",
            "fn main() -> Int64 { let l = readLine(\"x\") return 0 }".into(),
        ),
        (
            "stringFromBytes",
            "`stringFromBytes` argument 1 expects Array<UInt8>",
            "fn main() -> Int64 { let s = stringFromBytes(\"x\") return 0 }".into(),
        ),
        (
            "floatBits",
            "`floatBits` argument 1 expects Float64",
            "fn main() -> Int64 { let b = floatBits(7) return 0 }".into(),
        ),
        (
            "floatFromBits",
            "`floatFromBits` argument 1 expects UInt64",
            "fn main() -> Int64 { let f = floatFromBits(1.5) return 0 }".into(),
        ),
        (
            "renameFile",
            "`renameFile` expects 2 argument(s)",
            "fn main() -> Int64 { let r = renameFile(\"a\") return 0 }".into(),
        ),
        (
            "fsyncFile",
            "`fsyncFile` argument 1 expects String",
            "fn main() -> Int64 { let r = fsyncFile(1) return 0 }".into(),
        ),
        (
            "a method builtin's arity",
            "`keys` expects 0 argument(s), got 1",
            "fn main() -> Int64 { let m: Map<String, Int64> = [:] let k = m.keys(1) return 0 }"
                .into(),
        ),
        (
            "assert",
            "`assert` argument 1 expects Bool, found Int64",
            "fn main() -> Int64 { return 0 } test \"t\" { assert(5) }".into(),
        ),
        (
            "lineAt of Int64s",
            "`lineAt` argument 1 expects Array<UInt8>, found Array<Int64>",
            "fn main() -> Int64 { let mut a: Array<Int64> = []  a.push(1)  a.push(10)
             print(lineAt(a, 2))  return 0 }"
                .into(),
        ),
        (
            "colAt of Int64s",
            "`colAt` argument 1 expects Array<UInt8>, found Array<Int64>",
            "fn main() -> Int64 { let mut a: Array<Int64> = []  a.push(1)  a.push(10)
             print(colAt(a, 2))  return 0 }"
                .into(),
        ),
        (
            "lineAt of a String",
            "`lineAt` argument 1 expects Array<UInt8>, found String",
            "fn main() -> Int64 { print(lineAt(\"ab\", 1))  return 0 }".into(),
        ),
        // A `SmallArray` is not the `{ ptr, i64, i64 }` an `Array` is.
        (
            "lineAt of a SmallArray",
            "`lineAt` argument 1 expects Array<UInt8>",
            "fn main() -> Int64 { let mut a: SmallArray<UInt8, 4> = []  a.push('x')
             print(lineAt(a, 0))  return 0 }"
                .into(),
        ),
        (
            "a narrow record",
            "`f` argument 1 expects User, found Named",
            "type Named = { name: Int64 } type User = { name: Int64, age: Int64 }
             fn f(u: User) -> Int64 { return u.age }
             fn main() -> Int64 { let n = Named { name: 1 } return f(n) }"
                .into(),
        ),
        (
            "a Pick",
            "`f` argument 1 expects User, found Id",
            "type User = { id: Int64, name: Int64 } type Id = Pick<User, id>
             fn f(u: User) -> Int64 { return u.id }
             fn main() -> Int64 { return f(Id { id: 1 }) }"
                .into(),
        ),
        (
            "a bound's arity",
            "`m` expects 1 argument(s), got 3",
            format!(
                "{P} fn go<T: P>(x: T) -> Int64 {{ return x.m(1, 2, 3) }}
                 fn main() -> Int64 {{ return go(4) }}"
            ),
        ),
        (
            "through an impl",
            "`m` argument 1 expects Int64, found String",
            format!("{P} fn main() -> Int64 {{ return 7.m(\"x\") }}"),
        ),
        (
            "through a bound",
            "`m` argument 1 expects Int64, found String",
            format!(
                "{P} fn go<T: P>(x: T) -> Int64 {{ return x.m(\"x\") }}
                 fn main() -> Int64 {{ return go(7) }}"
            ),
        ),
        (
            "dispatched without a dot",
            "`m` argument 2 expects Int64, found String",
            format!("{P} fn main() -> Int64 {{ return m(7, \"x\") }}"),
        ),
        (
            "the arity without a dot",
            "`m` expects 2 argument(s), got 3",
            format!("{P} fn main() -> Int64 {{ return m(7, 1, 2) }}"),
        ),
        (
            "by name",
            "`m` argument 2 expects Int64, found String",
            "fn m(x: Int64, k: Int64) -> Int64 { return x + k }
             fn main() -> Int64 { return m(7, \"x\") }"
                .into(),
        ),
    ];
    let dir = common::scratch("call-rule");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(says) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a declared call's rule no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// Rule 1's move (census row 07) around shapes other than the rule: a builtin's
/// sink, a record literal, a `for .. in consume`, a prefix take, a take on one
/// arm, a stream producer. The kernel states it, so these are asked of the
/// whole compiler.
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

/// Rule 2 (census rows 01, 02, 03, 27, 34): a borrow may not be put anywhere
/// that outlives the call. The destination shapes the sentence, so each case
/// names a destination. The needles carry the `fix:` lines and read the whole
/// standard error, because the menu is part of what the reader gets.
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
            // menu does not offer `consume`.
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
            // The container still owns an element read, so the loop borrows.
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
        // The kernel refuses it, so standing the kernel aside accepts it.
        let (kok, _) = whole_refusal_in(dir.to_path_buf(), &name, true);
        if !kok {
            bad.push(format!("{what}: it is not the kernel that refuses it"));
        }
    }
    // The ways out the menus offer compile.
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

/// Census row 19 is a whole `read` parameter; this is the other shape, a loop
/// variable put into `Some(..)` under a `return`. The refusal names the
/// constructor, not the `return` that wraps it.
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

/// Rule 3 (census row 17): a return is owned. The kernel states it at one exit,
/// because the core carries the exit into `if` and `match` arms. The needle is
/// the whole first line, since the wording is what is pinned.
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
        // The three spellings an export refuses. None may offer
        // ``declare the parameter `q: consume ..` ``, which the compiler refuses
        // at an export's signature.
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
        // Standing the kernel aside accepts it, so the sentence is the kernel's.
        let (kok, _) = refusal_in(dir.to_path_buf(), &name, true);
        if !kok {
            bad.push(format!("{what}: it is not the kernel that refuses it"));
        }
    }
    assert!(
        bad.is_empty(),
        "rule 3 no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// Census row 25: a consumption inside a loop would run again next turn. The
/// kernel judges the loop's back edge against its entry, one rule for four
/// shapes: a `while` body, a `while` condition, a body ending in `continue`, and
/// a take of a projection.
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

/// Rule 2 at the third exit (census rows 13, 14): a borrow may not be handed to
/// a declared `consume` parameter. The kernel asks it of the value, so each
/// spelling of the argument is pinned here, with its menu, because the menu is
/// what a reader acts on.
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

/// The prefix `consume` form (census rows 10, 11, 29). The sentence names the
/// taker the value reaches: the `consume` parameter a call hands it to, or the
/// loop that takes the container. `for x in consume r.xs` on a record this frame
/// owns is refused.
#[test]
fn the_shapes_rows_ten_eleven_and_twenty_nines_unit_tests_pinned_are_still_refused() {
    const DECLS: &str = "type Bag = { a: String } \
                         type R = { xs: Array<String> } \
                         let g: String = \"m\" \
                         let gs: Array<String> = [] \
                         let gr: R = R { xs: [] } \
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
        (
            "a prefix take of a field of module state at a call",
            vec![
                "`gr.xs` may not be passed to a `consume` parameter via `take(..)` — it is \
                 module state, which nothing may take",
                "fix: `gr.xs.copy()`",
            ],
            go("", "let n = take(consume gr.xs) o.push(\"x\")"),
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
    // The ways out compile, and a container this frame owns is the loop's to take.
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

/// `let r = R { s: x }` and an inline `R { s: x }` are one literal, and both
/// name the field, not "the literal". The core keeps field names on the
/// temporary an inline literal gets as well as on a `let` binding. No corpus
/// program has a borrow in the inline form, so this is a pin.
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
        // An array has no field names, so none is invented.
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
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok {
            bad.push(format!("{what}: accepted"));
        } else if !text.contains(needle) {
            bad.push(format!("{what}: wanted `{needle}`, got {text}"));
        }
    }
    assert!(
        bad.is_empty(),
        "a literal's part has lost its field:\n  {}",
        bad.join("\n  ")
    );
}

/// A nullary constructor is a value with no owner, not a name.
/// `take(None)` twice hands the callee two values; keying rule 1 on the bare name
/// a nullary variant parses as would report the second as a use of the first.
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
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if !ok {
            bad.push(format!("{what}:\n    {}", text.replace('\n', "\n    ")));
        }
    }
    assert!(
        bad.is_empty(),
        "a nullary constructor is read as a name:\n  {}",
        bad.join("\n  ")
    );
}

/// Programs a checker-only test would read as accepted, asked of the whole
/// compiler: `vyrn-frontend` does not link the kernel, so the checker's silence
/// is not acceptance. A row with no sentence compiles; a row with one is refused
/// with that sentence.
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
            // The core carries the exit into the arm, so the kernel names the
            // loop variable the reader wrote, not a temporary the core minted.
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

/// A borrow wrapped into a constructor or aggregate, and the sentence that
/// refuses it. The kernel refuses every such shape, so no call-graph closure
/// of lending functions or retaining positions is needed. If a row stops being
/// refused, that closure is needed.
struct Wrapped {
    stem: &'static str,
    /// Which call-graph set the shape would seed: lending, retains, or both.
    seeds: &'static str,
    source: &'static str,
    says: &'static str,
}

fn wrapped_lends() -> Vec<Wrapped> {
    vec![
        // `std/html`'s `attrKey`: a payload borrow wrapped into `Some(..)` and
        // stored in a local.
        Wrapped {
            stem: "attrkey",
            seeds: "lending",
            source: "type Attr =\n    | Key(String)\n    | Pair(String, String)\n\n\
                     fn attrKey(xs: Array<Attr>) -> Option<String> {\n\
                     \x20 let mut found: Option<String> = None\n\
                     \x20 for a in xs {\n\
                     \x20   found = match a {\n\
                     \x20     Key(k) => Some(k),\n\
                     \x20     Pair(k, v) => None,\n\
                     \x20   }\n\
                     \x20 }\n\
                     \x20 return found\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let xs: Array<Attr> = [Key(\"a\" + \"b\")]\n\
                     \x20 match attrKey(xs) { Some(s) => print(s), None => print(\"none\") }\n\
                     \x20 return 0\n}\n",
            says: "`k` may not be put into `Some(..)` — it is read out of a place that owns it",
        },
        // The same wrap at the `return` itself, over a loop element.
        Wrapped {
            stem: "firstkey",
            seeds: "lending",
            source: "type Attr = | Key(String) | Pair(String, String)\n\n\
                     fn firstKey(xs: Array<Attr>) -> Option<String> {\n\
                     \x20 for a in xs {\n\
                     \x20   return match a {\n\
                     \x20     Key(k) => Some(k),\n\
                     \x20     Pair(k, v) => None,\n\
                     \x20   }\n\
                     \x20 }\n\
                     \x20 return None\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let xs: Array<Attr> = [Key(\"a\" + \"b\")]\n\
                     \x20 match firstKey(xs) { Some(s) => print(s), None => print(\"n\") }\n\
                     \x20 return 0\n}\n",
            says: "`k` may not be put into `Some(..)` — it is read out of a place that owns it",
        },
        // A `read` parameter's field, wrapped in a constructor at the return:
        // it seeds both sets.
        Wrapped {
            stem: "fieldwrap",
            seeds: "lending and retains",
            source: "type Pack = { j: String }\n\n\
                     fn f1(p: Pack) -> Option<String> {\n\
                     \x20 return Some(p.j)\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let p = Pack { j: \"a\" + \"b\" }\n\
                     \x20 match f1(p) { Some(s) => print(s), None => print(\"n\") }\n\
                     \x20 return 0\n}\n",
            says: "`p.j` may not be put into `Some(..)` — it is a `read` parameter",
        },
        // A struct literal at the return.
        Wrapped {
            stem: "structwrap",
            seeds: "lending and retains",
            source: "type Pack = { j: String }\ntype Wrap = { s: String }\n\n\
                     fn f2(p: Pack) -> Wrap {\n\
                     \x20 return Wrap { s: p.j }\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let p = Pack { j: \"a\" + \"b\" }\n\
                     \x20 let w = f2(p)\n\
                     \x20 print(w.s)\n\
                     \x20 return 0\n}\n",
            says: "`p.j` may not be stored into the field `Wrap.s` — it is a `read` parameter",
        },
        // Through an `if` arm.
        Wrapped {
            stem: "ifwrap",
            seeds: "lending and retains",
            source: "type Pack = { j: String }\n\n\
                     fn f3(p: Pack, c: Bool) -> Option<String> {\n\
                     \x20 return if c { Some(p.j) } else { None }\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let p = Pack { j: \"a\" + \"b\" }\n\
                     \x20 match f3(p, true) { Some(s) => print(s), None => print(\"n\") }\n\
                     \x20 return 0\n}\n",
            says: "`p.j` may not be put into `Some(..)` — it is a `read` parameter",
        },
        // A projection of a locally built record.
        Wrapped {
            stem: "localfield",
            seeds: "lending",
            source: "type Doc = { title: String, body: String }\n\n\
                     fn pick() -> Option<String> {\n\
                     \x20 let d = Doc { title: \"a\" + \"b\", body: \"c\" + \"d\" }\n\
                     \x20 return Some(d.title)\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 match pick() { Some(s) => print(s), None => print(\"n\") }\n\
                     \x20 return 0\n}\n",
            says: "`d.title` may not be put into `Some(..)` — it is read out of a place that owns it",
        },
        // The same, stored into a local aggregate rather than returned.
        Wrapped {
            stem: "localstore",
            seeds: "lending",
            source: "type Doc = { title: String, body: String }\ntype Box = { v: String }\n\n\
                     fn pick() -> Box {\n\
                     \x20 let d = Doc { title: \"a\" + \"b\", body: \"c\" + \"d\" }\n\
                     \x20 let b = Box { v: d.title }\n\
                     \x20 return b\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let b = pick()\n\
                     \x20 print(b.v)\n\
                     \x20 return 0\n}\n",
            says: "`d.title` may not be stored into the field `Box.v` — it is read out of a place that owns it",
        },
        // A payload binder handed straight back into its own constructor.
        Wrapped {
            stem: "rewrap",
            seeds: "lending",
            source: "type Bag = | One(Array<String>) | Nil\n\n\
                     fn take(b: Bag) -> Bag {\n\
                     \x20 return match b {\n\
                     \x20   One(xs) => One(xs),\n\
                     \x20   Nil => Nil,\n\
                     \x20 }\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let b = One([\"a\" + \"b\"])\n\
                     \x20 match take(b) { One(xs) => print(\"\\{xs.length}\"), Nil => print(\"n\") }\n\
                     \x20 return 0\n}\n",
            says: "`xs` may not be put into `One(..)` — it is a second name for the `read` parameter `b`",
        },
        // An array literal.
        Wrapped {
            stem: "arraywrap",
            seeds: "retains",
            source: "type Pack = { j: String }\n\n\
                     fn wrap(p: Pack) -> Array<String> {\n\
                     \x20 return [p.j]\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let p = Pack { j: \"a\" + \"b\" }\n\
                     \x20 let a = wrap(p)\n\
                     \x20 print(\"\\{a.length}\")\n\
                     \x20 return 0\n}\n",
            says: "`p.j` may not be stored into the literal — it is a `read` parameter",
        },
        // A doubly wrapped borrow is refused at the inner constructor.
        Wrapped {
            stem: "nestwrap",
            seeds: "lending and retains",
            source: "type Pack = { j: String }

                     fn f4(p: Pack) -> Option<Option<String>> {
                       return Some(Some(p.j))
}

                     fn main() -> Int64 {
                       let p = Pack { j: \"a\" + \"b\" }
                       match f4(p) { Some(o) => print(\"s\"), None => print(\"n\") }
                       return 0
}
",
            says: "`p.j` may not be put into `Some(..)` — it is a `read` parameter",
        },
        // A generic container's field, whose type is a parameter.
        Wrapped {
            stem: "genericfield",
            seeds: "neither, and it is refused all the same",
            source: "type Cell<T> = { v: T }\n\n\
                     fn get<T>(c: Cell<T>) -> Option<T> {\n\
                     \x20 return Some(c.v)\n}\n\n\
                     fn main() -> Int64 {\n\
                     \x20 let c = Cell { v: \"a\" + \"b\" }\n\
                     \x20 match get(c) { Some(s) => print(s), None => print(\"n\") }\n\
                     \x20 return 0\n}\n",
            says: "`c.v` may not be put into `Some(..)` — it is a `read` parameter",
        },
    ]
}

#[test]
fn a_lend_through_a_wrapper_is_refused_and_the_kernel_is_what_refuses_it() {
    let root = std::env::temp_dir().join(format!("vyrn-wrapped-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for w in wrapped_lends() {
        let file = format!("{}.vyrn", w.stem);
        std::fs::write(root.join(&file), w.source).unwrap();
        let (ok, err) = refusal_in(root.clone(), &file, false);
        assert!(!ok, "{} ({}) is accepted:\n{err}", w.stem, w.seeds);
        assert_eq!(split_head(&err).1, w.says, "{} ({})", w.stem, w.seeds);
        let (ok, _) = refusal_in(root.clone(), &file, true);
        assert!(
            ok,
            "{} ({}) is refused with the kernel aside, so the door is not the kernel's",
            w.stem, w.seeds
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Census rows 22, 23 and 24, as shapes: a drop after a partial take, a
/// `modify` borrow read again in the same call, a stored closure's capture.
/// Each runs twice, as a census row does.
#[test]
fn the_shapes_the_last_three_rules_unit_tests_pinned_are_still_refused() {
    const END: &str = " fn main() -> Int64 { return 0 }";
    // The flag is who states it: `true` for the kernel, `false` for the checker.
    let cases: &[(&str, &str, bool, String)] = &[
        (
            "a drop after a partial take",
            "`t` may not be dropped — `t.name` was taken out of it on line 1, and `drop` \
             releases the whole binding",
            true,
            format!(
                "type T = {{ id: Int64, name: String }} \
                 impl Owned for T {{ fn release(consume self) \
                 {{ let a = consume self.name drop a }} }} \
                 fn go() -> Int64 {{ let t = T {{ id: 1, name: \"n\" }} \
                 let n = consume t.name drop n drop t return 0 }}{END}"
            ),
        ),
        (
            "a modify borrow read again in the same call",
            "`xs` is passed to `f` as `modify` and read again in the same call — a `modify` \
             borrow is exclusive",
            false,
            format!(
                "fn f(a: modify Array<Int64>, b: Array<Int64>) -> Int64 {{ return a.length }} \
                 fn go() -> Int64 {{ let mut xs: Array<Int64> = [] return f(xs, xs) }}{END}"
            ),
        ),
        (
            "a modify receiver read again in the same call",
            "`t` is passed to `merge` as `modify` and read again in the same call — a `modify` \
             borrow is exclusive",
            false,
            format!(
                "type T = {{ n: Int64 }} \
                 protocol Merging {{ fn merge(modify self, other: T) -> Unit }} \
                 impl Merging for T {{ fn merge(modify self, other: T) -> Unit \
                 {{ self.n = self.n + other.n }} }} \
                 fn go() -> Int64 {{ let mut t = T {{ n: 1 }} t.merge(t) return 0 }}{END}"
            ),
        ),
        (
            "a stored closure captures a borrow",
            "`s` may not be captured by a closure that outlives this call — it is a `read` \
             parameter",
            true,
            format!(
                "fn go(s: String) -> Int64 \
                 {{ let f: fn(Int64) -> Int64 = n -> n + s.byteLength \
                 return f(1) }}{END}"
            ),
        ),
        (
            "a closure at a consume fn parameter captures a borrow",
            "`q` may not be captured by a closure that outlives this call — it is a `read` \
             parameter",
            true,
            format!(
                "fn reg(f: consume fn(Int64) -> Int64) -> Int64 {{ return f(0) }} \
                 fn go(q: read String) -> Int64 {{ return reg(n -> n + q.byteLength) }}{END}"
            ),
        ),
    ];
    let dir = common::scratch("last-three-shapes");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, by_kernel, src) in cases {
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
        // With the kernel aside, its rows are accepted and the checker's rows
        // say the same sentence.
        let (kok, ktext) = refusal_in(dir.to_path_buf(), &name, true);
        if *by_kernel && !kok {
            bad.push(format!("{what}: it is not the kernel that refuses it"));
        }
        if !*by_kernel && (kok || ktext != text) {
            bad.push(format!("{what}: the checker's sentence did not survive"));
        }
    }
    assert!(
        bad.is_empty(),
        "a rule the last three shapes pin no longer refuses:\n  {}",
        bad.join("\n  ")
    );
}

/// A block-bodied lambda captures its body's own mentions minus the names the
/// body binds (F2-051). A capture is a read, so capturing every name in scope
/// would refuse a body that reads nothing, refuse a shadow, and add a second
/// diagnostic above the one real use.
#[test]
fn a_block_bodied_lambda_captures_only_what_its_body_reads() {
    const HEAD: &str = "type T = { id: Int64 }\n\
                        fn take(t: consume T) -> Int64 { return t.id }\n";
    let dir = common::scratch("lambda-captures");
    let mut bad: Vec<String> = Vec::new();

    // Accepted: neither lambda body names `s`. The shadow's `s` is the
    // lambda's own binding.
    for (what, tail) in [
        (
            "reads nothing",
            "  let g: fn(Int64) -> Int64 = x -> { return x + 1 }\n",
        ),
        (
            "shadows the name",
            "  let g: fn(Int64) -> Int64 = x -> { let s = 0 return x + s }\n",
        ),
    ] {
        let src = format!("{HEAD}fn main() -> Int64 {{\n  let s = T {{ id: 1 }}\n  let n = take(s)\n{tail}  return g(n)\n}}\n");
        let name = format!("accepted_{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if !ok {
            bad.push(format!("a lambda that {what} was refused: {text}"));
        }
    }

    // Refused once: the shadowing lambda stands, and `take(s)` on line 7 is
    // the only use of the consumed `s`.
    let src = format!(
        "{HEAD}fn main() -> Int64 {{\n  let s = T {{ id: 1 }}\n  let n = take(s)\n  \
         let g: fn(Int64) -> Int64 = x -> {{ let s = 0 return x + s }}\n  \
         return g(n) + take(s)\n}}\n"
    );
    std::fs::write(dir.join("refused.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "refused.vyrn", false);
    let heads: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("already consumed by"))
        .collect();
    if ok || heads.len() != 1 || !heads[0].contains("refused.vyrn:7:") {
        bad.push(format!(
            "the one use of `s` is not the one diagnostic: {text}"
        ));
    }

    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A `for` reads its container through a borrow from its head to its end, so a
/// store over the container inside the body is refused. Accepted, the store
/// would free the buffer the loop still reads. A `modify` argument over the
/// container ends the borrow too. `run` and `build` must both refuse
/// before running. A `consume` of the container is the loop rule's.
#[test]
fn a_store_over_the_container_a_for_walks_is_refused() {
    let cases: &[(&str, &str)] = &[
        (
            "a store over the name",
            "fn main() -> Int64 {\n  let mut ys: Array<Int64> = [1, 2, 3]\n  let mut s = 0\n  \
             for y in ys {\n    ys = []\n    let zs: Array<Int64> = [100, 200, 300]\n    \
             s = s + y + zs.length - 3\n  }\n  print(s.toString())\n  return 0\n}\n",
        ),
        (
            "a push onto the name",
            "fn main() -> Int64 {\n  let mut ys: Array<Int64> = [1, 2]\n  let mut s = 0\n  \
             for y in ys {\n    ys.push(y)\n    s = s + y\n  }\n  return s\n}\n",
        ),
        (
            "a modify argument of the name",
            "fn grow(xs: modify Array<Int64>) { xs = [9, 9, 9] }\n\
             fn main() -> Int64 {\n  let mut ys: Array<Int64> = [1, 2, 3]\n  let mut s = 0\n  \
             for y in ys {\n    grow(ys)\n    s = s + y\n  }\n  return s\n}\n",
        ),
    ];
    let dir = common::scratch("for-container-store");
    let mut bad: Vec<String> = Vec::new();
    for (what, src) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        let want = "`ys` is written here while `ys` still reads out of it";
        if ok || !text.lines().next().is_some_and(|l| l.ends_with(want)) {
            bad.push(format!("{what}: `check` said {text}"));
        }
        for engine in [&["run"][..], &["build"][..]] {
            let out = vyrn()
                .current_dir(&dir)
                .args(engine)
                .arg(&name)
                .args(if engine[0] == "build" {
                    &["-o", "never"][..]
                } else {
                    &[]
                })
                .output()
                .expect("vyrn");
            let err = String::from_utf8_lossy(&out.stderr);
            if out.status.success() || !out.stdout.is_empty() || !err.contains(want) {
                bad.push(format!("{what}: `{}` ran or said {err}", engine[0]));
            }
        }
    }
    // The way out the menu names: the loop walks a copy of its own.
    let fixed = cases[0].1.replace("for y in ys {", "for y in ys.copy() {");
    std::fs::write(dir.join("copy.vyrn"), fixed).expect("write the program");
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "copy.vyrn"])
        .output()
        .expect("vyrn run");
    if String::from_utf8_lossy(&out.stdout).trim() != "6" {
        bad.push(format!(
            "the copy printed {:?}",
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A take of a name ends every alias that reads out of it, as a drop does.
/// After `let xs = b.items`, a `consume` of `b` or a move into
/// another name lets the new owner free the buffer `xs` still reads.
#[test]
fn a_take_of_the_place_a_read_binding_reads_is_refused() {
    let head = "type Bag = { items: Array<Int64>, n: Int64 }\n\
                fn eat(b: consume Bag) -> Int64 { return b.n }\n";
    let cases: &[(&str, &str)] = &[
        ("a consume argument", "let k = eat(b)\n  print(xs[0] + k)"),
        ("a move", "let c = b\n  print(xs[0] + c.n)"),
    ];
    let dir = common::scratch("alias-take");
    let want = "`b` is written here while `xs` still reads out of it";
    let mut bad: Vec<String> = Vec::new();
    for (what, tail) in cases {
        let src = format!(
            "{head}fn main() -> Int64 {{\n  let b = Bag {{ items: [1, 2, 3], n: 3 }}\n  \
             let xs = b.items\n  {tail}\n  return 0\n}}\n"
        );
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        std::fs::write(dir.join(&name), &src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.lines().next().is_some_and(|l| l.ends_with(want)) {
            bad.push(format!("{what}: `check` said {text}"));
        }
        let out = vyrn()
            .current_dir(&dir)
            .args(["run", &name])
            .output()
            .expect("vyrn run");
        if out.status.success() || !out.stdout.is_empty() {
            bad.push(format!("{what}: `run` ran"));
        }
    }
    // The way out the menu names: `xs` is a value of its own.
    let fixed = format!(
        "{head}fn main() -> Int64 {{\n  let b = Bag {{ items: [1, 2, 3], n: 3 }}\n  \
         let xs = b.items.copy()\n  let k = eat(b)\n  print(xs[0] + k)\n  return 0\n}}\n"
    );
    std::fs::write(dir.join("copy.vyrn"), fixed).expect("write the program");
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "copy.vyrn"])
        .output()
        .expect("vyrn run");
    if String::from_utf8_lossy(&out.stdout).trim() != "4" {
        bad.push(format!(
            "the copy printed {:?}",
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A `modify` argument ends every borrow that reads what it names,
/// as a `for` does for its container: `reset(b)` may store over the buffer `xs`
/// reads. `run` and `build` must both refuse before running.
#[test]
fn a_modify_of_the_place_a_read_binding_reads_is_refused() {
    let src = "type Bag = { items: Array<Int64>, n: Int64 }\n\
               fn reset(b: modify Bag) { b.items = [9, 9, 9] }\n\
               fn main() -> Int64 {\n  let mut b = Bag { items: [1, 2, 3], n: 3 }\n  \
               let xs = b.items\n  reset(b)\n  print(xs[0])\n  return 0\n}\n";
    let dir = common::scratch("alias-modify");
    let want = "`b` is written here while `xs` still reads out of it";
    let mut bad: Vec<String> = Vec::new();
    std::fs::write(dir.join("modify.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "modify.vyrn", false);
    if ok || !text.lines().next().is_some_and(|l| l.ends_with(want)) {
        bad.push(format!("`check` said {text}"));
    }
    for engine in [
        &["run", "modify.vyrn"][..],
        &["build", "modify.vyrn", "-o", "never"][..],
    ] {
        let out = vyrn()
            .current_dir(&dir)
            .args(engine)
            .output()
            .expect("vyrn");
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.success() || !out.stdout.is_empty() || !err.contains(want) {
            bad.push(format!("`{}` ran or said {err}", engine[0]));
        }
    }
    // The way out the menu names: `xs` is a value of its own.
    let fixed = src.replace("let xs = b.items\n", "let xs = b.items.copy()\n");
    std::fs::write(dir.join("copy.vyrn"), fixed).expect("write the program");
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "copy.vyrn"])
        .output()
        .expect("vyrn run");
    if String::from_utf8_lossy(&out.stdout).trim() != "1" {
        bad.push(format!(
            "the copy printed {:?}",
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A call through a `fn` value reads the value: a `modify` of the
/// root it was read out of ends that borrow, so the call after it is refused.
/// Accepted, the call would go through the field's address and run the
/// replacement.
#[test]
fn a_call_through_a_fn_value_after_a_modify_of_its_root_is_refused() {
    let src = "type H = { f: fn(Int64) -> Int64, n: Int64 }
               fn inc(x: Int64) -> Int64 { return x + 1 }
               fn dbl(x: Int64) -> Int64 { return x * 2 }
               fn swap(h: modify H) { h.f = dbl }
               fn main() -> Int64 {
  let mut h = H { f: inc, n: 1 }
                 let g = h.f
  swap(h)
  print(g(5))
  return 0
}
";
    let dir = common::scratch("fnval-modify");
    let want = "`h` is written here while `g` still reads out of it";
    let mut bad: Vec<String> = Vec::new();
    std::fs::write(dir.join("modify.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "modify.vyrn", false);
    if ok || !text.lines().next().is_some_and(|l| l.ends_with(want)) {
        bad.push(format!("`check` said {text}"));
    }
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "modify.vyrn"])
        .output()
        .expect("vyrn run");
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || !out.stdout.is_empty() || !err.contains(want) {
        bad.push(format!("`run` ran or said {err}"));
    }
    // The way out the menu names: `g` is a value of its own, the one `inc`.
    let fixed = src.replace(
        "let g = h.f
",
        "let g = h.f.copy()
",
    );
    std::fs::write(dir.join("copy.vyrn"), fixed).expect("write the program");
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "copy.vyrn"])
        .output()
        .expect("vyrn run");
    if String::from_utf8_lossy(&out.stdout).trim() != "6" {
        bad.push(format!(
            "the copy printed {:?}",
            String::from_utf8_lossy(&out.stdout)
        ));
    }
    assert!(
        bad.is_empty(),
        "{}",
        bad.join(
            "
  "
        )
    );
}

/// A payload binder reads out of the scrutinee for the arm's extent, whoever
/// owns the scrutinee: a store or a `modify` argument that writes the
/// scrutinee while the binder lives is refused, heap or not. The emitter holds
/// the binder as the payload's address, so a heapless payload would see the
/// write.
#[test]
fn a_write_to_the_scrutinee_a_payload_binder_reads_is_refused() {
    let cases = [
        (
            "store",
            "fn main() -> Int64 {\n  let mut o: Option<Array<Int64>> = Some([1, 2, 3])\n  \
             let mut t = 0\n  match o {\n    Some(xs) => {\n      o = Some([7, 8, 9, 10])\n      \
             t = xs[0] + xs.length\n    }\n    None => {}\n  }\n  print(t)\n  return 0\n}\n",
            "`o` is written here while `xs` still reads out of it",
            "match o {",
            "match o.copy() {",
            "4",
        ),
        (
            "modify",
            "type P = { x: Int64, y: Int64 }\n\
             fn reset(o: modify Option<P>) { o = Some(P { x: 50, y: 0 }) }\n\
             fn main() -> Int64 {\n  let mut o: Option<P> = Some(P { x: 1, y: 0 })\n  \
             let mut t = 0\n  if let Some(p) = o {\n    reset(o)\n    t = p.x\n  }\n  \
             if let Some(q) = o { t = t + q.x * 100 }\n  print(t)\n  return 0\n}\n",
            "`o` is written here while `p` still reads out of it",
            "if let Some(p) = o {",
            "if let Some(p) = o.copy() {",
            "5001",
        ),
    ];
    let dir = common::scratch("binder-write");
    let mut bad: Vec<String> = Vec::new();
    for (name, src, want, from, to, prints) in cases {
        let file = format!("{name}.vyrn");
        std::fs::write(dir.join(&file), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &file, false);
        if ok || !text.lines().next().is_some_and(|l| l.ends_with(want)) {
            bad.push(format!("`check {file}` said {text}"));
        }
        let out = vyrn()
            .current_dir(&dir)
            .args(["run", &file])
            .output()
            .expect("vyrn");
        let err = String::from_utf8_lossy(&out.stderr);
        if out.status.success() || !out.stdout.is_empty() || !err.contains(want) {
            bad.push(format!("`run {file}` ran or said {err}"));
        }
        // The way out the menu names: the binder reads a value of its own.
        let copy = format!("{name}-copy.vyrn");
        std::fs::write(dir.join(&copy), src.replace(from, to)).expect("write the program");
        let out = vyrn()
            .current_dir(&dir)
            .args(["run", &copy])
            .output()
            .expect("vyrn run");
        if String::from_utf8_lossy(&out.stdout).trim() != prints {
            bad.push(format!(
                "`run {copy}` printed {:?}",
                String::from_utf8_lossy(&out.stdout)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A call whose callee stores into a global ends every borrow of that global,
/// by the effect judgment's write half; otherwise the borrow reads a
/// freed array. The fix names the `let` of the borrow, also where a `while`
/// walks it.
#[test]
fn a_call_that_writes_module_state_ends_its_borrows() {
    let src = "let mut xs: Array<Int64> = [1, 2, 3]\n\
               fn reset() { xs = [7, 8, 9, 10] }\n\
               fn main() -> Int64 {\n  let a = xs\n  reset()\n  print(a.length)\n  \
               print(a[0])\n  return 0\n}\n";
    let want = "`xs` is written here while `a` still reads out of it";
    let dir = common::scratch("state-write");
    let mut bad: Vec<String> = Vec::new();
    std::fs::write(dir.join("state.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "state.vyrn", false);
    if ok || !text.lines().next().is_some_and(|l| l.ends_with(want)) {
        bad.push(format!("`check state.vyrn` said {text}"));
    }
    // A `while` that walks `a` hoists no header the call writes: the fix
    // names the `let` on line 4, not the loop on line 6.
    let walked = "let mut xs: Array<Int64> = [1, 2, 3]\n\
                  fn reset() { xs = [7, 8, 9, 10] }\n\
                  fn main() -> Int64 {\n  let a = xs\n  let mut i = 0\n  \
                  while i < a.length {\n    reset()\n    print(a[i])\n    i = i + 1\n  }\n  \
                  return 0\n}\n";
    std::fs::write(dir.join("walked.vyrn"), walked).expect("write the program");
    let out = vyrn()
        .current_dir(&dir)
        .args(["check", "walked.vyrn"])
        .output()
        .expect("vyrn check");
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success()
        || !err.contains(want)
        || !err.contains("fix: `xs.copy()` on line 4,")
        || err.contains("on line 6,")
    {
        bad.push(format!("`check walked.vyrn` said {err}"));
    }
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "state.vyrn"])
        .output()
        .expect("vyrn");
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || !out.stdout.is_empty() || !err.contains(want) {
        bad.push(format!("`run state.vyrn` ran or said {err}"));
    }
    // The way out the menu names: `a` is a value of its own.
    let copy = src.replace("let a = xs\n", "let a = xs.copy()\n");
    std::fs::write(dir.join("state-copy.vyrn"), copy).expect("write the program");
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "state-copy.vyrn"])
        .output()
        .expect("vyrn run");
    let got = String::from_utf8_lossy(&out.stdout);
    if got.split_whitespace().collect::<Vec<_>>() != ["3", "1"] {
        bad.push(format!("`run state-copy.vyrn` printed {got:?}"));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A payload binder of a value whose type declares `release` may not be handed
/// to a `consume` parameter, because the declared release reads every payload;
/// the hand-off would free it twice or leak the node. Both ways out run clean
/// under `VYRN_LEAK_CHECK`; the copy's arm releases `n` through the declared
/// `release`.
#[test]
fn a_payload_of_a_type_that_declares_release_is_not_handed_on() {
    let src = "type Node =\n  | Elem(String, Array<Int64>)\n  | Text(String)\n\
               impl Owned for Node {\n  fn release(consume self) {\n    match consume self {\n      \
               Elem(tag, kids) => {\n        drop tag\n        drop kids\n      }\n      \
               Text(s) => {\n        drop s\n      }\n    }\n  }\n}\n\
               type One = { node: Node, k: Int64 }\n\
               fn sum(xs: consume Array<Int64>) -> Int64 {\n  let mut t = 0\n  \
               for x in xs {\n    t = t + x\n  }\n  return t\n}\n\
               fn f(n: consume Node) -> One {\n  return match n {\n    \
               Elem(tag, kids) => One { node: Text(\"x\"), k: sum(kids) },\n    \
               Text(s) => One { node: n, k: 0 },\n  }\n}\n\
               fn main() -> Int64 {\n  let a = f(Elem(\"a\", [1, 2, 3]))\n  \
               let b = f(Text(\"b\"))\n  print(a.k + b.k)\n  return 0\n}\n";
    let want = "`kids` may not be handed to a `consume` parameter: `Node` declares \
                `release`, which reads it; consume `n` or copy `kids`";
    let dir = common::scratch("sealed-payload");
    let mut bad: Vec<String> = Vec::new();
    std::fs::write(dir.join("m.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "m.vyrn", false);
    if ok || !text.lines().next().is_some_and(|l| l.ends_with(want)) {
        bad.push(format!("`check m.vyrn` said {text}"));
    }
    let out = vyrn()
        .current_dir(&dir)
        .args(["run", "m.vyrn"])
        .output()
        .expect("vyrn");
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || !out.stdout.is_empty() || !err.contains(want) {
        bad.push(format!("`run m.vyrn` ran or said {err}"));
    }
    let ways = [
        ("copy", "sum(kids)", "sum(kids.copy())"),
        ("consume", "match n {", "match consume n {"),
    ];
    for (name, from, to) in ways {
        let file = format!("{name}.vyrn");
        let fixed = src.replace(from, to);
        let fixed = if name == "consume" {
            fixed.replace("One { node: n, k: 0 }", "One { node: Text(s), k: 0 }")
        } else {
            fixed
        };
        std::fs::write(dir.join(&file), fixed).expect("write the program");
        let out = vyrn()
            .current_dir(&dir)
            .env("VYRN_LEAK_CHECK", "1")
            .args(["run", &file])
            .output()
            .expect("vyrn run");
        if out.status.code() != Some(0) || String::from_utf8_lossy(&out.stdout).trim() != "6" {
            bad.push(format!(
                "`run {file}` exited {:?} and said {}{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n  "));
}

/// A cold cache compiles the generator, and that compile's kernel refusals must
/// reach the host's list once, with their file.
#[test]
fn a_gen_body_is_refused_once_at_its_own_line_cold_and_warm() {
    let dir = common::scratch("gen-body-refusal");
    std::fs::write(
        dir.join("lib.vyrn"),
        "fn sink(t: consume String) -> Int64 { drop t return 0 }\n\
         \n\
         export gen fn gen1(d: String) -> String {\n\
         \x20   if d == \"never\" {\n\
         \x20       let x = \"a\" + \"b\"\n\
         \x20       let n = sink(x)\n\
         \x20       region { let x = 9 }\n\
         \x20       return x + \"!\"\n\
         \x20   }\n\
         \x20   return \"export fn v() -> Int64 { return 7 }\"\n\
         }\n",
    )
    .expect("write lib.vyrn");
    std::fs::write(
        dir.join("host.vyrn"),
        "import { gen1 } from \"./lib\"\n\
         import { v } from gen1(\"x\")\n\
         \n\
         fn main() -> Int64 { return v() }\n",
    )
    .expect("write host.vyrn");
    let check = || {
        let out = vyrn()
            .current_dir(&*dir)
            .env("VYRN_GEN_CACHE_DIR", dir.join("cache"))
            .env_remove("VYRN_NO_GEN_CACHE")
            .args(["check", "host.vyrn"])
            .output()
            .expect("vyrn check");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        )
    };
    let (cold_ok, cold) = check();
    let (warm_ok, warm) = check();
    assert!(!cold_ok && !warm_ok, "accepted:\n{cold}\n{warm}");
    assert_eq!(cold, warm, "the answer depends on the generator cache");
    assert_eq!(cold.matches("is used here but").count(), 1, "{cold}");
    assert!(cold.starts_with("lib.vyrn:8:"), "{cold}");
}

/// A gen fn no program runs as a generator is judged by the program that
/// holds it, which is the only build that sees it.
#[test]
fn a_gen_body_no_program_runs_is_refused() {
    let dir = common::scratch("gen-body-unran");
    std::fs::write(
        dir.join("lib.vyrn"),
        "fn sink(t: consume String) -> Int64 { drop t return 0 }\n\
         \n\
         export fn plain() -> Int64 { return 3 }\n\
         \n\
         export gen fn unran(d: String) -> String {\n\
         \x20   let x = \"a\" + \"b\"\n\
         \x20   let n = sink(x)\n\
         \x20   region { let x = 9 }\n\
         \x20   return x + \"!\"\n\
         }\n",
    )
    .expect("write lib.vyrn");
    std::fs::write(
        dir.join("host.vyrn"),
        "import { plain } from \"./lib\"\n\nfn main() -> Int64 { return plain() }\n",
    )
    .expect("write host.vyrn");
    let out = vyrn()
        .current_dir(&*dir)
        .args(["check", "host.vyrn"])
        .output()
        .expect("vyrn check");
    let err = String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n");
    assert!(!out.status.success(), "accepted:\n{err}");
    assert!(err.starts_with("lib.vyrn:9:"), "{err}");
}

/// A `for` over a user container holds it for the whole loop, as over an
/// `Array`: the core walks the `Iterate` expansion, whose `nth` reads it each
/// turn (record `0125-m7-letpay`).
#[test]
fn a_store_into_a_user_container_inside_its_own_loop_is_refused() {
    let window = "type Window = { data: Array<Int64>, start: Int64 }
         impl Iterate for Window {
             fn size(self) -> Int64 { return self.data.length - self.start }
             fn nth(read self, i: Int64) -> read Int64 { return self.data[self.start + i] }
         }";
    let cases = [
        ("a push", "`w.data` is written here", "w.data.push(x)"),
        (
            "a store",
            "`w` is written here",
            "w = Window { data: [], start: 0 }",
        ),
    ];
    let dir = common::scratch("iterate-hold");
    let mut bad: Vec<String> = Vec::new();
    for (what, says, store) in cases {
        let name = format!("{}.vyrn", what.replace(' ', "_"));
        let src = format!(
            "{window}
             fn main() -> Int64 {{
                 let mut w = Window {{ data: [1, 2, 3], start: 0 }}
                 for x in w {{ {store} }}
                 return 0
             }}"
        );
        std::fs::write(dir.join(&name), src).expect("write the program");
        let (ok, text) = refusal_in(dir.to_path_buf(), &name, false);
        if ok || !text.contains(&format!("{says} while `w` still reads out of it")) {
            bad.push(format!("{what}: {}", if ok { "accepted" } else { &text }));
        }
    }
    assert!(
        bad.is_empty(),
        "a store inside the loop is accepted:\n  {}",
        bad.join("\n  ")
    );
}

/// A `modify` projection rooted at a parameter that is not `modify` returns
/// the argument's value, so the checker refuses it before the backend finds
/// no place to store through (record `0125-m11-lazy-unify`).
#[test]
fn a_modify_projection_rooted_at_a_read_parameter_is_refused() {
    let dir = common::scratch("modify-read-root");
    std::fs::write(
        dir.join("p.vyrn"),
        "type Slice = { data: Array<Int64>, start: Int64 }
         impl Index for Slice {
             fn at(read self, i: Int64) -> read Int64 { return self.data[self.start + i] }
             fn atSet(modify self, i: Int64) -> modify Int64 { return i }
         }
         fn main() -> Int64 {
             let mut s = Slice { data: [1, 2], start: 0 }
             s[0] = 5
             return 0
         }",
    )
    .expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "p.vyrn", false);
    assert!(!ok, "accepted");
    assert!(
        text.contains("projection `atSet` returns a value, not a place"),
        "{text}"
    );
}

/// #463: `m[k]` on a `Map` borrows the entry, so a binder of `match m[k]`
/// borrows too, and a take of it is refused. The map still owns the array.
#[test]
fn a_binder_of_a_map_key_read_is_a_borrow() {
    let src = "fn keep(xs: consume Array<Int64>) -> Int64 { return xs.length } \
               fn main() -> Int64 { let mut m: Map<String, Array<Int64>> = [:] \
               m[\"a\"] = [1, 2] \
               let n = match m[\"a\"] { Some(xs) => keep(xs), None => 0 } \
               return n }";
    let dir = common::scratch("map-key-binder");
    std::fs::write(dir.join("m.vyrn"), src).expect("write the program");
    let (ok, text) = whole_refusal_in(dir.to_path_buf(), "m.vyrn", false);
    assert!(!ok, "accepted");
    assert!(
        text.contains(
            "`xs` may not be passed to a `consume` parameter via `keep(..)` — it is read out of \
             a place that owns it"
        ),
        "{text}"
    );
}

/// A join arm that yields a name bound outside the construct moves it, so a
/// use after the join is refused as maybe-moved, with the copy as the fix.
#[test]
fn a_name_yielded_out_of_a_join_arm_is_moved() {
    let src = "fn f(c: Bool) -> Int64 { let st = \"a\" + \"b\" \
               let rel = if c { st } else { st + \"/\" } \
               return rel.byteLength + st.byteLength } \
               fn main() -> Int64 { return f(true) }";
    let dir = common::scratch("join-yield-moves");
    std::fs::write(dir.join("m.vyrn"), src).expect("write the program");
    let (ok, text) = whole_refusal_in(dir.to_path_buf(), "m.vyrn", false);
    assert!(!ok, "accepted");
    for needle in [
        "`st` was moved here into a store",
        "... and `st` is used again here",
        "fix: `st.copy()` if both sides need a value",
    ] {
        assert!(text.contains(needle), "wanted `{needle}`, got {text}");
    }
}
