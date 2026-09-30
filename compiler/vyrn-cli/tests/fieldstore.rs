//! Pins place-based stores and frames by counting instructions in the
//! emitted WAT. The parser desugars `a[i].f = v` into copy-out, store, copy-back;
//! the core states it as one store into the element's field. The counts matter
//! because a wasm engine keeps the copies LLVM deletes (nbody ran 13x slower).

mod common;

use common::{scratch, vyrn};

/// The one function in `src`'s module whose body contains `marker`, as WAT.
/// Found by content because the module carries no name section.
fn wat_func_containing(src: &str, marker: &str) -> String {
    let dir = scratch("places-wat");
    let file = dir.join("p.vyrn");
    std::fs::write(&file, src).unwrap();
    let out = vyrn()
        .arg("emit-wat")
        .arg(&file)
        .output()
        .expect("vyrn emit-wat");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wat = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let bodies: Vec<&str> = wat
        .split("\n  (func ")
        .skip(1)
        .map(|f| &f[..f.find("\n  )").expect("unterminated function")])
        .filter(|f| f.contains(marker))
        .collect();
    assert_eq!(
        bodies.len(),
        1,
        "expected exactly one function containing `{marker}`, found {}",
        bodies.len()
    );
    bodies[0].to_string()
}

/// A store of `1234567.0` is the marker: no other function in the module holds
/// that constant.
const MARK: &str = "1234567";

/// How many `memory.copy` of exactly `size` bytes `body` holds, so element
/// copies are told apart from the 24-byte array-header copies.
fn copies_of(body: &str, size: u32) -> usize {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    lines
        .windows(2)
        .filter(|w| w[0] == format!("i32.const {size}") && w[1] == "memory.copy")
        .count()
}

#[test]
fn a_field_write_into_a_heapless_element_is_one_store_and_no_copy() {
    let body = wat_func_containing(
        "type P = { x: Float64, y: Float64 }\n\
         fn bump(ps: consume Array<P>, i: Int64) -> Array<P> {\n\
         let mut a = ps\n\
         a[i].y = 1234567.0\n\
         return a\n\
         }\n\
         fn main() -> Int64 {\n\
         let ps: Array<P> = [P { x: 1.0, y: 2.0 }]\n\
         let out = bump(ps, 0)\n\
         print(out[0].y)\n\
         return 0\n\
         }\n",
        MARK,
    );
    // `P` is 16 bytes.
    assert_eq!(
        copies_of(&body, 16),
        0,
        "the element was copied out or back around one field store:\n{body}"
    );
    assert!(
        body.contains("f64.store"),
        "the field store itself is missing:\n{body}"
    );
}

/// How many header reads `body` holds: an `i32.load` of the header's pointer
/// from an address in a local. The call-depth counter's `i32.load` comes from a
/// constant address and is not counted.
fn word_loads(body: &str) -> usize {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    lines
        .windows(2)
        .filter(|w| w[0].starts_with("local.get") && w[1].starts_with("i32.load"))
        .count()
}

#[test]
fn a_loop_that_only_reads_an_array_loads_its_header_once() {
    let body = wat_func_containing(
        "fn sum(xs: Array<Int64>, n: Int64) -> Int64 {\n\
         let mut i = 0\n\
         let mut s = 1234567\n\
         while i < n {\n\
         s = s + xs[i] + xs[i]\n\
         i = i + 1\n\
         }\n\
         return s\n\
         }\n\
         fn main() -> Int64 {\n\
         let xs: Array<Int64> = [1, 2, 3]\n\
         print(sum(xs, 3))\n\
         return 0\n\
         }\n",
        MARK,
    );
    assert_eq!(
        word_loads(&body),
        1,
        "the header is reloaded inside the loop:\n{body}"
    );
    // The two bounds checks share one trap call after the body, with the index
    // parked in a local; a call per check cost nbody's loop 3.56 s against
    // 1.71 s under Cranelift. The other call is the call-depth trap.
    assert_eq!(
        body.matches("call ").count(),
        2,
        "a bounds check carries its own trap call:\n{body}"
    );
}

