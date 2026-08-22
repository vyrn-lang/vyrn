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

/** @param {import("vscode").ExtensionContext} context */
function activate(context) {
  const vsc = require("vscode");
  // Required here rather than at the top so this file loads outside VS Code
  // with nothing installed — which is what lets `test/resolve.test.mjs` import
  // the real server-resolution helpers instead of a copy of them.
  const { LanguageClient, TransportKind } = require("vscode-languageclient");

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

  // Where the language server comes from, first hit wins:
  //   1. the `vyrn.serverPath` setting;
  //   2. `vyrn-lsp` from the installed toolchain — on PATH, or beside the
  //      `vyrn` that is on PATH (release archives carry both, RFC-0105 M3);
  //   3. the repository's own debug build, which is what F5 uses.
  const cfg = vsc.workspace.getConfiguration("vyrn");
  let serverPath = cfg.get("serverPath", "");

  if (!serverPath) {
    const win = process.platform === "win32";
    const exe = win ? "vyrn-lsp.exe" : "vyrn-lsp";
    const driver = win ? "vyrn.exe" : "vyrn";
    // 2. The server that came with the toolchain. `vyrn-lsp` ships in the same
    //    release archive as `vyrn` and lands in the same directory, so an
    //    install.sh / install.ps1 user already has it: take it off PATH
    //    directly, or from beside the `vyrn` that is on PATH (a shim or a
    //    symlink into ~/.vyrn/bin resolves through `realpathSync`).
    const installed = onPath(exe) || beside(onPath(driver), exe);
    // 3. Dev fallback: the extension lives at <repo>/editor/vscode, so the
    //    freshly-built dev server is two levels up, then into
    //    compiler/vyrn-lsp/target/debug. Resolved relative to the EXTENSION's
    //    own location, not the workspace folder — the workspace may be empty
    //    (a single .vyrn file opened directly), in which case
    //    `workspaceFolders[0]` is undefined and a relative path fails to spawn.
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
    serverPath = installed || dev;
  }

  // A missing server is a setup problem, not a crash. Name every way to get one
  // and bail out cleanly instead of taking down the host.
  if (!fs.existsSync(serverPath)) {
    vsc.window.showWarningMessage(
      `Vyrn: language server not found (looked for "${serverPath}"). ` +
        `Either install the toolchain (the release archive carries vyrn-lsp beside vyrn), ` +
        `or build it with: cargo build --manifest-path compiler/vyrn-lsp/Cargo.toml, ` +
        `or point the "vyrn.serverPath" setting at a binary you already have.`
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

      // The derived wire path above each procedure a generator mounts
      // (RFC-0073 M3). The path is derived from the file's own location and the
      // export's name, so nothing in the buffer states it — which is exactly
      // what makes it worth a lens. Semantic, and it comes from the symbol map
      // the generator baked in, so the server answers it (`vyrn/routeLenses`).
      //
      // No command: a POST endpoint is not something a click can usefully open,
      // and the lens exists to make a derived fact visible rather than to do
      // anything. `command: ""` is VS Code's own spelling for a lens that is
      // text.
      if (lspState.client) {
        let routes = [];
        try {
          routes = await lspState.client.sendRequest("vyrn/routeLenses", {
            textDocument: { uri: document.uri.toString() },
          });
        } catch (_e) {
          routes = []; // server down / not ready: no route lenses, no error noise.
        }
        for (const r of routes || []) {
          const pos = new vsc.Position(r.line, 0);
          lenses.push(
            new vsc.CodeLens(new vsc.Range(pos, pos), { title: r.title, command: "" })
          );
        }
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
 * The first executable named `exe` on PATH, with symlinks and shims resolved to
 * the real file — so "what is beside it" means what is beside the binary, not
 * what is beside the link. Null if PATH has none. No dependency: `which` is a
 * loop over `process.env.PATH`, and this needs no PATHEXT search because every
 * caller passes the full file name including `.exe`.
 *
 * @param {string} exe
 * @returns {string | null}
 */
function onPath(exe) {
  const dirs = (process.env.PATH || "").split(path.delimiter);
  for (const dir of dirs) {
    if (!dir) continue;
    const candidate = path.join(dir, exe);
    try {
      if (fs.existsSync(candidate)) return fs.realpathSync(candidate);
    } catch (_e) {
      // An unreadable PATH entry is not this extension's problem: keep looking.
    }
  }
  return null;
}

/**
 * `exe` in the same directory as `anchor`, if both exist. Null when the anchor
 * is null, so it composes directly with [onPath].
 *
 * @param {string | null} anchor
 * @param {string} exe
 * @returns {string | null}
 */
function beside(anchor, exe) {
  if (!anchor) return null;
  const candidate = path.join(path.dirname(anchor), exe);
  return fs.existsSync(candidate) ? candidate : null;
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
 * Quote a single path/argument for the integrated terminal: PowerShell rules
 * (double quotes, backtick escapes) on Windows, POSIX single quotes elsewhere.
 *
 * @param {string} s
 * @returns {string}
 */
function quote(s) {
  if (process.platform === "win32") {
    // The integrated terminal on Windows is PowerShell (the `&` in `invoke`
    // already assumes it). Inside its double-quoted strings a backtick, a `$`
    // and a `"` are all live: each is escaped with a backtick, and a literal
    // backtick doubles. Order matters — the backtick that escapes `"` is
    // inserted after literal backticks have been doubled.
    return `"${s.replace(/`/g, "``").replace(/\$/g, "`$").replace(/"/g, '`"')}"`;
  }
  // POSIX shells: single quotes keep every byte literal except `'` itself,
  // which closes the string, is an escaped literal quote, and reopens.
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

function deactivate() {}

// `onPath` and `beside` are exported for `test/resolve.test.mjs`. VS Code reads
// `activate`/`deactivate` and ignores the rest.
module.exports = { activate, deactivate, onPath, beside };