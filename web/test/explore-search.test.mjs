// The three rules behind the explorer's search and the masthead's marker. Each
// one is silent when it breaks — a search that matches nothing still renders a
// list, a link built from what was typed still looks like a link, and a marker
// on the wrong row still marks something — so they are the three worth a test.
//
// Run: node --test web/test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { matchingExports } from "../../site/public/search.js";
import { navOwns } from "../../site/public/masthead.js";

const row = (names) => ({ dataset: { e: names } });

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
  // A row does not own a page whose name merely starts with its own.
  assert.equal(navOwns("/docs.html", "/docsomething.html"), false);
  assert.equal(navOwns("/explore.html", "/index.html"), false);
});

test("the Docs row speaks for the whole documentation", () => {
  // The row points at /guide.html and owns every area — the
  // reference, the Web shelf, the Tooling shelf — which is the export's
  // currentNav rule. The prefixes join the row's own stem, so a mount point
  // rides along.
  assert.equal(navOwns("/guide.html", "/docs/std/json.html"), true);
  assert.equal(navOwns("/guide.html", "/docs.html"), true);
  assert.equal(navOwns("/guide.html", "/web.html"), true);
  assert.equal(navOwns("/guide.html", "/web/views.html"), true);
  assert.equal(navOwns("/guide.html", "/tooling/editors.html"), true);
  assert.equal(navOwns("/vyrn/guide.html", "/vyrn/tooling/editors.html"), true);
  // Only the documentation: the registry and the playground stay their own.
  assert.equal(navOwns("/guide.html", "/explore.html"), false);
  assert.equal(navOwns("/guide.html", "/webbing.html"), false);
});
