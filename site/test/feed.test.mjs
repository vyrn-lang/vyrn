// THE RELEASES FEED, PARSED (RFC-0106 M1).
//
// The gate is "valid feed, parsed by a test", and it is parsed rather than
// grepped: `site/app/feed.vyrn` writes XML, and the failure a string search
// cannot see is the one that matters — an element left open, a `&` that is not an
// entity, a `<` inside a description. So the twenty lines below are an XML reader,
// and it is twenty lines rather than a dependency because RFC-0106 says the site
// takes none and node ships no XML parser. It is deliberately STRICT: anything it
// does not understand throws, which is the behaviour a validator is wanted for.
//
// What the feed says is then checked against `site/data/history.json`, which is
// where the releases come from, so a feed that dropped a release or invented one
// fails here.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";

const OUT = path.resolve(import.meta.dirname, "../../out");
const xml = await readFile(path.join(OUT, "releases.xml"), "utf8");
const history = JSON.parse(await readFile(path.resolve(import.meta.dirname, "../data/history.json"), "utf8"));

/// Parse `src` into `{ name, attrs, children, text }`, or throw.
///
/// It knows the declaration, elements, self-closing elements, attributes in
/// double quotes, text, and the five named entities the site writes. It does NOT
/// know comments, CDATA, processing instructions or namespaces-as-anything — the
/// input is one generated file and a construct it does not know is a construct
/// nobody meant to write.
function parseXml(src) {
  let i = 0;
  const fail = (why) => {
    throw new Error(`${why} at byte ${i}: ${JSON.stringify(src.slice(i, i + 40))}`);
  };
  const decl = /^<\?xml version="1\.0" encoding="utf-8"\?>\s*/.exec(src);
  if (!decl) fail("no XML declaration");
  i = decl[0].length;
  const text = (s) => {
    // A bare `&` is the error this exists to catch: every one of them has to be
    // an entity, and only the five the site writes are entities here.
    for (const m of s.matchAll(/&[^;]*;?/g)) {
      if (!["&amp;", "&lt;", "&gt;", "&quot;", "&#39;"].includes(m[0])) {
        i += m.index;
        fail(`unknown entity ${m[0]}`);
      }
    }
    if (s.includes("<")) fail("a `<` in text");
    return s
      .replace(/&lt;/g, "<")
      .replace(/&gt;/g, ">")
      .replace(/&quot;/g, '"')
      .replace(/&#39;/g, "'")
      .replace(/&amp;/g, "&");
  };
  const element = () => {
    const open = /^<([A-Za-z][\w:.-]*)((?:\s+[\w:.-]+="[^"<]*")*)\s*(\/?)>/.exec(src.slice(i));
    if (!open) fail("expected an element");
    i += open[0].length;
    const attrs = {};
    for (const a of open[2].matchAll(/([\w:.-]+)="([^"<]*)"/g)) attrs[a[1]] = text(a[2]);
    const node = { name: open[1], attrs, children: [], text: "" };
    if (open[3] === "/") return node;
    while (true) {
      const close = new RegExp(`^</${node.name}>`).exec(src.slice(i));
      if (close) {
        i += close[0].length;
        return node;
      }
      if (i >= src.length) fail(`<${node.name}> is never closed`);
      if (src[i] === "<") {
        if (src.startsWith("</", i)) fail(`<${node.name}> closed by the wrong tag`);
        node.children.push(element());
      } else {
        const upTo = src.indexOf("<", i);
        node.text += text(src.slice(i, upTo < 0 ? src.length : upTo));
        i = upTo < 0 ? src.length : upTo;
      }
    }
  };
  const root = element();
  if (src.slice(i).trim() !== "") fail("content after the root element");
  return root;
}

const rss = parseXml(xml);
const child = (node, name) => node.children.find((c) => c.name === name);
const channel = child(rss, "channel");
const items = channel.children.filter((c) => c.name === "item");

test("the reader is strict enough to be worth running", () => {
  // A parser that accepted anything would make every assertion below vacuous, so
  // it is shown rejecting each of the four things it exists to catch.
  const bad = [
    ['<?xml version="1.0" encoding="utf-8"?><a><b></a>', "wrong tag"],
    ['<?xml version="1.0" encoding="utf-8"?><a>', "never closed"],
    ['<?xml version="1.0" encoding="utf-8"?><a>x & y</a>', "unknown entity"],
    ["<a></a>", "no declaration"],
  ];
  for (const [src] of bad) assert.throws(() => parseXml(src), `${src} parsed`);
  // And accepting what the feed is made of.
  assert.equal(parseXml('<?xml version="1.0" encoding="utf-8"?><a x="1"><b/>t &amp; u</a>').text, "t & u");
});

test("the feed is one RSS 2.0 channel with the elements a reader needs", () => {
  assert.equal(rss.name, "rss");
  assert.equal(rss.attrs.version, "2.0");
  assert.equal(rss.attrs["xmlns:atom"], "http://www.w3.org/2005/Atom");
  assert.equal(child(channel, "title").text, "Vyrn releases");
  assert.ok(child(channel, "description").text.length > 20);
  // The channel points at the page, and the feed at itself — the one element of
  // Atom that RSS borrows, and the only way an aggregator handed the file can
  // find where it came from.
  assert.equal(child(channel, "link").text, "https://vyrn-lang.github.io/vyrn/releases.html");
  const self = child(channel, "atom:link");
  assert.equal(self.attrs.rel, "self");
  assert.equal(self.attrs.type, "application/rss+xml");
  assert.equal(self.attrs.href, "https://vyrn-lang.github.io/vyrn/releases.xml");
});

test("every release the history knows is an item, newest first", () => {
  const tags = history.releases.map((r) => r.t);
  assert.ok(tags.length > 0, "the history has no releases, so this test proves nothing");
  assert.deepEqual(
    items.map((it) => child(it, "title").text),
    [...tags].reverse()
  );
  for (const it of items) {
    const tag = child(it, "title").text;
    const url = `https://github.com/vyrn-lang/vyrn/releases/tag/${tag}`;
    assert.equal(child(it, "link").text, url);
    // The guid is the link and says it is one, so an aggregator shows a release
    // once and not again on every rebuild of the feed.
    assert.equal(child(it, "guid").text, url);
    assert.equal(child(it, "guid").attrs.isPermaLink, "true");
    assert.ok(child(it, "description").text.length > 20);
    // A pre-release is named as one. Both of this repository's tags are.
    const pre = tag.includes("-");
    assert.equal(child(it, "description").text.startsWith("A pre-release"), pre);
  }
});

test("every pubDate is a date, and it is the day the tag was published", () => {
  // THE DAY NAME IS ARITHMETIC in `feed.vyrn` (Zeller's congruence), so it is
  // checked against the platform's own calendar rather than against itself: a
  // wrong day name in a field a machine reads is worse than no field.
  for (const it of items) {
    const tag = child(it, "title").text;
    const stamp = child(it, "pubDate").text;
    const day = history.releases.find((r) => r.t === tag).d;
    const parsed = new Date(stamp);
    assert.ok(!Number.isNaN(parsed.getTime()), `${stamp} is not a date any reader can parse`);
    assert.equal(parsed.toISOString(), `${day}T00:00:00.000Z`);
    // The day name the feed wrote is the day name the calendar gives.
    assert.equal(stamp, parsed.toUTCString());
  }
});

test("every page tells a browser where the feed is", async () => {
  const link = '<link rel="alternate" type="application/rss+xml" title="Vyrn releases" href=';
  for (const page of ["index.html", "releases.html", "docs/std/json.html", "guide/values.html"]) {
    const html = await readFile(path.join(OUT, page), "utf8");
    assert.ok(html.includes(link), `${page} does not declare the feed`);
    // Relative to the document, like every other URL the export writes: a page
    // two directories down names it two directories up.
    const up = "../".repeat(page.split("/").length - 1);
    assert.ok(html.includes(`${link}"${up}releases.xml">`), `${page} names the feed at the wrong depth`);
  }
});
