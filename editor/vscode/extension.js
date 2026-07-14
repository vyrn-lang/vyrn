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

function deactivate() {}

module.exports = { activate, deactivate };