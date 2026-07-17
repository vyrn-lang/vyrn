# RFC-0033 — Origin Maps: Editor Support Inside Generator Inputs

- **Status:** Draft (design locked)
- **Depends on:** RFC-0021 (generator imports — whose synthesized modules
  this makes traceable), RFC-0026 M4 (the `.vyx` compiler — first
  consumer, and the RFC that demanded this be format-agnostic), the LSP
  architecture (pure adapter over `analyze_linked`)
- **Evidence:** `.vyx` diagnostics for pass-through expressions surface
  against the *generated* module with only `// vyx <file> line N`
  breadcrumbs readable via `emit-gen`; there is no hover, completion, or
  go-to-def inside a template; and RFC-0026 named the guardrail: whatever
  mechanism fixes this must be one any third-party generator can use, or
  the first-party framework acquires an unfair advantage.

---

## The mechanism: origin directives (format-agnostic, the whole contract)

A generator may interleave **origin directives** in the source text it
returns:

```
//@origin ./components/ItemRow.vyx:14:21
```

- Scope: the directive applies to the **following lines** of generated
  source until the next directive or `//@origin end`. `path` is relative
  to the generator's importing module (the same base its inputs resolve
  against); `line:col` are 1-based positions in that input file.
- Semantics: "the generated text below was derived from — and for
  expression pass-throughs, is verbatim — the input at this position."
  A directive whose region is verbatim input text SHOULD set the exact
  start column; derived (non-verbatim) regions point at the construct
  they lower.
- That is the entire public contract. Nothing in it mentions `.vyx`,
  templates, or UI — any generator over any input format (a future SQL
  file, a GraphQL SDL, a `.vt` template dialect) emits the same
  directive and gets everything below.

## What the toolchain does with it

1. **Diagnostic remapping (CLI + LSP, single-sourced).** When a check/load
   diagnostic lands in a synthesized module at a line governed by an
   origin directive, the reported location becomes the origin
   (`file:line:col`), with the generated location preserved as a
   secondary note (the `emit-gen` breadcrumb story, kept). One remapping
   implementation in the frontend serves both `vyrn check`/`run` output
   and LSP published diagnostics — the LSP then publishes them against
   the INPUT file's URI, so errors appear inside the `.vyx` buffer.
2. **Forward mapping (editor requests inside the input file).** The LSP
   inverts the directive table per synthesized module: given a position
   in an input file that some generator consumed (the loader already
   records which generator import read which files), find the governed
   generated span, map the position into it (verbatim regions map
   column-exactly; derived regions map to the region start), and answer
   **hover / completion / go-to-definition** against the generated
   module's existing analysis. v1 scope: verbatim regions give full
   fidelity (template `{expr}`, `@click` args, script sections); derived
   regions give region-level answers. No rename/refactor in v1.
3. **Editor registration.** The VS Code extension registers the `.vyx`
   language id, forwards it to the server, and ships a small TextMate
   grammar for the template syntax (sections, brace blocks,
   interpolation, component tags) so `.vyx` stops rendering as plain
   text. Other input formats opt in the same way when they exist.

## Producers (v1)

- **`std/vyx`** upgrades its `// vyx <file> line N` breadcrumbs to
  `//@origin` directives with real columns for every pass-through
  expression (script section, `{expr}`, `{#if}`/`{#for}` heads, event
  attribute values) and region-level directives for lowered structure.
  This is the fidelity proof: a type error inside `{item.titel}` appears
  in the `.vyx` buffer at the exact offending column, and hover on
  `item.` inside the template completes the record's fields.
- **`std/ui` pages** emits directives for the page-module spans it
  copies (Params/loader glue) — a smaller, second producer proving the
  mechanism isn't vyx-shaped.

## Guardrails (locked)

- The directive is **inert everywhere else**: it is a comment; parsing,
  hashing (gen cache keys), fmt, and `emit-gen` treat it as ordinary
  source text. A generator that emits none behaves exactly as today.
- Mapping must never LOSE a diagnostic: if a directive is malformed or
  the origin file/position doesn't exist, the diagnostic surfaces at the
  generated location with the malformed directive noted — never dropped.
- `analyze_linked` performance: the directive table is built during the
  existing analysis pass (no second parse); the editor never re-runs
  generation beyond what analysis already does (the cache carries it).

## Out of scope

Rename/refactor across the mapping, watch-mode regeneration semantics
(the LSP's existing generation/caching behavior is unchanged), source
maps for non-generator transforms (there are none), embedding foreign
LANGUAGES inside Vyrn (this is about Vyrn generated FROM foreign files),
`.vyx`↔`Tw` class checking (needs the components↔theme coupling design;
still deferred, though this RFC builds the road it will use).
