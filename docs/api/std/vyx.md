# std/vyx

std/vyx — the `.vyx` single-file component compiler (RFC-0026 M4, RFC-0039 v2),
a template compiler written in comptime-pure Vyrn on RFC-0021 generator imports.
One `gen fn`, `components(dir)`, reads every `<Name>.vyx` file in `dir` at
compile time (sandboxed, deterministic, cached), parses its `<script>`/
`<template>` sections with a real, comment/string-aware scanner (the ICU
parser's big sibling), and synthesizes ONE module exporting one pure view
function per component. The compiler knows nothing about templates — everything
below is plain Vyrn over `listDir`/`readFile` (RFC-0021) emitting `std/html`
(RFC-0026 M1) hyperscript.

  import { components } from "std/vyx"
  import { itemRow, cart } from components("./components")

This is a SIBLING file to `std/ui` (the pages router), not a section of it, on
purpose: the router and the template compiler are two independent libraries on
the same mechanism, each large enough to read on its own, and their generated
helpers live in separate namespaces (`ui`-prefixed vs `vyx`-prefixed) so the
two never intermix. `std/ui`'s `pages` calls into this file's EXPOSED PURE
COMPILER CORE (`vyxCompileComponent` / `vyxBuildModule`) for `.vyx`-page sugar.

A `.vyx` file has two sections:
  <script>   — an optional `props { name: Type, … }` block (the view fn's
               parameters), optional `import { … } from "…"` lines (passed
               through, relative paths REBASED so they resolve from the
               synthesized module, and DEDUPED across all components — a
               component may import what it needs directly, RFC-0039 §3), and
               optional plain `fn`/`let`/`type` helpers (passed through,
               module-internal).
  <template> — exactly ONE root element, compiled to the view fn body.

Template grammar (Vue-flavored, RFC-0039 §1):
  {{ expr }}                  → text((expr).toString())     (always escaped)
  v-html="expr"               → Raw(expr) as the element's content (NOT escaped)
  <slot/>                     → splices the trailing children param
  v-if="c" / v-else-if="c" / v-else   → nested Empty-elided conditionals (each
                                directive on an element; the v-else-if/v-else
                                element must be the immediately following
                                element sibling — whitespace/comments between
                                are fine; a dangling one is a named diagnostic)
  v-for="x in expr"  (+ REQUIRED :key="expr")   → keyed loop
  :name="expr"  (incl. :class, :value)          → dynamic attribute
  name="…"                    → static attribute (`class` stays compile-checked
                                when themed)
  <Capitalized :p="e" q="s"/> / <Capitalized>…</Capitalized>  → component call
  @click="handler" / @click="handler(scalar)" (also @input/@change/@submit/…)
                              → On(event, "handler", (scalar).toString())

Whitespace rule: an all-whitespace text run that spans a newline (indentation
between block tags) is dropped; a same-line whitespace run is one significant
inline space; other runs have their whitespace collapsed to single spaces.
`<!-- … -->` comments and a lone `{` in text are inert (the `{` is literal;
only `{{` opens an interpolation).

Every generation failure is an identifier-carrying load diagnostic naming the
`.vyx` file and line (the std/ui / std/rpc convention). Inspect the whole
synthesized module with:  vyrn emit-gen <file>

## VyxAttr

```vyrn
type VyxAttr = { name: String, value: String, dyn: Bool, evt: Bool, line: Int64, col: Int64 }
```

One attribute on a template element. `dyn` = a `:name="expr"` dynamic value;
`evt` = an `@event` binding (its `name` has the `@` stripped). A `v-…`
directive is carried as a plain attribute whose `name` retains the `v-` prefix
(`dyn` false, since its value is written as a quoted string) and is consumed by
the grouping pass. `line`/`col` are the 1-based position of the attribute value
in the `.vyx` file (RFC-0033 origins).

## VyxNode

```vyrn
type VyxNode = VNText(String) | VNInterp(String, Int64, Int64) | VNRaw(String, Int64, Int64) | VNChildren | VNElem(String, Bool, Array<VyxAttr>, Array<VyxNode>, Int64) | VNIf(Array<String>, Array<Int64>, Array<Int64>, Array<VyxBody>, VyxBody, Bool) | VNFor(String, String, String, Array<VyxNode>, Int64, Int64)
```

A node in a parsed template. The trailing `Int64` pair on position-bearing
variants is the 1-based `(line, col)` of the pass-through in the `.vyx` file,
so RFC-0033 origin directives point at the exact source column. `VNIf` carries
PARALLEL per-condition `(line, col)` arrays so each `v-if`/`v-else-if` maps to
its own condition column.

## VyxBody

```vyrn
type VyxBody = { nodes: Array<VyxNode> }
```

A node-list wrapper, so an `Array<VyxNode>` can nest inside another node's
payload (an if-chain's per-branch bodies) without an `Array<Array<…>>`.

## VyxTemplate

```vyrn
type VyxTemplate = { nodes: Array<VyxNode>, err: String }
```

The parsed `<template>` of a `.vyx` source: its sibling nodes, or the `err`
that stopped the parse.

## vyxParseTemplate

```vyrn
fn vyxParseTemplate(source: String) -> VyxTemplate
```

Parse ONLY the `<template>` section of a `.vyx` source, into the same node
tree the compiler walks — tags, attributes, and the 1-based line and column of
every attribute value in the file.

Exported for the checking libraries (RFC-0100). A rule wants what the
compiler saw and nothing else: no props, no imports, no component resolution,
no sibling `v-if` grouping (a `v-if` / `v-for` stays a plain attribute here,
which is the form a rule reads it in anyway). `err` is non-empty when the
template does not parse, and a checking library says nothing in that case —
the compile is already failing, with a better message than a lint has.

`std/vyx-hints` holds no privilege over this: it is the same import any
third-party rule library writes.

## vyxCompileComponent

```vyrn
fn vyxCompileComponent(compName: String, source: String, dir: String) -> VyxComp
```

Compile one `.vyx` source into a `VyxComp` (parse only — component-call
resolution happens later, once every sibling name is known). `compName` is the
file stem; `dir` rebases relative imports. EXPORTED so `std/ui`'s `pages` can
reuse it for `.vyx` pages.

## vyxBuildModule

```vyrn
fn vyxBuildModule(comps: consume Array<VyxComp>, themed: Bool, theme: String) -> String
```

Assemble the whole synthesized module from the compiled components. When
`themed` (RFC-0036), the module imports the theme namespaced and every `class`
attribute is routed through `vyxTheme.cls(…)`; `theme` is the path passed to
`componentsThemed`, resolved (like `dir`) relative to the importing module.
EXPORTED so `std/ui`'s `pages` can build a page module from `.vyx` components.

## components

```vyrn
fn components(dir: String) -> String
```

`components(dir)` — compile every `<Name>.vyx` file under `dir` into ONE module
exporting one view function per component. `class="…"` emits an unchecked
`Cls(…)` (backward-compatible, unchanged). Reach for `componentsThemed` to
compile-check classes against a theme.

## componentsThemed

```vyrn
fn componentsThemed(dir: String, theme: String) -> String
```

`componentsThemed(dir, theme)` (RFC-0036) — like `components`, but the
synthesized module imports `theme` namespaced (`import * as vyxTheme from
tw(<theme>)`) and routes every `class` attribute through `vyxTheme.cls(…)`: a
STATIC class literal is proven `⊆ Tw` at compile time (a typo is a `vyrn check`
error mapped to the `.vyx` line:col), a dynamic `:class="expr"` is coerced to
`Tw` at runtime. `theme` resolves relative to the importing module, exactly
like `dir`.

## vyxDataForm

```vyrn
fn vyxDataForm(ret: String) -> Int64
```

Which alternative signature of the `std/ui:Page` contract's `data` member a
return-type spelling names — the same 0..3 `matchedMember` reports, or
`0 - 1` when the spelling is not a data type at all (RFC-0071 M2b):

  0  `Query<T>`            blocking, no params
  1  `Lazy<T>`             render-then-fill, no params
  2  `ParamQuery<P, T>`    blocking, deferred call takes `Params`
  3  `ParamLazy<P, T>`     render-then-fill, deferred call takes `Params`

Both facts the router needs — laziness and params-ness — are now read from
the DECLARED TYPE. Laziness used to be read from `data`'s body (the `lazy(…)`
call), which was the last source scan in the page pipeline; the `.vyx` file is
not a module, so this reads its declaration rather than a `moduleInterface`,
but it reads a *declaration* either way and a misspelled type is an error.

## vyxDataFormIsLazy

```vyrn
fn vyxDataFormIsLazy(form: Int64) -> Bool
```

Whether a `data` form renders lazily (RFC-0070).

## vyxDataFormHasParams

```vyrn
fn vyxDataFormHasParams(form: Int64) -> Bool
```

Whether a `data` form's deferred call takes the page's `Params`.

## vyxDataRunner

```vyrn
fn vyxDataRunner(form: Int64) -> String
```

The `std/ui` runner that turns a `data` form into the loaded value.

## vyxQueryDataType

```vyrn
fn vyxQueryDataType(ret: String) -> String
```

The DATA type a `data` return-type spelling carries ("" when `ret` is not one
of the four). This is what the page's synthesized `load()` returns, so
`Query<Result<Paste, PageError>>` still routes through the `Result` failure
path unchanged, and `ParamQuery<Params, Result<Paste, PageError>>` routes
identically — the data is the LAST type argument, because the params type
comes first when there is one.

EXPORTED so `std/ui` reads a `.vyrn` page's `data` return the same way, rather
than keeping a second copy of the rule.

## vyxPageInterface

```vyrn
fn vyxPageInterface(source: String) -> ModuleInterface
```

The PUBLIC surface a `.vyx` page's `<script>` declares — its statement-leading
`export fn` declarations — as the reflection a contract check reads.

A `.vyx` file is not a module (it is compiled INTO one), so there is no
`moduleInterface` to ask; this is the equivalent, read from the source the
author wrote. Everything NOT marked `export` is a private helper and is
outside the contract entirely, which is exactly RFC-0071's rule.

## vyxPageShape

```vyrn
fn vyxPageShape(source: String) -> VyxPageShape
```

Reflect a `.vyx` page's `<script>` — its `params` fields and loader presence.
EXPORTED so `std/ui` learns the page's routing shape without duplicating the
scanner. (`err` is set on a malformed `params` block.)

## vyxBuildPageModule

```vyrn
fn vyxBuildPageModule(source: String, dir: String, themed: Bool, theme: String) -> String
```

Build a full `.vyx`-PAGE module (RFC-0039 §4). The template compiles to a
`uiPageBody(<params>[, data: Data])` view fn; the module exports `page`/`Params`
(and passes through `load`/`Data` auto-exported). EXPORTED so `std/ui`'s `pages`
gen fn synthesizes it from a nested `vyxPage(…)` import. This is the wrapper
carrying the pre-RFC-0048 synthetic `UiPageBody.vyx` origins (still used by the
pure unit tests, which assert on the generated code, not on origins).

## vyxBuildPageClientModuleAt

```vyrn
fn vyxBuildPageClientModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildPageModuleAt` for the CLIENT bundle (RFC-0069 §1): `load` and its now-
dead imports are stripped, and `head`/`headTitle` are omitted (the title travels
in the payload, head assets through v2's additive head machinery). The compiled
view fn + `Params` + `page` are byte-for-byte what the server module produces.

## vyxBuildPageModuleAt

```vyrn
fn vyxBuildPageModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildPageModule` threading the REAL route-file path (RFC-0048 §2): when
`srcPath` is non-empty the compiled page's template AND script `//@origin`
directives target the real `.vyx` at real coordinates (via a template
line-shift + import/helper origin matching), instead of the synthetic
`UiPageBody.vyx`. The generated page/`respond` code is byte-identical either
way — only the origin comments differ.

## vyxPage

```vyrn
fn vyxPage(vyxPath: String) -> String
```

`vyxPage(vyxPath)` — synthesize a `.vyx` page module from the file at
`vyxPath` (RFC-0039 §4). A `gen fn` (its `readFile` is comptime-only). The
FULL `.vyx` path is the constant argument (not the stem), because the generator
sandbox admits reads of exactly its path arguments — reading `arg + ".vyx"`
would escape the declared inputs. `std/ui`'s `pages` imports this per `.vyx`
page: `import * as p<n> from vyxPage("routes/foo.vyx")`.

## vyxPageThemed

```vyrn
fn vyxPageThemed(vyxPath: String, theme: String) -> String
```

`vyxPageThemed(vyxPath, theme)` — the themed variant (RFC-0036): the page
body's classes are compile-checked against `theme`.

## vyxPageClient

```vyrn
fn vyxPageClient(vyxPath: String) -> String
```

`vyxPageClient(vyxPath)` — the CLIENT-bundle variant (RFC-0069 §1): the same
page compilation with `load` (and its dead imports) stripped and no head/title,
so `std/ui`'s `pagesClient` can render the page from its props on a soft nav.

## vyxPageClientThemed

```vyrn
fn vyxPageClientThemed(vyxPath: String, theme: String) -> String
```

`vyxPageClientThemed(vyxPath, theme)` — the themed client variant (RFC-0069 §1).

## vyxBuildLayoutModule

```vyrn
fn vyxBuildLayoutModule(source: String, dir: String, themed: Bool, theme: String) -> String
```

Build a `.vyx`-LAYOUT module: `layout(children: Array<Html>) -> Html` around
the compiled template (which must contain `<slot/>`), plus `head()`/
`headTitle()` from its head block. EXPORTED so `std/ui`'s router synthesizes it
from a nested `vyxLayout(…)` import.

## vyxBuildLayoutModuleAt

```vyrn
fn vyxBuildLayoutModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildLayoutModule` threading the real `layout.vyx` path (RFC-0048 §2).

## vyxLayout

```vyrn
fn vyxLayout(vyxPath: String) -> String
```

`vyxLayout(vyxPath)` — synthesize a `.vyx` layout module (RFC-0041 §1). A
`gen fn` (its `readFile` is comptime-only).

## vyxLayoutThemed

```vyrn
fn vyxLayoutThemed(vyxPath: String, theme: String) -> String
```

`vyxLayoutThemed(vyxPath, theme)` — the themed layout variant (RFC-0036): the
layout's template classes are compile-checked against `theme`.

## vyxBuildErrorModule

```vyrn
fn vyxBuildErrorModule(source: String, dir: String, themed: Bool, theme: String) -> String
```

Build a `.vyx`-ERROR module: `errorPage(e: PageError) -> Html` around the
compiled template, with an injected `error: PageError` prop. EXPORTED so
`std/ui`'s router synthesizes it from a nested `vyxError(…)` import.

## vyxBuildErrorModuleAt

```vyrn
fn vyxBuildErrorModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildErrorModule` threading the real `error.vyx` path (RFC-0048 §2). The
injected `PageError` import is generator-synthesized (not in the user file), so
it stays origin-less; the template + any user script map to the real file.

## vyxError

```vyrn
fn vyxError(vyxPath: String) -> String
```

`vyxError(vyxPath)` — synthesize a `.vyx` error-page module (RFC-0041 §3).

## vyxErrorThemed

```vyrn
fn vyxErrorThemed(vyxPath: String, theme: String) -> String
```

`vyxErrorThemed(vyxPath, theme)` — the themed error-page variant (RFC-0036).
