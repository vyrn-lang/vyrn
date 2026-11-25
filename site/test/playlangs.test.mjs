// A playground file shows the icon its `languages` entry names, as the
// editor reads that entry: the last whose glob matches, in the nearest vyrn.json.
import { test } from "node:test";
import assert from "node:assert/strict";
import { languageIcon } from "../public/play-langs.js";

const SVG = "<svg xmlns='http://www.w3.org/2000/svg'/>";
const project = (entries) =>
  new Map([
    ["vyrn.json", JSON.stringify({ name: "p", languages: entries })],
    ["icons/calc.svg", SVG],
    ["a.calc", "1 + 2"],
    ["sub/b.calc", "3"],
  ]);

test("the last matching entry's icon, relative to the manifest", () => {
  const files = project([
    { files: "*.calc", module: "./calc" },
    { files: "*.calc", module: "./calc", icon: "./icons/calc.svg" },
  ]);
  assert.equal(languageIcon(files, "a.calc"), SVG);
});

test("a glob names one directory level unless it says **", () => {
  const files = project([{ files: "*.calc", module: "./calc", icon: "icons/calc.svg" }]);
  assert.equal(languageIcon(files, "sub/b.calc"), null);
  const deep = project([{ files: "**/*.calc", module: "./calc", icon: "icons/calc.svg" }]);
  assert.equal(languageIcon(deep, "sub/b.calc"), SVG);
});

test("no icon, a missing svg, no manifest and bad JSON show the plain file", () => {
  assert.equal(languageIcon(project([{ files: "*.calc", module: "./calc" }]), "a.calc"), null);
  assert.equal(languageIcon(project([{ files: "*.calc", module: "./calc", icon: "none.svg" }]), "a.calc"), null);
  assert.equal(languageIcon(new Map([["a.calc", ""]]), "a.calc"), null);
  assert.equal(languageIcon(new Map([["vyrn.json", "{"]]), "a.calc"), null);
});

test("the nearest manifest governs", () => {
  const files = project([{ files: "**/*.calc", module: "./calc", icon: "icons/calc.svg" }]);
  files.set("sub/vyrn.json", JSON.stringify({ name: "s", languages: [] }));
  assert.equal(languageIcon(files, "sub/b.calc"), null);
});
