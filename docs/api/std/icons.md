# std/icons

Inline SVG icons generated from a pinned Iconify collection, one function
per named glyph. Resolution happens at generation time from hash-locked
bytes, and a misspelled glyph fails the build with the nearest name.

```vyrn
import { icons } from "std/icons"
import * as ic from icons("icons", "github rss circle-check")
import { toHtmlString } from "std/html"

fn main() -> Int64 {
    print(toHtmlString(ic.github()))
    return 0
}
```

`"icons"` is a relative `.json` path or a `vyrn.json` dependency alias
(`vyrn add github:iconify/icon-sets@<sha>/json/lucide.json --name icons`).
Each glyph `circle-check` becomes `fn circleCheck() -> Html`: an inline
`<svg>` sized `1em`, in `currentColor`, and `aria-hidden`. Two names that
camel-case to one identifier are refused.

A template's `<Icon name="ui:github"/>` needs a project provider module:

```vyrn
// `vyrn.json` binds `ui` and `codex`; those names are the prefixes.
import { iconProvider } from "std/icons"

export gen fn Icon(attrs: String, file: String, line: Int64, col: Int64) -> String {
    return iconProvider(attrs, file, line, col, "ui codex")
}
```

The tag takes `name`, and optionally `size` (a CSS length), `label` (the
accessible name) and `class`; any other attribute is refused.

## icons

```vyrn
fn icons(collection: String, names: String) -> String
```

One function per glyph in `names` (space-separated Iconify names), read
from `collection`: a relative `.json` path or a `vyrn.json` dependency alias.

## iconsAt

```vyrn
fn iconsAt(collection: String, names: String, anchorFile: String, line: Int64, col: Int64) -> String
```

[`icons`] with reports anchored at `anchorFile`/`line`/`col`, the template
tag that asked. A provider emits an import of this, so the collection alias
is this generation's own constant argument and resolves through `vyrn.json`.

## iconsModule

```vyrn
fn iconsModule(collectionText: String, collectionPath: String, names: String, anchorFile: String, line: Int64, col: Int64) -> String
```

[`icons`] over collection text already read. `collectionPath` is only
named in diagnostics and the header. An empty `anchorFile` anchors reports
in the collection file.

## iconProvider

```vyrn
fn iconProvider(attrs: String, file: String, line: Int64, col: Int64, collections: String) -> String
```

A project's `<Icon>` provider. `collections` is a space-separated list of
`vyrn.json` aliases, and those names are the tag's prefixes. With one
collection bound a bare name means that one; with more it is refused.
Reads nothing: it emits an [`iconsAt`] import, where the alias resolves.

## iconAttrs

```vyrn
fn iconAttrs(g: consume Html, size: String, label: String, class: String) -> Html
```

Applies the use site's attributes to a generated glyph at run time. A `size`
replaces `1em`; a `label` adds `role="img"` and `aria-label` and removes
`aria-hidden`; a `class` is appended.
