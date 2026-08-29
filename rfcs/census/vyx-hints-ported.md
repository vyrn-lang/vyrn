# Ported: html-validate rules the `.vyx` checker now enforces

Seven rules from `rfcs/census/ui/README.md`, table one ("Already in hand — the
parsed tree decides. No new input."), now live in `std/vyx-hints.vyrn`. They run
at compile time, so a defect `html-validate` reports on a rendered page fails
the build instead.

Read date for every `html-validate` source below: 2026-08-24, branch `master`.

## Severity, and where it came from

`html-validate` sets a severity per preset, not per rule. The preset files hold
the words. Every rule ported here reads `"error"`, so every port is `Error`.

| Vyrn code | html-validate rule | severity | read at |
| --- | --- | --- | --- |
| `html/prefer-tbody` | `prefer-tbody` | `"error"` | `src/config/presets/recommended.ts:66` |
| `a11y/th-scope` | `wcag/h63` | `"error"` | `src/config/presets/recommended.ts:84`, `a11y.ts:32` |
| `html/input-type` | `no-implicit-input-type` | `"error"` | `src/config/presets/recommended.ts:52` |
| `sec/inline-style` | `no-inline-style` | `"error"` | `src/config/presets/recommended.ts:54` |
| `a11y/autoplay` | `no-autoplay` | `"error"` | `src/config/presets/recommended.ts:45`, `a11y.ts:17` |
| `html/void-content` | `void-content` | `"error"` | `src/config/presets/recommended.ts:78`, `standard.ts:40` |
| `sec/require-sri` | `require-sri` | `"error"` | `src/config/presets/document.ts:11` |

Read with `gh api repos/html-validate/html-validate/contents/src/config/presets/<name>.ts`.
Line numbers are lines of the decoded file.

One difference in kind, worth stating: an `html-validate` `"error"` raises
`errorCount`; a Vyrn `Error` stops the compile. The severity word is the same
and the consequence is heavier. A project that wants advice instead writes
`{"hints": {"html/prefer-tbody": "warning"}}` in its manifest.

## The rules

### `html/prefer-tbody` (Error)

Fires on a `<table>` with a `<tr>` as a direct child. The browser inserts the
`<tbody>` the markup left out, so the rendered tree is not the written one and
every `table > tr` selector in the sheet misses.

Does not fire on a `<tr>` inside `<thead>`, `<tbody>` or `<tfoot>` — that row is
a child of the section, not of the table, so it never reaches the check.
Reported once per table, at the table, so one waiver covers one table.

`std/vyx-hints.vyrn`, rule at the `tag == "table"` branch; predicate
`vhHasDirectRow`.

### `a11y/th-scope` (Error)

Fires on a `<th>` with no `scope`. A screen reader reads a data cell with the
header it belongs to; without `scope` it works that header out from the table's
shape, and it works it out wrong as soon as a table has a header column as well
as a header row.

A `:scope="…"` counts as present: `vhHas` reads any spelling. Does not fire on
`<td>`.

### `html/input-type` (Error)

Fires on an `<input>` with no `type`. Presence only, so `:type="kind"` counts.

### `sec/inline-style` (Error)

Fires on a static `style` attribute.

Deliberately does NOT fire on a bound `:style`. A `:style` carries a computed
value — a bar width, an SVG `viewBox` — that no class in a sheet can hold, so
the rule's own remedy does not reach it. `html-validate` has no binding to
narrow against; Nuxt Hints turns the whole rule off for the same reason, calling
it unreasonable for a Vue app (`rfcs/census/ui/html-validate-rules.md:101`).
Narrowing to the static half keeps the half where the remedy exists.

Three bound `:style` sites stand in the tree and are not reported:
`site/app/routes/benchmarks.vyx:62`, `site/app/routes/docs/graph.vyx:104`,
`site/app/routes/index.vyx:298`.

### `a11y/autoplay` (Error)

Fires on `autoplay` on `<audio>` or `<video>`, static or bound. `html-validate`
carries the same element list in the preset line itself
(`recommended.ts:45`, `include: ["audio", "video"]`). Does not fire on
`autoplay` on any other tag.

### `html/void-content` (Error)

Fires on a void element with children that are not whitespace. `.vyx` has no
void-element table of its own: it wants the end tag it was given, so
`<img>x</img>` parses here and renders as `<img>x` in a browser — the content
leaves the element it was written in.

Uses a NAMED list of the thirteen void tags, for the reason `vhIsHandlerAttr`
gives: a rule at error severity may miss and may not guess. `<br></br>` does not
fire — the children are empty.

### `sec/require-sri` (Error)

Fires on `<script src>` and `<link rel="stylesheet" href>` when the URL is
static, names another host (`//`, `http://`, `https://`), and there is no
`integrity`.

Narrower than `html-validate` in two ways, both recorded in the predicate's own
doc comment:

- `html-validate`'s `target` option defaults to `"all"`
  (`src/rules/require-sri.ts`, `const defaults`), so it asks for a hash of a
  same-origin file the build itself wrote a moment earlier. This port checks
  crossorigin only, the option `html-validate` calls `"crossorigin"` and
  implements with `/^(?:\w+:\/\/|\/\/)/`.
- `html-validate` also covers `rel="preload"` and `rel="modulepreload"`
  (`require-sri.ts`, `supportedRel`). This port covers `stylesheet`.

A `:src` is a value this library cannot see, so it is not reported.

## The eighth rule was dropped: `svg-focusable`

The brief and the census both record `svg-focusable` as forbidding the legacy
`focusable` attribute on `<svg>` (`rfcs/census/ui/README.md`, table one, and
`rfcs/census/ui/html-validate-rules.md:74`, "Legacy `focusable` handling").

That is backwards. The rule REQUIRES the attribute. Read at
<https://html-validate.org/rules/svg-focusable.html> on 2026-08-24: the
incorrect example is `<svg></svg>` and the error is `<svg> is missing required
"focusable" attribute`; the correct example is `<svg focusable="false"></svg>`.
It is an Internet Explorer tab-order workaround.

It is also `"off"` in every preset that names it —
`src/config/presets/recommended.ts:69` and `a11y.ts:24` both read
`"svg-focusable": "off"` — which the census records correctly in its overlap
column ("off in every preset").

A rule that no preset enables, that targets a dead browser, and that would fire
on every inline `<svg>` in the site is not worth a port. Dropped. Seven, not
eight.

## 2026-08-29: two rules from table TWO — the vocabulary lands

The census's second table ("Needs one fixed table beside the checker") begins
with the element vocabulary, and the table now sits beside the checker: 143
elements read from `html-validate` master, `src/elements/html5.ts`, on
2026-08-29 — 118 current and 25 carrying the ELEMENT-level `deprecated` flag —
grouped by first byte in `vhKnownTag`/`vhDeprecatedTag`.

| Vyrn code | html-validate rule | severity | why |
| --- | --- | --- | --- |
| `html/deprecated` | `deprecated` | `Error` | `"error"` in `src/config/presets/recommended.ts`; a removed element has no specified behavior |
| `html/element-name` | `element-name` + `no-unknown-elements` | `Warning` | deliberately quieter than upstream: the census's own caveat is that a table must be exact before a rule fires louder, and a vocabulary read at one date is the kind of table that grows |

Scope, in the walk's own terms: a component call is excluded as ever
(`isComp`); a tag with a `-` is a custom element, legal by definition; and the
children of `<svg>`/`<math>` are FOREIGN CONTENT with a vocabulary of their
own, gated by a new `inForeign` flag threaded exactly as `inFor`/`inLabel`/
`inForm` are. A removed element reports `html/deprecated` only — the sharper
sentence — never `html/element-name` as well.

Zero noise on the repository's own corpus: `vyxHints("site/app")` reports the
same 61 pre-existing findings before and after, none from the two new rules.

Same day, two more rows — the attribute vocabularies:

| Vyrn code | source | severity | why |
| --- | --- | --- | --- |
| `html/deprecated-attr` | `no-deprecated-attr` | `Error` | `"error"` in `src/config/presets/recommended.ts`; 189 exact (tag, attribute) pairs from the same `html5.ts` read, each an attribute-level `deprecated` flag — `align` is removed on `<td>` and was never on `<span>`, so the pair answers, not the name |
| `a11y/aria-name` | the closed WAI-ARIA set | `Warning` | 51 names read from `A11yance/aria-query` master, `src/ariaPropsMap.js`, 2026-08-29. Anything beginning `aria` outside the set is a typo by construction (`arial-label`, `aria-lable`), and a misspelled ARIA attribute is silently ignored — the worst failure mode an accessibility attribute can have. The deprecated `aria-dropeffect`/`aria-grabbed` stay KNOWN: a legal name is not a typo |

The `td scope` corner is its own test: removed on `<td>` by the pair table,
required on `<th>` by `a11y/th-scope` — exact on both sides.

And the waiver audit — `no-unused-disable`, `hint/unused-waiver` here, the
one rule that needs no table at all because its subject is the checker's own
markers. A `vyrn-ignore` that waives nothing usually means the finding it
apologized for was fixed, and the marker now misleads the reader about the
line below it. The mechanics live in `std/hints`, where the waivers do: the
policy grows a `strict` flag (`strictly(p)`) under which `hint` ignores
waivers, and a template that carries a marker — only such a template — is
walked a second time to learn what the rules WOULD have said; a marker is
used when a strict report of its code sits on its line or the one below,
the two lines a waiver covers. The audit reports through `hint` with the
caller's own policy, so it is configured and waived like any rule — a
marker may vouch for a marker, and the test pins that case.

And the entity rows closed WITHOUT the 2,231-name table they were filed
under, because the premise does not hold in this stack: `.vyx` text is
emitted ESCAPED (`std/html`: "Text(s) — ALWAYS escaped"), so NO character
reference decodes — `&nbsp;` in a template is seven literal bytes on the
page, every time. `html/entity-in-text` (Warning) therefore fires on any
entity-shaped sequence in a text node: the HTML habit meant it to decode,
which is a bug here, and a docs page showing markup on purpose sees exactly
what it wrote and waives the line. That one rule is `unrecognized-char-ref`
and `no-raw-characters`' compile-time half, resolved by the renderer's own
contract instead of a table. Anchored at the owning element — a text node
carries no position of its own.

Still open from table two, and now the only row: the full per-element
attribute tables (`no-unknown-attributes` — `html5.ts` lists constrained
attributes, not the complete allowed set, so that rule needs a source this
census has not read). `no-unused-disable` landed as `hint/unused-waiver`;
everything else in the table is either live above or recorded as vacuous
under `.vyx` (the parse-structure rules a strict template parser refuses
before any lint runs).

## Pages changed

Six files. Every change is a `scope="col"` on a header cell or a `<tbody>`
around rows. No visual change; no new CSS.

| file:line | rule | change |
| --- | --- | --- |
| `site/app/routes/benchmarks.vyx:128-130` | `a11y/th-scope` | `scope="col"` on the three `<thead>` headers |
| `site/app/routes/benchmarks.vyx:159-163` | `html/prefer-tbody` | `<tbody>` around the environment rows |
| `site/app/routes/docs/index.vyx:176` | `a11y/th-scope` | `scope="col"` on both headers |
| `site/app/routes/explore/[package].vyx:85` | `a11y/th-scope` | `scope="col"` on all three headers |
| `site/app/routes/releases.vyx:154` | `a11y/th-scope` | `scope="col"` on all four headers |
| `site/app/routes/tooling/editors.vyx:102` | `a11y/th-scope` | `scope="col"` on all three headers |

The `<th scope="row">` cells that were already correct
(`site/app/routes/benchmarks.vyx:135`, `:161`) were left alone.

Measured after the change, by running the checker over the whole tree:

```
import { vyxHints } from "std/vyx-hints"
import * as h from vyxHints("./site/app")
```

One report from the seven new rules remains, and it is the one below.

## Needs a decision

`site/app/routes/why-vyrn.vyx:122` — `sec/inline-style`.

```
<p class="lede" style="font-size:1.05rem">{{ c.means }}</p>
```

`.lede` sets `font-size: var(--t-lede)` (`site/public/style.css:1109`). The
inline declaration overrides it for the four capability panes only.

Two options, and both need a decision that is not the checker's:

1. Add a rule to `site/public/style.css` scoped to the pane, for example
   `.panes .lede { font-size: 1.05rem; }`. `.panes` is not unique to this page —
   `site/app/routes/index.vyx:412` carries a `.panes` block too — so this
   selector reaches markup nobody asked it to reach, and picking a selector that
   does not needs a class name somebody has to choose.
2. Drop the inline declaration and let `--t-lede` apply. That is a visual change
   to four panes.

Left as it stands. The rule was not weakened for it.

## Count against html-validate

Measured over `rfcs/census/ui/html-validate-rules.md`, counting table rows whose
`when it can be checked` column opens `COMPILE TIME` and whose `Vyrn today`
column does not open `absent`:

| measure | before | after |
| --- | --- | --- |
| rules in the census | 94 | 94 |
| rules that can be checked at compile time | 41 | 41 |
| compile-time rules with a Vyrn port | 4 | 11 |
| compile-time rules with no port | 37 | 30 |
| rules of all 94 with a Vyrn port | 8 | 15 |

The four that already had a port are `area-alt`, `no-implicit-button-type`,
`wcag/h36` and `wcag/h37`. The other four counterparts the census records —
`empty-heading`, `input-missing-label`, `wcag/h30`, `no-dup-id` — sit on
`EITHER` or `RUN TIME` rows and so are outside the 41.

Two corrections to the census, both measured:

- The README says "9 of 94 rules have a counterpart here". The count is 8. Rows
  with a `Vyrn today` column that does not open `absent`: `area-alt`,
  `empty-heading`, `input-missing-label`, `no-implicit-button-type`, `wcag/h30`,
  `wcag/h36`, `wcag/h37`, `no-dup-id`.
- The README says "None of the 41 compile-time-capable rules outside that set
  has a port", which reads as though none of the 41 had one. Four did.

## What is left of the 41

Thirty rules. They fall in three groups, unchanged from the census:

- Fourteen need the raw source spelling the parse tree drops (`attr-case`,
  `attr-quotes`, `no-dup-attr`, `no-self-closing` and the rest of table three).
- Eight need one fixed table beside the checker (`element-name`,
  `no-unknown-elements`, `no-unknown-attributes`, `unrecognized-char-ref`,
  `deprecated`, `no-deprecated-attr`, `no-raw-characters`, `no-unused-disable`).
- Seven sit in table one and are still unported: `close-order`,
  `script-element`, `no-implicit-close` (all three are parse structure, and a
  template that gets them wrong fails to parse already, so the compiler reports
  them with a better message than a lint has — `std/vyx-hints.vyrn:187-192`),
  `no-utf8-bom` (file bytes before parse, which `vhCheck` never sees),
  `element-required-attributes` (needs the per-element requirement tables that
  `vhNeedsAlt` hand-codes for one case), `require-csp-nonce` (a nonce is a
  per-request value, so a template can only ask for an attribute a build step
  will fill), and `svg-focusable`, dropped above.
- One, `no-style-tag`, reads `COMPILE TIME` but sits in none of the README's
  three tables. It is a tag comparison and would port in one line.

---

## The rules now run, which they did not before

`std/vyx-hints` had nineteen rules and one caller: its own test blocks. Nothing
in the repository ran it over the site, so the twelve older rules were as
unenforced as the seven new ones. Checked directly: removing a `scope="col"`
from `site/app/routes/benchmarks.vyx` and running the export still exported
eighty routes and exited 0.

`site/markup.vyrn` runs every rule over every `.vyx` file under `site/app`, and
`.github/workflows/site.yml` runs it as its own step. It reports the number of
files it checked — 61 — so a file that stops being discovered shows up as a
smaller number rather than as silence. Twenty-six warnings remain, all
`sec/raw-html`, which was a warning before this work and still is.

**Its own program, not an import in `site/export.vyrn`, and the reason is a
defect worth its own line.** `std/vyx-hints` reaches `std/hints`, which declares
`Policy`; `std/http` declares `Policy` too. A top-level name is program-wide, so
the two modules cannot be linked into one program:

```
`Policy` is declared by both std/hints.vyrn and std/http.vyrn — a top-level name
is program-wide, so two linked modules cannot share one
```

The export links `std/http`, so the rules cannot run from there until one of
those exported names changes. That is an API decision and it is not taken here.

## The site page that needed a decision, and what it needed instead

The report above left `site/app/routes/why-vyrn.vyx:122` alone —
`<p class="lede" style="font-size:1.05rem">` — with two options, both of which
changed how something looks.

There was a third. The declaration is given a class of its own, `.pane-lede`,
scoped under `.panes` so the home page's demo does not match it, and the size
names `--t-body` instead of the literal. That is 17px against 16.8px: a fifth of
a pixel.

The literal had to go anyway. Moving it into the stylesheet unchanged failed
`site/test/typescale.test.mjs`, which refuses a `font-size` that names no token
— so the inline attribute had been hiding an unnamed size from a test that
exists to find them.
