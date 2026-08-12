# Review: the code generation backends, five lenses

An external review of Vyrn's two codegen backends at `52e462f`: the textual
LLVM-IR backend (`compiler/vyrn-codegen/src/lib.rs`, the reference native
path), the direct wasm backend (`compiler/vyrn-codegen/src/direct.rs`,
RFC-0077), and the layer they share (`layout.rs`, `toolchain.rs`, `wasm.rs`).
Five reviewers, each with its own values. Read-only; nothing was fixed.

Every finding carries evidence. A code finding cites `file:line`. A
behavioural finding carries a program that was run, with output recorded from
all three engines where they build. Findings are ranked **CONFIRMED**
(reproduced or measured) above **PLAUSIBLE** (argued from reading, not run).
Where an RFC records a decision with its argument, the entry says "design
critique", not "defect".

A separate review already covered the C shim and the prelude, and another
covered a first wave of backend findings (non-reproducible wasm builds from
`HashSet` order, the statics `assert!`, layout multiply overflow, the missing
v128 layout cases, several shim leaks); neither set is re-derived here.

Every behavioural claim below was verified against a binary built from this
branch's own tree, after an earlier run against a stale binary gave one wrong
answer (a call-depth probe passed where the fresh binary traps).

---

## Top 10 by severity

| # | Severity | Lens | Finding | Ref |
|---|---|---|---|---|
| 1 | **Critical** | Agda | **The textual backend's symbol mangle is not injective, and the driver dedups instantiations by symbol.** A generic instantiated at `Option<Int64>` and at a user type named `OptInt64` emits ONE body under one symbol; `vyrn check` says ok, interp and wasm print `9`, native prints `1948444778241`. The direct backend's own comment (`direct.rs:716`) documents this exact hole — in the sibling it did not fix. | G4.1 |
| 2 | **Critical** | Agda / C | **A `match` arm or `if let` that lets the payload out of the arm double-frees on both compiled backends.** `let s = match o { Some(v) => v, None => "" }` — the everyday unwrap-or — aborts native with no output; under stress wasm's free list corrupts and traps OOB. String and Array payloads both. Interp alone is right. | G4.2 |
| 3 | High | Agda | **The direct backend's entire shadow stack is one wasm page.** A 7,000-element array literal compiles in 0.1 s to a module that traps `out of bounds memory access` at a wild address; recursion with an aggregate local dies the same way at depth ~300, breaking the shared `call depth exceeds 1000` contract the engines just adopted. No compile-time check, even for a single frame larger than the whole stack. | G4.3 |
| 4 | High | Agda / Linus | **`vyrn check` accepts an array literal native cannot build.** 100,000 constant elements: check 0.1 s ok, native runs clang for 2 m 19 s and dies `LLVM ERROR: out of memory` — because the literal lowers to N chained `insertvalue` instructions over an `[N x i64]` SSA aggregate. Combined with #3, a 100k literal runs on NO compiled backend. | G4.4, G1.1 |
| 5 | Medium | PL | **The runtime trap wordings are duplicated as hand-written literals in both backends** (`lib.rs:1161` vs `direct.rs:12244`), while `IO_MESSAGES`, `validation_message` and `CALL_DEPTH_LIMIT` are shared constants. Half the parity-load-bearing strings have a single source; the other half have two. | G5.1 |
| 6 | Medium | PL | **The calling convention exists only as code.** `llt_of` is the one shared layout function (good); how a value is passed — by value, by shadow-stack address, `modify` copy-back — is implied by `signature`/`wasm_sig` in one backend and `function` in the other, with no written statement to check either against. | G5.2 |
| 7 | Medium | PL | **The parity gate spans the corpus, not the language.** It discovers examples by glob (good) — but findings #1–#4 are all shapes no example reaches: the corpus matches on call results, never on owned locals; declares no type whose name collides with a mangle prefix; writes no aggregate frame near 64 KB. | G5.3 |
| 8 | Medium | C systems | **A statement-position `match` on a heap temporary never frees it** — the sibling `if let` does. `let d = match makeResult(i) { … }` in a 3M loop: native peak 141.6 MB against 3.4 MB for the same loop written `if let`. A pure leak, both compiled backends. | G2.1 |

Counts: **8 CONFIRMED** (G4.1–G4.4, G1.1, G2.1, G5.1, G5.3), the rest
PLAUSIBLE from reading, cited in each lens.

---

## Lens 1 — Linus Torvalds: taste and performance

### G1.1 CONFIRMED — High. A constant array literal is N chained `insertvalue` over an N-wide aggregate

`vyrn emit-ir` on `let xs = [0, 1, 2, …]` (7,000 constant elements):

```llvm
%xs.addr7000 = alloca [7000 x i64]
%t0 = insertvalue [7000 x i64] undef, i64 0, 0
%t1 = insertvalue [7000 x i64] %t0, i64 1, 1
%t2 = insertvalue [7000 x i64] %t1, i64 2, 2
...
```

One SSA value of the full aggregate type per element, 7,000 of them, then one
store. At 100,000 elements clang's `-O2` pipeline (`early-cse` on
`vyrn_main`, per its own crash dump) allocates until it dies:

```
$ vyrn check p20_biglit.vyrn        # 0.104 s, ok
$ vyrn build p20_biglit.vyrn -o p20.exe
LLVM ERROR: out of memory
Allocation failed
...
real  2m18.746s
```

The direct wasm backend compiles the same file in **0.100 s** (and then hits
finding G4.3 at run time). The interpreter prints the right answer.

A constant literal of a scalar type belongs in a `private constant` global
plus a copy — the string pool a few hundred lines away already does exactly
that for string literals.

