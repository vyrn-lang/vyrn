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
               parameters), `import` lines (relative paths rebased to the
               synthesized module, deduplicated across components), and
               plain `fn`/`let`/`type` helpers (module-private).
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

## VyxAttr

```vyrn
type VyxAttr = { name: String, value: String, dyn: Bool, evt: Bool, line: Int64, col: Int64 }
```

One attribute of a template element. `name` drops a leading `:` or `@`; a
`v-` directive keeps its prefix (`v-if`, `v-for`). `value` is the quoted
text, trimmed, and `""` for a bare attribute (`disabled`). `dyn` marks
`:name="expr"`, whose value is a Vyrn expression; `evt` marks
`@event="handler"`. `line` and `col` are 1-based in the `.vyx` file: the
value's position, or the name's for a bare attribute.

## VyxSpan

```vyrn
type VyxSpan = { text: String, line: Int64, col: Int64 }
```

Text of a `.vyx` file and the 1-based line and column it starts at there.

## VyxNode

```vyrn
type VyxNode = enum { VNText(String), VNInterp(String, Int64, Int64), VNRaw(String, Int64, Int64), VNChildren, VNElem(String, Bool, Array<VyxAttr>, Array<VyxNode>, Int64), VNIf(Array<String>, Array<Int64>, Array<Int64>, Array<VyxBody>, VyxBody, Bool), VNFor(String, VyxSpan, String, Array<VyxNode>, Int64, Int64, Int64, Int64) }
```

A node of a parsed template. Lines and columns are 1-based in the `.vyx`
file. [`vyxParseTemplate`](#vyxparsetemplate) yields four variants:

- `VNText(text)`: literal text, each whitespace run collapsed to one space.
  A whitespace-only run that holds a line break yields no node.
- `VNInterp(expr, line, col)`: `{{ expr }}`, the expression trimmed.
- `VNChildren`: `<slot/>`.
- `VNElem(tag, isComp, attrs, kids, line)`: an element, or a component when
  `tag` starts with an uppercase letter (`isComp`). `line` is the `<`'s.
  A self-closing tag has no `kids`.

The compiler's grouping pass consumes the directives and makes the other
three: `VNRaw(expr, line, col)` for `v-html`; `VNIf(conds, condLines,
condCols, bodies, elseBody, hasElse)` for a `v-if` chain, one body per
condition; and `VNFor(var, iter, key, body, line, col, keyLine, keyCol)` for
`v-for` with its `:key`, where `iter` is the iterable and its position and
`line` and `col` are the header's. A `match` has no wildcard arm, so a walker names
them too.

## VyxBody

```vyrn
type VyxBody = { nodes: Array<VyxNode> }
```

One branch of a `VNIf`: a node list, so bodies nest in a payload without
`Array<Array<VyxNode>>`.

## VyxTemplate

```vyrn
type VyxTemplate = { nodes: Array<VyxNode>, err: String }
```

The parsed `<template>` of a `.vyx` source: its top-level nodes, and in
`err` the diagnostic that stopped the parse, or `""`. After an error,
`nodes` holds what parsed before it.

## vyxParseTemplate

```vyrn
fn vyxParseTemplate(source: String) -> VyxTemplate
```

Parses the `<template>` section of the `.vyx` text `source` into the tree
the compiler walks, for a generator or a checker such as `std/vyx-hints`.
It reads no props or imports, resolves no component and groups no
directive: `v-if`, `v-for` and `v-html` stay attributes. `err` is set when
`source` has no `<template>` or it does not parse. Call it during
generation only.

A generator that writes the outline of a template, saved as `outline.vyrn`:

```vyrn
import { vyxParseTemplate, VyxAttr, VyxNode } from "std/vyx"

/// ` :name` for each bound attribute in `attrs`.
gen fn bound(attrs: Array<VyxAttr>) -> String {
    let mut out = ""
    for a in attrs {
        if a.dyn {
            out = out + " :" + a.name
        }
    }
    return out
}

/// One line per element, depth first: its tag, line and bound attributes.
gen fn walk(nodes: Array<VyxNode>) -> String {
    let mut out = ""
    for n in nodes {
        out = out + match n {
            VNElem(tag, _, attrs, kids, line) => tag + ":" + line.toString() + bound(attrs) + "\n" + walk(kids),
            VNText(_) => "",
            VNInterp(_, _, _) => "",
            VNRaw(_, _, _) => "",
            VNChildren => "",
            VNIf(_, _, _, _, _, _) => "",
            VNFor(_, _, _, _, _, _, _, _) => "",
        }
    }
    return out
}

/// Writes `outlineOf() -> String`: the outline of `source`, or why it does
/// not parse.
export gen fn outline(source: String) -> String {
    let t = vyxParseTemplate(source)
    let text = if t.err != "" { t.err.copy() } else { walk(t.nodes) }
    return render(vyrn"export fn outlineOf() -> String { return \{text} }")
}
```

A program that prints `ul:2` and `li:3 :class`:

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
fn components(dir: String) -> String
```

Compiles every `<Name>.vyx` under `dir` into one module exporting one view
per component. `class` is unchecked; `componentsThemed` checks it.

## componentsThemed

```vyrn
fn componentsThemed(dir: String, theme: String) -> String
```

[`components`](#components) with a theme: the module imports `tw(<theme>)` as
`vyxTheme`, a static class literal is checked against `Tw` at compile time
at its `.vyx` position, and a dynamic `:class` is coerced at run time.
`theme` resolves relative to the importing module, like `dir`.
