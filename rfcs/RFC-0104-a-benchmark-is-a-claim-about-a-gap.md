# RFC-0104 — A Benchmark Is a Claim About a Gap

- **Status:** **M0, M1 and M2 landed** (the census, see
  [M0 — as landed](#m0--as-landed) and `rfcs/bench-0104/`; the eight programs,
  see [M1 — as landed](#m1--as-landed) and `examples/`; the harness and the
  numbers, see [M2 — as landed](#m2--as-landed) and
  `rfcs/bench-0104/harness/` + `rfcs/bench-0104/results/`). M3 proposed.
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

**M1 — the programs.** *Landed — see [M1 — as landed](#m1--as-landed).* The
expressible set, written idiomatically, output-verified against the fixtures,
in the three-way parity corpus at small N. Gate: every program byte-identical
across interp, native, and wasm, and `vyrn fmt --check` clean.

**M2 — the harness and the numbers.** *Landed — see
[M2 — as landed](#m2--as-landed).* Same-discipline C, Rust, and JS sources;
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

## M1 — as landed

Gate met on all three legs. The eight expressible programs are in `examples/`,
they are byte-identical across the interpreter, the native binary and wasm, they
print the M0 fixtures byte for byte, and `vyrn fmt --check` passes on every one.
No compiler code was touched: M1 is `examples/`, one test file and this section.

### The programs

| program | example | census N | fixture | bench N |
|---|---|---|---|---|
| nbody | `examples/nbody.vyrn` | 1000 steps | `nbody-1000.expected` | 50,000 steps |
| spectral-norm | `examples/spectralnorm.vyrn` | N = 100 | `spectralnorm-100.expected` | N = 500 |
| fannkuch-redux | `examples/fannkuch.vyrn` | n = 7 | `fannkuch-7.expected` | n = 9 |
| binary-trees | `examples/binarytrees.vyrn` | depth 10 | `binarytrees-10.expected` | depth 14 |
| fasta | `examples/fasta.vyrn` | n = 1000 | `fasta-1000.expected` | 250,000 bases |
| reverse-complement | `examples/revcomp.vyrn` | that fasta on stdin | `revcomp-1000.expected` | 2,000,000 bases |
| k-nucleotide | `examples/knucleotide.vyrn` | that fasta on stdin | `knucleotide-1000.expected` | 200,000 bases, k = 2 and k = 18 |
| pidigits | `examples/pidigits.vyrn` | 27 digits | `pidigits-27.expected` | 1000 digits |

**regex-redux and mandelbrot are not here, and their absence is the boundary
rather than an omission.** Neither was worked around. `=~` is an anchored full
match against a compile-time-constant pattern — it answers neither "how many"
nor "where", and there is no substitution by pattern — so regex-redux needs a
runtime regex that searches, counts and replaces. mandelbrot's pixels are right
and cannot leave the program: `print` and `writeFile` both take a `String` and
`stringFromBytes` refuses a packed row, so it needs a byte sink.
`regexredux-1000.expected` and `mandelbrot-200.expected` stay committed with no
program beside them, which is what a named gap looks like in a corpus.

### Where each program deviates from its probe

Seven of the eight were whole programs in M0 and are promoted rather than
rewritten: the algorithms, the constants and the output formats are the probes'.
Only k-nucleotide's probe stopped short of its fixture. What changed elsewhere
is what a reader would notice.

- **Every program.** The compute is a named function and `main` is the print, so
  the bench block and `main` run the same code rather than two copies of it. The
  N a program runs at is a `let` at the top with the fixture's value beside the
  bench's, so "what size is this" is answered in the first ten lines.
- **nbody, spectral-norm.** `sqrtF` and `fixed9` are duplicated in both files
  rather than shared. That is deliberate: a Benchmarks Game entry is one file,
  M2 compares each against a single-file C, Rust and JS source, and the two
  helpers are the census's finding — the cost of no scalar `sqrt` and no
  nine-decimal `print` should be visible in the program that pays it.
  `spectralnorm`'s `fixed9` gained the sign branch its probe left out; the value
  is a norm and cannot be negative, but a formatter a reader might copy should
  not be wrong for the case it does not meet.
- **fannkuch-redux.** The probe copied `perm1` element by element into a fresh
  array; `perm1.copy()` is the one-line spelling and works. The `consume`-in
  owned-out `flip` is unchanged — it is the language's real answer and M2 prices
  it. The walk returns a `Fold` record instead of printing from inside itself,
  and the one explicit `drop` in the eight programs is here: `foldCount` owns the
  copy it folds and nothing else wants it back.
- **binary-trees.** Unchanged apart from the split into `checkAll` and
  `iterationsFor`. The probe's finding holds exactly: no `.copy()` and no
  explicit `drop` anywhere in the file.
- **fasta.** The 60-column width is one `lineWidth` rather than three copies of
  the same `if`, and the weight tables are named functions. The generator's seed
  stays module state, and the program now says why: the game specifies ONE
  stream, so THREE continues where TWO left off and a per-call seed would
  silently restart it.
- **reverse-complement.** The probe complemented into 60-byte lines as it walked
  backwards. M1 splits that into `reverseComplement` (the whole transformation)
  and `writeWrapped` (the printing), because the bench block needs the first
  without the second and because it is the shape a reader would write. The `for
  b in bytes(l)` allocation a line is unchanged and still commented.
- **k-nucleotide.** The probe read `fasta-1000.expected` with `readFile` and
  printed the 1-mer and 2-mer sections. M1 reads stdin like the game, keeps only
  the THREE sequence a line at a time so the whole input is never in memory, and
  prints the whole fixture including the five named fragments. Nothing new was
  needed for the fragment section: `countOf` builds the table for that width and
  looks the fragment up, rather than scanning for the one string, because the
  table IS the benchmark and a scan would measure something the other languages
  are not doing. The hand-written insertion sort and its three `.copy()` stay —
  see the findings below.
- **pidigits.** `guard` is a named constant and the `wraps()` demonstration is
  gone: it belonged to the census's question, not to the program. The rules
  caveat moved into the file's own header, so a reader of `examples/pidigits.vyrn`
  meets it before the code.

### Corpus wiring, and why

- **Flat files in `examples/`.** The parity loop reads `examples/*.vyrn`
  non-recursively, so a `examples/bench/` subdirectory would have put all eight
  outside the gate this milestone exists to enter.
- **N is a `let` in the program, not `args()`.** No `.args` fixtures. Reading
  argv would be the game's own interface, but the corpus needs one deterministic
  run per program and M2's timing surface is the bench block, not a process
  argument — so a constant is the whole of it, and there is one fewer file per
  program to keep in step.
- **Two `.stdin` fixtures, each a copy of `fasta-1000.expected`.** The harness's
  rule is one fixture per example (`examples/<name>.stdin`, RFC-0014), so
  `revcomp.stdin` and `knucleotide.stdin` are copies rather than one shared file.
  M0 wrote that its directory holds one copy and not two; that decision was about
  `rfcs/bench-0104/` and does not reach the corpus. The copies cannot drift:
  `the_stdin_fixtures_are_the_fasta_output` compares both against
  `fasta-1000.expected` on every `cargo test`.
- **Bench blocks coexist with parity, verified.** Tests and benches are stripped
  before `run` and `build` (RFC-0055), so the three engines never see a `bench`
  block and the corpus output is `main`'s alone. All eight are in the parity run
  and all eight are byte-identical.
- **The bench blocks that cannot read stdin build their own input.**
  `vyrn bench` does not run `main` and has no fixture piped into it, so
  `revcomp` and `knucleotide` generate a synthetic sequence in the block — which
  is RFC-0055's own rule ("each block builds what it reads") rather than a
  concession. `fasta`'s block generates and assembles lines without writing
  them, and says so: a sample that spends itself in a pipe measures the pipe.
- **The bench corpus is a pinned list, and it moved.**
  `bench_corpus_is_exactly_the_bench_bearing_examples` in
  `compiler/vyrn-cli/tests/benching.rs` names every example carrying a `bench`
  block, because CI's blocking `--check` step discovers that set by grep and a
  silently lost row would be a gate that stopped gating. It went from five
  entries to thirteen. The eight add about 40 seconds to that step: `--check`
  runs each block once under the INTERPRETER, so a native median of 22.9 ms is
  11.7 seconds there.
- **Sample sizes.** Native medians are 1.8 ms (nbody), 8.0 ms (spectral-norm),
  22.9 ms (fannkuch-redux), 13.6 ms (binary-trees), 4.6 ms (fasta), 2.2 ms
  (reverse-complement), 10.5 ms and 32.9 ms (k-nucleotide), 9.4 ms (pidigits) —
  all far enough above timer noise for M2 to work with, all small enough that
  `vyrn bench --check` on the whole set is seconds.

### The gate that outlives the milestone

`compiler/vyrn-cli/tests/benchgame.rs` runs each program under `vyrn run` and
compares its bytes against the M0 fixture, with line endings normalized. It
needs no clang, no wasmtime and no build, so a plain `cargo test` runs it.

It exists because parity is the wrong gate for this arc on its own. Parity says
the three engines agree, and three engines can agree on a wrong answer. A
benchmark is a measurement of the thing it names only while its output is still
the game's output, and the moment a program is edited for speed that is exactly
what stops being true silently. M2 will write tuned variants; this is where one
that stopped printing the fixture fails.

### What contradicted the census

Two, both in the same place, and both found by writing k-nucleotide's sort
properly rather than by reading the compiler.

1. **The refusal to swap two records inside an array is not about records — it
   is about whether the element type is named.** M0 recorded
   `es[j] may not be stored into es — it is read out of a place that owns it` as
   a fact about arrays of records. But `std/arrays`' own `sortBy` is an insertion
   sort that performs precisely that swap over `Array<T>`, with no `.copy()`
   anywhere, and `knucleotide.vyrn` calls it with `Array<Entry>` — so the
   accepted generic monomorphizes into the shape the concrete spelling is
   refused for. One rule, two answers, decided by whether the element type is a
   parameter or a name. The three `.copy()` in `ranked` are still there, because
   they are what the compiler asks for at that spelling; what M0 got wrong is the
   scope of the rule, not the cost.
2. **The first fix the diagnostic offers does not exist for an array element.**
   The refusal reads
   `fix: `consume es[j]` if `es` should give it up — the field is dead
   afterwards`. Taking that advice produces
   `es[..] may not be taken — an element is not a place a take reaches`, whose
   own fix is `swapRemove`, which is not a swap. So the guidance's first branch
   is dead for exactly the container it fires on most.

Neither is M1's to fix — a language gap a program hits is a finding, not a scope
change — and neither changes a printed byte. They are recorded here because the
census's method is that a claim about the language is answered by running code,
and these two were answered by running it.

## M2 — as landed

Gate met. Five contestants, eight programs, every one byte-identical to every
other at the timing size before anything was timed; two full measurement runs
whose medians agree to 1.9% (median) and 9.5% (worst); and every deviation of
Vyrn native from Rust beyond that noise named below, with the two largest
answered by an experiment rather than by an opinion.

No compiler code was touched. M2 is `rfcs/bench-0104/harness/`,
`rfcs/bench-0104/results/` and this section.

**CI does not run any of this.** `rfcs/**` is CI-ignored, and that is
deliberate: the harness needs clang, rustc, node and wasmtime together, it takes
about fifteen minutes a run, and a number measured on a shared CI runner is not
a number. The harness is run by hand and **the committed JSON is the record**.
What CI still gates is that the eight programs keep printing the fixtures —
`compiler/vyrn-cli/tests/benchgame.rs`, from M1.

### The harness

`rfcs/bench-0104/harness/` — `c/`, `rust/`, `js/`, one file per program per
language, and `run.py`, which builds all five contestants, verifies them,
times them and writes the record.

- **Discipline on the other side.** Plain C11: no intrinsics, no threads, no
  OpenMP, `<stdio.h>`/`<stdlib.h>`/`<string.h>`/`<math.h>` only. Safe Rust,
  std only: no `unsafe`, no external crate, `BufWriter`/`BufRead` where a book
  would, borrowed slice keys where a book would. Plain node: no
  `worker_threads`, no addon, no dependency, typed arrays and `Map` because
  those are ordinary JavaScript. Where the idiomatic form of a language does
  structurally less work than the Vyrn program, it was written that way and the
  difference is reported as a finding — that is the point of the milestone, not
  a leak in it.
- **Flags, stated.** `clang -O2 -ffp-contract=off -std=c11` for C;
  `rustc -C opt-level=3` for Rust; no build for node. The two C flags are not a
  choice — they are what `vyrn build` itself passes clang
  (`add_native_clang_flags` in `compiler/vyrn-cli/src/main.rs`:
  `-O2 -ffp-contract=off -Wno-override-module`, and no `-march` on the default
  x86-64 target). Without `-ffp-contract=off` the C leg's last printed digit
  would differ from everyone else's and the cross-check would fail for a reason
  that has nothing to do with speed. The Vyrn legs are `vyrn build` and
  `vyrn build --target wasm`, and the wasm one runs under the wasmtime the
  repository's own lock pins — `run.py` reads `vyrn.lock` for the version and
  the sha256 rather than trusting an environment variable, so the record cannot
  claim a wasmtime the repo does not pin.
- **N reaches the Vyrn programs through a temp copy.** M1 decided N is a `let`
  in the program and not `args()`, so there is nothing to pass. The runner does
  not edit `examples/*.vyrn`. It copies the source into its build directory and
  rewrites exactly the one line matching `^let <name> = <number>$` — the match
  must be unique or the run aborts — and compiles the copy. The check that this
  is sound is in the verification pass: a copy stamped with the FIXTURE N is
  built and must still print the fixture, which is what proves the stamp moved
  the number and nothing else. C, Rust and node take N on the command line.
- **Whole-process wall time**, the game's own convention, stdout discarded,
  `perf_counter` around the process, ten runs, median reported with the raw
  runs kept. So the number includes start-up, and for the wasm leg it includes
  wasmtime's compile — `--floor` measures an empty program in each contestant
  so a reader can subtract it: **C 4.1 ms, Rust 4.3 ms, Vyrn native 4.1 ms,
  Vyrn wasm 14.3 ms, node 30.9 ms.**

### The sizes

Chosen so the Vyrn **native** leg lands roughly 0.5–5 s. reverse-complement and
k-nucleotide have no N in the source; their size is the FASTA piped in, which
the runner generates with the C fasta (itself verified against the fixture, and
against the other four at fasta's own timing N).

| program | census N | timing N | Vyrn native |
|---|---|---|---|
| nbody | 1,000 steps | 25,000,000 steps | 0.87 s |
| spectral-norm | N = 100 | N = 5,500 | 0.98 s |
| fannkuch-redux | n = 7 | n = 11 | 2.65 s |
| binary-trees | depth 10 | depth 18 | 1.90 s |
| fasta | n = 1,000 | n = 5,000,000 (50 M bases) | 0.92 s |
| reverse-complement | that fasta | fasta n = 4,000,000 (40 M bases) | 1.07 s |
| k-nucleotide | that fasta | **fasta n = 4,000** (20,000-base THREE) | 2.89 s |
| pidigits | 27 digits | 12,000 digits | 1.29 s |

k-nucleotide's N is three orders of magnitude below reverse-complement's, and
that is the first finding rather than a mistake: see the cause below. **There
is no N at which both ends of that row are in band** — at fasta n = 4,000 the C
and Rust legs are 10.1 ms and 8.5 ms, near their 4 ms floor; at fasta n = 8,000
they are still under 20 ms and the Vyrn leg is 13.4 s. The rule was kept on the
Vyrn side and the asymmetry is recorded here.

### The numbers

`rfcs/bench-0104/results/2026-08-19-LOCUST.json` and
`2026-08-19-LOCUST-run2.json`. Both were measured from the same commit and the
same binaries; the second record's `worktree_clean: false` is the first
record's own JSON sitting untracked beside it, and nothing under `compiler/`
differs between them. Medians of ten runs, milliseconds:

| program | N | C | Rust | node | Vyrn native | Vyrn wasm |
|---|---|---|---|---|---|---|
| nbody | 25,000,000 | 898 | 658 | 1114 | 866 | 13029 |
| spectral-norm | 5,500 | 974 | 964 | 1388 | 981 | 3019 |
| fannkuch-redux | 11 | 1857 | 1891 | 2656 | 2654 | 4090 |
| binary-trees | 18 | 807 | 1799 | 437 | 1904 | 1085 |
| fasta | 5,000,000 | 653 | 478 | 882 | 921 | 3420 |
| reverse-complement | 4,000,000 | 288 | 69 | 497 | 1071 | 13318 |
| k-nucleotide | 4,000 | 10.1 | 8.5 | 40 | 2888 | 3038 |
| pidigits | 12,000 | 1280 | 1288 | 3064 | 1291 | 1968 |

Normalized to C = 1.00:

| program | C | Rust | node | Vyrn native | Vyrn wasm |
|---|---|---|---|---|---|
| nbody | 1.00 | 0.73 | 1.24 | **0.97** | 14.52 |
| spectral-norm | 1.00 | 0.99 | 1.43 | **1.01** | 3.10 |
| fannkuch-redux | 1.00 | 1.02 | 1.43 | **1.43** | 2.20 |
| binary-trees | 1.00 | 2.23 | 0.54 | **2.36** | 1.35 |
| fasta | 1.00 | 0.73 | 1.35 | **1.41** | 5.23 |
| reverse-complement | 1.00 | 0.24 | 1.73 | **3.72** | 46.29 |
| k-nucleotide | 1.00 | 0.84 | 4.01 | **287** | 302 |
| pidigits | 1.00 | 1.01 | 2.39 | **1.01** | 1.54 |

And against Rust, which is the comparison the gate is written about — Vyrn
native divided by Rust: nbody **1.32**, spectral-norm **1.02**, fannkuch-redux
**1.40**, binary-trees **1.06**, fasta **1.93**, reverse-complement **15.4**,
k-nucleotide **340**, pidigits **1.00**.

### The noise, stated numerically

Two full runs, back to back, same machine, nothing else running. The gate's
"within stated noise" is decided by the second run:

- **Run-to-run median drift**, over all forty program/contestant cells: median
  **1.9%**, 95th percentile **8.6%**, worst **9.5%** (binary-trees / Rust).
  Thirty-six of the forty cells moved by less than 5%.
- **Spread within a run** (max − min as a percentage of the median): under 15%
  for thirty-eight of forty cells. The two outliers are k-nucleotide's C
  (27.8%) and Rust (23.4%) cells, which are 10 ms and 8.5 ms measurements
  against a 4 ms process floor — the noisiest cells in the set, and labelled as
  such wherever they are used.

**So the noise band is ±10% on a median**, and every ratio called a deviation
below is outside it by a wide margin. Three rows are inside it and are
therefore **not** deviations: spectral-norm (1.02× Rust), pidigits (1.00×) and
binary-trees (1.06×).

### The named causes

The claim this RFC opened with is "same LLVM, same discipline, same numbers —
every measured deviation is a named defect or a named missing feature". Five
rows deviate. Here is each one, and what was ruled out where something was.

**nbody — 1.32× Rust, 0.97× C. Cause: the vectorizer runs on rustc's IR and not
on ours.** The emitted assembly answers it. Rust's `advance` is auto-vectorized
and unrolled — 342 packed-double instructions and four `sqrtpd`, 1,236 lines.
Vyrn's `vyrn_advance` is scalar: thirteen scalar float ops, one `sqrtsd`, 184
lines. The C leg is scalar too (three `sqrtsd`, no `sqrtpd`), and loses to Rust
by 1.40× — the same order Vyrn does. So this is not a Vyrn-specific defect; it
is a difference between what two LLVM front ends tell the same optimizer, and
Vyrn is on the clang side of that line because it hands clang textual IR. What
that IR does not carry is the reason to look at next: **it contains no
`noalias` at all** (zero occurrences in the whole module) and no loop metadata,
so the facts the ownership model already proves are not written down where the
optimizer can read them.

Three hypotheses were tested and killed before that one was kept:

1. *"Rust wins because the five bodies are a fixed-size stack array
   (`[Body; 5]`) and Vyrn's `Array<Body>` is always heap."* Measured: the same
   Rust program with `Vec<Body>` runs in **609 ms** against `[Body; 5]`'s
   **640 ms** — the heap version is *faster*. Not it.
2. *"The `F64x2` square-root shim costs a lane."* The emitted assembly for
   `vyrn_advance` contains exactly one `sqrtsd`: the two-lane spelling folds to
   a single scalar instruction. Not it.
3. *"Bounds checks and the `consume`-in/owned-out move."* Vyrn native (866 ms)
   is faster than plain C (898 ms), which has neither. Whatever they cost, it
   cannot account for a loss.

**fannkuch-redux — 1.40× Rust, 1.43× C. Cause: `perm1.copy()` allocates a fresh
array per permutation** — 39,916,800 of them at n = 11. Rust reuses one scratch
`Vec` with `clone_from`; C copies into one preallocated buffer. Measured, not
argued: giving the C leg the same malloc/free per permutation and changing
nothing else moves it from **1830 ms to 2430 ms**, so the allocation accounts
for about 600 ms of the 763 ms gap. The language has no way to copy into an
array that already exists; `.copy()` is the whole API and it always allocates.

**fasta — 1.93× Rust, 1.41× C. Cause: what a 60-byte output line costs.** The
program builds a fresh `Array<UInt8>` per line, grown from empty by doubling
(4, 8, 16, 32, 64 — five reallocations for sixty bytes), and then calls
`stringFromBytes`, which is a byte-at-a-time UTF-8 validation over bytes that
are known to be ASCII plus a second malloc'd NUL-terminated copy — because
`print` takes a `String` and there is no byte sink. Measured: that exact
sequence, transcribed into C and run on its own, costs **160 ns a line**, which
over 833,334 lines is **133 ms of the 268 ms gap to C**. Rust reuses one 60-byte
`Vec` and writes it into a `BufWriter`: no validation, no per-line allocation.
**The byte sink is not a new gap** — it is the one M0 already named for
mandelbrot. What M2 adds is that it is not only mandelbrot's: it taxes every
line of output in two more programs.

**reverse-complement — 15.4× Rust, 3.7× C. Cause: three per-byte passes and the
same per-line tax, on top of an input path C shares.** For 40 M bases the
program performs about 120 M `push` operations — once building `seq` from the
input, once building the complement, once filling each output line — each a
bounds/capacity check into an array grown by doubling. Rust's
`extend_from_slice` moves the input in one memcpy per line and `bs.chunks(60)`
is a *view*, so its output loop allocates nothing at all: 69 ms against C's
288 ms. The per-line `stringFromBytes` tax above accounts for ~107 ms of the
783 ms Vyrn loses to C; the rest is the per-byte pushes. The input side is
shared with the C leg and is why C is itself 4.2× Rust here: `readLine` in the
native shim reads one byte at a time with `getchar()`.

**k-nucleotide — 340× Rust, 287× C. Cause: `Map` is not a hash map.** In every
engine, `Map<String, V>` is an insertion-ordered vector of pairs and lookup is a
linear scan — `__vyrn_map_find` is literally
`for (i = 0; i < len; i++) if (strcmp(keys[i], key) == 0) return i;`. So a
program that counts every k-mer is **quadratic in the number of distinct keys**,
and k-nucleotide is exactly that program. Measured on the Vyrn native binary,
input quadrupling each step:

| fasta n | THREE bases | Vyrn native | C |
|---|---|---|---|
| 2,000 | 10,000 | 0.78 s | 0.04 s |
| 4,000 | 20,000 | 2.92 s | 0.01 s |
| 8,000 | 40,000 | 13.4 s | 0.02 s |
| 16,000 | 80,000 | 55.0 s | 0.03 s |

Four times the input is 4.1–4.6× the time: quadratic, against C's flat line.
Three further costs sit on top of it and are small only by comparison — one
`stringFromBytes` per window (an allocation and a UTF-8 validation per position,
which M0 did name), two scans per increment because `m[key]` and
`m[key] = seen + 1` each search from the start, and the hand-written O(n²)
insertion sort with three `.copy()` per swap that stands in for a comparator
`sortBy` does not take.

### Vyrn wasm against Vyrn native — the optimizer gap, priced

RFC-0101 §2.3/§2.4 asked for a number. It is not a number; it is a range, and
the shape of the range is the finding:

| program | wasm ÷ native | |
|---|---|---|
| nbody | **15.0×** | tight scalar float loop |
| reverse-complement | **12.4×** | per-byte `fd_read` (below) |
| fasta | 3.7× | |
| spectral-norm | 3.1× | |
| fannkuch-redux | 1.5× | |
| pidigits | 1.5× | |
| k-nucleotide | 1.05× | both ends are the same linear scan |
| binary-trees | **0.57×** | the wasm leg **wins** |

The direct backend (RFC-0077) runs no optimizer at all, so 15× on nbody's
register-hungry float loop is the honest top of the range. Two rows say
something the average would hide:

- **reverse-complement's 12.4× is not the optimizer.** `direct.rs` reads one
  byte per WASI `fd_read`, and the source already says so in a `ponytail:`
  comment naming exactly this ceiling ("one `fd_read` per byte, where C's
  `getchar` is buffered"). At 40 M bases that is 40 M syscalls. This is a
  deliberate shortcut that M2 has now priced.
- **binary-trees is faster with no optimizer than with `clang -O2`.** The wasm
  backend carries its own allocator — a segregated free list over size classes
  with an eight-byte header — while the native leg's `__vyrn_malloc` is a thin
  wrapper over the platform `malloc`, which on Windows is the UCRT heap. On the
  one benchmark that is nothing but allocate-and-release, the backend's own
  allocator beats the platform's by enough to cover having no optimizer.

### Where node stands

node is between 0.54× and 4.01× C, and it is not the slowest contestant on any
row. It **wins binary-trees outright** (0.54× C, 4.4× faster than Vyrn native):
a generational collector allocating short-lived trees by bumping a pointer beats
`malloc`/`free`, which is the row's whole point. It is worst on k-nucleotide
(4.01× C) and pidigits (2.39×) — and still 71× faster than Vyrn native on the
first of those. It also carries the largest start-up in the set at 30.9 ms,
which is why its advantage on small rows is understated here.

### The work items

Each is one fixable cause named above. None is in M2's scope; all of them are
what this milestone exists to produce.

1. **A hashed `Map`.** Lookup is a linear `strcmp` scan in both backends
   (`__vyrn_map_find`); the ordered-vector representation can keep insertion
   order with an index beside it.
2. **Byte-slice map keys**, so a k-mer window is not a `String` allocation and a
   UTF-8 validation per position.
3. **One search per update**, so `m[k] = m[k] + 1` does not scan the map twice.
4. **A comparator, or a secondary key, for `sortBy`**, so "count descending,
   ties by fragment" stops being a hand-written insertion sort with three
   `.copy()` per swap.
5. **A copy into an array that already exists**, so `.copy()` inside a loop
   stops allocating (fannkuch-redux: 600 ms of 763 ms, measured).
6. **A byte sink for `print` and `writeFile`** — already named for mandelbrot in
   M0, and now measured taxing fasta and reverse-complement at 160 ns a line.
7. **Capacity reservation and a bulk append for `Array`**, so a 60-byte line is
   one allocation rather than five doublings and sixty checked pushes.
8. **Buffered stdin**: `getchar()` per byte in the native shim, one `fd_read`
   per byte in the direct wasm backend.
9. **Loop facts in the emitted IR.** No `noalias` anywhere and no loop metadata,
   so the ownership model's proofs never reach the optimizer.
10. **An allocator of Vyrn's own on the native leg.** The wasm backend's
    segregated free list beats the platform `malloc` by enough to win
    binary-trees while running no optimizer.

### What contradicted the RFC

1. **"Same LLVM, same discipline, same numbers" is too strong as written, and
   the reason is not the language.** Feeding `clang -O2` the same source shape
   Rust compiles does not put a program on Rust's footing: on nbody the
   vectorizer runs on rustc's IR and not on the IR clang gets, and the C leg
   loses by the same 1.4×. The claim survives as a claim about the *language* —
   monomorphized, no collector, static releases — and it now has a stated
   exception: same LLVM is not the same optimizer decisions, and the emitted IR
   carries none of the facts that would change them.
2. **M0's k-nucleotide row named the wrong cost.** It recorded the per-window
   `stringFromBytes` and the missing comparator, and both are real. It did not
   record that `Map` is a linear scan, because `p-mapkey.vyrn` measured only
   k = 1 and k = 2 — four keys and sixteen, where a linear scan is free. The
   cost the census could not see is the one that dominates the row by two orders
   of magnitude. The census's own method is what caught it: a claim about the
   language is answered by running code, at a size where the claim can fail.
3. **The wasm line is not "the slow one".** This document said its distance from
   native "prices RFC-0101's §2.3/§2.4 endpoints with a number instead of an
   adjective". One number was the wrong shape: the range is 0.57× to 15.0×, and
   on binary-trees the backend that runs no optimizer is the faster of the two.
4. **"Why should it lose everywhere?"** — the question this arc started from —
   is answered, and the answer is that it does not. Three of the eight rows are
   at parity with Rust inside the noise band, and on nbody Vyrn native is faster
   than C. The losses are five, they are all allocation, string or container
   costs rather than code quality, and every one of them now has a work item.
5. **The 0.5–5 s sizing rule cannot be applied symmetrically.** It was written
   as if one N could put every contestant in band. k-nucleotide has no such N,
   because the two ends of that row are not on the same curve.

## What this RFC does not do

- It does not vendor Benchmarks Game sources. Reference ceilings are recorded
  numbers with dates.
- It does not add a chart library, a benchmark framework, or any dependency.
- It does not promise wins. It promises that every loss is a named defect or a
  named missing feature, and that the idiomatic line is the one on the chart.
