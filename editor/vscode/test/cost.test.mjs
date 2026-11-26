// The cost rows the extension draws: which rendering it asks the server for, and how a
// `vyrn/costRows` reply maps to decorations.
//
// Run: node --test editor/vscode/test/cost.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const { costRendering, costDecorations } = require(
  fileURLToPath(new URL("../extension.js", import.meta.url))
);

const settings = (render) => ({ get: (_k, fallback) => render ?? fallback });

test("decorations are the default rendering", () => {
  assert.equal(costRendering(settings(undefined)), "decorations");
  assert.equal(costRendering(settings("decorations")), "decorations");
});

test("inlay leaves the cost hints to the server", () => {
  assert.equal(costRendering(settings("inlay")), "inlay");
});

const at = (line, a, b) => ({
  start: { line, character: a },
  end: { line, character: b },
});

test("no rows, no decorations", () => {
  assert.deepEqual(costDecorations([]), { glyphs: [], spans: [] });
});

test("each row is a glyph at its line, and each span carries the row's hover", () => {
  const rows = [
    { line: 72, glyph: "\u2197", count: 1, spans: [at(72, 4, 35)], hover: "grows" },
    { line: 72, glyph: "?", count: 1, spans: [at(72, 11, 34)], hover: "kept" },
    { line: 72, glyph: "\u21d2\u00b3", count: 3, spans: [at(72, 21, 28), at(72, 20, 33)], hover: "proved" },
    { line: 80, glyph: "\u00d73", count: 3, spans: [], hover: "run" },
  ];
  const { glyphs, spans } = costDecorations(rows);
  assert.deepEqual(glyphs, [
    { line: 72, glyph: "\u2197", hover: "grows" },
    { line: 72, glyph: "?", hover: "kept" },
    { line: 72, glyph: "\u21d2\u00b3", hover: "proved" },
    { line: 80, glyph: "\u00d73", hover: "run" },
  ]);
  assert.deepEqual(spans, [
    { range: at(72, 4, 35), hover: "grows" },
    { range: at(72, 11, 34), hover: "kept" },
    { range: at(72, 21, 28), hover: "proved" },
    { range: at(72, 20, 33), hover: "proved" },
  ]);
});