/// A store into an element moves no header, so the loop still reads it once.
#[test]
fn a_loop_that_stores_into_its_elements_reads_them_through_one_header() {
    let body = wat_func_containing(
        "fn twice(xs: modify Array<Int64>, n: Int64) {
         let mut i = 0
         while i < n {
         xs[i] = xs[i] + xs[i] + 1234567
         i = i + 1
         }
         }
         fn main() -> Int64 {
         let mut xs: Array<Int64> = [1, 2, 3]
         twice(xs, 3)
         print(xs[0])
         return 0
         }
",
        MARK,
    );
    assert_eq!(
        word_loads(&body),
        1,
        "the reads reload the header inside the loop:
{body}"
    );
}

#[test]
fn a_loop_that_grows_the_array_it_indexes_reloads_the_header() {
    let body = wat_func_containing(
        "fn grow(n: Int64) -> Int64 {\n\
         let mut xs: Array<Int64> = [1234567]\n\
         let mut i = 0\n\
         let mut s = 0\n\
         while i < n {\n\
         xs.push(i)\n\
         s = s + xs[i] + xs[i]\n\
         i = i + 1\n\
         }\n\
         return s\n\
         }\n\
         fn main() -> Int64 {\n\
         print(grow(3))\n\
         return 0\n\
         }\n",
        MARK,
    );
    assert!(
        word_loads(&body) >= 2,
        "a loop that pushes into the array must reload its header at each \
         access:\n{body}"
    );
}

/// The field's old value is the store's to release.
#[test]
fn a_field_write_into_an_element_that_holds_heap_is_one_store() {
    let body = wat_func_containing(
        "type Q = { name: String, y: Float64 }\n\
         fn bump(qs: consume Array<Q>, i: Int64) -> Array<Q> {\n\
         let mut a = qs\n\
         a[i].y = 1234567.0\n\
         return a\n\
         }\n\
         fn main() -> Int64 {\n\
         let qs: Array<Q> = [Q { name: \"n\", y: 2.0 }]\n\
         let out = bump(qs, 0)\n\
         print(out[0].y)\n\
         return 0\n\
         }\n",
        MARK,
    );
    // `Q`'s size is the layout's, so count every copy but the header's.
    let elem_copies = body.matches("memory.copy").count() - copies_of(&body, 24);
    assert_eq!(
        elem_copies, 0,
        "a field write copies the element it writes into:\n{body}"
    );
}

/// The bytes `body`'s prologue claims from the shadow stack: the `i32.const`
/// between `global.get 0` and `i32.sub`. Zero for a body with no frame.
fn frame_of(body: &str) -> u32 {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    lines
        .windows(3)
        .find(|w| w[0] == "global.get 0" && w[2] == "i32.sub")
        .and_then(|w| w[1].strip_prefix("i32.const "))
        .map(|n| n.parse().expect("a frame size"))
        .unwrap_or(0)
}

/// A literal is built where its consumer wants it: `o`'s 48-byte slot is the
/// whole frame, and the `return` copies `o` out without a slot of its own.
#[test]
fn a_nested_literal_costs_the_frame_of_its_outermost_value_only() {
    let body = wat_func_containing(
        "type Inner = { a: Float64, b: Float64 }\n\
         type Outer = { p: Inner, q: Inner, r: Inner }\n\
         fn build() -> Outer {\n\
         let o = Outer { p: Inner { a: 1234567.0, b: 1.0 }, q: Inner { a: 2.0, b: 3.0 }, \
         r: Inner { a: 4.0, b: 5.0 } }\n\
         return o\n\
         }\n\
         fn main() -> Int64 {\n\
         let o = build()\n\
         print(o.q.b)\n\
         return 0\n\
         }\n",
        MARK,
    );
    assert_eq!(
        frame_of(&body),
        48,
        "a nested literal took a slot of its own:\n{body}"
    );
    assert_eq!(
        copies_of(&body, 16),
        0,
        "an `Inner` literal was built beside its field and copied in:\n{body}"
    );
}

