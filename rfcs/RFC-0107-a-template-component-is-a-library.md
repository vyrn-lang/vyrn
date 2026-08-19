# RFC-0107 — A Template Component Is a Library

- **Status:** **Proposed.** M0 (the feasibility probe) has landed — see
  [M0 — as landed](#m0--as-landed) and `rfcs/probe-0107/`; no feature is
  implemented. Milestones below; a milestone that fails its gate says so in this
  file.
- **Depends on:** RFC-0021 (generators — the comptime sandbox and the recorded
  inputs its cache is keyed on), RFC-0026/0039 (std/vyx, the template compiler
  this RFC gives an extension point), RFC-0099 (generator diagnostics with
  positions), RFC-0033/0048 (origin maps — how a provider's error lands in the
  template), RFC-0010 (aliases and pinned remote files — how a collection
  arrives), RFC-0027 (`import * as ns`).
- **Evidence (user):** "does vyx have iconify support? Like nuxt/icon does",
  "but that import looks to complex", "such components shouldn't be hardcoded
  and there shouldn't be such behaviors, this is nonsense", "is Icons bound to
  vyx?".

---

## The problem

The site needs icons; every Vyrn UI will. The obvious shape — an `<Icon>` tag
resolved at compile time from a pinned Iconify collection — is easy to build
by hardwiring it into `std/vyx`. That is the wrong build, and the user said so
in the words above: the moment the template compiler carries one privileged
component, it stops being "a template language as a library" and becomes a
framework with blessed built-ins. The repository's whole thesis (RPC, i18n,
OpenAPI, GraphQL, the UI layer — all libraries over `gen fn`, zero compiler
changes) argues the other way.

## The line

**Directives are the language; components are libraries.** `v-for`, `:attr`,
`{{ }}` belong to `std/vyx`. Every capitalized tag resolves to a name the
template's script section imported — user `.vyx` components today, and with
this RFC, generation-time components from any library. `std/vyx` names no
component. That sentence is a gate (greppable), not a hope.

## The design

**Discovery is an import, not a registry.** A `.vyx` script section imports
the component like anything else; the tag resolves against names in scope.
No manifest key, no plugin config, no global state.

**The provider contract is a protocol.** A generation-time component is an
exported value conforming to a conventional shape — attributes in,
`Result<Html, Issue>` out — which the `.vyx` compiler evaluates in the
comptime sandbox while generating the page, splicing the returned tree where
the tag stood. Static attributes only: a `:name` bound to a runtime
expression is refused, because a name the compiler cannot read cannot be
checked or pinned, and that refusal is the feature.

