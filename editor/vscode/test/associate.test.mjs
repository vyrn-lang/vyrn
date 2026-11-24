// "Vyrn: associate project languages" writes one association per `languages`
// glob that names a module, spelled so VS Code matches it where the server does.
//
// Run: node --test editor/vscode/test/*.test.mjs   (no install needed)
import { test } from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const { languageAssociations } = require(fileURLToPath(new URL("../extension.js", import.meta.url)));

const manifest = (languages) => JSON.stringify({ name: "app", languages });

test("a root glob is a file-name pattern, a nested one a path under **", () => {
  const got = languageAssociations([
    { dir: "", text: manifest([{ files: "*.calc", module: "./calc" }, { files: "data/*.q", module: "./q" }]) },
    { dir: path.join("examples", "dsl"), text: manifest([{ files: "*.orders", module: "./orders" }]) },
  ]);
  assert.deepEqual(got, {
    "*.calc": "vyrn-dsl",
    "**/data/*.q": "vyrn-dsl",
    "**/examples/dsl/*.orders": "vyrn-dsl",
  });
});

test("an entry with no module, a manifest with no languages and bad JSON add nothing", () => {
  const got = languageAssociations([
    { dir: "", text: manifest([{ files: "*.txt", module: null }]) },
    { dir: "a", text: JSON.stringify({ name: "a" }) },
    { dir: "b", text: "{" },
  ]);
  assert.deepEqual(got, {});
});
