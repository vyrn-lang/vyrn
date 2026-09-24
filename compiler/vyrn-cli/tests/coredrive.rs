//! RFC-0125 §3 M3, the endgame: the emitter's two walks, compared byte for
//! byte over the corpus.
//!
//! §2.3 says "the emitter reads the core and writes wasm ... it decides
//! nothing". The census before this one counted what the emitter reads and
//! named the blocker — the core stated no OPERATION — and the operation slice
//! wrote the three rows. What was left was a DRIVER: a walk over
//! [`vyrn_lower::core::Body`]'s statements beside the walk over the AST, and
//! `direct.rs` has one now (`Fn_::core_body`).
//!
//! This is its licence and its count in one test. Every corpus program is
//! emitted twice — once with the driver, once with `VYRN_NO_CORE_WALK=1`,
//! which takes the AST walk back — and the two modules are compared. Where the
//! bytes differ, both modules are run and must print and exit the same
//! (RFC-0125 M7): the core gives a local to a value the arm kept on the
//! operand stack, so a byte-identical module stopped being the witness.
//!
//! The unit of selection is the STATEMENT since the interleave slice, so the
//! count beside the licence is per FORM: how many occurrences of each form of
//! the AST dispatch the arm emitted, and how many the core's rows did. An arm
//! goes when nothing reaches it, which is what the count is for. The body
//! count stands beside it: how many of the corpus's bodies the rows carry end
//! to end, out of how many the emitter lowers. The classification below says what each of the rest waits on, and
//! it is the same list §3 M3 records.

mod common;

use std::path::{Path, PathBuf};
use vyrn_frontend::ast::Program;
use vyrn_lower::core::{Body, Callee, Rhs, St};

struct Fs;

impl vyrn_frontend::loader::ModuleResolver for Fs {
    fn read(&self, resolved: &str) -> Result<String, String> {
        std::fs::read_to_string(resolved).map_err(|e| e.to_string())
    }
}

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn load_src(src: &str, root: &str) -> Result<Program, String> {
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(repo_root().join("std").to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    vyrn_frontend::load(src, root, &opts, &Fs).map_err(|d| {
        d.first()
            .map(|d| d.render())
            .unwrap_or_else(|| "load failed".into())
    })
}

fn load(path: &std::path::Path) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    load_src(&src, &path.to_string_lossy().replace('\\', "/"))
}

