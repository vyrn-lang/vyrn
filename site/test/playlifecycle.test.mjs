// State that outlives the run it belongs to, and a press the page owes a reader.
//
// The kill timer belongs to the worker it kills: a timer held per mount
// survived a second Run, Ctrl-Enter or Reset and terminated the next run. The
// hero editor keeps two flags: arming, and a Run press owed a replay.
//
// Both need a DOM, a Worker and a clock, which node does not have, so this
// checks the code that states each rule.
//
// Run: node --test site/test/playlifecycle.test.mjs
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const [runJs, heroJs] = await Promise.all([read("site/public/play-run.js"), read("site/public/hero.js")]);

// The text of a function, from `function <name>(` to the first `\n}`. Every
// function in these files closes at column 0.
function body(src, name) {
  const at = src.indexOf(`function ${name}(`);
  if (at < 0) throw new Error(`${name} is gone from the file this test checks`);
  const end = src.indexOf("\n}", at);
  if (end < 0) throw new Error(`${name} has no closing brace at column 0`);
  return src.slice(at, end);
}

test("the kill timer dies with the worker it would kill", () => {
  const stop = body(runJs, "stopWorker");
  if (!stop.includes("clearTimeout(runTimer)")) {
    throw new Error(
      "stopWorker does not clear runTimer, so every path that abandons a live run " +
        "(a second Run, Ctrl-Enter, Reset) leaves the old timeout armed over the next one",
    );
  }
});

test("the kill timer is module state, so one mount cannot hide another's", () => {
  const decls = [...runJs.matchAll(/^\s*let runTimer = null;$/gm)];
  if (decls.length !== 1) {
    throw new Error(`expected one runTimer declaration, found ${decls.length}`);
  }
  if (decls[0][0] !== "let runTimer = null;") {
    throw new Error("runTimer is declared indented, which means inside a function, where stopWorker cannot reach it");
  }
  if (runJs.indexOf("let runTimer") > runJs.indexOf("function stopWorker(")) {
    throw new Error("runTimer is declared after stopWorker rather than beside the worker it belongs to");
  }
});

test("the hero editor replays a Run press that arrived after hover or focus armed it", () => {
  const arm = body(heroJs, "armHeroEditor");
  const owed = arm.indexOf("runOnReady = runOnReady || thenRun");
  const latch = arm.indexOf("if (armed) return;");
  if (owed < 0) {
    throw new Error("armHeroEditor tracks no run-on-ready flag, so one latch again serves arming and the replay");
  }
  if (latch < 0 || owed > latch) {
    throw new Error("the press is recorded after the arming latch returns, which is where it was being dropped");
  }
  if (!arm.includes("onReady: () => runOnReady && runBtn.click()")) {
    throw new Error("readiness replays `thenRun` again, which is false for the hover and focus arming");
  }
});
