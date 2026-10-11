// The playground's run limit, and the tooltip that states it in prose.
//
// `play-run.js` holds `RUN_LIMIT_MS`; the tooltip on `/play.html` says "five
// seconds". This ties the two, so a changed limit cannot leave the tooltip
// promising another. Recursion has no number to check: no engine counts calls,
// and a recursion that outgrows the tab's stack ends with `vyrn run`'s trap.
//
// Run: node --test site/test/playlimits.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const [runJs, page] = await Promise.all([
  read("site/public/play-run.js"),
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

const runLimitMs = Number(only(runJs, /const RUN_LIMIT_MS = (\d+);/g, "RUN_LIMIT_MS"));

// Whole seconds as the tooltip spells them; a limit this cannot spell fails
// and says to extend the list.
const WORDS = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];

test("the tooltip's run limit is the limit play-run.js enforces", () => {
  if (runLimitMs % 1000 !== 0 || runLimitMs / 1000 >= WORDS.length) {
    throw new Error(`RUN_LIMIT_MS is ${runLimitMs}ms, which this test cannot spell — extend WORDS`);
  }
  const said = `${WORDS[runLimitMs / 1000]} second${runLimitMs === 1000 ? "" : "s"}`;
  if (!page.includes(`stopped after ${said}`)) {
    throw new Error(
      `play-run.js kills a program after ${runLimitMs}ms, so the tooltip should say "stopped after ${said}" and it does not`,
    );
  }
});
