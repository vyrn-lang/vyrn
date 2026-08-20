// THE GLYPHS THE TEMPLATES NAME, AND NO OTHERS (RFC-0107 M3).
//
// `<Icon name="bytesize:github"/>` is a generation-time component: the tag is
// resolved while the page is compiled, one inline `<svg>` is spliced where it
// stood, and nothing is fetched at run time. The claim that buys — the artifact
// carries exactly the glyphs the templates ask for, out of collections holding a
// hundred each — is only a claim until something counts.
//
// So this counts, over the exported tree, and it takes every number from the
// TEMPLATES rather than from a list somebody has to remember to update: add a
// tag and the expectation moves with it; add a tag to a template this file does
// not map to pages and the first assertion says so by name.
//
// It also checks the one a11y rule an icon has: an icon beside a text label is
// decoration and renders `aria-hidden`, an icon that IS the label carries a
// `label` attribute and renders as `role="img"` with that accessible name.
// Both rows are on the site, and both are asserted here.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const routes = path.join(here, "..", "app", "routes");
const out = path.join(here, "..", "..", "out");

/// The shell every consumer page wears. Its footer link is the marker: the
/// backstage builds a masthead of its own and wears none of this.
const SHELL = "layout.vyx";
const SHELL_MARK = "Source on GitHub";
/// The one route template that carries tags of its own, and the page it makes.
const PAGE_TAGS = { "install.vyx": "install.html" };

/// Every `<Icon .../>` in a template, as `{ name, label }`.
function tagsOf(src) {
  return [...src.matchAll(/<Icon\s([^>]*?)\/>/g)].map((m) => ({
    name: (m[1].match(/name="([^"]*)"/) || [])[1],
    label: (m[1].match(/label="([^"]*)"/) || [])[1] || "",
  }));
}

/// Every glyph `std/icons` drew, as `{ open, body }`. The signature is that
/// module's own: a `viewBox` from the collection and `1em` in both dimensions,
/// which no other `<svg>` on this site carries.
function glyphsIn(html) {
  return [...html.matchAll(/<svg (viewBox="[^"]*" width="1em" height="1em"[^>]*)>(.*?)<\/svg>/gs)]
    .map((m) => ({ open: m[1], body: m[2] }));
}

async function htmlFiles(dir) {
  const found = [];
  for (const e of await readdir(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) found.push(...(await htmlFiles(p)));
    else if (e.name.endsWith(".html")) found.push(p);
  }
  return found;
}

async function templates(dir) {
  const found = [];
  for (const e of await readdir(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) found.push(...(await templates(p)));
    else if (e.name.endsWith(".vyx")) found.push(p);
  }
  return found;
}

test("every template carrying tags is one this gate maps to pages", async () => {
  const carrying = [];
  for (const t of await templates(routes)) {
    if (tagsOf(await readFile(t, "utf8")).length > 0) carrying.push(path.basename(t));
  }
  carrying.sort();
  assert.deepEqual(
    carrying,
    [SHELL, ...Object.keys(PAGE_TAGS)].sort(),
    "a template gained `<Icon>` tags and this file does not know which pages it makes — add it to PAGE_TAGS",
  );
});

test("each page carries exactly the glyphs its templates name", async () => {
  const shell = tagsOf(await readFile(path.join(routes, SHELL), "utf8"));
  assert.ok(shell.length >= 3, `the shell should carry glyphs: ${shell.length}`);
  const own = {};
  for (const [file, page] of Object.entries(PAGE_TAGS)) {
    own[page] = tagsOf(await readFile(path.join(routes, file), "utf8"));
  }

  const pages = await htmlFiles(out);
  assert.ok(pages.length > 100, `the export looks empty: ${pages.length} pages`);
  let shellPages = 0;
  for (const p of pages) {
    const html = await readFile(p, "utf8");
    const wearsShell = html.includes(SHELL_MARK);
    const expected = (wearsShell ? shell : []).concat(own[path.basename(p)] || []);
    if (wearsShell) shellPages += 1;
    assert.equal(
      glyphsIn(html).length,
      expected.length,
      `${path.relative(out, p)} carries ${glyphsIn(html).length} glyphs, its templates name ${expected.length}`,
    );
  }
  assert.ok(shellPages > 50, `only ${shellPages} pages wear the shell`);
});

test("the export carries no glyph the templates did not name", async () => {
  // One collection holds a hundred glyphs; six of them are named on this site.
  // Distinct BODIES, so the same glyph on two hundred pages counts once and a
  // seventh drawing anywhere in the tree fails this.
  const named = new Set();
  for (const t of await templates(routes)) {
    for (const tag of tagsOf(await readFile(t, "utf8"))) named.add(tag.name);
  }
  const drawn = new Set();
  for (const p of await htmlFiles(out)) {
    for (const g of glyphsIn(await readFile(p, "utf8"))) drawn.add(g.body);
  }
  assert.equal(
    drawn.size,
    named.size,
    `the templates name ${named.size} glyphs (${[...named].join(", ")}) and the export draws ${drawn.size}`,
  );
});

test("a decorative glyph is hidden and a labelled one is named", async () => {
  const shell = tagsOf(await readFile(path.join(routes, SHELL), "utf8"));
  const labelled = shell.filter((t) => t.label !== "");
  assert.ok(labelled.length >= 1, "the shell should exercise a labelled glyph too");

  const html = await readFile(path.join(out, "index.html"), "utf8");
  const glyphs = glyphsIn(html);
  for (const g of glyphs) {
    const hidden = g.open.includes('aria-hidden="true"');
    const named = /role="img" aria-label="[^"]+"/.test(g.open);
    assert.ok(
      hidden !== named,
      `a glyph is neither decoration nor content: <svg ${g.open}>`,
    );
    if (named) assert.ok(!hidden, `a named glyph must not also be hidden: <svg ${g.open}>`);
  }
  const named = glyphs.filter((g) => g.open.includes('role="img"'));
  assert.equal(named.length, labelled.length, "the labelled tags and the named glyphs disagree");
  for (const t of labelled) {
    assert.ok(
      named.some((g) => g.open.includes(`aria-label="${t.label}"`)),
      `no glyph carries the accessible name ${t.label}`,
    );
  }
  // And the glyph inherits colour and size: no `fill`, no pixel dimension.
  for (const g of glyphs) {
    assert.ok(!/ fill="/.test(g.open), `a glyph pins its own colour: <svg ${g.open}>`);
    assert.ok(g.open.includes('width="1em"'), `a glyph pins a pixel size: <svg ${g.open}>`);
  }
});
