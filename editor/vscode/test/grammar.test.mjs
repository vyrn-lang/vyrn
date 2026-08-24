// The grammar's keyword list is the lexer's keyword list.
//
// A TextMate grammar is a second copy of a fact the compiler already holds, and
// a second copy drifts. This one had: `from` and `logging` were coloured as
// keywords while `keyword_or_ident` in the lexer holds neither, so `let from =
// 1` and `let logging = 2` — both ordinary bindings the parser accepts — read as
// reserved words in the editor.
//
// The lexer is the source. This test reads its match arms and compares them to
// the alternation in `#keywords`, so a keyword added to the language and not to
// the grammar fails here, and a word coloured as one that the lexer never
// reserved fails here too.
//
// CONTEXTUAL WORDS ARE NOT KEYWORDS and are deliberately not compared. `as`,
// `gen`, `extern`, `lazy`, `place`, `read`, `modify`, `yield`, `test`, `bench`,
// `from` and `logging` are identifiers the parser recognises by position; the
// grammar matches each with its own lookahead in `#contextual-keywords`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "..", "..", "..");

/** Every word `keyword_or_ident` maps to a keyword token. */
async function lexerKeywords() {
  const src = await readFile(
    path.join(repo, "compiler", "vyrn-frontend", "src", "lexer.rs"),
    "utf8",
  );
  const at = src.indexOf("fn keyword_or_ident(");
  assert.ok(at > 0, "keyword_or_ident is gone from lexer.rs — this test needs a new anchor");
  const body = src.slice(at, src.indexOf("\n}", at));
  // `"fn" => Tok::Fn,` — the arms that name a token, not the `_ => Ident` fallback.
  const words = [...body.matchAll(/"([a-z]+)"\s*=>\s*Tok::/g)].map((m) => m[1]);
  assert.ok(words.length > 15, `only ${words.length} keyword arms found — the shape changed`);
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

test("the grammar colours exactly the words the lexer reserves", async () => {
  const lex = await lexerKeywords();
  const gram = await grammarKeywords();
  const coloured = [...gram].filter((w) => !lex.has(w)).sort();
  const missed = [...lex].filter((w) => !gram.has(w)).sort();
  assert.deepEqual(
    coloured,
    [],
    `coloured as keywords but not reserved by the lexer: ${coloured.join(", ")}`,
  );
  assert.deepEqual(
    missed,
    [],
    `reserved by the lexer but not coloured: ${missed.join(", ")}`,
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
