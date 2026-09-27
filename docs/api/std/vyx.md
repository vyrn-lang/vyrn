# std/vyx

The `.vyx` single-file component compiler, in comptime-pure Vyrn.

`components(dir)` reads every `<Name>.vyx` in `dir` at compile time and
synthesizes one module that exports one pure view function per component,
built from `std/html` calls. `std/ui`'s `pages` reuses the compiler core
(`vyxCompileComponent`, `vyxBuildModule`) for `.vyx` pages.

  import { components } from "std/vyx"
  import { itemRow, cart } from components("./components")

A `.vyx` file has two sections:
  <script>   -- optional: a `props { name: Type, ... }` block (the view fn's
               parameters), `import` lines (relative paths rebased to the
               synthesized module, deduplicated across components), and
               plain `fn`/`let`/`type` helpers (module-private).
  <template> -- exactly one root element, compiled to the view fn body.

Template grammar (Vue-flavored):
  {{ expr }}                  -> text((expr).toString())     (escaped)
  v-html="expr"               -> Raw(expr) as the element's content (not escaped)
  <slot/>                     -> splices the trailing children param
  v-if="c" / v-else-if="c" / v-else   -> nested Empty-elided conditionals; a
                                v-else-if/v-else must be the next element
                                sibling (whitespace and comments between are
                                fine)
  v-for="x in expr"  (+ required :key="expr")   -> keyed loop
  :name="expr"  (incl. :class, :value)          -> dynamic attribute
  name="..."                  -> static attribute (`class` is checked when
                                themed)
  <Capitalized :p="e" q="s"/> / <Capitalized>...</Capitalized>  -> component call
  @click="handler" / @click="handler(scalar)" (also @input/@change/@submit/...)
                              -> On(event, "handler", (scalar).toString())

Whitespace: an all-whitespace run that spans a newline is dropped; a
same-line whitespace run is one space; other runs collapse to single spaces.
`<!-- ... -->` comments are inert, and a lone `{` in text is literal.

Every generation failure is a `std/diag` report at the `.vyx` line it came
from. `vyrn emit-gen <file>` shows the synthesized module.

Generation-time components. A capitalized tag resolves first
against sibling `.vyx` files, then against names the `<script>` sections
import. An imported name is a provider: `std/vyx` emits a nested generator
import that runs the imported `gen fn` with the tag's attributes as constant
arguments, and splices its output where the tag stood.

  <script>
  import { Icon } from "./icon-provider"
  </script>
  <template>
  <p><Icon name="brand:github" label="GitHub"/></p>
  </template>

A provider is an ordinary module that exports a `gen fn` of this shape:

  export gen fn <TagName>(attrs: String, file: String, line: Int64, col: Int64) -> String

  attrs  the tag's static attributes as a JSON object of strings, in source
         order, written by `std/json` so `std/jsonread` reads it back exactly;
         `{}` when there are none.
  file   the `.vyx` file the tag was written in.
  line   the tag's 1-based line in that file.
  col    always 1: a template node carries no column.

The generated module must export

  export fn provide() -> Html

which `std/vyx` calls at the tag site through a namespace alias it mints.
The name is not `render`: a user function named after a surface builtin
shadows it program-wide, so it would break `render` here.

A provider tag takes static attributes only and no children; `:attr`,
`@event` and children are diagnostics. Compute a value in a sibling `.vyx`
component instead. The provider runs in its own sandbox and cache entry, and
anchors its own reports at the `file`/`line` it is given. `std/vyx` cannot
check the provider's shape, because a template-named provider is outside its
read roots; a wrong shape fails the emitted import with the loader's
diagnostic.

## VyxAttr

```vyrn
type VyxAttr = { name: String, value: String, dyn: Bool, evt: Bool, line: Int64, col: Int64 }
```

One attribute on a template element. `dyn`: a `:name="expr"` value; `evt`:
an `@event` binding, `@` stripped. A `v-...` directive is a plain attribute
that keeps its `v-` prefix and is consumed by the grouping pass.
`line`/`col`: the 1-based position of the value in the `.vyx` file.

## VyxNode

```vyrn
type VyxNode = VNText(String) | VNInterp(String, Int64, Int64) | VNRaw(String, Int64, Int64) | VNChildren | VNElem(String, Bool, Array<VyxAttr>, Array<VyxNode>, Int64) | VNIf(Array<String>, Array<Int64>, Array<Int64>, Array<VyxBody>, VyxBody, Bool) | VNFor(String, String, String, Array<VyxNode>, Int64, Int64)
```

A node in a parsed template. Lines are absolute in the `.vyx` file. A
trailing `(line, col)` pair is the pass-through's position for
origin directives; `VNIf` carries parallel per-condition position arrays.

## VyxBody

```vyrn
type VyxBody = { nodes: Array<VyxNode> }
```

A node list, so bodies can nest in a payload without `Array<Array<...>>`.

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

Parses only the `<template>` section of a `.vyx` source into the node tree
the compiler walks, for checking libraries such as `std/vyx-hints`. No
props, imports, component resolution or `v-if` grouping: directives stay
attributes. `err` is non-empty when the template does not parse.

## vyxCompileComponent

```vyrn
fn vyxCompileComponent(compName: String, source: String, dir: String) -> VyxComp
```

Compiles one `.vyx` source into a `VyxComp`; component calls are resolved
later, once every sibling name is known. `compName` is the file stem; `dir`
rebases relative imports. Exported for `std/ui`'s `pages`.

## vyxBuildModule

```vyrn
fn vyxBuildModule(comps: consume Array<VyxComp>, themed: Bool, theme: String) -> String
```

