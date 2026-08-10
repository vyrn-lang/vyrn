# Web quality rules and the vyx UI layer

Research note. Not an RFC. It measures the Vyrn web stack against the rules
`nuxt/hints` ships, against the canonical sources that module draws from, and
against what the Vyrn generators actually do today.

Every claim about Vyrn below comes from the source, with a file and a line.
The repo history says an audit that trusts docs over code gets it wrong, so
the docs were read second.

Sources read:

- `nuxt/hints` at `main`: `src/module.ts`, `src/features.ts`,
  `src/runtime/web-vitals/plugin.client.ts`,
  `src/runtime/third-party-scripts/plugin.client.ts` and its Nitro plugin,
  `src/runtime/html-validate/nitro.plugin.ts`, `src/runtime/lazy-load/*`,
  `src/runtime/hydration/*`, `README.md`.
- `html-validate` at `master`: `src/config/presets/{standard,document,browser,recommended,a11y}.ts`
  and the rule summaries in `docs/rules/`.
- In-repo: `std/vyx.vyrn`, `std/html.vyrn`, `std/ui.vyrn`, `std/tw.vyrn`,
  `web/vyrn-dom.js`, `web/vyrn-nav.js`, `compiler/vyrn-frontend/src/diagnostics.rs`,
  `compiler/vyrn-frontend/src/origin.rs`, RFC-0026, RFC-0036, RFC-0039,
  RFC-0067, RFC-0068, RFC-0069.

---

## 1. What `nuxt/hints` is

It is a Nuxt module that runs only in dev (`src/module.ts`, `if (!nuxt.options.dev) return`).
It has five features. Four observe the running page. One is static.

| Feature | Class | How it detects |
| --- | --- | --- |
| `webVitals` | performance | runtime. The `web-vitals` library with attribution. |
| `thirdPartyScripts` | perf + security | runtime. `document.scripts` plus a `MutationObserver`. |
| `lazyLoad` | performance | build-time Vite scan, then runtime render tracking. |
| `hydration` | correctness | runtime. Hooks Vue hydration, diffs the two DOMs. |
| `htmlValidate` | a11y + correctness | static, but on the rendered output. A Nitro `render:response` hook runs `html-validate` over the response body. |

The module holds almost no rules of its own. Its own rule set is 12 checks in
two plugin files. The rest of its power comes from `html-validate`.

### 1.1 The `webVitals` rules

From `src/runtime/web-vitals/plugin.client.ts`. All fire only when the metric
rating is not `good`. All are runtime observations. Each needs a real LCP or
CLS attribution, so none can run without a browser.

| Hint | Trigger |
| --- | --- |
| LCP element has `loading="lazy"` | LCP entry element is an `img` with `loading=lazy`. |
| LCP image is not a next-gen format | the `src` path has an image extension but no `webp` or `avif`. |
| LCP image lacks `fetchpriority="high"` | `element.fetchPriority !== "high"`. |
| image has no `width`/`height` | attribute absent on the LCP element, and again on any CLS source element. |
| LCP took too long | `performanceEntry.startTime > 2500`. |
| LCP image is not preloaded | no `<link>` in `head` with that `href`. |
| CLS is too big | a layout-shift entry with `value > 0.1`. |
| INP is poor | any INP metric whose rating is not `good`. |

Note the shape. Six of the eight are static properties of the markup
(`loading`, format, `fetchpriority`, `width`, `height`, a preload link). Only
the identity of the LCP element and the two time thresholds need the browser.

### 1.2 The `thirdPartyScripts` rules

From `src/runtime/third-party-scripts/plugin.client.ts` and its Nitro plugin.

| Hint | Class | Trigger |
| --- | --- | --- |
| third-party script is missing `crossorigin` | security | `!script.crossOrigin` on any cross-origin script. |
| third-party scripts are present | performance | any cross-origin `src` at mount. Advice: use `@nuxt/scripts`. |
| script timing report | performance | request, download, total network, parse plus execute, per script. |

The README also promises render-blocking detection and a security-attribute
dashboard. That work happens in the DevTools client, not in a rule.

### 1.3 The `lazyLoad` rule

One rule. A Vite plugin records every statically imported `.vue` component. The
runtime records which of them rendered during SSR and first hydration. Anything
imported and never rendered is reported: prefix it with `Lazy`, or use
`defineAsyncComponent`. It is a bundle-size rule, not a paint rule.

