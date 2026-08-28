# The regular-expression gap — what blocks regex-redux, and what would unblock it

Status: **CLOSED. Option 1 was chosen and built** —
[RFC-0112](../RFC-0112-a-regular-expression-that-searches.md), 2026-08-25.
`std/regex` is a Thompson NFA written in Vyrn, and `examples/regexredux.vyrn`
prints the committed fixture — nine counts and three lengths — under all three
engines.

Option 1 rather than this file's first-ranked Option 2, and for the reason
Option 2 was ranked first: the generator's value was compile-time pattern
validation, and every one of regex-redux's fifteen patterns is a literal of its
own source, so a refusal at run time is a panic on the first line of `main`. The
generator is a strict addition on top of the walker whenever something wants it,
because the walker does not change either way. This file's central finding — that
the parity risk is in the walk and not in the table builder — is what decided it,
and putting the walk in Vyrn made parity a property rather than a test.

## Why this file exists

`rfcs/RFC-0104-a-benchmark-is-a-claim-about-a-gap.md:408-416` records the gap:

> **regex-redux and mandelbrot are not here, and their absence is the boundary
> rather than an omission.** Neither was worked around. `=~` is an anchored full
> match against a compile-time-constant pattern — it answers neither "how many"
> nor "where", and there is no substitution by pattern — so regex-redux needs a
> runtime regex that searches, counts and replaces. mandelbrot's pixels are right
> and cannot leave the program: `print` and `writeFile` both take a `String` and
> `stringFromBytes` refuses a packed row, so it needs a byte sink.

