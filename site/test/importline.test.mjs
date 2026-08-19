// Every import line the explorer prints is a line that compiles (RFC-0105 M2).
//
// A package page carries the line a reader copies — `import { … } from
// "std/json"`, naming every export the reference renders — in the box and in a
// `data-import` attribute beside it. A line that reads like code and does not
// build is worse than no line: the reader types it, the compiler refuses, and
// the page was the one that lied.
//
// So the line is taken out of the EXPORTED html, written into a program of its
// own, and put through `vyrn check`. Nothing is parsed or reconstructed on the
// way: what is compiled is the bytes a reader would paste.
//
// It lives here rather than in `site/export.vyrn` for one reason — that program
// cannot start another one. It is also the right side of the M1 timing lesson:
// the export's own gates walk `sitePaths()` six to ten times, and thirty-four
// compiler runs do not belong in that walk.
//
// Run: node --test site/test/*.test.mjs   (after `vyrn run site/export.vyrn out`
// and a release build of the CLI)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir, writeFile, mkdtemp, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import path from "node:path";

const run = promisify(execFile);
const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const OUT = path.join(ROOT, "out", "explore");

/// The compiler this repository just built. There is no fallback to a `vyrn` on
/// `PATH`: a gate that silently checks somebody else's compiler is not this
/// repository's gate.
const VYRN = ["vyrn.exe", "vyrn"]
  .map((n) => path.join(ROOT, "compiler", "target", "release", n))
  .find((p) => existsSync(p));

/// An attribute value as HTML text, back to the bytes it was written from.
/// `std/html` escapes exactly these five.
const unescape = (s) =>
  s
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&amp;", "&");

async function importLines() {
  const out = [];
  for (const name of await readdir(OUT).catch(() => [])) {
    if (!name.endsWith(".html")) continue;
    const html = await readFile(path.join(OUT, name), "utf8");
    for (const m of html.matchAll(/ data-import="([^"]*)"/g)) out.push({ page: name, line: unescape(m[1]) });
  }
  return out;
}

const lines = await importLines();

test("the package pages are there, with an import line on each module's own", () => {
  assert.ok(VYRN, `no release build of the compiler at compiler/target/release — run: cargo build --release -p vyrn-cli`);
  // 34 standard library modules today, and the four example projects have no
  // import line at all. A floor, so the list growing does not need an edit here
  // and the list emptying does not pass in silence.
  assert.ok(lines.length >= 30, `only ${lines.length} import line(s) found in out/explore — run: vyrn run site/export.vyrn out`);
  for (const { page, line } of lines) {
    assert.match(line, /^import \{ .+ \} from "std\/[a-z0-9-]+"$/, `${page}: not an import line`);
  }
});

test("every import line compiles", async () => {
  const dir = await mkdtemp(path.join(tmpdir(), "vyrn-importline-"));
  try {
    // Eight at a time. One at a time takes a minute of wall clock for nothing;
    // all thirty-four at once is thirty-four compilers on a two-core runner.
    const queue = [...lines];
    const failures = [];
    const worker = async () => {
      for (let job = queue.shift(); job; job = queue.shift()) {
        // A program, not a fragment: the import, and a `main` that uses none of
        // it. What is being checked is that the names are importable, which is
        // exactly what a reader who pastes the line finds out first.
        const file = path.join(dir, `${job.page.replace(/\.html$/, "")}.vyrn`);
        await writeFile(file, `${job.line}\nfn main() -> Int64 {\n    return 0\n}\n`, "utf8");
        try {
          await run(VYRN, ["check", file]);
        } catch (e) {
          failures.push(`${job.page}: ${(e.stderr || e.stdout || e.message).trim()}`);
        }
      }
    };
    await Promise.all(Array.from({ length: 8 }, worker));
    assert.deepEqual(failures, [], `an import line on a package page does not compile:\n${failures.join("\n")}`);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
