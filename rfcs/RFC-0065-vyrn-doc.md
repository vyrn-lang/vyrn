# RFC-0065 — `vyrn doc`: Markdown API Docs (Mermaid Included)

- **Status:** Locked design
- **Depends on:** the `///` markdown doc-comment convention (2026-07-14
  directive; detached-block rules from the hardening arc), RFC-0010
  (modules/exports), the `Analysis` symbol index (docs already travel
  there for hover)
- **Evidence (user):** "maybe we can add mermaid support to docs" — there
  is currently NO docs tool to support it: `///` markdown exists but only
  hover reads it.

---

## Design: emit Markdown, let the renderer do mermaid

`vyrn doc [file|project] [-o <dir>]` (default `docs/api/`) generates
**GitHub-flavored Markdown** — one `.md` per module + an `index.md` — and
passes fenced code blocks through VERBATIM. That single decision is the
mermaid feature: ````mermaid` fences in `///` comments render natively on
GitHub and in VS Code's preview, with zero bundled JavaScript, no HTML
pipeline, and no CDN (none of which a `vyrn doc --html` could ship
self-contained today).

```vyrn
/// Routes a request through the middleware chain.
///
/// ```mermaid
/// flowchart LR
///   req --> mw1 --> mw2 --> handler
/// ```
export fn route(req: Request) -> Response { … }
```

- **Per module page:** the module-header doc block, then every EXPORT in
  declaration order: a rendered signature line (from the checker's typed
  view — `fn route(req: Request) -> Response`, `type Paste = { … }`,
  protocols with their methods), then its `///` block verbatim. Private
  declarations are omitted (docs are the export surface).
- **Project mode:** with a `vyrn.json` (or a directory argument), walk
  the manifest main's import closure of LOCAL modules (std and remotes
  are excluded by default; `--std` adds the std modules for the repo's
  own docs). `index.md` lists modules with their header-doc first lines.
- Source of truth is the `Analysis` symbol index (docs + types already
  live there for hover) — NOT `moduleInterface` (its known gap: no docs).
  Deterministic, byte-stable output (stable ordering everywhere) so
  generated docs diff cleanly in git.
- Tests and benches are not documented (stripped declarations); `.vyx`
  components are out of scope v1 (their doc story belongs to the pages
  layer).

## Verification

1. Golden tests: a fixture module with header docs, fn/type/protocol
   exports, a mermaid fence, an unclosed fence (passes through verbatim
   — the tool never eats content), and detached `///` blocks (not
   attached, per the established rule) → byte-pinned output.
2. Project mode over `examples/bin` produces an index + per-module pages;
   determinism pinned (two runs byte-identical).
3. `docs/api/` for the repo's own `std/` generated with `--std` and
   committed as the living example (regenerated in the same commit
   whenever std docs change — a CI `--check`-style drift gate:
   `vyrn doc --std -o docs/api --verify` exits 1 on diff).
4. Full suite + LSP + parity green; 0 new clippy warnings; CLI-only ⇒ no
   LSP redeploy (state the unchanged hash).

## Out of scope

HTML output/site generator, search, cross-reference linking of type
names, doc-tests from `///` examples (still attractive, still later),
`.vyx` component docs, and rendering mermaid ourselves.
