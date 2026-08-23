# Census — How Fast Languages Implement Strings

Research for [RFC-0108](RFC-0108-the-string-scan-is-interpreted.md). Two halves:
what the fastest implementations actually do, and what Vyrn actually costs.
Every Vyrn number below is measured on this repository, not estimated.

The short answer to "maybe we need multiple implementations?" is **yes for the
search algorithm and no for the string type**, and the measurements in §3 are
why. Four plausible redesigns died to a stopwatch before this document reached
its recommendation.

---

## 1. The search algorithm: everyone ships several, and picks at run time

No fast implementation uses one algorithm. Every one surveyed dispatches, and
three of the four adapt *during* the search.

| implementation | what it dispatches on | how it adapts mid-search |
| --- | --- | --- |
| Rust `memchr::memmem` | needle length; target ISA (x86-64, aarch64, wasm32) | a SIMD prefilter finds candidates, and a **dynamic heuristic detects when the prefilter is ineffective and turns it off** |
| CPython `fastsearch.h` | needle length (≤5 → plain loop) | a **Bloom filter** over the pattern's bytes tests the window's last byte; a miss skips the whole needle length |
| Go `strings.Index` | needle length, haystack length | **counts `IndexByte` false positives** and cuts over to `bytealg.Index` when the count exceeds a budget |
| StringZilla | needle length **and** instruction set (SWAR, NEON, AVX2, AVX-512, SVE) | — |

The common shape: a cheap filter that is wrong sometimes, plus a guaranteed-linear
fallback, plus a rule for noticing the filter is not earning its keep. Rust states
the goal directly — combine the fast SIMD path while keeping Two-Way's complexity
guarantee, so the whole thing is O(m+n) with constant space.

The fallbacks are the same two algorithms nearly everywhere: **Two-Way**
(Crochemore–Perrin) for the linear guarantee, and **Boyer–Moore–Horspool** style
bad-character skipping for the sublinear common case. CPython layers both, plus
Sunday's shift.

**Why this matters for Vyrn:** substring search is a *pure function*. Every one
of these implementations returns the same index for the same input — they differ
only in how fast they get there. That is exactly the property the
`interp == native == wasm` parity gate needs, and it means Vyrn can use a
*different* implementation per backend without weakening the gate. Contrast float
formatting, where implementation differences are observable and parity forbids
them.

### The wasm question RFC-0108 has to answer anyway

`memchr` has vector implementations for **wasm32 `simd128`**, and falls back to
**SWAR** (SIMD-within-a-register: 64-bit integer arithmetic that tests eight
bytes at once) where no vector unit exists. SWAR is the important one for Vyrn:
it needs no target features, no runtime detection, and it is portable to every
backend including the interpreter. It gives a large fraction of the SIMD win
without any of the ISA machinery.

**Recommendation:** if RFC-0108 proceeds, the first implementation should be
SWAR, not SIMD. One body, three backends, no feature detection, no `-C
target-feature=+simd128` requirement on the wasm build.

---

## 2. The string type: the fastest runtimes use several, mostly to save memory

Here the survey is more interesting, because the multiple representations are
generally **not** about search speed.

**V8** carries the most: `SeqString` (the only one that holds bytes),
`ConsString` (a lazy concatenation node, used above `kMinLength = 13`),
`SlicedString` (a substring that borrows rather than copies), `ThinString`, and
`ExternalString` — plus one-byte and two-byte variants of the sequential form.
Anything that needs contiguous bytes must first *flatten* the tree. The purpose
is to make concatenation and slicing O(1) and pay the copy only if someone looks.

**Java** (JEP 254, "compact strings") stores `byte[]` plus a `coder` flag, Latin-1
or UTF-16, because measurement showed strings dominate heap and most contain only
Latin-1. Every `String` method has two specialisations and the hot ones are
replaced by JIT intrinsics.

**C++** uses small-string optimisation, and the two standard libraries disagree
on the layout: libc++ is 24 bytes with 22 inline; libstdc++ is 32 bytes with 15
inline, and is reported roughly 4.5x faster for strings up to 15 characters. The
point of SSO is avoiding a heap allocation for a *value* type.

**Text editors** are the one place the structure is chosen for edit speed, and
even there no winner exists: a gap buffer wins most benchmarks but degrades on
very large files; a piece table (VS Code) gives cheap undo and memory-mapped
originals; a rope gives O(log n) worst case at higher constants and, for a
200-line file, loses to a gap buffer on every operation.

---

## 3. What Vyrn actually does, measured

Vyrn's `String` is `Rc<String>` — contiguous UTF-8 bytes, copy-on-write, with
`Rc::make_mut` used so an accumulator can grow in place.

Four redesigns suggested themselves from the survey above. All four were
measured. **All four were already handled, or were not the problem.**

