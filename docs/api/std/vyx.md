# std/vyx

The `.vyx` single-file component compiler, in comptime-pure Vyrn.

`components(dir)` reads every `<Name>.vyx` in `dir` at compile time and
synthesizes one module that exports one pure view function per component,
built from `std/html` calls. `std/ui`'s `pages` compiles `.vyx` pages,
layouts and error pages through `vyxPage`, `vyxLayout` and `vyxError`.
[`vyxParseTemplate`](#vyxparsetemplate) gives a generator or a tool the parsed `<template>`.

  import { components } from "std/vyx"
  import { itemRow, cart } from components("./components")

A `.vyx` file has two sections:
  <script>   -- optional: a `props { name: Type, ... }` block (the view fn's
               parameters), `import` lines (copied as written, so a
               relative path resolves beside the `.vyx`; a name a sibling
               already imports from the same path is dropped), and plain
               `fn`/`let`/`type` helpers (module-private).
  <template> -- exactly one root element, compiled to the view fn body.

Template grammar (Vue-flavored):
  {{ expr }}                  -> text((expr).toString())     (escaped)
  v-html="expr"               -> Raw(expr) as the element's content; `expr` is a
                                `Markup`, which `Raw` never escapes
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
`<!-- ... -->` comments are inert, and a lone `{` in text is literal. The grammar
is `std/vyx.syntax`, and each of its rows has its sentence in `std/vyx.errors`.

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

  export gen fn <TagName>(attrs: String, file: String, line: Int64, col: Int64) -> Code

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

## VyxSpan

```vyrn
type VyxSpan = { text: String, line: Int64, col: Int64 }
```

Text of a `.vyx` file and the 1-based line and column it starts at there.

## VyxTemplate

```vyrn
type VyxTemplate = { nodes: Array<vx.VyxNode>, err: String }
```

The parsed `<template>` of a `.vyx` source: its top-level nodes in the tree
`std/vyx.syntax` writes, and in `err` the sentence of the row that stopped the
parse, or `""`.

## vyxParseTemplate

```vyrn
fn vyxParseTemplate(source: String) -> VyxTemplate
```

Parses the `<template>` section of the `.vyx` text `source` into its tree, for a
generator or a checker such as `std/vyx-hints`. It reads no props or imports,
resolves no component and groups no directive: `v-if`, `v-for` and `v-html` stay
attributes, a comment is no node, and `<slot/>` is an element. `err` is set when
`source` has no `<template>`, or it does not parse. Call it during generation only.

A generator that writes the outline of a template, saved as `outline.vyrn`:

```vyrn
import { syntax } from "std/syntax"
import * as vx from syntax("std/vyx.syntax")
import { vyxParseTemplate } from "std/vyx"

/// ` :name` for each bound attribute in `attrs`.
gen fn bound(attrs: Array<vx.VyxAttr>) -> String {
    let mut out = ""
    for a in attrs {
        if let Some(mark) = a.dyn {
            out = out + " :" + a.name.text
        }
    }
    return out
}

/// One line per element, depth first: its tag, its offset and bound attributes.
gen fn walk(nodes: Array<vx.VyxNode>) -> String {
    let mut out = ""
    for n in nodes {
        if let VyxNodeVyxElem(e) = n {
            out = out + e.tag.text + ":" + e.start.toString() + bound(e.attrs) + "\n"
            if let Some(kids) = e.body {
                out = out + walk(kids.nodes)
            }
        }
    }
    return out
}

/// Writes `outlineOf() -> String`: the outline of `source`, or why it does
/// not parse.
export gen fn outline(source: String) -> Code {
    let t = vyxParseTemplate(source)
    let text = if t.err != "" { t.err.copy() } else { walk(t.nodes) }
    return vyrn"export fn outlineOf() -> String { return \{text} }"
}
```

A program that prints `ul:11` and `li:18 :class`:

```vyrn
import { outline } from "./outline"
import { outlineOf } from outline("<template>\n<ul>\n  <li :class=\"c\">{{ x }}</li>\n</ul>\n</template>")

fn main() -> Int64 {
    print(outlineOf())
    return 0
}
```

## components

```vyrn
fn components(dir: String) -> Code
```

Compiles every `<Name>.vyx` under `dir` into one module exporting one view
per component. `class` is unchecked; `componentsThemed` checks it.

## componentsThemed

```vyrn
fn componentsThemed(dir: String, theme: String) -> Code
```

[`components`](#components) with a theme: the module imports `tw(<theme>)` as
`vyxTheme`, a static class literal is checked against `Tw` at compile time
at its `.vyx` position, and a dynamic `:class` is coerced at run time.
`theme` resolves relative to the importing module, like `dir`.