### 1.4 The `hydration` rule

One rule. Vue's hydration hook reports a mismatch; the module captures the
pre- and post-hydration HTML and diffs it. `normalizeHTMLForComparison`
normalizes the `style` attribute first, to kill false positives.

---

## 2. The `html-validate` rules the module turns on

`src/runtime/html-validate/nitro.plugin.ts` extends `html-validate:standard`,
`html-validate:document` and `html-validate:browser`, then overrides.

The effective set matters, because two of the presets subtract.

**Enabled and a11y-relevant:** `element-required-attributes` (this is the one
that requires `img` `alt`), `area-alt`, `aria-label-misuse`, `no-abstract-role`,
`multiple-labeled-controls`, `input-missing-label`, `heading-level`, `valid-for`,
`valid-autocomplete`, `no-missing-references`, `no-dup-id`, `no-multiple-main`,
`valid-id`.

**Enabled and correctness-relevant:** `element-permitted-content`,
`element-permitted-parent`, `element-permitted-order`,
`element-permitted-occurrences`, `element-required-ancestor`,
`element-required-content`, `close-order`, `close-attr`, `no-dup-attr`,
`attribute-allowed-values`, `attribute-misuse`, `void-content`, `deprecated`,
`no-deprecated-attr`, `doctype-html`, `element-name`, `no-unknown-elements`
(added by the module), `no-raw-characters`, `unrecognized-char-ref`,
`script-element`, `map-dup-name`, `map-id-name`.

**Turned off by the module or by the `browser` preset:** `require-sri`
(off — "conflicts with Nuxt defaults"), `missing-doctype`, `no-inline-style`,
`svg-focusable`, `void-style`, `no-trailing-whitespace`, `attribute-boolean-style`,
`attribute-empty-style`, `doctype-style`, `no-self-closing`.

**Never enabled, because the module extends `standard` and not `recommended`
or `a11y`:** all seven `wcag/*` rules (`h30` link text, `h32` submit buttons,
`h36` and `h37` and `h67` image alt text, `h63` table scope, `h71` fieldset
legends), `empty-heading`, `empty-title`, `hidden-focusable`, `meta-refresh`,
`no-autoplay`, `no-implicit-button-type`, `no-implicit-input-type`,
`prefer-native-element`, `prefer-button`, `unique-landmark`, `text-content`,
`autocomplete-password`, `form-dup-name`, `input-attributes`, `long-title`,
`no-redundant-role`, `require-csp-nonce`, `allowed-links`.

So `nuxt/hints` ships a smaller a11y set than most readers assume. Vyrn should
draw from the `recommended` and `a11y` presets, not from what this module
happens to enable.

---

## 3. What the wider sources add

Only classes the module misses.

**Lighthouse.** Render-blocking resources. Missing `<meta name="viewport">`.
Missing `<html lang>`. Document title quality. Text compression. Font display.
Preconnect to required origins. Unsized images (same as above). Tap targets
below 48 px.

**axe-core.** Two rule classes matter here and appear nowhere in `nuxt/hints`:

- **Colour contrast.** `color-contrast` compares computed foreground and
  background. axe needs the browser for this. Vyrn does not — see §6, row A22.
- **Keyboard operability.** A click handler on a non-interactive element with
  no `role` and no `tabindex` is unreachable by keyboard. `html-validate`
  cannot see handlers; axe can, at runtime. vyx sees them at compile time.

axe also adds `aria-*` attribute validity, `aria-required-children`,
`landmark-one-main`, `page-has-heading-one`, `region` (content outside a
landmark), `duplicate-id-active`, `frame-title`, `html-has-lang`,
`meta-viewport` with `user-scalable=no`, and `tabindex` above zero.

**OWASP.** Four classes:

- **XSS sinks.** Any path from data to markup. In this stack: `Raw`, `v-html`,
  `innerHTML`, an attribute name built from data, an event name built from data.
- **Unsafe URLs.** `javascript:` and untrusted `data:` in `href`, `src`,
  `action`, `formaction`, `srcdoc`, `poster`, `xlink:href`.
- **`target="_blank"`.** `rel="noopener noreferrer"`. Modern browsers imply
  `noopener`, so the live part of the rule is referrer leakage.
- **CSP.** A `Content-Security-Policy` header, nonce discipline, and no inline
  script or style. Plus the neighbouring response headers:
  `X-Content-Type-Options`, `Referrer-Policy`, and cookie flags.

