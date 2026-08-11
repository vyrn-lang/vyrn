//! RFC-0082 M1: a container mutation reaches through a *place* — a record
//! field, an array element, a chain of them — and not only a plain variable.
//!
//! The behaviour is pinned three ways by `examples/slottable.vyrn`. What
//! behaviour cannot pin is the thing the whole milestone turns on: the desugar
//! must MOVE the container's header out of the field and back, not copy the
//! container. A copying lowering would be just as correct and would make every
//! write O(N), so no output and no assertion can tell them apart — only the
//! emitted code can. These tests are that half, in the shape of RFC-0081's
//! `the_json_writer_does_not_copy_once_per_element`: a structural count, not a
//! duration, so a loaded machine cannot make them flaky.

mod common;
use common::*;

/// The body of `fn <name>` in `src`'s emitted LLVM IR.
fn body_of(src: &str, name: &str) -> String {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    // Two tests here both emit `fn bump`, and cargo runs them concurrently, so
    // the file name has to be unique per call and not per function.
    static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = dir.join(format!("{name}-{nth}.vyrn"));
    std::fs::write(&file, src).unwrap();
    let out = vyrn()
        .arg("emit-ir")
        .arg(&file)
        .output()
        .expect("vyrn emit-ir");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ir = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let start = ir
        .find(&format!("@vyrn_{name}("))
        .unwrap_or_else(|| panic!("no `vyrn_{name}` in the emitted IR:\n{ir}"));
    let start = ir[..start]
        .rfind("\ndefine ")
        .expect("no `define` before it")
        + 1;
    ir[start..start + ir[start..].find("\n}\n").expect("unterminated body")].to_string()
}

/// Every `call` in a body, minus the trap path — which is unreachable on the
/// hot path and prints and exits, never allocates. Phase 8d made that path one
/// `@__vyrn_trap_*` call where it was `stderr`/`fprintf`/`exit` inline, so both
/// spellings are listed and neither counts.
fn allocating_calls(body: &str) -> Vec<&str> {
    body.lines()
        .filter(|l| l.contains("call ") || l.contains("call("))
        .filter(|l| {
            [
                "@__vyrn_stderr",
                "@fprintf",
                "@exit",
                "@__vyrn_trap_msg",
                "@__vyrn_trap_idx",
                "@__vyrn_panic",
                // The call-depth counter (RFC-0004 addendum): a load, an add and
                // a store on one global, in every prologue and before every
                // `ret`. It allocates nothing, which is what this list is for.
                "@__vyrn_call_enter",
                "@__vyrn_call_exit",
            ]
            .iter()
            .all(|f| !l.contains(f))
        })
        .collect()
}

/// `s.xs[i] = 9` must lower to: load the `{ptr,len,cap}` header out of the
/// record, bounds-check, store the element, put the header back. Nothing is
/// allocated and nothing is copied elementwise, which is what makes it O(1) per
/// write instead of O(N).
#[test]
fn an_index_assign_through_a_record_field_allocates_nothing() {
    let body = body_of(
        "type Store = { xs: Array<Int64>, n: Int64 }\n\
         fn bump(s: modify Store, i: Int64) {\n\
         s.xs[i] = 9\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut s = Store { xs: [1, 2, 3], n: 0 }\n\
         bump(s, 0)\n\
         print(s.xs[0])\n\
         return 0\n\
         }\n",
        "bump",
    );
    let calls = allocating_calls(&body);
    assert!(
        calls.is_empty(),
        "an index assignment through a field must not allocate or copy — a \
         copying desugar would be correct and quadratic:\n{}\nin:\n{body}",
        calls.join("\n")
    );
    // The store itself is still there (an empty body would also allocate
    // nothing), and there is exactly one — not one per element.
    assert_eq!(
        body.matches("store i64 9, ptr").count(),
        1,
        "expected exactly one element store:\n{body}"
    );
}

