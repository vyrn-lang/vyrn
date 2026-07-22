// @ts-check
//
// Vyrn VS Code extension — a thin client for the `vyrn-lsp` language server.
//
// Deliberately plain JavaScript (no TypeScript compile step) to keep the
// maintenance surface tiny: edit extension.js, reload the window. The only
// runtime dependency is `vscode-languageclient`, which spawns the server
// binary and shuttles JSON-RPC over stdio. The server does ALL the work
// (diagnostics); this file just launches it.

const path = require("path");
const fs = require("fs");
const { LanguageClient, TransportKind } = require("vscode-languageclient");

/** @param {import("vscode").ExtensionContext} context */
function activate(context) {
  const vsc = require("vscode");

  // Shared handle to the started language client, so the CodeLens provider can
  // query the server's `vyrn/isDevEntry` predicate (RFC-0064) for the
  // "▶ Run dev server" lens. Stays null until (and unless) the server starts.
  const lspState = { client: null };

  // A "▶ Run" CodeLens above `fn main` + the command it invokes. Mostly
  // independent of the language server (the Run/test/bench lenses work purely
  // from the document text and a terminal, so they register even if the server
  // binary is missing below); the "▶ Run dev server" lens additionally consults
  // `lspState.client` and simply stays hidden until the server is up.
  registerRun(context, vsc, lspState);

  const cfg = vsc.workspace.getConfiguration("vyrn");
  let serverPath = cfg.get("serverPath", "");

  if (!serverPath) {
    const exe = process.platform === "win32" ? "vyrn-lsp.exe" : "vyrn-lsp";
    // Resolve relative to the EXTENSION's own location, not the workspace
    // folder — the workspace may be empty (e.g. a single .vyrn file opened
    // directly), in which case `workspaceFolders[0]` is undefined and a
    // relative path would fail to spawn (ENOENT).
    //
    // 1. A server bundled inside the extension at ./server/<exe> — the .vsix
    //    ships this (see scripts/make-vsix.mjs), so the installed extension
    //    works with no Rust toolchain or build step.
    const bundled = path.join(context.extensionPath, "server", exe);
    // 2. Dev fallback: the extension lives at <repo>/editor/vscode, so the
    //    freshly-built dev server is two levels up, then into
    //    compiler/vyrn-lsp/target/debug.
    const dev = path.join(
      context.extensionPath,
      "..",
      "..",
      "compiler",
      "vyrn-lsp",
      "target",
      "debug",
      exe
    );
    serverPath = fs.existsSync(bundled) ? bundled : dev;
  }

  // A missing server is a setup problem, not a crash. Tell the user how to
  // build it and bail out cleanly instead of taking down the host.
  if (!fs.existsSync(serverPath)) {
    vsc.window.showWarningMessage(
      `Vyrn: language server not found at "${serverPath}". Build it with: ` +
        `cargo build --manifest-path compiler/vyrn-lsp/Cargo.toml ` +
        `(or set the "vyrn.serverPath" setting).`
    );
    return;
  }

  const serverOptions = {
    run: { command: serverPath, transport: TransportKind.stdio },
    debug: { command: serverPath, transport: TransportKind.stdio },
  };

  const clientOptions = {
    // `.vyrn` sources, plus `.vyx` generator inputs (RFC-0033): the server maps
    // hover/completion/go-to-definition and remapped diagnostics into the `.vyx`
    // buffer through the synthesized module that consumes it.
    documentSelector: [
      { scheme: "file", language: "vyrn" },
      { scheme: "file", language: "vyx" },
    ],
  };

  const client = new LanguageClient(
    "vyrn-lsp",
    "Vyrn Language Server",
    serverOptions,
    clientOptions
  );

  // `start()` returns a promise that rejects if the server can't be spawned;
  // catching it surfaces a clean error message instead of an unhandled
  // rejection that would crash the Extension Development Host.
  const started = client.start();
  context.subscriptions.push(started);
  started
    .then(() => {
      // The server is up: expose it to the CodeLens provider and nudge VS Code
      // to recompute lenses now that "▶ Run dev server" can be answered.
      lspState.client = client;
      vsc.commands.executeCommand("vyrn._refreshDevLens");
    })
    .catch((err) => {
      vsc.window.showErrorMessage(
        `Vyrn: failed to start language server "${serverPath}": ${err.message}`
      );
    });
}

/**
 * Register the "▶ Run" CodeLens over `fn main` and the `vyrn.run` command that
 * it fires. Vyrn's only entry point is `fn main`, so that is the one place a
 * "run this program" affordance belongs.
 *
 * @param {import("vscode").ExtensionContext} context
 * @param {typeof import("vscode")} vsc
 * @param {{ client: import("vscode-languageclient").LanguageClient | null }} lspState
 */
