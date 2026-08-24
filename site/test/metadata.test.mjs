// WHAT A LINK TO THIS SITE SHOWS WHEN IT IS PASTED SOMEWHERE (RFC-0106 M6).
//
// The defect this exists for: 67 of the 80 published documents carried the same
// `<meta name="description">` — one paragraph about the language — so a link to
// `std/json`, a link to `/guide/values` and a link to `examples/shelf` previewed
// as the same page in a chat, in a search result and on a social site.
//
// It is checked over the EXPORTED TREE and not over the sources, because the
// export is what publishes, and a per-route claim that nothing reads the route's
// own file is a claim that rots. Every assertion below is about a property of
// the whole set — every page has it, no two pages share it — which is the only
// shape that catches a description going back to being one string.
//
// Run: node --test site/test/metadata.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir, stat } from "node:fs/promises";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "../..");
const OUT = path.join(ROOT, "out");

// The origin is read out of the module that declares it. A test that typed it
// would pass on the day the site moved and lie on every day after.
const repo = await readFile(path.join(ROOT, "site/app/repo.vyrn"), "utf8");
const ORIGIN = (/export fn siteOrigin\(\) -> String \{\s*return "([^"]+)"/.exec(repo) || [])[1];

async function tree(dir, base = "") {
  const out = [];
  for (const e of await readdir(path.join(dir, base), { withFileTypes: true })) {
    if (e.isDirectory()) out.push(...await tree(dir, `${base}${e.name}/`));
    else out.push(`${base}${e.name}`);
  }
  return out;
}

const files = await tree(OUT).catch(() => []);
const html = files.filter((f) => f.endsWith(".html"));
const source = new Map(await Promise.all(html.map(async (f) => [f, await readFile(path.join(OUT, f), "utf8")])));

// A redirect stub is the old name of a page and not a place: it carries a
// refresh, a canonical to its replacement and nothing else. The export gives it
// no card, so it is not a route here either.
const routes = [...source].filter(([, doc]) => !doc.includes('http-equiv="refresh"'));

// The head only. The benchmarks page carries 39 SVG `<title>` elements and the
// reference pages quote `<meta>` in prose, and neither is metadata.
const head = (doc) => doc.slice(0, doc.indexOf("</head>"));
const meta = (doc, key) => {
  const attr = key.includes(":") && !key.startsWith("twitter") ? "property" : "name";
  const re = new RegExp(`<meta ${attr}="${key}" content="([^"]*)"`);
  return (re.exec(head(doc)) || [])[1];
};
const title = (doc) => (/<title>([^<]*)<\/title>/.exec(head(doc)) || [])[1];
const canonical = (doc) => (/<link rel="canonical" href="([^"]*)"/.exec(head(doc)) || [])[1];

// `index.html` is `/`, and `guide/values.html` is `/guide/values` — the inverse
// of `published` in `site/app/nav.vyrn`.
const routeOf = (file) => (file === "index.html" ? "/" : "/" + file.replace(/\.html$/, ""));

test("the export is there to check", () => {
  assert.ok(ORIGIN, "site/app/repo.vyrn no longer declares siteOrigin as a literal");
  assert.ok(routes.length > 60, `only ${routes.length} routes in out/ — run \`vyrn run site/export.vyrn out\` first`);
});

test("every route emits every tag a shared link is rendered from", () => {
  const required = ["description", "og:site_name", "og:type", "og:title", "og:description", "og:url", "twitter:card", "twitter:title", "twitter:description"];
  for (const [file, doc] of routes) {
    for (const key of required) {
      const value = meta(doc, key);
      assert.ok(value && value.trim().length > 0, `${file} has no ${key}`);
    }
    assert.ok(canonical(doc), `${file} has no canonical URL`);
    assert.ok(title(doc), `${file} has no title`);
    // The card says what the page says. Two strings that can differ do.
    assert.equal(meta(doc, "og:description"), meta(doc, "description"), `${file}: card and description disagree`);
    assert.equal(meta(doc, "twitter:description"), meta(doc, "og:description"), `${file}: two cards disagree`);
    assert.equal(meta(doc, "twitter:title"), meta(doc, "og:title"), `${file}: two cards disagree`);
    assert.equal(meta(doc, "og:url"), canonical(doc), `${file}: og:url is not the canonical URL`);
    assert.equal(meta(doc, "og:site_name"), "Vyrn");
    assert.equal(meta(doc, "twitter:card"), "summary");
  }
});

test("no two routes share a description", () => {
  const seen = new Map();
  for (const [file, doc] of routes) {
    const d = meta(doc, "description");
    assert.ok(!seen.has(d), `${file} and ${seen.get(d)} share a description: ${JSON.stringify(d)}`);
    seen.set(d, file);
  }
});

