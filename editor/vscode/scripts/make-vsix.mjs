// Package the Vyrn VS Code extension + a bundled `vyrn-lsp` server into a
// platform-tagged .vsix. Cross-platform (Node) — no bash required.
//
//   npm run package      (run from editor/vscode)
//
// Steps: (1) `cargo build --release` the server, (2) copy the binary into
// ./server/ next to extension.js so the .vsix ships it, (3) `vsce package`
// with a `--target` matching the host (the bundled binary is host-specific, so
// the .vsix is tagged for that platform). The produced file is
// `vyrn-<version>-<target>.vsix` in editor/vscode/.
//
// `extension.js` resolves the server as: the `vyrn.serverPath` setting, else
// the bundled `./server/<exe>` (this step), else the dev build at
// <repo>/compiler/vyrn-lsp/target/debug/<exe>.
import { spawnSync } from "node:child_process";
import path from "node:path";
import fs from "node:fs";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const extDir = path.resolve(here, ".."); // editor/vscode
const repoRoot = path.resolve(extDir, "..", ".."); // the Vyrn repo root
const lspManifest = path.join(repoRoot, "compiler", "vyrn-lsp", "Cargo.toml");

const isWin = process.platform === "win32";
const exe = isWin ? "vyrn-lsp.exe" : "vyrn-lsp";
// The bundled binary is host-specific, so tag the .vsix for that platform.
const target =
  isWin ? "win32-x64"
  : process.platform === "darwin" ? (process.arch === "arm64" ? "darwin-arm64" : "darwin-x64")
  : "linux-x64";

function run(cmd, args, cwd) {
  console.log(`$ ${cmd} ${args.join(" ")}`);
  const res = spawnSync(cmd, args, { cwd, stdio: "inherit", shell: isWin });
  if (res.status !== 0) {
    console.error(`\n${cmd} exited with status ${res.status}`);
    process.exit(res.status ?? 1);
  }
}

function readVersion(extDir) {
  const pkg = JSON.parse(fs.readFileSync(path.join(extDir, "package.json"), "utf8"));
  return pkg.version;
}

// 1. Build the server (release — a smaller, distributable binary).
run("cargo", ["build", "--manifest-path", lspManifest, "--release"], repoRoot);

// 2. Copy the binary into ./server/ so `vsce` bundles it (extension.js finds
// it there at runtime).
const built = path.join(repoRoot, "compiler", "vyrn-lsp", "target", "release", exe);
if (!fs.existsSync(built)) {
  console.error(`built server not found at ${built}`);
  process.exit(1);
}
const serverDir = path.join(extDir, "server");
fs.mkdirSync(serverDir, { recursive: true });
const dest = path.join(serverDir, exe);
fs.copyFileSync(built, dest);
console.log(`copied ${built} -> ${dest}`);

// 3. Package. `--allow-missing-repository` avoids fabricating a repo URL in
// package.json; `--target` tags the .vsix for the host platform; `--no-update-
// package-json` keeps the version pinned (no git-tag bump in this dev flow).
run(
  "npx",
  [
    "--yes",
    "vsce",
    "package",
    "--allow-missing-repository",
    "--no-update-package-json",
    "--target",
    target,
  ],
  extDir,
);

console.log(`\nDone. Install with:  code --install-extension vyrn-${target}-${readVersion(extDir)}.vsix`);
``