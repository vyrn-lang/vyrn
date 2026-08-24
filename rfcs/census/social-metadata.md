# Metadata for every route

Branch `site/metadata`, on top of `site/visual-defects`.

The owner's words: "improve social media metadata of pages, make each url show
relevant info, make it beautiful."

## What was wrong, measured

The export publishes 80 documents: 74 routes and 6 redirect stubs. Before this
branch, 67 of the 80 carried the same `<meta name="description">` — one
paragraph about the language, written once in `pageHead` and stamped on every
page that was not one of the thirteen the table in `site/app/meta.vyrn` named.

```
$ grep -ho 'name="description" content="[^"]*"' out/*.html out/*/*.html out/*/*/*.html | sort | uniq -c | sort -rn | head -1
     67 name="description" content="Vyrn is a systems language with the expressiveness of TypeScript. …"
```

So a link to `std/json`, a link to `/guide/values` and a link to
`examples/shelf` previewed as the same page, in a chat, in a search result and
on a social site. The card was built from that description, so all three cards
were the same card.

After: 74 routes, 74 distinct descriptions. The six stubs keep the paragraph and
are not routes — a stub is the old name of a page, and the export gives it no
card at all.

| | before | after |
|---|---|---|
| published documents | 80 | 80 |
| routes (documents that are not stubs) | 74 | 74 |
| distinct descriptions among routes | 13 | 74 |
| routes with a canonical URL | 0 | 74 |
| routes with `og:url` | 0 | 74 |
| description length, characters | — | min 58, mean 108, max 152 |

## What every route now emits

| tag | value | where it comes from |
|---|---|---|
| `<title>` | the route's own title | unchanged; the page writes it |
| `<meta name="description">` | the route's own sentence | see the next table |
| `<link rel="canonical">` | `siteOrigin() + published(path)` | `site/app/repo.vyrn:153`, `site/app/nav.vyrn:122` |
| `og:site_name` | `Vyrn` | written here — see "What is written by hand" |
| `og:type` | `website` for the home page and the ten section indexes, `article` for the other 63 | `isIndex` reads the path |
| `og:title` | the `<title>` with the site's name taken off the end | `withoutSiteName` |
| `og:description` | the same string as `description` | read back out of the document |
| `og:url` | the same as canonical | |
| `article:section` | the section index's own title, for an `article` | `metaOf(sectionPath(path)).title` |
| `twitter:card` | `summary` | |
| `twitter:title`, `twitter:description` | the same values as their `og` counterparts | |
| `<meta name="theme-color">` | two, one per colour scheme | `--n4` read out of `site/public/style.css` |
| JSON-LD | `SoftwareApplication` on the home page, `TechArticle` on every `article` | the same title, description and URL as the card |

No `og:image`. See "The image question".

## Where each description comes from

Nothing below is typed twice. Every string is the one the page already renders.

| routes | count | source | function |
|---|---|---|---|
| the named consumer pages | 13 | `pageMetas()`, unchanged | `pageHeadOf` |
| chapters of the book, Web and Tooling | 19 | the chapter's own `lede`, through `inlineCode` so the backticks become code spans and then plain words | `chapterBlurb`, `site/app/guide.vyrn:673` |
| reference modules | 38 | the module's first `///` line off `std/`, plus its export count | `apiBlurb`, `site/app/docs.vyrn:176` |
| packages | 4 | the package's own summary, with the package named in front of it | `packageBlurb`, `site/app/packages.vyrn:169` |

All four go through one function, `blurbFrom` in `site/app/meta.vyrn`, which

- drops the tags and decodes the entities, because the prose is HTML and an
  attribute is not;
- cuts at the last full stop that fits, so a description ends a sentence;
- falls back to a word boundary with an ellipsis when that would leave less than
  50 bytes, which is what happens on the three routes listed below;
- drops a dangling comma or em dash in front of the ellipsis.

Three of the 74 descriptions are truncated:

```
docs/std/symbolmap  std/symbolmap (RFC-0073 M1) — the shared shape of a generated module's symbol map: for each exported symbol, the source declaration it… 5 exports.
docs/std/vyx        std/vyx — the .vyx single-file component compiler (RFC-0026 M4, RFC-0039 v2), a template compiler written in comptime-pure Vyrn on… 31 exports.
tooling/commands    One binary. vyrn run needs nothing else on your machine; vyrn build writes a native binary; every other task — tests, benchmarks, formatting, docs…
```

Two routes lose a clause to the 155-byte ceiling rather than an ellipsis,
because their last full sentence ends earlier: `/guide/ownership` stops after
"frees the memory" and drops "There is no collector, no lifetime syntax, and no
borrow annotation", and `/web/styling` stops after the first of its two
sentences. Both remaining descriptions are true and whole. Raising the ceiling
to 160 would fit both; 155 is the number the brief set and the number the site's
own table already sat under.

