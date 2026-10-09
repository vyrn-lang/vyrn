// The playground's run limit, and the place that states it in prose.
//
// The defect this exists for: the number a reader sees is English, and the
// number that enforces it is code. The tooltip on `/play.html` says "five
// seconds". `play.js` holds `RUN_LIMIT_MS`.
//
// Nothing tied them together. Lowering `RUN_LIMIT_MS` to three seconds would
// leave a tooltip promising five, and a reader whose program was killed at three
// has no way to tell a limit from a bug.
//
// A census proposed emitting a limits
// file from the wasm build and reading it at generation time. This does not,
// deliberately: a sentence says "five seconds", not "5000", so the prose would
// still be written by hand and would still be the thing that drifts. Comparing
// the prose to the source catches the drift the file would not.
//
// Recursion has no number to check: no engine counts calls, and a recursion
// that outgrows the tab's stack ends with the trap `vyrn run` prints.
//
// Run: node --test site/test/playlimits.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const [playJs, page] = await Promise.all([
  read("site/public/play.js"),
  read("out/play.html").catch(() => ""),
]);

test("the export is there to check", () => {
  if (!page) throw new Error("out/play.html is missing — run `vyrn run site/export.vyrn out` first");
});

// The one place each number is decided.
function only(src, re, what) {
  const hits = [...src.matchAll(re)];
  if (hits.length !== 1) throw new Error(`${what}: expected one definition, found ${hits.length}`);
  return hits[0][1];
}

const runLimitMs = Number(only(playJs, /const RUN_LIMIT_MS = (\d+);/g, "RUN_LIMIT_MS"));

// Whole seconds spelled the way the tooltip spells them. The limit has been
// 5000 for as long as the page has existed; if it ever stops being a whole
// small number, this map is the thing to change, and the failure says so.
const WORDS = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];

test("the tooltip's run limit is the limit play.js enforces", () => {
  if (runLimitMs % 1000 !== 0 || runLimitMs / 1000 >= WORDS.length) {
    throw new Error(`RUN_LIMIT_MS is ${runLimitMs}ms, which this test cannot spell — extend WORDS`);
  }
  const said = `${WORDS[runLimitMs / 1000]} second${runLimitMs === 1000 ? "" : "s"}`;
  if (!page.includes(`stopped after ${said}`)) {
    throw new Error(
      `play.js kills a program after ${runLimitMs}ms, so the tooltip should say "stopped after ${said}" and it does not`,
    );
  }
});
