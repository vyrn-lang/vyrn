# RFC-0107 — A Template Component Is a Library

- **Status:** **Proposed.** No implementation. Milestones below; a milestone
  that fails its gate says so in this file.
- **Depends on:** RFC-0021 (generators — the comptime sandbox and the recorded
  inputs its cache is keyed on), RFC-0026/0039 (std/vyx, the template compiler
  this RFC gives an extension point), RFC-0099 (generator diagnostics with
  positions), RFC-0033/0048 (origin maps — how a provider's error lands in the
  template), RFC-0010 (aliases and pinned remote files — how a collection
  arrives), RFC-0027 (`import * as ns`).
- **Evidence (user):** "does vyx have iconify support? Like nuxt/icon does",
  "but that import looks to complex", "such components shouldn't be hardcoded
  and there shouldn't be such behaviors, this is nonsense", "is Icons bound to
  vyx?".

---

## The problem

The site needs icons; every Vyrn UI will. The obvious shape — an `<Icon>` tag
resolved at compile time from a pinned Iconify collection — is easy to build
by hardwiring it into `std/vyx`. That is the wrong build, and the user said so
in the words above: the moment the template compiler carries one privileged
component, it stops being "a template language as a library" and becomes a
framework with blessed built-ins. The repository's whole thesis (RPC, i18n,
OpenAPI, GraphQL, the UI layer — all libraries over `gen fn`, zero compiler
changes) argues the other way.

## The line

**Directives are the language; components are libraries.** `v-for`, `:attr`,
`{{ }}` belong to `std/vyx`. Every capitalized tag resolves to a name the
template's script section imported — user `.vyx` components today, and with
this RFC, generation-time components from any library. `std/vyx` names no
component. That sentence is a gate (greppable), not a hope.

## The design

**Discovery is an import, not a registry.** A `.vyx` script section imports
the component like anything else; the tag resolves against names in scope.
No manifest key, no plugin config, no global state.

**The provider contract is a protocol.** A generation-time component is an
exported value conforming to a conventional shape — attributes in,
`Result<Html, Issue>` out — which the `.vyx` compiler evaluates in the
comptime sandbox while generating the page, splicing the returned tree where
the tag stood. Static attributes only: a `:name` bound to a runtime
expression is refused, because a name the compiler cannot read cannot be
checked or pinned, and that refusal is the feature.

**Diagnostics and caching are the existing machinery.** A provider's `Issue`
becomes an RFC-0099 diagnostic positioned into the template through origin
maps. Every file a provider reads goes through the recording resolver, so the
generator cache is keyed on the provider's true inputs — a changed collection
file or provider source invalidates exactly what it should.

## std/icons — the proof of protocol

Layered so nothing binds to `.vyx`:

1. **The data**: an Iconify collection is one JSON file (name → SVG body),
   pinned like any dependency — `vyrn add` writes the alias and the lock line.
   Each collection's license is surfaced into the generated module's doc
   header; glyphs ship with their terms.
2. **The core is a plain generator**, usable from any `.vyrn` file:
   `import * as ic from icons("icons", "github discord rss")` — names once,
   in the argument; unknown names are generation diagnostics with a "nearest"
   suggestion; only named glyphs are generated, so editor analysis stays fast
   and the artifact carries exactly what is used.
3. **`<Icon name="brand:github"/>`** is one consumer of that core through the
   provider protocol. The prefix vocabulary is the manifest's alias keys
   (`icons`, `icons/brand`), not a hardcoded registry. Emitted markup is
   inline `<svg aria-hidden="true">` using `currentColor` — the glyphs follow
   the palette tokens and the theme control with no per-icon work; a `label`
   attribute adds the accessible name when the icon is the content.

The dependency arrow points one way: the `.vyx` consumer depends on
`std/icons`; `std/icons` does not know `.vyx` exists. A third-party template
language consumes the same core — that is the test the layering is real.

## What the Iconify runtime does, that this deliberately does not

No runtime fetching of icon data (their API/CDN mode): resolution is at
compile time, from hash-locked files, offline-capable. An unknown icon fails
the build instead of rendering an empty box.

## Milestones

**M0 — the feasibility probe.** The one real design risk: the `.vyx`
generator evaluating an *imported provider module* inside the comptime
sandbox — dynamic from the generator's point of view, recorded for the cache,
diagnosable with positions. One probe that proves or refutes it, in the
census-by-execution style, before anything else is written. Gate: the probe's
transcript in this file, and a stated verdict; if refuted, the fallback
design (provider resolution precomputed by the loader) is chosen here with
the reason.

**M1 — the protocol in std/vyx.** The contract type, tag resolution against
imported names, attribute passing, the static-attribute refusal, diagnostics
through origin maps, cache soundness. Gate: a toy provider in the test suite
round-trips; `std/vyx` contains zero component names — asserted by a test,
not a grep someone runs once; all existing `.vyx` pages compile unchanged.

**M2 — std/icons core.** The generator, the `vyrn add` flow for a collection,
license surfacing, the nearest-name diagnostic, `* as` usage from plain
`.vyrn`. Gate: a program using two collections builds offline from the lock;
a misspelled icon shows the diagnostic verbatim in this file.

**M3 — the `<Icon>` provider and the first consumer.** The provider over the
core; the site's shell (RFC-0106 M1/M2) consumes it for OS tiles, footer and
pillar glyphs. Gate: the site export carries only glyphs the templates name
(counted); the a11y checklist rows for decorative-vs-labelled icons pass.

## What this RFC does not do

- It does not put any component into `std/vyx` — including `<Icon>`.
- It does not fetch anything at runtime, ever.
- It does not support runtime-chosen icon names; render the fixed set and
  toggle visibility.
- It does not vendor Iconify's tooling; the data format is the interface.