The census row at `rfcs/RFC-0104-a-benchmark-is-a-claim-about-a-gap.md:85` names
it `p-regex`: "**The named gap: a runtime regex — search, count, replace**". The
probe findings at lines 275-295 add the specifics: a sequence containing the
pattern twice answers `false`; counting is a hand-written scan over a window
whose width must be hard-coded from outside; substitution has no API at all
(`std/strings`' `replace` takes a literal `from`, `std/strings.vyrn:291`).

**The statement is still true.** Checked on 2026-08-23 against the sources
below: `=~` still takes only a string-literal pattern
(`compiler/vyrn-frontend/src/checker.rs:4295-4310`) and still answers one `Bool`
for the whole string (`checker.rs:5471-5474`). No search, count, position, or
pattern-substitution primitive exists anywhere in `std/`.

## Exactly what =~ supports today

- **Token.** `=~` lexes as `TildeMatch` (`compiler/vyrn-frontend/src/lexer.rs:79`),
  distinct from `~` applied to integers (`fmt.rs:676-678` notes the disambiguation).
- **Literal-pattern rule.** "``=~`` requires the right operand to be a *string
  literal* (the pattern is compiled to a DFA at compile time) and that pattern
  must be valid" — `checker.rs:4295-4310`. A variable pattern is rejected:
  `"the right side of \`=\~\` must be a string-literal pattern"`
  (`checker.rs:4308`), pinned by `regex_operator_requires_literal_pattern`
  (`checker.rs:12840-12853`), which also pins compile-time rejection of invalid
  patterns such as `[z-a]`.
- **Result.** `Bool`, nothing else — "`=~` matches a String against a regex
  literal → Bool" (`checker.rs:5471-5474`). It answers neither how many nor
  where, and there is no capture or span of any kind.
- **The grammar.** From the engine's own header (`compiler/vyrn-frontend/src/regex.rs:1-23`):
  alternation `|`, grouping `()`, `.`, escapes `\d \w \s \D \W \S` and `\<c>`,
  classes `[a-z]` with negation and rejected reversed ranges, quantifiers
  `* + ? {m} {m,} {m,n}` with counted repetition expanded structurally and
  bounds capped at 255. There are NO anchors in the grammar because matching is
  ALWAYS anchored. Rejected by omission: backreferences, lookaround, lazy
  quantifiers, flags, named groups. Two budgets keep compilation finite:
  `EXPANSION_BUDGET = 16_384` nodes of counted-repetition expansion
  (`regex.rs:76-81`) and `PARSE_DEPTH_BUDGET = 250` nesting levels
  (`regex.rs:83-88`). Documented caveat: the engine is byte-wise, so on
  multi-byte input `.` and negated classes count bytes, not code points
  (`regex.rs:20-23`).
- **The machine.** A parsed tree becomes a complete DFA: every state has all 256
  transitions and "a dead state absorbs a non-match"
  (`compiler/vyrn-codegen/src/direct.rs:14374-14377`). The type is `Dfa { start,
  accepting, table }` (`regex.rs:519-536`) and the whole matcher is:
  ```rust
  pub fn matches(&self, s: &str) -> bool {
      let mut st = self.start as usize;
      for &byte in s.as_bytes() {
          st = self.table[st * 256 + byte as usize] as usize;
      }
      // accepting[st]
  ```
  (`regex.rs:531-536`) — anchored start and end by construction.
- **Where each backend gets it.** The compiler builds the DFA once
  (`vyrn_frontend::regex::compile`, memoised, `regex.rs:1132-1136`). Three
  runners walk the same table: the interpreter calls `compile(..).matches(..)`
  directly (`interp.rs:6474-6478`; consteval too, `consteval.rs:152-155`); the
  textual native backend emits the table plus the fixed LLVM runner
  `@__vyrn_regex_run` (`compiler/vyrn-codegen/src/lib.rs:469-486`, collection at
  `lib.rs:1643-1646`); the direct backend emits the table and a generated
  `regex_run` function (`direct.rs:5608-5620`, `5697-5703`, `14374-14389`),
  which comments at `direct.rs:14380-14384` call "three spellings of one walk"
  kept identical by having ONE source for the table.
- **Reuse beyond the operator.** The same DFAs power validated-string
  refinement types, including intersection for `&&` conjunctions
  (`compiler/vyrn-frontend/src/finite.rs:3-5`, `59-66`, `828-832` region) and
  JSON Schema `pattern` reflection, which anchors with `^…$` precisely because
  Vyrn's match is full-match (`compiler/vyrn-frontend/src/schema.rs:938-941`).

## Can the existing DFA compiler serve a searching engine?

This is the central question. Answer in two halves.

**The compiler: yes, unchanged.** Searching is derivable without touching the
table builder. Two standard routes, both documented in the prior-art survey
below: prefix the program with `.*?` so the VM searches in one linear pass (Russ
Cox, regexp2, "Unanchored matches"), or — the equivalent edit on this codebase's
complete-DFA representation — let the start state absorb any byte that does not
begin progress, and treat the dead state as "restart from start", not "fail".
Either way `Re` → NFA → `Dfa` (`regex.rs`) produces a usable table for search.

**The runners: no — each is built around the anchored assumption.** All three
runners consume the entire string and test acceptance once, at the end
(`regex.rs:531-536`; `lib.rs:469-486`; `direct.rs:14374-14389`). A searching
engine needs a runner that reports EVERY accepting position, restarts through
the dead state, and for substitution records spans. That runner does not exist,
and under the current structure it would have to be spelled three times —
Rust for the interpreter, IR for the textual backend, generated wasm-IR for the
direct backend — which is exactly the triplication `direct.rs:14380-14384`
already complains about for the full-match walk, now with harder parity
obligations (offsets and counts must agree byte-for-byte, not just a Bool).

So: the expensive half (parsing, expansion budgets, determinisation, the table
format) is reusable today. The cheap-looking half (the walk) is where the new
parity risk concentrates. This is the argument that pushes options 1 and 2
below ahead of option 3: both put the NEW walker in ONE place.

## What regex-redux actually needs

From the Benchmarks Game description page (archived copy read 2026-08-23:
https://benchmarksgame-team.pages.debian.net/benchmarksgame/description/regexredux.html
— text verified via https://web.archive.org/web/20231219095945/ same path):

Each program reads a FASTA file from stdin and must:

1. "use the same simple regex pattern match-replace to remove FASTA sequence
   descriptions and all linefeed characters", recording the length before and after;
2. "(one pattern at a time) count matches" for these nine patterns, printing
   each pattern and count:
   ```text
   agggtaaa|tttaccct
   [cgt]gggtaaa|tttaccc[acg]
   a[act]ggtaaa|tttacc[agt]t
   ag[act]gtaaa|tttac[agt]ct
   agg[act]taaa|ttta[agt]cct
   aggg[acg]aaa|ttt[cgt]ccct
   agggt[cgt]aa|tt[acg]accct
   agggta[cgt]a|t[acg]taccct
   agggtaa[cgt]|[acg]ttaccct
   ```
3. "one pattern at a time, in the same order" replace these five, recording the
   final length:
   ```text
   tHa[Nt]              -> <4>
   aND|caN|Ha[DS]|WaS   -> <3>
   a[NSt]|BY            -> <2>
   <[^>]*>              -> |
   \|[^|][^|]*\|        -> -
   ```

Operations needed, exactly: **replace-by-pattern** over the whole sequence
(once to clean, five times chained), **counted matches** (nine scans),
**length**. Not needed: captures, lookaround, backreferences, case-insensitive
matching, and — the point that decides the hosting question — **no pattern is
chosen at run time**. All fifteen patterns are literals of the program source.
The committed fixture agrees: nine counted rows then the three lengths
(`rfcs/bench-0104/regexredux-1000.expected`, read 2026-08-23).

## Prior art

Six surveys were collected independently. Sizes marked MEASURED were counted by
the surveying pass from the cited source; anything else is NOT MEASURED.

### Rust `regex` (rust-lang/regex 1.13.1)

- Guarantee: "all regex searches in this crate have worst case `O(m * n)` time
  complexity"; "doesn't use unbounded backtracking … meant to be able to run
  regex searches on untrusted haystacks without fear of ReDoS"
  (https://docs.rs/regex/latest/regex/).
- How: a meta engine over finite automata in `regex-automata` — lazy DFA
  (builds at most one state per byte searched, bounded configurable cache;
  https://docs.rs/regex-automata/latest/regex_automata/hybrid/), fully-compiled
  DFA behind a non-default feature, one-pass DFA, a visited-bitmap Bounded
  Backtracker, and a Pike VM as the always-works fallback
  (https://docs.rs/regex-automata/latest/regex_automata/).
- Syntax given up, and why: look-around and backreferences — maintainer:
  "general look-around will never be supported … it isn't known how to implement
  arbitrary look-around in an efficient manner"
  (https://github.com/rust-lang/regex/discussions/910). Atomic groups and
  possessive quantifiers go with them.
- Size: MEASURED from the 1.13.1 tarball — core three crates 137,972 lines of
  Rust across 147 files; whole workspace 160,774 lines / 227 files; `regex-lite`
  (same syntax minus Unicode and performance tuning, linear bound KEPT) is about
  9,770 lines — a realistic floor for "linear-time engine".
- Needs Vyrn lacks: a bounded growable DFA cache, per-search scratch arenas,
  compile-time size limits, general heap allocation. None need threads.

### RE2 (google/re2)

- Guarantee: "One of its primary guarantees is that the match time is linear in
  the length of the input string"
  (https://github.com/google/re2/blob/main/README.md).
- How: parse → simplify → flat bytecode program, executed by four engines
  chosen per pattern: one-pass (`re2/onepass.cc`, 629 lines), bitstate
  backtracker linearised by a visited bitmap (`re2/bitstate.cc`, 395 lines,
  header: "limits the search to run in time linear in the length of the text"),
  Thompson NFA simulation (`re2/nfa.cc`, 720 lines), lazy DFA with memory budget
  and NFA fallback (`re2/dfa.cc`, 1,416 lines; default budget 8 MiB, `re2.h`
  line 671). All sizes MEASURED at commit `972a15ce`.
- Syntax given up: "As a matter of principle, RE2 does not support constructs
  for which only backtracking solutions are known to exist. Thus, backreferences
  and look-around assertions are not supported." (README; full NOT-SUPPORTED
  tables at https://github.com/google/re2/wiki/Syntax, repeat counts capped at
  1000.)
- Size: core library ≈ 24,984 lines of C++ (no tests); largest single file is
  generated Unicode tables (`unicode_groups.cc`, 6,518 lines); the four
  execution engines together ≈ 4,350 lines. MEASURED.
- Needs Vyrn lacks: hash maps for DFA-state dedup (replaceable with RE2's own
  array-based sparse sets, `re2/sparse_set.h`), mutexes/atomics for cache
  sharing (deletes entirely without threads), budget accounting (plain
  integers). No closures, no GC, no recursion.

### Thompson construction and the Pike VM

- Guarantee: Thompson's 1968 construction plus lock-step simulation of ALL
  live threads gives O(n·m) worst case — "there are no regular expressions that
  are pathological for the Thompson NFA implementation"
  (https://swtch.com/~rsc/regexp/regexp1.html). The Pike VM adds submatch
  tracking via a `save` instruction while keeping duplicate-PC thread
  elimination (https://swtch.com/~rsc/regexp/regexp2.html).
- Why backreferences break it: two threads at the same PC normally cannot
  differ in future behaviour; with backreferences their capture sets differ and
  both must be kept — "a potentially exponential blowup in state" (regexp2).
- Unanchored search from an anchored engine: retrying at every offset is
  quadratic; the efficient form puts `.*?` at the front of the program, "lets
  the VM itself implement the unanchored search in a single linear-time pass"
  (regexp2, "Unanchored matches"). RE2 refines this to `.*re` plus an end check
  per byte and a first-byte memchr fast path (regexp3).
- Size: MEASURED — Cox's teaching engine `nfa.c` is 425 lines total
  (https://swtch.com/~rsc/regexp/nfa.c.txt); `thompson.c` ≈ 107 lines and
  `pike.c` 135 lines in rsc/re1 (https://github.com/rsc/re1); Plan 9's
  production libregexp is well under 1,500 lines
  (https://github.com/0intro/plan9/tree/main/sys/src/libregexp). Industrial
  engines are kilo-line-plus but almost all of that is syntax surface and
  Unicode, not the VM cores.
- Needs Vyrn lacks: nothing. Two preallocated thread lists, a sparse-set
  duplicate check (https://research.swtch.com/sparse — plain arrays plus a
  stamp integer), fixed-size capture arrays. Flat arrays and integers only.

### Hyperscan (intel/hyperscan; successor VectorCamp/vectorscan)

- Guarantee: simultaneous matching of "up to tens of thousands" of patterns
  with no allocations at scan time beyond fixed scratch/stream state sized at
  compile time (https://github.com/intel/hyperscan; doc/dev-reference/intro.rst).
  Streaming mode keeps per-stream state so matches can span blocks
  (doc/dev-reference/compilation.rst).
- Regex-redux fit: many short literal-ish patterns counted over one big input is
  the FDR multi-string matcher's headline case — 8 bytes per step through
  128-bit SIMD shift-or masks (NSDI'19 paper §4.1,
  https://www.usenix.org/system/files/nsdi19-wang-xiang.pdf; paper reports
  2.15 s versus PCRE's 6,942 s on 1,300 parallel regexes).
- Syntax given up: backreferences AND capturing sub-expressions, arbitrary
  zero-width assertions, subroutine/recursion forms, atomic groups and
  possessive quantifiers — compile errors, not silent approximations
  (compilation.rst, Unsupported Constructs). Greedy/lazy distinction deliberately
  erased because it is uncomputable in one streaming pass.
- Size: MEASURED — 155,118 lines of C++ in `src/`; ~240k first-party lines
  repo-wide including tests and tools (commit `809f09b6`).
- Needs Vyrn lacks: SSSE3 minimum with AVX2/AVX-512 paths (x86-only story; wasm
  v128 could carry parts, the pure interpreter backend forfeits the throughput);
  enormous compile-time graph analysis (dominator trees, max-flow, Boost);
  Ragel/Python build tooling. Out of scale as something to port; relevant as
  proof the many-patterns case is linear and allocation-free at scan time.

### Go `regexp` (golang/go src/regexp)

- Guarantee: "guaranteed to run in time linear in the size of the input"
  (package doc, https://github.com/golang/go/blob/master/src/regexp/regexp.go).
- How: THREE engines selected per pattern — one-pass NFA (`onepass.go`),
  a backtracker bounded by a visited bit-vector with hard limits
  `maxBacktrackProg = 500`, `maxBacktrackVector = 256*1024` bits
  (`backtrack.go:29-33`), and a Pike VM over sparse-set thread queues
  (`exec.go`). Unlike RE2's C++ there is NO lazy DFA. Leftmost-first Perl
  semantics preserved despite automata execution.
- Syntax given up: RE2's delta versus PCRE — backreferences, lookaround,
  possessive/atomic groups, recursion (https://github.com/google/re2/wiki/Syntax,
  mirrored in src/regexp/syntax/doc.go); repeats above 1000 rejected.
- Size: MEASURED from master — src/regexp 2,801 lines + src/regexp/syntax
  3,863 lines ≈ **6,664 lines of Go**, tests excluded. Execution engines ≈
  1,445 lines; parser alone 2,242 lines.
- Needs Vyrn lacks: GC-managed slices everywhere, interface dispatch over three
  input types, sync.Pool — all mechanically replaceable with flat arrays, tagged
  unions and preallocated buffers; per-rune decoding simplifies further because
  Vyrn strings are already validated UTF-8. Closest architectural template of
  the six for a from-scratch port.

### Why backtracking engines blow up

Mechanism: a backtracker explores one ambiguous path at a time and saves
positions to retry; when a nested quantifier can carve a run of n characters
into pieces in 2^(n-1) ways and failure is only provable at the end, the search
tree doubles per character. OWASP's measured example: `^(a+)+$` against 4 a's +
X explores 16 paths; 16 a's + X, 65,536 paths
(https://owasp.org/www-community/attacks/Regular_expression_Denial_of_Service_-_ReDoS).

A concrete case a Vyrn user could hit — an email-shaped validator:

```text
^([a-zA-Z0-9])(([\-.]|[_]+)?([a-zA-Z0-9]+))*(@){1}[a-z0-9]+[.]...$
```

against input `aaaaaaaaaaaaaaaaaaaaaaaa!` — the `(...)*` over a group holding
`[...]+` is textbook nested quantification; OWASP lists this real RegExLib
pattern among its Evil Regex examples (same URL). Cox's timing benchmark: Perl
needs more than 60 seconds on a 29-character string where a Thompson NFA takes
20 microseconds (https://swtch.com/~rsc/regexp/regexp1.html).

Real-world cost: Cloudflare's 2019 global outage — one PCRE WAF rule
(effectively `.*.*=.*.*;`), CPU pegged on every edge core, 80% of traffic lost
for 27 minutes (https://blog.cloudflare.com/details-of-the-cloudflare-outage-on-july-2-2019/).
Immune engines: RE2, Go regexp, Rust regex (each cited above). Vulnerable:
PCRE, JavaScript RegExp, Python re, Perl (Cox 2007; Snyk
https://learn.snyk.io/lesson/redos/). Mitigations compared: automata (removes
the class structurally), memoization (O(m·n) time, O(m·n) memory), timeouts
(bandage; Cloudflare's protection had been removed in a refactor weeks before).

## Where does it live? Three options costed

### Option 1 — in `std/`, written in Vyrn

A Pike VM (or byte-DFA builder plus searching runner) in `std/`, portable to all
three backends by construction, exactly like `std/stream.vyrn:1-4` argues ("they
get interpreter == native == wasm parity for free").

Cost: the engine is ordinary Vyrn — arrays, integers, while loops; the survey
shows every data structure the technique needs is flat arrays plus integers.
Speed, using the ONLY measured number in the repository, the interpreted
scanning rate of about **1.5 MB/s** (`rfcs/census-strings.md:102-104`;
measured directly in `rfcs/RFC-0108-the-string-scan-is-interpreted.md:105-106`,
"20 full scans of a 90,890-byte document, 1.4s"):

- Work at the game's performance size — 5,000,000 bases
  (spec: "command line arguments: 5000000 > input5000000.txt"): cleaned
  sequence ≈ 5 MB. Passes: 1 clean-up replace + 9 counts + 5 replaces ≈ 15 ×
  5 MB = **75 MB of scanning**. At 1.5 MB/s ≈ **50 seconds** in the INTERPRETER.
- At this repository's own bench size, 250,000 bases
  (`RFC-0104:403`): ≈ 3.75 MB ≈ **2.5 seconds** interpreted.

Plainly said: yes — a Vyrn-hosted engine completes regex-redux at the game size
in about a minute even in the slowest backend, and the arithmetic above is a
ceiling, since a table-driven DFA walk moves less work per byte than the
substring scans that figure was measured on. Native and wasm speeds are NOT
MEASURED; RFC-0104's M2 timings show compiled Vyrn far above interpreted rates
but no MB/s number exists to cite. As a competitive benchmark ENTRY it loses;
as a closed gap with correct output it passes. A Pike VM's per-byte cost grows
with program length, so prefer one DFA per literal pattern (nine small DFAs)
over one big automaton.

What it cannot do well: compile-time validation of user patterns happens at run
time, per process; error messages and rejection of pathological patterns must be
re-built in Vyrn (the compiler's budgets, `regex.rs:76-88`, do not transfer).

### Option 2 — a compile-time generator

Vyrn generators are implemented: a `gen fn` "runs at compile time and
synthesizes a module", returning Vyrn SOURCE TEXT
(`rfcs/RFC-0021-generator-imports.md:12-19`, `26-31`). A generator
`import { search9 } from regexgen("agggtaa[cgt]|[acg]ttaccct")` would compile
the literal pattern to a table at compile time and emit — or import from
`std/` — the searching runner as ordinary Vyrn.

What this gives: compile-time pattern validation with the compiler's existing
budgets (the generator can reject pathological patterns during `vyrn check`);
zero interpreter overhead at run time beyond the emitted table walk; parity by
construction, because everything executed is plain Vyrn. Precedent for
comptime-pure engines living in `std/`: `std/tw.vyrn:5-7`, `std/json.vyrn:1-5`.

What it CANNOT do, stated plainly: a pattern that is only known at run time —
a `String` parameter — has no literal to generate from, so no library API of
the shape `count(haystack, pattern)` where `pattern` is a value. Whether
anything needs that is the owner's call, but **regex-redux does not**: all
fifteen patterns and the clean-up pattern are literals in the program source
(spec quoted above). Note the RFC's own phrase "a runtime regex" means
search/count/replace executed at run time, not a run-time-chosen pattern.

Cost: a pattern-to-Vyrn-code compiler written in comptime-pure Vyrn (parser +
NFA construction; the survey's sizes suggest several hundred to low thousands of
lines), plus the shared runner. Runs on whichever engine executes generators
(today the interpreter, optionally wasm — `compiler/vyrn-genwasm/`), on inputs
of tens of bytes, so the 1.5 MB/s rate is irrelevant here.

### Option 3 — a compiler builtin

New builtins (say `search`, `countMatches`, `replaceAll`) whose bodies live in
the compiler next to `crate::regex`, with per-backend lowerings like `=~` has
today.

Cost: fastest of the three and architecturally continuous — the honest
observation is that Vyrn ALREADY has a compiler-builtin regex engine: the
`Dfa` compiler lives in `compiler/vyrn-frontend/src/regex.rs`, and the three
runner spellings exist because the table has one source
(`direct.rs:14380-14384`). Extending it means one new searching walker per
backend instead of one shared Vyrn walker, plus checker signatures, capability
rows, SPAWN/COMPTIME list entries, and schema/reflection decisions for every
new operation.

And the rule — the census brief's standing backend rule, quoted here in full
because the brief file (`.claude/ox/RULES.md`, retired with the tool that read
it) no longer lives in the tree: "Do NOT add a native body for a
standard library function. Do NOT hard-implement any standard library behaviour
inside a backend. Do NOT write one implementation for one backend and a
different one for another … If a task looks like it needs a backend
implementation, stop and report that instead of writing one." Option 3 puts
match SEMANTICS (which syntax is accepted, what a match means) into
backend-owned code, which is the shape the rule forbids. The counter-reading —
that `=~` itself already lives there, so regex is language surface rather than
library behaviour — is exactly the kind of ruling the brief reserves to the owner
("Language syntax, standard library shape … are decided by the repository
owner"). Included so the owner sees the trade, not because it is available.

## Recommendation, not a decision

Ranked, with what supports each rank:

1. **Option 2 (generator) composed with Option 1's shared runner.** The
   generator owns parsing and validation at compile time; ONE searching/
   substituting walker, written in Vyrn in `std/`, serves all three backends —
   which is precisely where this file found the parity risk concentrated
   (three runner spellings). Covers regex-redux completely; covers the common
   literal-pattern case; costs a comptime pattern compiler. Speed ceiling at
   the game size: about 50 s interpreted (arithmetic above), faster compiled.
2. **Option 1 alone.** Same walker, run-time parsing in Vyrn. Smaller to ship,
   loses compile-time validation, gains the ability to accept any pattern —
   pick this if the owner wants the library-first shape.
3. **Option 3.** Fastest, most continuous with `=~`, and the one the standing
   rule says to stop and report rather than write. Owner's ruling required
   before anyone builds it.

Whichever option wins, the syntax bar to clear is the RE2/Go/Rust set: give up
backreferences and lookaround, keep the linear-time guarantee, and say so in the
docs — the catastrophic case is a property of backtracking, not of ambition.
