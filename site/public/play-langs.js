// The icon a playground file shows: the svg text that the `icon` of the last
// `languages` entry of its nearest `vyrn.json` names, when that entry's glob
// matches the file. The glob rule is vyrn-frontend's manifest::glob_path.
const segs = (s) => s.split("/").filter((x) => x && x !== ".");

function globName(p, s) {
  const re = p.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
  return new RegExp(`^${re}$`, "s").test(s);
}

function globPath(g, p) {
  if (!g.length) return !p.length;
  if (g[0] === "**") return globPath(g.slice(1), p) || (p.length > 0 && globPath(g, p.slice(1)));
  return p.length > 0 && globName(g[0], p[0]) && globPath(g.slice(1), p.slice(1));
}

function joined(dir, rel) {
  const out = segs(dir);
  for (const s of segs(rel)) {
    if (s === "..") out.pop();
    else out.push(s);
  }
  return out.join("/");
}

// `files` maps each project path to its text; null means no icon.
export function languageIcon(files, name) {
  const parts = segs(name);
  for (let i = parts.length - 1; i >= 0; i--) {
    const dir = parts.slice(0, i).join("/");
    const manifest = files.get(joined(dir, "vyrn.json"));
    if (manifest === undefined) continue;
    let languages;
    try {
      languages = JSON.parse(manifest).languages;
    } catch {
      return null;
    }
    if (!Array.isArray(languages)) return null;
    const rel = parts.slice(i);
    const entry = languages.findLast((l) => l && typeof l.files === "string" && globPath(segs(l.files), rel));
    if (!entry || typeof entry.icon !== "string") return null;
    return files.get(joined(dir, entry.icon)) ?? null;
  }
  return null;
}