Assembles the synthesized module. When `themed`, it imports the theme
`theme` (resolved like `dir`) and routes every `class` through
`vyxTheme.cls(...)`. Exported for `std/ui`'s `pages`.

## components

```vyrn
fn components(dir: String) -> String
```

Compiles every `<Name>.vyx` under `dir` into one module exporting one view
per component. `class` is unchecked; `componentsThemed` checks it.

## componentsThemed

```vyrn
fn componentsThemed(dir: String, theme: String) -> String
```

[`components`] with a theme: the module imports `tw(<theme>)` as
`vyxTheme`, a static class literal is checked against `Tw` at compile time
at its `.vyx` position, and a dynamic `:class` is coerced at run time.
`theme` resolves relative to the importing module, like `dir`.

## vyxDataForm

```vyrn
fn vyxDataForm(ret: String) -> Int64
```

Which alternative signature of the `std/ui:Page` `data` member a return-type
spelling names, numbered as `matchedMember` reports, or -1 for none:

  0  `Query<T>`            blocking, no params
  1  `Lazy<T>`             render-then-fill, no params
  2  `ParamQuery<P, T>`    blocking, deferred call takes `Params`
  3  `ParamLazy<P, T>`     render-then-fill, deferred call takes `Params`

## vyxDataFormIsLazy

```vyrn
fn vyxDataFormIsLazy(form: Int64) -> Bool
```

Whether a `data` form renders lazily.

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

The data type a `data` return-type spelling carries ("" when `ret` is none
of the four): the last type argument, since a params type comes first. This
is what the synthesized `load()` returns. Exported so `std/ui` reads a
`.vyrn` page's `data` the same way.

## vyxPageInterface

```vyrn
fn vyxPageInterface(source: String) -> ModuleInterface
```

The public surface of a `.vyx` page's `<script>`, its statement-leading
`export fn` declarations, as reflection for a contract check. The page has
no `moduleInterface`, so this reads the source; unexported helpers are
outside the contract.

## vyxPageShape

```vyrn
fn vyxPageShape(source: String) -> VyxPageShape
```

Reflects a `.vyx` page's `<script>`: its `params` fields and loader. `err` is
set on a malformed `params` block. Exported for `std/ui`.

## vyxBuildPageModule

```vyrn
fn vyxBuildPageModule(source: String, dir: String, themed: Bool, theme: String) -> String
```

Builds a `.vyx` page module: the template compiles to a
`uiPageBody(<params>[, data: Data])` view, and the module exports `page` and
`Params`. Origins point at a synthetic `UiPageBody.vyx`; the unit tests use
it.

## vyxBuildPageClientModuleAt

```vyrn
fn vyxBuildPageClientModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildPageModuleAt` for the client bundle: `load`
and its dead imports are stripped and `head`/`headTitle` omitted. The view,
`Params` and `page` are the server module's, byte for byte.

## vyxBuildPageModuleAt

```vyrn
fn vyxBuildPageModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildPageModule` with the real route-file path: when `srcPath` is set,
origins target the real `.vyx`. The generated code is the same either way.

## vyxPage

```vyrn
fn vyxPage(vyxPath: String) -> String
```

Synthesizes a `.vyx` page module from `vyxPath`. The
argument is the full `.vyx` path, because the generator sandbox admits reads
of exactly its path arguments.

## vyxPageThemed

```vyrn
fn vyxPageThemed(vyxPath: String, theme: String) -> String
```

[`vyxPage`] with the page's classes checked against `theme`.

## vyxPageClient

```vyrn
fn vyxPageClient(vyxPath: String) -> String
```

[`vyxPage`] for the client bundle: no `load`, head or
title, for `std/ui`'s `pagesClient`.

## vyxPageClientThemed

```vyrn
fn vyxPageClientThemed(vyxPath: String, theme: String) -> String
```

[`vyxPageClient`] with classes checked against `theme`.

## vyxBuildLayoutModule

```vyrn
fn vyxBuildLayoutModule(source: String, dir: String, themed: Bool, theme: String) -> String
```

Builds a `.vyx` layout module: `layout(children: Array<Html>) -> Html` around
the compiled template, which must contain `<slot/>`, plus `head()` and
`headTitle()`.

## vyxBuildLayoutModuleAt

```vyrn
fn vyxBuildLayoutModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildLayoutModule` with the real `layout.vyx` path for origins.

## vyxLayout

```vyrn
fn vyxLayout(vyxPath: String) -> String
```

Synthesizes a `.vyx` layout module from `vyxPath`.

## vyxLayoutThemed

```vyrn
fn vyxLayoutThemed(vyxPath: String, theme: String) -> String
```

[`vyxLayout`] with the layout's classes checked against `theme`.

## vyxBuildErrorModule

```vyrn
fn vyxBuildErrorModule(source: String, dir: String, themed: Bool, theme: String) -> String
```

Builds a `.vyx` error module: `errorPage(e: PageError) -> Html` around the
compiled template, with the injected `error` prop.

## vyxBuildErrorModuleAt

```vyrn
fn vyxBuildErrorModuleAt(source: String, srcPath: String, dir: String, themed: Bool, theme: String) -> String
```

`vyxBuildErrorModule` with the real `error.vyx` path. The injected
`PageError` import has no origin.

## vyxError

```vyrn
fn vyxError(vyxPath: String) -> String
```

Synthesizes a `.vyx` error-page module from `vyxPath`.

## vyxErrorThemed

```vyrn
fn vyxErrorThemed(vyxPath: String, theme: String) -> String
```

[`vyxError`] with classes checked against `theme`.
