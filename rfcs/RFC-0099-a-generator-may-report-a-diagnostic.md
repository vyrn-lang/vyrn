# RFC-0099 — A Generator May Report a Diagnostic

- **Status:** **M1 landed.** The primitive, `std/diag`, and a non-web generator
  that proves a third party can use it. One of M1's claims died to measurement:
  the mechanism was specified as new, and half of it already existed —
  RFC-0071 M2b's `//@warning` is this directive with its severity nailed shut, so
  M1 is smaller than it was written to be and the older spelling survives as one
  match arm. A second claim died the same way: no LSP change was needed at all,
  because a report anchored at an input file already travels the road RFC-0053
  built for remapped errors. The one thing M1 did have to widen was the
  cross-engine gate, which asserted that generation SUCCEEDS — a corpus example
  that a generator refuses is now required to be refused by both engines in the
  same words instead.
- **Depends on:** RFC-0006 (structured diagnostics, `Severity`), RFC-0021
  (generator imports, the comptime sandbox, the gen cache), RFC-0033 (origin
  maps — the `path:line:col` notation and its resolution), RFC-0053 (generated
  error mapping, and the LSP routing of an input-anchored diagnostic), RFC-0071
  M2b (`//@warning`, `--deny-warnings`), RFC-0076 (generators as wasm, and the
  cross-engine byte-equality gate).
- **Research:** `docs/research/vyx-hints.md` — the audit of the vyx web stack
  against `nuxt/hints`, `html-validate`, Lighthouse, axe-core and OWASP. It
  ranked 60-odd checks and found that every one of them waits on the same
  missing seam. Its §7 named the seam; this RFC generalizes it away from the web
  before building it.
- **Principle:** the compiler gains one capability — a generator may report a
  diagnostic. Every rule about what is worth reporting is an ordinary library,
  written in Vyrn, replaceable by anyone.

---

## The question

A `gen fn` (RFC-0021) reads files and returns Vyrn source. It knows things about
those files that nobody else will ever know: which line of a `.vyx` template
carries the `img` with no `alt`, which line of a `.proto` declares a field
number twice, which column of a table definition holds a `String` with no
length. It has exactly one way to say so.

That way is to emit an identifier that does not resolve. `vyxErr`
(`std/vyx.vyrn` 518) synthesizes `VYX_MISSING_FOR_KEY__file__line_12` and lets
the checker fail on it. So a generator's every remark is a hard error, worded by
the parser, in a machine-shaped name the user must decode.

Two things follow. Advice cannot exist — a hint that fails the build is not a
hint, so the vyx layer ships zero of the a11y, performance and security checks
its own compile-time knowledge would make cheap. And a refusal cannot be worded
— the generator knows exactly what is wrong and gets to say `unknown identifier`.

Both halves of the fix were already in the tree, unjoined. `Severity::Warning`
exists and rides a load that succeeded (RFC-0071 M2b). `//@origin file:line:col`
already relocates a diagnostic from generated text onto the user's real file
(RFC-0033). `//@warning` already joins them — for one severity, chosen by the
compiler rather than by the generator.

## What this is not

It is not a web feature, an a11y feature, or a lint framework. The compiler
learns no rule, no rule name, no category, no target. It learns that a
generator may say something, at a place, at a severity.

This is the bet the project has already made five times. RPC, UI, i18n, OpenAPI
and GraphQL are libraries over `moduleInterface` rather than keywords over a
domain. A checking layer is the same shape: `moduleInterface` and `readFile` are
how a generator LEARNS something, and this is how it SAYS something. A generator
that emits SQL, protobuf, a Makefile or a C header uses it identically, and one
that targets another language entirely uses it identically, because there is
nothing in it about any language including Vyrn.

## The surface

A generator returns source text. One line of that text may be a report:

```text
//@diag <severity> <anchor> <message>
```

`std/diag` is the surface a generator calls, and it is two functions and an
enum:

```vyrn
export type Severity = | Warning | Error

/// A report anchored in an input file, at a 1-based `line` and `col`.
export fn report(severity: Severity, file: String, line: Int64, col: Int64, message: String) -> String

/// A report with no position to give — shown at the generated line it sits on.
export fn reportHere(severity: Severity, message: String) -> String
```

```vyrn
out = out + report(Warning, path, 9, 3, "column `email` has no length limit; write `String(120)`")
```

The functions return a string, because everything a generator produces is a
string. They exist for two reasons that are worth their twelve lines: a message
containing a newline would end the comment and hand the rest of itself to the
parser as source, and a 0 that a generator computed off by one would make the
whole anchor unparseable and silently drop the position. `report` fixes both.

### Why the report rides the output text

The obvious alternative is a call: a comptime builtin `reportDiagnostic(...)`
that pushes onto a list the loader reads back. It is refused, and the reason is
not taste.

