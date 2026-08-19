// The extension finds the server the toolchain installed (RFC-0105 M3).
//
// The second rung of the resolution order is the one with logic in it: a
// release archive carries `vyrn-lsp` beside `vyrn`, the install scripts move
// both into `$DIR/bin`, and the extension has to find the server from a PATH
// that may name either binary — through a symlink, on a machine where neither
// is present. That is a loop over `PATH` and a `realpath`, which is exactly the
// kind of code that quietly stops working on one platform.
//
// The helpers are imported from `extension.js` itself, not copied here: a test
// against a second implementation of a rule tests the second implementation.
//
// Run: node --test editor/vscode/test/*.test.mjs   (no install needed)
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, symlink, rm, realpath } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const { onPath, beside } = require(
  fileURLToPath(new URL("../extension.js", import.meta.url))
);

const win = process.platform === "win32";
const VYRN = win ? "vyrn.exe" : "vyrn";
const LSP = win ? "vyrn-lsp.exe" : "vyrn-lsp";

/// A directory tree, and PATH set to `dirs` for the duration of `body`.
async function withPath(dirs, body) {
  const saved = process.env.PATH;
  process.env.PATH = dirs.join(path.delimiter);
  try {
    return await body();
  } finally {
    process.env.PATH = saved;
  }
}

async function fixture() {
  // macOS spells `tmpdir()` as `/var/…`, itself a symlink to `/private/var/…`.
  // The resolver answers in realpath on purpose, so the expectation has to be
  // spelled there too, or the assertion compares two names for one file.
  const root = await realpath(await mkdtemp(path.join(tmpdir(), "vyrn-resolve-")));
  const bin = path.join(root, "bin");
  await mkdir(bin, { recursive: true });
  await writeFile(path.join(bin, VYRN), "");
  await writeFile(path.join(bin, LSP), "");
  return { root, bin };
}

test("an installed toolchain on PATH gives the server directly", async () => {
  const { root, bin } = await fixture();
  await withPath([bin], async () => {
    assert.equal(onPath(LSP), path.join(bin, LSP));
  });
  await rm(root, { recursive: true, force: true });
});

test("a PATH with no vyrn on it resolves to nothing, rather than to a guess", async () => {
  const { root } = await fixture();
  const empty = path.join(root, "empty");
  await mkdir(empty, { recursive: true });
  await withPath([empty, "", path.join(root, "does-not-exist")], async () => {
    assert.equal(onPath(LSP), null);
    assert.equal(onPath(VYRN), null);
    assert.equal(beside(null, LSP), null);
  });
  await rm(root, { recursive: true, force: true });
});

test("the server beside the vyrn that is on PATH", async () => {
  const { root, bin } = await fixture();
  await withPath([bin], async () => {
    assert.equal(beside(onPath(VYRN), LSP), path.join(bin, LSP));
  });
  await rm(root, { recursive: true, force: true });
});

test("a vyrn that is a symlink resolves to the directory the server is in", async (t) => {
  const { root, bin } = await fixture();
  const shim = path.join(root, "shim");
  await mkdir(shim, { recursive: true });
  try {
    await symlink(path.join(bin, VYRN), path.join(shim, VYRN));
  } catch (e) {
    // Windows without Developer Mode refuses an unprivileged symlink. The rule
    // being checked is `realpathSync`'s, which is the same one there.
    await rm(root, { recursive: true, force: true });
    return t.skip(`this machine cannot create a symlink: ${e.code}`);
  }
  await withPath([shim], async () => {
    // The shim directory holds no server, so a resolution that did not follow
    // the link would answer null here.
    assert.equal(onPath(LSP), null);
    assert.equal(beside(onPath(VYRN), LSP), path.join(bin, LSP));
  });
  await rm(root, { recursive: true, force: true });
});

test("the first PATH entry that has it wins", async () => {
  const { root, bin } = await fixture();
  const second = path.join(root, "second");
  await mkdir(second, { recursive: true });
  await writeFile(path.join(second, LSP), "");
  await withPath([bin, second], async () => {
    assert.equal(onPath(LSP), path.join(bin, LSP));
  });
  await rm(root, { recursive: true, force: true });
});