---

## 4. What the Vyrn stack does today

Facts, with lines. This section is the basis for every "status" below.

### 4.1 `std/html.vyrn` — the tree

- The model is one generic constructor. `el(tag, attrs, kids)` at line 164.
  There is no `img`, no `a`, no `button`, no `input` helper. Attributes are
  `A(String, String)` — free strings, line 48.
- `escapeText` (235) escapes `& < > "`. `escapeAttr` (256) escapes `&` and `"`
  only. Values are always double-quoted, so that is not a breakout.
- **Attribute names are concatenated raw.** `attrPair` line 299:
  `" " + n + "=\"" + escapeAttr(v) + "\""`. An attribute name from data injects
  markup. Same for the tag name in `renderEl` (333) and the event name in
  `onPair` (306).
- `Raw(String)` (line 65) bypasses escaping entirely: `Raw(s) => s.copy()` at
  line 347. It is exported as a variant, so any importer can build it.
- No URL scheme check anywhere. `attr("href", "javascript:alert(1)")` renders
  unchanged. `attr("onclick", ...)` renders a live handler.
- `isVoid` (277) covers the 13 void tags. `renderEl` **silently discards**
  children of a void element. `toHtmlString` is total by design (line 342) —
  it never traps, so it can never reject bad markup.
- `document(title, head, body)` (788) emits `<!doctype html>`, then `<html>`
  with **no `lang`**, `<meta charset="utf-8">`, and the escaped `<title>`. It
  emits **no viewport meta**. The signature has no slot for either.
- There are zero asserts, zero diagnostics and zero tests in the file.

### 4.2 `std/vyx.vyrn` — the template compiler

- The compiler parses the whole template into `VyxNode` and knows, per node:
  the tag name, whether it is a component, every attribute name, every static
  attribute value, whether a value is dynamic, the children, and the 1-based
  line and column of each attribute value. That is the analysis surface.
- `vyxEmitAttrs` (2009) maps `class` to `Cls`, `@event` to `On`, and
  **everything else to `A(name, value)` with no check on the name**. A static
  `onclick="..."` in a `.vyx` compiles to a live inline handler.
- `v-html` becomes `VNRaw` (834) and emits `Raw(expr)` (1837, 2367). No
  diagnostic, no marker, no warning.
- The existing diagnostics are 21 identifiers: `VYX_UNCLOSED_TAG`,
  `VYX_BAD_ATTR_VALUE`, `VYX_MISSING_FOR_KEY`, `VYX_DANGLING_ELSE`,
  `VYX_UNKNOWN_COMPONENT`, `VYX_MISSING_PROP`, `VYX_EVENT_ON_COMPONENT`,
  and so on. Every one is structural or type-related. **None is an a11y,
  performance or security rule.**
- Class attributes are the exception, and the precedent: when the build is
  themed, `vyxEmitAttrs` routes `class` through `vyxTheme.cls(...)` (2028), so
  a static class literal is proven a member of the generated `Tw` type at
  compile time, at the exact column.
- `head { ... }` entries compile in `vyxHeadItemExpr` (3211) to
  `el("script", [A("src", ...)], [])` — **no `defer`, no `async`, no
  `integrity`, no `crossorigin`**. Stylesheets get `rel` and `href` only.

### 4.3 `std/ui.vyrn` — pages and the shell

- `headHtml` (185) emits stylesheets, then modules, then classic scripts, then
  meta. The classic script has no `defer`. No asset carries SRI or
  `crossorigin`. The typed `Head` record (137) cannot express them.
- The default page title is **the URL pattern** (`uiSuccessResponse`, 1788), so
  a page without a `head` block ships `<title>/users/:id</title>`.
- `withMeta` (176) exists, so a viewport meta is reachable by hand. Nothing in
  the framework adds one.
- The hydration payload is a **separate HTTP response**, negotiated on `Accept`
  with `vary: "Accept"` (421-469). No JSON is inlined into any document. The
  `</script>` breakout class does not exist here. `uiPayload` (451) does no
  `<` or `U+2028` escaping, which is correct today and a trap the day anyone
  inlines it.
- Repo-wide grep for `aria-`, `role=`, `tabindex`, `activeElement`: **zero hits**
  in `std/*.vyrn`, `web/vyrn-dom.js`, `web/vyrn-nav.js`.

