// A byte literal's scope is `constant.character`, which VS Code reads as code, so the `{`
// in `'{'` would count as a bracket and shift every bracket colour after it. Each grammar
// entry that holds Vyrn maps that scope to the string token type, which bracket matching
// skips. A written grammar cannot state a fact of `package.json`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const editor = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (file) => readFile(path.join(editor, file), "utf8");
const scope = "constant.character.vyrn";

test("every grammar entry that holds Vyrn treats a byte literal as a string", async () => {
  assert.ok((await read("vyrn.tmLanguage.json")).includes(`"${scope}"`));
  const pkg = JSON.parse(await read("package.json"));
  for (const entry of pkg.contributes.grammars) {
    if ((await read(entry.path)).includes("source.vyrn")) {
      assert.equal(entry.tokenTypes?.[scope], "string", entry.language);
    }
  }
});
