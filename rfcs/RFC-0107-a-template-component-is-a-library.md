# RFC-0107 — A Template Component Is a Library

- **Status:** **Proposed.** M0 (the feasibility probe) and M1 (the protocol in
  `std/vyx`) have landed — see [M0 — as landed](#m0--as-landed),
  [M1 — as landed](#m1--as-landed) and `rfcs/probe-0107/`. The protocol works and
  `std/icons` (M2) is not written. Milestones below; a milestone that fails its
  gate says so in this file.
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

## M1 — as landed

Built exactly the design M0 proved. Two files changed: `std/vyx.vyrn`
(+234/-29) and `compiler/vyrn-cli/tests/vyx.rs`. No compiler change — the
`.vyx` compiler is still a library over `gen fn`, and the extension point it
gained is one more thing a template can name.

### The protocol, as documented

`std/vyx.vyrn`'s header carries it in full, because a third party has to be able
to write a provider from the doc alone. A provider is an ORDINARY LIBRARY
MODULE: it does not import `std/vyx`, and nothing in `std/vyx` knows what any
provider is called. It exports a `gen fn` of exactly this shape:

```
export gen fn <TagName>(attrs: String, file: String, line: Int64, col: Int64) -> String
```

- `attrs` — the tag's STATIC attributes as a JSON object, `{"name":"…", …}`, in
  source order, every value a JSON string. `std/json`'s writer produced it, so
  `std/jsonread`'s `parseJson` reads it back exactly; a value holding a quote, a
  newline or any UTF-8 survives the trip. A tag with no attributes gets `{}`.
- `file` — the `.vyx` file the tag was written in.
- `line` — the tag's 1-based line in that file.
- `col` — the tag's column, for `std/diag`'s `report`.

The generated module MUST export the conventional entry point
`export fn render() -> Html`, and may export or import anything else it needs.
`std/vyx` calls exactly `render()` at the tag site, through a namespace alias it
mints itself.

**Why one string argument and not one per attribute.** A `gen fn` has fixed
arity and a tag has any number of attributes, so the attributes have to arrive
as one value. JSON was chosen over an ad-hoc `k=v` list because the encoding is
LOSSLESS BY CONSTRUCTION — `std/json`'s escaping is already written and already
tested, and a separator scheme would have needed its own escaper or would break
silently on an attribute value holding the separator. The cost is that
`std/vyx` now imports `std/json`; that module imports nothing itself, so the
link is the writer only.

### The two decisions M0 left open

**Eager contract check vs. the emitted call's own load error — LAZY, and the
choice was forced rather than preferred.** M0 wrote the check as
"`moduleInterface` + `checkContract` when the provider is named at the
`components(…)` call, and unenforced otherwise". In this design the provider is
NEVER named at the `components(…)` call — the template names it — so P1a/P1b
apply to the check exactly as they applied to the read: a generator may only
read under its own constant path arguments, and a path learned from a template
is not one of them. An eager check is not a trade-off here, it is unavailable.
This is the difference from `std/connect` and `std/rpc`, which do reflect the
modules they check: those modules are the generator's OWN string arguments.

The cost turned out to be nothing, because the emitted import's own diagnostic
is both specific and already anchored at the tag — the import is emitted inside
the tag's `//@origin` bracket:

```
Badge.vyx:7:1: `Glyph` is not an imported `gen fn` — a generator import target must be an exported `gen fn` in a module this file imports
Badge.vyx:7:1: generator `Glyph` takes 1 argument(s), got 4
```

A provider whose module generates no `render` is the one misuse that does not
land on the tag; it lands on the generated module and names the arguments,
which include the `.vyx` file and line:

```
namespace `vyxp___comp_Badge_vyx_r_0` (module `generated by Glyph("{\"name\":\"github\"}", "./comp/Badge.vyx", 7, 1) at …`) has no exported member `render` — namespaces reach exported declarations only, one level deep
```

**Which imports a tag resolves against — the WHOLE SET, not the tag's own
file.** The RFC sentence says "names the template's `<script>` imported". The
synthesized module has ONE import block merged from every component
(`vyxMergeImports`) and one flat helper namespace, so a name a sibling's
`<script>` imported is genuinely in scope where any tag is emitted; resolving
per-file would have refused a call the module accepts. `vyxImportedNames` is
therefore flat across the set, and it takes SELECTIVE imports only — an
`import * as ns` alias is not a callable provider name, and `ns.member` is not a
tag.

### What the mechanism looks like

`Badge.vyx`, whose `<script>` imports the provider and whose tags name glyphs,
beside an ordinary sibling component:

```
// ==== generated by components("./comp") at app.vyrn ====
import { el, text, keyed, empty, Html, Attr } from "std/html"
//@origin ./comp/Badge.vyx:2:1
import { Glyph } from "./comp/../provider"
//@origin end
//@origin ./comp/Badge.vyx:7:1
import * as vyxp___comp_Badge_vyx_r_0 from Glyph("{\"name\":\"github\",\"label\":\"GitHub\"}", "./comp/Badge.vyx", 7, 1)
//@origin end
//@origin ./comp/Badge.vyx:9:1
import * as vyxp___comp_Badge_vyx_r_2 from Glyph("{\"name\":\"discord\",\"label\":\"Discord\"}", "./comp/Badge.vyx", 9, 1)
//@origin end
export fn badge() -> Html {
let mut kr: Array<Html> = []
kr.push(vyxp___comp_Badge_vyx_r_0.render())
kr.push(dot())
kr.push(vyxp___comp_Badge_vyx_r_2.render())
return el("span", [Cls("badge")], kr)
}
```

**Namespace minting.** M0 required minted namespaces and sketched `g0`, `g1`.
What landed mints `vyxp_<source path>_<tree path>`, with every byte outside
`[A-Za-z0-9_]` replaced by `_`. It is uglier and it is injective by
construction: the component's source path and the node's tree path already name
exactly one tag in the whole set, so no counter has to be threaded through the
emitter, and the same template mints the same names on every run. Runs of `_`
are deliberately NOT collapsed — that would map two distinct paths onto one
identifier, and the cosmetic win is not worth a silent wrong-namespace bug.

**How the import gets out of the function body.** A provider tag is only
discovered while its view's BODY is emitted, and an import cannot sit in a
function body. `VyxEmit` — the record every emitter already threads for `code`
and `err` — gained an `imports` field, and `vyxBuildModule` builds the views
into a buffer before assembling, so the hoisted imports reach the import block.
With no provider tag anywhere the field is `""` and the emitted module is
byte-identical to the one this assembled before (verified: `emit-gen` on
`examples/vyxdemo.vyrn` is byte-for-byte the same as `main`'s).

### The refusals, verbatim

A `:attr`, an `@event` and children are all refused, each on the offending
attribute's own line. The `:attr` refusal is the one the design section wanted,
and it is now structural rather than stylistic — an expression cannot be written
into a generator call at all:

```
Badge.vyx:7:1: `<Glyph>` is a generation-time provider, and `:name` binds an expression — a provider's attributes become constant arguments to a generator, so write `name="…"` as a static attribute, or wrap `<Glyph>` in a sibling `.vyx` component that computes it
Badge.vyx:7:1: `<Glyph>` is a generation-time provider, and `@click` binds a handler — a provider's attributes become constant arguments to a generator, so a provider tag takes static attributes only
Badge.vyx:7:1: `<Glyph>` is given children, and it is a generation-time provider — a provider's tree comes from its attributes alone, so it takes none
```

Children were not in M0's list. Silently dropping a subtree is worse than a
message, and the check is one line beside the sibling-component one that
already refuses children on a slotless component.

M0's contradiction 3 is closed — the tag-miss message now describes what exists:

```
Badge.vyx:7:1: `<Glyf>` names no component — a component is a `.vyx` file in the same directory, or a generation-time provider a `<script>` imports
```

And a provider's own report lands on the tag, with no origin-map work, exactly
as P7b showed:

```
Badge.vyx:7:1: no glyph `githup` here - nearest is `github`
  note: in generated code generated by Glyph("{\"name\":\"githup\",\"label\":\"GitHub\"}", "./comp/Badge.vyx", 7, 1) at generated by components("./comp") at app.vyrn:1 (see `vyrn emit-gen`)
```

### The negative gate, as a test

"`std/vyx` contains zero component names" needed a rule precise enough to
assert. The one that landed (`std_vyx_names_no_component`, in
`compiler/vyrn-cli/tests/vyx.rs`): **outside its `test` blocks,
`std/vyx.vyrn` contains no string literal that is a bare capitalized identifier
— the shape of a component tag — except an allowlist of seven.** A privileged
built-in component cannot be added without such a literal, whether it is
compared against a tag, seeded into the registry, or emitted as the callee at a
tag site; so the allowlist is where a hardwired `<Icon>` would have to declare
itself, in the open, in a reviewable diff.

The seven, and why none is a component this compiler provides: `Html`, `Data`,
`Params` are TYPE spellings written into generated code; `UiPageBody`,
`UiLayoutBody`, `UiErrorBody`, `UiClientData` are the stems `std/ui` gives the
synthetic component it compiles a route file INTO — they name the user's own
page, not a widget any template can import. The `test` blocks are excluded
because they name the components they compile (`Item`, `Btn`, `Lst`); the rule
is about the compiler, and its region ends at the first `test "` line.

### Gate results

- **A toy provider round-trips.** `a_provider_tag_generates_and_splices_its_html`
  — two tags, two nested generations, the trees spliced where the tags stood:
  `<span class="badge"><svg aria-label="GitHub">M8 0 L16 8 L8 16 Z</svg><svg aria-label="Discord">M2 2 H14 V14 H2 Z</svg></span>`.
  Seven more rows cover the anchored diagnostic, the three refusals, the
  tag-miss message, minting (three tags across two components, three distinct
  minted aliases, a sibling component interleaved), the two shape misuses, and
  cache soundness. `cargo test -p vyrn-cli --test vyx`: **44 passed, 0 failed.**
- **Cache soundness.** `an_unchanged_rebuild_regenerates_no_provider`, over the
  test's own `VYRN_GEN_CACHE_DIR`: the first build writes 3 entries (the
  template plus one per tag); the unchanged rebuild renders identically and
  rewrites NO entry (names and mtimes compared, so a rewrite is visible and not
  just a new key); editing the provider's source changes the render, so it was
  not a stale hit. As M0 predicted, the provider's reads live in the provider's
  own entry and the template's entry — whose output is one import line — stays a
  hit.
- **`std/vyx` names no component.** Asserted, above.
- **All existing `.vyx` compiles unchanged.** `emit-gen` on
  `examples/vyxdemo.vyrn` is byte-identical to `main`'s. The full site export
  (172 routes, 13 assets) is byte-identical to `main`'s in every file EXCEPT
  four, and all four are the pages generated FROM `std/` source:
  `docs/std/vyx.html`, `docs/std/vyx.data.json` (the new protocol doc), and
  `docs/std/json.html` + `docs.html` (the std dependency graph gained the edge
  `vyx → json`, which is true). No consumer page moved a byte.
- **Site `vyrn test` loop:** 167 blocks ran, 0 failed. The guide's programs:
  green. `vyrn fmt --check` on `std/vyx.vyrn` and on `site/`: clean.
- **Workspace `cargo test --release`:** 71 suites, 0 failed. **Parity**
  (`--ignored --test-threads=1`, tools pinned): **40 passed, 0 failed.**
- **LSP:** `cargo test --release` in `compiler/vyrn-lsp`: 76 + 7 passed, 0
  failed. `std/vyx.vyrn` grew 234/-29 lines (≈5%) and gained one `std/json`
  import, so the keystroke path is measurably — slightly — slower: the
  `lspbench` probe over `examples/vyxdemo.vyrn` reads 31.4 ms on `main` and
  32.8 ms here (mean of two runs each), +1.4 ms, in proportion to the source
  growth and far inside RFC-0084's 97 ms budget. Nothing was done about it.
- **Excluded crates:** `vyrn-play` builds for `wasm32-unknown-unknown`;
  `vyrn-genwasm` tests pass. `cargo fmt --check` clean for the workspace,
  `vyrn-lsp`, `vyrn-genwasm` and `vyrn-play`.

### What M1 contradicts, and what it leaves

1. **The anchor's column is always 1.** M0's transcript showed column 5 because
   its stand-in scanner computed one. A `VNElem` carries a LINE AND NO COLUMN —
   `std/vyx-hints` says so in its own comment — and every `std/vyx` diagnostic
   already reports at column 1. So `col` is in the protocol signature, is passed,
   and is 1. Giving it a real value means adding a column to `VNElem`, which is
   22 match sites plus `std/vyx-hints`, for a number no other diagnostic in this
   compiler has. Not done, and the protocol needs no change when it is.
2. **The RFC's "the template's `<script>`" is now "the set's `<script>`s"** —
   corrected above with the reason.
3. **A provider tag's `class` is not theme-checked.** `componentsThemed` proves a
   static `class` literal `⊆ Tw` on an element; on a provider tag `class` is just
   an attribute in the JSON, and what the provider does with it is the provider's
   business. Recorded, not fixed: the check belongs to whoever emits the
   element, and that is the provider.
4. **M0's contradiction 4 (a checker error inside a generator module attributed
   to the importing file) is untouched**, as scoped. So is the alias-data-read
   hole, which is M2's.

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

**M1 — the protocol in std/vyx.** **Landed** — see
[M1 — as landed](#m1--as-landed): the protocol documented in `std/vyx`'s header,
the per-tag generator import, attributes as one JSON constant argument, the three
structural refusals, the anchor arguments, cache soundness, and the negative gate
as a test. The provider's shape is NOT checked eagerly, because it cannot be (the
reason is recorded there). Tag resolution reaches the whole set's `<script>`
imports rather than the tag's own file, for the reason recorded there too. Gate
met: the toy provider round-trips, `std_vyx_names_no_component` asserts the line,
and every existing `.vyx` page compiles byte-identically. The original line was:
Tag resolution against imported names, the
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