### 4.4 `web/vyrn-dom.js` and `web/vyrn-nav.js` — the runtime

- `innerHTML` is used at four sites, all for `Raw`: 121, 172, 317, 386. No
  sanitizer, no Trusted Types.
- `setAttribute` at 124, 159 and 356 has **no allowlist and no denylist**. An
  `onclick` attribute or a `javascript:` href in the tree reaches the DOM.
- Events bind by string. `invoke` (272) calls `exports[handler](String(arg))`
  with no allowlist over the wasm export table.
- Keyed diffing: duplicate keys collide silently in a `Map` (194, 408). An
  unkeyed child inside a keyed list is rebuilt from scratch on every render
  (197, 411), which destroys focus and caret in that node.
- **Focus is never managed.** No `activeElement` capture, no `.focus()`, no
  `tabindex="-1"` target — in either file. Soft navigation replaces `<main>`
  (`vyrn-nav.js` 244, 310, 389) and focus resets to `<body>` with no
  announcement. Scroll, by contrast, is handled well: manual restoration with
  a five-step re-apply schedule (327-363).
- `document.title` updates on every soft-nav path (208, 384, 443, 498, 568).
  That is the one a11y-adjacent feature that exists.
- `executableImport` (198-204) re-creates fetched `<script>` elements so they
  execute, copying every attribute including `nonce`.

### 4.5 `std/tw.vyrn` — the theme

- Class names are validated against `[a-z][a-z0-9-]*` (`twClassSafe`, 86), and
  a single gate (`twSheetSafetyErrors`, 775) blocks the whole sheet on any
  violation. The old breakpoint-forging hole is closed, with a test at 1045.
- Colours flatten from the theme JSON to `bg-<token>`, `text-<token>` and
  `border-<token>` with the **hex value known at compile time** (438-446).
- Variants are `sm:`, `md:`, `hover:` and `focus:`. There is no `dark:`.

### 4.6 The diagnostic machinery

- `Severity::Warning` exists (`diagnostics.rs` 20, 85). A warning rides a load
  that succeeded and never changes an exit code. The LSP already maps it to
  `DiagnosticSeverity::WARNING` (`vyrn-lsp/src/main.rs` 3597).
- `//@origin path:line:col` directives (`origin.rs`) already relocate a
  diagnostic from the synthesized module onto the real `.vyx` at the real
  column. vyx already emits them per attribute (`vyxRegion`, 1622).
- **But a generator cannot emit a warning.** `vyxErr` (518) synthesizes an
  identifier such as `VYX_MISSING_FOR_KEY__file__line_12`, which fails to
  resolve and produces a hard error. There is no warning channel from a
  generator. Every hint below needs one. See §7.

---

## 5. Reading the table

- **Applies?** Whether the rule has meaning for a `.vyx` page or a `std/html`
  tree.
- **Status.** `enforced` — the stack rejects or prevents it today.
  `possible` — the information exists at the stated point; nobody looks.
  `hole` — the information does not exist there, or the API cannot express
  the fix.
- **Point.** `vyx` — a compile-time diagnostic in `std/vyx.vyrn`, anchored by
  `//@origin` to the `.vyx` line and column. `html` — a type or a constructor
  in `std/html.vyrn`. `ui` / `tw` — the generator. `dom` — a dev-mode check in
  `web/vyrn-dom.js`. `runtime` — needs a real browser.
- **Cost.** S: under a day, one function. M: one to three days, needs a small
  table or a new API. L: needs a content-model table, a cross-component pass,
  or a new mechanism.

---

## 6. The audit

### Performance

