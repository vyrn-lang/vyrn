import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";

const OUT = path.resolve(import.meta.dirname, "../../out");
const xml = await readFile(path.join(OUT, "releases.xml"), "utf8");
const history = JSON.parse(await readFile(path.resolve(import.meta.dirname, "../data/history.json"), "utf8"));

// Node ships no XML parser and the site takes no dependency, so this reader is small and strict.
function parseXml(src) {
  let i = 0;
  const fail = (why) => {
    throw new Error(`${why} at byte ${i}: ${JSON.stringify(src.slice(i, i + 40))}`);
  };
  const decl = /^<\?xml version="1\.0" encoding="utf-8"\?>\s*/.exec(src);
  if (!decl) fail("no XML declaration");
  i = decl[0].length;
  const text = (s) => {
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
  const bad = [
    ['<?xml version="1.0" encoding="utf-8"?><a><b></a>', "wrong tag"],
    ['<?xml version="1.0" encoding="utf-8"?><a>', "never closed"],
    ['<?xml version="1.0" encoding="utf-8"?><a>x & y</a>', "unknown entity"],
    ["<a></a>", "no declaration"],
  ];
  for (const [src] of bad) assert.throws(() => parseXml(src), `${src} parsed`);
  assert.equal(parseXml('<?xml version="1.0" encoding="utf-8"?><a x="1"><b/>t &amp; u</a>').text, "t & u");
});

test("the feed is one RSS 2.0 channel with the elements a reader needs", () => {
  assert.equal(rss.name, "rss");
  assert.equal(rss.attrs.version, "2.0");
  assert.equal(rss.attrs["xmlns:atom"], "http://www.w3.org/2005/Atom");
  assert.equal(child(channel, "title").text, "Vyrn releases");
  assert.ok(child(channel, "description").text.length > 20);
  assert.equal(child(channel, "link").text, "https://vyrn-lang.github.io/vyrn/releases.html");
  const self = child(channel, "atom:link");
  assert.equal(self.attrs.rel, "self");
  assert.equal(self.attrs.type, "application/rss+xml");
  assert.equal(self.attrs.href, "https://vyrn-lang.github.io/vyrn/releases.xml");
});

test("every release the history knows is an item, newest first", () => {
  const tags = history.releases.map((r) => r.t);
  if (tags.length === 0) {
    assert.match(child(channel, "description").text, /None has been published/);
  }
  assert.deepEqual(
    items.map((it) => child(it, "title").text),
    [...tags].reverse()
  );
  for (const it of items) {
    const tag = child(it, "title").text;
    const url = `https://github.com/vyrn-lang/vyrn/releases/tag/${tag}`;
    assert.equal(child(it, "link").text, url);
    assert.equal(child(it, "guid").text, url);
    assert.equal(child(it, "guid").attrs.isPermaLink, "true");
    assert.ok(child(it, "description").text.length > 20);
    const pre = tag.includes("-");
    assert.equal(child(it, "description").text.startsWith("A pre-release"), pre);
  }
});

test("every pubDate is a date, and it is the day the tag was published", () => {
  for (const it of items) {
    const tag = child(it, "title").text;
    const stamp = child(it, "pubDate").text;
    const day = history.releases.find((r) => r.t === tag).d;
    const parsed = new Date(stamp);
    assert.ok(!Number.isNaN(parsed.getTime()), `${stamp} is not a date any reader can parse`);
    assert.equal(parsed.toISOString(), `${day}T00:00:00.000Z`);
    assert.equal(stamp, parsed.toUTCString());
  }
});

test("every page tells a browser where the feed is", async () => {
  const link = '<link rel="alternate" type="application/rss+xml" title="Vyrn releases" href=';
  for (const page of ["index.html", "releases.html", "docs/std/json.html", "guide/values.html"]) {
    const html = await readFile(path.join(OUT, page), "utf8");
    assert.ok(html.includes(link), `${page} does not declare the feed`);
    const up = "../".repeat(page.split("/").length - 1);
    assert.ok(html.includes(`${link}"${up}releases.xml">`), `${page} names the feed at the wrong depth`);
  }
});
