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
  assert.ok(gz <= 9650, `search.json is ${gz} bytes gzipped (${raw.length} raw), ceiling 9,650`);
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
    const file = r.u.slice(1).split("#")[0];
    assert.ok(pages.has(file), `${r.u} is not a page this export wrote`);
  }
  assert.deepEqual([...sections].sort(), ["Docs", "Packages", "Reference", "Releases"]);
});

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
  assert.ok(checked >= 350, `${checked} export rows checked, and the census counted 354`);
  assert.ok(pages.size >= 37, `${pages.size} module pages, and the census counted 37`);
});

test("the index is never inlined in a document, and the backstage is not in it", async () => {
  assert.ok(!raw.includes("/backstage"), "the backstage is in the consumer index");
  const head = raw.slice(0, 60);
  for (const page of ["index.html", "docs.html", "guide.html", "play.html"]) {
    const html = await readFile(path.join(OUT, page), "utf8");
    assert.ok(!html.includes(head), `${page} inlines the search index`);
    assert.ok(html.includes("data-find-input"), `${page} has no search overlay`);
    assert.ok(html.includes("<noscript>"), `${page} offers no fallback without script`);
  }
});
