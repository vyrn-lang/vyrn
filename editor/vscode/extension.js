// @ts-check
//
// Vela VS Code extension — a thin client for the `vela-lsp` language server.
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

  // A "▶ Run" CodeLens above `fn main` + the command it invokes. Independent of
  // the language server: it works purely from the document text and a terminal,
  // so it is registered even if the server binary is missing (below).
  registerRun(context, vsc);

  const cfg = vsc.workspace.getConfiguration("vela");
  let serverPath = cfg.get("serverPath", "");

  if (!serverPath) {
    const exe = process.platform === "win32" ? "vela-lsp.exe" : "vela-lsp";
    // Resolve relative to the EXTENSION's own location, not the workspace
    // folder — the workspace may be empty (e.g. a single .vela file opened
    // directly), in which case `workspaceFolders[0]` is undefined and a
    // relative path would fail to spawn (ENOENT).
    //
    // 1. A server bundled inside the extension at ./server/<exe> — the .vsix
    //    ships this (see scripts/make-vsix.mjs), so the installed extension
    //    works with no Rust toolchain or build step.
    const bundled = path.join(context.extensionPath, "server", exe);
    // 2. Dev fallback: the extension lives at <repo>/editor/vscode, so the
    //    freshly-built dev server is two levels up, then into
    //    compiler/vela-lsp/target/debug.
    const dev = path.join(
      context.extensionPath,
      "..",
      "..",
      "compiler",
      "vela-lsp",
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
      `Vela: language server not found at "${serverPath}". Build it with: ` +
        `cargo build --manifest-path compiler/vela-lsp/Cargo.toml ` +
        `(or set the "vela.serverPath" setting).`
    );
    return;
  }

  const serverOptions = {
    run: { command: serverPath, transport: TransportKind.stdio },
    debug: { command: serverPath, transport: TransportKind.stdio },
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "vela" }],
  };

  const client = new LanguageClient(
    "vela-lsp",
    "Vela Language Server",
    serverOptions,
    clientOptions
  );

  // `start()` returns a promise that rejects if the server can't be spawned;
  // catching it surfaces a clean error message instead of an unhandled
  // rejection that would crash the Extension Development Host.
  const started = client.start();
  context.subscriptions.push(started);
  started.catch((err) => {
    vsc.window.showErrorMessage(
      `Vela: failed to start language server "${serverPath}": ${err.message}`
    );
  });
}

/**
 * Register the "▶ Run" CodeLens over `fn main` and the `vela.run` command that
 * it fires. Vela's only entry point is `fn main`, so that is the one place a
 * "run this program" affordance belongs.
 *
 * @param {import("vscode").ExtensionContext} context
 * @param {typeof import("vscode")} vsc
 */
function registerRun(context, vsc) {
  // One CodeLens per `fn main` declaration. A fresh regex per pass (the `g`
  // flag makes `lastIndex` stateful — never share it across calls).
  const provider = {
    provideCodeLenses(document) {
      const lenses = [];
      const text = document.getText();
      const re = /^[ \t]*fn\s+main\s*\(/gm;
      let m;
      while ((m = re.exec(text)) !== null) {
        const pos = document.positionAt(m.index);
        const range = new vsc.Range(pos, pos);
        lenses.push(
          new vsc.CodeLens(range, {
            title: "▶ Run",
            command: "vela.run",
            arguments: [document.uri],
          })
        );
        // A zero-width match can't happen here (the pattern consumes `fn main(`),
        // but guard against an accidental infinite loop regardless.
        if (re.lastIndex === m.index) re.lastIndex++;
      }
      return lenses;
    },
  };

  context.subscriptions.push(
    vsc.languages.registerCodeLensProvider({ scheme: "file", language: "vela" }, provider)
  );

  context.subscriptions.push(
    vsc.commands.registerCommand("vela.run", (uri) => runFile(vsc, uri))
  );
}

/**
 * Run a `.vela` file with velac in the integrated terminal. Resolution order for
 * the compiler (first hit wins):
 *   1. the `vela.velacPath` setting, if set;
 *   2. `${workspaceFolder}/compiler/target/release/velac.exe`, if it exists;
 *   3. `${workspaceFolder}/compiler/target/debug/velac.exe`, if it exists;
 *   4. fallback: `cargo run -q --manifest-path <ws>/compiler/Cargo.toml -p vela-cli -- run <file>`.
 *
 * @param {typeof import("vscode")} vsc
 * @param {import("vscode").Uri=} uri  the file to run (defaults to the active editor)
 */
function runFile(vsc, uri) {
  const target = uri || (vsc.window.activeTextEditor && vsc.window.activeTextEditor.document.uri);
  if (!target || target.scheme !== "file") {
    vsc.window.showWarningMessage("Vela: no file to run.");
    return;
  }
  const file = target.fsPath;

  // The workspace folder anchors the compiler paths. Prefer the folder that
  // owns the file; fall back to the first folder, then the file's directory
  // (single-file open with no workspace).
  const wsFolder =
    (vsc.workspace.getWorkspaceFolder(target) &&
      vsc.workspace.getWorkspaceFolder(target).uri.fsPath) ||
    (vsc.workspace.workspaceFolders &&
      vsc.workspace.workspaceFolders[0] &&
      vsc.workspace.workspaceFolders[0].uri.fsPath) ||
    path.dirname(file);

  const exe = process.platform === "win32" ? "velac.exe" : "velac";
  const cfg = vsc.workspace.getConfiguration("vela");
  const velacPath = cfg.get("velacPath", "");

  let command;
  const release = path.join(wsFolder, "compiler", "target", "release", exe);
  const debug = path.join(wsFolder, "compiler", "target", "debug", exe);
  if (velacPath) {
    command = invoke(velacPath, ["run", file]);
  } else if (fs.existsSync(release)) {
    command = invoke(release, ["run", file]);
  } else if (fs.existsSync(debug)) {
    command = invoke(debug, ["run", file]);
  } else {
    const manifest = path.join(wsFolder, "compiler", "Cargo.toml");
    // `cargo` is a bare program name on PATH, so it runs in any shell without a
    // call operator; only its arguments need quoting.
    command = `cargo run -q --manifest-path ${quote(manifest)} -p vela-cli -- run ${quote(file)}`;
  }

  // Reuse a single named terminal rather than spawning one per click.
  const name = "vela";
  let terminal = vsc.window.terminals.find((t) => t.name === name);
  if (!terminal) {
    terminal = vsc.window.createTerminal(name);
  }
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