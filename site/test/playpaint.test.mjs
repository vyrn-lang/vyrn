import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { instantiatePlay } from "../public/play-wasm.js";
import { highlight } from "../public/play-paint.js";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)));
const play = await instantiatePlay(await read("out/play.wasm"));
const index = (await read("out/index.html")).toString("utf8");

const unescape = (s) => s.replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&amp;/g, "&");

// Each code line of the landing page's cards, as site/app/hl.vyrn coloured it at export.
// A hint line (`vx-js`) is markup, not code, and a line holding a clickable word nests the scene's span.
const lines = [...index.matchAll(/<div class="(vx-line[^"]*)"[^>]*>((?:(?!<\/div>).)*)<\/div>/g)]
  .filter((m) => !m[1].includes("vx-js") && !m[2].includes("data-op"))
  .map((m) => m[2]);

test("the landing page's cards are coloured by the highlighter", () => {
  assert.ok(lines.length >= 15, `found ${lines.length} card lines`);
  assert.ok(lines.some((l) => l.includes('<span class="f">')), "no function name is marked");
});

test("the playground colours every card line as the landing page does", () => {
  for (const html of lines) {
    const src = unescape(html.replace(/<[^>]*>/g, ""));
    assert.equal(highlight(src, play.tokens(src).spans).replace(/\n$/, ""), html, src);
  }
});
