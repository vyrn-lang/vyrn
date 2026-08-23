# UI census, source 4: nuxt/hints rules

Source: <https://raw.githubusercontent.com/nuxt/hints/refs/heads/main/README.md> (read in full, 2026-08-23).

The README does not ship a numbered rule list. It names five features and two
example console warnings. This census takes every distinct check the README
describes and judges each one for compile-time checkability against Vyrn's
existing hint checker.

Baseline: `std/vyx-hints.vyrn` parses a `.vyx` file with `vyxParseTemplate` and
walks the node tree (`std/vyx-hints.vyrn:185-194`). An element node carries its
tag, whether it is a component call, its attributes, its children, and its line
(`std/vyx-hints.vyrn:222-223`). Its header states what it deliberately does not
check (`std/vyx-hints.vyrn:35-49`).

## Rules

| rule | what it forbids | why | when it can be checked | Vyrn today |
|---|---|---|---|---|
| Hydration mismatch detection (`hydration`) | Any difference between the server-rendered DOM and the client-hydrated DOM. The module hooks Vue's hydration and diffs both trees (README, "Hydration Mismatch Detection"). | A mismatch is by definition a disagreement between two renderings of the same template. No single text contains both sides. | RUN TIME — needs the server HTML and the client DOM after hydration. Neither exists at compile time. | missing — needs a rendered page; `std/vyx-hints.vyrn:39-43` excludes rules that need one. |
| LCP element must not have `loading="lazy"` (`webVitals`, example warning) | Putting `loading="lazy"` on whatever element the browser measured as the Largest Contentful Paint (README, "Example Console Output", first block). | Which element is the LCP comes out of a runtime measurement. The same attribute on a small image is fine. | RUN TIME — needs the identity of the measured LCP element. A `.vyx` file carries the literal attribute text, so a checker could flag every `loading="lazy"`, but that would fire on elements that are not the LCP and would not be this rule. | missing — needs the rendered page to name the LCP element; `std/vyx-hints.vyrn:39-43`. |
| INP tracking (`webVitals`) | Slow interaction-to-next-paint latency, attributed to an element (README, "Web Vitals Analysis"). | Interaction latency only exists while a user interacts with the running page. | RUN TIME — needs real interactions and paint timing. Nothing in `.vyx` text predicts it. | missing — needs a live page; `std/vyx-hints.vyrn:39-43`. |
| CLS tracking (`webVitals`) | Cumulative layout shift above threshold, attributed to elements (README, "Web Vitals Analysis"). | Layout shift is a measurement over the loaded page. | RUN TIME — needs rendered geometry over time. | missing — needs a rendered page; `std/vyx-hints.vyrn:39-43`. Adjacent compile-time help exists: `perf/img-size` flags an `<img>` without `width`/`height`, the common self-inflicted shift cause (`std/vyx-hints.vyrn:410-421`). |
| Third-party script performance audit (`thirdPartyScripts`) | Render-blocking or slow third-party scripts, measured per script (README, "Third-Party Script Analysis"). | Load time and render-blocking status are properties of network fetches during page load. | RUN TIME — needs actual request timing against parsing progress. Template text cannot produce it. | missing — needs runtime network observation; `std/vyx-hints.vyrn:39-43`. |
| Third-party script missing `crossorigin` (`thirdPartyScripts`, example warning) | A third-party script loaded without a `crossorigin` attribute (README, "Example Console Output", second block). | For scripts written literally in a template, the compiler needs the tag's attribute list — a static `<script src>` carries it, and attrs are in the parsed tree. Nuxt Hints instead observes every script the page actually loads, including ones other code injects, which no template shows. | EITHER — literal `<script>` tags check at COMPILE TIME from the parsed attrs; injected scripts need RUN TIME observation. | missing — the tree has everything needed for the static half, but no rule reads script tags; the URL-attribute helper only serves the `javascript:` scheme check (`std/vyx-hints.vyrn:545-549`). |
| Unused imported component detection (`lazyLoad`) | Statically importing a component that never renders during SSR or initial hydration; fix is `Lazy` prefix or `defineAsyncComponent` (README, "Unused Component Detection"). | Two halves. "Never referenced anywhere" is a whole-program question the compiler can settle. "Never rendered during SSR" is a fact about an execution. | EITHER — the reference half checks at COMPILE TIME across all templates: every call site is a component node in some `.vyx` tree, so the compiler has the full call graph. The render half needs RUN TIME. | missing — the checker walks one file and skips component nodes without inspecting them (`std/vyx-hints.vyrn:284-289`); no cross-file import analysis exists. |
| Server-rendered HTML validation (`htmlValidate`) | Markup defects `html-validate` finds in the server-rendered response (README, "HTML Validate integration"), such as invalid nesting or duplicate ids. | Most `html-validate` checks read markup structure alone. The integration runs after rendering because Nuxt ships templates as rendered HTML. | EITHER — the underlying checks need only the markup text, and a `.vyx` file carries it in the parsed tree, so they check at COMPILE TIME; catching output produced dynamically needs RUN TIME. | missing — no structural-well-formedness rules beyond what the parser itself rejects; `vhCheck` returns nothing for a template that fails to parse (`std/vyx-hints.vyrn:186-192`). Separate coverage lands through the html-validate census source. |

## Tallies

Rules: 8.

By check-time verdict:

| verdict | count |
|---|---|
| COMPILE TIME | 0 |
| RUN TIME | 5 |
| EITHER | 3 |

Coverage in `std/vyx-hints.vyrn`: covered 0, missing 8. No README-named check
exists in the checker. Three rows have adjacent compile-time work already in
place or reachable: CLS via `perf/img-size`
(`std/vyx-hints.vyrn:410-421`), static `crossorigin` from the existing
attribute walk (`std/vyx-hints.vyrn:295`), and unused components from the
component nodes the walker already visits but skips
(`std/vyx-hints.vyrn:284-289`). This matches the checker's own scope
statement: it keeps the rules a template's own text decides
(`std/vyx-hints.vyrn:35-43`).
