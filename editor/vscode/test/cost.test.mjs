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
  assert.deepEqual(costDecorations([]), []);
});

test("each anchor is a glyph with its own hover", () => {
  const pos = (line, character) => ({ line, character });
  const rows = [
    { anchors: [{ at: pos(72, 35), glyph: "↗", hover: "grows" }], spans: [at(72, 4, 35)] },
    {
      anchors: [
        { at: pos(72, 28), glyph: "⇒", hover: "proved a" },
        { at: pos(72, 33), glyph: "⇒", hover: "proved b" },
      ],
      spans: [at(72, 21, 28), at(72, 20, 33)],
    },
    { anchors: [{ at: pos(80, 12), glyph: "×3", hover: "run" }], spans: [] },
  ];
  assert.deepEqual(costDecorations(rows), [
    { at: pos(72, 35), glyph: "↗", hover: "grows" },
    { at: pos(72, 28), glyph: "⇒", hover: "proved a" },
    { at: pos(72, 33), glyph: "⇒", hover: "proved b" },
    { at: pos(80, 12), glyph: "×3", hover: "run" },
  ]);
});