### G1.2 CONFIRMED — footnote. The 50k-term constant chain

`fn main` containing `let s = 1 + 1 + … + 1` (50,000 terms): `vyrn check`
19.1 s, `emit-ir` 25.2 s (so ~6 s of that is the backend), and no constant
folding — 50,000 `add` instructions for a constant the frontend knew. The
19 s is the checker's, so it is out of this review's scope, but the backend
inherits the shape: `check` does not predict `build` cost in either
direction. Recorded as context for G4.4, not ranked.

### G1.3 PLAUSIBLE — Medium. The instantiation worklist is quadratic in distinct instantiations

`lib.rs:1511` dedups a discovered instantiation by re-mangling the whole
pending queue:

```rust
if !emitted.contains(&m) && !queue.iter().any(|(qn, qa)| mangle_name(qn, qa) == m) {
```

`mangle_name` allocates, and this runs per discovered instantiation, so it is
O(insts²) mangles. The HO drain has the same shape (`lib.rs:698`), and the
direct backend keys its cache by a linear scan of a `Vec<Pending>` on every
call site (`direct.rs:1034`, `self.mono.borrow().insts.iter().find(|p| p.key
== key)`), where a `HashMap<Key, Sig>` is sitting right there.

Measured, distinct generic instantiations (growing array literals), `emit-ir`
net: 60 / 114 / 334 / 1,237 ms for n = 200 / 400 / 800 / 1,600 — ~3.4×
per doubling. Ranked PLAUSIBLE not CONFIRMED because `emit-ir` includes the
checker's own per-instantiation cost (the prior audit's L1.1 already found it
quadratic), and this probe cannot separate the backend's mangle-scan from the
checker's clone. The code at `lib.rs:1511` is quadratic on its face; its share
of the measured curve is the unproven part. (Reported independently by the
Rust-lens reading sweep, items 17/18/19.)

**Fix.** A `HashSet<String>` of mangled names beside the queue; a
`HashMap<Key, Sig>` in `Cx::mono`.

---

## Lens 2 — C systems programmer: resource discipline in the emitted code

The confirmed core of this lens turned out to be G4.2 — the double free is a
resource-discipline defect first and a parity break second; it is written up
under the Agda lens because the three-engine disagreement is what proves it.
G2.1 below is its mirror: the same missing rule, in the direction that leaks
instead of double-freeing.

### G2.1 CONFIRMED — Medium. A statement-position `match` on a heap temporary leaks it every iteration

The `if let` form frees a match scrutinee that is a heap temporary; the
`match` form does not. `own::analyze` writes a statement row for
`Stmt::IfLet` and `Stmt::ForIn` (`own.rs:1084, 1117`) but an `Expr::Match`
scrutinee gets none (`own.rs:1200-1207` descends only into lambdas), so
neither backend pushes a drop for it (`lib.rs:5729-5743`,
`direct.rs` match lowering). The interpreter reference-counts, so it does not
leak.

**Repro** — two files differing only in `match` vs `if let`, 3,000,000
iterations, native, peak working set measured:

```vyrn
// p34_matchleak.vyrn
while i < 3000000 {
    let d = match makeResult(i) {   // makeResult -> Result<String, String>
        Ok(s) => s.byteLength,
        Err(e) => e.byteLength,
    }
    c = c + d
    i = i + 1
}
```

| form | peak | output |
|---|---|---|
| `match` (above) | **141.6 MB** | `total=28888890` |
| `if let Ok(s) = makeResult(i) { … }` | **3.4 MB** | correct |

Both print the right number; one keeps every scrutinee's String forever.
The wasm build leaks the same bytes (it completes here because 144 MB fits
its growable memory). The parity harness compares stdout and cannot see it.

**Fix.** Give `Expr::Match` the same scrutinee-ownership row `Stmt::IfLet`
already has in `own::analyze`.

### PLAUSIBLE — drop-placement shapes from the reading sweep

Reported by the drop-placement reading pass; each names a control-flow shape
whose release one backend forgets or the two disagree on. Not reproduced.

- **G2.2 — the in-region reassignment guard is on one backend only.** `lib.rs`
  `slot_owns` carries a `region_depth == 0` guard (`lib.rs:4344-4347`);
  `direct.rs` `place_owns` (`direct.rs:2523-2525`) has none, so every rule-4
  snap site (`direct.rs:2836, 2883, 3264`) snaps-and-frees a replaced value
  inside a region on wasm while native deliberately stands aside. Guaranteed
  emitted-code divergence (native leaks the buffer, wasm frees it); a double
  free if any store of a region-allocated value past the escape guard reaches
  it. *Probed the reachable half:* the escape guard refuses the obvious route
  (`cannot store a heap value into 's', which outlives the enclosing region`),
  and an in-region reassignment of an in-region binding runs clean and
  identical on all three engines over 20,000 iterations — so the divergence is
  real in the emitted code but I could not drive it to an observable double
  free from valid source. PLAUSIBLE, latent.
- **G2.3 — `if let` scrutinee holes are recorded but never consumed.**
  `own.rs:1092-1096` inserts holes for the `IfLet` statement, but the
  holes_map→hole_slots wiring exists only in `Stmt::Let` (`lib.rs:4484-4486`,
  `direct.rs:2782-2787`); `IfLet` lowering reads only `droppable`. `ForIn`
  refuses droppable when holes are non-empty (`own.rs:1129-1132`); `IfLet`
  does not — the asymmetry is the tell. If a `consume` can land a hole on an
  if-let scrutinee, the Deep walk frees the taken place twice.
