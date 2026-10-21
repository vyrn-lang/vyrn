// A second click while a soft navigation reads its body must take the hard
// navigation, never start a second painter: `inflight` is cleared only once the
// body is in. Node has no DOM or fetch to drive `navigate`, so this reads the rule.
//
// Run: node --test web/test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const src = await readFile(new URL("../vyrn-nav.js", import.meta.url), "utf8");

test("the in-flight guard holds from the fetch until the body is read", () => {
  const answered = [...src.matchAll(/clearTimeout\(timer\);\n\s*if \(!res\.ok\)/g)];
  assert.equal(answered.length, 2, "both fetching paths keep the guard past the headers");
  for (const m of answered) {
    const read = src.indexOf("await res.text()", m.index);
    assert.ok(read > m.index, "a body read follows the headers");
    assert.ok(!src.slice(m.index, read).includes("inflight = null"), "the guard is cleared before the body");
  }
});
