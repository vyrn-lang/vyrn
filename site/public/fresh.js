// The one network call the site makes (design brief section 5, step 3).
//
// The page is BAKED: `site/release.txt` carries the newest published tag, the
// site workflow refreshes it from the same GitHub listing before every build,
// and every release sentence — the install line, the version, the date — is in
// the exported HTML before a script runs. This file exists for the window
// between a release being cut and the site being rebuilt, and it does exactly
// one thing in it: says that a newer release exists.
//
// It is therefore SILENT when the baked tag is the newest one, which is almost
// always. The version the page shows is never rewritten from here: a page that
// quietly swapped its own version string would be claiming to have been built
// against a release it was not.
//
// Constraints that shape it: the unauthenticated GitHub API allows 60 requests
// an hour per address, so one call a visit with an hour of caching is safe; no
// token may ever appear in the page; and a pre-release appears only in the
// listing endpoint, which is why this reads `/releases` and not
// `/releases/latest` — every release so far is a pre-release.
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

/// Say that a newer release exists, when one does, and nothing otherwise.
///
/// `data-release-note` carries the tag the page was built against — empty when
/// the page was built with nothing released. Every element it touches is
/// optional, so a page without them is fine.
export async function refreshRelease() {
  const note = document.querySelector("[data-release-note]");
  if (!note) return;
  const baked = note.getAttribute("data-release-note");
  let rows = null;
  try {
    rows = await load();
  } catch (err) {
    return; // stay with the baked page, silently
  }
  if (!rows || rows.length === 0) return;
  // Newest first, pre-releases included — the listing's own order, and what
  // both installers resolve to.
  const newest = rows[0];
  if (newest.tag === baked) return; // the page is current: say nothing at all
  note.replaceChildren(
    document.createTextNode(
      baked
        ? `A newer release, ${newest.tag}, is available${newest.pre ? " as a pre-release" : ""}. `
        : `${newest.tag} is published${newest.pre ? " as a pre-release" : ""}. `,
    ),
    link(newest),
    document.createTextNode(". The commands below install the newest release, so they already fetch it."),
  );
  note.hidden = false;
}

/// The release's own page, as a link rather than a bare URL.
function link(release) {
  const a = document.createElement("a");
  a.href = release.url;
  a.textContent = "Release notes";
  return a;
}
