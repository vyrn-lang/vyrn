// The search index, weighed: at most 8,500 bytes gzipped, and never inlined in
// a document. It runs here because the export cannot gzip; the export asserts
// the raw size, and the two fail independently.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import path from "node:path";

const OUT = path.resolve(import.meta.dirname, "../../out");
const raw = await readFile(path.join(OUT, "search.json"), "utf8");
const rows = JSON.parse(raw);

test("the index is inside its ceiling, gzipped", () => {
  const gz = gzipSync(Buffer.from(raw), { level: 9 }).length;
  // The ceiling catches bloat (a runaway generator, an inlined document) and
  // leaves room for the index to grow with std.
  assert.ok(gz <= 8500, `search.json is ${gz} bytes gzipped (${raw.length} raw), ceiling 8,500`);
  // A floor too: an index that collapsed to nothing passes every ceiling.
  assert.ok(rows.length >= 440, `${rows.length} entries, and the census counted 448`);
});

test("every row has the four fields and points somewhere the site publishes", async () => {
  const pages = new Set();
  const walk = async (dir, base = "") => {
    for (const e of await readdir(dir, { withFileTypes: true })) {
      if (e.isDirectory()) await walk(path.join(dir, e.name), base + e.name + "/");
      else if (e.name.endsWith(".html")) pages.add(base + e.name);
    }
  };
  await walk(OUT);
  const sections = new Set();
  for (const r of rows) {
    assert.deepEqual(Object.keys(r).sort(), ["d", "s", "t", "u"]);
    assert.ok(r.t.length > 0, `a row with no title: ${JSON.stringify(r)}`);
    sections.add(r.s);
    // The URL is the route (a leading slash and `.html`, with an optional
    // fragment), and it must be a page the export wrote.
    const file = r.u.slice(1).split("#")[0];
    assert.ok(pages.has(file), `${r.u} is not a page this export wrote`);
  }
  assert.deepEqual([...sections].sort(), ["Docs", "Packages", "Reference", "Releases"]);
});

// An export row carries no fragment: `widgets.js` derives `#e-<name>` from its
// title, which keeps the index inside its ceiling. This checks the derived id
// against the published page, so a row whose target is gone fails.
test("every export row's derived anchor is an id the module page carries", async () => {
  const pages = new Map();
  let checked = 0;
  for (const r of rows) {
    const cut = r.s === "Reference" ? r.t.indexOf(" — std/") : -1;
    if (cut < 0) continue;
    assert.ok(!r.u.includes("#"), `${r.u} carries the fragment the overlay derives`);
    const file = r.u.slice(1);
    if (!pages.has(file)) pages.set(file, await readFile(path.join(OUT, file), "utf8"));
    const id = `id="e-${r.t.slice(0, cut)}"`;
    assert.ok(pages.get(file).includes(id), `${file} has no ${id}, so the row leads to the top of the page`);
    checked += 1;
  }
  // Floors, so a loop over nothing cannot pass.
  assert.ok(checked >= 350, `${checked} export rows checked, and the census counted 354`);
  assert.ok(pages.size >= 37, `${pages.size} module pages, and the census counted 37`);
});

test("the index is never inlined in a document, and the backstage is not in it", async () => {
  assert.ok(!raw.includes("/backstage"), "the backstage is in the consumer index");
  // A page that inlined even a slice of the index would contain its opening
  // bytes.
  const head = raw.slice(0, 60);
  for (const page of ["index.html", "docs.html", "guide.html", "play.html"]) {
    const html = await readFile(path.join(OUT, page), "utf8");
    assert.ok(!html.includes(head), `${page} inlines the search index`);
    // And every page carries the overlay it would open, with nothing in it.
    assert.ok(html.includes("data-find-input"), `${page} has no search overlay`);
    assert.ok(html.includes("<noscript>"), `${page} offers no fallback without script`);
  }
});