/// Three calls each need a 16-byte result slot; none is live past its
/// statement, so the frame is one slot, not three.
#[test]
fn a_statements_temporaries_are_given_back_at_its_end() {
    let body = wat_func_containing(
        "type Inner = { a: Float64, b: Float64 }\n\
         fn make(x: Float64) -> Inner {\n\
         return Inner { a: x, b: x }\n\
         }\n\
         fn work() -> Float64 {\n\
         let mut s = 1234567.0\n\
         s = s + make(1.0).a\n\
         s = s + make(2.0).a\n\
         s = s + make(3.0).a\n\
         return s\n\
         }\n\
         fn main() -> Int64 {\n\
         print(work())\n\
         return 0\n\
         }\n",
        MARK,
    );
    assert_eq!(
        frame_of(&body),
        16,
        "a call's result slot outlived its statement:\n{body}"
    );
}

/// A nullary constructor bound by a `let` is built in the binding's slot, as
/// `Some(v)` is: one 16-byte slot, no temporary and no copy into the binding.
#[test]
fn a_let_of_a_nullary_constructor_is_built_in_its_slot() {
    let body = wat_func_containing(
        "fn pick(o: Option<Int64>) -> Int64 {\n\
         return match o {\n\
         Some(v) => v,\n\
         None => 0,\n\
         }\n\
         }\n\
         fn work() -> Int64 {\n\
         let o: Option<Int64> = None\n\
         return pick(o) + 1234567\n\
         }\n\
         fn main() -> Int64 {\n\
         print(work())\n\
         return 0\n\
         }\n",
        MARK,
    );
    assert_eq!(
        frame_of(&body),
        16,
        "a nullary constructor took a slot of its own:\n{body}"
    );
    assert_eq!(
        copies_of(&body, 16),
        0,
        "a nullary constructor was copied into its binding:\n{body}"
    );
}

/// The witnesses of a result left in a `consume` parameter
/// (`vyrn_lower::core::returned_param`). `Cell` is 40 bytes and owns heap, `P` owns none.
const HANDED: &str =
    "type Cell = { a: Float64, b: Float64, c: Float64, d: Float64, tag: String }\n\
    type P = { x: Float64, y: Float64, z: Float64, w: Float64 }\n\
    type Box<T> = { v: T, n: Int64, m: Int64, k: Int64 }\n\
    fn grow(c: consume Cell, v: Float64) -> Cell {\n\
    let mut d = c\n\
    d.a = d.a + v\n\
    d.b = 1234567.0\n\
    d.tag = d.tag + \"+\"\n\
    return d\n\
    }\n\
    fn pick(c: consume Cell, fresh: Bool) -> Cell {\n\
    if fresh {\n\
    return Cell { a: 2345678.0, b: 0.0, c: 0.0, d: 0.0, tag: \"new\" }\n\
    }\n\
    return c\n\
    }\n\
    fn shift(p: consume P, s: Float64) -> P {\n\
    let mut q = p\n\
    q.x = q.x + s\n\
    return q\n\
    }\n\
    fn apply(f: fn(P, Float64) -> P, p: consume P) -> P {\n\
    return f(p, 2.0)\n\
    }\n\
    fn inc<T>(b: consume Box<T>) -> Box<T> {\n\
    let mut c = b\n\
    c.n = c.n + 3456789\n\
    return c\n\
    }\n\
    fn run(n: Int64) -> Float64 {\n\
    let mut c = Cell { a: 1.0, b: 2.0, c: 3.0, d: 4.0, tag: \"t\" }\n\
    let mut i = 0\n\
    while i < n {\n\
    c = grow(c, 7654321.0)\n\
    i = i + 1\n\
    }\n\
    return c.a\n\
    }\n\
    fn main() -> Int64 {\n\
    print(run(3))\n\
    let e = grow(Cell { a: 1.0, b: 2.0, c: 3.0, d: 4.0, tag: \"t\" }, 10.0)\n\
    print(e.a)\n\
    print(e.tag)\n\
    let kept = pick(e, false)\n\
    print(kept.tag)\n\
    print(pick(kept, true).tag)\n\
    let q = apply(shift, P { x: 1.0, y: 0.0, z: 0.0, w: 0.0 })\n\
    let g: fn(P, Float64) -> P = shift\n\
    let r = g(q, 5.0)\n\
    print(q.x)\n\
    print(r.x)\n\
    let mut bx = Box { v: \"s\", n: 0, m: 0, k: 0 }\n\
    bx = inc(bx)\n\
    print(bx.n)\n\
    return 0\n\
    }\n";

