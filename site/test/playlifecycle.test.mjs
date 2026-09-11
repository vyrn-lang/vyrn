// State that outlives the run it belongs to, and a press the page owes a reader.
//
// The playground's kill timer was held per-mount and cleared only by the path
// that finished a run, so a second `Run`, a Ctrl-Enter or a `Reset` left the
// first run's timeout armed; five seconds later it terminated whatever was
// running by then and wrote "Stopped after 5 seconds." over a run that had
// already succeeded.
//
// The hero editor used one latch for two facts: arming and the replayed press.
// A reader who hovered or tabbed into the plate armed it with no press owed,
// and the Run press that followed returned at the latch.
//
// Neither is reachable from node: both need a DOM, a Worker and a clock. What
// this file checks is the rule each fix states, in the one place it is stated.
//
// Run: node --test site/test/playlifecycle.test.mjs
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const [playJs, widgetsJs] = await Promise.all([read("site/public/play.js"), read("site/public/widgets.js")]);

// The text of a function, from its `function <name>(` to the `\n}` that closes
// it. Every function in these two files is written at the column its body is
// closed at, so the first line-initial `}` ends it.
function body(src, name) {
  const at = src.indexOf(`function ${name}(`);
  if (at < 0) throw new Error(`${name} is gone from the file this test checks`);
  const end = src.indexOf("\n}", at);
  if (end < 0) throw new Error(`${name} has no closing brace at column 0`);
  return src.slice(at, end);
}

test("the kill timer dies with the worker it would kill", () => {
  const stop = body(playJs, "stopWorker");
  if (!stop.includes("clearTimeout(runTimer)")) {
    throw new Error(
      "stopWorker does not clear runTimer, so every path that abandons a live run " +
        "(a second Run, Ctrl-Enter, Reset) leaves the old timeout armed over the next one",
    );
  }
});

test("the kill timer is module state, so one mount cannot hide another's", () => {
  const decls = [...playJs.matchAll(/^\s*let runTimer = null;$/gm)];
  if (decls.length !== 1) {
    throw new Error(`expected one runTimer declaration, found ${decls.length}`);
  }
  if (decls[0][0] !== "let runTimer = null;") {
    throw new Error("runTimer is declared indented, which means inside mountPlay — stopWorker cannot reach it");
  }
  if (playJs.indexOf("let runTimer") > playJs.indexOf("function stopWorker(")) {
    throw new Error("runTimer is declared after stopWorker rather than beside the worker it belongs to");
  }
});

test("the hero editor replays a Run press that arrived after hover or focus armed it", () => {
  const arm = body(widgetsJs, "armHeroEditor");
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
