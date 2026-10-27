import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { instantiatePlay } from "../public/play-wasm.js";
import { runVyrn } from "../../web/wasi-min.js";

const root = new URL("../../", import.meta.url);
const bytes = await readFile(fileURLToPath(new URL("out/play.wasm", root)));

const src = `export gen fn fieldsOf(t: TypeArg) -> String {
    let mut out = ""
    for r in t.roots {
        let n = t.nodes[r]
        let mut names = ""
        for m in n.members {
            names = names + " " + m.name
        }
        out = out + "fn " + n.name + "(v: " + n.spelling + ") -> String {\n    return \\"" + n.kind + names + "\\"\n}\n"
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
  assert.deepEqual(play.check(src).diagnostics, []);
  const answer = play.compile(src);
  assert.ok(answer.module, JSON.stringify(answer));
  const { exitCode, stdout } = await runVyrn(answer.module, {});
  assert.equal(stdout, "record x y\nString\n");
  assert.equal(exitCode, 0);
});

test("a generator that asks the page for more than its TypeArg is refused, not run", async () => {
  const play = await instantiatePlay(bytes);
  const reads = src.replace('let mut out = ""', 'let mut out = moduleInterface("./x").types.length.toString()');
  const { diagnostics } = play.compile(reads);
  assert.deepEqual(
    diagnostics.map((d) => d.message),
    ["the playground serves a generator its TypeArg and nothing else"],
  );
});

const code = `export gen fn fieldsOf(t: TypeArg) -> String {
    let n = t.nodes[t.roots[0]]
    let mut names = n.kind
    for m in n.members {
        names = names + " " + m.name
    }
    return render(vyrn"fn \\{name(n.name)}(v: \\{name(n.spelling)}) -> String { return \\{names} }")
}

type Point = { x: Int64, y: Int64 }

fn main() -> Int64 {
    print(derive(fieldsOf, Point { x: 1, y: 2 }))
    return 0
}
`;

test("a generator's code quote runs its imports in the compiler", async () => {
  const play = await instantiatePlay(bytes);
  assert.deepEqual(play.check(code).diagnostics, []);
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
