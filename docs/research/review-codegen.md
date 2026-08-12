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
| 5 | High | C systems | **`?` on a `region`'s early-return path pops the arena before reading the propagated payload** — the same read-after-pop shape `lib.rs:30-40` documents for `return`, but reachable without the documented guard. | G2.x (pending agent evidence — see section) |
| 6 | Medium | PL | **The runtime trap wordings are duplicated as hand-written literals in both backends** (`lib.rs:1161` vs `direct.rs:12244`), while `IO_MESSAGES`, `validation_message` and `CALL_DEPTH_LIMIT` are shared constants. Half the parity-load-bearing strings have a single source; the other half have two. | G5.1 |
| 7 | Medium | PL | **The calling convention exists only as code.** `llt_of` is the one shared layout function (good); how a value is passed — by value, by shadow-stack address, `modify` copy-back — is implied by `signature`/`wasm_sig` in one backend and `function` in the other, with no written statement to check either against. | G5.2 |
| 8 | Medium | PL | **The parity gate spans the corpus, not the language.** It discovers examples by glob (good) — but findings #1–#4 are all shapes no example reaches: the corpus matches on call results, never on owned locals; declares no type whose name collides with a mangle prefix; writes no aggregate frame near 64 KB. | G5.3 |

Counts at this commit: 6 CONFIRMED (G4.1–G4.4, G1.1, G5.1), the rest
pending the reading sweep, marked where they land below.

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

*(Further Linus-lens findings from the reading sweep land in this section
when confirmed.)*

---

## Lens 2 — C systems programmer: resource discipline in the emitted code

The confirmed core of this lens turned out to be G4.2 — the double free is a
resource-discipline defect first and a parity break second; it is written up
under the Agda lens because the three-engine disagreement is what proves it.
What this lens verified by running programs:

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

*(The systematic control-flow-shape sweep of `emit_drops_above` /
`emit_releases_above` is in flight; PLAUSIBLE entries land here when it
reports.)*

---

## Lens 3 — Rust reviewer: the backend code itself

*(Reading sweep in flight; findings land here marked PLAUSIBLE unless
reproduced.)*

One item verified directly: the `Let` lowering (`lib.rs:4423-4490`) keys
ownership decisions on `stmt as *const Stmt as usize` — "node-address
identity — must match `vyrn_frontend::own`, which ran on this same borrowed
AST" (`lib.rs:4431-4433`). That invariant is carried by a comment across two
crates; nothing asserts the maps were built from this AST. It held everywhere
probed.

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
Also reproduced with `Err(e) => e` on `Result<Int64, String>`, and through
`?`-bearing callers (four distinct programs total).

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
  read, not exercised.
- Reading sweeps of drop placement across every control-flow shape, backend
  code quality, and the shared-fact census were dispatched and land in
  their lenses' sections as they report.