## What is written by hand, and why nothing could supply it

Three strings, and no fourth:

1. `og:site_name` is `Vyrn`. It is the site's name. No function in the
   repository returns it — `siteOrigin()` returns a URL and the titles carry it
   inside a longer sentence — and deriving one word out of a title by cutting it
   would be more machinery than the word.
2. The two schema.org vocabulary words, `SoftwareApplication` and
   `TechArticle`, and `DeveloperApplication` beside them. These are the
   vocabulary's own terms, not facts about this site.
3. `inLanguage: "en"`, which repeats the `lang="en"` the export already stamps
   on every document (`withLang`).

Every description, every title and every URL is derived.

## The ten examples
### `/`

```html
<title>Vyrn — a systems language with the expressiveness of TypeScript</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="Types carry the rules that make a value valid. Ownership is a word you write on a parameter. No garbage collector, three backends, one result.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="website">
<meta property="og:title" content="Vyrn — a systems language with the expressiveness of TypeScript">
<meta property="og:description" content="Types carry the rules that make a value valid. Ownership is a word you write on a parameter. No garbage collector, three backends, one result.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="Vyrn — a systems language with the expressiveness of TypeScript">
<meta name="twitter:description" content="Types carry the rules that make a value valid. Ownership is a word you write on a parameter. No garbage collector, three backends, one result.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"SoftwareApplication","name":"Vyrn — a systems language with the expressiveness of TypeScript","description":"Types carry the rules that make a value valid. Ownership is a word you write on a parameter. No garbage collector, three backends, one result.","url":"https://vyrn-lang.github.io/vyrn/","applicationCategory":"DeveloperApplication"}</script>
```

### `/guide/getting-started.html`

```html
<title>Getting started — the Vyrn guide</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="Install the compiler, write one file, run it. Vyrn needs no project file and no build step for a single program.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/guide/getting-started.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="Getting started">
<meta property="og:description" content="Install the compiler, write one file, run it. Vyrn needs no project file and no build step for a single program.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/guide/getting-started.html">
<meta property="article:section" content="Docs">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="Getting started">
<meta name="twitter:description" content="Install the compiler, write one file, run it. Vyrn needs no project file and no build step for a single program.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"Getting started","description":"Install the compiler, write one file, run it. Vyrn needs no project file and no build step for a single program.","url":"https://vyrn-lang.github.io/vyrn/guide/getting-started.html","inLanguage":"en"}</script>
```

### `/guide/ownership.html`

```html
<title>Ownership, consume, regions — the Vyrn guide</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="You write what a function does with a value. The compiler enforces it and frees the memory.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/guide/ownership.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="Ownership, consume, regions">
<meta property="og:description" content="You write what a function does with a value. The compiler enforces it and frees the memory.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/guide/ownership.html">
<meta property="article:section" content="Docs">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="Ownership, consume, regions">
<meta name="twitter:description" content="You write what a function does with a value. The compiler enforces it and frees the memory.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"Ownership, consume, regions","description":"You write what a function does with a value. The compiler enforces it and frees the memory.","url":"https://vyrn-lang.github.io/vyrn/guide/ownership.html","inLanguage":"en"}</script>
```

### `/web/components.html`

```html
<title>Components: .vyx — Vyrn docs</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="A .vyx file is one component: a script block of ordinary Vyrn and a template block of markup.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/web/components.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="Components: .vyx">
<meta property="og:description" content="A .vyx file is one component: a script block of ordinary Vyrn and a template block of markup.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/web/components.html">
<meta property="article:section" content="Web">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="Components: .vyx">
<meta name="twitter:description" content="A .vyx file is one component: a script block of ordinary Vyrn and a template block of markup.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"Components: .vyx","description":"A .vyx file is one component: a script block of ordinary Vyrn and a template block of markup.","url":"https://vyrn-lang.github.io/vyrn/web/components.html","inLanguage":"en"}</script>
```

### `/tooling/commands.html`

```html
<title>The CLI — Vyrn docs</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="One binary. vyrn run needs nothing else on your machine; vyrn build writes a native binary; every other task — tests, benchmarks, formatting, docs…">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/tooling/commands.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="The CLI">
<meta property="og:description" content="One binary. vyrn run needs nothing else on your machine; vyrn build writes a native binary; every other task — tests, benchmarks, formatting, docs…">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/tooling/commands.html">
<meta property="article:section" content="Tooling">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="The CLI">
<meta name="twitter:description" content="One binary. vyrn run needs nothing else on your machine; vyrn build writes a native binary; every other task — tests, benchmarks, formatting, docs…">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"The CLI","description":"One binary. vyrn run needs nothing else on your machine; vyrn build writes a native binary; every other task — tests, benchmarks, formatting, docs…","url":"https://vyrn-lang.github.io/vyrn/tooling/commands.html","inLanguage":"en"}</script>
```