> **Corrected by M0.** "which the `.vyx` compiler evaluates in the comptime
> sandbox" is not something a `gen fn` can do and never was: the mediated set
> inside the sandbox is `readFile`, `listDir`, `moduleInterface`, none of them
> evaluates anything, and a provider named by the template is outside the
> generator's declared input roots anyway. A provider is therefore not an
> exported *value* the compiler calls; it is an exported **`gen fn` the compiler
> emits an import of**, with the tag's attributes as the constant arguments. The
> splice, the static-attribute-only rule and the refusal all survive unchanged —
> the refusal gets stronger, because a non-constant attribute is not merely
> refused by a rule, it cannot be written into a generator call at all. See
> [M0 — as landed](#m0--as-landed).

**Diagnostics and caching are the existing machinery.** A provider's `Issue`
becomes an RFC-0099 diagnostic positioned into the template through origin
maps. Every file a provider reads goes through the recording resolver, so the
generator cache is keyed on the provider's true inputs — a changed collection
file or provider source invalidates exactly what it should.

## std/icons — the proof of protocol

Layered so nothing binds to `.vyx`:

1. **The data**: an Iconify collection is one JSON file (name → SVG body),
   pinned like any dependency — `vyrn add` writes the alias and the lock line.
   Each collection's license is surfaced into the generated module's doc
   header; glyphs ship with their terms.
2. **The core is a plain generator**, usable from any `.vyrn` file:
   `import * as ic from icons("icons", "github discord rss")` — names once,
   in the argument; unknown names are generation diagnostics with a "nearest"
   suggestion; only named glyphs are generated, so editor analysis stays fast
   and the artifact carries exactly what is used.
3. **`<Icon name="brand:github"/>`** is one consumer of that core through the
   provider protocol. The prefix vocabulary is the manifest's alias keys
   (`icons`, `icons/brand`), not a hardcoded registry. Emitted markup is
   inline `<svg aria-hidden="true">` using `currentColor` — the glyphs follow
   the palette tokens and the theme control with no per-icon work; a `label`
   attribute adds the accessible name when the icon is the content.

The dependency arrow points one way: the `.vyx` consumer depends on
`std/icons`; `std/icons` does not know `.vyx` exists. A third-party template
language consumes the same core — that is the test the layering is real.

## What the Iconify runtime does, that this deliberately does not

No runtime fetching of icon data (their API/CDN mode): resolution is at
compile time, from hash-locked files, offline-capable. An unknown icon fails
the build instead of rendering an empty box.

## M0 — as landed

The probe is `rfcs/probe-0107/` (one program per question, the `census-0103` /
`bench-0104` style); its README carries every transcript, and this section
carries the verdicts. No `std/`, `compiler/` or `site/` file was touched: the
`.vyx` compiler's part is played by a stand-in generator, and the toy provider is
ordinary Vyrn.

### The mechanism map — what `std/vyx` does with a component today

A capitalized tag resolves against **`VyxRegistry.names`, which is
`vyxCompNames(comps)` — the stems of the `.vyx` files in the directory
`components(dir)` was pointed at, and nothing else** (`std/vyx.vyrn`,
`vyxLookup` / `vyxEmitComp`). The registry TAKES the compiled components, and the
emitted call is `comp.fnName(args…)` against a view function in the same
synthesized module. There is no second lookup path.

Two mechanisms already exist beside it, and both matter to M1:

- **A `<script>` import passes through.** `vyxParseImport` accepts a spec that is
  either a `"…"` literal **or a `gen("…")` call**, and `vyxRebaseImport` rebases
  the generator's first quoted path argument the same way it rebases a plain
  relative spec. Generator imports in a template's `<script>` are a designed-for
  case, not an accident.
- **Imports are merged and deduped across the whole component set**, in one flat
  namespace, and a name bound twice to different modules is a generation
  diagnostic naming both files (`vyxMergeImports`).

Which is why P0b runs: the shipped `std/vyx`, the shipped loader, no patch, a
`.vyx` whose `<script>` says `import * as gp from glyphs("../data", "discord")`
and whose template says `v-html="toHtmlString(gp.discord())"` prints
`<span class="badge"><svg>M2 2 H14 V14 H2 Z</svg></span>`. **A generation-time
provider already reaches a template today. Only the tag syntax is missing** — P0a
is the exact wall:

```
vyxtag/Badge.vyx:8:1: `<Icon>` names no component — a component is a `.vyx` file in the same directory, or one this `<script>` imports
```

That message's second clause is false today (see the contradictions below).

### The four verdicts

**(a) load and evaluate the imported provider module inside the sandbox —
REFUTED.** The generator's input roots are the *generator call's own constant
string arguments*; a path learned from a template is not one of them:

```
p1a-escape-read.vyrn:4:0: generator `p1EscapeRead("./data")` failed: generator read `./data/../provider.vyrn` escapes its declared inputs (data, data.vyrn) — a generator may only read under its constant path arguments
```

`moduleInterface` is refused by the same check with the same wording (P1b). When
the provider IS one of the arguments, reflection works and yields signatures
only — `glyphs(dir: String, names: String) -> String` (P1c). So the sandbox will
tell a generator what a provider's surface *is*; it will not open a provider the
template chose.

**(b) call an exported function of it with the tag's attributes — REFUTED.**
There is no call primitive. `FnInfo` has no member for it:

```
p2a-call.vyrn:10:0: call to unknown function `call`
```

and the provider cannot arrive as a value instead of a name, because a generator
import takes constants:

```
p2b-fn-arg.vyrn:5:0: generator import `p3Nested(..)` needs compile-time-constant arguments (v1: string / integer / boolean literals)
```

**(b′) the same thing deferred — PROVEN, and this is the mechanism M1 gets.** A
generator need not evaluate the provider; it emits the import. P7's stand-in
compiler reads `Badge.vyx` (its own path argument), takes the spec the `<script>`
imported and the glyphs the tags named, and emits one provider import per tag:

```
import { glyphsAt } from "./provider"
import * as g0 from glyphsAt("./data", "github", "./Badge.vyx", 7, 5)
import * as g1 from glyphsAt("./data", "discord", "./Badge.vyx", 8, 5)
```

The loader follows a generator call that appears in generated source, the
provider runs in its own sandbox with its own roots, and the page prints its two
glyphs. Attributes reach the provider as constant arguments; the returned `Html`
is spliced by an ordinary call where the tag stood.

**(c) every file the provider reads recorded, cache sound — PROVEN.** Counting
entries written under `~/.vyrn/cache/gen`: an unchanged rebuild writes none, an
edit to the collection writes one and changes the rendered output, a rebuild
after that writes none again — on the P3 chain and on the shipped-`std/vyx` P0b
chain alike. Soundness needs nothing new: the provider's reads are recorded in
the provider generation's own entry, and the outer generation's output is an
import line that did not change, so it stays a hit while the inner one misses.
This is *better* than folding the provider's inputs into the template's entry,
which is what the design section assumed would be needed.

**(d) a provider error positioned at the tag — PROVEN.**

```
BadgeTypo.vyx:7:5: the collection under `./data` has no glyph `githup` — nearest is `github`
  note: in generated code generated by glyphsAt("./data", "githup", "./BadgeTypo.vyx", 7, 5) at generated by p7Vyx("./BadgeTypo.vyx") at p7b-diagnostic.vyrn:3 (see `vyrn emit-gen`)
```

Line 7 column 5 is the tag. No origin-map work was involved: `std/diag`'s
`report` anchors at any file the generator names, and the tag's position
travelled to the provider as two integer arguments. RFC-0033/0048 origin maps
stay what they are for — mapping the template's *own* verbatim expressions — and
are not on this path.

### The alias question (the M2 scoping answer)

An aliased **module** specifier works everywhere, including as the target of a
generator import inside generated source (P6b renders through
`import { glyphs } from "prov"`). An aliased **data path** does not:

```
vyrn run p6-alias.vyrn          -> readFile said: cannot read `coll/collection.txt`
vyrn run p6c-alias-reflect.vyrn -> generator `p1Reflect("prov")` failed: moduleInterface cannot read `prov`: …
```

`gen_scoped_path` is path arithmetic against the importer's directory with no
import-map step, so a `gen fn` cannot read a manifest-aliased (and therefore
lock-pinned) file. **This is a hole M2 must fill.** Three ways out, in the order
M2 should consider them: teach the mediated `readFile`/`listDir`/`moduleInterface`
to resolve an alias-rooted path through the same resolver module specifiers use
(a compiler change, and the honest one); or reach the pinned bytes by their
`vyrn vendor` path, which is an ordinary relative path (no compiler change, costs
a vendor step); or ship a collection as a `.vyrn` module the provider imports
statically, which works today and costs the RFC its "the data format is the
interface" line. The first is the only one that keeps a collection a JSON file
that `vyrn add` pins.

### The design M1 builds

**Mainline, amended — not the fallback.** The RFC's named fallback (the loader
precomputes provider resolution and hands the `.vyx` generator the provider's
exports as an input) is **not chosen**, and does not need to be: it would add a
loader-side pre-pass, a new input shape for `components(…)`, and a second place
where provider resolution lives, to buy an evaluation step that P3/P7 show is
unnecessary. The chosen shape keeps every piece where it already is:

