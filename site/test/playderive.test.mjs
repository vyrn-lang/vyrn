import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { instantiatePlay } from "../public/play-wasm.js";
import { runVyrn } from "../../web/wasi-min.js";

const root = new URL("../../", import.meta.url);
const bytes = await readFile(fileURLToPath(new URL("out/play.wasm", root)));

const src = `export gen fn fieldsOf(t: TypeArg) -> Code {
    let mut out = vyrn""
    for r in t.roots {
        let n = t.nodes[r]
        let mut words = n.shape.kind.copy()
        for m in n.shape.members {
            words = words + " " + m.name
        }
        out = out + vyrn"fn \\{name(n.shape.name)}(v: \\{n.code}) -> String {\\n    return \\{words}\\n}\\n"
    }
    return out
}

type Point = { x: Int64, y: Int64 }

fn main() -> Int64 {
    print(derive(fieldsOf, Point { x: 1, y: 2 }))
    print(derive(fieldsOf, "text"))
    return 0
}
`;

test("derive runs its generator in the page and the program prints what it wrote", async () => {
  const play = await instantiatePlay(bytes);
  assert.deepEqual(play.check(src, "vyrn").diagnostics, []);
  const answer = play.compile(src);
  assert.ok(answer.module, JSON.stringify(answer));
  const { exitCode, stdout } = await runVyrn(answer.module, {});
  assert.equal(stdout, "record x y\nString\n");
  assert.equal(exitCode, 0);
});

test("a generator that reflects on a module is refused, naming the call", async () => {
  const play = await instantiatePlay(bytes);
  const reads = src.replace("let mut words = n.shape.kind.copy()", 'let mut words = moduleInterface("./x").types.length.toString()');
  const { diagnostics } = play.compile(reads);
  const said = "the playground cannot run `fieldsOf`: its module calls `moduleInterface`, which the page's host does not serve";
  assert.ok(diagnostics.some((d) => d.message.includes(said)), JSON.stringify(diagnostics));
});

const calc = await readFile(fileURLToPath(new URL("examples/syntaxcalc.vyrn", root)), "utf8");
const calcGrammar = await readFile(fileURLToPath(new URL("examples/syntaxcalc.syntax", root)), "utf8");

test("syntax() reads a grammar tab, and the program prints what vyrn run prints", async () => {
  const play = await instantiatePlay(bytes);
  const project = { path: "syntaxcalc.vyrn", files: { "syntaxcalc.syntax": calcGrammar } };
  const answer = play.compile(calc, project);
  assert.ok(answer.module, JSON.stringify(answer));
  const { exitCode, stdout } = await runVyrn(answer.module, {});
  assert.ok(stdout.startsWith("let a = (|| (== (>= (+ 1 (* 2 (^ 3 (^ 4 5)))) 6) 7) 8)\n"), stdout);
  assert.equal(exitCode, 0);
});

test("a broken grammar tab is refused in the grammar's own place", async () => {
  const play = await instantiatePlay(bytes);
  const project = { path: "syntaxcalc.vyrn", files: { "syntaxcalc.syntax": "syntax T\nexpect ^E\nA = 'a\n" } };
  const [d] = play.check(calc, "vyrn", project).diagnostics;
  assert.deepEqual([d.file, d.line, d.col, d.message], ["syntaxcalc.syntax", 3, 7, "rule A: an unclosed literal"]);
});

const code = `export gen fn fieldsOf(t: TypeArg) -> Code {
    let n = t.nodes[t.roots[0]].shape
    let mut names = n.kind
    for m in n.members {
        names = names + " " + m.name
    }
    return vyrn"fn \\{name(n.name)}(v: \\{name(n.spelling)}) -> String { return \\{names} }"
}

type Point = { x: Int64, y: Int64 }

fn main() -> Int64 {
    print(derive(fieldsOf, Point { x: 1, y: 2 }))
    return 0
}
`;

test("a derive generator that returns Code runs its imports in the compiler", async () => {
  const play = await instantiatePlay(bytes);
  assert.deepEqual(play.check(code, "vyrn").diagnostics, []);
  const answer = play.compile(code);
  assert.ok(answer.module, JSON.stringify(answer));
  const { exitCode, stdout } = await runVyrn(answer.module, {});
  assert.equal(stdout, "record x y\n");
  assert.equal(exitCode, 0);
});

test("a code import's refusal in the page is the engine's sentence", async () => {
  const play = await instantiatePlay(bytes);
  const { diagnostics } = play.compile(code.replace("name(n.spelling)", 'name(n.spelling + " a")'));
  assert.ok(
    diagnostics.some((d) => d.message.includes('`name` needs one identifier, and "Point a" is not one')),
    JSON.stringify(diagnostics),
  );
});
