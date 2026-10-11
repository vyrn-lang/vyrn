import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const EXPLORE = path.join(ROOT, "out", "explore");

const unescape = (s) =>
  s
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&amp;", "&");

async function attribute(dir, name) {
  const out = [];
  const re = new RegExp(` ${name}="([^"]*)"`, "g");
  for (const file of await readdir(dir).catch(() => [])) {
    if (!file.endsWith(".html")) continue;
    const html = await readFile(path.join(dir, file), "utf8");
    for (const m of html.matchAll(re)) out.push({ page: file, value: unescape(m[1]) });
  }
  return out;
}

/// `resolve_to_url` in `compiler/vyrn-cli/src/remote.rs`, rule for rule.
export function resolveToUrl(spec) {
  if (spec.startsWith("github:")) {
    const rest = spec.slice("github:".length);
    const at = rest.indexOf("@");
    if (at < 0) throw new Error("github specifier needs `@ref`");
    const ownerRepo = rest.slice(0, at);
    const after = rest.slice(at + 1);
    let ref, filePath;
    if (after.startsWith("ref=")) {
      const slash = after.indexOf("/");
      if (slash < 0) throw new Error("`@ref=<ref>/<path>` needs the file path after the ref");
      ref = after.slice(4, slash);
      filePath = after.slice(slash);
    } else {
      const slash = after.indexOf("/");
      if (slash < 0) throw new Error("github specifier needs a file path");
      ref = after.slice(0, slash);
      filePath = after.slice(slash);
    }
    const sha = /^[0-9a-fA-F]{40}$/.test(ref) ? ref : "<sha>";
    return `https://raw.githubusercontent.com/${ownerRepo}/${sha}${filePath}`;
  }
  if (spec.startsWith("gist:")) {
    const segs = spec.slice("gist:".length).split("/");
    if (segs.length < 3) throw new Error("gist specifier needs user/id/file");
    const [user, idRev, ...rest] = segs;
    const file = rest.join("/");
    const cut = idRev.indexOf("@");
    const id = cut < 0 ? idRev : idRev.slice(0, cut);
    const rev = cut < 0 ? null : idRev.slice(cut + 1);
    return rev === null
      ? `https://gist.githubusercontent.com/${user}/${id}/raw/${file}`
      : `https://gist.githubusercontent.com/${user}/${id}/raw/${rev}/${file}`;
  }
  if (spec.startsWith("https://")) return spec;
  throw new Error(`not a remote specifier: ${spec}`);
}

const specs = await attribute(EXPLORE, "data-spec");

test("every install specifier resolves under the compiler's own grammar", () => {
  assert.ok(specs.length >= 4, `only ${specs.length} specifier(s) found in out/explore — run: vyrn run site/export.vyrn out`);
  for (const { page, value } of specs) {
    const url = resolveToUrl(value);
    assert.match(
      url,
      /^https:\/\/raw\.githubusercontent\.com\/[^/]+\/[^/]+\/[0-9a-f<>a-z]+\/.+\.vyrn$/,
      `${page}: \`${value}\` resolves to ${url}`,
    );
    assert.ok(value.endsWith(".vyrn"), `${page}: \`${value}\` does not name a module`);
    assert.ok(url.endsWith(value.slice(value.indexOf("/", value.indexOf("@")))), `${page}: path lost in resolution`);
  }
});

test("the grammar refuses what the compiler refuses", () => {
  assert.throws(() => resolveToUrl("github:vyrn-lang/vyrn/examples/shelf/shared/wire.vyrn"), /needs `@ref`/);
  assert.throws(() => resolveToUrl("github:vyrn-lang/vyrn@main"), /needs a file path/);
  assert.throws(() => resolveToUrl("gist:user/id"), /needs user\/id\/file/);
  assert.throws(() => resolveToUrl("http://x.dev/m.vyrn"), /not a remote specifier/);
  assert.throws(() => resolveToUrl("./local.vyrn"), /not a remote specifier/);
  const sha = "a".repeat(40);
  assert.equal(resolveToUrl(`github:o/r@${sha}/src/x.vyrn`), `https://raw.githubusercontent.com/o/r/${sha}/src/x.vyrn`);
  assert.equal(resolveToUrl("gist:u/abc123/f.vyrn"), "https://gist.githubusercontent.com/u/abc123/raw/f.vyrn");
  assert.equal(resolveToUrl("gist:u/abc123@rev9/f.vyrn"), "https://gist.githubusercontent.com/u/abc123/raw/rev9/f.vyrn");
  assert.equal(resolveToUrl("https://x.dev/m.vyrn"), "https://x.dev/m.vyrn");
  assert.equal(resolveToUrl(`github:o/r@ref=${sha}/src/x.vyrn`), `https://raw.githubusercontent.com/o/r/${sha}/src/x.vyrn`);
  assert.throws(() => resolveToUrl("github:o/r@ref=main"), /needs the file path/);
});
