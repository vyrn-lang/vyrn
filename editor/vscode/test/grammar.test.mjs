// The TextMate grammar's keyword list is the `keyword` lines of `vyrn.syntax`.
//
// A TextMate grammar is a second copy of a fact the compiler already holds, and
// a second copy drifts. This one had: `from` and `logging` were coloured as
// keywords while no `keyword` line holds either, so `let from = 1` and
// `let logging = 2`, both ordinary bindings the parser accepts, read as reserved
// words in the editor.
//
// `vyrn.syntax` is the source. This test reads its `keyword` lines and compares
// them to the alternation in `#keywords`, so a keyword added to the language and
// not to the TextMate grammar fails here, and a word coloured as one that the
// language never reserved fails here too.
//
// CONTEXTUAL WORDS ARE NOT KEYWORDS and are deliberately not compared. `as`,
// `gen`, `extern`, `lazy`, `read`, `modify`, `test`, `bench`, `from`,
// `logging`, `to` and `await` are identifiers the parser recognises by position; the grammar
// matches each with its own lookahead in `#contextual-keywords`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "..", "..", "..");

/** Every word a `keyword` line of `vyrn.syntax` reserves. */
async function reservedWords() {
  const src = await readFile(
    path.join(repo, "compiler", "vyrn-syntax", "vyrn.syntax"),
    "utf8",
  );
  const words = [...src.matchAll(/^keyword \w+ '([a-z]+)'$/gm)].map((m) => m[1]);
  assert.ok(words.length > 15, `only ${words.length} keyword lines found`);
  return new Set(words);
}

/** The words `#keywords` colours unconditionally. */
async function grammarKeywords() {
  const g = JSON.parse(
    await readFile(path.join(here, "..", "vyrn.tmLanguage.json"), "utf8"),
  );
  const m = /^\\b\((.+)\)\\b$/.exec(g.repository.keywords.match);
  assert.ok(m, `#keywords is not a plain alternation any more: ${g.repository.keywords.match}`);
  return new Set(m[1].split("|"));
}

test("the TextMate grammar colours exactly the words vyrn.syntax reserves", async () => {
  const lex = await reservedWords();
  const gram = await grammarKeywords();
  const coloured = [...gram].filter((w) => !lex.has(w)).sort();
  const missed = [...lex].filter((w) => !gram.has(w)).sort();
  assert.deepEqual(
    coloured,
    [],
    `coloured as keywords but not reserved by vyrn.syntax: ${coloured.join(", ")}`,
  );
  assert.deepEqual(
    missed,
    [],
    `reserved by vyrn.syntax but not coloured: ${missed.join(", ")}`,
  );
});

test("from and logging are matched in position, not as keywords", async () => {
  const g = JSON.parse(
    await readFile(path.join(here, "..", "vyrn.tmLanguage.json"), "utf8"),
  );
  const contextual = g.repository["contextual-keywords"].patterns.map((p) => p.match);
  const from = contextual.find((m) => m.includes("from"));
  const logging = contextual.find((m) => m.includes("logging"));
  assert.ok(from, "`from` has no contextual rule");
  assert.ok(logging, "`logging` has no contextual rule");

  // The rule fires where the language uses the word, and not on a binding of
  // that name. Anchored, because a TextMate match is applied to a whole line.
  const fires = (rule, line) => new RegExp(rule).test(line);
  assert.ok(fires(from, 'import { x } from "std/json"'), "a module path");
  assert.ok(fires(from, 'import { x } from vyxHints("./app")'), "a generator call");
  assert.ok(!fires(from, "let from = 1"), "`from` as a binding must stay plain");
  assert.ok(!fires(from, "    return from + 1"), "`from` as a value must stay plain");

  assert.ok(fires(logging, "logging { level: warn, sink: stderr }"), "a logging block");
  assert.ok(!fires(logging, "let logging = 2"), "`logging` as a binding must stay plain");
});

test("to is matched after export, before a path, and nowhere else", async () => {
  const g = JSON.parse(
    await readFile(path.join(here, "..", "vyrn.tmLanguage.json"), "utf8"),
  );
  const contextual = g.repository["contextual-keywords"].patterns.map((p) => p.match);
  const to = new RegExp(contextual.find((m) => m.includes("to\\b")));
  assert.ok(to.test('export to "./vyx.editor" fn vyxSlice('), "an export to clause");
  assert.ok(!to.test("let to = 1"), "`to` as a binding must stay plain");
  assert.ok(!to.test("export type to = Int64"), "`to` as a declared name must stay plain");
});

test("await is matched before a call, and nowhere else", async () => {
  const g = JSON.parse(
    await readFile(path.join(here, "..", "vyrn.tmLanguage.json"), "utf8"),
  );
  const contextual = g.repository["contextual-keywords"].patterns.map((p) => p.match);
  const marks = new RegExp(contextual.find((m) => m.includes("await")));
  assert.ok(marks.test("    let user = await getUser(id)?"), "a marked call");
  assert.ok(!marks.test("let await = 1"), "`await` as a binding must stay plain");
  assert.ok(!marks.test("    return await(x)"), "a call of a function named `await` must stay plain");
});

// A byte literal is scoped `constant.character`, which VS Code reads as code, so
// the `{` in `'{'` counted as a bracket and shifted every bracket colour after
// it. Each grammar entry maps the byte literal's scope to the string token type,
// which bracket matching skips.
test("every grammar entry treats a byte literal as a string", async () => {
  const g = JSON.parse(
    await readFile(path.join(here, "..", "vyrn.tmLanguage.json"), "utf8"),
  );
  const scope = g.repository["byte-literal"].name;
  const pkg = JSON.parse(await readFile(path.join(here, "..", "package.json"), "utf8"));
  for (const entry of pkg.contributes.grammars) {
    assert.equal(entry.tokenTypes?.[scope], "string", `${entry.language} does not map ${scope}`);
  }
});

// #function-calls needs `(` right after a name, so a generic declaration
// (`fn callWith<P, T>(`) lost its function colour. #fn-declaration names the
// declared function whatever follows it, and runs before #keywords so `fn`
// keeps its keyword scope as capture 1.
test("a generic function's declared name is coloured as a function", async () => {
  const g = JSON.parse(
    await readFile(path.join(here, "..", "vyrn.tmLanguage.json"), "utf8"),
  );
  const includes = g.patterns.map((p) => p.include);
  assert.ok(
    includes.indexOf("#fn-declaration") < includes.indexOf("#keywords"),
    "#fn-declaration must run before #keywords",
  );
  const rule = new RegExp(g.repository["fn-declaration"].match);
  for (const [line, name] of [
    ["fn callWith<P, T>(run: fn(P) -> T, p: P) -> T {", "callWith"],
    ["fn defer<P, T>(run: consume fn(P) -> T) -> Deferred<P, T> {", "defer"],
    ["fn apply(g: fn(Int64) -> Int64, v: Int64) -> Int64 {", "apply"],
  ]) {
    assert.equal(rule.exec(line)?.[2], name, line);
  }
  assert.equal(rule.exec("let g: fn(Int64) -> Int64 = h"), null, "a function type names nothing");
});