Generated output is content-addressed and cached (RFC-0021): the second build
reads the text from `~/.vyrn/cache/gen` and never runs the generator. A
diagnostic delivered by a call happens during interpretation, so it exists on the
cold build and vanishes on every warm one. A diagnostic that IS output text is
cached with the output, and cannot be lost.

The same argument runs again at the cross-engine gate. RFC-0076 runs generators
as compiled wasm in the LSP and gates that engine by comparing `emit-gen` bytes
against the interpreter's. A side channel would need a second gate, written by
hand, for the diagnostics; text needs none, because the diagnostics ARE the
bytes being compared. Determinism, caching and cross-engine equality are three
properties this mechanism gets for free by not being a call.

## The anchor

`<anchor>` is `path:line:col` in exactly the notation `//@origin` uses, parsed
by the same function, resolved against the module that imported the generator.
Or it is `-`, meaning "no position".

The common case is correct by construction, and the reason is worth stating.
A generator's inputs arrive as constant path arguments written by the importer
(`table("./data/users.tbl")`), and those paths are already relative to the
importing module because that is how the loader resolved them. A generator that
anchors at a path it was GIVEN is therefore anchoring correctly without knowing
anything about the filesystem. Computing the line and column is its own job,
which it can only do for text it actually read.

Three degradations, all of which keep the report:

| the anchor is | the report |
| --- | --- |
| `-` | at the generated line, in the synthesized module |
| unparseable | at the generated line; the whole field stays in the message |
| a file the generator never read | wherever it points — v1 does not check |

The last row is the honest one. See *Containment*.

## Severity

Two, because `Severity` has two (RFC-0006), and a third that nothing consumes
would be a third code path to keep honest.

- **`Warning`** rides a load that SUCCEEDED. It changes no exit code and no byte
  of program output. `--deny-warnings` (or `VYRN_DENY_WARNINGS=1`) turns any
  warning into a failed build, which is how a project demands zero. A build that
  did not ask stays green — advice that breaks a build is not advice, which is
  the whole reason this RFC exists.
- **`Error`** fails the load, at the anchor, with the generator's wording. It
  needs no flag and no flag disables it: the severity is the generator's
  judgement about its inputs, not the build's policy. It travels in the `Err` arm
  like every other error, so the program never runs.

A severity word the compiler does not recognize is reported as a WARNING with a
note naming it. A generator written for a later compiler must not fail an older
build, and must not be silently swallowed by one either.

### Codes and fixes are not in v1

The agent-facing research argues for stable machine-readable codes, and it is
right — for a consumer. A code is load-bearing exactly when something reads it:
a suppression comment that waives one rule at one line, a per-code severity
policy, an agent that maps a code to a fix. None of those exists yet, and each
is a design of its own (a suppression comment lives in the INPUT file, so it is
the generator that must parse and honour it, not the compiler).

A code with no consumer is a field every generator must fill and nobody reads.
So v1 carries it by convention in the message — `a11y/img-alt: img has no alt` —
which costs nothing and reads the same. When the first consumer lands, the code
becomes a field of the directive and the convention becomes a rule. A suggested
fix is deferred for the same reason: nothing applies it. `vyrn fix` applies exactly
one fix, the `.copy()` a move diagnostic names, and refuses every other on the
grounds that it is a decision.

## Where the reports surface

`vyrn check`, `vyrn run`, `vyrn build`, `vyrn emit-ir` — one print site in
`load_program`, so this is one behaviour rather than four. Warnings print as
`file:line:col: warning: message`; errors as the existing `file:line:col:
message`, unmarked, because that is what every other error in the toolchain
prints and a marker on this one alone would be a lie about where it came from.

The LSP needs no change at all, which is worth stating as a finding rather than
as an omission. A warning is already positioned at the input file, so
`analyze_inner` routes it by `from_generated` and publishes it against that
file's URI — the road RFC-0053 built for remapped errors. An error from a
generator IS a load failure, and load failures already take the same road. The
keystroke path is untouched: the directive scan is the same single pass over the
generated text that already parses `//@origin`, run at the same place, so
RFC-0076's 58 ms `.vyx` keystroke is unaffected and no measurement is owed.

## Containment

v1's answer: **a generator is trusted like any other code the project imports.**
It already synthesizes modules that link into the program. A generator that can
emit a function can emit a wrong function; that it can also emit a wrong
sentence about a file is a strictly smaller power. Nothing here escalates.

What does hold, without being a security boundary:

- **The sandbox is unchanged.** No new capability enters the comptime
  environment: the report is text the generator was already free to write.
- **Anchors resolve, they do not roam.** A path is normalized against the
  importer's directory. It can name a file the generator never read; it cannot
  name a file the user could not open.