/// `grow` hands back `c`: it works in the caller's storage and writes no result.
#[test]
fn a_parameter_handed_back_is_copied_neither_in_nor_out() {
    let body = wat_func_containing(HANDED, MARK);
    assert_eq!(copies_of(&body, 40), 0, "`grow` copied its record:\n{body}");
}

/// `c = grow(c, v)` passes `c`'s own storage and copies nothing back.
#[test]
fn a_call_that_hands_back_its_argument_runs_in_the_arguments_storage() {
    let body = wat_func_containing(HANDED, "7654321");
    assert_eq!(copies_of(&body, 40), 0, "the loop copied `c`:\n{body}");
}

/// `pick` returns a fresh record on one path, so it keeps the out-pointer: it copies
/// its argument in at entry and writes it out at `return c`.
#[test]
fn a_second_result_keeps_the_out_pointer() {
    let body = wat_func_containing(HANDED, "2345678");
    assert_eq!(
        copies_of(&body, 40),
        2,
        "`pick` changed convention:\n{body}"
    );
}

/// `inc<String>` hands back its `Box<String>` as a declared function does.
#[test]
fn a_generic_instance_hands_back_its_parameter() {
    let body = wat_func_containing(HANDED, "3456789");
    assert_eq!(copies_of(&body, 32), 0, "`inc` copied its record:\n{body}");
}

/// The convention moves no output and no release: through a direct call, a
/// specialization over `fn` (`apply`) and a stored `fn` value (`g`), under the free
/// audit.
#[test]
fn a_parameter_handed_back_prints_what_a_copy_prints() {
    let dir = scratch("handed");
    let file = dir.join("h.vyrn");
    std::fs::write(&file, HANDED).unwrap();
    let out = vyrn()
        .arg("run")
        .arg(&file)
        .env("VYRN_LEAK_CHECK", "1")
        .output()
        .expect("vyrn run");
    assert_eq!(
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
        ),
        (
            Some(0),
            "22962964.000000\n11.000000\nt+\nt+\nnew\n3.000000\n8.000000\n3456789\n".to_string()
        ),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A record passed to a `consume` parameter is gone after the call, wherever the
/// callee leaves its result.
#[test]
fn an_argument_handed_back_is_still_consumed() {
    let dir = scratch("handed");
    let file = dir.join("u.vyrn");
    std::fs::write(
        &file,
        HANDED.replace(
            "let kept = pick(e, false)\n",
            "let kept = pick(e, false)\nprint(e.a)\n",
        ),
    )
    .unwrap();
    let out = vyrn().arg("check").arg(&file).output().expect("vyrn check");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("`e.a` is used here but was already consumed by `pick(..)` on line 44"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `x = g(x, x)` reads `x` in the call that takes it. The call runs in a copy of `x`,
/// so `r.a` is the value before the callee's store.
#[test]
fn a_read_beside_the_argument_handed_back_sees_the_old_value() {
    let dir = scratch("handed");
    let file = dir.join("r.vyrn");
    std::fs::write(
        &file,
        "type Cell = { a: Float64, b: Float64, tag: String }\n\
         fn g(r: Cell, c: consume Cell) -> Cell {\n\
         let mut d = c\n\
         d.a = 5.0\n\
         d.b = r.a\n\
         return d\n\
         }\n\
         fn main() -> Int64 {\n\
         let mut x = Cell { a: 1.0, b: 0.0, tag: \"t\" }\n\
         x = g(x, x)\n\
         print(x.b)\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let out = vyrn().arg("run").arg(&file).output().expect("vyrn run");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        "1.000000\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