/// The same for `pop`, which mutates AND returns, so it is hoisted around the
/// whole statement rather than desugared inside the expression.
#[test]
fn a_pop_through_a_record_field_allocates_nothing() {
    let body = body_of(
        "type Store = { xs: Array<Int64>, n: Int64 }\n\
         fn take(s: modify Store) -> Int64 {\n\
         let x = s.xs.pop()\n\
         return x ?? -1\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut s = Store { xs: [1, 2, 3], n: 0 }\n\
         print(take(s))\n\
         print(s.xs.length)\n\
         return 0\n\
         }\n",
        "take",
    );
    let calls = allocating_calls(&body);
    assert!(
        calls.is_empty(),
        "`pop` through a field must shrink the header in place, not rebuild the \
         array:\n{}\nin:\n{body}",
        calls.join("\n")
    );
}

/// The nested case: `o.i.xs[k] = v` moves the outer field out first and back
/// last. Records are values, so the outer move is a fixed-size header copy —
/// still independent of the array's length, which is the property that matters.
#[test]
fn a_nested_field_chain_allocates_nothing_either() {
    let body = body_of(
        "type Inner = { xs: Array<Int64> }\n\
         type Outer = { i: Inner, n: Int64 }\n\
         fn bump(o: modify Outer, k: Int64) {\n\
         o.i.xs[k] = 9\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut o = Outer { i: Inner { xs: [1, 2] }, n: 0 }\n\
         bump(o, 1)\n\
         print(o.i.xs[1])\n\
         return 0\n\
         }\n",
        "bump",
    );
    let calls = allocating_calls(&body);
    assert!(calls.is_empty(), "{}\nin:\n{body}", calls.join("\n"));
}

/// The interpreter timing the two ratios below are built from: the fastest of
/// three `vyrn run`s of `src`, which must print `expect`.
fn best_of_3(dir: &std::path::Path, name: &str, src: &str, expect: &str) -> std::time::Duration {
    let file = dir.join(format!("{name}.vyrn"));
    std::fs::write(&file, src).unwrap();
    (0..3)
        .map(|_| {
            let t = std::time::Instant::now();
            let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), expect);
            t.elapsed()
        })
        .min()
        .unwrap()
}

/// The interpreter's half, which this file could not see at all until RFC-0082
/// M2 — and that is precisely why a quadratic shipped: the IR assertions above
/// are true, the compiled backends really do write through one buffer, and the
/// interpreter was copying the whole vector on every write anyway.
///
/// There is no emitted code to count here, so the pin is a RATIO between two
/// programs that differ in one token: whether the array lives in a record field
/// or in a local. Both do N writes. Same machine, same interpreter, same
/// process floor, so a loaded box slows both and the ratio holds; what does not
/// hold is a copy per write, which measured 660x at this N before the fix
/// against 1.4x after. The threshold is deliberately far from both.
#[test]
fn the_interpreter_does_not_copy_the_array_once_per_write() {
    const N: usize = 32_000;
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();

    // The array is built as a local in BOTH, then handed to the record: `push`
    // through a field is a separate quadratic (RFC-0082 M2's "as landed") and
    // measuring it here would hide the one this test is for.
    let build = format!(
        "let mut xs: Array<Int64> = []\n\
         let mut i = 0\n\
         while i < {N} {{ xs.push(0)  i = i + 1 }}\n\
         let mut k = 0\n"
    );
    let plain = format!(
        "fn main() -> Int64 {{\n{build}\
         while k < {N} {{ xs[k] = k  k = k + 1 }}\n\
         print(xs[{N} - 1])\n\
         return 0\n}}\n"
    );
    let field = format!(
        "type T = {{ xs: Array<Int64> }}\n\
         fn main() -> Int64 {{\n{build}\
         let mut t = T {{ xs: xs }}\n\
         while k < {N} {{ t.xs[k] = k  k = k + 1 }}\n\
         print(t.xs[{N} - 1])\n\
         return 0\n}}\n"
    );

    let plain = best_of_3(&dir, "interp-plain", &plain, "31999");
    let field = best_of_3(&dir, "interp-field", &field, "31999");
    assert!(
        field.as_secs_f64() < 4.0 * plain.as_secs_f64(),
        "an index assignment through a field is copying the array: \
         {field:?} against {plain:?} for the same {N} writes on a local"
    );
}

