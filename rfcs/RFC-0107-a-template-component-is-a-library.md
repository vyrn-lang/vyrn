# RFC-0107 — A Template Component Is a Library

- **Status:** **Proposed.** M0 (the feasibility probe), M1 (the protocol in
  `std/vyx`) and M2 (`std/icons` and the alias hole) have landed — see
  [M0 — as landed](#m0--as-landed), [M1 — as landed](#m1--as-landed),
  [M2 — as landed](#m2--as-landed) and `rfcs/probe-0107/`. The protocol works, the
  core is a library any `.vyrn` file can import, and the `<Icon>` consumer (M3)
  is not written. Milestones below; a milestone that fails its gate says so in
  this file.
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

## M2 — as landed

Two things: the hole M0 found is filled in the compiler, and `std/icons` is a
library over it. Nothing in `std/vyx` changed — the core knows no template
language, which was the layering claim, and the file count says so.

### The alias hole, as filled

M0's three ways out were: teach the mediated reads to resolve an alias, reach the
bytes by their `vyrn vendor` path, or ship the collection as a `.vyrn` module.
The first was taken, for the reason M0 gave — it is the only one that keeps a
collection a JSON file `vyrn add` pins.

**The import map is one step, in one place.** `gen_scoped_path` took
`(importer_dir, allowed, arg)` and did path arithmetic. It now takes
`aliased` as well: the `(spelling, resolved key)` pairs for those of the
generator's constant arguments that name a `vyrn.json` dependency. The loader
builds them with `resolve_spec` — the same function a module specifier goes
through, so an alias means one thing in this compiler and not two. A read
spelling one of them gets the resolved key; every other read is the arithmetic it
always was.

**The sandbox rule is unchanged, and that is a property of where the pairs come
from.** They come from the generator's OWN constant arguments, and each resolved
key is pushed into `allowed` beside the two spellings that argument already
contributed. So the input-root check still decides, on the resolved key, with the
same message. An alias is a second SPELLING of a declared root, never a new root.
The negative row `the_input_root_rule_still_decides` builds a path under an alias
argument that climbs out of it, and gets:

```
generator read `coll/../../secret.txt` escapes its declared inputs (…) — a generator may only read under its constant path arguments
```

**Cache soundness needed the resolved key too, and this is the part that is easy
to get wrong.** The lookup key is `sha256(generator sources ++ args ++ resolved
inputs)`, and the recorded inputs are validated by re-hashing. Re-pinning a
dependency changes neither the arguments nor the bytes of the file the old entry
recorded, so without the resolved key in the lookup the entry stays valid and the
build serves the glyph nobody points at any more. Measured, not argued: removing
the one `allowed.push(key)` from the cache key's input list (and only from there)
makes `re_pinning_a_collection_misses_the_generator_cache` fail with the old
glyph in the message.

**A pinned read that fails ABORTS the generation, in the resolver's own
words.** A local `readFile` miss stays an `Err` value under the canonical wording
(RFC-0014: never OS text). A pinned dependency that cannot be produced is not a
condition to branch on — it is "locked but not cached" or "the upstream changed
under an immutable URL", each with a remedy — so it is a trap carrying that
refusal verbatim:

```
app.vyrn:2:0: generator `icons("icons", "activity alert archive")` failed: `github:iconify/icon-sets@66114542c442d138c1da78932ddbad862fb7a65c/json/bytesize.json` is locked (sha256 33c635f7…) but not cached, and this is an offline build — run once online, `vyrn vendor`, or drop any copy of the file with that hash into the cache
```

That wording is `vyrn-cli`'s existing one, single-sourced, reached through the
same `RemoteResolver` a module import uses — so the lock, the vendor directory,
the content cache, the hash check and `--offline` all apply with no second copy
of any of them. The wasm generation engine (RFC-0076) refuses the same read the
same way, because its status alphabet carries no message and an answer that
differs by engine is two answers.

**What it does NOT do: subpaths under an alias.** `icons/lucide.json` is not
resolved, because `resolve_spec` does not resolve it for a module import either —
a bare specifier is an EXACT key in the dependency map. One collection is one
alias, which is what `vyrn add --name` writes. Growing that is a change to module
resolution, not to the sandbox.

`moduleInterface` goes through the same step; the only care needed was to leave
an alias spelling alone rather than append `.vyrn` to it.

### std/icons, the surface

```vyrn
import { icons } from "std/icons"
import * as ic from icons("icons", "activity alert archive")
```

- **`icons(collection, names)`** — the plain-`.vyrn` generator. `collection` is a
  relative `.json` path or a dependency alias; `names` is space-separated Iconify
  names. It emits one `export fn <name>() -> Html` per glyph over one private
  `glyph(box, markup)` helper.
- **`iconsModule(collectionText, collectionPath, names, anchorFile, line, col)`** —
  the same generator with the read already done, exported for an M3 provider. A
  `gen fn` can call it (`std/rpc` calls `std/symbolmap`'s `symbolMapFn` the same
  way), and it is a pure function of its text: no read, no manifest, no `.vyx`.

Emitted markup is `<svg viewBox="0 0 W H" width="1em" height="1em"
aria-hidden="true">` with the collection's body verbatim inside a `Raw`. No
`fill` and no pixel size are written, and that is deliberate: an Iconify body
already paints itself in `currentColor` (lucide's carry `fill="none"
stroke="currentColor"` on the body element), so leaving both alone is what makes
the glyph follow the palette tokens and the theme control. `aria-hidden="true"`
because an icon beside a label is decoration; the accessible name belongs at the
use site.

The body is baked with an RFC-0054 code quote — `render(vyrn"\{body}")` — for
`std/symbolmap`'s reason: an SVG body carries quotes, a JSON decode has already
unescaped it once, and a hand-written escaper here would be a second escaper free
to disagree with the lexer.

**Aliases, sizes and licences.** The collection's `aliases` are followed to their
parent (bounded at 8 hops, so a circular data file is a message and not a hang).
A per-glyph or per-alias `width`/`height` overrides the collection's. `aliases`
also carry TRANSFORMS in real collections (`rotate`, `hFlip`, `vFlip`); applying
one needs an SVG rewriter this module does not have, so such an alias is refused
by name rather than rendered unturned. `info.license` becomes the generated
module's own header line, and a collection that declares none says so:

```
/// Glyphs from `icons` (prefix `bytesize`), generated by std/icons — do not edit.
///
/// License: MIT — https://github.com/danklammer/bytesize-icons/blob/master/LICENSE.md
```

**Lookup is lazy on purpose.** The parsed document's `icons` and `aliases` field
lists are kept as they are and scanned per requested name. Expanding 1,800
entries to emit five is work the caller did not ask for, and the RFC's "only
named glyphs are generated" is about the editor and the artifact as much as about
the build.

### The gate, as run

**Two collections, offline, from the lock.** Both are real Iconify collections,
pinned at one commit with `vyrn add`:

```
vyrn add github:iconify/icon-sets@66114542c442d138c1da78932ddbad862fb7a65c/json/bytesize.json --name icons
vyrn add github:iconify/icon-sets@66114542c442d138c1da78932ddbad862fb7a65c/json/codex.json  --name codex
```

`vyrn run --offline`, with a cold generator cache and the bytes from
`~/.vyrn/cache`:

```
<svg viewBox="0 0 32 32" width="1em" height="1em" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 16h7l3 13l4-26l3 13h7"/></svg>
<svg viewBox="0 0 32 32" width="1em" height="1em" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="m16 3l14 26H2Zm0 8v8m0 4v2"/></svg>
<svg viewBox="0 0 32 32" width="1em" height="1em" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 10v18h24V10M2 4v6h28V4Zm10 11h8"/></svg>
<svg viewBox="0 0 24 24" width="1em" height="1em" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-width="2" d="M9 12V7.1a.1.1 0 0 1 .1-.1h1.3c1.1 0 3.6.1 3.6 2.5c0 0 0 2.5-3 2.5m-2 0v4.8c0 .11.09.2.2.2h3.3c1.5 0 2.5-1 2.5-2.5c0-2.795-4-2.5-4-2.5m-2 0h2"/></svg>
<svg viewBox="0 0 24 24" width="1em" height="1em" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-width="2" d="M18 7H6m12 10H6m10-5H8"/></svg>
```

Cold: 8.9 s for both collections. Warm (generator cache hit): 0.045 s. No network
was reachable for either run — the second half of
`a_pinned_remote_collection_reads_offline_from_the_vendor` proves the point
negatively by deleting the bytes and getting the pinning refusal.

**The misspelled glyph, verbatim.**

```
toy:1:1: the collection `toy` has no glyph `githup` — nearest is `github`
  note: in generated code generated by icons("toy", "githup") at bad.vyrn:4 (see `vyrn emit-gen`)
```

With an anchor from a caller (what an M3 provider passes), the same sentence
lands on the tag instead:

```
//@diag error ./Badge.vyx:7:1 the collection `toy` has no glyph `githup` — nearest is `github`
```

### Gate results

- **The alias hole, proven both ways.** `compiler/vyrn-cli/tests/icons.rs`, six
  rows: the aliased read (a manifest at the root, the program in `src/`, so the
  arithmetic alone reaches nothing), the undeclared alias, the input-root refusal,
  the re-pin cache miss, a pinned REMOTE collection read offline from
  `vyrn_vendor/` and then the pinning refusal with its bytes deleted, and the
  two-collection program with the misspelled glyph. Against `main`'s binary the
  first of them prints
  `std/icons cannot read the collection \`coll\`: cannot read \`coll\``; here it
  prints the glyph.
- **`std/icons`'s own `test` blocks: 14 passed, 0 failed**, discovered by the
  `std_suite` sweep rather than listed anywhere. They run on an inline
  three-glyph collection, so the suite needs no file, no manifest and no cache.
- **Existing generators byte-identical.** `vyrn emit-gen` over all **171**
  examples: **0 differing** against a `main` build (each run with its own
  `VYRN_GEN_CACHE_DIR`, since two compilers sharing one cache directory warn
  about each other's entries on stderr).
- **Workspace `cargo test --release`:** 72 suites, **1,758 tests, 0 failed**.
  **Parity** (`--ignored --test-threads=1`, tools pinned): **40 passed, 0
  failed**, 252.9 s.
- **Excluded crates:** `vyrn-lsp` 76 + 7 passed; `vyrn-genwasm` tests pass;
  `vyrn-play` builds for `wasm32-unknown-unknown`. `cargo fmt --all --check`
  clean for the workspace and for the three excluded manifests.
  `vyrn fmt --check std/icons.vyrn` clean. `vyrn doc --std -o docs/api --verify`
  up to date (39 files), committed.

### The wall M2 hit, with numbers

**`std/icons` cannot read a 566 kB collection under the default build, and the
reason is not `std/icons`.** `parseJson` in the generation sandbox is QUADRATIC in
document size. Timings, `VYRN_NO_GEN_CACHE=1`, one `gen fn` that reads a
synthetic Iconify-shaped document and parses it:

| document | before | after | after, `--features wasm-gen` |
| --- | --- | --- | --- |
| 2.7 kB | 0.33 s | — | — |
| 11 kB | 4.5 s | 1.16 s | — |
| 43 kB | 94 s | 15.8 s | — |
| 566 kB (`lucide.json`) | did not finish in 10 min | still over 2 min | **0.41 s** |

The cause is parameter binding, not the reader. A parameter of type
`Array<UInt8>` — which is what `bytes(s)` gives, so it is what every byte-level
`std/strings`, `std/scan` and `std/jsonread` function takes — was COERCED on every
call, and the coercion rebuilt the whole array. `coercion_is_noop` had no arm for
"an `IntN` value already at this width and signedness", so every element answered
"there is work to do" and the array was rebuilt element by element, per call. The
control cases pin it exactly, all at 43 kB: the same byte loop with no
`Array<UInt8>` parameter runs in 0.075 s, with an `Array<Int64>` parameter (no
range to check) in 0.18 s, and compiled to wasm in about a second including the
runtime's own start-up — the compiled backends pass by reference and never had
this.

The one arm that was missing is now there, with a guard that PROVES the value is
where wrapping would leave it rather than assuming it, so the semantics cannot
see it. It buys 6x. **It does not make the reader linear** — the array is still
scanned per call, just cheaply — so a 566 kB collection is still out of reach in
the interpreter. Making it O(1) means an array value that carries its element
type, which is a change to `Val` and to RFC-0082's boundary rules, and is not
this RFC's. Recorded here because M2 is what surfaced it, and because every
generator in this repository that reads a file pays it.

**The RFC-0076 engine already clears the wall, and that is the strongest
statement available about whose wall it is.** With `--features wasm-gen` the
generator is compiled and runs in wasmtime, which passes arrays by reference and
never had the coercion at all: `lucide.json` — all 566 kB, 1,843 icons and 217
aliases of it, `activity-square` resolved through the alias table — generates four
glyphs in **0.41 s**, and the two-collection gate below runs in **0.088 s**
instead of 8.9 s. So the design reads a real Iconify collection today. What cannot
is the DEFAULT build, which is what `cargo test` uses and what
`.github/workflows/release.yml` ships (it builds `-p vyrn-cli` with no features).
Which of those two facts to change — the interpreter's coercion, or what the
release carries — belongs to RFC-0082 and RFC-0076 respectively, and neither is
decided here.

So the gate ran on the two smallest real collections in `iconify/icon-sets`
(22 kB and 23 kB) rather than on `lucide.json`. That is the honest scope of what
landed: `std/icons` is correct on real data, fast on real data of any size under
the compiled engine, and fast enough on 22 kB under the interpreted one.

### What M2 contradicts, and what it leaves

1. **An M3 provider cannot name its collection by alias yet, and the reason is
   structural.** M1's protocol hands a provider `(attrs, file, line, col)`, with
   the tag's attributes as ONE JSON string. A collection alias inside that string
   is not a constant path argument, so it contributes no input root and this
   milestone's import-map step never sees it — P1a applies to it exactly as it
   applied to reading a provider the template chose. M3 therefore has to either
   give the provider the collection as an argument of its own (a provider module
   per collection, which the "the prefix vocabulary is the manifest's alias keys"
   sentence did not anticipate), or the root rule has to grow a way to declare an
   input that arrives inside a structured argument. Neither is decided here.
   `iconsModule` is shaped for the first: it takes the collection TEXT, so
   whoever can read the bytes can call it.
2. **A report about an alias-named collection anchors at the alias spelling.**
   `toy:1:1` above is not a file any editor can open. `std/diag` anchors at a file
   the generator NAMED, and what the generator was given is `toy`; the resolved
   key is the sandbox's business and is not handed back. The note carries the
   import site, which is the position a reader actually wants. Recorded, not
   fixed.
3. **The prefix vocabulary is not implemented, because the core does not need
   one.** The RFC's `<Icon name="brand:github"/>` splits a prefix off the name;
   the core takes the collection as its first argument instead, and REFUSES a
   name carrying a `:` with the bare name to write. Splitting belongs to whoever
   parses the tag.
4. **Two glyphs whose names camel-case to one identifier are refused**, not
   renamed. `circle-check` and `circleCheck` in one import both want
   `circleCheck()`. Emitting both would be a duplicate-declaration error inside a
   generated module; the refusal names both and says to import the collection
   twice.

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

**M2 — std/icons core.** **Landed** — see [M2 — as landed](#m2--as-landed).
The hole M0 found is filled the way M0 called honest: the mediated reads take an
import-map step, through the same `resolve_spec` a module specifier uses, with
the resolved key in the input roots AND in the generator cache key. `std/icons`
is the library over it. Gate met on two REAL pinned Iconify collections, offline
from the lock, with the misspelled-glyph diagnostic recorded verbatim there.
M2 also hit a wall it did not create and could not close: `parseJson` in the
generation sandbox is quadratic in document size, so a 566 kB collection does not
finish under the INTERPRETED engine, which is what the default build and the
release use. The compiled engine (`--features wasm-gen`) does the same collection
in 0.41 s. The cause is measured, one arm of it is fixed (6x), the rest is filed,
and the gate ran on 22 kB and 23 kB collections instead — all of it in that
section. The original line was:
The generator, the `vyrn add` flow for a collection,
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
