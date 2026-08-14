// The three rules behind the explorer's search and the masthead's marker. Each
// one is silent when it breaks — a search that matches nothing still renders a
// list, a link built from what was typed still looks like a link, and a marker
// on the wrong row still marks something — so they are the three worth a test.
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
  location: { pathname: "/guide/values.html", href: "https://host.test/base/guide/values.html" },
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
  mountHero: nothing,
  refreshRelease: nothing,
};
vm.createContext(ctx);
vm.runInContext(script, ctx);
const { matchingExports, navOwns } = ctx;

const row = (names) => ({ dataset: { e: names } });

// The script runs in its own realm, so its arrays are not this realm's arrays
// and `deepStrictEqual` refuses them on identity alone. What the rule answers is
// a list of names, so the names are what is compared.
const hits = (r, q, limit) => matchingExports(r, q, limit).join(" ");

test("a search over a row's exports matches on any part of a name", () => {
  const strings = row("joinWith substring indexOf split toLower");
  assert.equal(hits(strings, "join", 10), "joinWith");
  // The haystack is the export name and the needle is already lowercased by the
  // caller, so a lowercase query still finds a camelCase name.
  assert.equal(hits(strings, "tolower", 10), "toLower");
  assert.equal(hits(strings, "in", 10), "joinWith substring indexOf");
  assert.equal(hits(strings, "zzz", 10), "");
  assert.equal(hits(row(""), "in", 10), "");
});

test("a row shows a bounded number of matches, not all forty", () => {
  const many = row("a1 a2 a3 a4 a5");
  assert.equal(hits(many, "a", 3), "a1 a2 a3");
});

test("a navigation row owns the subtree named after it", () => {
  assert.equal(navOwns("/docs.html", "/docs.html"), true);
  assert.equal(navOwns("/docs.html", "/docs/std/json.html"), true);
  assert.equal(navOwns("/guide.html", "/guide/values.html"), true);
  assert.equal(navOwns("/guide.html", "/docs/std/json.html"), false);
  // A row does not own a page whose name merely starts with its own.
  assert.equal(navOwns("/docs.html", "/docsomething.html"), false);
  assert.equal(navOwns("/explore.html", "/index.html"), false);
});
