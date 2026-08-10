// The one network call the site makes (design brief section 5, step 3).
//
// The baked page is what renders. This runs afterwards and may replace one
// version string. It never blocks rendering, never retries, and on any failure
// leaves the baked page exactly as it was.
//
// Constraints that shape it: the unauthenticated GitHub API allows 60 requests
// an hour per address, so one call a visit with an hour of caching is safe; no
// token may ever appear in the page; and a pre-release appears only in the
// listing endpoint, which is why this reads `/releases` and not
// `/releases/latest`.
const API = "https://api.github.com/repos/vyrn-lang/vyrn/releases?per_page=10";
const KEY = "vyrn.releases";
const HOUR = 3600 * 1000;

async function load() {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) {
      const cached = JSON.parse(raw);
      if (Date.now() - cached.at < HOUR) return cached.rows;
    }
  } catch (err) {
    // A full or blocked localStorage is not a reason to skip the fetch.
  }
  const res = await fetch(API, { headers: { Accept: "application/vnd.github+json" } });
  if (!res.ok) return null;
  const rows = (await res.json()).map((r) => ({
    tag: r.tag_name,
    pre: !!r.prerelease,
    url: r.html_url,
  }));
  try {
    localStorage.setItem(KEY, JSON.stringify({ at: Date.now(), rows }));
  } catch (err) {
    // Not caching is slower, not wrong.
  }
  return rows;
}

/// Fill in the release line if the project has published one since this page was
/// built. Every element it touches is optional, so a page without them is fine.
export async function refreshRelease() {
  const note = document.querySelector("[data-release-note]");
  if (!note) return;
  let rows = null;
  try {
    rows = await load();
  } catch (err) {
    return; // stay with the baked page, silently
  }
  if (!rows || rows.length === 0) return;
  const stable = rows.find((r) => !r.pre) || rows[0];
  const tag = document.querySelector("[data-release-tag]");
  if (tag) tag.textContent = stable.tag;
  const link = document.querySelector("[data-release-link]");
  if (link) link.href = stable.url;
  note.hidden = false;
  note.textContent =
    `${stable.tag} is published${stable.pre ? " as a pre-release" : ""}. ` +
    "This page was built before it existed, so the commands below still build from source.";
}
