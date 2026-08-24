# C1 — Where each benchmark loses, and to what

Every gap between Vyrn and the reference implementations, attributed to a
named cause with a measurement. Nothing was fixed. Each recommendation below is
an option for the owner, marked `RECOMMENDATION, NOT A DECISION`.

## Method

- Repository state: head `82234d6`, branch `main`, working tree otherwise
  untouched by this census. Driver: `compiler/target/release/vyrn.exe`
  (vyrn 0.1.0-alpha.2). No Rust code was built or changed.
- Machine: Windows 11 10.0.26200, AMD Ryzen 5 9600X, 31.1 GiB RAM. Same machine
  as the committed record `rfcs/bench-0104/results/2026-08-19-v2-run2.json`.
- Timing tool: `/usr/bin/time -v` does not exist on this platform. Every wall
  time and peak working set came from
  `measure.py --runs N [--stdin FILE] -- <command>` (scratch copy, method
  identical to `rfcs/bench-0104/harness/run.py:589-623`: whole-process wall
  clock, then `GetProcessTimes` / `GetProcessMemoryInfo` on the child handle).
- Correctness before timing: every timed artifact reproduced its fixture in
  `rfcs/bench-0104/` byte-for-byte after CRLF normalization, or was compared
  against the rebuilt C reference where no fixture exists at the timing size.
- Reference times: the C reference in `rfcs/bench-0104/harness/c/` was rebuilt
  in each investigator's scratch directory with
  `clang -O2 -ffp-contract=off -std=c11` and re-timed in the same session. The
  committed record corroborates: every conclusion below reproduces its
  committed ratio within load noise.
- Load caveat: eight investigators ran concurrently on this machine. Absolute
  times drift up to about 60 percent between sessions. Ratios measured in one
  interleaved window are the comparison the attributions rest on; where an
  agent had a quiet window, its quiet numbers are the ones quoted.

## The table

Vyrn native against the C reference, at the corpus timing sizes. Interp and
wasm columns are the other two engines of the same source. Times are median
wall seconds; peak memory is the native leg's peak working set.

| program | N | Vyrn native s | C ref s | native/C | Vyrn wasm s | Vyrn interp s | peak MB | cause (native leg) |
|---|---|---|---|---|---|---|---|---|
| nbody | 25,000,000 steps | 1.037 | 1.024 | **1.01x** | 16.49 | NOT MEASURED at size (32.5 s at 100k steps; extrapolates to hours) | 4.2 | none — parity. wasm: CODE GENERATION |
| spectralnorm | 5500 | 1.044 | 1.029 | **1.03x** | 3.268 | NOT MEASURED (one run passed 23.8 CPU-min unfinished) | 4.6 | none — parity. wasm: CODE GENERATION |
| fannkuch | 11 | 3.094 | 2.196 | **1.41x** | 5.127 | NOT MEASURED at 11 (188.96 s at order 10) | 4.4 | REPRESENTATION — one `.copy()` allocation round-trip per permutation |
| binarytrees | 18 | 2.656 | 1.261 | **2.11x** | 1.797 | 517.1 | 2093.6 | REPRESENTATION — double payload boxing, never freed |
| fasta | 5,000,000 | 1.293 | 0.803 | **1.61x** | 5.095 | 217.42 | 7.8 | CODE GENERATION — `pick` never inlined; per-line heap churn secondary |
| reverse-complement | fasta n 4,000,000 | 1.461 | 0.383 | **3.8x** | 20.45 | 84.39 | 251 | STANDARD LIBRARY — `readLine` reads one byte per `getchar()` call. wasm: CODE GENERATION — one `fd_read` syscall per input byte |
| k-nucleotide | fasta n 4,000 | 0.058 | 0.020 | **2.9x** | 0.264 | 53.69 | 6.7 | REPRESENTATION — a heap `String` key manufactured per window position |
| pidigits | 12,000 digits | 1.511 | 1.457 | **1.04x** | 2.199 | 674.61 | 4.8 | none — parity. wasm: CODE GENERATION |