fn corpus() -> Vec<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(repo_root().join("examples"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no examples found");
    names
}

/// What a body waits on before the core's rows could carry it, hardest first.
///
/// A body's class is the HARDEST thing in it, so the counts partition the
/// corpus and the ranked list in §3 M3 reads straight off them.
const CLASSES: [&str; 5] = [
    "the row names no value (`Val::Lit(Opaque)`)",
    "a lambda body the row does not carry (`Op::Closure`)",
    "a layout: an aggregate made, read or taken",
    "a callee that is no declared function of the program (`Rhs::Call`)",
    "nothing: the rows carry it",
];

/// The two entries of [`vyrn_codegen::direct::FORMS`] this probe tables per
/// program: the exits whose arm is nearest retirement.
///
/// **A zero here is a zero over `examples/` and not over the language**, which
/// is what [`SHAPES`] is for: a shape the corpus does not write is emitted
/// beside it, both ways, and counted into the same table.
///
/// `Stmt::Continue`'s zero was measured over the whole gate list by the
/// occurrence slice and its arm is retired, so the column reads zero here
/// because there is no arm left to reach. `Stmt::Break`'s does not: it stands
/// at 8 over this corpus and at 298 over the gate list, and [`PIN`] names the
/// three programs this walk sees.
const BREAK: usize = 7;
const CONT: usize = 8;

/// Per program: how many `break` and how many `continue` occurrences the AST
/// arm emitted, for every program where either is not zero. The residue the
/// next slice on this line has to empty, program by program and not as one sum.
///
/// The one left is a projection's body INLINED at its caller.
/// `jchain.vyrn`'s is `doc.field("items")[1]`, where the emitter inlines
/// `Json`'s `at` and then inlines `field` from the CLONE of the receiver that
/// expansion holds, so the `break` stands on a node the core never saw; the
/// core would have to inline `a[i]` on a user container too, and the store
/// side of that is `atSet`.
///
/// The other rewrite that put an exit out of the core's reach,
/// `project::iterate_loop`'s clone of a user container's loop body, is off this
/// table since the exits slice — `own::ReleasePlan::key_of` maps a clone back
/// to the node the core keyed.
const PIN: [(&str, usize, usize); 1] = [("jchain.vyrn", 1, 0)];

/// Per program and callee: the projection CALL rows the core still states.
///
/// A projection is inlined at its access site (RFC-0091 M2, RFC-0120), so a
/// [`Callee::Projection`] row is a site the core did NOT inline — its rows
/// are in the projection's own body ([`vyrn_lower::Lowered::places`]) keyed to
/// the projection's own parameters, and at a site those parameters are the
/// caller's expressions, so no row there can stand for this call. That is what
/// the eight `break` occurrences on the AST arm were: the emitter inlines and
/// the core did not.
///
/// The table is EMPTY since the optional slice: the last five rows were the
/// OPTIONAL kind (RFC-0122), whose body splits into four parts at a miss test
/// and whose consumer is an `if let`, and the core states that split at the
/// site too (`Builder::optional_if_let`). A row here again is a site the core
/// stopped inlining.
const CALLS: [(&str, &str, usize); 0] = [];

/// The shapes `examples/` does not write, emitted both ways here so the licence
/// above and the exit count below are the LANGUAGE's and not one directory's.
///
/// Each is loaded from source rather than added to `examples/`, where it would
/// move every corpus census and the wasm manifest.
///
/// The first two are the readers the exits slice attributed `Stmt::Continue`'s
/// arm to, and neither is one. The other five are shapes the driver got wrong
/// and nothing asked: `examples/` writes none of them, and the file that does
/// compiles with no core at all.
const SHAPES: [(&str, &str); 56] = [
    (
        "a `for` over an array literal",
        "fn vyrnTestMain() -> Int64 { let mut s = 0 \
         for i in [0, 1, 2, 3, 4, 5] { if i % 2 == 1 { continue } s = s + i } \
         return s }",
    ),
    (
        "a `continue` under a `region`",
        "fn vyrnTestMain() -> Int64 { let mut n = 0 let mut i = 0 \
         while i < 100 { i = i + 1 \
         region { if i % 2 == 0 { continue } n = n + 1 } } \
         return n }",
    ),
    (
        "a `let` annotated with a `where` type",
        "type Age = Int64 where value >= 18 \
         fn vyrnTestMain() -> Int64 { let mut x = 30 x = x - 25 \
         let a: Age = x return a }",
    ),
    (
        "a store into a binding of a `where` type",
        "type Age = Int64 where value >= 18 \
         fn vyrnTestMain() -> Int64 { let mut a: Age = 20 a = a - 15 return a }",
    ),
    (
        "a `let` of an `if` expression",
        "fn vyrnTestMain() -> Int64 { let x = if 2 > 1 { 10 } else { 20 } return x }",
    ),
    (
        "a match on two string literals",
        "fn vyrnTestMain() -> Int64 { if \"abc\" =~ \"[a-z]+\" { return 1 } return 0 }",
    ),
    (
        "an order on two string literals",
        "fn vyrnTestMain() -> Int64 { if \"abc\" < \"abd\" { return 1 } return 0 }",
    ),
    // The tag family's own witness (RFC-0125 M7). Every `if let` of
    // `examples/` waits on another family as well — a builtin call in the
    // arm, a layout, a release — so the corpus cannot say whether the two
    // walks agree on the switch itself.
    (
        "an `if let` the rows carry",
        "fn vyrnTestMain() -> Int64 { let o = Some(7) let mut t = 0          if let Some(n) = o { t = t + n } else { t = 1 } return t }",
    ),
    // A `for` over a String literal gave the whole body up, so the
    // `continue` in the other loop had no row, and the retired arm failed the
    // build (RFC-0125 M7, `m7-forhead`).
    (
        "a `for` over a String literal beside a `continue`",
        "fn vyrnTestMain() -> Int64 { let xs: Array<Int64> = [1, 2, 3, 4, 5] let mut s = 0          for x in xs { if x % 2 == 0 { continue } s = s + x }          for c in \"abc\" { s = s + Int64(c) } return s }",
    ),
    // A key read is the runtime's lookup into a slot of its own, and the
    // `let` copies it out (RFC-0125 M7). Every key read of `examples/` reads
    // a String value, which the frame does not hold.
    (
        "a map key read the rows carry",
        "fn look(n: Map<String, Int64>, k: String) -> Int64 { let o = n[k] let mut r = 0          if let Some(v) = o { r = v } return r }          fn vyrnTestMain() -> Int64 { let n: Map<String, Int64> = [\"a\": 10, \"b\": 20]          return look(n, \"b\") + look(n, \"c\") }",
    ),
    // A layout that owns no heap is a value, so a `let` of a read of one
    // copies its bytes into the binding's slot (RFC-0125 M7).
    (
        "a copy of a layout that owns no heap",
        "type P = { x: Int64, y: Int64 } type S = { a: P, n: Int64 }          fn pick(xs: Array<P>, i: Int64) -> Int64 { let e = xs[i] return e.x + e.y * 100 }          fn inner(s: S) -> Int64 { let p = s.a return p.x + p.y * 10 + s.n }          fn vyrnTestMain() -> Int64 { let xs: Array<P> = [P { x: 1, y: 2 }, P { x: 3, y: 4 }]          let s = S { a: P { x: 5, y: 6 }, n: 7 } return pick(xs, 1) + inner(s) * 1000 }",
    ),
    // A payload binder of a layout the construct does not own holds the
    // payload's address in the scrutinee's storage: a box for the array, the
    // sum itself for a two-word record (RFC-0125 M7).
    (
        "a payload binder that is a layout",
        "type P = { x: Int64, y: Int64 } fn f(o: Option<Array<Int64>>) -> Int64 { let mut t = 0          if let Some(xs) = o { t = xs[0] + xs.length } return t }          fn g(o: Option<P>) -> Int64 { return match o { Some(p) => p.x + p.y * 10, None => 0 } }          fn vyrnTestMain() -> Int64 { return f(Some([7, 8, 9])) + f(None) + g(Some(P { x: 1, y: 2 })) * 100 }",
    ),
    // An accumulator's append is one `@strAppend` row, and its ownership word
    // is the `let`'s (RFC-0125 M7). `examples/` seeds every accumulator it
    // grows from a literal or a call and never stores into one between
    // appends, so a borrowed seed and a store that clears the word are here.
    (
        "a String accumulator seeded, stored into and borrowed",
        "type R = { name: String, n: Int64 }          fn grow(n: Int64) -> String { let mut s = \"[\" let mut i = 0          while i < n { s = s + i.toString() + \",\" i = i + 1 } return s }          fn tail(r: R) -> String { let mut s = r.name s = s + \"!\" + r.name return s }          fn vyrnTestMain() -> Int64 { let mut s = grow(3) s = s + \"x\" s = \"y\" s = s + grow(2)          let r = R { name: \"ab\", n: 1 } let t = tail(r) return s.byteLength * 100 + t.byteLength }",
    ),
    // A `for` over a String yields each byte as an `Int64` and `s[i]` is a
    // `UInt8` (RFC-0022). The corpus only compares the loop's byte, which
    // runs the same at either type (RFC-0125 M7, `m7-rows`).
    (
        "a `for` over a String whose byte leaves a byte's range",
        "fn sum(s: String) -> Int64 { let mut t = 0 for c in s { t = t + c * 1000 }          return t + Int64(s[1]) } fn vyrnTestMain() -> Int64 { return sum(\"ab\") }",
    ),    // A removal shrinks its receiver in place and hands back what it took
    // (RFC-0125 M7, `m7-rows`). Every `pop` of `examples/` sits in a body
    // the rows do not take for another reason.
    (
        "a pop and a swapRemove the rows carry",
        "type P = { x: Int64, s: String }          fn vyrnTestMain() -> Int64 { let mut ps: Array<P> = [P { x: 1, s: \"a\" }, P { x: 2, s: \"bb\" }]          let q = ps.swapRemove(0) let o = ps.pop() let mut xs: Array<Int64> = [5, 6, 7]          let a = xs.swapRemove(0) let mut t = q.x * 10 + q.s.byteLength + a * 1000          if let Some(p) = o { t = t + p.x * 100 } return t + xs.length * 10000 }",
    ),
    // A scrutinee that is a call's result is the storage the call wrote. Every
    // element of `words(n)` leaves through `w`, so the loop gives back the
    // buffer alone; the deep walk frees `kept`'s strings under `keep`'s caller,
    // and the core walk printed 307 where the arm prints 317 (RFC-0125 M7).
    (
        "a `for` and a `match` over a call result",
        "fn words(n: Int64) -> Array<String> { let mut out: Array<String> = [] let mut i = 0          while i < n { out.push(i.toString() + \"w\") i = i + 1 } return out }          fn keep(n: Int64) -> Array<String> { let mut kept: Array<String> = []          for w in words(n) { kept.push(w) } return kept }          fn pick(n: Int64) -> Option<Array<Int64>> { if n > 0 { return Some([n, n + 1]) } return None }          fn vyrnTestMain() -> Int64 { let kept = keep(3) let again = words(4)          let t = match pick(4) { Some(xs) => xs[1] + xs.length, None => 0 }          let same = if kept[2] == \"2w\" && again[3] == \"3w\" { 1 } else { 0 }          return kept.length * 100 + same * 10 + t }",
    ),
    // A literal nested in a literal is built at its offset in the parent's
    // storage, a named array part is copied there, and an empty array of
    // records has no element to place (RFC-0125 M7).
    (
        "a literal nested in a literal",
        "type In = { a: Int64, b: Int64 } type Out = { i: In, xs: Array<Int64>, k: Int64 }          fn mk(k: Int64) -> Out { return Out { i: In { a: k, b: 2 }, xs: [k, 3], k: k } }          fn bind(k: Int64) -> Int64 { let ys: Array<Int64> = [k] let e: Array<In> = []          let o = Out { i: In { a: 1, b: k }, xs: ys, k: 5 } return o.i.b + o.xs[0] + e.length }          fn vyrnTestMain() -> Int64 { let o = mk(4) return o.i.a + o.xs[1] * 10 + bind(7) * 100 }",
    ),
    // Module state (RFC-0013): a String accumulator reset and grown at its
    // address, and a `match` on a global read by address. `examples/` writes
    // both only in programs the emitter census does not load.
    (
        "module state reset, grown and matched",
        "type Lang = | En | Uk          let mut lang: Lang = En          let mut acc = \"\"          fn grow(n: Int64) -> Int64 { acc = \"\" let mut i = 0          while i < n { acc = acc + \"ab\" + i.toString() i = i + 1 } return acc.byteLength }          fn pick() -> Int64 { return match lang { En => 1, Uk => 2 } }          fn vyrnTestMain() -> Int64 { let a = pick() lang = Uk return grow(3) * 100 + a * 10 + pick() }",
    ),
    // A payload read out of a scrutinee the frame owns and handed to
    // `consume` leaves a hole the scrutinee's release walks around
    // (RFC-0125 M7, `m7-hole`); `std/vyx.vyrn`'s `vyxProcessElem` is one.
    (
        "a payload handed on from a scrutinee the frame keeps",
        "type N = | E(String, Array<Int64>) | T(String) type One = { n: N, k: Int64 }          fn sum(xs: consume Array<Int64>) -> Int64 { let mut t = 0 for x in xs { t = t + x } return t }          fn f(n: consume N) -> One { return match n { E(s, xs) => One { n: T(\"x\"), k: sum(xs) }, T(s) => One { n: n, k: 0 } } }          fn vyrnTestMain() -> Int64 { return f(E(\"a\", [1, 2, 3])).k + f(T(\"b\")).k * 10 }",
    ),
    // A `while` hoists the header of module state no call of the loop stores
    // into, and not one a call replaces: the second is built again once the
    // effect judgment is held (RFC-0125 M7, `m7-statehoist`).
    (
        "a `while` over module state, and one whose call replaces it",
        "let mut xs: Array<Int64> = [1, 2, 3]          fn bump() { xs = [4, 5, 6, 7] }          fn sum() -> Int64 { let mut t = 0 let mut i = 0 while i < xs.length { t = t + xs[i] i = i + 1 } return t }          fn vyrnTestMain() -> Int64 { let mut t = 0 let mut i = 0 while i < xs.length { t = t + xs[i] if i == 0 { bump() } i = i + 1 } return sum() * 100 + t }",
    ),
    // A part is written at its offset in the parent's storage where its own
    // row stands, whether a call, a variant or a literal makes it: a record's
    // or a fixed array's, the caller's where the parent is returned, its
    // offset in a parent of its own, a heap array's buffer, and a variant's
    // box (RFC-0125 M7).
    (
        "a part built at its offset where its own row stands",
        "type P = { x: Int64, y: Int64 } type N = { s: String, k: Int64 } type H = { n: N, p: P, q: P } \
         type B = { a: Int64, b: Int64, c: Int64 } type V = { o: Option<B>, k: Int64 } \
         type G = { h: H, k: Int64 } type J = | JStr(String) | JNum(String) type F = { key: String, value: J } \
         type R = { a: String, m: Map<String, String> } \
         fn mk(k: Int64) -> P { return P { x: k, y: k + 1 } } \
         fn nm(k: Int64) -> N { return N { s: k.toString(), k: k } } \
         fn big(k: Int64) -> B { return B { a: k, b: 2, c: 3 } } \
         fn land(k: Int64) -> H { return H { p: mk(k), n: nm(k), q: mk(k * 2) } } \
         fn bind(k: Int64) -> Int64 { let n = nm(9) let h = H { n: n.copy(), q: mk(k), p: mk(1) } \
         return h.n.k + h.q.y * 10 + h.p.x * 100 + h.n.s.byteLength + n.k } \
         fn arr(k: Int64) -> Array<P> { return [mk(k), P { x: 5, y: 6 }, mk(k + 1)] } \
         fn grow(k: Int64) -> Int64 { let mut xs: Array<N> = [nm(k), nm(k * 3)] xs.push(nm(4)) \
         let fs: Array<P, 2> = [mk(3), mk(k)] return xs[1].s.byteLength + xs.length * 10 + fs[1].y * 100 } \
         fn deep(k: Int64) -> Option<Option<B>> { return Some(Some(big(k))) } \
         fn inrec(k: Int64) -> V { return V { o: Some(big(k)), k: k } } \
         fn boxed(k: Int64) -> Int64 { let v = inrec(k) let r = match v.o { Some(b) => b.a + v.k, None => 0 } \
         return r + match deep(k) { Some(o) => match o { Some(b) => b.c, None => 0 }, None => 0 } } \
         fn fields(k: Int64) -> Array<F> { return [F { key: \"f\", value: JStr(\"f\".copy()) }, F { key: \"k\", value: JNum(k.toString()) }] } \
         fn outer(k: Int64) -> G { return G { h: H { n: nm(k), p: mk(k), q: P { x: 1, y: 2 } }, k: k } } \
         fn inbox(k: Int64) -> Option<H> { return Some(H { n: nm(k), p: mk(1), q: mk(2) }) } \
         fn made(k: Int64) -> Int64 { let fs = fields(k) let g = outer(k) let r = R { a: k.toString(), m: [:] } \
         let b = match inbox(k) { Some(h) => h.q.x, None => 0 } return fs.length + g.h.p.y * 10 + r.a.byteLength * 100 + b * 1000 } \
         fn vyrnTestMain() -> Int64 { let h = land(4) let a = arr(2) \
         return h.p.x + h.q.y * 10 + h.n.k * 100 + bind(7) * 1000 + (a[2].y + grow(5) * 10) * 1000000 + boxed(6) * 100000000000 + made(3) * 10000000000000 }",
    ),
    // A field read out of a root handed to `modify` holds the field's address
    // where no `modify` of the root falls in the read's extent: after the call,
    // or before it in the same turn.
    (
        "a field read beside a `modify` of its root",
        "type P = { src: Array<Int64>, at: Int64 }          fn step(p: modify P) { p.at = p.at + 1 }          fn grow(p: modify P) { p.src.push(p.at) }          fn total(xs: Array<Int64>) -> Int64 { let mut t = 0 for x in xs { t = t + x } return t }          fn after(p: modify P) -> Int64 { step(p) let n = total(p.src) return n + p.at * 100 }          fn turns(p: modify P) -> Int64 { let mut t = 0 let mut i = 0 while i < 3 { t = t + total(p.src) grow(p) i = i + 1 } return t }          fn vyrnTestMain() -> Int64 { let mut p = P { src: [1, 2], at: 5 } let a = after(p) let b = turns(p) return a + b * 1000 + p.src.length * 1000000 }",
    ),
    // A call to a generic function calls the instance the checker solved,
    // from the arguments or from the type the call's result is bound to.
    (
        "a call to a generic function's instance",
        "type Box<T> = { v: T, n: Int64 }          fn wrap<T>(v: consume T) -> Box<T> { return Box { v: v, n: 1 } }          fn empty<T>() -> Array<T> { let xs: Array<T> = [] return xs }          fn size<T>(b: Box<T>) -> Int64 { return b.n }          fn pair<A, B>(a: A, b: B) -> Int64 { return 2 }          fn outer<T>(v: T) -> Int64 { let b = wrap(v.copy()) return size(b) + pair(v, 3) }          fn vyrnTestMain() -> Int64 { let b = wrap(7) let s = wrap(\"ab\") let e: Array<String> = empty()          return size(b) + size(s) * 10 + e.length * 100 + pair(1, \"x\") * 1000 + outer(\"q\") * 10000 + outer(5) * 100000 }",
    ),
    // A scalar handed to `modify` lives in a local, which has no address, so
    // the call spills it to a slot and reloads it after (RFC-0125 M7).
    // `examples/` hands `modify` only layouts.
    (
        "a scalar `modify` argument",
        "fn bump(n: modify Int64, by: Int64) { n = n + by }          fn flip(b: modify Bool) { b = !b }          fn thrice(n: modify Int64) { bump(n, 3) }          fn vyrnTestMain() -> Int64 { let mut x = 1 bump(x, 2) thrice(x)          let mut f = false flip(f) if f { x = x + 100 } return x }",
    ),
    // A `for` over a container it alone owns binds each element at its
    // address in the buffer, and the element leaves by a push or a `consume`
    // or is released there (RFC-0125 M7).
    (
        "an element a `for` hands on out of a container it alone owns",
        "type F = { key: String, n: Int64 }          fn mk(k: Int64) -> Array<F> { return [F { key: k.toString(), n: k }, F { key: \"b\".copy(), n: 2 }, F { key: \"cc\".copy(), n: 3 }] }          fn pick(k: Int64) -> Int64 { let mut out: Array<F> = [] for f in mk(k) { if f.n != 2 { out.push(f) } }          return out.length * 10 + out[0].key.byteLength + out[1].key.byteLength * 100 }          fn eat(f: consume F) -> Int64 { return f.key.byteLength + f.n }          fn sum(k: Int64) -> Int64 { let mut t = 0 for x in mk(k) { if x.n > 2 { t = t + eat(x) } } return t }          fn vyrnTestMain() -> Int64 { return pick(40) + sum(123) * 1000 }",
    ),
    // The element may be any layout: an enum and an array are bound at their
    // address in the buffer as a record is, and leave the same ways.
    (
        "an enum and an array element a `for` hands on",
        "type N = | A(String) | B(Int64)          fn mk(k: Int64) -> Array<N> { return [A(k.toString()), B(k), A(\"cc\".copy())] }          fn size(n: consume N) -> Int64 { return match n { A(s) => s.byteLength, B(k) => k } }          fn keep(k: Int64) -> Int64 { let mut out: Array<N> = [] for n in mk(k) { out.push(n) } return out.length }          fn sum(k: Int64) -> Int64 { let mut t = 0 for n in mk(k) { t = t + size(n) } return t }          fn skim(k: Int64) -> Int64 { let mut t = 0 for n in mk(k) { t = t + 1 } return t }          fn first(k: Int64) -> Int64 { for n in mk(k) { return size(n) } return 0 }          fn rows(k: Int64) -> Array<Array<String>> { return [[k.toString(), \"a\".copy()], [\"bb\".copy()], []] }          fn eat(r: consume Array<String>) -> Int64 { let mut t = 0 for s in r { t = t + s.byteLength } return t }          fn flat(k: Int64) -> Int64 { let mut out: Array<Array<String>> = [] for r in rows(k) { if r.length > 0 { out.push(r) } } return out.length }          fn total(k: Int64) -> Int64 { let mut t = 0 for r in rows(k) { t = t + eat(r) } return t }          fn vyrnTestMain() -> Int64 { return keep(40) + sum(123) * 10 + skim(1) * 10000 + first(5) * 100000 + (flat(7) + total(12) * 10) * 10000000 }",
    ),
    // A String taken out of a field is the pointer the field held, and the
    // release of the record it left walks around the hole (RFC-0125 M7).
    (
        "a String taken out of a field, on one edge and in a loop",
        "type F = { key: String, value: String, n: Int64 }          fn mk(k: Int64) -> F { return F { key: k.toString(), value: \"vv\".copy(), n: k } }          fn fields(k: Int64) -> Array<F> { return [mk(k), mk(k + 1), mk(1)] }          fn one(k: Int64) -> Int64 { let mut out: Array<String> = [] let f = mk(k)          if f.n > 1 { out.push(consume f.key) } return out.length * 100 + f.value.byteLength }          fn all(k: Int64) -> Int64 { let mut keys: Array<String> = []          for f in fields(k) { if f.n != 1 { keys.push(consume f.key) } } return keys.length * 10 + keys[1].byteLength }          fn vyrnTestMain() -> Int64 { return one(5) + one(0) * 1000 + all(99) * 1000000 }",
    ),
    // A `return` and a `break` out of a `for` whose every element leaves
    // through the loop variable release the elements no turn reached
    // (RFC-0125 M7). `examples/` writes only the `return`, in the generated
    // decoders.
    (
        "a `for` left early over the elements it never reached",
        "type Rec = { name: String, k: Int64 }          fn mk(n: Int64) -> Array<Rec> { let mut out: Array<Rec> = [] let mut i = 0          while i < n { out.push(Rec { name: \"r\" + i.toString(), k: i }) i = i + 1 } return out }          fn sink(r: consume Rec) -> Int64 { return r.k }          fn first(n: Int64) -> Int64 { for x in mk(n) { return sink(x) + 10 } return 0 }          fn until(n: Int64, stop: Int64) -> Int64 { let xs = mk(n) let mut t = 0          for x in consume xs { if x.k == stop { break } t = t + sink(x) } return t }          fn vyrnTestMain() -> Int64 { return first(3) + until(5, 3) * 100 }",
    ),
    // A float literal takes its sibling's type: the core walk ran `0.0 - o`
    // at `Float64` into a `Float32` local, which no engine loads.
    (
        "a float literal left of a `Float32` operand",
        "fn neg(o: Float32) -> Float32 { return 0.0 - o }          fn vyrnTestMain() -> Int64 { let x: Float32 = 1.5 if neg(x) < 0.0 { return 1 } return 0 }",
    ),
    // A vector is one wasm `v128`, and its lane-wise operators are rows the
    // walk writes.
    (
        "a lane-wise operator on vectors",
        "fn mix(a: F32x4, b: F32x4) -> F32x4 { return -(a * b + a - b / a) }          fn vyrnTestMain() -> Int64 { let v = mix(F32x4.splat(2.0), F32x4.splat(0.5))          if v.lane(0) < 0.0 { return 1 } return 0 }",
    ),
    // A `match` statement's Unit join is a name no row names.
    (
        "a `match` statement with block arms",
        "fn vyrnTestMain() -> Int64 { let o = Some(3) let mut t = 0          match o { Some(n) => { t = t + n } None => { t = 1 } } return t }",
    ),
    // A lane store's type is the row's, which the emitter decides only as it
    // writes the store.
    (
        "a lane store whose result is discarded",
        "fn vyrnTestMain() -> Int64 { let mut xs: Array<Float32> = [0.0, 0.0, 0.0, 0.0, 0.0]          F32x4.store(xs, 1, F32x4.splat(2.5)) if xs[4] > 2.0 { return 1 } return 0 }",
    ),
    // A host-boundary name is an import the runtime serves, and the core walk
    // calls it through the arm's own emission.
    (
        "a call to a host-boundary extern",
        "extern fn hostNowMillis() -> Int64          fn vyrnTestMain() -> Int64 { let t = hostNowMillis() if t > 0 { return 1 } return 0 }",
    ),
    // A field taken into a literal moves its header to the part's offset,
    // and the root's release carries the hole.
    (
        "a field taken into a part",
        "type In = { d: Array<Int64>, k: Int64 } type Out = { d: Array<Int64>, n: Int64 }          fn mk(k: Int64) -> In { let mut a: Array<Int64> = [] a.push(k) a.push(k + 1) return In { d: a, k: k } }          fn moved(k: Int64) -> Out { let t = mk(k) return Out { d: consume t.d, n: t.k } }          fn kept(k: Int64) -> Int64 { let t = mk(k) let o = Out { d: consume t.d, n: t.k } return o.d.length + o.n }          fn vyrnTestMain() -> Int64 { let o = moved(3) return o.d.length * 100 + o.d[1] + kept(5) * 1000 }",
    ),
    // A `let` of a layout that owns no heap copies it, so a store into the
    // copy leaves the original.
    (
        "a heapless record copied by `let`",
        "type P = { x: Int64, y: Int64 }          fn bump(a: P) -> P { let mut b = a b.x = b.x + 10 return b }          fn vyrnTestMain() -> Int64 { let a = P { x: 1, y: 2 } let b = bump(a) return a.x * 100 + b.x }",
    ),
    // A move is a rename: `zs` and `b` hold the place the moved name held. A
    // layout taken out of a field is a copy into a slot, and so is `let q = p`
    // of a layout that owns no heap (RFC-0125 M7, `m7-move`).
    (
        "a move of a layout, and a take out of a field",
        "type R = { xs: Array<Int64>, s: String, n: Int64 } type P = { x: Int64, y: Int64 }          fn grab(r: consume R) -> Int64 { let t = consume r.s let ys = consume r.xs let mut zs = ys          zs.push(t.byteLength) return zs.length * 100 + zs[3] * 10 + r.n }          fn two(p: P) -> Int64 { let q = p return q.x + q.y * 10 }          fn pair(n: Int64) -> Int64 { let a = [1, n] let b = a return b[1] }          fn vyrnTestMain() -> Int64 { let r = R { xs: [4, 5, 6], s: \"abc\", n: 1 }          return grab(r) * 1000 + pair(2) * 100 + two(P { x: 1, y: 2 }) }",
    ),
    // A store into a nested place is one store into its path: a field of an
    // element that holds heap, an element of a field, and a field of an
    // element of a field (RFC-0125 M7, `m7-move`), and a String stored into
    // a field of an element (`m7-store`).
    (
        "a store into a nested place",
        "type Q = { name: String, y: Int64 } type R = { qs: Array<Q>, ns: Array<Int64>, n: Int64 }          fn bump(qs: consume Array<Q>, i: Int64) -> Array<Q> { let mut a = qs a[i].y = 7 return a }          fn deep(r: consume R) -> R { let mut s = r s.ns[1] = s.n * 2 s.qs[0].y = 3 return s }          fn name(qs: consume Array<Q>) -> Array<Q> { let mut a = qs a[0].name = \"r\" + a[0].y.toString() return a }          fn vyrnTestMain() -> Int64 { let qs: Array<Q> = [Q { name: \"n\" + \"0\", y: 2 }, Q { name: \"m\", y: 1 }]          let out = name(bump(qs, 1)) let d = deep(R { qs: [Q { name: \"a\" + \"b\", y: 0 }], ns: [5, 6], n: 4 })          return out[1].y * 1000 + out[0].name.byteLength * 100 + d.ns[1] * 10 + d.qs[0].y }",
    ),
    // An element that owns no heap, read off a receiver that is no place, binds
    // the receiver to a temporary and releases it after the read (RFC-0125 M7).
    (
        "an element read off a receiver that is no place",
        "import { chars } from \"std/text\"          type P = { at: Int64, n: Int64 }          fn nm(k: Int64) -> String { return \"Ab\" + k.toString() }          fn m(k: Int64) -> Map<String, Int64> { let mut o: Map<String, Int64> = [:] o[\"a\"] = k return o }          fn same(a: UInt8, b: Int64) -> Int64 { if Int64(a) == b { return 1 } return 0 }          fn lead(s: String) -> Bool { return s.byteLength > 0 && bytes(s)[0] >= 'A' && bytes(s)[0] <= 'Z' }          fn vyrnTestMain() -> Int64 { let p = P { at: 2, n: 0 } let b = bytes(nm(7))[p.at]          let mut c = bytes(\"xyz\")[1] c = bytes(nm(3))[1] let d = chars(nm(5))[0]          let e = match m(9)[\"a\"] { Some(v) => v, None => 0 }          let mut t = Int64(b) + Int64(c) * 1000 + Int64(d) * 1000000 + e * 1000000000 + same(bytes(nm(1))[p.at], 49) * 10000000000          if lead(nm(2)) { t = t + 100000000000 } return t }",
    ),
    // A compiled build has no accept loop, so `serveStream` traps with the
    // frontend's sentence and never pulls the stream (RFC-0074 M3a, RFC-0125 M7).
    (
        "a `serveStream` in a compiled build",
        "fn feed(k: Int64) -> Stream<String> { let xs: Array<String> = [k.toString(), \"b\".copy()] return fromArray(xs) }          fn open(k: Int64) -> Int64 { if k > 1 { serveStream(feed(k)) } return k }          fn vyrnTestMain() -> Int64 { return open(1) + open(2) }",
    ),
    // `jsonSchema<T>()` is the String literal its declaration renders, and a
    // name bound to it owns nothing (RFC-0125 M7).
    (
        "a `jsonSchema` bound, stored into and concatenated",
        "type Age = Int64 where value >= 18          type U = { name: String, age: Age }          fn vyrnTestMain() -> Int64 { let s = jsonSchema<U>() let mut t = jsonSchema<Age>() t = t + \"!\"          let n = (s + jsonSchema<U>()).byteLength return n * 1000 + t.byteLength }",
    ),
    // `T?(v)` is a `Ctor::Try` row, and both walks call one check (RFC-0003,
    // RFC-0125 M7): a scalar base, a String base, no `where` clause, and a part.
    (
        "a checked construction the rows carry",
        "type Age = Int64 where value >= 18          type Root = String where value.byteLength > 0          type Any = Int64          fn age(n: Int64) -> Int64 { return match Age?(n) { Some(a) => a, None => 0 - 1 } }          fn root(s: consume String) -> Int64 { if let Some(r) = Root?(s) { return r.byteLength } return 0 - 1 }          fn any(n: Int64) -> Int64 { let o = Any?(n) return match o { Some(a) => a, None => 0 } }          fn some(k: Int64) -> Int64 { let xs = [Age?(k), Age?(k + 10)] let mut t = 0 for x in xs { if let Some(a) = x { t = t + a } } return t }          fn vyrnTestMain() -> Int64 { return age(30) + age(3) * 100 + root(\"abc\".copy()) * 10000 + root(\"\") * 100000 + any(7) * 1000000 + some(12) * 100000000 }",
    ),
    // A method is a call after dispatch: `Callee::Fn` to the impl function the
    // receiver's type names (section 2.1, RFC-0125 M7).
    (
        "a method call on a concrete receiver",
        "type Tally = { counts: Array<Int64>, running: Int64 }          protocol Counting { fn record(modify self, n: Int64) fn sum(read self) -> Int64 }          impl Counting for Tally { fn record(modify self, n: Int64) { self.counts.push(n) self.running = self.running + n } fn sum(read self) -> Int64 { return self.running + self.counts.length } }          protocol Label { fn label(self) -> String }          impl Label for Int64 { fn label(self) -> String { return \"i\" + self.toString() } }          impl Label for Bool { fn label(self) -> String { if self { return \"yes\" } return \"no\" } }          fn both(x: Int64, flag: Bool) -> String { return label(x) + \"/\" + flag.label() }          fn vyrnTestMain() -> Int64 { let mut t = Tally { counts: [], running: 0 } t.record(5) record(t, 7) return t.sum() * 1000 + both(42, true).byteLength }",
    ),
    // A discarded call whose result is a layout lands in a slot of the row's own,
    // given back when the row ends, and a popped record is released (RFC-0125 M7).
    (
        "a discarded removal and a discarded layout result",
        "type P = { x: Int64, y: Int64 }          type Q = { k: Int64 }          fn mk(k: Int64) -> P { return P { x: k, y: k * 2 } }          fn vyrnTestMain() -> Int64 { let mut xs: Array<Int64> = [1, 2, 3, 4] let mut qs: Array<Q> = [Q { k: 1 }, Q { k: 2 }, Q { k: 3 }] let mut ps: Array<P> = [mk(1), mk(2), mk(3), mk(4), mk(5)] let mut i = 0          while i < 2 { xs.pop() qs.pop() ps.swapRemove(0) mk(i) i = i + 1 }          return xs.length * 1000 + qs.length * 100 + ps.length * 10 + ps[0].x }",
    ),
    // `@str` copies each String hole, and the release row the driver placed
    // frees the temporary it copied: a field read, a `+`, a call result and a
    // rendered number (RFC-0125 M7).
    (
        "`@str` of a String temporary",
        "type U = { name: String } fn id(a: String) -> String { return a + \"!\" }          fn show(u: U, n: Int64) -> String { return \"<\\{u.name}|\\{u.name + u.name}|\\{id(u.name)}|\\{n.toString()}>\" }          fn vyrnTestMain() -> Int64 { let u = U { name: \"ab\" } return show(u, 7).byteLength }",
    ),
    // A store into a name of a layout copies the value's bytes into the name's
    // place and releases what the place held where the row says so, in a loop
    // and out of one (RFC-0125 M7, `m7-store`).
    (
        "a store into a name of an array, an enum and a record",
        "type R = { s: String, n: Int64 }          fn mk(n: Int64) -> R { return R { s: n.toString() + \"r\", n: n } }          fn fill(n: Int64) -> Array<String> { let mut xs: Array<String> = [] let mut i = 0          while i < n { xs.push(i.toString()) i = i + 1 } return xs }          fn opt(n: Int64) -> Option<String> { if n > 0 { return Some(n.toString()) } return None }          fn vyrnTestMain() -> Int64 { let mut xs = fill(2) let mut i = 0          while i < 3 { xs = fill(i + 3) i = i + 1 } let mut o = opt(1) o = opt(22)          let mut r = mk(1) r = mk(333) let t = match o { Some(s) => s.byteLength, None => 7 }          o = opt(0) return xs.length * 1000 + xs[4].byteLength * 100 + r.s.byteLength * 10 + t }",
    ),
    (
        "a store into a field of a String, a record, an array and an enum",
        "type P = { x: Int64, s: String }          type R = { name: String, p: P, xs: Array<Int64>, o: Option<String> }          fn mkp(x: Int64) -> P { return P { x: x, s: x.toString() + \"p\" } }          fn mkr() -> R { let p = mkp(1) let xs: Array<Int64> = [1, 2]          return R { name: \"a\", p: p, xs: xs, o: None } }          fn seq(n: Int64) -> Array<Int64> { let mut xs: Array<Int64> = [] let mut i = 0          while i < n { xs.push(i) i = i + 1 } return xs }          fn some(n: Int64) -> Option<String> { return Some(n.toString()) }          fn vyrnTestMain() -> Int64 { let mut r = mkr() let mut i = 0          while i < 3 { r.name = i.toString() + \"n\" i = i + 1 }          r.p = mkp(77) r.xs = seq(3) r.o = some(123)          let t = match r.o { Some(s) => s.byteLength, None => 0 }          return r.name.byteLength * 10000 + r.p.x * 100 + r.p.s.byteLength * 10 + r.xs.length + t }",
    ),
    (
        "a store into an element of a String and a record",
        "type P = { x: Int64, s: String }          fn mkp(x: Int64) -> P { return P { x: x, s: x.toString() + \"p\" } }          fn strs(n: Int64) -> Array<String> { let mut xs: Array<String> = [] let mut i = 0          while i < n { xs.push(i.toString()) i = i + 1 } return xs }          fn ps(n: Int64) -> Array<P> { let mut xs: Array<P> = [] let mut i = 0          while i < n { xs.push(mkp(i)) i = i + 1 } return xs }          fn vyrnTestMain() -> Int64 { let mut ss = strs(3) let mut qs = ps(3) let mut i = 0          while i < 3 { ss[i] = i.toString() + \"ss\" qs[i] = mkp(i * 11) i = i + 1 } ss[1] = \"x\"          return ss[0].byteLength * 1000 + ss[1].byteLength * 100 + qs[2].x + qs[2].s.byteLength * 10 }",
    ),
    (
        "a store into a map key, on a hit and on a miss",
        "type P = { x: Int64, s: String }          fn mkp(x: Int64) -> P { return P { x: x, s: x.toString() + \"p\" } }          fn names() -> Map<String, String> { let mut m: Map<String, String> = [:] return m }          fn counts() -> Map<Int64, Int64> { let mut m: Map<Int64, Int64> = [:] return m }          fn recs() -> Map<String, P> { let mut m: Map<String, P> = [:] return m }          fn vyrnTestMain() -> Int64 { let mut m = names() let mut c = counts() let mut r = recs()          let mut i = 0 while i < 4 { m[(i % 2).toString()] = i.toString() + \"v\" c[i % 3] = i          r[\"k\"] = mkp(i) i = i + 1 } return m.length * 1000 + c.length * 100 + r.length * 10 }",
    ),
    // `std/html`'s `escapeText`: the argument temporary of an `append` is
    // released between the rebuild and the store that puts it back.
    (
        "an append whose argument is released before its store",
        "fn esc(s: String) -> Array<UInt8> { let raw = bytes(s) let mut out: Array<UInt8> = []          for b in raw { if b == '&' { out.append(bytes(\"&amp;\")) } else { out.push(b) } }          return out }          fn vyrnTestMain() -> Int64 { let o = esc(\"a&b&&c\") return o.length * 100 + Int64(o[1]) }",
    ),
    // A rebuild's result that no store puts back holds the receiver's slot.
    (
        "a push whose result is returned, not stored back",
        "fn one(j: Int64) -> Array<String> { let mut out: Array<String> = []          return out.push(j.toString() + \"x\") }          fn two(j: Int64) -> Array<String> { let mut out: Array<String> = []          if j > 0 { return out.push(j.toString()) } return out.push(\"none\") }          fn vyrnTestMain() -> Int64 { let a = one(7) let b = two(0) let c = two(55)          return a[0].byteLength * 100 + b[0].byteLength * 10 + c[0].byteLength }",
    ),
    // A stream is linear (RFC-0081), so the callee disposes a stream it is
    // handed: `keep` hands it back, and a close after the call ended the
    // stream `two` returned.
    (
        "a stream handed to a function that returns it",
        "fn feed(n: Int64) -> Stream<Int64> { let mut xs: Array<Int64> = [] let mut i = 0 while i < n { xs.push(i + 1) i = i + 1 } return fromArray(xs) }          fn keep(s: Stream<Int64>) -> Stream<Int64> { return s }          fn two(n: Int64) -> Stream<Int64> { return keep(feed(n)) }          fn vyrnTestMain() -> Int64 { let mut t = 0 for v in two(3) { t = t * 10 + v } return t }",
    ),
    // A specialization is one instance per target (RFC-0023): a call through
    // a parameter bound to a named function is a call to it, a pass-through
    // names the same instance, and a generic callee is solved by the checker.
    (
        "a call through a `fn` parameter bound to a named function",
        "fn inc(x: Int64) -> Int64 { return x + 1 }          fn twice(x: Int64, f: fn(Int64) -> Int64) -> Int64 { return f(f(x)) }          fn four(x: Int64, f: fn(Int64) -> Int64) -> Int64 { return twice(twice(x, f), f) }          fn mapAll<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> { let mut out: Array<U> = [] for x in xs { out.push(f(x)) } return out }          fn vyrnTestMain() -> Int64 { let xs: Array<Int64> = [1, 2, 3] let ys = mapAll(xs, inc) return four(10, inc) * 100 + ys[2] }",
    ),
    // A stored value is the closure enum's variant for its target (RFC-0037):
    // a function's name returned or stored in a field is a make of it.
    (
        "a function's name stored as a value",
        "fn inc(x: Int64) -> Int64 { return x + 1 }          fn dbl(x: Int64) -> Int64 { return x * 2 }          type Op = { f: fn(Int64) -> Int64, n: Int64 }          fn pick(b: Bool) -> fn(Int64) -> Int64 { if b { return inc } return dbl }          fn vyrnTestMain() -> Int64 { let o = Op { f: dbl, n: 3 } let g = pick(true) let h = o.f return g(10) * 100 + h(o.n) }",
    ),
    // A call's binding is typed by the producer type its row carries, and an
    // extern has no declared signature for the screen to ask (RFC-0125 M7).
    // The store into module state beside it keeps the body off the whole-body
    // walk, so the `let` is the interleave's.
    (
        "a `let` of an extern call beside a statement the arm emits",
        "extern fn hostNowMillis() -> Int64          let mut pending: Map<String, fn(Int64) -> Int64> = [:]          fn stash(cb: fn(Int64) -> Int64) { let t = hostNowMillis() pending[t.toString()] = cb.copy() }          fn vyrnTestMain() -> Int64 { stash(n -> n + 1) return pending.length }",
    ),
    // A `region` statement is its block row, keyed by the block the row names
    // (RFC-0004, RFC-0125 M7). The store into module state keeps the body off
    // the whole-body walk, so each region is the interleave's.
    (
        "a `region` beside a statement the arm emits",
        "let mut pending: Map<String, fn(Int64) -> Int64> = [:]          fn keep(cb: fn(Int64) -> Int64) -> Int64 { let mut n = 0          region { let k = 3 let mut i = 0 while i < k { n = n + i i = i + 1 } }          pending[n.toString()] = cb.copy() region { n = n * 10 } return n }          fn vyrnTestMain() -> Int64 { return keep(x -> x + 1) + pending.length }",
    ),
    // A layout a statement makes and does not bind, a call result read by
    // the statement's call and an `if let`'s scrutinee, lands in a slot of its
    // own, and the run carries its release. The lambda keeps the body off the
    // per-body walk, so each statement is asked alone (RFC-0125 M7,
    // `m7-iflet`).
    (
        "a made layout the statement does not bind",
        "type P = { xs: Array<Int64>, n: Int64 } fn mk(n: Int64) -> P { return P { xs: [n, n + 1], n: n } }          fn total(p: P) -> Int64 { return p.xs[0] + p.xs[1] + p.n }          fn find(p: P) -> Option<Int64> { if p.n > 0 { return Some(p.n) } return None }          fn vyrnTestMain() -> Int64 { let twice: fn(Int64) -> Int64 = x -> x * 2          let a = total(mk(3)) let mut b = 0 if let Some(k) = find(mk(4)) { b = k }          return a * 100 + b * 10 + twice(1) }",
    ),
];

/// What `semantics.rs`'s `run` wraps a shape in, so what is emitted here is the
/// program that test compiles: the answer is printed, which puts a String and
/// its release in the frame the loop sits in.
const WRAP: &str = "fn main() -> Int64 { print(vyrnTestMain().toString()) return 0 }";

/// Per shape: how many `break` and how many `continue` occurrences the AST arm
/// emitted. An arm goes when this table and [`PIN`] both read zero.
const SHAPE_PIN: [(&str, usize, usize); 56] = [
    ("a `for` over an array literal", 0, 0),
    ("a `continue` under a `region`", 0, 0),
    ("a `let` annotated with a `where` type", 0, 0),
    ("a store into a binding of a `where` type", 0, 0),
    ("a `let` of an `if` expression", 0, 0),
    ("a match on two string literals", 0, 0),
    ("an order on two string literals", 0, 0),
    ("an `if let` the rows carry", 0, 0),
    ("a `for` over a String literal beside a `continue`", 0, 0),
    ("a map key read the rows carry", 0, 0),
    ("a copy of a layout that owns no heap", 0, 0),
    ("a payload binder that is a layout", 0, 0),
    (
        "a String accumulator seeded, stored into and borrowed",
        0,
        0,
    ),
    (
        "a `for` over a String whose byte leaves a byte's range",
        0,
        0,
    ),
    ("a pop and a swapRemove the rows carry", 0, 0),
    ("a `for` and a `match` over a call result", 0, 0),
    ("a literal nested in a literal", 0, 0),
    ("module state reset, grown and matched", 0, 0),
    ("a payload handed on from a scrutinee the frame keeps", 0, 0),
    (
        "a `while` over module state, and one whose call replaces it",
        0,
        0,
    ),
    ("a part built at its offset where its own row stands", 0, 0),
    ("a field read beside a `modify` of its root", 0, 0),
    ("a call to a generic function's instance", 0, 0),
    ("a scalar `modify` argument", 0, 0),
    (
        "an element a `for` hands on out of a container it alone owns",
        0,
        0,
    ),
    ("an enum and an array element a `for` hands on", 0, 0),
    (
        "a String taken out of a field, on one edge and in a loop",
        0,
        0,
    ),
    (
        "a `for` left early over the elements it never reached",
        0,
        0,
    ),
    ("a float literal left of a `Float32` operand", 0, 0),
    ("a lane-wise operator on vectors", 0, 0),
    ("a `match` statement with block arms", 0, 0),
    ("a lane store whose result is discarded", 0, 0),
    ("a call to a host-boundary extern", 0, 0),
    ("a field taken into a part", 0, 0),
    ("a heapless record copied by `let`", 0, 0),
    ("a move of a layout, and a take out of a field", 0, 0),
    ("a store into a nested place", 0, 0),
    ("an element read off a receiver that is no place", 0, 0),
    ("a `serveStream` in a compiled build", 0, 0),
    ("a `jsonSchema` bound, stored into and concatenated", 0, 0),
    ("a checked construction the rows carry", 0, 0),
    ("a method call on a concrete receiver", 0, 0),
    ("a discarded removal and a discarded layout result", 0, 0),
    ("`@str` of a String temporary", 0, 0),
    (
        "a store into a name of an array, an enum and a record",
        0,
        0,
    ),
    (
        "a store into a field of a String, a record, an array and an enum",
        0,
        0,
    ),
    ("a store into an element of a String and a record", 0, 0),
    ("a store into a map key, on a hit and on a miss", 0, 0),
    (
        "an append whose argument is released before its store",
        0,
        0,
    ),
    ("a push whose result is returned, not stored back", 0, 0),
    ("a stream handed to a function that returns it", 0, 0),
    (
        "a call through a `fn` parameter bound to a named function",
        0,
        0,
    ),
    ("a function's name stored as a value", 0, 0),
    (
        "a `let` of an extern call beside a statement the arm emits",
        0,
        0,
    ),
    ("a `region` beside a statement the arm emits", 0, 0),
    ("a made layout the statement does not bind", 0, 0),
];

/// The types `Fn_::core_walkable` admits a name of, spelled here so the count
/// beside each class is the emitter's own screen and not a second rule.
fn scalar(t: &vyrn_frontend::ast::Type) -> bool {
    use vyrn_frontend::ast::Type;
    matches!(
        t,
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool
    )
}

fn class_of(body: &Body) -> usize {
    let mut worst = CLASSES.len() - 1;
    for tag in vyrn_lower::core::gaps(body) {
        // The tag is the core's, stated once in `core::gaps`; the ranking is
        // this census's. A tag with no class here is a row shape the core
        // learned to state and nobody ranked.
        let c = match tag.split(':').next().unwrap() {
            "Opaque" => 0,
            "Lambda" => 1,
            "Read" | "Take" | "Make" => 2,
            "Call" => 3,
            other => panic!("the core states a gap this census does not rank: {other}"),
        };
        worst = worst.min(c);
    }
    worst
}

/// Every projection call this body still states, by callee name — the census
/// [`CALLS`] pins.
fn projection_calls(ss: &[St], out: &mut Vec<String>) {
    fn of(r: &Rhs, out: &mut Vec<String>) {
        if let Rhs::Call {
            callee,
            kind: Callee::Projection,
            ..
        } = r
        {
            out.push(callee.clone());
        }
    }
    for s in ss {
        match s {
            St::Let(_, r) | St::Do { rhs: r, .. } => of(r, out),
            St::If { then, els, .. } => {
                projection_calls(then, out);
                projection_calls(els, out);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => projection_calls(b, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    projection_calls(&a.body, out);
                }
            }
            _ => {}
        }
    }
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test coredrive -- --ignored"]
fn the_two_walks_emit_the_same_wasm() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    vyrn_genwasm::install();
    vyrn_lower::install();
    let mut classes = [0usize; CLASSES.len()];
    // Beside each class, how many of its bodies name ONLY the scalar types the
    // driver's own screen admits — RFC-0125 §3 M3, the loop slice's finding.
    // A class's count says what the CORE still owes; this says what writing
    // that row would buy today, because a body the emitter's screen refuses is
    // one the row cannot reach. The two numbers ranked the list differently:
    // the tag on `Arm` is 792 bodies and none of them, because a scrutinee is
    // an enum and an enum is not a scalar.
    let mut scalars = [0usize; CLASSES.len()];
    let mut carried: std::collections::BTreeSet<String> = Default::default();
    let mut bodies = 0usize;
    let mut judged_only = 0usize;
    let mut programs = 0usize;
    let mut from_core = 0usize;
    let mut emitted = 0usize;
    let mut forms = [(0usize, 0usize); vyrn_codegen::direct::FORMS.len()];
    let mut differ: Vec<String> = Vec::new();
    let mut runs_apart: Vec<String> = Vec::new();
    let mut exits: Vec<(String, usize, usize)> = Vec::new();
    let mut calls: std::collections::BTreeMap<(String, String), usize> = Default::default();
    let mut same = 0usize;
    for path in corpus() {
        let Ok(program) = load(&path) else { continue };
        programs += 1;
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        {
            let _memo = vyrn_frontend::project::Memo::open();
            let lowered = vyrn_lower::lower(&program);
            let own = vyrn_frontend::own::analyze(&program);
            for inst in &lowered.instances {
                let Ok(top) = vyrn_lower::core::build(&program, inst, &own) else {
                    continue;
                };
                // M7's measure counts the bodies an emitter reads. A gen body
                // here is judged and emitted by the generator's own compile.
                if inst.judged_only() {
                    judged_only += top.frames().len();
                    continue;
                }
                for body in top.frames() {
                    bodies += 1;
                    let c = class_of(body);
                    classes[c] += 1;
                    if body.names.iter().all(|i| scalar(&i.ty)) {
                        scalars[c] += 1;
                    }
                    if c == CLASSES.len() - 1 {
                        carried.insert(body.name.clone());
                    }
                    let mut names = Vec::new();
                    projection_calls(&body.stmts, &mut names);
                    for n in names {
                        *calls.entry((name.clone(), n)).or_default() += 1;
                    }
                }
            }
        }

        // The two walks over the same program. The lowering is re-run for each
        // so the core's own side tables are the ones that emit answers.
        let core = emit(&program, false);
        let (f, e) = vyrn_codegen::direct::walks();
        from_core += f;
        emitted += e;
        let per = vyrn_codegen::direct::forms();
        for (i, (arm, took)) in per.iter().enumerate() {
            forms[i].0 += arm;
            forms[i].1 += took;
        }
        if per[BREAK].0 > 0 || per[CONT].0 > 0 {
            exits.push((name.clone(), per[BREAK].0, per[CONT].0));
        }
        let ast = emit(&program, true);
        match (core, ast) {
            (Ok(a), Ok(b)) if a == b => same += 1,
            (Ok(a), Ok(b)) => match runs_the_same(&path) {
                Ok(()) => differ.push(format!(
                    "{name}: {} bytes from the core, {} from the AST, and they run the same",
                    a.len(),
                    b.len()
                )),
                Err(e) => runs_apart.push(format!("{name}: {e}")),
            },
            (Err(a), Err(b)) if a == b => same += 1,
            (a, b) => runs_apart.push(format!("{name}: {a:?} against {b:?}")),
        }
    }
    // The same two walks over the shapes the corpus does not write. Their
    // counts stay out of `forms` above, so the corpus totals a record compares
    // against the slice before it are the same measurement.
    let mut shapes: Vec<(&str, usize, usize)> = Vec::new();
    for (what, src) in SHAPES {
        let root = repo_root().join("examples/@shape.vyrn");
        let src = format!("{src}\n{WRAP}\n");
        let program = match load_src(&src, &root.to_string_lossy().replace('\\', "/")) {
            Ok(p) => p,
            Err(e) => panic!("{what} does not load: {e}"),
        };
        let core = emit(&program, false);
        let per = vyrn_codegen::direct::forms();
        let ast = emit(&program, true);
        // Two walks that fail alike are no witness of a shape.
        assert!(core.is_ok(), "{what}: {core:?}");
        if core != ast {
            let dir = common::scratch("coredrive");
            let file = dir.join("shape.vyrn");
            std::fs::write(&file, &src).unwrap();
            if let Err(e) = runs_the_same(&file) {
                panic!("{what}: the two walks' modules run differently\n{e}");
            }
        }
        shapes.push((what, per[BREAK].0, per[CONT].0));
    }

    eprintln!("{programs} programs, {bodies} bodies");
    eprintln!("{judged_only} more bodies the kernel judges and no emitter reads");
    eprintln!("what a body waits on before the core's rows could carry it:");
    for (i, what) in CLASSES.iter().enumerate() {
        eprintln!("  {:6}  {:6} scalar-only  {what}", classes[i], scalars[i]);
    }
    eprintln!(
        "  {} distinct bodies the rows carry end to end",
        carried.len()
    );
    eprintln!("the emitter took the core's walk for {from_core} of {emitted} bodies");
    // The unit of selection is the STATEMENT since the interleave slice, so
    // the count that says what an AST arm still costs is per FORM: how many
    // occurrences the arm emitted, and how many the core's rows did.
    eprintln!("what each form of the AST dispatch still emits:");
    for (i, (what, arm_exists)) in vyrn_codegen::direct::FORMS.iter().enumerate() {
        let (arm, core) = forms[i];
        let retired = if *arm_exists { "" } else { "   (retired)" };
        eprintln!("  {arm:8} the arm   {core:8} the core's rows   {what}{retired}");
    }
    eprintln!("where a `break` or a `continue` still reaches the AST arm:");
    for (name, brk, cont) in &exits {
        eprintln!("  {brk:4} break   {cont:4} continue   {name}");
    }
    eprintln!("{same} of {programs} programs emit the same module either way");
    eprintln!(
        "{} more emit different modules that run the same",
        differ.len()
    );
    for d in &differ {
        eprintln!("  {d}");
    }
    eprintln!("and off the corpus, in the shapes `examples/` does not write:");
    for (what, brk, cont) in &shapes {
        eprintln!("  {brk:4} break   {cont:4} continue   {what}");
    }
    eprintln!("the projection calls the core still states, rather than inlining:");
    for ((program, callee), n) in &calls {
        eprintln!("  {n:4}  {callee}   {program}");
    }
    let named_exits: Vec<(&str, usize, usize)> =
        exits.iter().map(|(n, b, c)| (n.as_str(), *b, *c)).collect();
    assert_eq!(
        named_exits, PIN,
        "a `break` or a `continue` reaches the AST arm somewhere the record does not name"
    );
    let named_calls: Vec<(&str, &str, usize)> = calls
        .iter()
        .map(|((p, c), n)| (p.as_str(), c.as_str(), *n))
        .collect();
    assert_eq!(
        named_calls, CALLS,
        "the core states a projection call somewhere the record does not name"
    );
    assert_eq!(
        shapes, SHAPE_PIN,
        "a shape off the corpus reaches the AST arm a different number of times"
    );
    // A retired form has no arm to reach. The flag in `FORMS` is the schedule's
    // one home and this is what keeps it true over the corpus: a form marked
    // retired whose arm emits anything is an arm that came back.
    for (i, (what, arm_exists)) in vyrn_codegen::direct::FORMS.iter().enumerate() {
        if !arm_exists {
            assert_eq!(forms[i].0, 0, "the retired arm for {what} emitted again");
        }
    }
    // The forms whose arm the rows have started to relieve. An arm goes when
    // its first number reaches zero, and this pin says which eight are on that
    // road: a form that drops off the list has lost a reader the record has to
    // explain, and one that joins it is a slice's own count. The count is per
    // statement, so a form whose whole body the core walk takes leaves it:
    // `Stmt::IfLet` did, with its arm unmoved at 155 (the M7 screen track).
    let carrying: Vec<&str> = vyrn_codegen::direct::FORMS
        .iter()
        .enumerate()
        .filter(|(i, _)| forms[*i].1 > 0)
        .map(|(_, w)| w.0)
        .collect();
    assert_eq!(
        carrying,
        [
            "Stmt::Let",
            "Stmt::Assign",
            "Stmt::Return",
            "Stmt::If",
            "Stmt::Expr",
            "Stmt::While",
            "Stmt::Break",
            "Stmt::Continue"
        ],
        "the forms the core's rows carry are not the ones the record names"
    );
    // The driver is a screen and not a judgement: where it stands down, the
    // AST walk emits exactly what it did. So a body it takes has to reach the
    // corpus at all, or this test measures nothing.
    assert!(from_core > 0, "the core walk emitted no body");
    // The licence. A program whose two modules differ runs the same under
    // both: same stdout, stderr and exit code. Two shapes make the bytes
    // differ. The core names every value, so it gives a local to the values
    // the arm kept on the stack (RFC-0125 M7); and it writes a `return`
    // per arm where the arm joins a `match` or an `if` and returns once
    // (`Builder::return_through`).
    assert!(
        runs_apart.is_empty(),
        "the two walks' modules run differently:\n{}",
        runs_apart.join("\n")
    );
}

/// Whether the module each walk emits for `file` runs the same: `vyrn run`
/// with and without `VYRN_NO_CORE_WALK=1`, under the corpus's conventions
/// (`common::run_io`). `Err` names the first stream that differs.
fn runs_the_same(file: &Path) -> Result<(), String> {
    let run = |ast: bool| {
        let mut cmd = common::vyrn();
        cmd.arg("run").arg(file);
        cmd.args(common::read_args(&file.with_extension("args")));
        if ast {
            cmd.env("VYRN_NO_CORE_WALK", "1");
        } else {
            cmd.env_remove("VYRN_NO_CORE_WALK");
        }
        common::run_io(cmd, &common::examples_dir(), &file.with_extension("stdin"))
    };
    let (core, ast) = (run(false), run(true));
    let (c, a) = (core.status.code(), ast.status.code());
    let mut why = String::new();
    if c != a {
        why = format!("exit {c:?} from the core, {a:?} from the AST\n");
    }
    for (stream, x, y) in [
        ("stdout", &core.stdout, &ast.stdout),
        ("stderr", &core.stderr, &ast.stderr),
    ] {
        let (x, y) = (common::norm(x), common::norm(y));
        why += &common::first_diff(stream, "core", &x, "AST", &y).unwrap_or_default();
    }
    if why.is_empty() {
        Ok(())
    } else {
        Err(why)
    }
}

fn emit(program: &Program, ast: bool) -> Result<Vec<u8>, String> {
    if ast {
        std::env::set_var("VYRN_NO_CORE_WALK", "1");
    } else {
        std::env::remove_var("VYRN_NO_CORE_WALK");
    }
    let _memo = vyrn_frontend::project::Memo::open();
    let _lowered = vyrn_lower::lower(program);
    vyrn_codegen::direct::forget_walks();
    let out = vyrn_codegen::direct::compile(program);
    std::env::remove_var("VYRN_NO_CORE_WALK");
    out
}