1. `std/vyx` resolves a capitalized tag against the names its `<script>`
   imported, after the sibling registry misses.
2. For an imported name, it emits a **generator import per tag** — the tag's
   static attributes as constant arguments, plus the tag's `file`, `line`, `col`
   so the provider can anchor its own diagnostics — and calls the namespace
   member where the tag stood.
3. A `:attr` bound to an expression is refused: it cannot be a constant argument.
   The refusal the design section wanted is now structural.
4. The provider protocol is therefore "an exported `gen fn` taking
   `(collection, names, anchorFile, line, col)`", checkable with
   `moduleInterface` + `checkContract` when the provider is named at the
   `components(…)` call, and unenforced otherwise. **M1 must decide whether it
   checks the shape or lets the emitted call's own load error do it** — the probe
   proves both are possible and takes no position.

Two costs M1 owns, both visible in the probe:

- **One generation per tag**, because the anchor differs per tag. Batching all of
  one provider's names into a single import (as P3 does) is faster and loses the
  per-tag position. A middle course — one import per provider, anchors passed as
  one packed argument — is available and untested.
- **Namespace minting.** Sibling components share one flat import namespace, and
  binding one alias to a provider twice with different arguments is already a
  generation diagnostic:

  ```
  `./vyxcomp/Plain.vyx` imports `g` from `glyphs("./vyxcomp/../data", "discord")`, and a sibling component already binds that name to `glyphs("./vyxcomp/../data", "github")` — one namespace alias reaches one module across the whole set, so rename one of them
  ```

  So M1's emitted namespaces must be minted (`g0`, `g1`, …), never the author's.

