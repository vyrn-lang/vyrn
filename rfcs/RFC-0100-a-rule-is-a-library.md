# RFC-0100 — A Rule Is a Library

- **Status:** **Implemented.** `std/hints` (policy), `std/vyx-hints` (eleven rules
  for `.vyx`), and one export on `std/vyx` that makes its template parse a public
  seam. Two of this RFC's claims died to measurement and are recorded below: the
  configuration facility could not live where the manifest lives, and the corpus
  it was written for is cleaner than the audit predicted.
- **Depends on:** RFC-0099 (`//@diag` — a generator may report a diagnostic, and
  `std/diag`), RFC-0021 (generator imports, the comptime sandbox, the gen cache),
  RFC-0033 (origin maps — `path:line:col` and its resolution), RFC-0026 / RFC-0039
  (the `.vyx` component compiler and its template AST).
- **Research:** `docs/research/vyx-hints.md` — the audit of the vyx web stack
  against `nuxt/hints`, `html-validate`, Lighthouse, axe-core and OWASP. Its §8
  ranked ten checks to build first. This RFC builds the ones a template's own
  text decides, and says why each of the rest is not here.
- **Principle:** the compiler learns nothing. A rule is an ordinary library, a
  rule set is an ordinary import, and the project's policy over both is an
  ordinary JSON object. A third party writes their own with the same three
  imports and no permission.

---

## The question

RFC-0099 gave a generator a voice: `report(severity, file, line, col, message)`
becomes a `//@diag` line in the generated text, and the loader lifts it out as a
diagnostic anchored in the file the generator read. Its M1 shipped the primitive
and one non-web proof. Its M2 was named and deferred: the checking library.

The temptation at this point is obvious and wrong. The `.vyx` compiler already
parses every template, already knows every tag, every attribute and every
attribute's line and column; adding an `img has no alt` check inside
`vyxProcessElemInner` is nine lines. Do that ten times and the project has a
lint framework nobody asked for, welded to one compiler, with a rule list that
can only be changed by editing `std/vyx.vyrn` — and a project that disagrees with
one rule has no move except to stop using the framework.

The user's requirement was stated as a constraint on the mechanism, not on the
rules: *it must not be hardcoded, and it must be usable by third-party
developers for other languages.* So the question this RFC answers is not "which
checks should vyx run" but "what does a checking library need that it does not
already have, and how little of it can live in the compiler".

The answer is **none of it**. What was missing was two pieces of ordinary Vyrn
and one `export` keyword.

## The mechanism

Three parts, none privileged:

1. **`std/diag`** (RFC-0099, already shipped) — how a generator SAYS something.
   A severity, an anchor, a message. The compiler's entire contribution.
2. **`std/hints`** (new) — how a rule is *governed*: the project's per-code
   severity policy, and the author's per-line waiver. Knows nothing about `.vyx`,
   HTML, the web, or Vyrn. A rule is a `code`, an opaque string; an input file is
   text with lines in it.
3. **A rule library** — `std/vyx-hints` here, `their-hints` for anybody else. A
   `gen fn` that reads files, decides what is worth saying, and says it.

The whole of a third-party hint library is:

```vyrn
import { reportHere, Severity } from "std/diag"
import { hint, noPolicy, policyOf, Policy } from "std/hints"

export gen fn sqlHints(path: String, config: String) -> String {
    let src = match readFile(path) { Ok(t) => t, Err(e) => "" }
    // ... its own rules, its own codes, its own severities ...
    return hint(pol, "sql/select-star", Warning, src, path, line, 1,
        "`SELECT *` ships every column, including the ones added later")
}
```

```vyrn
import { sqlHints } from "./sql_hints"
import * as _sql from sqlHints("./schema.sql", "./vyrn.json")
```

That library is in the test suite (`compiler/vyrn-cli/tests/hints.rs`), it checks
a file format that is neither Vyrn nor `.vyx`, it imports nothing `std/vyx-hints`
imports beyond those two modules, and it gets the same configuration and the same
waivers. It needed no change to the compiler, no change to `std/`, and no
registration anywhere. That test is the acceptance evidence for this RFC's
central claim, and it would fail if any part of the mechanism were
`vyx-hints`-shaped.

### What `std/hints` is

