// The playground's three limits, and the four places that state them in prose.
//
// The defect this exists for: the numbers a reader sees are English, and the
// numbers that enforce them are code, in three different languages. The tooltip
// on `/play.html` says "five seconds" and "466 calls". `play.js` holds
// `RUN_LIMIT_MS`. `play-worker.js` writes "1,000" into the message a reader gets
// when a program recurses too deep. `CALL_DEPTH_LIMIT` is a Rust constant.
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
// Run: node --test site/test/playlimits.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const [playJs, worker, trap, page] = await Promise.all([
  read("site/public/play.js"),
  read("site/public/play-worker.js"),
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

test("the worker quotes the interpreter's own call-depth limit", () => {
  // `1_000` in Rust is "1,000" in a sentence.
  const said = depthLimit.toLocaleString("en-US");
  const quotes = [...worker.matchAll(new RegExp(said.replace(/,/g, ","), "g"))].length;
  if (quotes < 2) {
    throw new Error(
      `CALL_DEPTH_LIMIT is ${depthLimit}, so play-worker.js's stack message should say "${said}" ` +
        `where it contrasts the browser's ceiling with the language's — found ${quotes} of the 2 it writes`,
    );
  }
});

// 466 is a MEASUREMENT of one browser's worker stack, not a setting: nothing in
// this repository decides it, and it moves when V8 does. So the check is that
// the two places a reader can see it agree with each other — not that either
// agrees with a constant, because there is no constant to agree with.
test("the tooltip and the worker report the same measured recursion ceiling", () => {
  const fromWorker = worker.match(/measured at (\d+) nested calls/);
  if (!fromWorker) throw new Error("play-worker.js no longer states a measured recursion ceiling");
  const fromPage = page.match(/recursion stops near (\d+) calls/);
  if (!fromPage) throw new Error("the play.html tooltip no longer states a recursion ceiling");
  if (fromWorker[1] !== fromPage[1]) {
    throw new Error(
      `play-worker.js measured ${fromWorker[1]} nested calls and the tooltip says ${fromPage[1]}`,
    );
  }
  // And it is below the language's limit, which is the whole point of the
  // sentence: the ceiling a reader hits here is the browser's, not Vyrn's.
  if (Number(fromWorker[1]) >= depthLimit) {
    throw new Error(
      `the measured browser ceiling (${fromWorker[1]}) is not below CALL_DEPTH_LIMIT (${depthLimit}), ` +
        `so the message contrasting them is now wrong`,
    );
  }
});