### What the probe contradicts

1. **"the `.vyx` compiler evaluates [the provider] in the comptime sandbox"** —
   corrected in place in the design section above. Evaluation is deferred to a
   nested generation, not performed by the template compiler.
2. **Origin maps are not on the diagnostic path.** The RFC lists RFC-0033/0048
   as a dependency for "how a provider's error lands in the template"; the actual
   mechanism is `std/diag`'s anchor plus two integer arguments. The origin-map
   dependency is real for the template's own expressions and misattributed here.
3. **`std/vyx`'s tag error over-promises.** "`<Icon>` names no component — a
   component is a `.vyx` file in the same directory, **or one this `<script>`
   imports**" — the second clause describes nothing that exists. Today the
   registry is siblings only. M1 makes the message true rather than fixing the
   message.
4. **A checker error inside a generator module is attributed to the wrong file.**
   P2a's fault is at `probe-p2.vyrn:10`; the diagnostic says
   `p2a-call.vyrn:10:0` — the importing file's path with the generator module's
   line number. Recorded here, not fixed by M0, and not in this RFC's way.

## Milestones

**M0 — the feasibility probe.** **Landed** — `rfcs/probe-0107/`, verdicts in
[M0 — as landed](#m0--as-landed): (a) and (b) refuted as written, the deferred
form proven, (c) and (d) proven, the mainline design amended rather than
replaced by the fallback. The question it was given: the one real design risk is
the `.vyx` generator evaluating an *imported provider module* inside the comptime
sandbox — dynamic from the generator's point of view, recorded for the cache,
diagnosable with positions. One probe that proves or refutes it, in the
census-by-execution style, before anything else is written. Gate: the probe's
transcript in this file, and a stated verdict; if refuted, the fallback
design (provider resolution precomputed by the loader) is chosen here with
the reason.

**M1 — the protocol in std/vyx.** Tag resolution against imported names, the
emitted per-tag generator import, attribute passing as constant arguments, the
static-attribute refusal, the anchor arguments that carry the tag's position,
cache soundness. (M0 replaced two words of this line: the "contract type" is a
convention on a `gen fn`'s signature, checkable but not necessarily checked, and
"diagnostics through origin maps" is diagnostics through `std/diag`'s anchor.)
Gate: a toy provider in the test suite
round-trips; `std/vyx` contains zero component names — asserted by a test,
not a grep someone runs once; all existing `.vyx` pages compile unchanged.

**M2 — std/icons core.** The generator, the `vyrn add` flow for a collection,
license surfacing, the nearest-name diagnostic, `* as` usage from plain
`.vyrn`. **Plus the hole M0 found**: a `gen fn` cannot read a manifest-aliased
file today, so M2 owns the choice between teaching the mediated reads to resolve
an alias and reaching the pinned bytes another way. Gate: a program using two collections builds offline from the lock;
a misspelled icon shows the diagnostic verbatim in this file.

**M3 — the `<Icon>` provider and the first consumer.** The provider over the
core; the site's shell (RFC-0106 M1/M2) consumes it for OS tiles, footer and
pillar glyphs. Gate: the site export carries only glyphs the templates name
(counted); the a11y checklist rows for decorative-vs-labelled icons pass.

## What this RFC does not do

- It does not put any component into `std/vyx` — including `<Icon>`.
- It does not fetch anything at runtime, ever.
- It does not support runtime-chosen icon names; render the fixed set and
  toggle visibility.
- It does not vendor Iconify's tooling; the data format is the interface.
