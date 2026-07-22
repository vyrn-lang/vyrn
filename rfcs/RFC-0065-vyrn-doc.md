# RFC-0065 — `vyrn doc`: Markdown API Docs (Mermaid Included)

- **Status:** Implemented
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

## As landed

`vyrn doc [file|dir] [-o <dir>] [--std] [--verify]` ships (default output
`docs/api/`). GitHub-flavored Markdown, one `.md` per module plus `index.md`;
`///` blocks — mermaid fences and all — pass through **verbatim**, so the
diagram feature is exactly "don't touch fenced content."

- **Source of truth.** A new `pub vyrn_frontend::symbols::module_doc(source) ->
  ModuleDoc` builds the model from the **parse alone** — no checker, no linker —
  so a module documents even with unresolved imports, and the signature line
  reuses the very renderers hover uses (`function_detail` / `type_decl_detail` /
  `protocol_detail`). It carries the detached file-header block plus every
  export (`fn`/`type`/`protocol`) in declaration-line order. Private decls,
  tests, and benches never appear (they aren't in `functions`/`protocols`/
  `type_decls`, or aren't `exported`).
- **The header block** reuses the parser's detachment rule: the leading `///`
  run is the module header only when a blank line (or EOF) detaches it from the
  first declaration; a block sitting directly above a decl belongs to that
  decl. An **unclosed fence** is emitted verbatim — the tool never eats content.
- **Input modes.** A directory argument scans every `.vyrn` under it (module
  name = path relative to the dir); a file or bare invocation walks the manifest
  main's local-import **closure** via the RFC-0010 module graph (remote and
  generated modules excluded); `--std` adds the reached std modules, and — with
  no target — documents the whole std library (`std/<rel>` names). Output is
  deterministic and byte-stable: every list is sorted, newlines are always LF
  (a CRLF checkout of a generated page is not flagged as drift). Because the
  lexer strips CR from doc tokens, the output is identical from a CRLF (Windows)
  or LF (CI) checkout of the std sources.
- **The drift gate.** `--verify` writes nothing and exits 1 when `docs/api/`
  differs from a fresh generation — a stale/extra page, a missing page, or any
  byte of content. A plain generate prunes stale `.md` files so the two always
  converge. `docs/api/` (index + 20 std module pages) is committed as the living
  example, and the test job runs `vyrn doc --std -o docs/api --verify` (cheap,
  no clang) so std doc changes must be regenerated in the same commit.

**Deviation (documented):** the RFC sketched `vyrn doc --std -o docs/api
--verify` for the repo's own docs but left the "document std itself" invocation
shape open. As landed, `--std` with **no** file/directory argument (and no
manifest main) documents the entire std library by scanning the std root — which
is exactly that committed command. `--std` alongside a project still layers the
reached std modules onto the local closure, as specified.

**Verification.** Byte-pinned golden tests (`compiler/vyrn-cli/tests/doc.rs`):
a fixture with a header block, `fn`/`type`/`protocol` exports, a `mermaid`
fence, an unclosed fence, a detached `///` block (does not attach), a private
decl and a `test` block (both omitted) → pinned page + index; determinism and
both `--verify` outcomes covered. Frontend unit tests pin the detach rule and
verbatim fenced passthrough. Full workspace suite, `vyrn-lsp` (excluded crate),
and the three-way parity harness all green; `vyrn fmt --check` clean; 0 new
clippy warnings. CLI-only — no LSP redeploy; the deployed `vyrn-lsp.exe` stays
`57569c62bbec95ca7cdcb43f093a001af4836db969d0ef5a55a013f25049a116` (the new
`module_doc` is additive and unused by the LSP, so its served behavior is
unchanged).