- **G2.4 — the `?` propagate path skips pending argument-temporary frees.**
  `gen_try`'s propagate arm runs `emit_all_drops` (drop_stack only) then
  `ret` (`lib.rs:6453-6459`); the call's `arg_frees` drain lands in `try.ok`
  (`lib.rs:8292-8302`), so `f("n" + 1.toString(), o?)` with `o == None` leaks
  the concat. Direct backend identical (`direct.rs:10470-10472` vs
  `direct.rs:5739-5744`).
- **G2.5 — the String self-append fast path never drains `arg_frees`.** A
  call-shaped operand of `s = s + label(i)` is parked in `arg_frees` with no
  enclosing mark and survives to function end unfreed (`lib.rs:4501-4519`;
  `direct.rs:2821-2829`). A per-iteration leak in `s = s + label(i)`.
- **G2.6 — a String `+` feeding a comparison is freed by nobody.**
  `free_str_temp`/`tee_str_temp` fire only under the `Add` arm
  (`lib.rs:6639-6647`, `direct.rs:5144-5156`); `("a" + x) == y` is neither an
  Add operand nor an `arg_drops` row (`own.rs:766-772` records operands only
  under `@concat`/call positions). Per-evaluation leak, both backends.

What this lens verified by running programs (CLEAN):

- **`break`/`continue` with owned Strings between the loop header and the
  exit** — nested loops, an accumulator, `continue` at i%4, `break` from an
  inner loop and an early `break` from the outer: byte-identical across all
  three engines.
- **`for x in consume xs { if …break }`** — the RFC-0095 M3 shape: identical
  output, no crash, on all three.
- **`?` propagation with a `modify` parameter outstanding** — the copy-out
  runs before the early return (`gen_try` emits `emit_all_drops` then
  `emit_modify_copyout`, `lib.rs:6457-6459`; behaviourally: `xs` keeps the
  push that preceded the failing `?`, on all three engines).
- **Early `return` from inside `region`, and nested regions** — correct and
  identical, including the byte counts of arena-built strings.
- **`panic` with a live droppable local** — canonical
  `error: too big: 9 (file:line)` on all three engines.

The reading pass additionally traced and found CLEAN, with cites: the
`break`/`continue` boundary walk (both backends set `drop_boundary` before the
body frame and `emit_drops_above`/`emit_releases_above` clone without popping,
`lib.rs:4140-4148`, `direct.rs:1954-1968`); regions on break/continue vs
return/`?` (freed on the loop exit, popped-not-freed on return — documented
safe leak, backends agree); `?` vs `return` drop parity (both emit
`emit_all_drops` + `emit_modify_copyout` before `ret`); the modify copy-out on
every non-trap exit; stream close-on-every-exit exactly once; the
reassignment snap (`x = f(x)`/`x = x` take no snap via the shared
`mentions_place` guard → recorded leak never double free; `x = y` snaps old
`x` once and marks `y` Moved); loop-variable frames (element is a borrow, its
frame stays empty); `join`/`spawn` release order (RFC-0095, consume-once
compile-checked, no double release found).

---

## Lens 3 — Rust reviewer: the backend code itself

The reading sweep's judgement: no panic traces to a valid checked program;
the real exposure is the stringly-typed layer and the quadratic worklists
(G1.3). All PLAUSIBLE — read, not driven to a crash.

**Panics — all invalid-IR-only or structurally unreachable.**
- **G3.1** `lib.rs:5751, 5758` — `gen_match` over Option/Result does
  `arms.iter().find(|a| pattern_is_one(&a.pattern)).unwrap()` and its negated
  twin. The invariant (exactly one tag-1 and one tag-0 arm) is the checker's;
  any future relaxation (a wildcard arm, a single-arm match) panics here
  instead of erroring. Reachable only from invalid IR today.
- **G3.2** `wasm.rs:100-114` — `boundary()` compiles a dummy program through
  the TEXT backend and string-parses its emitted IR
  (`rest.split_once(" @").expect("declare RET @NAME(..)")`) inside a
  `OnceLock`. Any drift in `lib.rs`'s `declare` formatting is a process panic
  on every subsequent wasm build — a cross-file format coupling whose only
  detector is the panic. Same shape in `declare_sig` (`wasm.rs:135-137`). This
  is the same defect as G3.10 (the import boundary recovered by parsing IR).
- **G3.3** `lib.rs:11171` `decl.predicate.clone().expect("predicate present")`
  — both callers filter on predicate presence; invalid-IR-only.
- Locally-paired, unreachable: `lib.rs:3840/4104/4487/6129` scope+drop_stack
  `last_mut`/`pop`; `direct.rs:2790` `.expect("a let outside any block")`;
  `direct.rs:4340` guarded by `c.len() == 1`; `direct.rs:5879`
  `Num::of(it).unwrap()` guarded by its match (evaluated twice — style).

**Integer casts — bounded by the checker, no single choke point.**
- **G3.4** `direct.rs:8987` `b.alloc(stride * elems.len() as u32, …)` — the
  same u32-multiply class as the dispatched `direct.rs:9026`, on array-literal
  element counts; `n` is source-bounded, effectively unreachable.
- **G3.5** `direct.rs` pervasive `l.size as i32` / `I32Const(off as i32)` —
  layout sizes are checker-bounded (SmallArray `N ≤ 64` at `checker.rs:2231`,
  ArrayN = literal length), fine today, but the bound lives in many heads with
  no single assertion that `size ≤ i32::MAX`. Hygiene note.

