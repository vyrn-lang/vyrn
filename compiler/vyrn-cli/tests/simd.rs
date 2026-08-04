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

/// `nearest` is roundTiesToEven, and `llvm.round` is a DIFFERENT function —
/// roundTiesAwayFromZero, which answers `3` for `2.5` where wasm's
/// `f32x4.nearest` answers `2`. The two agree everywhere except at an exact half,
/// so a wrong intrinsic is invisible until a `.5` reaches it; this fails in the
/// default suite instead. `llvm.rint` and not `llvm.roundeven` is a linking
/// choice argued where the declaration is emitted — baseline x86-64 scalarizes
/// `roundeven` to a `roundevenf` the MSVC UCRT does not ship — and the two are
/// the same function under the only rounding mode Vyrn can produce.
#[test]
fn nearest_lowers_to_ties_to_even_and_not_to_ties_away() {
    let body = body_of(
        "fn four(v: F32x4) -> Float32 {\n\
         return F32x4.ceil(v).lane(0) + F32x4.floor(v).lane(1)\n\
         + F32x4.trunc(v).lane(2) + F32x4.nearest(v).lane(3)\n\
         }\n\
         fn main() -> Int64 {\n\
         print(four(F32x4.splat(2.5)))\n\
         return 0\n\
         }\n",
        "four",
    );
    assert!(body.contains("@llvm.ceil.v4f32"), "not ceil:\n{body}");
    assert!(body.contains("@llvm.floor.v4f32"), "not floor:\n{body}");
    assert!(body.contains("@llvm.trunc.v4f32"), "not trunc:\n{body}");
    assert!(body.contains("@llvm.rint.v4f32"), "not ties-to-even:\n{body}");
    assert!(
        !body.contains("llvm.round.v4f32"),
        "`llvm.round` is ties-AWAY and answers 3 for 2.5:\n{body}"
    );
}

/// An `I32x4` comparison is SIGNED, and that is the M3 decision a wrong opcode
/// hides best: `icmp slt` and `icmp ult` agree on every value except across the
/// sign bit, so `min(Int32.min, 1)` is the only place the difference shows.
/// `examples/simdint.vyrn` prints it, but only under `--ignored` parity and only
/// with a clang; this fails in the default suite.
///
/// The wrap is pinned in the same body, and for a related reason. `add <4 x i32>`
/// carries no `nsw`/`nuw`, so `Int32.max + 1` is `Int32.min` — the language's
/// overflow rule at every other width, and what `i32x4.add` does with no choice
/// in the matter. An `nsw` here would make the same expression UB natively and a
/// wrap on wasm: a divergence that shows at exactly one input and nowhere else.
#[test]
fn integer_lane_compare_is_signed_and_the_add_does_not_promise_no_overflow() {
    let body = body_of(
        "fn both(a: I32x4, b: I32x4) -> Int32 {\n\
         if (a < b).anyTrue() { return (a + b).lane(0) }\n\
         return (a - b).lane(0)\n\
         }\n\
         fn main() -> Int64 {\n\
         print(both(I32x4.splat(1), I32x4.splat(2)))\n\
         return 0\n\
         }\n",
        "both",
    );
    assert!(body.contains("icmp slt <4 x i32>"), "not a signed compare:\n{body}");
    assert!(
        !body.contains("icmp ult <4 x i32>"),
        "`ult` is the `U32x4` comparison and answers false for `Int32.min < 1`:\n{body}"
    );
    assert!(body.contains("add <4 x i32>"), "no vector add at all:\n{body}");
    assert!(
        !body.contains("nsw <4 x i32>") && !body.contains("nuw <4 x i32>"),
        "integer vector arithmetic WRAPS; a no-overflow flag makes it UB:\n{body}"
    );
}

/// The wide width's bounds check spans TWO elements, not four (RFC-0083 M4).
///
/// This is the one thing the wider lane genuinely changed, and half of it cannot
/// be seen in output. A span that is too LARGE shows up immediately — the last
/// valid index stops loading, which `examples/simdwide.vyrn` reads. A span that
/// is too small does not show up at all: it accepts every legal program and also
/// accepts a read one element past the end, which is a check that silently stops
/// being one. So the limit is counted here.
///
/// The intrinsics come along for the ride, for the reason the narrow pair is
/// pinned above: `llvm.minnum.v2f64` returns the non-NaN operand and would make
/// native the only engine printing `1.000000` for `min(NaN, 1.0)`.
#[test]
fn the_wide_load_spans_two_elements_and_is_still_checked_once() {
    let body = body_of(
        "fn read(xs: Array<Float64>, i: Int64) -> Float64 {\n\
         let v = F64x2.min(F64x2.load(xs, i), F64x2.splat(1.0))\n\
         return F64x2.max(v, F64x2.sqrt(v)).lane(0) + v.lane(1)\n\
         }\n\
         fn main() -> Int64 {\n\
         let xs: Array<Float64> = [1.0, 2.0, 3.0, 4.0]\n\
         print(read(xs, 0))\n\
         return 0\n\
         }\n",
        "read",
    );
    assert_eq!(checks(&body), 1, "one check for two lanes:\n{body}");
    assert!(
        body.contains("sub nsw i64 %") && body.contains(", 2\n"),
        "the limit is `len - 2`; a `len - 4` refuses the last legal index:\n{body}"
    );
    assert!(
        !body.contains(", 4\n"),
        "a four-element span is the narrow width's, and here it is wrong \
         in both directions:\n{body}"
    );
    assert!(body.contains("@llvm.minimum.v2f64"), "not minnum:\n{body}");
    assert!(body.contains("@llvm.maximum.v2f64"), "not maxnum:\n{body}");
    assert!(body.contains("@llvm.sqrt.v2f64"), "not a vector sqrt:\n{body}");
    assert!(!body.contains("minnum"), "minNum is the wrong rule:\n{body}");
    assert_eq!(
        body.matches("getelementptr double, ptr").count(),
        1,
        "one element address, and it steps by 8 because the element type says so:\n{body}"
    );
}

/// The mask reductions are ONE reduction over the vector, not four lane reads
/// and a branch chain — which is the whole reason they are builtins rather than
/// the Vyrn `||`/`&&` `examples/simdbench.vyrn` prices against them. `-O2` can
/// turn the chain back into a reduce (that is what makes the measured gap only
/// 1.2x on wasm), so an unoptimised body is the only place the difference is
/// visible, and no output ever shows it. Same shape as the intrinsic pin above:
/// a "simplification" back to `extractelement` fails here rather than only under
/// `--ignored` parity.
#[test]
fn the_mask_reductions_are_one_reduce_and_not_four_lane_reads() {
    let body = body_of(
        "fn both(a: F32x4, b: F32x4) -> Bool {\n\
         return (a < b).anyTrue() && (a > b).allTrue()\n\
         }\n\
         fn main() -> Int64 {\n\
         print(both(F32x4.splat(1.0), F32x4.splat(2.0)))\n\
         return 0\n\
         }\n",
        "both",
    );
    assert!(body.contains("@llvm.vector.reduce.or.v4i1"), "not a reduce:\n{body}");
    assert!(body.contains("@llvm.vector.reduce.and.v4i1"), "not a reduce:\n{body}");
    assert!(
        !body.contains("extractelement"),
        "a reduction read lanes one at a time:\n{body}"
    );
}