```vyrn
export type Policy = { codes: Array<String>, levels: Array<String> }

export fn noPolicy() -> Policy
export fn policyOf(configText: String, key: String) -> Result<Policy, String>
export fn levelOf(p: Policy, code: String, dflt: Severity) -> String
export fn hint(p: Policy, code: String, dflt: Severity, src: String,
               file: String, line: Int64, col: Int64, message: String) -> String
export fn waived(src: String, line: Int64, code: String) -> Bool
```

`hint` is the only one a rule usually calls. It returns the `//@diag` line, or
`""` when the project turned the code off or the author waived it — so a rule
reads as one expression and the policy is not a thing each rule remembers to
consult.

### Where the configuration lives, and the claim that died

The obvious home is `vyrn.json`, and the obvious implementation is "the library
finds the manifest". The second half is impossible, and the reason is a property
of the sandbox worth stating rather than working around.

A generator may read **only under the constant path arguments it was given**
(`gen_scoped_path`, `interp.rs`). It cannot walk up looking for a manifest; it
cannot read a file the importer did not name. That is not an obstacle to route
around — it is what makes a generator's inputs describable, which is what makes
the gen cache correct and the wasm engine's mediation identical to the
interpreter's.

So the config path is an **argument**:

```vyrn
import * as _hints from vyxHintsConfigured("./app/widgets", "./vyrn.json")
```

`vyrn.json` is still the home — the manifest ignores keys it does not know, so
this adds a key and no file — but it is the home because the project *pointed*
the library at it, not because the library went looking. Which is the better
outcome: a third-party library gets the identical facility with its own key
(`"sqlHints"` beside `"hints"`), two hint libraries in one project configure
independently, and a project that would rather keep its lint policy in
`hints.json` writes that path instead. Nothing here is manifest-shaped.

```json
{ "hints": { "perf/img-size": "off", "a11y/img-alt": "error" } }
```

Three levels: `off`, `warning`, `error`. A code the project never mentions runs
at the severity its author chose.

**A broken config is a refusal.** `policyOf` returns `Err` for a document that
does not parse, a key that is not an object of strings, or a level word nobody
defined; `std/vyx-hints` turns that `Err` into an `Error` report and checks
nothing. This follows the discipline `find_manifest` was just hardened with: an
unreadable policy is not the empty policy. A hint library that fell back to its
defaults would tell a project its policy was in force while it was not, and the
project would read a green build as "no problems" over rules it had turned up.

### Waivers

`vyrn-ignore <code>` on the reported line or the line above it drops that one
report. The marker is plain text, so it rides whatever comment the input file's
own language spells — `<!-- … -->` in a template, `-- …` in SQL, `// …` in Vyrn —
and `std/hints` needs to know none of them.

```text
<!-- vyrn-ignore sec/raw-html: rendered from the repo's own markdown -->
<p v-html="body"></p>
```

This is the consumer RFC-0099 §"Codes and fixes are not in v1" said would come.
It does **not** promote the code to a directive field, because the code is read
by the library that emitted it, inside the input file, before the directive is
ever written — the compiler still never sees a rule name. RFC-0099 M3 remains
unspent.

### The one export

`std/vyx` gained `vyxParseTemplate(source) -> VyxTemplate` and `export` on the
three AST types it returns (`VyxNode`, `VyxAttr`, `VyxBody`). Parse only: no
props, no imports, no component resolution, no sibling `v-if` grouping.

This is the seam a rule library reads, and it is public. `std/vyx-hints` holds no
access to it that `their-vyx-hints` does not, which is the difference between a
library and a plug-in slot. The alternative — re-implementing an HTML scanner
inside the hint library — was refused for the reason `std/scan` exists: a second
scanner is a second set of bugs, and a rule anchored by a different parser than
the compiler's would eventually point at a column the compiler disagrees with.

`VNElem` carries a line but no column, so an element-shaped report anchors at its
first attribute's line and column and falls back to the element's line. Adding a
column to `VNElem` is a change to the compiler's own AST for a lint's benefit;
the first attribute is on the same tag and is already exact.

## The rules

Eleven, in `std/vyx-hints`. Each has a fixture that fires it and a near-miss
fixture that must not — the near miss is the half that catches an over-eager
rule, and precision is the whole design constraint: **a rule that fires when it
is not sure is worse than no rule**.

