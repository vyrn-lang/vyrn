# RFC-0104 — A Benchmark Is a Claim About a Gap

- **Status:** **Proposed.** No implementation. Milestones below; a milestone
  that fails its gate says so in this file.
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
missing `F64x2`. pidigits measures the missing bignum and cannot even enter.
The chart this arc builds is a roadmap that updates itself.

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

M0's census fixes this table; the expectation going in:

| program | needs | expectation |
|---|---|---|
| nbody, spectral-norm | Float64 arrays, math | expressible; within noise of Rust or a defect found |
| fannkuch-redux | Int arrays | expressible; same |
| binary-trees | allocation stress | expressible; measures RFC-0091's model against arenas |
| fasta, reverse-complement | bytes and stdio | expressible (RFC-0014) |
| k-nucleotide | `Map<String, V>` over bytes | expressible (RFC-0028); measures the map against hashbrown |
| regex-redux | regex over large input | probably; the census answers |
| mandelbrot | f64 math; `F64x2` to compete | expressible slowly — the SIMD gap, named |
| pidigits | arbitrary-precision integers | **not expressible** — the bignum gap, named |

Parallelism: the game's winning entries use every core. Vyrn has `spawn`/
`join` and no data-parallel utilities. The harness compares single-threaded
implementations across all five contestants, and additionally shows a
spawn-parallel Vyrn variant where one is natural — so the parallel gap is
measured, not hidden.

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

**M0 — the census.** One row per game program: expressible or not, what it
needs, the expected-output fixture for a small N checked into the corpus
directory. The bignum and `F64x2` absences recorded as facts with the programs
that measure them. Gate: the table above rewritten with no "probably" cell.

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

## What this RFC does not do

- It does not vendor Benchmarks Game sources. Reference ceilings are recorded
  numbers with dates.
- It does not add a chart library, a benchmark framework, or any dependency.
- It does not promise wins. It promises that every loss is a named defect or a
  named missing feature, and that the idiomatic line is the one on the chart.
