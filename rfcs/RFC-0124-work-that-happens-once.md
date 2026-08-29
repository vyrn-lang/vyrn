# RFC-0124 — Work That Happens Once

- **Status:** Implemented (2026-08-29) — M1, the generation-side memo, with
  the runtime side recorded as already answered.
- **Depends on:** RFC-0021 (generation targets and the sandbox), RFC-0029
  (module state — the runtime answer), the std-quality census's question two
  (rfcs/census/std-quality/README.md), which this RFC answers.
- **Evidence:** pattern 3 of the census — constant tables, schemas and
  audits rebuilt inside hot paths in seven modules — plus the per-request
  recomputation in `std/openapi`, `std/http` and `std/rpc`. And the census's
  own diagnosis: "the shape of the answer is a place for once-only work that
  a generator may also use. That is a language question, not eleven library
  patches."

## The question, split where it actually splits

"Where does work that happens once live?" has two halves with different
answers, and conflating them is what made it look unanswered:

**At runtime, the answer already exists.** Module state (RFC-0029) is the
place: initialize once, read forever. `std/http`'s mount audit — the row
that said it had "no init hook to hang it on" — now runs until it passes
once and stands for the process, and its census row closed. Nothing new is
needed on this half, and this RFC adds nothing there.

**At generation time, module state is refused** — deliberately, because a
generator is comptime-pure and state would let one target's run observe
another's. That refusal is exactly where the remaining payers live:
`std/openapi` re-resolving its schema per emit, tables rebuilt per call
inside generators. The gap is real and it is generation-side only.

## The theorem that makes the generation answer free

The generation sandbox is already pure BY ENFORCEMENT: no module state, all
I/O mediated and recorded, fuel-bounded. Purity is precisely the hypothesis
of referential transparency: **for a pure function, caching its result
cannot change any program's meaning.** That is the whole proof, and it is a
standard one — the design work is not inventing a mechanism but making the
hypothesis TRUE rather than assumed, because the sandbox's purity has two
honest leaks: a generator MAY log (diagnostics), and its mediated reads are
effects in the small (they are recorded into the cache key).

So the memo takes exactly the calls where the hypothesis holds:

> Inside a generation target, a call to a **nullary** function whose
> **transitive effects are none** is evaluated at most once.

"Effects are none" is decided by a one-sided screen
(`interp::effect_free_nullary`), computed once per generation target from
the program's call graph: a logging builtin, a mediated or host I/O
builtin, any `extern` (time and random arrive as externs), or a call to a
name the screen cannot see through (a `fn`-typed parameter) marks a
function effectful, and effect spreads caller-ward to a fixpoint. A
function the screen excludes only loses the memo — never correctness. A
trapping function needs no special row: only an `Ok` result is ever
cached, so a `panic` still fires where it always did.

Nullary on purpose, and not as a limitation: a nullary pure function IS a
constant — the census's pattern 3 is constants rebuilt per call — so the
memo is one value per function with no keying question, no argument
hashing, and no memory bound to defend. Memoizing pure calls WITH arguments
is a strict extension with the same theorem behind it; it waits for a payer
whose keys are worth hashing.

## What landed (M1)

- `GenCtx` carries the screen's answer and the per-target memo; the memo
  lives and dies with one generation target, so no state crosses targets
  and the sandbox's isolation argument is untouched.
- The hook is at the interpreter's single call seam (`call_capturing`):
  a memoized hit returns the cached value; everything else is exactly the
  code that ran before.
- The compiled generation engine (RFC-0076) does not memoize yet. By the
  theorem this is a PERFORMANCE difference only — both engines compute the
  same values — and the parity contract is about values. The engine can
  adopt the same screen whenever its numbers ask for it.
- The screen is pinned by a unit test from both sides (the pure table
  builder and `main` are in; a printer, a caller-of-a-printer, an extern
  caller, and a `fn`-typed-callee user are out).

## Measured

The site export — the most generator-heavy program in the repository
(vyx pages, i18n, tw) — drops from 16.7 s to 13.8 s wall, an 18 per cent
whole-program win, with byte-identical output under the full site gate.
No source changed; the payers' existing "wasteful" spellings became the
efficient ones, which is the census's requested outcome: one language
answer instead of eleven library patches.

## What this RFC deliberately does not add

- No `once` keyword, no grammar: the screen makes marking redundant for the
  constant case, and grammar is the most expensive thing to add.
- No memoization by arguments (recorded extension, above).
- No cross-target or cross-run cache: the generator CACHE (RFC-0021's
  content-addressed one) already answers cross-run; this answers
  within-run.
- No runtime mechanism: module state is the runtime answer, demonstrated by
  the census rows that closed with it.
