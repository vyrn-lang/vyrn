// Every command the extension registers is declared, and every declared one exists.
//
// THE DRIFT THIS EXISTS FOR: `extension.js` registered `vyrn.bench` and
// `vyrn.benchAll` and `package.json` declared neither, for as long as benches
// have had CodeLenses. Nothing was visibly broken — a CodeLens invokes a command
// by id whether or not it is declared — so the two bench commands worked from a
// lens and were absent from the Command Palette, which is the one place a reader
// looks for "what can this extension do".
//
// The other direction matters too: a command declared and not registered shows
// up in the palette and fails with "command not found" when clicked, which is
// worse than not being there.
//
// Run: node --test editor/vscode/test/commands.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const dir = new URL("../", import.meta.url);
const read = (p) => readFile(fileURLToPath(new URL(p, dir)), "utf8");

const [source, manifestText] = await Promise.all([
  read("extension.js"),
  read("package.json"),
]);
const manifest = JSON.parse(manifestText);

// `vsc.commands.registerCommand("id", ...)` — the only way this extension
// registers one, and a textual scan is the right tool because the alternative is
// loading the extension outside VS Code.
const registered = new Set(
  [...source.matchAll(/registerCommand\(\s*"([^"]+)"/g)].map((m) => m[1]),
);
const declared = new Set(manifest.contributes.commands.map((c) => c.command));

test("the scan found the registrations", () => {
  assert.ok(registered.size >= 6, `only ${registered.size} registerCommand calls found — the shape changed`);
});

// A leading underscore is VS Code's own convention for a command that is not
// user-facing. `vyrn._refreshDevLens` exists so the extension can nudge itself
// to re-request CodeLenses once the language server is up; putting it in the
// palette would offer a reader a button that appears to do nothing.
const userFacing = (c) => !c.startsWith("vyrn._");

test("every registered command is declared in package.json", () => {
  const missing = [...registered].filter(userFacing).filter((c) => !declared.has(c)).sort();
  assert.deepEqual(
    missing,
    [],
    `registered but not in the Command Palette: ${missing.join(", ")}`,
  );
});

test("every declared command is registered in extension.js", () => {
  const dangling = [...declared].filter((c) => !registered.has(c)).sort();
  assert.deepEqual(
    dangling,
    [],
    `declared but never registered — clicking these fails: ${dangling.join(", ")}`,
  );
});

// A command with no title is invisible in the palette even though it is there,
// and one whose title does not say Vyrn is unfindable among a hundred others.
test("an internal command stays out of the palette", () => {
  const leaked = [...declared].filter((c) => !userFacing(c)).sort();
  assert.deepEqual(leaked, [], `underscore-prefixed commands should not be declared: ${leaked.join(", ")}`);
});

test("every declared command has a title that names the language", () => {
  for (const c of manifest.contributes.commands) {
    assert.ok(c.title, `${c.command} has no title`);
    assert.ok(
      c.title.startsWith("Vyrn: "),
      `${c.command} is titled ${JSON.stringify(c.title)}, which will not be found by typing "vyrn"`,
    );
  }
});

// Each CodeLens names a command that exists. A lens with a typo'd id renders
// fine and does nothing at all when clicked.
test("every CodeLens invokes a registered command", () => {
  const invoked = new Set(
    [...source.matchAll(/command:\s*"(vyrn\.[A-Za-z]+)"/g)].map((m) => m[1]),
  );
  assert.ok(invoked.size > 0, "no CodeLens commands found — the shape changed");
  const unknown = [...invoked].filter((c) => !registered.has(c)).sort();
  assert.deepEqual(unknown, [], `CodeLenses pointing at nothing: ${unknown.join(", ")}`);
});

// `--profile` goes BEFORE the file, and this is the test that says why.
//
// Everything past the file in `vyrn run` is the PROGRAM's own `args()`
// (RFC-0014), so `vyrn run app.vyrn --profile` hands the flag to `app.vyrn`,
// prints no table, and exits 0. That is correct CLI behaviour and it is silent,
// which makes the wrong order here indistinguishable from a broken profiler.
// It was written the wrong way round first.
test("the profile commands put the flag before the file", () => {
  const bodies = [...source.matchAll(/registerCommand\("vyrn\.profile[A-Za-z]*",[\s\S]{0,220}?\)\n/g)].map(
    (m) => m[0],
  );
  assert.ok(bodies.length >= 2, `found ${bodies.length} profile command bodies, expected 2`);
  for (const body of bodies) {
    const args = body.match(/\[([^\]]*)\]/);
    assert.ok(args, `no argument list in: ${body}`);
    const parts = args[1].split(",").map((s) => s.trim());
    const flagAt = parts.indexOf('"--profile"');
    const fileAt = parts.indexOf("file");
    assert.ok(flagAt >= 0, `no --profile in: ${args[1]}`);
    assert.ok(fileAt >= 0, `no file in: ${args[1]}`);
    assert.ok(
      flagAt < fileAt,
      `--profile must precede the file, or vyrn hands it to the program: ${args[1]}`,
    );
  }
});
