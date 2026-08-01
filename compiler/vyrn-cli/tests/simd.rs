//! RFC-0083 M2: a vector load/store is bounds-checked ONCE for the whole
//! vector, not once per lane.
//!
//! That amortisation is most of what a vector load buys over a scalar loop — a
//! loop cannot amortise its own bounds test — and no output can tell the two
//! apart: four checks and one check accept and reject exactly the same
//! programs, with the same message. Only the emitted code differs. These tests
//! are that half, in the shape of `places.rs`: a structural count, not a
//! duration, so a loaded machine cannot make them flaky.
//!
//! The wasm column has no equivalent pin here — there is no wasm parser in this
//! workspace to count instructions with. What it has instead is
//! `bounds_check_span`'s single `If`, and `examples/simdoob.vyrn` /
//! `examples/simdoobstore.vyrn`, which prove the two branches of the check trap
//! identically on all three engines.

mod common;
use common::*;

/// The body of `fn <name>` in `src`'s emitted LLVM IR.
fn body_of(src: &str, name: &str) -> String {
    let dir = std::env::temp_dir().join("vyrn-simd");
    std::fs::create_dir_all(&dir).unwrap();
    static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = dir.join(format!("{name}-{nth}.vyrn"));
    std::fs::write(&file, src).unwrap();
    let out = vyrn().arg("emit-ir").arg(&file).output().expect("vyrn emit-ir");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ir = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let start = ir
        .find(&format!("@vyrn_{name}("))
        .unwrap_or_else(|| panic!("no `vyrn_{name}` in the emitted IR:\n{ir}"));
    let start = ir[..start].rfind("\ndefine ").expect("no `define` before it") + 1;
    ir[start..start + ir[start..].find("\n}\n").expect("unterminated body")].to_string()
}

/// How many bounds-check traps a body emits — one `fprintf` of `@.trap.aoob`
/// per check, which is the branch's only unambiguous fingerprint.
fn checks(body: &str) -> usize {
    body.matches("@.trap.aoob").count()
}

const PROLOGUE: &str = "fn main() -> Int64 {\n\
                        let xs: Array<Float32> = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]\n\
                        print(read(xs, 0))\n\
                        return 0\n\
                        }\n";

#[test]
fn a_vector_load_is_bounds_checked_once_and_not_per_lane() {
    let body = body_of(
        &format!(
            "fn read(xs: Array<Float32>, i: Int64) -> Float32 {{\n\
             let v = F32x4.load(xs, i)\n\
             return v.lane(0) + v.lane(1) + v.lane(2) + v.lane(3)\n\
             }}\n{PROLOGUE}"
        ),
        "read",
    );
    assert_eq!(checks(&body), 1, "one check for four lanes:\n{body}");
    // And one 16-byte access rather than four 4-byte ones. Counted by the
    // `align 4` the access carries, because the plain `load <4 x float>` spelling
    // also matches the reloads of the binding's own alloca. `align 4` and not 16:
    // the buffer is an array of `float`, so nothing guarantees the alignment a
    // vector would like, and claiming it would be a promise the allocator never
    // made.
    assert_eq!(
        body.matches("load <4 x float>, ptr").count()
            - body.matches("load <4 x float>, ptr %v.addr").count(),
        1,
        "one load from the array, the rest reload the binding:\n{body}"
    );
    assert_eq!(
        body.matches(", align 4").count(),
        1,
        "the one array access is unaligned-safe:\n{body}"
    );
    assert_eq!(
        body.matches("getelementptr float, ptr").count(),
        1,
        "one element address for four lanes:\n{body}"
    );
    // The four `lane` reads add no check of their own — the index is constant
    // and in range by the checker's rule, which is M1's claim still holding.
    assert_eq!(body.matches("extractelement <4 x float>").count(), 4);
}

/// The comparison the milestone rests on: the same four elements read one at a
/// time cost four checks, because a scalar index cannot promise anything about
/// the next one.
#[test]
fn the_same_four_elements_read_scalarly_cost_four_checks() {
    let body = body_of(
        &format!(
            "fn read(xs: Array<Float32>, i: Int64) -> Float32 {{\n\
             return xs[i] + xs[i + 1] + xs[i + 2] + xs[i + 3]\n\
             }}\n{PROLOGUE}"
        ),
        "read",
    );
    assert_eq!(checks(&body), 4, "four scalar reads, four checks:\n{body}");
}

#[test]
fn a_vector_store_is_bounds_checked_once_and_not_per_lane() {
    let body = body_of(
        "fn write(xs: Array<Float32>, i: Int64) {\n\
         F32x4.store(xs, i, F32x4.splat(1.0))\n\
         }\n\
         fn main() -> Int64 {\n\
         let xs: Array<Float32> = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0]\n\
         write(xs, 0)\n\
         print(xs[0])\n\
         return 0\n\
         }\n",
        "write",
    );
    assert_eq!(checks(&body), 1, "one check for four lanes:\n{body}");
    assert_eq!(
        body.matches("store <4 x float>").count(),
        1,
        "one vector store:\n{body}"
    );
}

/// `min`/`max` are IEEE-754-2019 `minimum`/`maximum` — NaN propagates — and the
/// intrinsic is how native was made to agree rather than left to whatever
/// `minps` does. `llvm.minnum` is the OTHER function: it returns the non-NaN
/// operand, and `min(NaN, 1.0)` would print `1.000000` natively and `NaN` on the
/// other two. Parity would catch that, but only under `--ignored` and only with
/// a clang; this catches it in the default suite.
#[test]
fn min_and_max_lower_to_the_nan_propagating_intrinsic() {
    let body = body_of(
        "fn both(a: F32x4, b: F32x4) -> Float32 {\n\
         return F32x4.min(a, b).lane(0) + F32x4.max(a, b).lane(0)\n\
         }\n\
         fn main() -> Int64 {\n\
         print(both(F32x4.splat(1.0), F32x4.splat(2.0)))\n\
         return 0\n\
         }\n",
        "both",
    );
    assert!(body.contains("@llvm.minimum.v4f32"), "not minnum:\n{body}");
    assert!(body.contains("@llvm.maximum.v4f32"), "not maxnum:\n{body}");
    assert!(!body.contains("minnum"), "minNum is the wrong rule:\n{body}");
}
