// The site, served from somewhere that is not the root.
//
// The defect this exists for shipped green: the export asserted that every
// internal link resolved, and asserted it at a root, where `/style.css` is
// correct. GitHub Pages serves this site from `/vyrn/`, where `/style.css` is a
// 404 — and so were both scripts, both wasm modules, the icon and every
// navigation link. A gate that only holds at the root would leave that one edit
// away from coming back.
//
// So this one does not read the HTML and reason about it. It starts a file
// server with a PREFIX in front of the tree, and fetches: every page, then every
// URL every page names, resolved the way a browser resolves it. Three prefixes,
// one of them nested, and the same artifact each time — nothing is rebuilt
// between them, because a build that has to know where it will be served is the
// thing being ruled out.
//
// Run: node --test site/test/*.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir, access } from "node:fs/promises";
import { createServer } from "node:http";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";

const OUT = fileURLToPath(new URL("../../out/", import.meta.url));
const PUBLIC = fileURLToPath(new URL("../public/", import.meta.url));

// The three mount points. The root is where the site used to work, `/vyrn/` is
// where it is actually published, and `/a/b/` is deeper than any page in the
// tree — so a prefix that is accidentally treated as one segment fails here.
const PREFIXES = ["", "/vyrn", "/a/b"];

const TYPES = { ".html": "text/html", ".css": "text/css", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".svg": "image/svg+xml", ".md": "text/markdown", ".xml": "application/rss+xml" };

async function tree(dir, base = "") {
  const out = [];
  for (const e of await readdir(dir, { withFileTypes: true })) {
    const rel = base + e.name;
    if (e.isDirectory()) out.push(...(await tree(path.join(dir, e.name), rel + "/")));
    else out.push(rel);
  }
  return out;
}

const files = await tree(OUT).catch(() => []);
const pages = files.filter((f) => f.endsWith(".html"));

// The export must have run. A gate that quietly passes on an empty directory is
// the gate that let this defect out in the first place.
test("the export is there to check", () => {
  // 59 consumer pages, and one per design record plus the backstage index
  // (RFC-0105 M1). A floor rather than a count: both halves grow by a file.
  assert.ok(pages.length >= 160, `expected the exported tree in out/, found ${pages.length} page(s) — run: vyrn run site/export.vyrn out`);
  assert.ok(pages.includes("backstage.html"), "the backstage index is missing");
  assert.ok(pages.filter((p) => p.startsWith("backstage/rfcs/")).length > 100, "the design record is missing");
  for (const f of ["style.css", "widgets.js", "vyrn-nav.js", "favicon.svg", "hero.wasm", "play.wasm", "play-worker.js"])
    assert.ok(files.includes(f), `out/${f} is missing`);
});

/// Every value of one attribute in `html`. The leading space keeps `data-src="`
/// from reading as `src="`.
const attr = (html, name) => [...html.matchAll(new RegExp(` ${name}="([^"]*)"`, "g"))].map((m) => m[1]);

const source = new Map(await Promise.all(pages.map(async (p) => [p, await readFile(path.join(OUT, p), "utf8")])));
const idsOf = new Map([...source].map(([p, html]) => [p, new Set(attr(html, "id"))]));

