// Node has no DOM, Worker or clock, so these tests read the source that states each rule.
import { test } from "node:test";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, root)), "utf8");

const [playJs, widgetsJs] = await Promise.all([read("site/public/play.js"), read("site/public/widgets.js")]);

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
