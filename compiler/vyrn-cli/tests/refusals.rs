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
            "`s` is a `Stream<Int64>` and is never disposed",
            Kernel::Its,
        ),
        row(
            "r31_stream_disposed_twice.vyrn",
            "a must-use obligation is discharged exactly once",
            "`s` is a `Stream<Int64>` and is disposed more than once",
            Kernel::Its,
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
        row(
            "r43_function_value_of_a_consume_parameter.vyrn",
            "a function value reads every argument, so its target takes each by `read`",
            "`grow` cannot be used as a function value: it takes `c` by `consume`, and a `fn` \
             type reads every argument",
            Kernel::Elsewhere,
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
    assert!(
        bad.is_empty(),
        "rule 2 no longer refuses:\n  {}",
        bad.join("\n  ")
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

/// A callee reads its arguments until it returns, so an argument that reads a
/// global the call stores into, by the effect judgment, is refused at the
/// call, as a `for` over the global is. Accepted, the callee would read the
/// array the store freed. The fix copies the argument, where there is one.
#[test]
fn an_argument_that_reads_a_global_the_call_writes_is_refused() {
    let push = "fn fill() {\n  let mut i = 0\n  while i < 100 {\n    g.push(i)\n    \
                i = i + 1\n  }\n}\n";
    let cases = [
        (
            "read",
            format!(
                "let mut g: Array<Int64> = [1, 2, 3]\n{push}\
                 fn f(xs: Array<Int64>) -> Int64 {{\n  fill()\n  return xs[0] + xs[2]\n}}\n\
                 fn main() -> Int64 {{\n  print(f(g))\n  return 0\n}}\n"
            ),
            "`g` is written here while `g` still reads out of it",
            Some(("f(g)", "f(g.copy())", "4")),
        ),
        (
            "binding",
            format!(
                "let mut g: Array<Int64> = [1, 2, 3]\n{push}\
                 fn f(xs: Array<Int64>) -> Int64 {{\n  fill()\n  return xs[0] + xs[2]\n}}\n\
                 fn main() -> Int64 {{\n  let a = g\n  print(f(a))\n  return 0\n}}\n"
            ),
            "`g` is written here while `a` still reads out of it",
            Some(("let a = g", "let a = g.copy()", "4")),
        ),
        (
            "field",
            "type R = { xs: Array<Int64>, n: Int64 }\n\
             let mut g: R = R { xs: [1, 2, 3], n: 3 }\n\
             fn grow() { g.xs = [4, 5, 6, 7] }\n\
             fn f(xs: Array<Int64>) -> Int64 {\n  grow()\n  return xs[0]\n}\n\
             fn main() -> Int64 {\n  print(f(g.xs))\n  return 0\n}\n"
                .to_string(),
            "`g` is written here while `g.xs` still reads out of it",
            Some(("f(g.xs)", "f(g.xs.copy())", "1")),
        ),
        (
            "modify",
            format!(
                "let mut g: Array<Int64> = [1, 2, 3]\n{push}\
                 fn f(xs: modify Array<Int64>) {{\n  fill()\n  xs.push(9)\n}}\n\
                 fn main() -> Int64 {{\n  f(g)\n  print(g.length)\n  return 0\n}}\n"
            ),
            "`g` is written here while `g` still reads out of it",
            None,
        ),
        (
            "binder",
            "let mut pending: Map<String, fn(Int64)> = [:]\n\
             fn deliver(key: String, cb: fn(Int64), res: Int64) {\n  \
             pending.remove(key)\n  cb(res)\n}\n\
             fn main() -> Int64 {\n  let name = \"a\" + \"b\"\n  \
             pending[\"k\"] = x -> print(name + x.toString())\n  \
             match pending[\"k\"] {\n    Some(cb) => deliver(\"k\", cb, 7),\n    \
             None => {}\n  }\n  return 0\n}\n"
                .to_string(),
            "`pending` is written here while `cb` still reads out of it",
            Some((
                "deliver(\"k\", cb, 7)",
                "deliver(\"k\", cb.copy(), 7)",
                "ab7",
            )),
        ),
    ];
    let dir = common::scratch("state-arg");
    let mut bad: Vec<String> = Vec::new();
    for (name, src, want, fix) in cases {
        let file = format!("{name}.vyrn");
        std::fs::write(dir.join(&file), &src).expect("write the program");
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
        let Some((from, to, prints)) = fix else {
            continue;
        };
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
    let cold = refused_cold_and_warm(&dir, &["check", "host.vyrn"]);
    assert_eq!(cold.matches("is used here but").count(), 1, "{cold}");
    assert!(cold.starts_with("lib.vyrn:8:"), "{cold}");
}

/// A failed generator's error is the typed refusal its compile states, also
/// when a warm cache skips that compile.
#[test]
fn a_failed_generator_says_its_typed_refusal_cold_and_warm() {
    let dir = common::scratch("gen-typed-trap");
    std::fs::write(
        dir.join("lib.vyrn"),
        "export gen fn g() -> String {\n\
         \x20   let n = 1\n\
         \x20   n = 2\n\
         \x20   panic(\"stop\")\n\
         \x20   return \"export fn f() -> Int64 { return 1 }\"\n\
         }\n",
    )
    .expect("write lib.vyrn");
    std::fs::write(
        dir.join("host.vyrn"),
        "import { g } from \"./lib\"\n\
         import { f } from g()\n\
         \n\
         fn main() -> Int64 { return f() }\n",
    )
    .expect("write host.vyrn");
    let cold = refused_cold_and_warm(&dir, &["check", "host.vyrn"]);
    assert!(cold.contains("cannot assign to `n`"), "{cold}");
}

/// A generator program's must-use refusal names the generator's module and
/// line, not the importer's.
#[test]
fn a_generator_programs_must_use_refusal_names_its_module() {
    let dir = common::scratch("gen-must-use");
    std::fs::write(
        dir.join("lib.vyrn"),
        "export gen fn g() -> String {\n\
         \x20   let xs: Array<Int64> = [1, 2]\n\
         \x20   let s = fromArray(xs)\n\
         \x20   return \"export fn f() -> Int64 { return 1 }\"\n\
         }\n",
    )
    .expect("write lib.vyrn");
    std::fs::write(
        dir.join("host.vyrn"),
        "import { g } from \"./lib\"\n\
         import { f } from g()\n\
         \n\
         fn main() -> Int64 { return f() }\n",
    )
    .expect("write host.vyrn");
    let cold = refused_cold_and_warm(&dir, &["check", "host.vyrn"]);
    assert!(cold.starts_with("lib.vyrn:3:"), "{cold}");
}

/// A generator program the typed judgment refuses is refused even when its
/// run succeeds. `emit-gen` checks no host, so only the engine can say it.
#[test]
fn a_refused_generator_program_is_refused_when_its_run_succeeds() {
    let dir = common::scratch("gen-typed-ran");
    std::fs::write(
        dir.join("lib.vyrn"),
        "export gen fn g() -> String {\n\
         \x20   let n = 1\n\
         \x20   n = 2\n\
         \x20   return \"export fn f() -> Int64 { return 1 }\"\n\
         }\n",
    )
    .expect("write lib.vyrn");
    std::fs::write(
        dir.join("host.vyrn"),
        "import { g } from \"./lib\"\n\
         import { f } from g()\n\
         \n\
         fn main() -> Int64 { return f() }\n",
    )
    .expect("write host.vyrn");
    let cold = refused_cold_and_warm(&dir, &["emit-gen", "host.vyrn"]);
    assert!(cold.starts_with("lib.vyrn:3:0: cannot assign"), "{cold}");
}

/// Runs `vyrn` with `args` in `dir` twice over one generator cache, cold then
/// warm, and returns the refusal's stderr, which must not depend on the cache.
fn refused_cold_and_warm(dir: &Path, args: &[&str]) -> String {
    let check = || {
        let out = vyrn()
            .current_dir(dir)
            .env("VYRN_GEN_CACHE_DIR", dir.join("cache"))
            .env_remove("VYRN_NO_GEN_CACHE")
            .args(args)
            .output()
            .expect("run vyrn");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        )
    };
    let (cold_ok, cold) = check();
    let (warm_ok, warm) = check();
    assert!(!cold_ok && !warm_ok, "accepted:\n{cold}\n{warm}");
    assert_eq!(cold, warm, "the answer depends on the generator cache");
    cold
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

/// Declarations whose `Ow` release stores into `gs`, the module state a
/// `for x in gs` loop walks.
const RELEASE_WRITES_GS: &str = "let mut gs: Array<Int64> = [1, 2, 3, 4]
type Ow = { d: Array<Int64>, tag: Int64 }
impl Owned for Ow {
    fn release(consume self) {
        gs = [70, 71, 72, 73]
        let d = consume self.d
        drop d
    }
}
";

/// Asserts that `check` refuses [`RELEASE_WRITES_GS`] followed by `rest`
/// first on `line`, for the write of `gs`.
fn release_refused(scratch: &str, rest: &str, line: usize) {
    let dir = common::scratch(scratch);
    let src = RELEASE_WRITES_GS.to_string() + rest;
    std::fs::write(dir.join("release.vyrn"), src).expect("write the program");
    let (ok, text) = refusal_in(dir.to_path_buf(), "release.vyrn", false);
    let want =
        format!("release.vyrn:{line}:0: `gs` is written here while `gs` still reads out of it");
    assert!(
        !ok && text.lines().next().is_some_and(|l| l.ends_with(&want)),
        "`check` said {text}"
    );
}

/// The release the scope exit runs writes `gs` where the scope ends, at its
/// last statement's line.
#[test]
fn a_scope_exit_whose_release_writes_the_iterated_global_is_refused() {
    let rest = "fn main() -> Int64 {
    for x in gs {
        let o = Ow { d: [x], tag: x }
        print(o.tag.toString())
    }
    return 0
}
";
    release_refused("release-exit", rest, 13);
}

/// A store releases the value it displaces.
#[test]
fn a_store_whose_release_writes_the_iterated_global_is_refused() {
    let rest = "fn main() -> Int64 {
    let mut o = Ow { d: [0], tag: 0 }
    for x in gs {
        o = Ow { d: [x], tag: x }
    }
    return o.tag
}
";
    release_refused("release-store", rest, 13);
}

/// A record with no release of its own runs its fields' releases.
#[test]
fn a_nested_release_that_writes_the_iterated_global_is_refused() {
    let rest = "type Box = { inner: Ow, n: Int64 }
fn main() -> Int64 {
    for x in gs {
        let b = Box { inner: Ow { d: [x], tag: x }, n: x }
        print(b.n.toString())
    }
    return 0
}
";
    release_refused("release-nested", rest, 14);
}

/// A callee that releases an `Ow` at its own scope exit writes `gs` at the
/// call.
#[test]
fn a_call_whose_callee_releases_into_the_iterated_global_is_refused() {
    let rest = "fn make(x: Int64) -> Int64 {
    let o = Ow { d: [x], tag: x }
    return o.tag
}
fn main() -> Int64 {
    for x in gs {
        print(make(x).toString())
    }
    return 0
}
";
    release_refused("release-callee", rest, 16);
}

/// A generic declared `release` has no instance until its row is placed, so
/// the effect judgment reads it as written.
#[test]
fn a_generic_release_that_writes_the_iterated_global_is_refused() {
    let rest = "type Gw<T> = { d: Array<T> }
impl<T> Owned for Gw<T> {
    fn release(consume self) {
        gs = [70, 71, 72, 73]
        let d = consume self.d
        drop d
    }
}
fn main() -> Int64 {
    for x in gs {
        let g = Gw { d: [x] }
        print(x.toString())
    }
    return 0
}
";
    release_refused("release-generic", rest, 21);
}

// A refused store into a join's temporary leaves the temporary filled, so the
// join and the `let` after it draw no second sentence about it. Before, each
// program drew a second one that named the temporary (`@t1`).
#[test]
fn a_refused_join_draws_one_sentence_and_names_no_temporary() {
    for f in [
        "r37_join_of_a_borrow_then_an_owned_if_arm.vyrn",
        "r38_join_of_an_owned_then_a_borrowed_if_arm.vyrn",
        "r39_join_of_a_borrowed_then_an_owned_match_arm.vyrn",
        "r40_join_of_an_owned_then_a_borrowed_match_arm.vyrn",
    ] {
        let (ok, text) = refusal(f, false);
        assert!(!ok, "{f}: the checker accepted it");
        assert_eq!(
            text.matches(f).count(),
            1,
            "{f}: more than one sentence:\n{text}"
        );
        assert!(!text.contains('@'), "{f}: names a temporary:\n{text}");
    }
}