Two of eight native legs are at parity with C. The committed record said three;
the third (k-nucleotide) moved since the recording — see
[The two known gaps](#the-two-known-gaps).

---

## nbody

Native parity holds. Interleaved quiet-window medians: C 1.024 s, Vyrn native
1.037 s (1.01x); loaded windows showed up to 1.09x, which the interleaved
re-measurement attributed to contention. At 25,000,000 steps both print
identical output.

Bisection findings:

- All scalable cost is `advance` (`examples/nbody.vyrn:158-189`). Both sides
  have the same phase structure.
- Adding hard bounds checks to the C copy costs about +26 percent, so checks
  could explain a gap. Removing all 91 bounds checks from Vyrn's emitted LLVM
  IR made it slower (cpu 1.250 vs 1.125): the checks are not the cost, and
  LLVM already hoists them against loop-invariant lengths. Inspected machine
  code shows one `cmpq/jae` guard per inner iteration, `sqrtsd` inlined, packed
  SSE for dx/dy/dz.
- Residual structural difference: C folds `NBODIES = 5` statically; Vyrn emits
  runtime-length loops. Worth a few percent, hidden by noise. No cause
  attributed — there is no native gap.

Wasm, 15.4x C: **CODE GENERATION.** The direct backend emits unoptimized code.
Per pair iteration in the emitted wat (func 19): bounds-check branches on every
access, none hoisted (`compiler/vyrn-codegen/src/direct.rs:9502-9523`); every
record field write through an array index lowers to bounds-check +
`memory.copy` of the 56-byte record out to scratch + scalar store +
`memory.copy` back — 672 bytes of copy traffic per pair update
(`direct.rs:3785-3797`; the plain-record path stores scalars directly,
`direct.rs:3420-3422`); descriptor re-loaded on every operand; `sqrtF` a real
call carrying the call-depth counter; no vectorization.

`RECOMMENDATION, NOT A DECISION.` One peephole/CSE pass over the direct wasm
emitter's function bodies (scalar store through element address instead of
copy-out/copy-in, bounds-check merging, descriptor-load hoisting, leaf-call
inlining) should take wasm from about 15x toward the native 1.0-1.1x band.
Larger blast radius alternative: route `--target wasm` through the LLVM path.

## spectral-norm

Native parity holds: C 1.029 s, Vyrn native 1.044 s (1.03x, min-wall basis in
one interleaved window). Auto-vectorization is not a factor — C with
`-fno-vectorize` measures the same as `-O2`.

Wasm, 3.08x C: **CODE GENERATION.** Bisection ladder on the emitted module:

| variant | min wall s |
|---|---|
| base wasm | 3.280 |
| drop `* v[j]` | 3.302 |
| drop `cell(i,j)` call too | 2.981 |
| hand-inline cell's formula | 2.973 |
| same emitted LLVM IR at clang -O1 | 1.051 |
| same emitted LLVM IR at clang -O0 | 7.291 |

The decisive pair is the last two rows: the direct-wasm module sits between the
optimized world (1.05 s) and the fully naive world (7.29 s) built from the very
same IR. Per inner iteration (about 1.21 x 10^9 of them at N = 5500) the
emitted wat pays a real `call` to `cell` with the call-depth counter bump/check
(`direct.rs:1854-1856`, `direct.rs:1962-1985`), division-guard branches on the
constant `/2`, and a header reload plus bounds check per element access
(`direct.rs:9502-9523`).

`RECOMMENDATION, NOT A DECISION.` An IR cleanup pass before the direct emitter
(small-function inlining, alloca promotion, constant-divisor guard folding,
invariant hoisting) is mechanically local and the -O0/-O1 pair prices most of
the 2.2 s gap as optimizer-recoverable; realistically wasm goes from about
3.1x toward at most 1.5x C.

## fannkuch-redux

Native 1.41x C (committed 1.40x — reproduced). Bisection, all variants
verified against correct order-11 output:

