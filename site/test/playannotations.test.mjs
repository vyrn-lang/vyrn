import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { instantiatePlay } from "../public/play-wasm.js";
import { runVyrn } from "../../web/wasi-min.js";

const root = new URL("../../", import.meta.url);
const bytes = await readFile(fileURLToPath(new URL("out/play.wasm", root)));

const gen = `export gen fn fieldsOf(t: TypeCode, iface: ModuleInterface) -> Code {
    let n = t.info.shape[0]
    let mut words = n.kind.copy()
    for m in n.members {
        words = words + " " + m.name
    }
    return vyrn"fn \\{name("fieldsOf" + t.info.name)}(v: \\{name(t.info.name)}) -> String {\\n    return \\{words}\\n}\\n"
}
`;

const src = `import { fieldsOf } from "./fields"

@fieldsOf
type Point = { x: Int64, y: Int64 }

fn main() -> Int64 {
    print(fieldsOfPoint(Point { x: 1, y: 2 }))
    return 0
}
`;

const project = (g) => ({ path: "main.vyrn", files: { "fields.vyrn": g } });

test("an annotation runs its generator in the page and the program prints what it wrote", async () => {
  const play = await instantiatePlay(bytes);
  assert.deepEqual(play.check(src, "vyrn", project(gen)).diagnostics, []);
  const answer = play.compile(src, project(gen));
  assert.ok(answer.module, JSON.stringify(answer));
  const { exitCode, stdout } = await runVyrn(answer.module, {});
  assert.equal(stdout, "record x y\n");
  assert.equal(exitCode, 0);
});

test("a generator that reflects on a module is refused, naming the call", async () => {
  const play = await instantiatePlay(bytes);
  const reads = gen.replace("let mut words = n.kind.copy()", 'let mut words = moduleInterface("./x").types.length.toString()');
  const { diagnostics } = play.compile(src, project(reads));
  const said = "the playground cannot run `fieldsOf`: its module calls `moduleInterface`, which the page's host does not serve";
  assert.ok(diagnostics.some((d) => d.message.includes(said)), JSON.stringify(diagnostics));
});

const calc = await readFile(fileURLToPath(new URL("examples/syntaxcalc.vyrn", root)), "utf8");
const calcGrammar = await readFile(fileURLToPath(new URL("examples/syntaxcalc.syntax", root)), "utf8");

test("syntax() reads a grammar tab, and the program prints what vyrn run prints", async () => {
  const play = await instantiatePlay(bytes);
  const calcProject = { path: "syntaxcalc.vyrn", files: { "syntaxcalc.syntax": calcGrammar } };
  const answer = play.compile(calc, calcProject);
  assert.ok(answer.module, JSON.stringify(answer));
  const { exitCode, stdout } = await runVyrn(answer.module, {});
  assert.ok(stdout.startsWith("let a = (|| (== (>= (+ 1 (* 2 (^ 3 (^ 4 5)))) 6) 7) 8)\n"), stdout);
  assert.equal(exitCode, 0);
});

test("a broken grammar tab is refused in the grammar's own place", async () => {
  const play = await instantiatePlay(bytes);
  const calcProject = { path: "syntaxcalc.vyrn", files: { "syntaxcalc.syntax": "syntax T\nexpect ^E\nA = 'a\n" } };
  const [d] = play.check(calc, "vyrn", calcProject).diagnostics;
  assert.deepEqual([d.file, d.line, d.col, d.message], ["syntaxcalc.syntax", 3, 7, "rule A: an unclosed literal"]);
});


test("a generator's refusal in the page is the engine's sentence", async () => {
  const play = await instantiatePlay(bytes);
  const spaced = gen.replace('name(t.info.name)}', 'name(t.info.name + " a")}');
  const { diagnostics } = play.compile(src, project(spaced));
  assert.ok(
    diagnostics.some((d) => d.message.includes('`name` needs one identifier, and "Point a" is not one')),
    JSON.stringify(diagnostics),
  );
});