- **Duplicates collapse.** A page generated twice (server and client bundle)
  reports each authored line once — de-duplicated on what the user sees, never
  on the banner.
- **Volume is capped by the generation caps.** RFC-0021's 4 MB output cap and
  20 M-step budget bound the reports as they bound everything else, because the
  reports are output.

The upgrade path, if a hostile generator ever becomes a real threat model: the
resolver already RECORDS every file a generator read (it is how the cache key is
built), so refusing an anchor outside that set is a few lines. It is not built,
because a project that imports a hostile generator has a larger problem than a
misplaced squiggle.

## Determinism

The cross-engine gate (RFC-0076) requires a generator to produce identical
output under the interpreter and under wasm. Reports are output, so they are
covered by the gate that already exists — and by the discovery rule that gate
uses: `genwasm` finds every example importing through a generator call rather
than reading a list, so `examples/gentable.vyrn` joined the corpus by existing.
The gate did need one widening. It asserted that generation succeeds, which was
true of every generator that could only ever be silent or fatal-by-accident. A
generator that refuses ON PURPOSE is a corpus citizen too, so a failed
generation is now compared rather than rejected: both engines must refuse it
with the same diagnostic. `gentablefail.vyrn` reads
`refused identically` in the gate's log.
The same property makes them survive the gen cache. This is the whole payoff of
the text channel, and it is why the design question "what surface does a
generator call" has the answer "none".

## Milestones

### M1 — the primitive (landed)

- `//@diag <severity> <anchor> <message>` in `compiler/vyrn-frontend/src/origin.rs`,
  beside `//@origin`, lifted by the same line scan. `//@warning` is retained as
  the warning spelling.
- The loader routes by severity: warnings ride the success path, errors fail the
  load with the generator's anchor and wording.
- `std/diag` — `Severity`, `report`, `reportHere`.
- `examples/lib/gen_table.vyrn` — the proof.

**Acceptance evidence.**

`examples/gentable.vyrn` imports a generator that reads a four-column table
definition and warns about one of them:

```text
data/users.tbl:3:7: warning: column `email` has no length limit; write `String(120)`
  note: in generated code generated by table("./data/users.tbl") at gentable.vyrn:1 (see `vyrn emit-gen`)
4
id,email,displayName,signupCount
```

Exit 0, program output unchanged, warning at the line AND column of the input
file. Under `--deny-warnings`, exit 1 and the program does not run.
`examples/gentablefail.vyrn` reads a table that declares `id` twice:

```text
data/dupe.tbl:4:1: column `id` is declared twice; a table has one column per name
```

Exit 1, pinned in `EXPECTED_CHECK_FAILURE` so a silently-fixed example cannot
keep claiming to demonstrate a refusal. Both are three-way parity citizens
(`gentable` runs identically under interpreter, native and wasm; the harness
already filters compile-time warnings from the compared stderr, so a warning is
not a divergence) and both are in the `genwasm` corpus by discovery.

The proof generator is deliberately not a web one. A table definition is a
schema, its two rules are a schema author's rules, and the compiler contains no
word of either.

### M2 — the first consumer: the vyx checking library

`docs/research/vyx-hints.md` §8 ranks ten. They are a LIBRARY over this
primitive — `std/vyx` reporting through `std/diag` — and they are not in this
RFC. Explicitly out of scope here: `a11y/img-alt`, `sec/raw-html`,
`a11y/click-target`, `sec/unsafe-url`, `sec/inline-handler`, `a11y/contrast`.
Two rows of that audit (raw attribute names, silently dropped void children) are
defects in `std/html` rather than advice, and belong to neither milestone.

M2 is where suppression earns its design: a waiver comment in the `.vyx`, parsed
by the generator that owns the rule, is the first thing that needs a code.

### M3 — codes, when M2 asks for one

Promote the message convention to a directive field, with per-code severity
policy if and only if a consumer exists. Not before.

## Alternatives refused

- **A comptime builtin that reports through a side channel.** Loses every
  diagnostic on a cache hit, needs a hand-written second gate for cross-engine
  equality, and adds a capability to the sandbox. The text channel needs none of
  the three. This is the central refusal.
- **A third severity (`note`/`hint`).** LSP has the level; nothing else does.
  A warning that must not fail CI is already spelled by not passing
  `--deny-warnings`.
- **A code registry in the compiler.** It would be the compiler learning rule
  names — the exact thing this RFC exists to avoid — and no consumer reads one
  yet.
- **Checks in the compiler.** Every reason RPC is not a keyword.
- **Keeping the failed-identifier convention and improving its wording.** It
  cannot produce a warning at all, which is half the requirement, and it makes
  the parser the author of every message.
- **Deleting `//@warning` for one spelling.** It is one match arm, it is the
  same mechanism, and RFC-0071 M2b's tests are the channel's proof.
