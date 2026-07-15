# Vela — VS Code support

A minimal VS Code extension that adds **syntax highlighting**, **live
diagnostics**, **hover**, **go-to-definition**, and **completion** for the
Vela language (`.vela` files).

It is deliberately tiny and plain-JavaScript (no TypeScript compile step):

- `extension.js` — spawns the `vela-lsp` server and shuttles JSON-RPC. The
  server does **all** the work; this file just launches it.
- `vela.tmLanguage.json` — a TextMate grammar (colors) derived from the real
  lexer token set. Works even without the server.
- `language-configuration.json` — `//` comment toggle + bracket matching.

The LSP server (`compiler/vela-lsp`) is a thin adapter over the compiler's core
diagnostics + symbol-query API (`vela_frontend::analyze`), the same one `velac
check` uses — so the editor and the CLI report identical errors, and a document
is parsed once per change (hover/def/completion read the cached result, never
re-parsing).

## Try it (development)

1. Build the server once (the `F5` launch does **not** rebuild it — a Windows
   file-lock on the running binary used to abort the launch, so the build is a
   separate manual step now). Re-run this whenever you change server source:

   ```
   cargo build --manifest-path compiler/vela-lsp/Cargo.toml
   ```
   (Equivalently: run the `build-lsp` VS Code task from the Command Palette.)

2. Open this repo (`N:\lang`) in VS Code and press **F5**. An "Extension
   Development Host" window opens with the Vela extension loaded.

3. Open any file under `examples/` (e.g. `examples/enum.vela`):
   - Colors render from the TextMate grammar.
   - Inject a type error and save — red squiggles appear at the **exact token**
     (lexer/parser) or **whole line** (checker/movecheck), one per error even
     across multiple functions.
   - Hover an identifier (e.g. `Circle` in `area(Circle(2))`) → a tooltip with
     the variant/function/type detail.
   - F12 / Ctrl-click on `area` → jumps to the `fn area` line; on `Circle` →
     jumps to `| Circle(Int64)`.
   - Trigger completion (Ctrl+Space, or type a prefix) → top-level functions,
     types, and variants.

The server path defaults to
`${workspaceFolder}/compiler/vela-lsp/target/debug/vela-lsp(.exe)`. Override
with the `vela.serverPath` setting for a release/bundled build.

## Package a .vsix

`npm run package` (from `editor/vscode`) builds the server in **release** mode,
copies the binary into `./server/`, and produces a platform-tagged
`vela-<target>-<version>.vsix` (e.g. `vela-win32-x64-0.1.0.vsix`) that bundles
the server — so the installed extension works with no Rust toolchain. `@vscode/vsce` is the only dev dependency (`npm install` once first). Install the
result with:

```
code --install-extension vela-win32-x64-0.1.0.vsix
```

`extension.js` resolves the server as: the `vela.serverPath` setting, else the
bundled `./server/vela-lsp(.exe)`, else the dev build at
`<repo>/compiler/vela-lsp/target/debug/vela-lsp(.exe)`. A bundled binary makes
the `.vsix` host-specific (the `--target` flag tags it accordingly); rebuild on
each target platform you want to ship.

## Layout

```
editor/vscode/
  package.json              extension manifest + grammar/language contributions
  extension.js              the LSP client (plain JS)
  vela.tmLanguage.json      TextMate grammar
  language-configuration.json
  node_modules/             vscode-languageclient (gitignored)
```

## What's covered, what's deferred

Hover / go-to-definition / completion cover **top-level** functions, types, and
variants; **locals/params** (with inferred `let` types, so hovering an
unannotated `let x = 5` shows `let x: Int64`); and **built-in method calls**
(`arr.push`, `log.info`, `Ref.get`, …) for hover plus **`.foo` member
completion** keyed off the receiver's type (`arr.` → `push`/`at`/`alen`/`afree`/
`length`; `log.` → `trace`/`debug`/`info`/`warn`/`error`). Deferred: user
`protocol`/`impl` method-call resolution (the checker itself does not resolve
`impl` methods yet), and parser error recovery. See `ROADMAP.md`.