function registerRun(context, vsc, lspState) {
  // Fired to make VS Code re-request CodeLenses — used once the language server
  // finishes starting, so the async "▶ Run dev server" lens (RFC-0064) can
  // appear without the user having to edit the file first.
  const onDidChangeCodeLenses = new vsc.EventEmitter();

  // CodeLenses: "▶ Run" over each `fn main`, and — for tests (RFC-0015) — a
  // "▶ Run test" over each `test "name" { .. }` plus a "▶ Run all tests" over
  // the first one. A fresh regex per pass (the `g` flag makes `lastIndex`
  // stateful — never share it across calls).
  const provider = {
    onDidChangeCodeLenses: onDidChangeCodeLenses.event,
    async provideCodeLenses(document) {
      const lenses = [];
      const text = document.getText();
      const mainRe = /^[ \t]*fn\s+main\s*\(/gm;
      let m;
      while ((m = mainRe.exec(text)) !== null) {
        const pos = document.positionAt(m.index);
        const range = new vsc.Range(pos, pos);
        lenses.push(
          new vsc.CodeLens(range, {
            title: "▶ Run",
            command: "vyrn.run",
            arguments: [document.uri],
          })
        );
        // A zero-width match can't happen here (the pattern consumes `fn main(`),
        // but guard against an accidental infinite loop regardless.
        if (mainRe.lastIndex === m.index) mainRe.lastIndex++;
      }

      // `test "name"` blocks. The name is captured so the lens can filter to it
      // with `vyrn test --name "<name>"`. Mirrors the parser's contextual
      // recognition: `test` directly before a string literal.
      const testRe = /^[ \t]*test\s+"((?:[^"\\]|\\.)*)"/gm;
      let first = true;
      let t;
      while ((t = testRe.exec(text)) !== null) {
        const pos = document.positionAt(t.index);
        const range = new vsc.Range(pos, pos);
        const name = t[1];
        if (first) {
          lenses.push(
            new vsc.CodeLens(range, {
              title: "▶ Run all tests",
              command: "vyrn.testAll",
              arguments: [document.uri],
            })
          );
          first = false;
        }
        lenses.push(
          new vsc.CodeLens(range, {
            title: "▶ Run test",
            command: "vyrn.test",
            arguments: [document.uri, name],
          })
        );
        if (testRe.lastIndex === t.index) testRe.lastIndex++;
      }

      // `bench "name"` blocks (RFC-0055): "▶ Run bench" over each, "▶ Run all
      // benches" over the first. Same contextual shape as `test`.
      const benchRe = /^[ \t]*bench\s+"((?:[^"\\]|\\.)*)"/gm;
      let firstBench = true;
      let b;
      while ((b = benchRe.exec(text)) !== null) {
        const pos = document.positionAt(b.index);
        const range = new vsc.Range(pos, pos);
        const name = b[1];
        if (firstBench) {
          lenses.push(
            new vsc.CodeLens(range, {
              title: "▶ Run all benches",
              command: "vyrn.benchAll",
              arguments: [document.uri],
            })
          );
          firstBench = false;
        }
        lenses.push(
          new vsc.CodeLens(range, {
            title: "▶ Run bench",
            command: "vyrn.bench",
            arguments: [document.uri, name],
          })
        );
        if (benchRe.lastIndex === b.index) benchRe.lastIndex++;
      }

      // "▶ Run dev server" (RFC-0064): shown ONLY on a dev-server entry — a root
      // that imports `std/rpc` and has an `rpcServer(...)` call site. That
      // predicate is semantic, so the language server answers it (`vyrn/isDevEntry`)
      // rather than a brittle client-side regex. The lens sits above `fn main`
      // (or line 1 if there is none), alongside the "▶ Run" lens.
      if (lspState.client) {
        let isDev = false;
        try {
          isDev = await lspState.client.sendRequest("vyrn/isDevEntry", {
            textDocument: { uri: document.uri.toString() },
          });
        } catch (_e) {
          isDev = false; // server down / not ready: no dev lens, no error noise.
        }
        if (isDev) {
          const mainMatch = /^[ \t]*fn\s+main\s*\(/m.exec(text);
          const pos = document.positionAt(mainMatch ? mainMatch.index : 0);
          const range = new vsc.Range(pos, pos);
          lenses.push(
            new vsc.CodeLens(range, {
              title: "▶ Run dev server",
              command: "vyrn.dev",
              arguments: [document.uri],
            })
          );
        }
      }
      return lenses;
    },
  };

  context.subscriptions.push(
    vsc.languages.registerCodeLensProvider({ scheme: "file", language: "vyrn" }, provider),
    // Internal: fire the lens-refresh event (used when the server finishes
    // starting so the dev-server lens can appear without a manual edit).
    vsc.commands.registerCommand("vyrn._refreshDevLens", () => onDidChangeCodeLenses.fire())
  );

  context.subscriptions.push(
    vsc.commands.registerCommand("vyrn.run", (uri) => runVyrn(vsc, uri, (file) => ["run", file])),
    vsc.commands.registerCommand("vyrn.testAll", (uri) =>
      runVyrn(vsc, uri, (file) => ["test", file])
    ),
    // The name is the JSON-string body as it appears in source (with escapes);
    // unescape it so `vyrn test --name` matches the runtime test name.
    vsc.commands.registerCommand("vyrn.test", (uri, name) =>
      runVyrn(vsc, uri, (file) => ["test", file, "--name", unescapeTestName(name)])
    ),
    // Benches (RFC-0055): `vyrn bench` compiles native and times; a single-bench
    // lens filters with `--name` exactly like `vyrn.test`.
    vsc.commands.registerCommand("vyrn.benchAll", (uri) =>
      runVyrn(vsc, uri, (file) => ["bench", file])
    ),
    vsc.commands.registerCommand("vyrn.bench", (uri, name) =>
      runVyrn(vsc, uri, (file) => ["bench", file, "--name", unescapeTestName(name)])
    ),
    // "▶ Run dev server" (RFC-0064): `vyrn dev` is manifest-driven (it reads the
    // project's `server`/`client` keys), so this runs it in the manifest
    // directory, in a dedicated restartable terminal.
    vsc.commands.registerCommand("vyrn.dev", (uri) => runDev(vsc, uri))
  );
}

