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

## `<Icon>` in a template — the provider recipe (RFC-0107 M3)

A template's `<Icon name="ui:github"/>` is a GENERATION-TIME PROVIDER: the
template compiler emits a generator import of a `gen fn` the `<script>`
imported, with the tag's static attributes as constant arguments. The provider
is a module THE PROJECT writes, and it is three lines, because the prefix
vocabulary is the one thing a library cannot supply — a collection alias is a
key in the project's own `vyrn.json`:

```vyrn
// app/icons.vyrn — the project's provider. `vyrn.json` binds `ui` and
// `codex` to two pinned collections; those two names are the prefixes.
import { iconProvider } from "std/icons"

export gen fn Icon(attrs: String, file: String, line: Int64, col: Int64) -> String {
    return iconProvider(attrs, file, line, col, "ui codex")
}
```

```
<script>
import { Icon } from "../icons"
</script>
<template>
<a href="…"><Icon name="ui:github"/> Source on GitHub</a>
<button><Icon name="ui:search" label="Search this site"/></button>
</template>
```

The tag takes `name` (required, `collection:glyph` when the provider binds
more than one collection), and optionally `size` (any CSS length — `1.25em`),
`label` (the accessible name: the glyph becomes `role="img"` with that
`aria-label` and stops being `aria-hidden`) and `class`. Any other attribute
is refused by name, at the tag.

[`iconProvider`] READS NOTHING. It emits an [`iconsAt`] import naming the
collection, so the alias is that nested generation's own constant argument —
which is what lets a collection stay a `vyrn.json` dependency even though the
attribute JSON a provider receives can declare no input root (RFC-0107 M2,
contradiction 1).

## For another template language

[`iconsAt`] is the generator with an anchor: a collection, the glyph names, and
the `file`/`line`/`col` a report about a misspelled glyph should land on.
[`iconsModule`] is the same with the read already done — hand it the collection
TEXT. Neither knows what `.vyx` is; the dependency arrow points one way.

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

## iconsAt

```vyrn
fn iconsAt(collection: String, names: String, anchorFile: String, line: Int64, col: Int64) -> String
```

[`icons`] with an anchor: `anchorFile`/`line`/`col` is where a report about a
glyph lands — the tag in a template that asked for it, rather than the
collection file, which is what a reader of the message can act on.

This is the surface a provider emits an import of (RFC-0107 M3), and the
reason the collection can be a `vyrn.json` alias: named HERE it is this
generation's own constant argument, so the import map and the input-root rule
both apply to it (RFC-0107 M2).

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

## iconProvider

```vyrn
fn iconProvider(attrs: String, file: String, line: Int64, col: Int64, collections: String) -> String
```

The whole of a project's `<Icon>` provider, minus the one thing a library
cannot know: which collections the project pinned. `collections` is a
space-separated list of `vyrn.json` dependency aliases, and those names ARE
the tag's prefix vocabulary (`name="ui:github"`). With exactly one collection
bound a bare `name="github"` means that one; with more than one a bare name is
refused rather than resolved by position.

`attrs`, `file`, `line` and `col` are the provider protocol's own arguments
(RFC-0107 M1) — pass them straight through. The header of this module carries
the three-line project module that calls this.

It reads NOTHING. What it returns is an [`iconsAt`] import naming the
collection, so the collection is that nested generation's constant argument
and an alias resolves there.

## iconAttrs

```vyrn
fn iconAttrs(g: consume Html, size: String, label: String, class: String) -> Html
```

A generated glyph with the use site's own attributes on it — the runtime half
of the provider, and the only part of `std/icons` that runs outside a
generator.

A `size` (any CSS length) replaces the `1em` the glyph was drawn at. A `label`
makes the glyph CONTENT rather than decoration: it gains `role="img"` and that
`aria-label`, and loses `aria-hidden`, which is the whole of the a11y rule an
icon has to follow — an icon beside a label is hidden, an icon that IS the
label is named. A `class` is appended and nothing else is touched.