| # | Hint | Class | Applies | Status | Point | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| P1 | `img` has no `width`/`height` | perf | yes | hole | vyx | S |
| P2 | LCP image has `loading="lazy"` | perf | yes, weakened | possible | vyx (first `img` in the page template) | M |
| P3 | image is not `webp`/`avif` | perf | yes, when `src` is a literal | possible | vyx | S |
| P4 | LCP image lacks `fetchpriority="high"` | perf | yes, weakened | possible | vyx + `ui` head | M |
| P5 | LCP image is not preloaded | perf | yes | hole — `Head` has no `preload` slot | ui | M |
| P6 | LCP over 2500 ms | perf | yes | hole | runtime (`dom`, dev mode) | M |
| P7 | CLS over 0.1 | perf | yes | hole | runtime | M |
| P8 | INP is poor | perf | yes | hole | runtime | M |
| P9 | render-blocking classic `<script>` | perf | yes | **hole in the generator itself** — `vyxHeadItemExpr` 3211 and `ui.vyrn` 194 emit no `defer` | ui + vyx | S |
| P10 | third-party script present | perf | yes | possible — head entries are literals at compile time | vyx + ui | S |
| P11 | unused imported component | perf | reshaped | possible — vyx knows every component and every call site; an uncalled component is dead code in one wasm bundle | vyx | S |
| P12 | no code splitting per route | perf | yes | hole, out of scope — one wasm bundle by design | — | L |
| P13 | unkeyed child in a keyed list rebuilds every render | perf | yes, Vyrn-specific | possible — `vyx` requires `:key` on `v-for` already; the hole is a hand-built `keyed` mix | dom (dev) | S |
| P14 | duplicate `:key` in a `v-for` | perf | yes, Vyrn-specific | hole — silent `Map` collision, `vyrn-dom.js` 194 | dom (dev) | S |
| P15 | no `preconnect` to third-party origins | perf | yes | hole | ui | M |
| P16 | hydration mismatch | correctness | **no** | n/a — RFC-0069 does not hydrate the first page; the client renders from navigation on | — | — |

### Accessibility

| # | Hint | Class | Applies | Status | Point | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| A1 | `img` without `alt` (`wcag/h37`, `element-required-attributes`) | a11y | yes | hole | vyx | S |
| A2 | `alt=""` with a `title` (`wcag/h67`) | a11y | yes | hole | vyx | S |
| A3 | `area` without `alt` | a11y | yes | hole | vyx | S |
| A4 | click handler on a non-interactive element | a11y | yes | hole — vyx sees `@click` and the tag; nothing checks | vyx | S |
| A5 | `input` without a label | a11y | yes, within one template | possible — vyx knows nesting, so a wrapping `<label>` is provable | vyx | M |
| A6 | `for` points at nothing (`valid-for`, `no-missing-references`) | a11y | yes, within one template | possible | vyx | M |
| A7 | duplicate `id` (`no-dup-id`) | a11y | yes | hole. A static `id` inside a `v-for` body is a duplicate **by construction** — provable with no cross-file work | vyx | S |
| A8 | `button` without `type` | a11y | yes | hole | vyx | S |
| A9 | empty heading, empty link, empty button (`empty-heading`, `wcag/h30`, `text-content`) | a11y | yes, when the content is literal | possible | vyx | S |
| A10 | heading level skips (`heading-level`) | a11y | yes, weakened | possible per template; a component call hides its root | vyx | L |
| A11 | `tabindex` above zero | a11y | yes | hole | vyx | S |
| A12 | `aria-hidden` on a focusable element (`hidden-focusable`) | a11y | yes | hole | vyx | S |
| A13 | unknown `aria-*` name or bad value | a11y | yes | hole — needs an ARIA table | vyx | M |
| A14 | abstract or redundant `role` | a11y | yes | hole — needs the same table | vyx | M |
| A15 | `meta refresh` with a delay | a11y | yes | hole | vyx + ui | S |
| A16 | `autoplay` on media | a11y | yes | hole | vyx | S |
| A17 | more than one `<main>`, non-unique landmark | a11y | yes, per page after layout composition | possible | ui | M |
| A18 | bad `autocomplete` value, password field without `autocomplete` | a11y | yes | hole | vyx | M |
| A19 | duplicate form control `name` | a11y | yes | hole | vyx | S |
| A20 | `<html>` has no `lang` | a11y | yes | **hole, and not expressible** — `document()` has no slot, `html.vyrn` 788 | html + ui | S |
| A21 | no viewport meta, or `user-scalable=no` | a11y | yes | hole by default; reachable by hand through `withMeta` | ui | S |
| A22 | insufficient colour contrast | a11y | yes | **possible, and unique** — `bg-*` and `text-*` resolve to theme hex values at compile time (`tw.vyrn` 438-446), so the ratio is computable without a browser | tw + vyx | M |
| A23 | title is the URL pattern | a11y | yes, Vyrn-specific | hole — `ui.vyrn` 1788 | ui | S |
| A24 | focus lost on soft navigation | a11y | yes, Vyrn-specific | **hole. Zero focus code in the whole stack** | nav | M |
| A25 | route change is not announced | a11y | yes | hole — no `aria-live` region anywhere | nav + ui | S |
| A26 | focus lost when the differ replaces a node | a11y | yes | hole — `vyrn-dom.js` 378, 416, 474, 663 | dom | M |
| A27 | `prefers-reduced-motion` ignored | a11y | yes | hole — the progress bar hardcodes transitions, `vyrn-nav.js` 677 | nav + tw | S |
| A28 | progress bar has no `role`/`aria-hidden` | a11y | yes | hole — `vyrn-nav.js` 663 | nav | S |
| A29 | global `Keydown` sub fires while typing | a11y | yes, Vyrn-specific | hole — `vyrn-dom.js` 594, no modifier or focus check | dom | S |
| A30 | unknown element name | correctness | yes | hole — an unknown lowercase tag compiles silently | vyx | S |
| A31 | children passed to a void element | correctness | yes | hole — `std/html` drops them silently, `renderEl` 332 | vyx | S |
| A32 | duplicate attribute on one tag | correctness | yes | hole — `vyxParseAttrs` 532 does not check | vyx | S |
| A33 | content model violated (`<div>` in `<p>`, `<li>` outside a list) | correctness | yes | hole — needs a content-model table | vyx | L |
| A34 | table header without `scope` (`wcag/h63`), fieldset without legend (`wcag/h71`) | a11y | yes | hole | vyx | M |