### `/docs.html`

```html
<title>Reference — Vyrn</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="Every standard library module and every name it exports, with the signature, the tests and the example programs for each.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/docs.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="website">
<meta property="og:title" content="Reference">
<meta property="og:description" content="Every standard library module and every name it exports, with the signature, the tests and the example programs for each.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/docs.html">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="Reference">
<meta name="twitter:description" content="Every standard library module and every name it exports, with the signature, the tests and the example programs for each.">
```

### `/docs/std/json.html`

```html
<title>std/json — Vyrn</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="std/json (RFC-0059) — the shared JSON value tree and its canonical writer, written in plain Vyrn on bytes/stringFromBytes. 8 exports.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/docs/std/json.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="std/json">
<meta property="og:description" content="std/json (RFC-0059) — the shared JSON value tree and its canonical writer, written in plain Vyrn on bytes/stringFromBytes. 8 exports.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/docs/std/json.html">
<meta property="article:section" content="Reference">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="std/json">
<meta name="twitter:description" content="std/json (RFC-0059) — the shared JSON value tree and its canonical writer, written in plain Vyrn on bytes/stringFromBytes. 8 exports.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"std/json","description":"std/json (RFC-0059) — the shared JSON value tree and its canonical writer, written in plain Vyrn on bytes/stringFromBytes. 8 exports.","url":"https://vyrn-lang.github.io/vyrn/docs/std/json.html","inLanguage":"en"}</script>
```

### `/docs/std/storage.html`

```html
<title>std/storage — Vyrn</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="std/storage — crash-safe persistence (RFC-0044). 1 export.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/docs/std/storage.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="std/storage">
<meta property="og:description" content="std/storage — crash-safe persistence (RFC-0044). 1 export.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/docs/std/storage.html">
<meta property="article:section" content="Reference">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="std/storage">
<meta name="twitter:description" content="std/storage — crash-safe persistence (RFC-0044). 1 export.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"std/storage","description":"std/storage — crash-safe persistence (RFC-0044). 1 export.","url":"https://vyrn-lang.github.io/vyrn/docs/std/storage.html","inLanguage":"en"}</script>
```

### `/docs/graph.html`

```html
<title>The import graph — Vyrn</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="Every import between the standard library's modules, drawn from the import lines themselves.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/docs/graph.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="The import graph">
<meta property="og:description" content="Every import between the standard library's modules, drawn from the import lines themselves.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/docs/graph.html">
<meta property="article:section" content="Reference">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="The import graph">
<meta name="twitter:description" content="Every import between the standard library's modules, drawn from the import lines themselves.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"The import graph","description":"Every import between the standard library's modules, drawn from the import lines themselves.","url":"https://vyrn-lang.github.io/vyrn/docs/graph.html","inLanguage":"en"}</script>
```

### `/explore/shelf.html`

```html
<title>examples/shelf — the Vyrn registry</title>
<meta name="theme-color" media="(prefers-color-scheme: light)" content="oklch(0.93 0.024 60)">
<meta name="theme-color" media="(prefers-color-scheme: dark)" content="oklch(0.155 0.005 60)">
<meta name="description" content="examples/shelf — The shelf wire types: the validated records and scalars that cross the RPC boundary, in their own leaf module.">
<link rel="canonical" href="https://vyrn-lang.github.io/vyrn/explore/shelf.html">
<meta property="og:site_name" content="Vyrn">
<meta property="og:type" content="article">
<meta property="og:title" content="examples/shelf">
<meta property="og:description" content="examples/shelf — The shelf wire types: the validated records and scalars that cross the RPC boundary, in their own leaf module.">
<meta property="og:url" content="https://vyrn-lang.github.io/vyrn/explore/shelf.html">
<meta property="article:section" content="Explore">
<meta name="twitter:card" content="summary">
<meta name="twitter:title" content="examples/shelf">
<meta name="twitter:description" content="examples/shelf — The shelf wire types: the validated records and scalars that cross the RPC boundary, in their own leaf module.">
<script type="application/ld+json">{"@context":"https://schema.org","@type":"TechArticle","headline":"examples/shelf","description":"examples/shelf — The shelf wire types: the validated records and scalars that cross the RPC boundary, in their own leaf module.","url":"https://vyrn-lang.github.io/vyrn/explore/shelf.html","inLanguage":"en"}</script>
```

## The image question

**RECOMMENDATION, NOT A DECISION.**

