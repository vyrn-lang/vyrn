// The site served from a path prefix, as GitHub Pages serves it under `/vyrn/`.
//
// A root-relative URL works at a domain root and 404s under a prefix. This
// starts a file server with a prefix in front of `out/` and fetches every page
// and every URL each page names, resolved as a browser resolves it. Three
// prefixes, one nested, over one artifact: a build that has to know where it
// is served is what this rules out.
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

// The mount points: the root, `/vyrn/` where the site is published, and
// `/a/b/`, deeper than any page, so a prefix treated as one segment fails.
const PREFIXES = ["", "/vyrn", "/a/b"];

const TYPES = { ".html": "text/html", ".css": "text/css", ".js": "text/javascript", ".json": "application/json", ".wasm": "application/wasm", ".svg": "image/svg+xml", ".md": "text/markdown", ".xml": "application/rss+xml", ".txt": "text/plain", ".sh": "application/x-sh", ".ps1": "application/octet-stream" };

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

// The export must have run: a gate that passes on an empty directory checks
// nothing.
test("the export is there to check", () => {
  // A floor, because the consumer site grows by pages.
  assert.ok(pages.length >= 70, `expected the exported tree in out/, found ${pages.length} page(s) — run: vyrn run site/export.vyrn out`);
  // The backstage is not part of the published site.
  assert.ok(!pages.includes("backstage.html"), "the backstage was removed and should not be exported");
  for (const f of ["style.css", "widgets.js", "vyrn-nav.js", "favicon.svg", "play.wasm", "play-worker.js"])
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
    // Outside the mount point there is nothing at all, which is exactly what
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

        // The soft-navigation payload beside it. `vyrn-nav.js` builds its URL
        // from the address bar, so it lands in the mount; the prose HTML it
        // carries must be relative to the same page, or a soft navigation
        // renders dead links.
        const payload = await fetch(`${base}/${page.slice(0, -5)}.data.json`);
        assert.equal(payload.status, 200, `${page}: no payload beside it`);
        assert.doesNotMatch(await payload.text(), / (?:href|src)=\\"\//, `${page}: its payload names a root`);

        for (const name of ["href", "src"]) {
          for (const value of attr(html, name)) {
            const url = new URL(value, at);
            if (url.protocol !== "http:" && url.protocol !== "https:") continue;
            if (url.origin !== new URL(base).origin) continue; // github.com and friends
            links++;
            // A URL that leaves the mount point names a root only one host has.
            assert.ok(url.pathname.startsWith(prefix + "/"), `${page}: ${name}="${value}" escapes the mount point (${url.pathname})`);
            const target = url.pathname.slice(prefix.length + 1);
            wanted.set(url.origin + url.pathname, page);
            if (url.hash) {
              fragments++;
              const frag = decodeURIComponent(url.hash.slice(1));
              const ids = idsOf.get(target || "index.html");
              assert.ok(ids, `${page}: ${name}="${value}" names a page this export did not write`);
              // The playground's link contract is `play.html#c=<base64url>`, not
              // an anchor.
              if (target === "play.html" && frag.startsWith("c=")) assert.match(frag, /^c=[A-Za-z0-9_-]+$/, `${page}: a playground link that is not base64url`);
              else assert.ok(ids.has(frag), `${page}: ${name}="${value}" points at an element that is not there`);
            }
          }
        }
      }

      // The scripts fetch two files no page names, each relative to the
      // script's own URL.
      for (const asset of ["play.wasm", "play-worker.js"]) wanted.set(`${base}/${asset}`, "the site's scripts");

      for (const [url, named] of wanted) {
        const res = await fetch(url);
        assert.equal(res.status, 200, `${url} is a 404, named by ${named}`);
      }

      // Floors, so a scan that stopped scanning fails. The tree held 2170
      // internal links and 907 fragments when these were set.
      assert.ok(links > 2000, `only ${links} internal links checked`);
      assert.ok(fragments > 800, `only ${fragments} fragments checked`);
      assert.ok(wanted.size > 60, `only ${wanted.size} distinct URLs fetched`);
    } finally {
      server.close();
    }
  });
}

test("a page opened from disk finds everything it names", async () => {
  // The fourth mount point, which no server can fake: `file://`, with no
  // origin and no root.
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
  // Every internal link except same-page fragments, which the HTTP runs check.
  assert.ok(checked > 1300, `only ${checked} links checked on disk`);
});

test("no browser module names a root", async () => {
  // A script that hard-codes `/play.wasm` loads at the root and 404s under a
  // prefix, which the fetches above cannot see. A module specifier or a fetch
  // target here is relative to the module that wrote it.
  for (const name of await readdir(PUBLIC)) {
    if (!name.endsWith(".js")) continue;
    const src = await readFile(path.join(PUBLIC, name), "utf8");
    const bad = [...src.matchAll(/(?:from|import|fetch|Worker|loadPlay)\s*\(?\s*"(\/[^/][^"]*)"/g)].map((m) => m[1]);
    assert.deepEqual(bad, [], `site/public/${name} names ${bad.join(", ")} from the root`);
  }
});
