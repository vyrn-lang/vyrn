# RFC-0033 — Origin Maps: Editor Support Inside Generator Inputs

- **Status:** Implemented — see `compiler/vyrn-frontend/src/origin.rs`,
  `std/vyx.vyrn`, `std/ui.vyrn`, `compiler/vyrn-lsp`, and `editor/vscode`.
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

---

## Implementation notes & decisions (as landed)

- **The directive table lives in `origin.rs`** (`OriginMaps`), built during the
  existing load pass. `loader::load_with_origins` returns `(Program,
  OriginMaps)`; the table is parsed by a single line-scan over each synthesized
  module's retained source (`Module.gen_source`) — no second parse, and the
  directive stays a plain `//` comment (its third char is `@`, not `/`, so the
  lexer skips it and `fmt` preserves it as trivia; the table parser is
  indentation-insensitive, so `fmt` re-indenting the comment is harmless).
- **One remapping implementation serves CLI + LSP.** `OriginMaps::remap(&mut
  Diagnostic) -> bool` relocates a diagnostic at a governed generated line to its
  origin `file:line:col`, moves the generated location into a new
  `Diagnostic.note`, and returns whether it landed in a real input file. `vyrn
  check`/`run` call it in `lib::load` (the note prints as `  note: …`); the LSP
  calls the same from `symbols::analyze_inner`, splitting relocated diagnostics
  into `Analysis.remapped` (published against the input file's URI) from the rest
  (unchanged foreign-adoption behavior). A **malformed directive never loses the
  diagnostic** — it stays at the generated location with a `malformed …` note.
- **Fidelity achieved.** `std/vyx` emits `//@origin` with **column-exact**
  positions for `{expr}` and `{@raw expr}` interpolations (the exact `.vyx`
  column of the expression), and **region-level** directives (pointing at the
  construct's head column) for `{#if}`/`{:else if}` chains and `{#for}` heads.
  Event-handler and dynamic-attribute expressions are emitted inline on the
  element's single push line, so they map region-level (no own directive) in v1.
  `std/ui` emits **region-level** (`file:1:1`) directives bracketing each page's
  dispatch glue, so a check error in the router (e.g. a `page` whose return type
  isn't `Html`) is reported against the page module — the second-producer proof
  that the mechanism isn't `.vyx`-shaped.
- **LSP forward mapping.** `Analysis.origins` carries the table; the server keeps
  a `vyx_owner` registry (input file → the Vyrn document that synthesized from
  it) and an overlay-aware `EditorResolver` so unsaved `.vyx` edits regenerate
  live. A request inside a `.vyx` maps the cursor into the governed generated
  line — column-exact for verbatim regions via a longest-verbatim-prefix search
  (`align_expr`), region-start for derived ones — then answers hover / completion
  / go-to-definition against a fresh `analyze_linked` of the synthesized module.
  Go-to-definition from a template expression jumps to an imported `.vyrn`
  declaration (a binding local to the synthesized module has no on-disk target).
- **Editor.** `editor/vscode` registers the `vyx` language id (forwarded to the
  server via the client's `documentSelector`) and ships `vyx.tmLanguage.json`
  (sections, `{…}` interpolation with embedded `source.vyrn`, control blocks,
  component/element tags, event + dynamic attributes).
- **Stack.** The LSP loop moved onto a 64 MB worker thread: analyzing a document
  with generator imports runs the comptime interpreter and re-checks the
  synthesized module — deeper than the OS default main-thread stack (~1 MB on
  Windows) survives once the JSON/LSP frames are also on it.
- **Cache.** Gen cache keys are unaffected by directive presence except that
  `std/vyx` and `std/ui`'s own sources changed once (they now emit directives),
  so modules they synthesize rekey once and self-heal.
