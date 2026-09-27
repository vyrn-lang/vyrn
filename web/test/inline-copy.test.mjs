// The two rules behind copy-on-click for an inline code span. Both are silent
// when they break — a copy that carries the markup's indentation still copies,
// and a copy that eats text selection still copies — so they are the two things
// worth a test.
//
// `site/public/widgets.js` is a browser module: it imports its siblings and
// derives the site's base from `import.meta.url`, neither of which a plain
// script can do. So the file is evaluated here as a script in a stubbed context,
// with the imports dropped and `import.meta.url` — a syntax error outside a
// module — standing in as a literal. Nothing is copied into this file: rename
// either rule, or change what it answers, and these tests fail.
//
// Run: node --test web/test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

const src = await readFile(new URL("../../site/public/widgets.js", import.meta.url), "utf8");
const script = src
  .replace(/^import .*$/gm, "")
  .replace(/^import\(.*$/gm, "")
  .replace(/import\.meta\.url/g, JSON.stringify("https://host.test/base/widgets.js"));

const nothing = () => null;
const element = () => ({
  setAttribute: nothing,
  appendChild: nothing,
  addEventListener: nothing,
  classList: { add: nothing, remove: nothing },
  style: {},
});
const ctx = {
  window: {},
  URL,
  location: { pathname: "/install.html", href: "https://host.test/base/install.html" },
  matchMedia: () => ({ matches: false }),
  addEventListener: nothing,
  document: {
    addEventListener: nothing,
    querySelector: nothing,
    querySelectorAll: () => [],
    getElementById: nothing,
    createElement: element,
    body: element(),
  },
  // The two imports the strip above removed. `boot()` calls the second one.
  mountHero: nothing,
  refreshRelease: nothing,
};
vm.createContext(ctx);
vm.runInContext(script, ctx);
const { codeText, isPlainCopyClick } = ctx;

const span = (text) => ({ textContent: text });

test("a span copies what the reader sees, not what the markup holds", () => {
  assert.equal(codeText(span("  cargo build  ")), "cargo build");
  // An inline box renders a newline and a run of indentation as one space, and
  // the copy says the same thing the box does.
  assert.equal(codeText(span("vyrn test\n            examples/fib.vyrn")), "vyrn test examples/fib.vyrn");
  assert.equal(codeText(span("$PATH")), "$PATH");
});

const at = (x, y) => ({ x, y });
const up = (x, y, detail = 1) => ({ x, y, detail });

test("a click that did not move copies", () => {
  assert.equal(isPlainCopyClick(at(100, 40), up(100, 40), false), true);
  // A hand is not a robot: two pixels of travel is still a click.
  assert.equal(isPlainCopyClick(at(100, 40), up(102, 41), false), true);
});

test("a drag selects and does not copy", () => {
  assert.equal(isPlainCopyClick(at(100, 40), up(160, 40), true), false);
  // Even with the selection already gone, the travel alone settles it.
  assert.equal(isPlainCopyClick(at(100, 40), up(160, 40), false), false);
  assert.equal(isPlainCopyClick(at(100, 40), up(100, 60), false), false);
});

test("the second click of a double-click selects a word and does not copy", () => {
  assert.equal(isPlainCopyClick(at(100, 40), up(100, 40, 2), false), false);
  assert.equal(isPlainCopyClick(at(100, 40), up(100, 40, 3), false), false);
});

test("a click while something is selected leaves the selection alone", () => {
  assert.equal(isPlainCopyClick(at(100, 40), up(100, 40), true), false);
});

test("a keyboard activation has no pointer, and copies", () => {
  assert.equal(isPlainCopyClick(null, up(0, 0, 0), false), true);
});