test("no two routes share an og:title", () => {
  const seen = new Map();
  for (const [file, doc] of routes) {
    const t = meta(doc, "og:title");
    assert.ok(!seen.has(t), `${file} and ${seen.get(t)} share an og:title: ${JSON.stringify(t)}`);
    seen.set(t, file);
  }
});

test("every canonical URL is absolute and names the route it is on", () => {
  for (const [file, doc] of routes) {
    const url = canonical(doc);
    assert.ok(url.startsWith("https://"), `${file}: canonical ${url} is not absolute`);
    const want = file === "index.html" ? ORIGIN : ORIGIN + file;
    assert.equal(url, want, `${file}: canonical names another page`);
  }
});

test("a title fits a search result, and a description fits a card", () => {
  for (const [file, doc] of routes) {
    // Sixty characters is where a search result cuts. The home page's title is
    // the sentence the site leads with and is three over; shortening it belongs
    // to whoever wrote it, and the ceiling here stops it growing.
    const ceiling = file === "index.html" ? 63 : 60;
    assert.ok([...title(doc)].length <= ceiling, `${file}: title is ${[...title(doc)].length} characters`);
    const d = meta(doc, "description");
    assert.ok([...d].length >= 50, `${file}: description is ${[...d].length} characters, too short to say anything`);
    assert.ok([...d].length <= 155, `${file}: description is ${[...d].length} characters, past where a card cuts`);
  }
});

test("a card image, if one is ever declared, is absolute and is published", async () => {
  // The site declares none: it publishes one graphic, `favicon.svg`, and no
  // crawler renders an SVG card. This is the rule waiting for the day one is
  // added — a card image that 404s is worse than a card with no image.
  for (const [file, doc] of routes) {
    const image = meta(doc, "og:image");
    if (!image) continue;
    assert.ok(image.startsWith(ORIGIN), `${file}: og:image ${image} is not an absolute URL on this site`);
    const local = path.join(OUT, image.slice(ORIGIN.length));
    assert.ok((await stat(local).catch(() => null))?.isFile(), `${file}: og:image names ${image} and no file is there`);
    assert.ok(meta(doc, "og:image:width") && meta(doc, "og:image:height") && meta(doc, "og:image:alt"), `${file}: og:image with no size and no alternative text`);
  }
});

test("every route names a ground for both colour schemes", () => {
  for (const [file, doc] of routes) {
    const tags = head(doc).match(/<meta name="theme-color"[^>]*>/g) || [];
    assert.equal(tags.length, 2, `${file} declares ${tags.length} theme colours`);
    assert.ok(tags.some((t) => t.includes("(prefers-color-scheme: light)")), `${file} has no light ground`);
    assert.ok(tags.some((t) => t.includes("(prefers-color-scheme: dark)")), `${file} has no dark ground`);
  }
});

test("the structured record parses, and says what the card says", () => {
  let articles = 0;
  let apps = 0;
  for (const [file, doc] of routes) {
    const found = /<script type="application\/ld\+json">(.*?)<\/script>/s.exec(head(doc));
    const kind = meta(doc, "og:type");
    if (kind === "article") assert.ok(found, `${file} is an article and carries no record`);
    if (!found) continue;
    const record = JSON.parse(found[1]);
    assert.equal(record["@context"], "https://schema.org");
    assert.equal(record.url, canonical(doc), `${file}: the record names another URL`);
    // The record is built from the two strings the card is built from, so a
    // difference here is a second source of truth appearing.
    const decode = (s) => s.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");
    assert.equal(record.description, decode(meta(doc, "og:description")));
    assert.equal(record.headline ?? record.name, decode(meta(doc, "og:title")));
    if (record["@type"] === "TechArticle") articles += 1;
    if (record["@type"] === "SoftwareApplication") apps += 1;
  }
  // One page describes the whole thing. The rest describe a page of it.
  assert.equal(apps, 1);
  assert.ok(articles > 50, `only ${articles} documentation pages carry a record`);
});

test("a documentation page names the shelf it sits on", () => {
  for (const [file, doc] of routes) {
    const section = meta(doc, "article:section");
    if (meta(doc, "og:type") !== "article") {
      assert.equal(section, undefined, `${file} is not an article and names a section`);
      continue;
    }
    // A page under a shelf names it; a page at the root of the site has none.
    const shelf = routeOf(file).split("/")[1];
    if (!routeOf(file).slice(1).includes("/")) continue;
    assert.ok(section, `${file} is under /${shelf} and names no section`);
  }
});
