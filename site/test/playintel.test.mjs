import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { instantiatePlay } from "../public/play-wasm.js";
import { lineCol, prefixAt, rank } from "../public/play-intel.js";
import { sheet } from "./sheet.mjs";

const root = new URL("../../", import.meta.url);
const intelJs = await readFile(fileURLToPath(new URL("site/public/play-intel.js", root)), "utf8");
const play = await instantiatePlay(await readFile(fileURLToPath(new URL("out/play.wasm", root))));
const labels = (answer) => answer.items.map((i) => i.label);

const SPLIT = 'import { split } from "std/strings"\n\nfn main() -> Int64 {\n    let parts = split("a,b", ",")\n    return 0\n}\n';

test("a caret maps to a 1-based line and a column in code points", () => {
  assert.deepEqual(lineCol("ab\n\u{1F600}c", 6), { line: 2, col: 3 });
  assert.deepEqual(lineCol("", 0), { line: 1, col: 1 });
});

test("the prefix is the identifier run before the caret", () => {
  assert.deepEqual(prefixAt("xs.pu", 5), { start: 3, prefix: "pu" });
  assert.deepEqual(prefixAt("xs.", 3), { start: 3, prefix: "" });
});

test("prefix matches rank before substring matches, once per label", () => {
  const items = ["popAll", "push", "unshift", "push", "map"].map((label) => ({ label }));
  assert.deepEqual(rank(items, "pu").map((i) => i.label), ["push"]);
  assert.deepEqual(rank(items, "sh").map((i) => i.label), ["push", "unshift"]);
  assert.deepEqual(rank(items, "").length, 4);
});

test("typing a dot after an array offers the array's members", () => {
  const src = "fn main() -> Int64 {\n    let xs = [1,2]; xs.\n    return 0\n}\n";
  const got = labels(play.complete(src, 2, 24));
  for (const m of ["at", "length", "copy"]) assert.ok(got.includes(m), `${m} in ${got}`);
});

test("a name offers the locals, the imports and the keywords", () => {
  const got = labels(play.complete(SPLIT, 5, 5));
  for (const m of ["parts", "split", "return"]) assert.ok(got.includes(m), `${m} in ${got}`);
});

test("a floating box's classes are styled only under .intel, so no shell rule reaches it", () => {
  const classes = new Set([...intelJs.matchAll(/floating\("([^"]+)"\)/g)].flatMap((m) => m[1].split(" ")));
  assert.ok(classes.has("intel") && classes.size > 1, [...classes].join(" "));
  const css = sheet.replace(/\/\*[\s\S]*?\*\//g, "");
  const leaks = [];
  for (const [, selectors] of css.matchAll(/([^{}]+)\{[^{}]*\}/g)) {
    for (const sel of selectors.split(",")) {
      for (const c of classes) {
        if (c !== "intel" && new RegExp(`\\.${c}(?![\\w-])`).test(sel) && !/\.intel(?![\w-])/.test(sel)) leaks.push(`${sel.trim()} styles .${c}`);
      }
    }
  }
  assert.deepEqual(leaks, []);
});

test("a fixed-size array is not offered push", () => {
  const src = "fn main() -> Int64 {\n    let xs = [1, 2]; xs.\n    return 0\n}\n";
  const got = labels(play.complete(src, 2, 25));
  assert.ok(got.includes("at") && !got.includes("push"), got.join(" "));
});