| variant | median s | delta |
|---|---|---|
| baseline | 3.094 | — |
| generation-only walk (fold and copy removed) | 0.187 | the walk is not where time goes |
| fold+flip fully inlined, fresh `perm1.copy()` kept | 3.106 | call/move overhead is nil |
| C + Vyrn's per-access bounds check + malloc/memcpy/free per permutation | 3.067 | matches baseline — Vyrn emits exactly this cost |
| same computation, scratch allocated once and reused | 2.227 | **1.01x C — gap closed** |

Cause: **REPRESENTATION — allocation policy.** The program folds each
permutation through `flip(p: consume Array<Int64>, …)`
(`examples/fannkuch.vyrn:27-39`), so the call site needs an owned value:
`foldCount(perm1.copy())` (`examples/fannkuch.vyrn:92`) lowers to malloc +
memcpy + free on every outer-loop iteration — about 39.9 million triples at
order 11. The C reference allocates its scratch once
(`rfcs/bench-0104/harness/c/fannkuch.c:68,90-93`). The allocator itself is
thin (`compiler/vyrn-codegen/src/toolchain.rs:103-109`); the churn is the
cost, not slow malloc.

The assigned question — does the consume/move helper compile to copies — has a
measured answer: no. The move-in is two register stores of the
`{ptr,len,cap}` handle and the move-out returns the same handle; step 2 shows
calls and moves are free. The copy that hurts is the explicit `.copy()` at the
call site, which the consume discipline requires.

Wasm residual after removing the allocation: 1.50x C — bounds branches plus
fat-aggregate arguments through stack `memory.copy` per call: CODE GENERATION,
secondary.

Incidental defect found while bisecting: rebinding an owned variable after
moving out of it double-frees. Minimal repro exits 0xC0000374 (heap
corruption). This blocks the natural buffer-reuse idiom.

`RECOMMENDATION, NOT A DECISION.` Program-level (about ten lines): allocate
scratch once, make `flip`/`foldCount` take `modify` parameters — verified
correct at 2.707 s (1.23x); fully inlined reuse reaches 1.01x. Compiler-level:
fix the owned-rebinding double-free first, because it blocks the idiom users
would otherwise write.

## binary-trees

