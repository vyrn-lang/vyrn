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
