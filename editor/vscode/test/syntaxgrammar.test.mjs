// Tokenizes examples/syntaxjson.syntax with syntax.tmLanguage.json, as VS Code does: line by
// line, the leftmost match of the patterns in force, the first pattern on a tie, and a begin
// rule's end before its own patterns. JavaScript has no atomic group and no \z, so each regex
// reads (?> as (?: and \z as the end of the text; neither changes a match in this grammar.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const read = (file) => readFile(path.resolve(here, file), "utf8");
const regex = (re) => new RegExp(re.replaceAll("(?>", "(?:").replaceAll("\\z", "(?![\\s\\S])"), "g");

function leftmost(rules, line, at) {
  let best = null;
  for (const rule of rules) {
    const re = regex(rule.match ?? rule.begin ?? rule.end);
    re.lastIndex = at;
    const m = re.exec(line);
    if (m && (!best || m.index < best.m.index)) best = { rule, m };
  }
  return best;
}

function tokens(grammar, text) {
  const out = [];
  const stack = [];
  for (const body of text.split(/(?<=\n)/)) {
    let at = 0;
    while (at < body.length) {
      const open = stack.at(-1);
      const rules = open ? [{ end: open.end, close: true }, ...open.patterns] : grammar.patterns;
      const hit = leftmost(rules, body, at);
      if (!hit) break;
      const { rule, m } = hit;
      const width = Math.max(m[0].length, 1);
      if (rule.close) stack.pop();
      else if (rule.begin) stack.push(rule);
      out.push({ text: m[0], scope: rule.close ? open.name : rule.name });
      at = m.index + width;
    }
  }
  return out;
}

test("each part of a .syntax file takes its scope", async () => {
  const grammar = JSON.parse(await read("../syntax.tmLanguage.json"));
  const got = tokens(grammar, await read("../../../examples/syntaxjson.syntax"));
  for (const [text, scope] of [
    ["// JSON", "comment.line"],
    ["syntax", "keyword.control"],
    ["Obj", "entity.name.function"],
    ["Member", "entity.name.type"],
    ["hex", "support.function"],
    ["[", "constant.other.character-class.regexp"],
    ["\\\\", "constant.character.escape"],
    ["^BadEscape", "entity.name.label"],
    ["members", "variable.parameter"],
    ["/", "keyword.operator"],
    ["'", "string.quoted"],
  ]) {
    const want = scope + ".syntax";
    assert.ok(got.some((t) => t.text.startsWith(text) && t.scope.startsWith(want)), `${text} as ${want}`);
  }
  const escapes = got.filter((t) => t.scope.startsWith("constant.character.escape"));
  assert.deepEqual([...new Set(escapes.map((t) => t.text[0]))], ["\\"]);
});