### Security

| # | Hint | Class | Applies | Status | Point | Cost |
| --- | --- | --- | --- | --- | --- | --- |
| S1 | XSS through raw markup (`v-html`, `Raw`) | sec | yes | **hole. No diagnostic, no marker.** Server: `html.vyrn` 347. Client: `innerHTML` at `vyrn-dom.js` 121, 172, 317, 386 | vyx + html | S |
| S2 | attribute name from data injects markup | sec | yes | **hole, and a real defect, not a hint.** `attrPair` `html.vyrn` 299, `onPair` 306, `renderEl` 333 concatenate the name raw | html | S |
| S3 | inline `on*` handler attribute | sec | yes | hole — `vyxEmitAttrs` passes `onclick` through as `A("onclick", ...)`; `vyrn-dom.js` 356 sets it | vyx + dom | S |
| S4 | `javascript:` or untrusted `data:` URL | sec | yes | hole — no scheme check anywhere. Static values are provable at compile time; dynamic ones need a validated `Href` type (the RFC-0020 `RoutePath` precedent) | vyx + html | M |
| S5 | `target="_blank"` without `rel` | sec | yes | hole | vyx | S |
| S6 | no SRI on a cross-origin asset (`require-sri`) | sec | yes | **hole, and not expressible** — `withScript`/`withStylesheet` take a bare string, `ui.vyrn` 155-172 | ui | M |
| S7 | no `crossorigin` on a third-party script | sec | yes | same hole as S6 | ui | S |
| S8 | no CSP header, no nonce | sec | yes | hole — nothing sets a security header; `Response` has a `headers` map. **The stack is unusually ready for it**: the framework emits no inline script and no inline style, and events travel as data attributes, so `script-src 'self'` works with no nonce | ui + http | M |
| S9 | `X-Content-Type-Options`, `Referrer-Policy` | sec | yes | hole | ui + http | S |
| S10 | cookie flags: `HttpOnly`, `SameSite`, `Secure` | sec | yes, outside vyx | hole — nothing in `std/http` sets them | http | M |
| S11 | payload JSON inlined into HTML | sec | **no today** | enforced by construction — the payload is a separate negotiated response, `ui.vyrn` 421-469. `uiPayload` 451 does no `<`/`U+2028` escaping, so this becomes live the day anyone inlines it | ui | — |
| S12 | fetched inline scripts re-executed on soft nav | sec | yes, Vyrn-specific | hole — `executableImport` `vyrn-nav.js` 198-204 copies every attribute, `nonce` included, and runs the body | nav | M |
| S13 | handler name indexes the whole export table | sec | yes, Vyrn-specific | hole — `invoke` `vyrn-dom.js` 273 has no allowlist; prototype keys pass the `typeof` test | dom | S |
| S14 | inline `style` attribute | sec + perf | yes | possible — `tw` covers the styling need, so an inline `style` in a `.vyx` is both a `style-src` problem and a theme bypass | vyx | S |
| S15 | unvalidated redirect or open `href` from props | sec | yes | hole — overlaps S4 | vyx | M |

