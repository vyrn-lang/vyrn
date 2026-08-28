# UI census rollup

Five sources, five files. Each one collects ground truth for a future Vyrn
component library in the shape of `reka-ui` and `nuxt-ui`, checked at compile
time instead of at run time. Read date for every external source: 2026-08-23.
This document records evidence. It decides nothing.

| source | file | size |
|---|---|---|
| reka-ui components | `reka-components.md` | 64 components, 13 contradictory-pair patterns |
| reka-ui defects | `reka-defects.md` | 81 issues read, 16 defect classes |
| nuxt-ui | `nuxt-ui.md` | 134 components, theming mechanism |
| nuxt/hints | `nuxt-hints-rules.md` | 8 checks |
| html-validate | `html-validate-rules.md` | 94 rules |

## Counts

| measure | reka-ui components | reka-ui defects | nuxt-ui | nuxt/hints | html-validate |
|---|---|---|---|---|---|
| items censused | 64 components | 81 issues | 134 components | 8 checks | 94 rules |
| classes or verdict groups | 13 pair patterns | 16 defect classes | ~41 wrap a reka-ui primitive, ~93 new or composed | 0 COMPILE TIME, 3 EITHER, 5 RUN TIME | 41 COMPILE TIME, 39 EITHER, 14 RUN TIME |

Notes on the numbers:

- Source 1 excludes 8 composables as non-components. 18 docs pages have an
  empty Accessibility section; their keyboard contracts are NOT VERIFIED
  (`reka-components.md`, "Verification gaps").
- Source 2 covers epic #2721 in full plus 80 open issues. 140 open issues were
  not read (`reka-defects.md`, "Method").
- Source 3 counts 131 documented components plus 3 that ship without a docs
  page (`DropdownMenuContent`, `ContextMenuContent`, `Toaster`)
  (`nuxt-ui.md`, "Components present in the repo but absent from the docs sitemap").
- The nuxt/hints README ships no numbered rule list. The census takes the 8
  distinct checks the README names (`nuxt-hints-rules.md`, preamble).
- Overlap between sources 4 and 5: nuxt/hints embeds html-validate itself and
  enables 41 of the 94 rules on rendered responses
  (`html-validate-rules.md`, "The overlap target").

## Rules a Vyrn compiler could enforce

Every rule from sources 4 and 5 whose row reads COMPILE TIME, with the
information the compiler needs. A `.vyx` file carries tags, attributes (name,
event flag, dynamic flag, literal value), children, control flow, and line
numbers after parse; it keeps no raw spelling (whitespace, quotes, comments,
end tags) (`html-validate-rules.md`, preamble, citing
`std/vyx-hints.vyrn:221-233`).

Source 4 contributes zero strict-COMPILE-TIME rows. Its four EITHER rows carry
a compile-time half: static `<script>` tags check for `crossorigin` from the
parsed attributes; unused-component detection over the whole-template call
graph; markup structure checks before render; CLS prevention through
`perf/img-size` (`nuxt-hints-rules.md`, rows 6-8 and 4).

The 41 COMPILE-TIME html-validate rules, grouped by the knowledge the compiler
needs:

**Already in hand — the parsed tree decides. No new input.**