/// The same ratio for `push`, which is the OTHER quadratic M2 found and left:
/// `t.xs.push(v)` desugars to `t.xs = push(t.xs, v)`, so the general path read
/// the field into a second `Rc` while the field still held the first, and the
/// `push` builtin's `make_mut` copied the whole vector per append.
///
/// One token apart again — whether the array being appended to lives in a record
/// field or in a local — and the same N appends. 449x at this N before the fix
/// (8.90 s against 19.8 ms), 1.1x after. A ratio and not a duration because the
/// claim is a complexity class and a loaded box slows both sides equally.
#[test]
fn the_interpreter_does_not_copy_the_array_once_per_append() {
    const N: usize = 32_000;
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();

    let local = format!(
        "fn main() -> Int64 {{\n\
         let mut xs: Array<Int64> = []\n\
         let mut i = 0\n\
         while i < {N} {{ xs.push(i)  i = i + 1 }}\n\
         print(xs[{N} - 1])\n\
         return 0\n}}\n"
    );
    let field = format!(
        "type T = {{ xs: Array<Int64> }}\n\
         fn main() -> Int64 {{\n\
         let mut t = T {{ xs: [] }}\n\
         let mut i = 0\n\
         while i < {N} {{ t.xs.push(i)  i = i + 1 }}\n\
         print(t.xs[{N} - 1])\n\
         return 0\n}}\n"
    );
    let local = best_of_3(&dir, "interp-push-local", &local, "31999");
    let field = best_of_3(&dir, "interp-push-field", &field, "31999");
    assert!(
        field.as_secs_f64() < 4.0 * local.as_secs_f64(),
        "a push through a field is copying the array: \
         {field:?} against {local:?} for the same {N} appends on a local"
    );
}

/// The same ratio for the THIRD and last receiver form, `rows[i].push(v)` —
/// RFC-0082 M2's finding 5, left open by the append fix above because it lands
/// in `Stmt::IndexSet` rather than `Stmt::SetField`.
///
/// It is the same quadratic for the same reason (`at` clones the row's `Rc` and
/// `push`'s `make_mut` copies it), and it is fixed by the same snapshot — one
/// `append_snapshot` now serves both, which is why there is no third pin shape
/// either: N appends onto a local against N appends into `rows[0]`, one token
/// apart. 438x at this N before the fix (9.08 s against 20.8 ms), 1.1x after.
#[test]
fn the_interpreter_does_not_copy_the_row_once_per_append() {
    const N: usize = 32_000;
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();

    let local = format!(
        "fn main() -> Int64 {{\n\
         let mut xs: Array<Int64> = []\n\
         let mut i = 0\n\
         while i < {N} {{ xs.push(i)  i = i + 1 }}\n\
         print(xs[{N} - 1])\n\
         return 0\n}}\n"
    );
    let element = format!(
        "fn main() -> Int64 {{\n\
         let mut rows: Array<Array<Int64>> = [[]]\n\
         let mut i = 0\n\
         while i < {N} {{ rows[0].push(i)  i = i + 1 }}\n\
         print(rows[0][{N} - 1])\n\
         return 0\n}}\n"
    );
    let local = best_of_3(&dir, "interp-push-local", &local, "31999");
    let element = best_of_3(&dir, "interp-push-elem", &element, "31999");
    assert!(
        element.as_secs_f64() < 4.0 * local.as_secs_f64(),
        "a push through an array element is copying the row: \
         {element:?} against {local:?} for the same {N} appends on a local"
    );
}