Native 2.11x C on time; on memory, 2093.6 MB against C's 21.0 MB — the known
2.1 GB symptom. See [The two known gaps](#the-two-known-gaps) for the memory
work; the time bisection belongs here.

- Full 2.656 s: build 2.356 s (89 percent), walk about 0.300 s, drop 0.000 s —
  no free instruction exists anywhere in the emitted IR.
- Vyrn issues 67.98 million `malloc(24)` calls (two boxes per Node) against
  C's 33.99 million `malloc(16)`; per-call allocator cost is similar
  (about 35 vs 47 ns). The walk is cheap (by-value triples vs pointers).

Time cause: the same **REPRESENTATION** finding as the memory section, showing
up as allocation count instead of residency.

## fasta

Native 1.61x C. Int64 arithmetic lowers to machine words (`mul/add/srem i64`,
`cvtsi2sd`) — checked, not a cause. Float formatting is not on the hot path;
`print` receives ready strings.

Bisection (native, medians):

| phase | median s |
|---|---|
| LCG only, 40M draws | 0.134 |
| + weighted pick | 0.636 (+0.502) |
| + per-line array build + `stringFromBytes`, no print | 0.886 (+0.250) |
| repeat section incl. print | 0.144 |
| print-only: 833,333 literal lines | 0.293 |

Cause: **CODE GENERATION**, primary. `pick` stays a standalone function called
once per base — four external `callq vyrn_pick` sites in the compiled asm,
each paying prologue, two TLS-segment loads for the `__vyrn_call_depth` meter,
and a `{ptr,len,cap}` triple by value. The +0.502 s pick step alone accounts
for the whole gap over C, whose `static pick` inlines to nothing.
Secondary, REPRESENTATION: each output line allocates a fresh growable
`Array<UInt8>` (five reallocs), then `__vyrn_bytes_dup` plus `__vyrn_utf8valid`
over known-ASCII data; C uses `char w[61]` on the stack. The language offers no
fixed stack line buffer — the program cannot express C's shape.

Wasm, 6.34x C: prints dominate. 833,333 constant-line prints cost 4.321 s on
wasm against 0.293 s native — `print` compiles to one unbuffered WASI
`fd_write` per line (`compiler/vyrn-codegen/src/direct.rs:13096-13194`,
"the ONE place bytes leave this module"), no userspace buffering anywhere in
the backend. About 70 percent of the wasm leg is output.

`RECOMMENDATION, NOT A DECISION.` Add a stdout buffer to the wasm runtime
(accumulate in linear memory, flush at exit or every N KiB): wasm 5.10 s to
about 1.7 s. Independently, size-based inlining or eliding the TLS depth meter
for leaf functions: worth about 0.5 s of the native leg.

## reverse-complement

Native 3.5-3.8x C; wasm about 43x; interp 177x with a 3867 MB peak.

Bisection (native, medians; sum closes exactly):

| step | variant | median s |
|---|---|---|
| B1 | `readLine()` loop only, sum lengths | 0.738 |
| B2a | + header probe | 0.732 (noise) |
| B2 | + sequence ingest via `for b in bytes(l)` push | 0.869 |
| B3 | + complement translate | 0.952 |
| B4 | + wrap loop, no string build | 1.163 |
| B5a | + `stringFromBytes` per output line | 1.171 |
| B5 | + print (full program) | 1.461 |

Cause: **STANDARD LIBRARY / runtime I/O primitives.** `readLine` alone costs
0.738 s — 1.9x the entire C program. Its native implementation reads one byte
per `getchar()` call, mallocs a fresh buffer per line with doubling growth, and
the lowering adds a full UTF-8 DFA validation per line
(`compiler/vyrn-codegen/src/toolchain.rs:269-286`;
`compiler/vyrn-codegen/src/lib.rs:9627-9631`). Print itself is at parity
(microbench: 0.205 s vs identical C printf loop 0.212 s). The census-flagged
per-line `bytes(l)` friction is real but minor: +0.131 s for ingest, and the
double `bytes()` call per line shows up as allocator churn (peak 4.8 to 58 MB)
without wall-time cost.

Wasm, 43x: **CODE GENERATION.** The runtime's `getbyte` performs one WASI
`fd_read` syscall PER INPUT BYTE — 40.7 million syscalls for this run
(`direct.rs:15391-15427`; the deferral note at `direct.rs:15393-15397` names
buffering as future work). Measured: the read path is 19.56 s of the 20.45 s
leg, 96 percent.

Interp, 177x and 3867 MB: **REPRESENTATION.** In the interpreter `bytes(s)`
boxes every byte as its own `Val::IntN` value
(`compiler/vyrn-frontend/src/interp.rs:5162-5167`), so the 40M-base sequence
and its reversed copy are tens of millions of heap values.

Missing primitive underneath both compiled legs: no way to read stdin as raw
bytes in bounded blocks — `readFileBytes` takes a path only — which forces the
line protocol.

`RECOMMENDATION, NOT A DECISION.` Two independent changes: (1) give the wasm
`getbyte` a 4 KiB read-ahead buffer — wasm 20.4 s toward 1-2 s, closing most of
43x; (2) block reads in `__vyrn_read_line` plus an ASCII fast path past the DFA
— native 0.738 s toward about 0.15 s, taking the leg from 3.8x to about
1.5-2x. No change to `examples/revcomp.vyrn` required.

## k-nucleotide

The recorded 174x C / 211x Rust is stale — see
[The two known gaps](#the-two-known-gaps). Today, native is 2.0-2.9x C
depending on session load; wasm about 2.3x C plus wasmtime start-up.

Cause: **REPRESENTATION, forced by a MISSING PRIMITIVE.** About 87 percent of
native time manufactures a heap `String` key per window position, twice over:

1. Per-window `Array<UInt8>` build, one pushed byte at a time
   (`examples/knucleotide.vyrn:65-71`) — 42 percent of the leg at fasta
   n = 40,000.
2. `stringFromBytes` — malloc dup plus UTF-8 DFA validation per position
   (`examples/knucleotide.vyrn:72`; lowering
   `compiler/vyrn-codegen/src/lib.rs:10036-10070`) — 45 percent.
3. Map double search (`m[key]` then `m[key] = seen + 1`) — at most 7 percent.
   The hash map itself is healthy: FNV-1a with open addressing
   (`compiler/vyrn-codegen/src/toolchain.rs:163-247`).
4. The hand-written insertion sort is exonerated: it sorts only the 4-key and
   16-key tables; removing it changed nothing measurable.

There is no way to key a `Map` by a byte slice or an integer window code —
`Map<String, V>` (RFC-0028) with `stringFromBytes` as the sole gate forces the
materialization. The C reference hashes the window bytes in place, zero
allocations.

`RECOMMENDATION, NOT A DECISION.` Let `countKmers` carry a rolling integer
window code (two-bit packing, O(1) per position) — removes about 87 percent of
the native leg without touching Map. More general: byte-slice map keys, which
the existing `map_hash` already iterates. Micro-win: a get-or-insert primitive
to collapse the double probe.

## pidigits

Native parity holds: 1.04x in a clean interleaved window (1.002x committed).
No native gap to attribute. The spigot carries limb arithmetic by hand because
`std/num` exports no big integer — that is why the program looks the way it
does, but the C reference runs the identical bounded algorithm, so the missing
primitive creates no measured gap.

Wasm, 1.51x C: **CODE GENERATION.**
`compiler/vyrn-codegen/src/direct.rs:5920-5984` emits division zero-check and
`INT_MIN/-1` overflow-trap branches unconditionally, with no folding of literal
divisors (`%10`, `/10` at `examples/pidigits.vyrn:56-57` can never trap) and no
reuse of the divisor computed twice per inner iteration; array headers reload
per access. Hand-stripping the statically-impossible guards took wasm from
2.191 s to 1.961 s (-10.5 percent); adding the CSE took it to 1.34x vs native.
The residual is Cranelift-baseline versus clang -O2 quality on a div/mod-heavy
loop.

`RECOMMENDATION, NOT A DECISION.` Skip the zero-trap for non-zero literal
divisors and the overflow-trap when the divisor is not -1, and reuse the
divisor local across `%//` pairs — moves wasm to 1.34x; the rest wants a real
optimization pass or the LLVM route.

---

## The two known gaps

### The `Map` operation, about 211 times slower

Where the number comes from: `site/app/bench.vyrn:1499` asserts
`ratioText(ratioOf("knucleotide", "vyrn-native", "rust")) == "211x"` — the
published dataset's k-nucleotide native leg (2.964 s) against its Rust leg
(0.014 s), both recorded 2026-08-19 in
`rfcs/bench-0104/results/2026-08-19-v2-run2.json`. The cause then was the
linear-scan `Map`: lookup walked the pair vector — `__vyrn_map_find`'s
`strcmp` loop — making k-nucleotide quadratic in distinct keys (RFC-0104's M2
section). That defect is closed at this head: work item 1 put an FNV-1a-hashed
open-addressing index beside the ordered storage, in all three engines.

Today's binary, fasta n = 4000 (the 20,000-base THREE sequence), C reference
rebuilt beside it:

| engine | median wall s | peak MB |
|---|---|---|
| C reference | 0.0203 | 7.8 |
| Vyrn native | 0.0584 | 6.7 |
| Vyrn wasm | 0.264 (wasmtime start-up dominated; cpu 0.063) | 18.7 |
| Vyrn interpreter | 71.0 | 21.1 |

The native leg went from 287x C to 2.9x C — about 50 times faster than the
recorded run, matching RFC-0104's own re-run (20 ms). **The 211x no longer
exists at this head.** What remains on the compiled legs is not the Map: it is
key materialization (the previous section). The interpreter leg is a different
story, found by this census:

**New interpreter-only Map cause: every expression-level read of a map binding
deep-copies the entire table.**

Evidence chain:

- Pure inserts scale flat: 2,000 / 4,000 / 8,000 / 16,000 distinct-key inserts
  take 0.043 / 0.044 / 0.046 / 0.060 s.
- Lookups against an EMPTY map scale flat: 16,000 misses take 0.061 s.
- Lookups against a POPULATED map scale with its size: 2,000 misses against a
  16,000-key map take 4.094 s — 2.0 ms per read, about 125 ns per entry. The
  lookup result is irrelevant; the read of the binding itself is the cost.
- Get-plus-set is quadratic: 8,000 keys 3.460 s, 16,000 keys 14.016 s.
- The real program shows the same signature. At fasta n = 4000 the interpreter
  run is 68.0 s total; building the k = 8 table alone (16,444 distinct keys)
  costs 27.2 s while the k = 4 table (256 keys) costs 0.741 s — the same
  number of window operations, thirty-seven times the time.

Cause: `Val::Map(MapVal)` is a bare struct
(`compiler/vyrn-frontend/src/interp.rs:379`) — unlike `Array(Rc<Vec>)` and
`Str(Rc<String>)`, there is no reference count. `Expr::Var` evaluates to
`slot.v.clone()` (`interp.rs:4247`), and cloning a `MapVal` copies both
`pairs` and the `idx` hash map (`interp.rs:417-420`). So `m[key]` clones the
whole map before the O(1) hashed lookup ever runs. The isolated operation,
reproduced as requested with the corpus's own shape (countKmers over a
2,000,000-base synthetic sequence, k = 2), measured on all three engines:

| engine | median wall s |
|---|---|
| native | 0.198 |
| wasm | 0.175 |
| interpreter | 11.84 |

(At k = 2 the synthetic sequence has only 4 distinct keys, which is exactly
why the interpreter survives this microbenchmark; the blow-up above needs many
distinct keys.)

Class: REPRESENTATION (interpreter). Smallest change, `RECOMMENDATION, NOT A
DECISION`: put the interpreter's map storage behind an `Rc` like arrays and
strings (clone the handle, `Rc::make_mut` on write), or special-case indexed
map reads to borrow in place. Effect: removes the quadratic term that makes
every map-heavy program hundreds of times slower under `vyrn run`. Cost: one
representation change in the interpreter, no frontend or backend impact — the
compiled engines never had the scan or the clone.

**DONE.** This finding was verified independently and the `Rc` was taken. The
measurement stands as recorded and was, if anything, understated. `vyrn run`
on `examples/knucleotide.vyrn` at fasta n = 4000, same machine, same input,
old binary against new: **35.14 s to 0.82 s, 43x.** The isolated shape — 2,000
reads of one key — went from 1.371 s against an 8,000-entry map to 0.022 s, and
from tracking the size of the map to flat. Pinned by
`a_map_read_does_not_copy_the_table` in `compiler/vyrn-cli/tests/places.rs`,
which fails on the old interpreter at 7.3x. Three-way parity green at 40.

### The binary-trees memory, about 2.1 GB

Symptom confirmed at order 18: native peak working set 2093.6 MB, wasm 2093.1
MB, against C 21.0 MB, Rust 36.8 MB, JS 387.5 MB (committed record). The
interpreter peaks lower (978 MB) only because it runs slower than it leaks.

Growth curve — peak MB at increasing depths, with the implied bytes per Node
construction (model = constructions x 2 boxes x 32-byte allocator granule):

| depth | constructions | native peak MB | B/node | wasm peak MB | model MB |
|---|---|---|---|---|---|
| 12 | 334,510 | 25.2 | 78.9 | 41.3 | 20.4 |
| 14 | 1,600,174 | 103.0 | 67.6 | 118.2 | 97.7 |
| 16 | 7,449,262 | 462.5 | 65.6 | 475.7 | 454.7 |
| 18 | 33,991,342 | 2093.6 | 64.6 | 2093.1 | 2074.7 |

The curve converges on 64 bytes per node and the model tracks measurement to
within 5 percent everywhere. Where it goes:

1. **Boxing doubles the allocations.** `Tree` is a recursive enum whose payload
   is not a machine word, so each non-word payload gets its own heap box — two
   `__vyrn_malloc(24)` per Node against C's one `malloc(16)` struct
   (`compiler/vyrn-codegen/src/lib.rs:6745-6764`).
2. **Granularity multiplies by 4/3.** Native links the plain CRT allocator
   (`toolchain.rs:103-108`); Windows hands a 32-byte granule to a 24-byte
   request. Verified against the control: the deliberately-leaking C build
   measures 33.99M x 32 B = 1083 MB, and Vyrn's 67.98M x 32 B predicts 2075 MB
   against 2094 measured.
3. **Nothing is ever freed — the dominant multiplier.** Peak equals the
   whole-run accumulation because the automatic release emits zero
   instructions for this type: a self-referential enum gets
   `release_kind None` through the self-referring-past analysis
   (`compiler/vyrn-frontend/src/own.rs:552`, `:683-713`), and the comment at
   `lib.rs:3940-3944` documents the leak ("48 bytes a node over a released
   tree"). Tested, not assumed: adding an `impl Owned for Tree` keeps the
   output correct but leaves peak UNCHANGED at 2093.5 MB, because
   `owns_heap(Tree)` returns false past depth 8 (`own.rs:810-814`) and the
   release path skips every payload before reaching its free
   (`lib.rs:3949-3998`, guards at `:3971`/`:3984`).

So: not the allocator's speed, not reference counting (there is none), not the
program — a REPRESENTATION decision (per-payload boxes) compounded by an
ownership-model refusal (the release machinery declines the one shape this
benchmark is made of). Net multipliers: 2.69x granularity-and-boxing over the
raw 24-byte payload, about 84x residency from never freeing, about 100x
against C end to end.

`RECOMMENDATION, NOT A DECISION.` Change `owns_heap` to stop descending at
types carrying a declared `impl Owned` row (pass the `self.impls` predicate
into the analysis at `own.rs:539-541`/`:753-757`) so `release_enum` descends to
its existing box free (`lib.rs:3993-3998`), and ship
`examples/binarytrees.vyrn` with a recursive `impl Owned for Tree` — verified
compiling and byte-correct in scratch. Expected peak at order 18: 2094 to
about 25-30 MB, C parity. Cost: the release traversal adds about 1.6 s today
with zero frees; part of that is recovered once frees let the allocator recycle
(the leaking-C control shows reuse beats leaking).

---

## Causes that repeat

Two cause classes explain three or more programs each. They are the valuable
rows: one fix each, several numbers move.

### 1. CODE GENERATION — the direct wasm backend runs no optimizer

Explains six of eight wasm columns, plus two runtime-emission defects in the
same file tree:

- nbody 15.4x: record copy-out/copy-in per field write, unhoisted bounds
  checks, uninlined calls, no vectorization.
- spectral-norm 3.1x: the same IR measures 1.05 s at clang -O1 and 7.29 s at
  -O0; the wasm module sits between.
- pidigits 1.5x: statically-impossible division traps, no CSE.
- fannkuch residual 1.5x after the allocation fix.
- reverse-complement 43x: one `fd_read` syscall per input byte
  (`direct.rs:15391-15427`).
- fasta 6.3x: one unbuffered `fd_write` per printed line
  (`direct.rs:13096-13194`).

One investment — an optimization pass over the lowered IR before the direct
emitter (or routing wasm through the LLVM pipeline native already uses), plus
input and output buffers in the wasm runtime — moves every wasm point on the
chart toward the native line, which already sits at or near parity.

### 2. REPRESENTATION — values boxed, copied, or cloned where the reference uses a machine word

- fannkuch: a mandatory `.copy()` allocation round-trip per permutation
  (consume discipline), 1.41x -> 1.01x when removed.
- binary-trees: double payload boxing, never reclaimed, 100x memory.
- k-nucleotide: a heap `String` manufactured per window position, 87 percent
  of the native leg.
- the interpreter's `Map`: no `Rc` around `MapVal`, so every map read deep-copies
  the table — quadratic in distinct keys, and the reason the interpreter
  column loses by 177x-7500x while the compiled columns do not.
- reverse-complement under the interpreter: every byte a separate boxed value,
  3.9 GB resident.

A related MISSING PRIMITIVE enables half of these: byte-slice map keys
(k-nucleotide), block reads of stdin (reverse-complement), and a fixed stack
line buffer (fasta) would each let the idiomatic program express what the C
reference expresses.

---

## Ranked by seconds recovered per unit of work

`RECOMMENDATION, NOT A DECISION.` Ordered by recovered seconds divided by
implementation size, largest first. Seconds are per single run at the corpus
sizes on this machine.

1. **Buffer the wasm runtime's stdin and stdout.** Small, localized edits in
   `compiler/vyrn-codegen/src/direct.rs`. Recovers about 18 s on
   reverse-complement (20.4 -> 1-2 s) and about 3.4 s on fasta (5.10 -> 1.7 s),
   and helps every future I/O-bound program on the wasm line.
2. **An optimization pass before the direct wasm emitter** (or the LLVM route).
   Larger work, the biggest total: recovers about 15 s on nbody, about 2.2 s on
   spectral-norm, 0.7 s on pidigits, about 1.8 s on fannkuch, and lifts the two
   I/O rows further after item 1. Six rows move toward the native line, which
   is the line the project's null hypothesis predicts.
3. **Stop declining release for self-referential enums with a declared
   `impl Owned`.** Moderate compiler change in `own.rs` plus a shipped
   annotation in the example. Recovers 2064 MB on binary-trees (2094 -> about
   25-30 MB, a 100x memory claim gone) and some of its 1.4 s native gap once
   frees recycle.
4. **Byte-slice map keys (or rolling integer window codes).** Stdlib/type-level
   work item, already numbered in RFC-0104. Recovers about 87 percent of
   k-nucleotide's native leg (2.9x C toward parity) and retires the corpus's
   largest remaining native outlier.
5. **Put the interpreter's maps behind an `Rc`.** Small interpreter change.
   Recovers the quadratic map-read term: k-nucleotide interp 54-68 s toward
   seconds, and every future map-heavy program under `vyrn run`.
6. **Block reads plus an ASCII fast path in native `readLine`.** Localized
   change in `toolchain.rs`. Recovers about 0.6 s of reverse-complement's
   native leg (3.8x toward 1.5-2x) and benefits every line-oriented program.
7. **Leaf-function inlining or eliding the TLS call-depth meter on native.**
   Recovers about 0.5 s on fasta's native leg (1.61x toward parity) and likely
   helps other call-heavy kernels.

---

## Refinement, made on verification

The reverse-complement attribution says `STANDARD LIBRARY — readLine reads one
byte per getchar() call`. The per-byte read is real, but the two backends do not
pay the same price and the census bills them as if they do.

**Native** — `__vyrn_read_line` at `compiler/vyrn-codegen/src/toolchain.rs:270`
does call `getchar()` once per byte. But `getchar` is buffered by libc, so each
call is a function call against a filled buffer, not a system call. That is a
real cost at four million bytes and it is not a system call per byte.

**wasm** — one `fd_read` per byte, and this one is a system call. The census is
right that wasm is the severe case, and its own `CODE GENERATION` note for wasm
carries that.

### The part worth more than the ratio

The wasm behaviour is already recorded in the compiler, at
`compiler/vyrn-codegen/src/direct.rs:15394`, as a deliberate simplification with
its ceiling written down:

> one `fd_read` per byte, where C's `getchar` is buffered. `readLine` is the only
> caller and the corpus feeds it a few hundred bytes from a fixture; a 4 KB
> buffer here would need its own invalidation story to stay correct if anything
> else ever reads fd 0.

The justification is that the corpus feeds a few hundred bytes. This census ran
reverse-complement at `fasta n 4,000,000`. **The assumption the simplification
rested on is no longer true.** The marked ceiling has been crossed by the
benchmark corpus itself.

That is the finding, and it is a better one than the ratio: not "the standard
library is slow", but "a documented trade-off has outlived the condition that
made it correct, and the thing that outgrew it is in this repository". The fix
named in the comment — a 4 KB buffer plus an invalidation rule for fd 0 — is the
one to cost.
