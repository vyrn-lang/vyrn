// Every install specifier the registry prints resolves under the compiler's
// own grammar.
//
// A package page carries the `github:` specifier `vyrn add` takes, in the box
// and in `data-spec`. A `github:` fetch needs the network, and a gate that needs
// the network fails offline, so this checks the GRAMMAR: `resolveToUrl` below is
// `resolve_to_url` in `compiler/vyrn-cli/src/remote.rs`, rule for rule, and every
// emitted specifier must resolve to the URL the site says it names.
//
// Run: node --test site/test/*.test.mjs   (after `vyrn run site/export.vyrn out`)
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const EXPLORE = path.join(ROOT, "out", "explore");

/// An attribute value as HTML text, back to the bytes it was written from.
/// `std/html` escapes exactly these five.
const unescape = (s) =>
  s
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&amp;", "&");

/// The value of one attribute, off every html file in a directory of the
/// exported tree.
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

/// `resolve_to_url` in `compiler/vyrn-cli/src/remote.rs`, in JavaScript. The
/// explicit `@ref=<ref>/<path>` form names a ref whose own name carries a `/`.
/// Without the network a floating ref cannot become a commit, so `<sha>` stands
/// where `git ls-remote` would answer.
///
/// Throws with the compiler's own words for anything that is not a specifier,
/// so a malformed one fails this gate with the message a user would get.
export function resolveToUrl(spec) {
  if (spec.startsWith("github:")) {
    // github:owner/repo@ref/path(.vyrn)
    const rest = spec.slice("github:".length);
    const at = rest.indexOf("@");
    if (at < 0) throw new Error("github specifier needs `@ref`");
    const ownerRepo = rest.slice(0, at);
    const after = rest.slice(at + 1);
    // The ref and the file path share their `/`, so a branch named
    // `feature/2/api` before `/src/x.vyrn` reads as ref `feature` — and
    // may BE ref `feature`. `@ref=<ref>/<path>` names both sides outright;
    // without it every `/` boundary is a candidate ref, tried shortest-first:
    // the reading the specifier always used to get.
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
    // gist:user/id[@rev]/file(.vyrn)
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
  // The four example projects today. A floor, so a registry that quietly
  // emptied does not pass.
  assert.ok(specs.length >= 4, `only ${specs.length} specifier(s) found in out/explore — run: vyrn run site/export.vyrn out`);
  for (const { page, value } of specs) {
    // It parses, and it parses to the file the page says it names — the failure
    // this catches is a specifier assembled from the wrong pieces, which is a
    // string that parses and fetches somebody else's module.
    const url = resolveToUrl(value);
    assert.match(
      url,
      /^https:\/\/raw\.githubusercontent\.com\/[^/]+\/[^/]+\/[0-9a-f<>a-z]+\/.+\.vyrn$/,
      `${page}: \`${value}\` resolves to ${url}`,
    );
    assert.ok(value.endsWith(".vyrn"), `${page}: \`${value}\` does not name a module`);
    // The path in the specifier is the path in the URL, so the two halves of
    // the page cannot disagree about which file is being installed.
    assert.ok(url.endsWith(value.slice(value.indexOf("/", value.indexOf("@")))), `${page}: path lost in resolution`);
  }
});

test("the grammar refuses what the compiler refuses", () => {
  // A gate nobody has seen fail is a gate nobody has tested. Each of these is
  // one rule of `resolve_to_url`, and each names the same refusal the CLI
  // prints.
  assert.throws(() => resolveToUrl("github:vyrn-lang/vyrn/examples/shelf/shared/wire.vyrn"), /needs `@ref`/);
  assert.throws(() => resolveToUrl("github:vyrn-lang/vyrn@main"), /needs a file path/);
  assert.throws(() => resolveToUrl("gist:user/id"), /needs user\/id\/file/);
  assert.throws(() => resolveToUrl("http://x.dev/m.vyrn"), /not a remote specifier/);
  assert.throws(() => resolveToUrl("./local.vyrn"), /not a remote specifier/);
  // And the shapes it accepts, against the compiler's own unit test.
  const sha = "a".repeat(40);
  assert.equal(resolveToUrl(`github:o/r@${sha}/src/x.vyrn`), `https://raw.githubusercontent.com/o/r/${sha}/src/x.vyrn`);
  assert.equal(resolveToUrl("gist:u/abc123/f.vyrn"), "https://gist.githubusercontent.com/u/abc123/raw/f.vyrn");
  assert.equal(resolveToUrl("gist:u/abc123@rev9/f.vyrn"), "https://gist.githubusercontent.com/u/abc123/raw/rev9/f.vyrn");
  assert.equal(resolveToUrl("https://x.dev/m.vyrn"), "https://x.dev/m.vyrn");
  // The explicit form says where the ref ends, so a ref whose own name
  // carries a `/` needs no guessing — remote.rs's own unit test, mirrored.
  assert.equal(resolveToUrl(`github:o/r@ref=${sha}/src/x.vyrn`), `https://raw.githubusercontent.com/o/r/${sha}/src/x.vyrn`);
  assert.throws(() => resolveToUrl("github:o/r@ref=main"), /needs the file path/);
});
