# Vela — VS Code support

A minimal VS Code extension that adds **syntax highlighting** (including
**regex highlighting** inside `=~` / `where` predicates and distinct colors for
`import`, function calls, and capability modifiers), **live diagnostics**,
**hover**, **go-to-definition**, **completion**, **document symbols / outline**
(including `test` blocks), **document formatting** (RFC-0017 — format-on-save
works with zero extra config, since the server advertises
`documentFormattingProvider`), **▶ Run / ▶ Run test CodeLenses**, and
**snippets** for the Vela language (`.vela` files).

It is deliberately tiny and plain-JavaScript (no TypeScript compile step):

- `extension.js` — spawns the `vela-lsp` server and shuttles JSON-RPC, and
  registers the ▶ Run / ▶ Run test CodeLenses + the `vela.run` / `vela.test` /
  `vela.testAll` commands (terminal launchers; no server needed). The server
  does the language-analysis work.
- `vela.tmLanguage.json` — a TextMate grammar (colors) derived from the real
  lexer token set: keywords, PascalCase types/variants, function calls,
  contextual capability modifiers (`consume`/`share`/`modify`/`read`), tagged
  templates, and structural regex highlighting after `=~`. Works even without
  the server.
- `snippets/vela.json` — snippets for `fn`, `main`, record/enum `type`,
  `protocol`, `impl`, `match`, `import`, the `logging` block, and `test`.
- `language-configuration.json` — `//` comment toggle + bracket matching.

## Run a file

A **▶ Run** CodeLens appears above every `fn main` (Vela's only entry point).
Clicking it runs the file in a reused integrated terminal named `vela`. The
compiler is resolved as: the `vela.velacPath` setting, else
`${workspaceFolder}/compiler/target/release/velac(.exe)`, else the `debug`
build, else `cargo run -p vela-cli -- run <file>`.

For tests (RFC-0015), a **▶ Run test** CodeLens sits above every
`test "name" { .. }` block — it runs `velac test <file> --name "name"` — and a
**▶ Run all tests** CodeLens sits above the first test block (`velac test
<file>`). Both use the same terminal and compiler-resolution as ▶ Run.

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
  package.json              extension manifest + grammar/language/snippet contributions
  extension.js              the LSP client + ▶ Run / ▶ Run test CodeLenses/commands (plain JS)
  vela.tmLanguage.json      TextMate grammar
  snippets/vela.json        snippets (fn, main, type, protocol, impl, match, import, logging, test)
  language-configuration.json
  server/vela-lsp.exe       bundled language server (deployed release build)
  node_modules/             vscode-languageclient (gitignored)
```

## What's covered, what's deferred

Hover / go-to-definition / completion cover **top-level** functions, types, and
variants; **locals/params** (with inferred `let` types, so hovering an
unannotated `let x = 5` shows `let x: Int64`); and **built-in method calls**
(`arr.push`, `log.info`, `Ref.get`, …) for hover plus **`.foo` member
completion** keyed off the receiver's type (`arr.` → `push`/`at`/`alen`/`afree`/
`length`; `log.` → `trace`/`debug`/`info`/`warn`/`error`). **Document symbols**
(outline / breadcrumbs / Ctrl-Shift-O) list the document's own top-level
functions, methods, types, and variants (imported symbols are excluded).
**Formatting** (Shift+Alt+F, or Format on Save) runs the canonical formatter
(RFC-0017) over the whole document; a buffer that fails to lex is left untouched
(no edit) rather than corrupted mid-edit. Deferred: range formatting
(whole-document only in v1), user
`protocol`/`impl` method-call resolution (the checker itself does not resolve
`impl` methods yet), and parser error recovery. See `ROADMAP.md`.