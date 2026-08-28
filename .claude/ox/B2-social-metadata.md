# B2 — Every URL shows the right thing when it is shared

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`. Work on a branch named `ox/social-metadata`. This
job edits `site/`. Do not run it at the same time as B1 or B3.

## Objective

Every route the site publishes must carry metadata that makes a link to it show
correct, specific, and readable information when pasted into a chat, a social
site, or a search result. Today it does not.

## The decision already made, which this job implements

**Text metadata first. One shared image.** Per-route generated images are a
separate question and this job does not attempt them, because Vyrn has no image
encoder and the platforms that matter do not accept SVG. Record the image
question in the report and leave it to the owner.

**Everything comes from route data, not from a literal in a template.** The site
already knows a route's title, its section, and its content. The metadata must
be derived from that. A hand-written `og:description` per page is exactly the
hardcoding the owner banned. See `.claude/ox/RULES.md`.

## What every route must emit

| tag | value |
| --- | --- |
| `<title>` | the route title, then a separator, then `Vyrn`. Under 60 characters total. Truncate the route title, never the site name. |
| `<meta name="description">` | the route's own first sentence, trimmed to 155 characters at a word boundary. Never the same string on two routes. |
| `<link rel="canonical">` | the absolute URL of this route |
| `og:type` | `website` for the home page and section indexes, `article` for a documentation or guide page |
| `og:title` | the route title alone, without the site name |
| `og:description` | the same string as `description` |
| `og:url` | the same as canonical |
| `og:site_name` | `Vyrn` |
| `og:image` | the absolute URL of the shared image |
| `og:image:width`, `og:image:height` | the real pixel size of that image |
| `og:image:alt` | a real description of the image, not the page title |
| `twitter:card` | `summary_large_image` |
| `twitter:title`, `twitter:description`, `twitter:image` | the same values as their `og` counterparts |
| `<meta name="theme-color">` | one value per colour scheme, using `media` |

Documentation and guide pages also emit JSON-LD of type `TechArticle`, and the
home page emits `SoftwareApplication`. Both built from the same route data. No
second source of truth.

## The description problem, which is the real work

A description must be specific and must not repeat another page. Derive it in
this order:

1. The first sentence of the page's own content, if that sentence stands alone.
2. If the first sentence needs its heading to make sense, join the heading and
   the sentence.
3. If the page has no prose, describe what the page lists, using the count.
   `The 38 modules in the Vyrn standard library, with every export.` A count
   read from the data, never typed.

Then verify: no two routes may share a description. Assert it in a test. That
test is the point of this job, because a duplicated description is exactly the
failure the owner is complaining about, and only a test keeps it fixed.

## Tests you must add

Add these to the site's own tests, in the same style as the existing ones. Read
`site/export.vyrn` for how the current route assertions are written.

1. Every route emits every required tag. No empty values.
2. No two routes share a `description`.
3. No two routes share an `og:title`.
4. Every `canonical` is absolute and matches the route path.
5. Every `title` is 60 characters or fewer.
6. Every `description` is between 50 and 155 characters.
7. `og:image` is absolute and the file exists in `out/`.

A test that fails today is correct. Fix the data, not the test.

## The shared image

If `site/public/` has no suitable image, make one as SVG for the site itself and
report that a raster version is needed for sharing. Do not fake it. Do not claim
a PNG exists if it does not. Record in the report exactly what is missing.

## Gates

```
cd compiler && cargo build --release -p vyrn-cli
cd ../ && compiler/target/release/vyrn run site/export.vyrn out
compiler/target/release/vyrn fmt --check site/app/*.vyrn site/guide/*.vyrn site/export.vyrn
```

Plus the site test steps from `.github/workflows/site.yml`.

Then validate the output. Pick ten routes across different sections, and for
each, extract the `<head>` from the built file in `out/` and paste the tag list
into the report. Ten real examples beat any claim.

## The report

Write `rfcs/census/social-metadata.md`: what each route now emits, the ten
examples, the duplicate descriptions found and fixed, and a section
`The image question` stating plainly what per-route images would need: a PNG
encoder, where it would live, and roughly how much code. Mark it
`RECOMMENDATION, NOT A DECISION`.

## What this job must not do

- Do not write a per-route description by hand.
- Do not add an image encoder.
- Do not change page content. That is B1.
- Open one pull request from `ox/social-metadata`, titled
  `site: metadata for every route`. No hard-wrapping in the body. No AI
  attribution.
