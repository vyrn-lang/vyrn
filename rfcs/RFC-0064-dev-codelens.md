# RFC-0064 — "Run dev server" CodeLens

- **Status:** Locked design
- **Depends on:** RFC-0019 (`vyrn dev`), RFC-0015/0055 (the Run/test/bench
  CodeLens plumbing this extends), RFC-0031 (module interfaces — how the
  LSP knows what a module imports)
- **Evidence (user):** "can we add button to run dev server in project
  where it should be used?"

---

## Design

- The LSP already computes per-document analysis; it gains a cheap
  predicate `isDevEntry`: the document (a `.vyrn` root) **calls `serve(`
  from `std/rpc`** (import present + a call site in the root module).
  That is the exact set of files `vyrn dev` is meant for — no config, no
  guessing by filename.
- The extension renders **"▶ Run dev server"** above `fn main` (or line 1
  if no main) of such documents, alongside the existing Run lens, and on
  click runs `vyrn dev <file>` — in the SAME dedicated terminal named
  `vyrn dev` per workspace: an existing terminal is disposed and
  replaced on re-click (restart semantics; two stacked servers on one
  port is never what anyone wants).
- Manifest awareness: if the file is the `vyrn.json` `main` of its
  project, the lens command runs `vyrn dev` from the manifest directory
  with no file argument (the established manifest-aware CLI behavior);
  otherwise it passes the file path.
- Non-server files show nothing — the "where it should be used" half of
  the request is the predicate, not a global button.

## Verification

1. LSP: `isDevEntry` surfaced (wherever the existing lens metadata
   travels — mirror how test/bench lenses are fed) and covered by an
   e2e test: a serve-calling root → lens present; a library module and a
   CLI example → absent.
2. Extension: lens appears on `examples/bin/server.vyrn` and
   `examples/shelf/server.vyrn`, not on `examples/vlog.vyrn`; re-click
   replaces the terminal (manual verification documented in the as-landed
   notes — extension UI has no automated harness today).
3. Full suite + LSP + parity green; 0 new clippy warnings; LSP rebuild +
   hash-verified redeploy (server-side predicate changed).

## Out of scope

Auto-restart on save (the dev loop's own job), port management, a status
bar indicator, and launching browsers.