| Code | Default | Fires when |
| --- | --- | --- |
| `a11y/img-alt` | warning | `<img>`, `<area>` or `<input type="image">` with no `alt` in any spelling |
| `a11y/control-name` | warning | `<button>`, `<a>` or a heading whose children are empty or whitespace, with no `aria-label`/`aria-labelledby`/`title` |
| `a11y/input-label` | warning | a control with no `id`, no `aria-label`, no `title` and no `<label>` ancestor |
| `a11y/click-target` | warning | `@click` on a non-interactive tag with no `role` and no `tabindex` |
| `a11y/tabindex-positive` | warning | a static `tabindex` above zero |
| `a11y/dup-id` | warning | a static `id` inside a `v-for` body |
| `a11y/button-type` | warning | a `<button>` inside a `<form>` with an event binding and no `type` |
| `perf/img-size` | warning | `<img>` without both `width` and `height` |
| `sec/raw-html` | warning | `v-html` |
| `sec/unsafe-url` | **error** | a static `href`/`src`/`action`/… whose value is a `javascript:` URL |
| `sec/inline-handler` | **error** | an `on*` attribute in a template |

Two are errors because the output is broken rather than improvable: both defeat a
content-security policy, and the template already has a correct spelling for each
(`@click`, a real URL). Everything else is advice and rides a build that
succeeded. A project moves any of them in either direction.

Three of these are certainties rather than heuristics, and they are the argument
for doing this statically:

- **`a11y/dup-id`** — a loop body renders once per row, so a static `id` inside a
  `v-for` is duplicated *by construction*. No HTML validator can know this,
  because it sees the rendered page and cannot tell which of two ids came from a
  loop.
- **`a11y/click-target`** — axe finds this in a browser. The template knows the
  tag and the handler at compile time, at the column, in the editor.
- **`a11y/input-label`** — a control with no `id` cannot be reached by any
  `<label for>` anywhere in the document. So if it also has no `aria-label` and no
  `<label>` ancestor, it has no accessible name, and this is not a guess.

### What is not here, and why

From the research's own ranking and table:

| Not built | Why |
| --- | --- |
| LCP is lazy / not preloaded / lacks `fetchpriority` (P2, P4, P5) | which element is the LCP is a runtime fact. The static half of each is guesswork about a page the library cannot see. |
| image is not `webp`/`avif` (P3) | a PNG logo is correct. The rule cannot tell a photo from a diagram, so it would fire on both. |
| LCP / CLS / INP thresholds (P6–P8) | measurement. Belongs in a dev-mode `PerformanceObserver`, not in a compiler. |
| render-blocking `<script>`, SRI, `crossorigin`, `preconnect` (P9, P15, S6, S7) | not checks. The `Head` record cannot *express* `defer` or `integrity`, so the fix is an API in `std/ui`, and a hint about a hole the framework itself emits is a hint the author cannot act on. |
| unused imported component (P11) | needs a whole-program view; one generator sees one directory. |
| heading level order (A10) | a component call hides its root element, so a level sequence across components is unknowable from one file. It would fire on correct pages. |
| unknown `aria-*`, abstract `role` (A13, A14) | needs an ARIA table. Real, and mechanical, and nobody's blocker — a later rule, or somebody else's library. |
| content model, `<div>` in `<p>` (A33) | needs the full HTML content-model table. The largest single item in the research and the one that buys correctness rather than accessibility. |
| `<html lang>`, viewport meta (A20, A21) | not a component-level fact, and not a check: `document()` should emit both. A fix in `std/html`, filed by the research, unchanged by this RFC. |
| focus on soft navigation, `aria-live` route announcement, reduced motion (A24–A29) | runtime behaviour in `web/vyrn-nav.js`. Nothing static to say. |
| raw attribute names, dropped void children (S2, A31) | defects in `std/html`, not advice. RFC-0099 said they belong to neither milestone; they still do. |
| colour contrast (A22) | **the one gap worth naming.** See below. |

### The gap this found: contrast needs a seam `std/tw` does not have

The research called `a11y/contrast` "the row that makes the case for the whole
approach", and it is right: `bg-*` and `text-*` both resolve to a theme hex value
at compile time, and vyx sees both classes on one element, so a ratio axe needs a
browser for is a compile-time constant here.

