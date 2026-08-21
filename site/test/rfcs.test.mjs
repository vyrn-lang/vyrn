// THE ESCAPED-PIPE TABLES, ON THE PAGES (RFC-0045, 0076, 0083, 0086).
//
// Four design records write `\|` inside a table cell — a pipe in the cell's
// content, not a separator — and the renderer used to cut on every pipe byte,
// garbling the row into extra cells and publishing it. These are the four
// rows, as the fixed renderer must emit them, read off the tree the export
// wrote. The fragments are exact: an extra or a missing cell fails the
// includes, which is the corruption this guards against.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";

const OUT = path.resolve(import.meta.dirname, "../../out");

test("an escaped pipe renders as a pipe in the cell, on every page that wrote one", async () => {
  const cases = [
    ["backstage/rfcs/0045.html",
      // `| `a \| b` | or | same integer type | that type |` — four cells, the
      // code span holding the pipe whole.
      "<tr><td><code>a | b</code></td><td>or</td><td>same integer type</td><td>that type</td></tr>"],
    ["backstage/rfcs/0076.html",
      "<code>(status &lt;&lt; 32) | len</code>"],
    ["backstage/rfcs/0083.html",
      "<code>&amp;&amp;</code>/<code>||</code>/<code>!=</code>"],
    ["backstage/rfcs/0086.html",
      "<code>type Wrap&lt;T&gt; = | W({ v: T })</code>"],
  ];
  for (const [file, fragment] of cases) {
    const html = await readFile(path.join(OUT, ...file.split("/")), "utf8");
    assert.ok(html.includes(fragment), `${file} lost an escaped pipe: ${fragment}`);
  }
});