| hypothesis from the survey | what the measurement said |
| --- | --- |
| `out = out + piece` is quadratic, so Vyrn wants V8's `ConsString` or a rope | **Not quadratic.** 1000 / 2000 / 4000 / 8000 appends: 0.08s / 0.10s / 0.07s / 0.09s. Flat across 8x the work — the `Rc::make_mut` accumulator path already gives V8's benefit without the tree. |
| `Map<String, V>` is a linear scan (an old note said so) | **Already fixed.** `MapVal` is ordered pairs plus a `HashMap` index over them; the index gives O(1) lookup while the `Vec` keeps iteration order deterministic for parity. |
| escaping dominates the render — `escapeText` walks every byte with four comparisons and an `Array` push | ~**2 MB/s**, the same class as everything else. Real, not dominant. |
| `bytes(s)` materialises one boxed `Val` per byte, so the String↔bytes bridge dwarfs the loops | ~**23.9 MB/s** — 12 to 16 times faster than the interpreted loops that consume it. Not the bottleneck. |

What *is* true, from RFC-0108 §4: interpreted scanning runs at about **1.5 MB/s**,
and a burn-slope measurement (one extra pass per scanned byte, warm export 56.9s
to 64.4s) puts substring scanning at **26–33% of a render**.

### The conclusion these four measurements force

**Vyrn does not have a data-structure problem.** It has the string type the fast
implementations converged on — contiguous UTF-8, refcounted, copy-on-write, with
an in-place append path. The representations V8 and Java carry solve problems
Vyrn does not have:

- `ConsString` makes concatenation O(1) — Vyrn's accumulator already is.
- `SlicedString` makes substrings free — worth considering, but `slice` returns
  an owned `String` and Vyrn's ownership model makes a borrowing substring a
  language question, not a representation question.
- Java's Latin-1/UTF-16 split exists because Java chose UTF-16 in 1995. Vyrn's
  String is UTF-8 bytes by definition and never had that problem.
- SSO avoids a heap allocation for a value type; `Rc<String>` already avoids the
  copy, which is the cost that matters here.

What remains is **per-operation interpreter overhead, spread across every
primitive**, of which substring search is between a quarter and a third.

---

## 4. Recommendations, in the order they should be tried

1. **One native body, several implementations inside it.** Follow the universal
   pattern: a cheap skip filter plus a linear-guaranteed fallback. Start with
   Boyer–Moore–Horspool bad-character skipping plus SWAR candidate finding —
   CPython's shape without its Two-Way tier. Add Two-Way only if a measured
   adversarial case demands the guarantee.
2. **SWAR before SIMD** (§1). Portable across all three backends, no feature
   detection, and it keeps the parity gate honest for free.
3. **Do not add a second String type.** §3 measured away every motivation the
   survey offered. A `SlicedString`-style borrowing substring is the only one
   worth revisiting, and it is a language design question about ownership, not a
   performance fix.
4. **Answer RFC-0108 §6 before any of this.** The `.vyx` compile phase is 110s of
   the 118s generator cost and it *lexes* rather than searches. If lexing does
   not use substring search, everything above is worth a third of 57s and none of
   110s. The interpreter-only prototype named in §6 settles it in an afternoon.

---

## 5. What this census does not cover

- SIMD instruction selection per ISA. Deliberately — recommendation 2 says SWAR
  first, and the ISA question only arises if SWAR proves insufficient.
- Sorting, hashing, edit distance. StringZilla accelerates all of these; none of
  them appear in Vyrn's measured hot path.
- Rope and piece-table structures for the playground editor. Different problem
  (interactive editing of one large buffer), different document.

## Sources

- Rust `memchr` / `memmem` — https://github.com/BurntSushi/memchr and https://docs.rs/memchr/
- `memchr` wasm `simd128` support — https://github.com/BurntSushi/memchr/pull/84
- CPython `fastsearch.h` — https://github.com/python/cpython/blob/main/Objects/stringlib/fastsearch.h and https://github.com/python/cpython/blob/main/Objects/stringlib/stringlib_find_two_way_notes.txt
- Go `internal/bytealg` — https://pkg.go.dev/internal/bytealg and https://go.dev/src/bytes/bytes.go
- StringZilla — https://github.com/ashvardanian/StringZilla and https://ashvardanian.com/posts/stringzilla/
- V8 string representations — https://iliazeus.lol/articles/js-string-optimizations-en/ and https://github.com/danbev/learning-v8/blob/master/notes/string.md
- Java JEP 254, compact strings — https://openjdk.org/jeps/254
- C++ SSO layouts — https://tastyhedge.com/blog/memory-layout-of-std-string/ and https://tc-imba.github.io/posts/cpp-sso/
- Editor structures — https://coredumped.dev/2023/08/09/text-showdown-gap-buffers-vs-ropes/ and https://en.wikipedia.org/wiki/Piece_table
- Two-way string-matching algorithm — https://en.wikipedia.org/wiki/Two-way_string-matching_algorithm
