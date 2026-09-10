//! RFC-0125 M1 — a field write into an array element is one store.
//!
//! `a[i].f = v` reaches every engine as the parser's idiom: the element copied
//! out into an unspellable temp, the field stored on the temp, the temp copied
//! back. The direct wasm backend recognises the idiom on a HEAPLESS element and
//! emits one store through the element's address instead (`elem_field_store`).
//! These tests pin the two halves of that rule by counting `memory.copy` in the
//! emitted function: none for a heapless element, and the idiom's two for an
//! element that holds heap, whose releases the placement accounted for and the
//! peephole must not disturb.
//!
//! The count is the whole claim. RFC-0125 §1.4 measured the copies at 21 per
//! inner iteration of nbody's `advance`, and the same wasm 13x slower under
//! Cranelift than the LLVM native binary because LLVM deletes them and a wasm
//! engine keeps them.

mod common;

use common::{scratch, vyrn};

/// The one function in `src`'s WASM module whose body contains `marker`, as
/// WAT — the `simd.rs` helper, with the same by-content lookup for the same
/// reason: the module carries no name section.
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

/// How many `memory.copy` of exactly `size` bytes `body` holds — the copies of
/// one element, told apart from the 24-byte array-header copies a `let mut a =
/// ps` and a `return a` make once per call.
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
    // `P` is two `Float64`: 16 bytes. Zero copies of that size.
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

/// How many header reads `body` holds. An `Array<T>` header is a pointer and
/// two `i64`s, so the pointer half of every header read is one `i32.load` from
/// an address in a local — `local.get` then `i32.load`. The call-depth counter
/// is read with `i32.load` too, from a constant address (`i32.const` then
/// `i32.load`), on every entry and exit, and is not counted.
fn word_loads(body: &str) -> usize {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    lines
        .windows(2)
        .filter(|w| w[0].starts_with("local.get") && w[1].starts_with("i32.load"))
        .count()
}

/// The read half of RFC-0125 M1: a `while` that indexes a binding it never
/// moves reads the header once, before the loop, not once per access.
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
    // Two accesses per iteration once reloaded the pointer twice; hoisted,
    // the pointer is read once for the whole loop.
    assert_eq!(
        word_loads(&body),
        1,
        "the header is reloaded inside the loop:\n{body}"
    );
    // And the two bounds checks share ONE trap call, after the function's
    // body: the check branches out with its index parked in a local. Each
    // check carrying its own call was what Cranelift charged for — nbody's
    // loop measured 3.56 s with the calls and 1.71 s without them, the
    // compares kept both times. The only other call in `sum` is the
    // call-depth budget's trap in the prologue, so two calls in all.
    assert_eq!(
        body.matches("call ").count(),
        2,
        "a bounds check carries its own trap call:\n{body}"
    );
}

/// The refusal: a loop that grows the array it indexes moves the header, so
/// nothing is hoisted and every access reloads it.
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

#[test]
fn a_field_write_into_an_element_that_holds_heap_keeps_the_idiom() {
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
    // `Q`'s own size is whatever the layout says, so the claim is on the
    // idiom's shape: a copy out and a copy back, two copies of a size other
    // than the header's.
    let elem_copies = body.matches("memory.copy").count() - copies_of(&body, 24);
    assert!(
        elem_copies >= 2,
        "a heap-holding element must still go through the idiom, whose releases \
         the placement accounted for:\n{body}"
    );
}

/// The bytes `body`'s prologue claims from the shadow stack: the `i32.const`
/// between the `global.get 0` and the `i32.sub` that opens every function
/// with a frame. Zero for a body that claims nothing.
fn frame_of(body: &str) -> u32 {
    let lines: Vec<&str> = body.lines().map(str::trim).collect();
    lines
        .windows(3)
        .find(|w| w[0] == "global.get 0" && w[2] == "i32.sub")
        .and_then(|w| w[1].strip_prefix("i32.const "))
        .map(|n| n.parse().expect("a frame size"))
        .unwrap_or(0)
}

/// The second slice of RFC-0125 M1: a literal is built where its consumer
/// wants it. `o`'s slot is the whole frame — the three `Inner` literals and
/// the `Outer` literal are written into it, not built beside it and copied,
/// and the `return` copies `o` to the caller's storage without a slot of its
/// own. Before this the same body needed 144 bytes: 48 for `o` and 96 for the
/// four temporaries a nested literal left in the frame.
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

/// The same slice's other half: a statement's temporaries are the next
/// statement's to reuse. Three calls each need a 16-byte slot for their
/// result; none is live past its statement, so the frame is one slot, not
/// three. Before this the frame was the sum of every temporary a body ever
/// took, which is what put a nine-hundred-statement page body at 23 KB.
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
