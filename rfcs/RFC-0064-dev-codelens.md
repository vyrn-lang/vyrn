# RFC-0064 — "Run dev server" CodeLens

- **Status:** Implemented
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

---

## As landed

Shipped as designed, with two spelling/plumbing realities the locked text
abstracted over (both flagged below — neither changes the behavior the evidence
asked for: a dev-server button exactly where it belongs).

### `isDevEntry` — an LSP predicate over a custom request

The server gained `is_dev_entry(source)`: a **lex+parse of the root document
only** (no linking, no generation), true iff `program.imports` contains a
`std/rpc` import AND an `rpcServer(…)` generator-import call site. That is
precisely the set of server roots `vyrn dev` builds+serves — a client
(`rpcClient`), an in-process module (`rpcInProcess`), a library, and a CLI
example all fail one half.

It is surfaced as a custom request, **`vyrn/isDevEntry`** (`{ textDocument:
{ uri } }` → a bool), answered from the open buffer (else disk). An e2e test
drives it over stdio in the VS Code URI form: positive on a synthetic
serve-calling root and the real `examples/bin/server.vyrn` +
`examples/shelf/server.vyrn`; negative on an rpc client, a plain module, and
`examples/vlog.vyrn`.

### Extension — an async lens, a restartable terminal

The client-side CodeLens provider became `async`: after its regex Run/test/bench
lenses, it `sendRequest("vyrn/isDevEntry", …)` and, when true, pushes
**"▶ Run dev server"** above `fn main` (or line 1), alongside Run. An
`onDidChangeCodeLenses` emitter fires once the server finishes starting, so the
lens appears without a manual edit. Clicking runs `vyrn dev` in the file's
manifest directory, in a dedicated terminal named `vyrn dev` that is disposed and
re-created on re-click (restart semantics). `package.json` gains the
`Vyrn: Run Dev Server` command.

### Deviations (locked points that were impossible verbatim)

1. **`serve(` → `rpcServer(`.** The RFC wrote the predicate as "calls `serve(`
   from std/rpc", but `std/rpc` exposes no `serve` — a server root is composed by
   importing from the `rpcServer("…")` generator
   (`import { rpcHandle } from rpcServer("./contract")`). That generator import
   *is* the "import present + a call site in the root module" the RFC describes,
   so `rpcServer` is the real spelling of `serve`, and the predicate is exactly
   as strict as intended.

2. **The lens is fed by a custom request, not existing lens metadata.** The RFC
   said to surface `isDevEntry` "wherever the existing lens metadata travels —
   mirror how test/bench lenses are fed". In this codebase the Run/test/bench
   lenses are produced *client-side* by regex in `extension.js`; there is no
   LSP→lens metadata channel to mirror. The closest sound thing — and what keeps
   the *predicate* semantic and server-side as the RFC requires (LSP change,
   e2e test, redeploy) — is a dedicated `vyrn/isDevEntry` request the client-side
   provider queries. This is the adapted "same channel".

3. **The lens always runs the manifest-dir `vyrn dev`, never `vyrn dev <file>`.**
   The RFC's manifest branch ("if the file is the `vyrn.json` main → `vyrn dev`
   from the manifest dir; otherwise pass the file path") assumes `vyrn dev` can
   take a file. It cannot: `vyrn dev` is inherently manifest-driven — it reads the
   project's `server`+`client` keys and needs both, so a lone server file can't be
   dev-served. A dev-entry file always lives in such a project, so the lens runs
   `vyrn dev` in the nearest `vyrn.json`'s directory (falling back to the file's
   own dir only so the CLI's own "needs a vyrn.json" error surfaces). The
   "pass the file path" branch is unreachable by construction.

### Manual verification (no automated extension harness today)

On `examples/bin/server.vyrn` and `examples/shelf/server.vyrn` the lens renders;
on `examples/vlog.vyrn` and library modules it does not (matching the e2e
predicate coverage). Clicking runs `vyrn dev` in the project directory; a second
click disposes the `vyrn dev` terminal and starts a fresh one.

### Verification

- LSP suite: **43 passed**, 1 ignored (`cd compiler/vyrn-lsp && cargo test`),
  incl. the new `is_dev_entry_positive_and_negative` e2e test.
- Full workspace suite + three-way parity green; `extension.js` / `package.json`
  pass `node --check` / JSON parse; 0 new clippy warnings.
- `vyrn-lsp.exe` rebuilt (release) + redeployed to `editor/vscode/server/`;
  SHA-256 pair verified equal:
  `57569c62bbec95ca7cdcb43f093a001af4836db969d0ef5a55a013f25049a116`.
