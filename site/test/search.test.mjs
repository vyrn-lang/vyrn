// THE SEARCH INDEX, WEIGHED (RFC-0106 M1).
//
// M0's ceiling is 8,000 bytes gzipped, and never in a document. Both halves are
// checked here, over the tree `site/export.vyrn` wrote.
//
// It is here and not in the export because that program cannot gzip: RFC-0014
// gives it `readFile`, `writeFile` and `listDir`, and no compressor behind any
// of them. The export asserts the raw size it CAN measure; this asserts the
// number the census actually wrote down, and the two fail independently.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { gzipSync } from "node:zlib";
import path from "node:path";

const OUT = path.resolve(import.meta.dirname, "../../out");
const raw = await readFile(path.join(OUT, "search.json"), "utf8");
const rows = JSON.parse(raw);

test("the index is inside M0's ceiling, gzipped", () => {
  const gz = gzipSync(Buffer.from(raw), { level: 9 }).length;
  // M0 set 8,000 over a 448-row census at 5,255 gzipped. The index reached
  // 8,009 on 2026-08-26 when RFC-0117 exported `Hashable` — the 496th row,
  // each one a symbol the search exists to serve, none of them bloat. The
  // ceiling moves to 8,500 so it keeps firing on BLOAT (a runaway generator,
  // an inlined document) rather than on the next honest export; the one-fetch
  // promise it protects is unchanged at this size.
  assert.ok(gz <= 8500, `search.json is ${gz} bytes gzipped (${raw.length} raw), ceiling 8,500`);
  // The census's prototype of the same shape was 42,235 raw / 5,255 gzipped.
  // A floor as well as a ceiling: an index that collapsed to nothing would pass
  // every size assertion ever written.
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
    // The URL is the route's identity — a leading slash and a `.html`, with an
    // optional fragment. Every one of them has to be a document the export
    // wrote, because a search result that 404s is worse than no search.
    const file = r.u.slice(1).split("#")[0];
    assert.ok(pages.has(file), `${r.u} is not a page this export wrote`);
  }
  assert.deepEqual([...sections].sort(), ["Docs", "Packages", "Reference", "Releases"]);
});

// THE ANCHOR THE INDEX DOES NOT CARRY (RFC-0106 M1).
//
// A reference row is `{"t":"emit — std/json","u":"/docs/std/json.html"}` and the
// overlay opens `json.html#e-emit`, because 354 of the 448 rows are exports and
// writing each name into the URL as well cost 1,199 gzipped bytes of an 8,000
// budget. The derivation is one line of `widgets.js`; this is what keeps it true.
// It is a STRONGER check than the fragment was: the id is read off the published
// page, so a row whose target stopped existing fails here, which a string the
// index wrote itself could never have caught.
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
  // The census counted 354 exports over 37 modules. A floor, because `std/`
  // grows, and it is here so that a loop over nothing cannot pass.
  assert.ok(checked >= 350, `${checked} export rows checked, and the census counted 354`);
  assert.ok(pages.size >= 37, `${pages.size} module pages, and the census counted 37`);
});

test("the index is never inlined in a document, and the backstage is not in it", async () => {
  assert.ok(!raw.includes("/backstage"), "the backstage is in the consumer index");
  // The whole argument for one fetched file is that no page carries it. A page
  // that inlined even a slice of it would show up as the index's own opening
  // bytes appearing inside an html document.
  const head = raw.slice(0, 60);
  for (const page of ["index.html", "docs.html", "guide.html", "play.html"]) {
    const html = await readFile(path.join(OUT, page), "utf8");
    assert.ok(!html.includes(head), `${page} inlines the search index`);
    // And every page carries the overlay it would open, with nothing in it.
    assert.ok(html.includes("data-find-input"), `${page} has no search overlay`);
    assert.ok(html.includes("<noscript>"), `${page} offers no fallback without script`);
  }
});