**Errors swallowed — all documented policy, CLEAN.** `lib.rs:4271/4305/4323`
`let _ = self.deep_release(...)` in `emit_drop` ("a drop this cannot emit is a
leak, never a wrong free"); `direct.rs:4380-4388` `if let Ok(t) =
self.peek_arm(...)` (re-checked downstream); `toolchain.rs` `.ok()?` discovery
chains (absence is a legitimate answer).

**The stringly-typed layer (the theme).**
- **G3.6 CONFIRMED-by-inspection** — the direct backend's type representation
  IS an LLVM type string: `layout::of_ll(&cx.ll(ty))` round-trips at
  `direct.rs:418, 1777, 3521, 3714, 7068, 7715, 8119, 10015`, and
  aggregate-ness is decided by `ll.starts_with('{') || ll.starts_with('[')`
  (`direct.rs:1098`). Deliberate single-source-of-truth (the layout.rs doc
  argues it), but every query pays a format+parse and the "enum" is a char
  prefix. This is the mechanism finding G4.1 exploits from the mangle side.
- **G3.7** `lib.rs:12451` `mangle_dispatch_sym` hashes `format!("{sig:?}")` —
  derived `Debug` output as the canonical structural form of a type. Any
  `Debug`-affecting change to `Type` silently changes every dispatcher symbol.
- **G3.8** ownership facts keyed by AST node address `stmt as *const Stmt as
  usize` (`lib.rs:4433` and 5 more; `direct.rs:2763` and 5 more). Sound only
  while the exact `&Program` from `own::analyze` reaches codegen unmoved; a
  future clone/transform between the two silently misses the map and drops
  stop being emitted — a leak with no diagnostic and no type-system backing.
  This is the invariant every drop finding above ultimately rests on.
- **G3.9** `lib.rs` places are raw strings; `slot_owns` tests
  `slot.starts_with('@')` to mean "global" (`lib.rs:4346`). `direct.rs`
  already has a `Place` enum; the text backend never got one.

**Duplication.**
- **G3.10** `wasm.rs:97-130` recovers the runtime import boundary by parsing
  the text backend's IR (see G3.2). A `Vec<(name, Sig)>` table both backends
  read would delete the parser and the panic. (Priority fix — removes a panic
  and a parser at once.)
- **G3.11** `direct.rs:633-643` re-implements LEB (`leb()`) inside
  `export_returns` while the crate already depends on `wasm-encoder` (which
  encodes LEB) and `wasm.rs` decodes it at `:843`. Ten dead lines.
- **G3.12** SIMD builtin dispatch tables duplicated: name→(element, span)
  prefix-matching on `"@i32x4"`/`"@f64x2"` at `lib.rs:8499, 8686` and
  `direct.rs:6264, 6459`, both with `ends_with("Load")` splits. Emission
  differs (text vs instructions), so only the 3-row name→lane table could
  share.

One item verified directly: the `Let` lowering (`lib.rs:4423-4490`) keys
ownership decisions on `stmt as *const Stmt as usize` (G3.8) — "node-address
identity — must match `vyrn_frontend::own`, which ran on this same borrowed
AST" (`lib.rs:4431-4433`). It held everywhere probed.

**CLEAN (Rust lens):** the string pool deduplicates (`wasm.rs:297-309`), the
type section interns via HashMap, dead-function pruning remaps indices
correctly; no `insert(0,..)`/rope string building anywhere (the old 240×
append bug has no surviving siblings); `toolchain.rs` unwraps are confined to
`#[cfg(test)]`; `io_message`'s panic-on-unknown-key is a documented typo guard
on a const table.

---

## Lens 4 — Agda implementer: soundness

The brief: can a program `vyrn check` accepts make a backend emit output that
is invalid or wrong, and do the three engines agree? Four confirmed breaks.

### G4.1 CONFIRMED — Critical. Mangle collision: two instantiations, one symbol, native answers wrong

**What breaks.** `mangle_name` (`compiler/vyrn-codegen/src/lib.rs:12442`)
builds a generic instantiation's symbol as `vyrn_{name}__{mangled type args}`,
and `mangle_ty` (`lib.rs:12460`) is not injective:

- `Type::Option(inner)` mangles as `Opt{inner}` (`lib.rs:12470`) — so
  `Option<Int64>` is `OptInt64`, the same string a **user type named
  `OptInt64`** produces through `Type::Named(n) => sanitize(n)`
  (`lib.rs:12469`).
- The same prefix ambiguity holds for `Arr` (`Array<Int64>` vs a type named
  `ArrInt64`), `Res`, `Strm`, `Map`, `Task`, and for `Type::App`'s
  separator-free concatenation (`P<Int8, Int64>` vs `P<Int8Int64>`,
  `lib.rs:12474-12480`).
- `Type::Record(_) => "Rec"` and `Type::Enum(_) => "Enum"` (`lib.rs:12472-12473`)
  collapse every structural record and enum to one string.

The monomorphization driver dedups by that symbol (`lib.rs:1713-1716`):

```rust
let sym = mangle_name(&name, &type_args);
if !emitted.insert(sym.clone()) {
    continue;
}
```

so the second instantiation is silently skipped and both call sites call the
first body.

**Repro** (`vyrn check` prints `ok`):

```vyrn
type OptInt64 = { a: Int64 }

fn dup<T>(x: T) -> Array<T> {
    let mut xs: Array<T> = []
    xs.push(x)
    return xs
}

fn main() -> Int64 {
    let o: Option<Int64> = Some(5)
    let r = OptInt64 { a: 9 }
    let xs = dup(o)
    let ys = dup(r)
    print("\{xs.length} \{ys.length}")
    print("\{ys[0].a}")
    let m = match xs[0] {
        Some(v) => v,
        None => -1,
    }
    print("\{m}")
    return 0
}
```

**Recorded output.**

```
$ vyrn run p5_mangle.vyrn          # interpreter
1 1
9
5
exit=0

$ vyrn build p5_mangle.vyrn -o p5.exe && ./p5.exe    # native
1 1
1948444778241
5
exit=0

$ vyrn build p5_mangle.vyrn --target wasm && wasmtime p5.wasm
1 1
9
5
exit=0
```

Three-way parity is broken and the wrong engine is the reference one: native
reads the one-word record `{ a: 9 }` through the `Option` instantiation's
body and answers stack garbage (a different number per run). A record with a
`String` field would make the same read a wild pointer dereference.

**The emitted IR shows the collision directly** (`vyrn emit-ir`):

```llvm
%t7  = call { ptr, i64, i64 } @vyrn_dup__OptInt64({ i1, i64, i64 } %t6)
%t10 = call { ptr, i64, i64 } @vyrn_dup__OptInt64({ i64 } %t9)
...
define { ptr, i64, i64 } @vyrn_dup__OptInt64({ i1, i64, i64 } %arg0) { ... }
```

One `define`, two call sites with different argument types. LLVM's call
instruction carries its own function type, so clang builds this without a
diagnostic and the mismatch becomes undefined behaviour at run time — which is
why the failure is silent.

**The defect is already documented — in the other backend.** The direct wasm
backend keys its instantiation cache structurally, and its comment says why
(`direct.rs:712-721`):

> Deliberately the type arguments and targets THEMSELVES rather than a mangled
> name: `mangle_name` is the textual backend's symbol and it is not injective
> (every record mangles as `Rec`), so two distinct specializations can produce
> one symbol and the textual driver's `emitted.insert(sym)` silently skips the
> second.

That is a correct description of a Critical defect in the sibling backend,
recorded as a design note for this one. The wasm engine is immune (verified
above); the interpreter never mangles; only the reference native path is wrong.

**Why the gate does not catch it.** The parity corpus would need a program
whose user-declared type name collides with a builtin mangle prefix — nothing
in `examples/` does, and nothing makes the collision a compile error.

**Fix, one sentence.** Key `emitted` on `(name, type_args)` as the direct
backend already does — the symbol can stay ugly as long as the dedup does not
trust it — or make `mangle_ty` injective (length-prefixing or hashing the
argument list) and keep the symbol as the key.

### G4.2 CONFIRMED — Critical. A payload that leaves its arm is freed twice, on both compiled backends

**What breaks.** Binding a sum's payload and letting the binding out of the
arm — as the arm's value, or by assigning it to an outer binding — produces a
value with two owners in the emitted code: the new binding, and the scrutinee
the release walk still frees.

**Repro 1 — the everyday unwrap-or** (`vyrn check` ok on all of these):

```vyrn
fn find(n: Int64) -> Option<String> {
    if n > 0 { return Some("hit-" + n.toString()) }
    return None
}

fn main() -> Int64 {
    let o = find(7)
    let s = match o {
        Some(v) => v,
        None => "miss",
    }
    print(s)
    return 0
}
```

```
$ vyrn run p6e_opt.vyrn        # interpreter
hit-7
exit=0
$ ./p6e_opt.exe                # native: no output at all
exit=127
$ wasmtime p6e_opt.wasm        # wasm, single shot
hit-7
exit=0
```

Native aborts with the CRT's heap-corruption kill before stdio flushes, so
even the `print` that already ran is lost.

**Repro 2 — `if let` assigning outward** (`out = v` is the trigger; the same
program without the assignment is fine):

```vyrn
let o2 = find(9)
let mut out = ""
if let Some(v) = o2 {
    out = v
}
print(out)
```

Interp and wasm print `hit-9`; native `exit=127`, no output.

**Repro 3 — Array payloads too**: `Some(xs) => xs` out of an
`Option<Array<Int64>>` — interp and wasm print `2 3 6`, native `exit=127`.
Also reproduced with `Err(e) => e` on `Result<Int64, String>`, through
`?`-bearing callers, and with a USER enum's payload
(`Bunch(xs, t) => xs` out of `type Node = | Leaf(Int64) |
Bunch(Array<Int64>, String)`: interp and wasm print `5 16`, native
`exit=127`) — six distinct programs total. Any sum type, any heap payload.

**Wasm is not actually correct — it just fails later.** Its allocator frees;
a double free corrupts its free list. Loop the unwrap-or 20,000 times:

```
$ vyrn run p26_wasmdf.vyrn
bad=0
$ ./p26_wasmdf.exe
exit=127
$ wasmtime p26_wasmdf.wasm
memory fault at wasm address 0x60000 in linear memory of size 0x20000
wasm trap: out of bounds memory access
```

All three engines now answer differently: right answer, silent abort, wild
trap.

**The emitted IR shows both frees** (`vyrn emit-ir`, Result variant):

```llvm
m.end.2:
  %t12 = phi ptr [ @.str.2…, %m.one.0 ], [ %t11, %m.zero.1 ]  ; t11 = Err payload
  store ptr %t12, ptr %msg.addr13
  …
  call void @__vyrn_str_free(ptr %t15)      ; frees msg — the payload
  …
rel.sum.hit.4:                               ; the scrutinee's release walk
  %t21 = inttoptr i64 %t19 to ptr
  call void @__vyrn_str_free(ptr %t21)      ; frees the same payload again
```

**Root cause.** The ownership map the backends consume
(`vyrn_frontend::own::analyze`, `own.rs:945`) marks BOTH the scrutinee local
and the `let` bound to the match result droppable, and has no rule that an
arm returning its own payload binding makes the result an alias into the
scrutinee. Both backends faithfully emit both releases: the textual `Let`
path registers the binding on `drop_stack` (`lib.rs:4480-4489`) and the
scrutinee is released by `release_sum` (`lib.rs:3490`); the direct backend
mirrors it through `emit_rel`. The defect lives in the frontend's map, but
the observable is the emitted code, and the interpreter — which reference-
counts instead of consuming the map — is the only engine that gets it right.

**Why the gate does not catch it.** The corpus systematically matches on
temporaries — `match stringFromBytes(b) { Ok(s) => s … }`
(`examples/codecbytes.vyrn:40`), `match ok.copy() { … }`
(`examples/copy.vyrn:150`) — and a call-result scrutinee is not a droppable
local, so no second owner exists. No example matches on a NAMED owned local
and lets the payload out. The two shapes are one `let` apart.

**Fix, one sentence.** In `own::analyze`, an arm whose value is (or contains)
its payload binding transfers ownership out of the scrutinee: either the
scrutinee's `DropKind` gains a skip for the escaping payload (the mechanism
RFC-0093's holes already provide), or the arm value is deep-copied.

### G4.3 CONFIRMED — High. The direct backend's whole stack is one wasm page, and nothing checks a frame against it

`wasm.rs:41-46`:

```rust
/// Top of the generated module's shadow stack; it grows down from here to 0.
pub const STACK_TOP: u32 = 65_536;
```

Every aggregate local lives in a shadow-stack frame; the total budget for
every frame of every call is 64 KB, and the only guard is the layout accident
that underflow traps as an out-of-bounds access ("the trap `--stack-first`
buys", `wasm.rs:700-702`).

**Consequence 1: a modest literal can never run.** A 7,000-element constant
array literal (56 KB frame) compiles to wasm in 0.1 s and every run traps:

```
$ wasmtime p20_7000.wasm
memory fault at wasm address 0xffe89600 in linear memory of size 0x20000
wasm trap: out of bounds memory access
```

Interp and native both run the same file fine. At 100,000 elements the
module still builds in 0.1 s — a module that cannot execute its first
statement, built without a warning. There is no compile-time refusal even
when ONE frame exceeds the ENTIRE stack, a comparison the backend could make
against two constants it already owns (the frame size and `STACK_TOP`).

**Consequence 2: the shared recursion contract breaks.** fd07b5c gave all
three engines one rule: `error: call depth exceeds 1000`. The interpreter's
comment argues the budget in frames (`interp.rs:30-42`); the wasm stack
budgets in bytes. A recursive function with a 16-field record local
(~200-byte frame) exhausts 64 KB near depth 300:

```
$ vyrn run p21_framedepth.vyrn        # also native, same two lines
900
error: call depth exceeds 1000
exit=1

$ wasmtime p21_framedepth.wasm        # never reaches the first answer
memory fault at wasm address 0xffffff00 in linear memory of size 0x20000
wasm trap: out of bounds memory access
exit=3
```

The engine that adopted the limit "so it stops with the same diagnostic
everywhere" (`interp.rs:37-38`) stops 700 frames early with a wild address
instead of the diagnostic.

**Fix, one sentence.** Size the stack for the declared limit (or make
`STACK_TOP` a function of the largest frame × `CALL_DEPTH_LIMIT`, capped),
refuse at compile time any single frame over the stack, and turn underflow
into the canonical trap by checking `sp` in the prologue of functions with
large frames.

### G4.4 CONFIRMED — High. `vyrn check` accepts an array literal native cannot build

The mechanics are Linus's (G1.1: N chained `insertvalue`); the soundness
half is recorded here. 100,000 constant elements:

```
$ vyrn check p20_biglit.vyrn      # 0.104 s
ok
$ vyrn run p20_biglit.vyrn        # interpreter: correct
100000 4
$ vyrn build p20_biglit.vyrn      # native
LLVM ERROR: out of memory          # after 2m19s
$ vyrn build --target wasm && wasmtime p20.wasm
wasm trap: out of bounds memory access   # G4.3
```

So a 100k-element constant table — a perfectly writable program, and the
kind a generator emits — runs on no compiled backend, and `check` predicts
none of it. This is the same class as the audit's A5.2 (check accepts what
build cannot do), reached through data size instead of type recursion.

### What refused every attack

Each line is a program that was written and run through all three engines,
byte-compared. Recorded because a negative result from a deliberate attack is
evidence.

- `Int64` min literal, `-9223372036854775808`, written directly and computed
  — identical.
- Float extremes: `-0.0`, `0.0 - 0.0`, overflow to `inf`/`-inf`, NaN and
  negated NaN, a subnormal after 1,100 halvings, `DBL_MAX` printed to all 309
  digits — identical across the interpreter's `{:.6}`, native `printf("%f")`
  and the direct backend's 511 hand-written lines. Three formatters, one
  output, including the hard cases.
- Sized-int conversions at the limits: `Int32/Int16/Int8` of `INT64_MAX` and
  `INT64_MIN`, float→int saturation (`Int64(inf)` = `INT64_MAX`,
  `Int64(NaN)` = 0), `UInt8` wrapping — identical.
- A zero-size record type: constructed, passed, returned, pushed into an
  `Array<E>` three times — identical (`3`).
- Empty everything: `"".byteLength`, `"".charCount()`, empty array length,
  `"" + "" + ""`, interpolating an empty string between multibyte scalars —
  identical.
- String content: tab/quote/backslash/percent/brace/backtick escapes, emoji,
  combining marks, CJK, RTL — the IR string pool and both backends'
  escaping agree byte-for-byte.
- The call-depth boundary itself: `down(990)` answers, `down(1005)` traps
  `error: call depth exceeds 1000` on all three engines (scalar frames).
- `spawn`/`join` with `Int64` and `String` results — identical.
- `Map<String, Int64>`: insert, overwrite, `remove`, miss, iteration order
  after removal — identical.
- Lazy stream combinators: `map` over `fromArray` consumed by a `for` with
  `break` — identical.
- Protocol dispatch through a generic bound (`tell<T: Show2>`) at `Int64` and
  `String` — identical.
- String ordering `<` on non-ASCII (byte order, `"ż" < "z"` false) —
  identical.
- Negative array index: `error: array index -5 out of bounds` — identical,
  including the sign, through the direct backend's hand-rolled number
  formatter.
- `panic` with a live droppable local: `error: too big: 9 (file:3)` —
  identical.
- Nested `Option<Option<T>>` and `Option<Result<..>>` — refused by the
  checker, identically, on all paths ("nested Option/Result is not supported
  in v0.1"). A refusal is the right answer for a `{ i1, i64, i64 }` encoding
  with one tag.
- Region shapes: early `return` from inside `region` (arena value crossing
  out as a scalar), nested regions, `break` inside a region loop — identical.
- Region escape by value: `return "n=" + n.toString()` straight out of a
  `region`, and an `Err` payload built inside one, both read back correctly
  after fifty allocations of churn on all three engines — RFC-0089 M3b's fix
  holds, including through the `?`-adjacent match shapes.
- Self-append and handover chains: `s = s + s` twice, `xs = grow(consume xs)`
  twice — identical; `xs[0] = xs[0]` is refused with the RFC-0093 wording on
  all three.
- One cosmetic non-difference, recorded so nobody re-finds it: through a
  merged pipe, native prints a trap's stderr before buffered stdout while
  interp/wasm interleave the other way. Per-stream bytes are identical; the
  parity harness compares streams separately and is right to.

---

## Lens 5 — PL theorist: coherence

### G5.1 CONFIRMED — Medium. Half the parity-load-bearing strings have one source; the other half have two

Shared, single-sourced, with the sharing argued in comments: `IO_MESSAGES`
(`lib.rs:602-618` — "One list because parity compares these bytes… neither
can hold a private copy that drifts"), `validation_message` (`lib.rs:12558`),
`CALL_DEPTH_LIMIT` (both backends render the same
`vyrn_frontend::interp::CALL_DEPTH_LIMIT`, `lib.rs:88-90`,
`direct.rs:12257`), and `llt_of` (`lib.rs:12583` — the one type→shape map,
which `layout::of_ll` parses back; verified: it is genuinely the only copy).

Duplicated as hand-written literals, enforced only by the parity gate:

| wording | textual backend | direct backend |
|---|---|---|
| `error: array index %lld out of bounds` | `lib.rs:1161` | `direct.rs:12244-12246` (split around the number) |
| `error: string index %lld out of bounds` | `lib.rs:1165` | `direct.rs:12245` |
| `error: shift amount out of range` | `lib.rs:1326` | `direct.rs:12238` |
| `error: region nesting exceeds 64` | `lib.rs:129` | `direct.rs:12250` |

The project already knows the cure — `IO_MESSAGES` is the precedent, one
file away. Four wordings never took it. A reworded trap in one backend fails
parity only if an example reaches that trap; the corpus reaches the array
one, and nothing reaches the string-index or shift one with both backends'
own eyes (the parity pins compare against the interpreter, which is the
right design — but only for pinned cases).

### G5.2 PLAUSIBLE — Medium. The calling convention has no written form

How a Vyrn value crosses a function boundary is decided in
`Gen::function`/`llt` for the textual backend (aggregates by LLVM value,
`modify` via pointer + copy-back) and in `signature`/`wasm_sig`/`Repr`
(`direct.rs:1159-1244, 677-700`) for the direct one (scalars on the operand
stack, aggregates as an `i32` shadow-stack address, `modify` copied back
since M2f). The two never link against each other, so they need not agree
instruction-for-instruction — but three seams cross between worlds and each
is defined only by the code on both sides of it:

- the extern ABI (`extern_abi_sig` `direct.rs:200`, `to_extern_abi`
  `lib.rs:11089`),
- the shim boundary (`toolchain.rs` C signatures against both emitters'
  declarations),
- `moduleInterface`/generator wasm (RFC-0076's memory map).

RFC-0077 M0 wrote the LAYOUT down and built `layout_vs_clang` to hold it.
Nothing equivalent exists for the convention: no document says "an
Option/Result is returned by value as `{ i1, i64, i64 }`; a `modify`
aggregate is an address and the callee copies back; a String crosses as a
pointer whose −8 word is capacity". The audit's P4.1 said the reference
semantics has no written form; the calling convention is the same finding
one layer down, and the same class of drift — G4.1 is what it looks like
when an unwritten agreement quietly stops holding.

### G5.3 CONFIRMED — Medium. The parity gate spans the corpus, not the language

`parity.rs` discovers programs by glob over `examples/` (`parity.rs:54-59`)
— the right design, nothing is enumerated by hand — plus pinned cases that
compare against the interpreter "because two backends can be confidently
wrong together" (`parity.rs:31`). Both choices are sound. What this review
adds is a measurement of the corpus's span: four confirmed breaks, each
one small step off the corpus's habits —

1. G4.1 needs a user type named like a mangle prefix; no example declares one.
2. G4.2 needs `match` on a named owned local whose arm exports the payload;
   every corpus match on a heap payload scrutinizes a call result
   (`codecbytes.vyrn:40`, `copy.vyrn:150`).
3. G4.3 needs one aggregate frame near 64 KB; the corpus's largest is a few
   hundred bytes.
4. G4.4 needs a five-digit literal element count; the corpus's largest
   literal has dozens.

A gate that discovers its programs cannot discover its programs' habits. The
missing instrument is adversarial generation — the audit's closing note said
"a random-program differ against the three engines is how the remaining
parity breaks will be found", and all four of these would have fallen to it.

### G5.4 PLAUSIBLE — Medium. The String header is spelled three times, in two layouts, under one RFC cite

The reading pass found one fact — the String header — written down three
times, and two of the three disagree:

- `toolchain.rs:116` `#define VSTR_HDR 16`: the shim header is 16 bytes,
  `{ long long len, long long cap }`, with `cap == -1` the static sentinel
  (`toolchain.rs:111-115`), citing RFC-0089 M1a.
- `direct.rs:12217-12226` `cap_at()` uses `offset: 4` and i32 loads: the
  direct backend's header is 8 bytes, `{ u32 len, u32 cap }`, static sentinel
  by address below `HEAP_BASE` (`wasm.rs:737-746`).
- `web/wasi-min.js:373` `const STR_HDR = 8;` with `setUint32(base…)` — its
  comment claims "the eight-byte { len, cap } header … every Vyrn String
  (RFC-0089 M1a)", which is false for the native/shim String.

Coherent TODAY only because `--target wasm` is unconditionally the direct
backend (`main.rs:4641-4652`), so `wasi-min.js` never meets a clang-built
module. No shared constant ties the three; if the clang→wasm path returns, or
`toolchain::shim_wasm()` (still alive, `toolchain.rs:815`) is paired with a
direct-backend guest, a JS-allocated String is a silent misread. This is the
calling-convention gap of G5.2 made concrete in one struct.

### G5.5 PLAUSIBLE — Medium. Two more hand-written pairs where the extern/shim ABI must agree

The scalar extern ABI IS a shared table (`crate::extern_abi_ll` at
`lib.rs:652`, consumed at `direct.rs:207/212` — CLEAN). Three shapes around it
are not:

- **The "String param = two words" rule** is hand-written in both consumers:
  `lib.rs:670-681` `extern_decl_params` pushes `ptr` then `i64`;
  `direct.rs:200-209` `extern_abi_sig` pushes `I32` then `I64`. Two copies of
  one shape.
- **Extern return narrowing** is two implementations: `lib.rs:11115+`
  `from_extern_abi` (`trunc i32→i1/iN`) vs `direct.rs:6843-6848` `renorm`,
  whose own comment points at "`from_extern_abi`'s `trunc` on the other
  backend". Enforced by parity only.
- **The WASI/browser import set** is implemented twice, admitted in source
  (`direct.rs:79-82`: "The set is implemented twice over: wasmtime provides
  all of preview1, and `web/wasi-min.js` implements exactly these for the
  browser"). No test diffs the `Wasi` struct (`direct.rs:83-121`) against
  `wasi-min.js`; enforced by manual browser verification.
- The `__vyrn_malloc`/`__vyrn_free` export condition is two hand-written
  predicates in two files (`direct.rs:597-603`, and the comment at
  `direct.rs:580` naming the LLVM path's `-Wl,--export` under "exactly the
  same condition").

**What the reading pass confirmed CLEAN, and it is exemplary:**
`imports_vs_shim.rs` parses `RUNTIME_SHIM`'s C definitions and diffs them
against `wasm::boundary()` — the same lines the direct backend builds its
import section from — with two pins (`the_i1_that_is_really_an_i32`,
`no_import_takes_or_returns_an_aggregate`). This is the source-of-truth test
G5.5's other shapes lack. Also CLEAN: the SHIM_BASE memory map is one constant
(`wasm.rs:51`) consumed by the clang flags; drop semantics take their SET from
the frontend in both backends (mechanics differ, set shared); float formatting
is shared by construction (both compile `std/num`'s `f64Str`); gap reporting
is loud at compile time (`direct.rs:55-61` returns `Err` from `compile`, and
the `Stmt` match is exhaustive with no wildcard, so a new AST statement is a
`rustc` error in the direct backend, not a runtime gap); parity pins compare
against the interpreter's live answer, not a spelling in the test.

---

## What this review did not cover

- **The generator-host path** (`compile_gen_host`, RFC-0076) and `vyrn"…"`
  quote lowering — not probed.
- **SIMD lowering** beyond reading `llt_of`'s vector cases — no F32x4/I32x4
  programs were run through three engines (the prior audit verified
  alignment on native).
- **The LLVM prelude's runtime functions** (`__vyrn_str_*`, map, slots) —
  covered by the separate shim review; only read here where a finding's
  emitted code called into them.
- **Frontend cost** — the 19 s `check` on a 50k-term chain (G1.2) is checker
  territory and was measured only as context.
- **`extern`/JS interop ABI** — the asymmetric String ABI (RFC-0012) was
  read, not exercised; the reading pass reports it as G5.4/G5.5.
- **The PLAUSIBLE drop-placement shapes (G2.2–G2.6) and the stringly-type /
  quadratic-worklist items (G3.x, G1.3)** come from reading, not running. Each
  names a file:line and a shape; none was driven to an observable crash, and
  where I could probe the reachable half (G2.2) the escape guard held. They
  are the obvious targets for the adversarial differ G5.3 calls for.
