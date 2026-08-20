# std/icons

std/icons — an icon collection is a LIBRARY (RFC-0107 M2), not a compiler
feature and not a template built-in.

An Iconify collection is one JSON file: `{prefix, icons: {name: {body, …}},
aliases?, width, height, info}`. Pin it like any other dependency and name the
glyphs you want in the import:

```vyrn
import { icons } from "std/icons"
import * as ic from icons("icons", "github rss circle-check")
import { toHtmlString } from "std/html"

fn main() -> Int64 {
    print(toHtmlString(ic.github()))
    return 0
}
```

`"icons"` is the collection: a relative path to a `.json` file, or — the point
of this module — a DEPENDENCY ALIAS from `vyrn.json`, so the bytes come from
`vyrn.lock` and the content-addressed cache:

```
vyrn add github:iconify/icon-sets@<sha>/json/lucide.json --name icons
```

Everything after that is offline. Resolution happens at GENERATION time from
hash-locked bytes: nothing is fetched at run time, ever, and a misspelled glyph
fails the build with a nearest-name suggestion instead of rendering an empty
box. Only the glyphs you name are generated, so the artifact carries exactly
what it uses and the editor has five functions to analyze rather than 1,800.

The generated module exports one `fn <name>() -> Html` per glyph — an inline
`<svg>` that inherits `currentColor` and the surrounding font size, and is
`aria-hidden="true"` because an icon beside a label is decoration. Give it an
accessible name at the USE site when the icon is the whole content.

Names are Iconify's own, hyphens and all; the function is the lowerCamelCase
of the name (`circle-check` becomes `circleCheck`), which is the house
spelling for an identifier. Two glyphs whose names camel-case to one
identifier are refused rather than silently collapsed.

## For a template language

[`iconsModule`] is the whole generator with the read already done: hand it the
collection TEXT, the path to name in diagnostics, the glyph names, and an
anchor (`file`, `line`, `col`) to report against. That is what an RFC-0107
provider — a `gen fn` a `.vyx` `<script>` imports — calls, and it is why this
module knows nothing about `.vyx`. The dependency arrow points one way.

Inspect any generated module with:  vyrn emit-gen <file>

## icons

```vyrn
fn icons(collection: String, names: String) -> String
```

One module, one function per named glyph, read from the collection at
`collection` — a relative `.json` path or a `vyrn.json` dependency alias.

`names` is a space-separated list of Iconify names. The read goes through the
generation sandbox, so the collection is one of this generator's declared
inputs and a change to it invalidates exactly this generation.

## iconsModule

```vyrn
fn iconsModule(collectionText: String, collectionPath: String, names: String, anchorFile: String, line: Int64, col: Int64) -> String
```

The generator with the read already done — the surface a template language's
provider calls (RFC-0107 M1's protocol).

`collectionPath` is only ever NAMED, in diagnostics and in the generated
header; nothing is read from it here. `anchorFile`/`line`/`col` is where a
report about a glyph should land — a tag in a template, say. Pass `""` for
`anchorFile` and reports anchor in the collection file instead.