| rule | what it forbids | compiler knowledge |
|---|---|---|
| [close-order](https://html-validate.org/rules/close-order.html) | end tags out of nesting order | nesting is the tree itself |
| [script-element](https://html-validate.org/rules/script-element.html) | unclosed `<script>` | parse structure |
| [no-implicit-close](https://html-validate.org/rules/no-implicit-close.html) | elements with optional end tags left unclosed | parse structure |
| [prefer-tbody](https://html-validate.org/rules/prefer-tbody.html) | `<tr>` directly inside `<table>` | child structure in the tree |
| [no-autoplay](https://html-validate.org/rules/no-autoplay.html) | `autoplay` on `<audio>`/`<video>` | attribute presence |
| [svg-focusable](https://html-validate.org/rules/svg-focusable.html) | legacy `focusable` on `<svg>` | attribute presence |
| [wcag/h63](https://html-validate.org/rules/wcag/h63.html) | `<th>` without `scope` | attribute presence |
| [area-alt](https://html-validate.org/rules/area-alt.html) | `<area>` without `alt` | attribute presence (`vhHas`) |
| [wcag/h36](https://html-validate.org/rules/wcag/h36.html) | image submit button without `alt` | attribute presence |
| [wcag/h37](https://html-validate.org/rules/wcag/h37.html) | `<img>` without `alt` | attribute presence |
| [no-implicit-input-type](https://html-validate.org/rules/no-implicit-input-type.html) | `<input>` without `type` | attribute presence |
| [no-implicit-button-type](https://html-validate.org/rules/no-implicit-button-type.html) | `<button>` without explicit `type` | attribute presence |
| [no-inline-style](https://html-validate.org/rules/no-inline-style.html) | a `style` attribute | attribute presence |
| [require-csp-nonce](https://html-validate.org/rules/require-csp-nonce.html) | script/style resources without a CSP nonce | attribute presence on listed elements |
| [require-sri](https://html-validate.org/rules/require-sri.html) | external scripts/styles without SRI integrity | attribute presence |
| [void-content](https://html-validate.org/rules/void-content.html) | content inside a void element | void-tag list plus children in tree |
| [no-utf8-bom](https://html-validate.org/rules/no-utf8-bom.html) | a UTF-8 byte-order mark | the file bytes before parse |
| [element-required-attributes](https://html-validate.org/rules/element-required-attributes.html) | a missing required attribute | per-element requirement tables, the shape `vhNeedsAlt` hand-codes (`std/vyx-hints.vyrn:505-510`) |

**Needs one fixed table beside the checker.**

| rule | what it forbids | compiler knowledge |
|---|---|---|
| [element-name](https://html-validate.org/rules/element-name.html) | element names outside the HTML vocabulary | WHATWG element table; the `isComp` flag separates component calls (`std/vyx-hints.vyrn:222`) |
| [no-unknown-elements](https://html-validate.org/rules/no-unknown-elements.html) | elements outside the vocabulary, catching typos like `<buton>` | same element table |
| [no-unknown-attributes](https://html-validate.org/rules/no-unknown-attributes.html) | attributes foreign to their element, catching typos like `arial-label` | WHATWG attribute-per-element tables |
| [unrecognized-char-ref](https://html-validate.org/rules/unrecognized-char-ref.html) | character references that name nothing | entity table |
| [deprecated](https://html-validate.org/rules/deprecated.html) | removed elements (`<center>`) | deprecation table |
| [no-deprecated-attr](https://html-validate.org/rules/no-deprecated-attr.html) | removed attributes | deprecation table |
| [no-raw-characters](https://html-validate.org/rules/no-raw-characters.html) | unescaped `<`, `>`, `&` in text | text nodes reach the tree as literals (`std/vyx-hints.vyrn:229`) |
| [no-unused-disable](https://html-validate.org/rules/no-unused-disable.html) | disable directives that suppress nothing | directives are source comments; the checker sees its own findings |

**Needs the raw source spelling, which the current parse tree drops.**

| rule | what it forbids | compiler knowledge |
|---|---|---|
| [attr-delimiter](https://html-validate.org/rules/attr-delimiter.html) | whitespace around `=` | raw token spelling |
| [attr-spacing](https://html-validate.org/rules/attr-spacing.html) | adjacent attributes with no whitespace | raw token spelling |
| [close-attr](https://html-validate.org/rules/close-attr.html) | attributes on an end tag | end-tag spelling |
| [no-dup-attr](https://html-validate.org/rules/no-dup-attr.html) | the same attribute twice on one element | duplicate tokens in source text |
| [attr-case](https://html-validate.org/rules/attr-case.html) | uppercase attribute names | literal names |
| [attr-quotes](https://html-validate.org/rules/attr-quotes.html) | unquoted or single-quoted values | quote characters |
| [attribute-boolean-style](https://html-validate.org/rules/attribute-boolean-style.html) | boolean attributes spelled against house style | raw spelling |
| [attribute-empty-style](https://html-validate.org/rules/attribute-empty-style.html) | empty attributes spelled against house style | raw spelling |
| [element-case](https://html-validate.org/rules/element-case.html) | uppercase element names | tag spelling |
| [no-self-closing](https://html-validate.org/rules/no-self-closing.html) | `/` on a non-void start tag | slash spelling; `.vyx` self-closing voids are language norm, so scope the rule |
| [void-style](https://html-validate.org/rules/void-style.html) | void elements closed against house style | slash spelling; irrelevant under `.vyx` norms |
| [no-trailing-whitespace](https://html-validate.org/rules/no-trailing-whitespace.html) | trailing blanks on lines | line endings; formatter territory |
| [no-conditional-comment](https://html-validate.org/rules/no-conditional-comment.html) | IE conditional comments | comments are source text; tree has no comment node |
| [attr-pattern](https://html-validate.org/rules/attr-pattern.html) | attributes failing configured regexes | config-supplied patterns against literal names and values |

One caveat sits across the whole group: a rule at error severity may miss and
may not guess, which is why `vhIsHandlerAttr` uses a named list instead of a
prefix test (`std/vyx-hints.vyrn:607-616`). Tables must be exact before any of
these fires as anything louder than a warning.

## Rules that need run time

Nineteen rules read RUN TIME across the two sources, plus the runtime half of
every EITHER row. Each reason names the fact no `.vyx` text holds.

| rule | source | why only at run time |
|---|---|---|
| Hydration mismatch detection | nuxt/hints | a mismatch compares two renderings; neither exists at compile time |
| LCP element must not have `loading="lazy"` | nuxt/hints | the measured LCP element is a runtime identity |
| INP tracking | nuxt/hints | interaction latency exists only while a page runs |
| CLS tracking | nuxt/hints | layout shift is geometry measured over time |
| Third-party script performance audit | nuxt/hints | render-blocking status is network timing during load |
| [aria-hidden-body](https://html-validate.org/rules/aria-hidden-body.html) | html-validate | `<body>` belongs to the shell document |
| [empty-title](https://html-validate.org/rules/empty-title.html) | html-validate | `<title>` composes in the head |
| [meta-refresh](https://html-validate.org/rules/meta-refresh.html) | html-validate | `<meta>` lives in the head |
| [long-title](https://html-validate.org/rules/long-title.html) | html-validate | title text composes across components |
| [unique-landmark](https://html-validate.org/rules/unique-landmark.html) | html-validate | landmark names collide across components |
| [no-multiple-main](https://html-validate.org/rules/no-multiple-main.html) | html-validate | the count spans every rendered component |
| [heading-level](https://html-validate.org/rules/heading-level.html) | html-validate | level order spans component boundaries, the case the checker header excludes (`std/vyx-hints.vyrn:42-43`) |
| [missing-doctype](https://html-validate.org/rules/missing-doctype.html) | html-validate | shell-document fact |
| [doctype-html](https://html-validate.org/rules/doctype-html.html) | html-validate | shell-document fact |
| [doctype-style](https://html-validate.org/rules/doctype-style.html) | html-validate | shell-document fact |
| [no-dup-id](https://html-validate.org/rules/no-dup-id.html) | html-validate | the uniqueness guarantee spans every rendered component; the static-id-inside-a-loop slice already fires (`std/vyx-hints.vyrn:357-371`) |
| [no-missing-references](https://html-validate.org/rules/no-missing-references.html) | html-validate | `for` and `aria-labelledby` targets resolve across files |
| [deprecated-class](https://html-validate.org/rules/deprecated-class.html) | html-validate | the retired-class list lives in project CSS, not template text |
| [deprecated-rule](https://html-validate.org/rules/deprecated-rule.html) | html-validate | fires on validator configuration, not templates |

The 39 EITHER rules keep a runtime half for the same structural reason: a bound
value (`:id`, `:type`, `:role`) or a cross-component reference escapes any
per-template check. The pattern repeats everywhere: literal values decide
statically, bindings need render.

## Defects a type system could have prevented

The five defect classes from source 2 that read YES: a compile-time check over
`.vyx` templates and their types catches the fault class
(`reka-defects.md`, "Tally").

| class | issues | what goes wrong | what the compiler would need to know |
|---|---|---|---|
| Contract drift between sibling triggers | [#2872](https://github.com/unovue/reka-ui/issues/2872) | `DrawerTrigger` hard-codes open while `DialogTrigger` toggles, so a second click is a no-op | a part named Trigger bound to a boolean open context must invert it; one family-wide rule |
| Invalid HTML content model in emitted markup | [#2384](https://github.com/unovue/reka-ui/issues/2384), [#1597](https://github.com/unovue/reka-ui/issues/1597) | checkbox inputs render inside `button` roots; axe reports interactive controls nested | the HTML content model: what a `button` may contain, and that interactive elements may not nest |
| Model-value type does not match the runtime value domain | [#2811](https://github.com/unovue/reka-ui/issues/2811), [#1308](https://github.com/unovue/reka-ui/issues/1308), [#2804](https://github.com/unovue/reka-ui/issues/2804), [#1641](https://github.com/unovue/reka-ui/issues/1641), [#1805](https://github.com/unovue/reka-ui/issues/1805) | pin inputs emit `undefined` into typed arrays; `multiple` fails to widen the value type | value types derived from sibling props such as `multiple`, not declared independently |
| Stale ref read at unmount | [#2873](https://github.com/unovue/reka-ui/issues/2873) | delete-by-ref misses because Vue unset the ref first; DOM nodes leak into a shared registry | ref validity phases: a forwarded-element ref reads non-null only between mount and before-unmount |
| `data-state` vocabulary collision | [#2823](https://github.com/unovue/reka-ui/issues/2823), [#1407](https://github.com/unovue/reka-ui/issues/1407) | seven vocabularies share one attribute name; composing two parts corrupts the state signal | each part declares a closed vocabulary per emitted `data-*` attribute; two parts on one element may not conflict |

Six PARTLY classes sit one step away, and each needs one named fact: mount
condition matching for dangling idrefs (#2882, #2597); whole-contract checking
for silently ignored props (#2034, #2142, #2476, #2548, #2103);
focusability-versus-visibility combination checks (#2776, #2163, #1280,
#2324); instantiating exported component types against representative call
sites before release (#2769, #2875); stable identifier derivation for hydration
(#1845, #2122, #785); presence-handoff state machines (#1985, #2767, #2160,
#2227, #920, #786, #1974).

## What std/vyx-hints.vyrn already covers

Twelve rules today (`std/vyx-hints.vyrn:294-495`):

| rule | line | shape |
|---|---|---|
| `sec/inline-handler` (error) | `std/vyx-hints.vyrn:298-310` | named handler-attribute list against `@click` replacement |
| `sec/unsafe-url` (error) | `std/vyx-hints.vyrn:311-325` | literal `javascript:` scheme, browser-exact stripping (`std/vyx-hints.vyrn:560-595`) |
| `sec/raw-html` | `std/vyx-hints.vyrn:326-341` | `v-html` warning, waivable at the line |
| `a11y/tabindex-positive` | `std/vyx-hints.vyrn:342-356` | literal positive tabindex |
| `a11y/dup-id` | `std/vyx-hints.vyrn:357-371` | static id inside `v-for` |
| `sec/blank-target` | `std/vyx-hints.vyrn:372-388` | `target="_blank"` without `rel="noopener noreferrer"` |
| `a11y/img-alt` | `std/vyx-hints.vyrn:392-407` | merges html-validate's `area-alt`, `wcag/h36`, `wcag/h37` |
| `perf/img-size` | `std/vyx-hints.vyrn:408-422` | unsized images, the common CLS cause |
| `a11y/click-target` | `std/vyx-hints.vyrn:423-440` | `@click` on non-interactive tags without `role`/`tabindex` |
| `a11y/control-name` | `std/vyx-hints.vyrn:441-458` | literally empty controls, partial `empty-heading` |
| `a11y/input-label` | `std/vyx-hints.vyrn:459-478` | the provable no-id slice of `input-missing-label` |
| `a11y/button-type` | `std/vyx-hints.vyrn:479-495` | handler button inside a form, narrower than `no-implicit-button-type` |

Against the two validator sources:

- Against html-validate: 9 of 94 rules have a counterpart here, and every one
  is narrower in element set or scope than the original
  (`html-validate-rules.md`, closing section). None of the 41 compile-time-capable
  rules outside that set has a port.
- Against nuxt/hints: covered 0 of 8. Three rows have adjacent work reachable
  from existing machinery — `perf/img-size` for CLS, the attribute walk for
  static `crossorigin`, the skipped component nodes for unused-component
  detection (`nuxt-hints-rules.md`, "Tallies"; `std/vyx-hints.vyrn:284-289`).

What the checker structurally cannot reach today, and why: rendered-page facts
(the header states this at `std/vyx-hints.vyrn:35-43`); cross-component facts
such as heading order and document-level id uniqueness; anything about a
component call, because a call is a Vyrn function and its tag name says nothing
about the markup it renders (`std/vyx-hints.vyrn:284-289`). That last limit is
the exact hole a Vyrn component library would fill: once component parts carry
declared contracts — required siblings, closed `data-*` vocabularies, prop
exclusion pairs like P1-P13 — the compiler can check composition instead of
guessing at opaque calls.