/**
 * Turn the source spelling of a test name (the characters between the quotes,
 * with `\"`/`\\` escapes) into its runtime value, so `--name` matches.
 *
 * @param {string} s
 * @returns {string}
 */
function unescapeTestName(s) {
  return String(s).replace(/\\(["\\])/g, "$1");
}

/**
 * The Vyrn repo root that owns `startDir`: the nearest ancestor containing
 * `compiler/Cargo.toml`. Walking up from the FILE (not the workspace folder)
 * is what makes the run command work when a subdirectory — `examples/`, a
 * project scaffold — is opened as the workspace: the workspace root then has
 * no `compiler/`, but an ancestor does.
 *
 * @param {string} startDir
 * @returns {string | null}
 */
function findRepoRoot(startDir) {
  let dir = startDir;
  for (let i = 0; i < 12; i++) {
    if (fs.existsSync(path.join(dir, "compiler", "Cargo.toml"))) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) return null; // filesystem root
    dir = parent;
  }
  return null;
}

/**
 * Run vyrn against a `.vyrn` file in the integrated terminal. `buildArgs(file)`
 * returns the full vyrn argument vector (e.g. `["run", file]` or
 * `["test", file, "--name", "..."]`). Resolution order for the compiler (first
 * hit wins):
 *   1. the `vyrn.path` setting, if set;
 *   2. `<repo>/compiler/target/release/vyrn.exe`, if it exists;
 *   3. `<repo>/compiler/target/debug/vyrn.exe`, if it exists;
 *   4. `cargo run -q --manifest-path <repo>/compiler/Cargo.toml -p vyrn-cli -- <args>`;
 *   5. no repo found at all: bare `vyrn <args>` (PATH install).
 * `<repo>` is found by walking up from the file (see [findRepoRoot]).
 *
 * @param {typeof import("vscode")} vsc
 * @param {import("vscode").Uri=} uri  the file (defaults to the active editor)
 * @param {(file: string) => string[]} buildArgs  vyrn args for the resolved file
 */
function runVyrn(vsc, uri, buildArgs) {
  const target = uri || (vsc.window.activeTextEditor && vsc.window.activeTextEditor.document.uri);
  if (!target || target.scheme !== "file") {
    vsc.window.showWarningMessage("Vyrn: no file to run.");
    return;
  }
  const file = target.fsPath;
  const command = resolveVyrnCommand(vsc, file, buildArgs(file));

  // Reuse a single named terminal rather than spawning one per click.
  const name = "vyrn";
  let terminal = vsc.window.terminals.find((t) => t.name === name);
  if (!terminal) {
    terminal = vsc.window.createTerminal(name);
  }
  terminal.show(true);
  terminal.sendText(command);
}

/**
 * Resolve the terminal command that runs `vyrn <args>` for a file. Compiler
 * resolution (first hit wins):
 *   1. the `vyrn.path` setting, if set;
 *   2. `<repo>/compiler/target/release/vyrn(.exe)`, if it exists;
 *   3. `<repo>/compiler/target/debug/vyrn(.exe)`, if it exists;
 *   4. `cargo run -q --manifest-path <repo>/compiler/Cargo.toml -p vyrn-cli -- <args>`;
 *   5. no repo found at all: bare `vyrn <args>` (PATH install).
 * `<repo>` is found by walking up from the file (see [findRepoRoot]).
 *
 * @param {typeof import("vscode")} vsc
 * @param {string} file  the .vyrn file (used only to locate the repo)
 * @param {string[]} args  the vyrn argument vector (e.g. `["run", file]`, `["dev"]`)
 * @returns {string} the shell command line
 */
function resolveVyrnCommand(vsc, file, args) {
  const exe = process.platform === "win32" ? "vyrn.exe" : "vyrn";
  const cfg = vsc.workspace.getConfiguration("vyrn");
  const vyrnPath = cfg.get("path", "");
  const repo = findRepoRoot(path.dirname(file));

  if (vyrnPath) {
    return invoke(vyrnPath, args);
  }
  if (repo) {
    const release = path.join(repo, "compiler", "target", "release", exe);
    const debug = path.join(repo, "compiler", "target", "debug", exe);
    if (fs.existsSync(release)) {
      return invoke(release, args);
    }
    if (fs.existsSync(debug)) {
      return invoke(debug, args);
    }
    const manifest = path.join(repo, "compiler", "Cargo.toml");
    // `cargo` is a bare program name on PATH, so it runs in any shell without a
    // call operator; only its arguments need quoting.
    return `cargo run -q --manifest-path ${quote(manifest)} -p vyrn-cli -- ${args
      .map(quote)
      .join(" ")}`;
  }
  // Not inside a Vyrn repo: assume an installed `vyrn` on PATH (and point at the
  // setting if that guess is wrong).
  vsc.window.setStatusBarMessage(
    'Vyrn: no compiler/ found above this file — using `vyrn` from PATH ' +
      '(set "vyrn.path" if that is not what you want)',
    8000
  );
  return `vyrn ${args.map(quote).join(" ")}`;
}

/**
 * The nearest ancestor directory of `startDir` that contains a `vyrn.json`, or
 * null. `vyrn dev` reads its `server`/`client` keys, so this is the directory
 * the command must run in.
 *
 * @param {string} startDir
 * @returns {string | null}
 */
function findManifestDir(startDir) {
  let dir = startDir;
  for (let i = 0; i < 20; i++) {
    if (fs.existsSync(path.join(dir, "vyrn.json"))) return dir;
    const parent = path.dirname(dir);
    if (parent === dir) return null; // filesystem root
    dir = parent;
  }
  return null;
}

/**
 * "▶ Run dev server" (RFC-0064). `vyrn dev` is manifest-driven — it reads the
 * project's `server`/`client` keys from `vyrn.json` and takes NO file argument —
 * so this runs `vyrn dev` in the project's manifest directory (the file's
 * nearest `vyrn.json`; falling back to the file's own dir if none is found, so
 * the CLI's own "needs a vyrn.json" error surfaces).
 *
 * The command runs in a DEDICATED terminal named `vyrn dev`, with restart
 * semantics: an existing one is disposed and replaced on re-click, so two
 * stacked dev servers never fight over the port.
 *
 * @param {typeof import("vscode")} vsc
 * @param {import("vscode").Uri=} uri  the server file (defaults to the active editor)
 */
function runDev(vsc, uri) {
  const target = uri || (vsc.window.activeTextEditor && vsc.window.activeTextEditor.document.uri);
  if (!target || target.scheme !== "file") {
    vsc.window.showWarningMessage("Vyrn: no file to run.");
    return;
  }
  const file = target.fsPath;
  const cwd = findManifestDir(path.dirname(file)) || path.dirname(file);
  const command = resolveVyrnCommand(vsc, file, ["dev"]);

  // Dedicated, restartable terminal: dispose an existing "vyrn dev" first so a
  // re-click restarts the server rather than stacking a second one on the port.
  const name = "vyrn dev";
  const existing = vsc.window.terminals.find((t) => t.name === name);
  if (existing) {
    existing.dispose();
  }
  const terminal = vsc.window.createTerminal({ name, cwd });
  terminal.show(true);
  terminal.sendText(command);
}

/**
 * Build a terminal command that invokes the quoted program `exe` with `args`.
 * A quoted path is a plain string literal in PowerShell (the modern default
 * shell on Windows) and would be echoed, not run — so on Windows the call
 * operator `&` is prepended to actually execute it. POSIX shells run a quoted
 * path directly, so no prefix there.
 *
 * @param {string} exe
 * @param {string[]} args
 * @returns {string}
 */
function invoke(exe, args) {
  const line = [quote(exe)].concat(args.map(quote)).join(" ");
  return process.platform === "win32" ? `& ${line}` : line;
}

/**
 * Double-quote a single path/argument for the integrated terminal.
 *
 * @param {string} s
 * @returns {string}
 */
function quote(s) {
  return `"${s}"`;
}

function deactivate() {}

module.exports = { activate, deactivate };