It is not built, and the reason is a missing seam rather than a missing rule.
`std/tw` flattens the theme JSON to `bg-<token>` / `text-<token>` with the hex
known, and exports **none of it** — no token-to-hex map, no palette accessor. A
contrast rule would have to re-parse the theme JSON with its own copy of `tw`'s
flattening rules, and the day the two disagree the rule reports a ratio for a
colour the page does not use.

The honest fix is one export on `std/tw` (`colorOf(theme, token) -> String`, or
the flattened table), and then contrast is an ordinary rule in this library or in
somebody else's. That is a separate change to a separate module with its own
tests, and inventing a private copy of the palette to make a demo work is exactly
what this RFC's premise forbids. Recorded here so the next person does not
rediscover it: **the mechanism needed nothing; one rule needs one export from
`std/tw`.**

## Dogfooding, and the second claim that died

The rules were run over every `.vyx` file in the repository (20) and over the
`website-v1` branch (13 more — routes, the guide, the explorer, the playground),
which is the largest real `.vyx` corpus that exists.

| Corpus | Files | Reports | Judgement |
| --- | --- | --- | --- |
| `examples/vyxcomp` | 3 | 1 × `sec/raw-html`, 1 × `a11y/input-label` | both true |
| `examples/bin`, `examples/fullstack`, `examples/shelf` | 17 | 0 | the corpus is right |
| `website-v1` `site/app/routes` | 13 | 25 × `sec/raw-html` | all true sinks, none a bug |

The audit predicted a rich harvest of accessibility faults. It found almost none,
and the reason is visible in the source: the forms wrap their controls in
`<label>`, the playground's textareas carry `aria-label`, the explorer's input
has a `<label for>`, its diagram carries `role="img"` and a real description, and
the live region is already `aria-live="polite"`. Those are the near-miss fixtures
in the wild, and every one of them correctly said nothing. `examples/vyxcomp/Row.vyx`
has a bare `<input class="q" @input="setQty(item.name)"/>` — a real unlabelled
control, found — and `Panel.vyx` uses `v-html`.

The 25 `sec/raw-html` reports on the website are the interesting number. Every one
is a true positive in the only sense the rule claims: that is an unescaped-markup
sink. None is a bug — they render the repository's own markdown. This is the rule
the waiver mechanism exists for, and a documentation site that has decided its
markdown pipeline is trusted will turn it off in `vyrn.json` in one line, or waive
each site and keep the count as its audit. The default stays `warning`, because
the alternative — a sink nobody can see — is how the stack got here.

## What this is not

It is not a lint framework. There is no rule registry, no rule interface, no
plug-in loader, no `vyrn lint` command and no `--rule` flag. A rule set is a
module; running it is an import; configuring it is a JSON object the project
hands to a generator. Every one of those existed before this RFC.

It is also not a `.vyx` feature. `std/vyx-hints` is one client. The mechanism was
proved on a `.sql` file by a library outside `std`, and would work identically for
a generator that reads protobuf, a Makefile, or a language the compiler has never
heard of.

## Alternatives refused

- **Checks inside `std/vyx`.** The whole point. It makes the rule list an
  attribute of the compiler, unremovable, unconfigurable, and impossible for a
  third party to extend without a pull request.
- **A `hints` key the library discovers by walking up to `vyrn.json`.** The
  sandbox forbids it, and forbidding it turns out to be the better design — see
  above. A discovered config is also invisible in the cache key story; a config
  passed as an argument is a recorded input, so editing it misses the gen cache
  by the same rule as editing any other input.
- **A per-rule `Severity` argument instead of a `Policy`.** It moves the policy
  into the call site of every rule, which means the project cannot change it at
  all — the call sites are inside the library.
- **A suppression directive the COMPILER understands.** It would be the compiler
  learning rule names, which is the thing RFC-0099 refused. The waiver lives in
  the input file and is read by the library that owns the rule.
- **Re-implementing the template parser inside the hint library.** Two parsers,
  two answers, and a squiggle that eventually lands on the wrong column. One
  `export` is cheaper than a second scanner and honest about what a rule reads.
- **Shipping every rule the research ranked.** A rule that fires on ambiguity
  trains its reader to ignore the whole library. Eleven that are right beat
  thirty that are usually right.