/// The third quadratic, and the one behaviour cannot see either: `coerce`
/// rebuilt a whole array at every typed boundary even when the element type can
/// neither change a value nor reject one, so `rows[i][j] = v` paid for the inner
/// row's LENGTH on every store (RFC-0082 M2 finding 3, closed in M3 by the
/// short-circuit the field-store validation needed anyway).
///
/// The ratio is between two grids with the same number of WRITES and different
/// row lengths — 40x4000 against 1600x100, 160,000 stores either way. A cost
/// proportional to the row is the only thing that can tell them apart: 15.2x
/// before (6,950 ms against 457), 1.0x after (272 against 268).
#[test]
fn the_interpreter_does_not_rebuild_a_row_per_element_store() {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let grid = |rows: usize, cols: usize| {
        format!(
            "fn main() -> Int64 {{\n\
             let mut rows: Array<Array<Int64>> = []\n\
             let mut r = 0\n\
             while r < {rows} {{\n\
             let mut row: Array<Int64> = []\n\
             let mut c = 0\n\
             while c < {cols} {{ row.push(0)  c = c + 1 }}\n\
             rows.push(row)\n\
             r = r + 1\n\
             }}\n\
             let mut i = 0\n\
             while i < {rows} {{\n\
             let mut j = 0\n\
             while j < {cols} {{ rows[i][j] = 1  j = j + 1 }}\n\
             i = i + 1\n\
             }}\n\
             print(rows[{rows} - 1][{cols} - 1])\n\
             return 0\n}}\n"
        )
    };
    let short = best_of_3(&dir, "interp-grid-short", &grid(1600, 100), "1");
    let long = best_of_3(&dir, "interp-grid-long", &grid(40, 4000), "1");
    assert!(
        long.as_secs_f64() < 4.0 * short.as_secs_f64(),
        "an element store is rebuilding its row: {long:?} for 40x4000 against \
         {short:?} for 1600x100 — the same 160,000 writes"
    );
}

/// The same store, with a VALIDATED element type — the half that had to be paid
/// for and was nearly paid twice. M3 made a field write coerce (so a runtime
/// value entering `t.xs: Array<Age>` is checked at all), and the write-back
/// every place desugar ends with then re-proved the whole array per store:
/// 13,467 ms for 8,000 writes against 76 for the same loop on `Array<Int64>`.
///
/// A variable already OF the field's type is skipped, which is what the compiled
/// backends do (`validation_required` is `None` when `from == to`). So the
/// predicate costs a constant per store, not a scan — and the ratio says so
/// without depending on the machine.
#[test]
fn a_validated_element_type_costs_a_constant_per_store() {
    const N: usize = 8_000;
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let prog = |decl: &str, elem: &str| {
        format!(
            "{decl}type T = {{ xs: Array<{elem}> }}\n\
             fn main() -> Int64 {{\n\
             let mut t = T {{ xs: [] }}\n\
             let mut i = 0\n\
             while i < {N} {{ t.xs.push(i + 18)  i = i + 1 }}\n\
             let mut k = 0\n\
             while k < {N} {{ t.xs[k] = k + 18  k = k + 1 }}\n\
             print(t.xs[{N} - 1])\n\
             return 0\n}}\n"
        )
    };
    let expect = (N + 17).to_string();
    let plain = best_of_3(&dir, "interp-store-plain", &prog("", "Int64"), &expect);
    let validated = best_of_3(
        &dir,
        "interp-store-validated",
        &prog("type Age = Int64 where value >= 18\n", "Age"),
        &expect,
    );
    assert!(
        validated.as_secs_f64() < 4.0 * plain.as_secs_f64(),
        "a validated element type is re-validating the whole array per store: \
         {validated:?} against {plain:?} for the same {N} writes"
    );
}