The site publishes one graphic, `editor/vscode/icons/vyrn.svg`, copied to
`favicon.svg`. No crawler renders an SVG as a card image, so this branch emits
no `og:image` and the card is a `summary` card with no picture. A card with a
broken image is worse than a card without one.

What a per-route image would need, in the order the work has to happen:

**1. A way to write bytes.** `writeFile` takes two `String` arguments —
`compiler/vyrn-frontend/src/checker.rs:6543`, which rejects anything else with
"`writeFile` needs String arguments". A String is UTF-8, and a PNG is not, so
this export cannot write one at all. `readFileBytes` exists
(`checker.rs:6607`); there is no write counterpart. Step zero is a compiler
change, not a site change. `site/export.vyrn:830` already records the same wall:
`site/public/hero.wasm` is copied by the deploy job because the export cannot
write it.

**2. A PNG writer.** Signature, `IHDR`, `IDAT`, `IEND`, a CRC-32 per chunk and
an Adler-32 over the zlib stream. `std/hash` has FNV and SplitMix64 and neither
of those; both are about twenty lines each. Stored (uncompressed) deflate blocks
would avoid a Huffman coder, but 1200x630 at three bytes a pixel is 2.3 MB per
image and 170 MB for 74 routes, so a real deflate is needed. Estimate: 150-250
lines for the PNG container, 300-500 more for LZ77 plus fixed Huffman.

**3. Text on the image.** A card is mostly type, and there is no font
rasteriser in this repository. Either a hand-built bitmap font — one weight, no
kerning, an atlas to check in — or a TrueType outline parser with an
anti-aliased scanline fill, which is 800 lines and a font file. This is the
expensive half, and it is the half a reader actually looks at.

So: roughly 500-900 lines of new Vyrn for the file format, plus a font, plus a
builtin that does not exist. The alternative is to rasterise an SVG in the
deploy job with `resvg` or `sharp`, which is one build-time dependency —
RFC-0106 says the site takes none, and `site/test/feed.test.mjs` carries a
twenty-line XML parser rather than break that rule.

**The cheap option, which is not this job's to take.** One shared card image,
1200x630, made once by hand and checked into `site/public/`. Then `og:image`,
`og:image:width`, `og:image:height` and `og:image:alt` on every route, four
lines in `withCards`, and every link gets a picture. The site has no such file
and this branch cannot make one. `site/test/metadata.test.mjs` already carries
the rule for the day one arrives: any `og:image` must be absolute, must name a
file that exists in `out/`, and must come with a size and alternative text.

## What is left undone

**The home page title is 63 characters.** A search result cuts around 60. It is
`Vyrn — a systems language with the expressiveness of TypeScript`, which is the
sentence the site leads with, and shortening it is an editorial decision. The
tests hold it at 63 so it cannot grow: `site/test/metadata.test.mjs` and the
`a title fits a search result` block in `site/export.vyrn`. Every other route is
under 60.

**The six redirect stubs still share the language paragraph.** They are the old
names of `/why-vyrn`, `/benchmarks`, `/tooling/editors` (twice) and two moved
chapters. Two of them point at the same page, so they could not have distinct
descriptions anyway. They carry a refresh and a canonical to their replacement
and no card, and the tests exclude them by that refresh.

**No `article:published_time` or `article:modified_time`.** The site has no
per-page date. `site/data/history.json` dates releases, not pages, and the
export reads no git.

## What could not be verified in a browser

The browser pane opens a `file://` page as a static snapshot with no scripting,
so `javascript_tool` answered "No site is open in this tab" on three attempts
and the head could not be read out of a live DOM. One fact it did show: the tab
was titled `std/json — Vyrn`, so the head parses and the title is the module's.
Everything else here was checked by reading the built files in `out/` — which is
what `site/test/metadata.test.mjs` does on every run, and is the stronger check
of the two.

## Gates

Run on the final tree in `N:/wt-meta`:

```
compiler/target/release/vyrn.exe run site/export.vyrn out     exported 80 route(s) and 14 asset(s)
compiler/target/release/vyrn.exe fmt --check site/app/*.vyrn site/guide/*.vyrn site/export.vyrn   clean
node --test "site/test/*.test.mjs"                            50 pass, 0 fail
compiler/target/release/vyrn.exe test site/export.vyrn        34 passed, 0 failed
compiler/target/release/vyrn.exe test <each of site/app/*.vyrn site/guide/*.vyrn>   55 modules, 0 failed
```

Ten of the 50 node tests are new, in `site/test/metadata.test.mjs`: every route
emits every tag, no two share a description, no two share an `og:title`, every
canonical is absolute and names its own route, titles and descriptions fit,
an `og:image` must be published if one is ever declared, both theme colours are
there, the JSON-LD parses and agrees with the card, and a documentation page
names its shelf.
