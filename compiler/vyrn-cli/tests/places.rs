//! A container mutation through a place (a record field, an array element, a chain of
//! them) moves the container's header out and back instead of copying the container.
//! A copy is just as correct and makes every write O(N), so no output can
//! tell them apart; `examples/slottable.vyrn` pins the behaviour. The compiled tests
//! count calls in the emitted code; the interpreter tests compare the timings of two
//! programs one token apart, so a loaded machine slows both and the ratio holds.

mod common;
use common::*;

/// The module, and the one function in it that holds `marker`, as WAT. A body that
/// allocates or copies elementwise calls the allocator or `std/mem`'s copy; a body that
/// writes through its header calls only the trap its bounds checks branch to.
fn module_and_body(src: &str, marker: &str) -> (String, String) {
    static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = scratch("places-wat");
    let name = format!("p{nth}");
    (
        common::wat_of(&dir, &name, src),
        common::wat_func_containing(&dir, &name, src, marker),
    )
}

/// The distinct functions a body calls, as indices: a bounds check per index expression
/// is still one callee, and an allocating lowering adds a second.
fn callees(body: &str) -> Vec<String> {
    let mut v: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("call "))
        .map(str::to_string)
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Whether the one callee is the trap: the function that calls `proc_exit`, import 1 in
/// every module this emitter writes. The count alone would accept a body that called
/// the allocator and never checked a bound.
fn only_calls_the_trap(wat: &str, body: &str) -> bool {
    let names = callees(body);
    let [index] = names.as_slice() else {
        return false;
    };
    wat.split("\n  (func ")
        .skip(1)
        .find(|f| f.starts_with(&format!("(;{index};)")))
        .is_some_and(|f| f[..f.find("\n  )").expect("unterminated function")].contains("call 1"))
}

/// `s.xs[i] = 9` loads the `{ptr,len,cap}` header out of the record, checks the bound,
/// stores the element and puts the header back: O(1) per write.
#[test]
fn an_index_assign_through_a_record_field_allocates_nothing() {
    let (wat, body) = module_and_body(
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
        "i64.const 9",
    );
    assert!(
        only_calls_the_trap(&wat, &body),
        "an index assignment through a field must not allocate or copy - a \n         copying desugar would be correct and quadratic; the only function a \n         correct body calls is the trap its bounds checks branch to: \n         {:?}
in:
{body}",
        callees(&body)
    );
    // An empty body would also call nothing.
    assert_eq!(
        body.matches("i64.store").count(),
        1,
        "expected exactly one element store:\n{body}"
    );
}

/// `pop` mutates and returns, so it is hoisted around the statement rather than
/// desugared inside the expression.
#[test]
fn a_pop_through_a_record_field_allocates_nothing() {
    let (wat, body) = module_and_body(
        "type Store = { xs: Array<Int64>, n: Int64 }\n\
         fn take(s: modify Store) -> Int64 {\n\
         let x = s.xs.pop()\n\
         return x ?? -12345\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut s = Store { xs: [1, 2, 3], n: 0 }\n\
         print(take(s))\n\
         print(s.xs.length)\n\
         return 0\n\
         }\n",
        // A sentinel no other function holds: the marker must name exactly one.
        "i64.const 12345",
    );
    assert!(
        only_calls_the_trap(&wat, &body),
        "`pop` through a field must shrink the header in place, not rebuild \n         the array: {:?}
in:
{body}",
        callees(&body)
    );
}

/// `o.i.xs[k] = v` moves the outer field out first and back last: a fixed-size copy,
/// independent of the array's length.
#[test]
fn a_nested_field_chain_allocates_nothing_either() {
    let (wat, body) = module_and_body(
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
        "i64.const 9",
    );
    assert!(
        only_calls_the_trap(&wat, &body),
        "{:?}
in:
{body}",
        callees(&body)
    );
}

/// The fastest of three `vyrn run`s of `src`, which must print `expect`.
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

