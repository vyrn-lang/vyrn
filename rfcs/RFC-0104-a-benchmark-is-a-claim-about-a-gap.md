# RFC-0104 — A Benchmark Is a Claim About a Gap

- **Status:** **M0 landed** (the census; see
  [M0 — as landed](#m0--as-landed) and `rfcs/bench-0104/`). M1 to M3 proposed.
  Milestones below; a milestone that fails its gate says so in this file.
- **Depends on:** RFC-0055 (bench blocks, `blackBox`, `vyrn bench`), RFC-0102
  (toolchain pinning — what "measured against wasmtime 46.0.1" means is now a
  lock line), RFC-0101 (the direct wasm backend whose optimizer gap this arc
  prices), RFC-0105 M1 (where the full data lives — the backstage).
- **Evidence (user):** "What about benchmarks and comparison (penta (or more)
  gram)", "Why it should lose everywhere??", "Also should prefer clean and
  idiomatic code when possible", "Also benchmark spider charts should be
  interactive".

---

## The claim

Vyrn's native path emits LLVM IR through the same `clang -O2` pipeline Rust
feeds. The language monomorphizes everything, has no garbage collector, and
places releases statically. So the null hypothesis for a tight numeric kernel
is not "a young language loses gracefully" — it is:

> **Same LLVM, same discipline, same numbers.** Every measured deviation is a
> named defect or a named missing feature.

A benchmark that loses is not an embarrassment; it is a diagnosis with a
program attached. binary-trees measures the allocator. mandelbrot measures the
`F64x2` path. pidigits measures the arithmetic a program has to carry itself.
The chart this arc builds is a roadmap that updates itself.

[Two of those three sentences said something else when this RFC was written —
"mandelbrot measures the missing `F64x2`" and "pidigits measures the missing
bignum and cannot even enter". M0 measured both claims and both were wrong.
`F64x2` shipped in RFC-0083 M4, and pidigits enters: see
[M0 — as landed](#m0--as-landed).]

## Three principles

1. **Idiomatic first, always charted.** Every program is written the way the
   guide would write it — house style, subject-first collections, `?` and
   `Fallible`, no allocation golf. That line is the chart, because "what does
   normal code cost" is the question a language answers for its users.
2. **A tuned variant only where the idiom measurably loses — and the delta is
   the finding.** A pooled binary-trees that beats the idiomatic one is a
   compiler or stdlib work item, recorded here like every other measured claim.
   The tuned line prices the idiomatic line; it never replaces it.
3. **Same discipline on the other side.** The Benchmarks Game's published top
   entries are hand-tuned to the bone. The harness compares plainly written C,
   Rust, and JavaScript held to the same "as the language's own book would
   write it" bar. The game's published top-entry number is drawn as a reference
   ceiling, labeled with its fetch date — recorded, not vendored.

## The contestants

Five lines on the radar: **C, Rust, JavaScript (Node), Vyrn native, Vyrn
wasm**. The wasm line is load-bearing: the direct backend runs no optimizer,
and its distance from the native line prices RFC-0101's §2.3/§2.4 endpoints
with a number instead of an adjective.

## The programs

M0 fixed this table by running a probe per doubt. Every row is an answer, and
every answer names the program in `rfcs/bench-0104/` that produced it. The
method, the outputs and the four corrections are in
[M0 — as landed](#m0--as-landed).

| program | expressible | what it needs that is not obvious | probe |
|---|---|---|---|
| nbody | **yes**, output matches at n = 1000 | no scalar square root exists: `F64x2.sqrt(v).lane(0)` is it. `print` is fixed at six decimals and the game prints nine, so the program carries a formatter | `p-nbody`, `p-sqrt`, `p-fmt9` |
| spectral-norm | **yes**, output matches at N = 100 | the same two, and nothing else | `p-spectralnorm` |
| fannkuch-redux | **yes**, output matches at n = 7 | a helper that mutates an array the caller keeps takes it `consume` and returns it — a `read` parameter may not be returned | `p-fannkuch` |
| binary-trees | **yes**, output matches at n = 10 | nothing. A recursive enum, a `read` walk that destructures payloads, an automatic drop; the movecheck asks for no copy anywhere | `p-trees` |
| fasta | **yes**, output matches at n = 1000 byte for byte | the LCG is `Int64` arithmetic and an `Int64`-to-`Float64` divide; output is one `print` a line | `p-fasta` |
| reverse-complement | **yes**, output matches at n = 1000 byte for byte | `readLine` is the whole of stdin, one line at a time; `for b in line` yields `Int64` where the byte table wants `UInt8`, so each line goes through `bytes` | `p-revcomp` |
| k-nucleotide | **yes**, the 1-mer and 2-mer tables match | one `stringFromBytes` per window — an allocation and a UTF-8 validation per position. `sortBy` takes an `Int64` key, so "by count, ties by fragment" is a hand-written sort, and swapping two records inside an array costs three `.copy()` | `p-mapkey` |
| regex-redux | **no** | the only regex is `=~`: a compile-time-constant pattern, matched anchored against the whole string. It answers neither "how many" nor "where", and there is no substitution by pattern (`std/strings`' `replace` takes a literal). **The named gap: a runtime regex — search, count, replace** | `p-regex` |
| mandelbrot | the pixels **yes**, the output **no** | the arithmetic is ordinary and `F64x2` has been here since RFC-0083 M4. What is missing is a sink: the game writes a binary PBM, `print` and `writeFile` both take a `String`, and `stringFromBytes` refuses a packed row with `bytes are not valid UTF-8`. **The named gap: a byte sink for stdout and for `writeFile`** | `p-mandelbrot`, `p-binout` |
| pidigits | **yes** — the 27-digit output matches — but not to the game's rule | `Int64` is the widest integer and it wraps; nothing in `std` exports a big one. The bounded spigot needs none (its widest intermediate is about `(10n/3)²`, inside `Int64` past a billion digits), so the output is reachable; the game's pidigits is specified as arbitrary-precision arithmetic, so a rules-conforming entry is not. **The named gap: a big integer in `std`** — a library gap, not a language gap, and `std/num`'s `f64Str` is already limb arithmetic written in Vyrn | `p-pidigits` |

Parallelism: the game's winning entries use every core. Vyrn has `spawn`/
`join` and no data-parallel utilities. The harness compares single-threaded
implementations across all five contestants, and additionally shows a
spawn-parallel Vyrn variant where one is natural — so the parallel gap is
measured, not hidden. M0 confirmed one is natural: `p-spawn.vyrn` is
spectral-norm's matrix-vector product over two tasks, prints the same
`1.274219991`, and needs no per-worker copy of the input vector — a `read`
parameter is shared, not moved into the task.

## Methodology, armor-plated

This genre attracts nitpicks, so nothing is left to trust: pinned toolchains
(Vyrn's via RFC-0102's lock; rustc, node, and clang versions recorded in the
committed results), published flags, N runs, medians, one machine per recorded
dataset with its environment written down, and every program's output checked
against the game's expected output before anything is timed. Vyrn's three
engines run the same source through the parity harness, so correctness is the
invariant the project already pays for.

## The chart

A radar ("pentagram") with one axis per benchmark and one line per contestant,
normalized to C = 1, and **interactive**:

- hover or focus an axis: the exact numbers, flags, and runs behind that point;
- the legend toggles contestants on and off;
- a normalization switch (C = 1 / Rust = 1);
- where a tuned variant exists, an overlay toggle shows both Vyrn lines.

Built with the site's own machinery — SVG from `std/html` at build time, a
client `.vyx` component for the behavior, no chart library — and accessible:
keyboard-reachable axes with labels, a static table fallback so the page
answers without script, reduced-motion respected. The consumer site's
`/compare` page carries the headline chart; the full data, methodology, and
per-run records live on the backstage (RFC-0105 M1).

## Milestones

**M0 — the census.** *Landed — see [M0 — as landed](#m0--as-landed).* One row
per game program: expressible or not, what it needs, the expected-output fixture
for a small N checked into the corpus directory. The bignum and `F64x2` absences
recorded as facts with the programs that measure them. Gate: the table above
rewritten with no "probably" cell.

**M1 — the programs.** The expressible set, written idiomatically, output-
verified against the fixtures, in the three-way parity corpus at small N.
Gate: every program byte-identical across interp, native, and wasm, and
`vyrn fmt --check` clean.

**M2 — the harness and the numbers.** Same-discipline C, Rust, and JS sources;
a runner that pins or records every toolchain, runs N times, takes medians,
and commits JSON plus the environment record. Gate: the committed dataset
reproduces on a second run within stated noise, and every Vyrn deviation from
Rust beyond noise has a named cause filed in this document.

**M3 — the interactive chart.** The radar as specified above, on `/compare`,
with the backstage carrying the full data. Gate: the chart works without
script (table fallback), the interactions work with keyboard alone, and the
page passes the site's accessibility pass (RFC-0105 M4's checklist).

## M0 — as landed

Gate met: the table above has no "probably" cell, and no cell in it is an
expectation. Every row is an answer a program produced.

### Method

Expressibility was decided by running code, never by reading the compiler. Where
a row's doubt was a single capability, the probe is the smallest program that
exercises it. Where the whole program was small enough to write, the probe *is*
the program at the fixture's N, because "it prints the expected bytes" is a
stronger answer than "the pieces exist" — six of the ten are answered that way.

All fourteen probes live in `rfcs/bench-0104/` beside the fixtures and the
reference generator. Each was run three ways — interpreter, native, and wasm
under wasmtime 46.0.1 — and **all fourteen are byte-identical across the three**.
One note for M2's runner, found here: a native Windows build writes `\r\n` where
the interpreter and wasm write `\n`, so a comparison has to normalize line
endings before it diffs.

No compiler code was touched. M0 is `rfcs/` and nothing else.

### The probes and what they printed

**`p-sqrt.vyrn`** — is there a square root for one `Float64`? The first version
imported one, and that is a compile error:

```
p-sqrt.vyrn:7:0: `…/std/math.vyrn` does not define `sqrt`
```

`F64x2.sqrt(F64x2(x, x)).lane(0)` is the answer, and a hand-written Newton
iteration is not a substitute — 40 iterations land one ULP low, which is a
printed digit in a benchmark that prints nine of them:

```
1.414214
1.414214
false
-1
0.037037
```

**`p-fmt9.vyrn`** — `print` on a `Float64` is fixed at six decimals and the game
prints nine. Nine are reachable by scaling into an `Int64`, and the digits are
right:

```
-0.169075164
-0.169087605
1.274219991
0.000000000
1.000000000
```

**`p-nbody.vyrn`** — nbody at n = 1000, whole, including `bodies[i].vx = …` in
the innermost loop. Record fields carry no `mut` (`p-nbody.vyrn:9:9: expected
identifier, found Mut`); an ordinary field is assignable through the index.
Matches `nbody-1000.expected`:

```
-0.169075164
-0.169087605
```

**`p-spectralnorm.vyrn`** — spectral-norm at N = 100, whole. Matches
`spectralnorm-100.expected`:

```
1.274219991
```

**`p-fannkuch.vyrn`** — fannkuch-redux at n = 7, whole. The first version wrote
the flip as an ordinary helper and the movecheck refused it:

```
p-fannkuch.vyrn:20:0: `a` may not be returned — it is a second name for the `read` parameter `p`, and a return is owned
  fix: declare the parameter `p: consume ..` if this function should own it
```

`consume` in and owned out is a move each way and no copy. Matches
`fannkuch-7.expected`:

```
228
Pfannkuchen(7) = 16
```

**`p-trees.vyrn`** — binary-trees at n = 10, whole, on `type Tree = | Leaf |
Node(Tree, Tree)`. The natural shape needs no `.copy()` and no explicit release
anywhere. Matches `binarytrees-10.expected`:

```
stretch tree of depth 11	 check: 4095
1024	 trees of depth 4	 check: 31744
256	 trees of depth 6	 check: 32512
64	 trees of depth 8	 check: 32704
16	 trees of depth 10	 check: 32752
long lived tree of depth 10	 check: 2047
```

**`p-fasta.vyrn`** — fasta at n = 1000, whole. 10,245 bytes, byte-identical to
`fasta-1000.expected`, so the game's LCG and its floating-point pick reproduce
exactly.

**`p-revcomp.vyrn`** — reverse-complement over `fasta-1000.expected` on stdin,
byte-identical to `revcomp-1000.expected`. `readLine` carried the whole input.
One friction found: `for b in line` over a `String` yields `Int64` where the
byte table holds `UInt8` (`p-revcomp.vyrn:66:0: push value is Int64 but the array
holds UInt8`), so each line goes through `bytes` — an allocation a line.

**`p-mapkey.vyrn`** — k-nucleotide's map. Reproduces the 1-mer and 2-mer sections
of `knucleotide-1000.expected` exactly (`T 31.520 / A 29.600 / C 19.480 /
G 19.400`, then twenty 2-mer rows). Two costs are now facts rather than guesses:
one `stringFromBytes` per window — an allocation and a UTF-8 validation per
position of the sequence — and an ordering that no API supplies, since `sortBy`
takes an `Int64` key and the game's order is count descending with ties by
fragment. Swapping two records inside an array is refused outright:

```
p-mapkey.vyrn:97:0: `es[j]` may not be stored into `es` — it is read out of a place that owns it
  fix: `es[j].copy()` if both sides need a value
```

**`p-regex.vyrn`** — the row the RFC left open. `=~` is an anchored full match
against a compile-time-constant pattern, so a sequence that contains the pattern
twice answers `false`:

```
true
true
false
true
occurrences of agggtaaa|tttaccct: 2
```

The third line is `dna =~ "agggtaaa|tttaccct"` over a string holding two
occurrences. The fourth is the same question wrapped in `.*(…).*`, which answers
presence and nothing else. The count on the last line is a hand-written scan: one
anchored test per offset over a window whose width is hard-coded from outside,
because nothing asks the engine how wide a match can be — quadratic, and
impossible for a pattern of variable length. Substitution has no API at all
(`std/strings`' `replace` takes a literal `from`). regex-redux is **not
expressible**; a program that carried its own matcher would not be the idiomatic
line this RFC charts.

**`p-binout.vyrn`** — mandelbrot's output:

```
P4
4 4
string from packed bytes: bytes are not valid UTF-8
string from ASCII bytes: ok, P4
```

Both sinks take a `String`, so a packed PBM row cannot leave the program by
either. `writeFile` is the same refusal one step later — it takes two `String`
arguments.

**`p-mandelbrot.vyrn`** — the pixels are right even though they cannot be
printed. The probe computes 200×200 and prints the packed bytes as hex;
`ref/checkhex.py` puts `mandelbrot-200.expected` into the same form:

```
header: same
body: same
```

**`p-pidigits.vyrn`** — the row the RFC called "not expressible". `Int64` wraps
rather than widening, which is the absence; and the bounded spigot does not need
it to widen, which is the answer. Matches `pidigits-27.expected`:

```
9223372036854775807
-9223372036854775808
3141592653	:10
5897932384	:20
6264338   	:27
```

**`p-spawn.vyrn`** — spectral-norm's matrix-vector product over two tasks, same
N = 100, prints `1.274219991`. This probe carried a correction of its own: it was
written with `v.copy()` per worker, on the assumption that a spawned call moves
its arguments into the task. It does not. `v` is a `read` parameter, both tasks
read the same array, and the copies came out. A read-only input needs no
per-worker duplicate in any of these programs.

### Fixture provenance

Ten fixtures, one per program, named `<program>-<N>.expected` for M1's harness.
`rfcs/bench-0104/ref/gen.py` writes all of them and is the provenance: each
routine is the game's published algorithm transcribed from its specification —
the constants, the LCG, the output formats — not a copy of any entry's source.
Re-running it must reproduce the ten byte for byte.

The transcription is checked against four numbers the game itself publishes
(nbody's `-0.169075164` / `-0.169087605`, spectral-norm's `1.274219991`,
fannkuch-redux's `228` and `Pfannkuchen(7) = 16`, pidigits' `3141592653`), and
six of the ten are additionally reproduced by a probe — a second implementation
in a second language. `fasta-1000.expected` is also the stdin of
reverse-complement, k-nucleotide and regex-redux, so it is one file and not two.
`mandelbrot-200.expected` is binary and `.gitattributes` marks it `-text`.

`knucleotide-1000.expected` breaks equal counts by fragment ascending. The game
publishes no rule for a tie because its own input has none at k = 1 or k = 2;
this is a fixture-level decision, recorded so M1 does not have to rediscover it.

### What contradicted the RFC

Four claims in this document were wrong, and the corrections are made where the
claims were:

1. **`F64x2` is not missing.** It shipped in RFC-0083 M4 — two `Float64` lanes,
   with `min`, `max`, `sqrt`, `/`, `load`/`store` and `Mask64x2`. "mandelbrot
   measures the missing `F64x2`" measured something that was already here.
   mandelbrot's real gap is smaller and more boring: **there is no way to write
   bytes out**. That is one absence blocking one program's output, not a SIMD
   hole.
2. **pidigits is expressible and does enter.** "not expressible — the bignum gap"
   and "cannot even enter" are both false: `p-pidigits.vyrn` prints the fixture
   using `Int64` alone. The gap is real but narrower and differently placed — no
   big integer in `std`, which is a library absence, not a language one, and the
   game's rule (arbitrary-precision arithmetic) is what a conforming entry would
   need it for. `std/num`'s `f64Str` is already limb arithmetic written in Vyrn,
   so nothing in the compiler is in the way.
3. **regex-redux is the one program that is out.** The table said "probably"; the
   answer is no. This is now the arc's most concrete missing feature: a runtime
   regex that searches, counts and replaces.
4. **nbody and spectral-norm are not free.** The table called them expressible
   and left it there. They are — the probes print the game's numbers — but two
   things a reader would assume exist do not: there is no scalar `sqrt` (one lane
   of `F64x2` stands in) and no way to print nine decimals (the program carries a
   formatter). Both are `std` work items M1 will either fix or carry.

A fifth, smaller, was in the census's own expectations rather than this
document's: `spawn` was expected to move an argument into the task, and does not.

## What this RFC does not do

- It does not vendor Benchmarks Game sources. Reference ceilings are recorded
  numbers with dates.
- It does not add a chart library, a benchmark framework, or any dependency.
- It does not promise wins. It promises that every loss is a named defect or a
  named missing feature, and that the idiomatic line is the one on the chart.