---

## 7. The mechanism gap, and the fix

Nothing in the table needs a language change. One piece of plumbing is missing.

A generator today reports a fault by synthesizing an identifier that does not
resolve (`vyxErr`, `std/vyx.vyrn` 518). The result is a hard error with a
machine-shaped message. Hints are advice. Advice must not fail a build.

Both halves of the answer already exist:

- `Severity::Warning` rides a successful load and never touches an exit code
  (`diagnostics.rs` 20). The LSP already publishes it as a warning
  (`vyrn-lsp/src/main.rs` 3597).
- `//@origin file:line:col` already relocates a generated diagnostic onto the
  real `.vyx` at the real column (`origin.rs`), and vyx already emits one per
  attribute (`vyxRegion`, 1622).

So add one directive, parsed beside `//@origin` in the same file:

```text
//@origin ./widgets/BookRow.vyx:14:9
//@hint a11y/img-alt img has no `alt` attribute; add `alt=""` if it is decorative
let vyxN_a0: Attr = A("src", cover)
```

The loader turns each `//@hint` into a `Diagnostic::warning` at the governing
origin, with the identifier as a stable name for suppression. Cost: one parse
arm, one lookup, no change to any existing behaviour, no change to `emit-gen`
bytes beyond the comment. A generator that emits none behaves exactly as
before — the same promise `//@origin` already keeps.

Suppression should reuse the same comment channel in the `.vyx`
(`<!-- vyrn-ignore a11y/img-alt -->`), so the rule can be waived at the line
that earns the waiver.

Two rows in the table are **not** hints and should not wait for this. S2 (raw
attribute names) is an injection defect in `std/html`. A31 (silently dropped
void children) is silent data loss. Both belong in the library, as a
validation, not as advice.

---

## 8. The ten to build first

Ranked by value over cost. Each names the exact diagnostic and the exact place.

**1. `a11y/img-alt` — `img` without `alt`.**
`std/vyx.vyrn`, in `vyxProcessElemInner` after attribute parsing.
> `img has no `alt` attribute; write `alt=""` if the image is decorative`

The single highest-value a11y rule in every source. The information is already
in hand: the tag name and the attribute list, with a line and a column. Also
covers `area` (A3) and `input type="image"` (`wcag/h36`) with two more lines.
Cost S.

**2. `sec/raw-html` — `v-html` and `Raw`.**
`std/vyx.vyrn` at the `VNRaw` emit site (1837, 2367).
> `v-html writes unescaped markup from `sepMarkup()`; the value must never come from user input`

The only XSS sink the stack has, on both the server (`html.vyrn` 347) and the
client (four `innerHTML` sites). Today it is silent. Make it loud, name the
expression, and let `<!-- vyrn-ignore sec/raw-html -->` mark a reviewed one.
The count of waivers becomes the audit. Cost S.

**3. `sec/attr-name` — validate attribute, tag and event names.**
`std/html.vyrn` in `attrPair` (299), `renderEl` (333) and `onPair` (306).
> Not a diagnostic. A validation, at the boundary.

`attr("x\" onclick=alert(1) z", "1")` injects markup today. This is the one
row that is a defect. Reject any name outside `[A-Za-z][A-Za-z0-9:_.-]*` at
the render, or introduce a validated `AttrName` type on the RFC-0020
precedent. `toHtmlString` is documented total, so the rejection must be a
skipped attribute plus a hint, not a trap. Cost S.

**4. `a11y/click-target` — a handler on a non-interactive element.**
`std/vyx.vyrn`, in `vyxEmitAttrs` where `@event` bindings are seen.
> `@click on a `div` is not reachable by keyboard; use a `button`, or add `role` and `tabindex``

axe finds this at runtime. vyx knows the tag and the handler at compile time.
The check is a set membership test against `button a input select textarea
summary` plus "has a `role` or a `tabindex`". This is the clearest case where
the static pipeline beats a runtime hint system. Cost S.

**5. `sec/unsafe-url` — `javascript:` in a URL attribute.**
`std/vyx.vyrn` for static values; `std/html.vyrn` for the dynamic case.
> `href starts with `javascript:`; only http, https, mailto, tel and relative URLs are allowed`

Static literals prove immediately. Dynamic values need a validated `Href`
string type, which the `RoutePath` precedent already shows how to build. Ship
the static half first, the type second. Covers `href`, `src`, `action`,
`formaction`, `poster`, `srcdoc`. Cost S then M.

