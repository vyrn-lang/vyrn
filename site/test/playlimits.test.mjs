// The playground's two limits, and the places that state them in prose.
//
// The defect this exists for: the numbers a reader sees are English, and the
// numbers that enforce them are code, in two different languages. The tooltip on
// `/play.html` says "five seconds" and "1,000 calls". `play.js` holds
// `RUN_LIMIT_MS`. `CALL_DEPTH_LIMIT` is a Rust constant in `trap.rs`.
//
// Nothing tied them together. Lowering `RUN_LIMIT_MS` to three seconds would
// leave a tooltip promising five, and a reader whose program was killed at three
// has no way to tell a limit from a bug.
//
// The census (rfcs/census/hardcoded-data.md, item 8) proposed emitting a limits
// file from the wasm build and reading it at generation time. This does not,
// deliberately: a sentence says "five seconds", not "5000", so the prose would
// still be written by hand and would still be the thing that drifts. Comparing
// the prose to the source catches the drift the file would not.
//
// THERE USED TO BE A THIRD LIMIT HERE, and it was the page's own: the tree-
// walking interpreter ran in this tab, spent ~8.5 KB of native stack per Vyrn
// call, and gave out at a MEASURED 466 nested calls — well under the language's
// 1,000 — so the page had to explain whose ceiling a reader had hit. RFC-0125 M5
// took the interpreter out. The page compiles the program and runs the module,
// one wasm frame per call, and the language's limit arrives first: a program
// that recurses too deep now gets `call depth exceeds 1000` on standard error,
// which is what `vyrn run` gives it. So the tooltip states the language's number
// and this file checks it against the language's constant — the measurement, and
// the sentence explaining it, are gone.
//
// Run: node --test site/test/playlimits.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const [playJs, trap, page] = await Promise.all([
  read("site/public/play.js"),
  read("compiler/vyrn-frontend/src/trap.rs"),
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
const depthLimit = Number(
  only(trap, /pub const CALL_DEPTH_LIMIT: u32 = ([\d_]+);/g, "CALL_DEPTH_LIMIT").replace(/_/g, ""),
);

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

test("the tooltip's recursion limit is the language's own constant", () => {
  // `1_000` in Rust is "1,000" in a sentence.
  const said = depthLimit.toLocaleString("en-US");
  const found = page.match(/recursion stops at ([\d,]+) calls/);
  if (!found) throw new Error("the play.html tooltip no longer states a recursion limit");
  if (found[1] !== said) {
    throw new Error(
      `CALL_DEPTH_LIMIT is ${depthLimit}, so the tooltip should say "recursion stops at ${said} calls" ` +
        `and it says ${found[1]}`,
    );
  }
});
