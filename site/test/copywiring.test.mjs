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

test("every page with a copy affordance loads widgets.js", () => {
  for (const [p, html] of source) {
    const emits = html.includes("data-copy");
    if (!emits) continue;
    const wired = /<script[^>]*src="[^"]*widgets\.js"/.test(html);
    if (!wired) throw new Error(`${p} emits data-copy but does not load widgets.js`);
  }
});

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

test("every docs page carries the Copy page button", () => {
  for (const [p, html] of source) {
    const docs = html.includes('class="page docs"');
    if (!docs) continue;
    if (!html.includes("data-copy-md")) throw new Error(`${p} is a docs page with no Copy page button`);
  }
});