**6. `sec/inline-handler` — an `on*` attribute in a template.**
`std/vyx.vyrn`, in `vyxEmitAttrs` before the `A(name, value)` fallthrough.
> `onclick is an inline script handler; use @click, which dispatches through the export table`

It bypasses the event model, it breaks CSP, and today it compiles silently.
This one should be an **error**, not a hint: the template already has `@click`
for the job, so there is no legitimate use. Cost S.

**7. `a11y/document-lang` and `a11y/viewport` — the page shell.**
`std/html.vyrn` `document()` (788) and `std/ui.vyrn` `headHtml` (185).
> `<html> has no lang attribute` and `page has no viewport meta`

Not a check. A fix. Give `document()` a `lang` and emit
`<meta name="viewport" content="width=device-width, initial-scale=1">` by
default, with a `Head` slot to override. Two Lighthouse audits and one axe
rule close on a five-line change. Do it with a `lang` slot on `Head`, so the
i18n layer can drive it. Cost S.

**8. `perf/script-defer` and `sec/asset-integrity` — the head API.**
`std/ui.vyrn` `withScript`/`withStylesheet` (155-172), `headHtml` (185), and
`std/vyx.vyrn` `vyxHeadItemExpr` (3211).
> `script "/legacy.js" blocks the parser; add `defer`` and
> `cross-origin script has no `integrity`; add an SRI hash and `crossorigin="anonymous"``

The classic `<script src>` in `<head>` with no `defer` is a render-blocking
stop the framework itself emits. Widen the `Head` record and the `head { }`
block to carry `defer`, `integrity` and `crossorigin`, then default `defer` to
on. This turns a hole in the API into a hint that can be given. Cost S for
`defer`, M for SRI.

**9. `a11y/nav-focus` — focus and announcement on soft navigation.**
`web/vyrn-nav.js` around the `<main>` replacement (244, 310, 389), plus one
`aria-live` region emitted by `std/ui.vyrn`.
> Not a diagnostic. Behaviour.

After a soft nav, move focus to the new `<main>` with `tabindex="-1"`, and
write the new title into a polite live region. Scroll is already handled with
care in the same file; focus is not handled at all. Every SPA a11y guide names
this as the first thing to fix, and a screen reader user cannot tell that the
page changed. Cost M.

**10. `a11y/contrast` — colour contrast from the theme.**
`std/tw.vyrn` (the ratio table) and `std/vyx.vyrn` (the pairing).
> `text-slate-400 on bg-white has contrast 2.8:1; WCAG AA needs 4.5:1`

`bg-*` and `text-*` both resolve to a theme hex value at compile time
(`tw.vyrn` 438-446), and vyx sees both classes on the same element. So the
ratio is a compile-time constant. axe needs a rendered browser for this; the
Vyrn stack does not. It only covers the case where both classes sit on one
element, and inherited colour needs an ancestor walk within the template —
which vyx also has. This is the row that makes the case for the whole
approach. Cost M.

### What did not make the ten, and why

`no-dup-id` (A7) is nearly free and should ride along with rule 1 — a static
`id` inside a `v-for` body is a duplicate by construction, which is a
Vyrn-specific certainty no HTML linter can reach. Web Vitals (P6-P8) are
genuinely runtime and belong in a dev-mode `vyrn-dom.js` observer, after the
static rules land; a `PerformanceObserver` for LCP, CLS and INP is about
forty lines and needs no dependency. The content model (A33) is the largest
single item and buys correctness rather than a11y — defer it. CSP (S8) is
cheap to add and hard to get right; the stack's freedom from inline script
means `script-src 'self'` already works, so the work is a header API and a
default, not an audit.

### The shape of the finding

`nuxt/hints` observes a running page and reports what it sees. Of its 12 own
rules, 6 are static properties of markup that it can only reach at runtime,
because Vue templates become render functions and the markup exists only once
it is rendered.

vyx compiles templates statically, so it can see the same 6 without a browser,
plus the entire `html-validate` class, plus two axe classes that need a
browser everywhere else: keyboard operability and colour contrast. It sees
them with a line and a column, in the editor, before the page ever loads.

What it cannot see is measurement: how long the LCP took, which element was
the LCP, and how much the layout shifted. That is the correct division. Build
the static rules first, and keep the runtime observer small.