/// The same ratio again, for a RECORD element type — because RFC-0084 M1 gave a
/// record a runtime name and stamping it is a thing `coerce` has to do, so a
/// named record type stopped being an identity coercion and the row rebuild came
/// straight back: 881 ms for 40x400 against 3,539 for 10x1600, the same 16,000
/// writes, which is the scaling the test above exists to forbid.
///
/// The fix is that a coercion whose only work is a name the value ALREADY
/// carries is not work. That still reads the row, so this ratio is not 1.0 the
/// way its `Int64` sibling's is — it is a name compare per element instead of a
/// `HashMap` clone per element, which is the difference between 9.7x and 1.4x.
#[test]
fn an_element_store_does_not_restamp_its_row() {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let grid = |rows: usize, cols: usize| {
        format!(
            "type Cell = {{ v: Int64 }}\n\
             fn main() -> Int64 {{\n\
             let mut rows: Array<Array<Cell>> = []\n\
             let mut r = 0\n\
             while r < {rows} {{\n\
             let mut row: Array<Cell> = []\n\
             let mut c = 0\n\
             while c < {cols} {{ row.push(Cell {{ v: 0 }})  c = c + 1 }}\n\
             rows.push(row)\n\
             r = r + 1\n\
             }}\n\
             let mut i = 0\n\
             while i < {rows} {{\n\
             let mut j = 0\n\
             while j < {cols} {{ rows[i][j] = Cell {{ v: 1 }}  j = j + 1 }}\n\
             i = i + 1\n\
             }}\n\
             print(rows[{rows} - 1][{cols} - 1].v)\n\
             return 0\n}}\n"
        )
    };
    let short = best_of_3(&dir, "interp-recgrid-short", &grid(400, 40), "1");
    let long = best_of_3(&dir, "interp-recgrid-long", &grid(25, 640), "1");
    assert!(
        long.as_secs_f64() < 3.0 * short.as_secs_f64(),
        "an element store is re-stamping its row: {long:?} for 25x640 against \
         {short:?} for 400x40 — the same 16,000 writes"
    );
}

/// `vyrn test` is a RECOVERABLE trap: a trapping test is reported and the next
/// one runs. M1 recorded that its stale field was unobservable "because traps
/// abort" and that a recoverable trap would have to revisit the desugar — this
/// is that trap, and it was already there. Taking the container out of MODULE
/// state would leave a `Val::Unit` behind that outlives the failed test, and the
/// next one reads `at of non-Array/Int64`: a value no program can otherwise
/// produce. So globals keep the copy, and this is the test that says so.
#[test]
fn a_trapping_test_does_not_leave_a_hole_in_module_state() {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("trap-hole.vyrn");
    std::fs::write(
        &file,
        "type T = { xs: Array<Int64> }\n\
         let mut gt: T = T { xs: [1, 2, 3] }\n\
         fn main() -> Int64 { print(gt.xs[0])  return 0 }\n\
         test \"traps mid-write\" { gt.xs[99] = 7 }\n\
         test \"still sees an array\" { assertEq(gt.xs[0], 1) }\n",
    )
    .unwrap();
    let out = vyrn().arg("test").arg(&file).output().expect("vyrn test");
    let all =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        all.contains("still sees an array\" ... ok"),
        "the field must survive the trapped test as an array:\n{all}"
    );
}

/// The receiver forms that have no place to write back to keep failing, and say
/// which ones do work. A call result is not a place: there is nowhere to put the
/// container back, so a desugar cannot rescue it and silently dropping the write
/// would be the worst outcome.
#[test]
fn a_call_result_is_still_not_an_assignable_place() {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("nonplace.vyrn");
    std::fs::write(&file, "fn main() -> Int64 { f()[0] = 9  return 0 }\n").unwrap();
    let out = vyrn().arg("check").arg(&file).output().expect("vyrn check");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "expected a refusal, got:\n{err}");
    assert!(
        err.contains("an array variable, a record field, or an array element"),
        "the refusal should name the forms that DO work:\n{err}"
    );
}
