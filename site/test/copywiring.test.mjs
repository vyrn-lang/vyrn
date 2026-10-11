// The copy affordances and the script that wires them, over the exported tree.
//
// A Copy button whose script is not loaded, or whose target is a 404, is worse
// than no button.
//
// Run: node --test site/test/copywiring.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import { readFile, readdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const OUT = fileURLToPath(new URL("../../out/", import.meta.url));

async function tree(dir, base = "") {
  const out = [];
  for (const e of await readdir(path.join(dir, base), { withFileTypes: true })) {
    if (e.isDirectory()) out.push(...await tree(dir, `${base}${e.name}/`));
    else out.push(`${base}${e.name}`);
  }
  return out;
}

const files = await tree(OUT).catch(() => []);
const pages = files.filter((f) => f.endsWith(".html"));

test("the export is there to check", () => {
  const ok = files.length > 0;
  if (!ok) throw new Error(`out/ is empty — run \`vyrn run site/export.vyrn out\` first`);
});

const source = new Map(await Promise.all(pages.map(async (p) => [p, await readFile(path.join(OUT, p), "utf8")])));

// Every page that emits a copy affordance loads `widgets.js`, which wires
// `copyButtons` and `copyPageButtons`.
test("every page with a copy affordance loads widgets.js", () => {
  for (const [p, html] of source) {
    const emits = html.includes("data-copy");
    if (!emits) continue;
    const wired = /<script[^>]*src="[^"]*widgets\.js"/.test(html);
    if (!wired) throw new Error(`${p} emits data-copy but does not load widgets.js`);
  }
});

// Every Copy page link names a twin that exists. The href is relative to the
// page, so it resolves the way a browser resolves it.
test("every Copy page link names an existing markdown twin", async () => {
  for (const [p, html] of source) {
    const tags = html.match(/<a [^>]*data-copy-md[^>]*>/g) || [];
    const hrefs = tags.map((t) => (t.match(/ href="([^"]*)"/) || [])[1]).filter(Boolean);
    for (const href of hrefs) {
      const target = path.normalize(path.join(path.dirname(p), href.split("#")[0]));
      let ok = false;
      try { ok = (await stat(path.join(OUT, target))).isFile(); } catch {}
      if (!ok) throw new Error(`${p} links ${href} and no file is there`);
    }
  }
});

// Every page of the documentation shell carries the page-level copy button.
// The command plates' Copy buttons are a different affordance and do not count.
test("every docs page carries the Copy page button", () => {
  for (const [p, html] of source) {
    const docs = html.includes('class="page docs"');
    if (!docs) continue;
    if (!html.includes("data-copy-md")) throw new Error(`${p} is a docs page with no Copy page button`);
  }
});
