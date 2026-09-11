// State that outlives the run it belongs to.
//
// The playground's kill timer was held per-mount and cleared only by the path
// that finished a run, so a second `Run`, a Ctrl-Enter or a `Reset` left the
// first run's timeout armed; five seconds later it terminated whatever was
// running by then and wrote "Stopped after 5 seconds." over a run that had
// already succeeded.
//
// This is not reachable from node: it needs a DOM, a Worker and a clock. What
// this file checks is the rule the fix states, in the one place it is stated.
//
// Run: node --test site/test/playlifecycle.test.mjs
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const playJs = await read("site/public/play.js");

// The text of a function, from its `function <name>(` to the `\n}` that closes
// it. Every function in this file is written at the column its body is closed
// at, so the first line-initial `}` ends it.
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