function serve(prefix) {
  const server = createServer(async (req, res) => {
    const url = new URL(req.url, "http://x");
    // Outside the mount point there is nothing at all — which is exactly what
    // GitHub Pages answers for `/style.css` when the site lives at `/vyrn/`.
    if (!url.pathname.startsWith(prefix + "/")) return res.writeHead(404).end();
    const rel = decodeURIComponent(url.pathname.slice(prefix.length + 1)) || "index.html";
    try {
      const body = await readFile(path.join(OUT, rel));
      res.writeHead(200, { "content-type": TYPES[path.extname(rel)] || "application/octet-stream" }).end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  return new Promise((ok) => server.listen(0, "127.0.0.1", () => ok({ server, base: `http://127.0.0.1:${server.address().port}${prefix}` })));
}

for (const prefix of PREFIXES) {
  test(`every page, asset and fragment resolves under ${prefix || "the root"}`, async () => {
    const { server, base } = await serve(prefix);
    try {
      const wanted = new Map(); // resolved URL -> the page that named it
      let links = 0;
      let fragments = 0;

      for (const page of pages) {
        const at = `${base}/${page}`;
        const res = await fetch(at);
        assert.equal(res.status, 200, `${page} itself did not load at ${at}`);
        const html = await res.text();

        // The soft-navigation payload beside it. `vyrn-nav.js` builds this URL
        // from the one in the address bar, so it lands in the mount by
        // construction; what it CARRIES is prose HTML, and that has to be
        // relative to the same page or a soft navigation renders dead links
        // where a full load renders live ones.
        //
        // A backstage page has NO payload, and that is checked rather than
        // skipped: the section loads no script, every link into and out of it is
        // a full load, and a payload would be a second copy of a whole rendered
        // design record that nothing would ever fetch (RFC-0105 M1).
        const payload = await fetch(`${base}/${page.slice(0, -5)}.data.json`);
        if (page.startsWith("backstage")) {
          assert.equal(payload.status, 404, `${page}: the backstage publishes no payload`);
        } else {
          assert.equal(payload.status, 200, `${page}: no payload beside it`);
          assert.doesNotMatch(await payload.text(), / (?:href|src)=\\"\//, `${page}: its payload names a root`);
        }

        for (const name of ["href", "src"]) {
          for (const value of attr(html, name)) {
            const url = new URL(value, at);
            if (url.protocol !== "http:" && url.protocol !== "https:") continue;
            if (url.origin !== new URL(base).origin) continue; // github.com and friends
            links++;
            // The whole defect, in one assertion: a URL that leaves the mount
            // point is a URL that named a root only one host has.
            assert.ok(url.pathname.startsWith(prefix + "/"), `${page}: ${name}="${value}" escapes the mount point (${url.pathname})`);
            const target = url.pathname.slice(prefix.length + 1);
            wanted.set(url.origin + url.pathname, page);
            if (url.hash) {
              fragments++;
              const frag = decodeURIComponent(url.hash.slice(1));
              const ids = idsOf.get(target || "index.html");
              assert.ok(ids, `${page}: ${name}="${value}" names a page this export did not write`);
              // The one fragment that is not an anchor: the playground's
              // contract is `play.html#c=` and base64url of the program.
              if (target === "play.html" && frag.startsWith("c=")) assert.match(frag, /^c=[A-Za-z0-9_-]+$/, `${page}: a playground link that is not base64url`);
              else assert.ok(ids.has(frag), `${page}: ${name}="${value}" points at an element that is not there`);
            }
          }
        }
      }

      // The scripts fetch three things no page mentions, each one relative to
      // the script's own URL. They are checked where the scripts would ask.
      for (const asset of ["hero.wasm", "play.wasm", "play-worker.js"]) wanted.set(`${base}/${asset}`, "the site's scripts");

      for (const [url, named] of wanted) {
        const res = await fetch(url);
        assert.equal(res.status, 200, `${url} is a 404, named by ${named}`);
      }

      // Floors, so a scan that quietly stopped scanning fails instead of
      // passing. The tree holds 2170 internal links and 907 fragments today,
      // and grows by a page every time `std/` or the book does.
      assert.ok(links > 2000, `only ${links} internal links checked`);
      assert.ok(fragments > 800, `only ${fragments} fragments checked`);
      assert.ok(wanted.size > 60, `only ${wanted.size} distinct URLs fetched`);
    } finally {
      server.close();
    }
  });
}

test("a page opened from disk finds everything it names", async () => {
  // The fourth mount point, and the one no server can fake: `file://`, where
  // there is no origin and no root to be absolute about. Same artifact.
  let checked = 0;
  for (const page of pages) {
    const at = pathToFileURL(path.join(OUT, page));
    for (const name of ["href", "src"]) {
      for (const value of attr(source.get(page), name)) {
        if (/^[a-z]+:/i.test(value) || value.startsWith("#")) continue;
        const url = new URL(value, at);
        await access(fileURLToPath(url.href.split("#")[0]));
        checked++;
      }
    }
  }
  // 1369 today: every internal link except the ones that name no document at
  // all, which are the same-page fragments the HTTP runs above check.
  assert.ok(checked > 1300, `only ${checked} links checked on disk`);
});

test("no browser module names a root", async () => {
  // The other half, and the one the fetches above cannot see: a script that
  // hard-codes `/play.wasm` still loads at the root and 404s under a prefix. A
  // module specifier or a fetch target here is relative to the module that
  // wrote it, always.
  for (const name of await readdir(PUBLIC)) {
    if (!name.endsWith(".js")) continue;
    const src = await readFile(path.join(PUBLIC, name), "utf8");
    const bad = [...src.matchAll(/(?:from|import|fetch|Worker|loadPlay)\s*\(?\s*"(\/[^/][^"]*)"/g)].map((m) => m[1]);
    assert.deepEqual(bad, [], `site/public/${name} names ${bad.join(", ")} from the root`);
  }
});