/// The array lives in a record field or in a local; both do N writes. A copy per write
/// measures about 660x at this N and a move about 1.4x, so 4x sits far from both.
#[test]
fn the_interpreter_does_not_copy_the_array_once_per_write() {
    const N: usize = 32_000;
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();

    // Both build the array as a local: `push` through a field has its own test,
    // and measuring it here would hide this one.
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

/// `t.xs.push(v)` desugars to `t.xs = push(t.xs, v)`. Reading the field into a second
/// `Rc` while the field holds the first makes `push`'s `make_mut` copy the whole vector
/// per append: about 449x at this N, against 1.1x without the copy.
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

/// `rows[i].push(v)` lands in a `Stmt::Store` with an element step, not a field step, with the same
/// hazard: `at` clones the row's `Rc` and `push`'s `make_mut` copies it. One
/// `append_snapshot` serves both. About 438x at this N with the copy, 1.1x without.
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

/// `coerce` must skip an element type that can neither change nor reject a value, or
/// `rows[i][j] = v` pays for the row's length on every store. Two grids with the same
/// 160,000 stores and different row lengths: about 15x with the rebuild, 1.0x without.
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

/// A field write coerces, so a value entering `t.xs: Array<Age>` is checked. The
/// write-back that ends every place desugar skips a value already of the field's type,
/// as the compiled backends do (`validation_required` is `None` when `from == to`);
/// otherwise it re-proves the whole array per store.
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

/// A record carries a runtime name that `coerce` stamps. Stamping a name the
/// value already carries is no work, but it still reads the row, so the ratio is about
/// 1.4x, not 1.0x; a `HashMap` clone per element measures about 9.7x.
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

/// A trap in `vyrn test` is recoverable: the next test runs. Taking the container out
/// of module state would leave a `Val::Unit` behind the failed test, so globals keep
/// the copy.
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

/// A call result has nowhere to put the container back, and silently dropping the write
/// is the worst outcome, so it is refused with the forms that work.
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

/// `Val::Map` sits behind an `Rc`; a bare `MapVal` makes `m[k]` copy the table before
/// the lookup. The maps differ only in size and both do 2,000 reads: about 8.9x with
/// the copy, 1.3x without.
#[test]
fn a_map_read_does_not_copy_the_table() {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let prog = |entries: usize| {
        format!(
            "fn main() -> Int64 {{\n\
             let mut m: Map<String, Int64> = [:]\n\
             let mut i = 0\n\
             while i < {entries} {{ m[\"k\" + i.toString()] = i  i = i + 1 }}\n\
             let mut hits = 0\n\
             let mut r = 0\n\
             while r < 2000 {{\n\
             hits = hits + match m[\"k1\"] {{ Some(v) => v, None => 0 }}\n\
             r = r + 1\n\
             }}\n\
             print(hits)\n\
             return 0\n}}\n"
        )
    };
    let small = best_of_3(&dir, "interp-map-small", &prog(1000), "2000");
    let large = best_of_3(&dir, "interp-map-large", &prog(8000), "2000");
    assert!(
        large.as_secs_f64() < 3.0 * small.as_secs_f64(),
        "a map read is copying the table: {large:?} for 8,000 entries against \
         {small:?} for 1,000 — the same 2,000 reads"
    );
}

/// `Val::Record` sits behind an `Rc`; a bare `HashMap` makes a call copy every field.
/// The records differ only in field count and both read one field: about 6.5x with the
/// copy, 1.0x without.
#[test]
fn passing_a_record_does_not_copy_its_fields() {
    let dir = std::env::temp_dir().join("vyrn-places");
    std::fs::create_dir_all(&dir).unwrap();
    let prog = |fields: usize| {
        let decl: Vec<String> = (0..fields).map(|i| format!("f{i}: Int64")).collect();
        let init: Vec<String> = (0..fields).map(|i| format!("f{i}: {i}")).collect();
        format!(
            "type R = {{ {} }}\n\
             fn peek(r: R) -> Int64 {{ return r.f0 }}\n\
             fn main() -> Int64 {{\n\
             let r = R {{ {} }}\n\
             let mut acc = 0\n\
             let mut i = 0\n\
             while i < 200000 {{ acc = acc + peek(r)  i = i + 1 }}\n\
             print(acc)\n\
             return 0\n}}\n",
            decl.join(", "),
            init.join(", ")
        )
    };
    let narrow = best_of_3(&dir, "interp-rec-narrow", &prog(2), "0");
    let wide = best_of_3(&dir, "interp-rec-wide", &prog(64), "0");
    assert!(
        wide.as_secs_f64() < 3.0 * narrow.as_secs_f64(),
        "passing a record is copying its fields: {wide:?} for 64 fields against \
         {narrow:?} for 2 — the same calls, the same one field read"
    );
}

/// A place through a user container's element is the place its `at` yields,
/// after `at`'s prologue: a field read through a removed handle traps with
/// `at`'s sentence instead of reading a freed slot.
#[test]
fn a_field_read_through_a_dead_slots_handle_traps_in_at() {
    let dir = scratch("dead-handle");
    let file = dir.join("dead.vyrn");
    std::fs::write(
        &file,
        "import { Slots, Handle, newSlots, insert, remove } from \"std/slots\"\n\
         type Node = { value: Int64, next: Option<Handle<Node>> }\n\
         fn nextValue(s: Slots<Node>, h: Handle<Node>) -> Int64 {\n\
         let n = s[h].next.copy()\n\
         return match n {\n\
         Some(k) => s[k].value,\n\
         None => 0,\n\
         }\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut s: Slots<Node> = newSlots()\n\
         let a = insert(s, Node { value: 7, next: None })\n\
         let b = insert(s, Node { value: 2, next: Some(a) })\n\
         print(nextValue(s, b))\n\
         remove(s, a)\n\
         print(nextValue(s, b))\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert_eq!(norm(&out.stdout), "7\n");
    let err = runtime_err(&out.stderr);
    assert!(
        err.starts_with("error: slots: handle is not alive (std/slots.vyrn:"),
        "{err}"
    );
}

/// A record with two columns, a module-state copy, and the writers the
/// witnesses below call inside a loop that reads `c.x`.
const COLUMNS: &str = "type C = { x: Array<Int64>, y: Array<Int64> }\n\
    let mut g = C { x: [1], y: [0] }\n\
    fn grown(c: consume C, v: Int64) -> C {\n\
    let mut x = consume c.x\n\
    x.push(v)\n\
    let y = consume c.y\n\
    return C { x: x, y: y }\n\
    }\n\
    fn widen(c: modify C) {\n\
    c.x.push(9)\n\
    }\n\
    fn bump() {\n\
    g.x.push(1)\n\
    }\n";

/// What `vyrn run` prints for `COLUMNS` and `body`, whose `f` is printed.
fn columns_run(body: &str) -> String {
    let dir = scratch("places-columns");
    let file = dir.join("c.vyrn");
    let main = "fn main() -> Int64 {\nprint(f().toString())\nreturn 0\n}\n";
    std::fs::write(&file, format!("{COLUMNS}{body}{main}")).unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    norm(&out.stdout)
}

/// A loop hoists a column's header only while nothing in it moves the
/// header. Each witness below grows `c.x` in the loop, so a header read once
/// before it would stop the loop after the first turn.
#[test]
fn a_loop_that_rebuilds_the_record_reads_the_new_column() {
    let body = "fn f() -> Int64 {\n\
        let mut c = C { x: [1], y: [0] }\n\
        let mut i = 0\n\
        while i < c.x.length && i < 50 {\n\
        c = grown(c, c.x[i] + 1)\n\
        i = i + 1\n\
        }\n\
        return i + c.x[c.x.length - 1]\n\
        }\n";
    assert_eq!(columns_run(body), "101\n");
}

#[test]
fn a_loop_that_stores_the_column_reads_the_new_column() {
    let body = "fn f() -> Int64 {\n\
        let mut c = C { x: [1], y: [0] }\n\
        let mut s = 0\n\
        let mut i = 0\n\
        while i < c.x.length {\n\
        s = s + c.x[i]\n\
        if i == 0 {\n\
        c.x = [5, 6, 7, 8]\n\
        }\n\
        i = i + 1\n\
        }\n\
        return s\n\
        }\n";
    assert_eq!(columns_run(body), "22\n");
}

#[test]
fn a_loop_that_hands_the_record_to_modify_reads_the_new_column() {
    let body = "fn f() -> Int64 {\n\
        let mut c = C { x: [1], y: [0] }\n\
        let mut s = 0\n\
        let mut i = 0\n\
        while i < c.x.length && i < 20 {\n\
        s = s + c.x[i]\n\
        widen(c)\n\
        i = i + 1\n\
        }\n\
        return s\n\
        }\n";
    assert_eq!(columns_run(body), "172\n");
}

#[test]
fn a_loop_that_calls_a_writer_of_module_state_reads_the_new_column() {
    let body = "fn f() -> Int64 {\n\
        let mut s = 0\n\
        let mut i = 0\n\
        while i < g.x.length && i < 20 {\n\
        s = s + g.x[i]\n\
        bump()\n\
        i = i + 1\n\
        }\n\
        return s\n\
        }\n";
    assert_eq!(columns_run(body), "20\n");
}

/// An element store moves no header, so a loop of them reads each column's
/// header once, before the loop, and an inner loop reuses the outer loop's.
#[test]
fn a_loop_of_element_stores_reads_each_column_header_once() {
    let dir = scratch("places-columns");
    let file = dir.join("c.vyrn");
    let body = "fn f(c: modify C) {\n\
        let mut i = 0\n\
        while i < c.x.length {\n\
        let mut j = 0\n\
        while j < c.y.length {\n\
        c.x[i] = c.x[i] + c.y[j]\n\
        j = j + 1\n\
        }\n\
        i = i + 1\n\
        }\n\
        }\n\
        fn main() -> Int64 {\n\
        let mut c = C { x: [1, 2], y: [10, 20] }\n\
        f(c)\n\
        print(c.x[0] + c.x[1])\n\
        return 0\n\
        }\n";
    std::fs::write(&file, format!("{COLUMNS}{body}")).unwrap();
    let out = vyrn().arg("emit-lowered").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let low = norm(&out.stdout);
    let f = &low[low.find("fn f(").unwrap()..low.find("fn grown(").unwrap()];
    let before = &f[..f.find("loop").unwrap()];
    assert!(before.contains("let @borrow = read c.x\n"), "{f}");
    assert!(before.contains("let @borrow = read c.y\n"), "{f}");
    let run = vyrn().arg("run").arg(&file).output().unwrap();
    assert_eq!(norm(&run.stdout), "63\n");
}
