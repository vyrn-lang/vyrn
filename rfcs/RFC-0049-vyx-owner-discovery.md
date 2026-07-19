# RFC-0049 — `.vyx` Owner Discovery & Cached Forward-Mapping (the LSP unblock)

- **Status:** Draft (design locked)
- **Depends on:** RFC-0033 (the `vyx_owner` registry + origin forward-map),
  RFC-0047/0048 (semantic tokens + `.vyx` script/page origins — all
  invisible without this), RFC-0042 (`.vyx` completion)
- **Evidence (user review, after reload):** in a `.vyx`, imported
  functions still colour as blue variables, hover doesn't work anywhere
  (functions, attributes, tags, types), and there's no Ctrl+Click
  go-to-definition. **Root-caused:** the server does hover/tokens/def for
  a `.vyx` ONLY via its owning `.vyrn` (the root that generates from it);
  the `vyx_owner` map is populated only when that owner is itself opened.
  Opening a `.vyx` directly — the normal action — leaves it owner-less, so
  the server returns nothing and the editor falls back to TextMate (which
  colours `Locale` by capitalisation and everything lowercase blue —
  precisely the reported symptom). The `refresh_document` comment states
  it: "An input with no known owner yet is simply stored." Every scripted
  test passed because it opened the owner first; no real user does.

---

## 1. Owner discovery (the correctness fix)

When a `.vyx` is opened/changed and `vyx_owner` has no entry for it, the
server **discovers** its owner instead of giving up:

- **Search:** from the `.vyx`'s path, gather candidate `.vyrn` roots —
  walk up to the nearest app root (a dir with `vyrn.json`, or a `.vyrn`
  importing `pages`/`pagesThemed`/`components`/`componentsThemed`), and
  scan `.vyrn` files at/under it (bounded: nearest-first, a sane cap).
  Analyze each candidate; the owner is the one whose
  `analysis.origins.input_files()` contains this `.vyx`. Register the
  whole owner→inputs mapping (so sibling `.vyx` under the same root are
  wired in one shot).
- **Cache the negative too:** a `.vyx` with genuinely no consumer (a
  scratch file) is marked owner-less so discovery doesn't re-run every
  keystroke; re-tried on the owner's change or a bounded interval.
- **On owner discovery, analyze once and cache** (see §2) — discovery and
  the first hover share the work.
- Result: opening `routes/index.vyx` or `widgets/CreateForm.vyx`
  standalone now yields full hover/tokens/def/completion — no need to
  open `server.vyrn` first.

## 2. Cache the synthesized analysis (the performance fix)

`handle_hover` / `handle_semantic_tokens` / completion each call
`analyze_linked(&gen_source, …)` **per request** to rebuild the
synthesized module's analysis — re-running the owner's generators
(`pages`+`rpc`+`openapi`+`tw`+`i18n` for bin) on every keystroke and
hover. The RFC-0048 agent measured this as "impractically slow" for a
real app. Fix:

- **Memoize the synth `Analysis` per (owner-root, gen-source hash).** The
  owner's link already produces each synthesized module's `gen_source`;
  cache the analyzed result keyed by its content hash, invalidated when
  the owner (or a linked input) changes. Hover/tokens/completion then hit
  the cache, not a fresh generator run.
- Discovery (§1) and each editor request reuse this one cache, so a
  themed page with the full bin stack answers hover in cache-time, not
  generator-time.

## 3. Go-to-definition through `.vyx` (Ctrl+Click)

`definition_provider` is declared and the resolver exists, but — like
hover — it produced nothing for a `.vyx` without an analyzed owner. With
§1/§2 it works; this RFC pins the behaviours the user asked for:

- **Ctrl+Click in a `.vyx`:** a call/type/import specifier → its
  definition in the `.vyrn`/std/generated source (through the origin map
  into the synth module, then to the real decl — cross-file already
  handled by RFC-0027/0031's def machinery); a **component tag**
  (`<CreateForm>`) → the sibling `.vyx`; an imported name → the module it
  comes from (generator imports resolve into the synthesized module's
  export, then to the real source where one exists).
- **Route handlers:** Ctrl+Click a page's `load`/`page`/`respond` or a
  handler name → its definition. (Where a name resolves only into
  synthesized glue with no user source, definition falls back to the
  nearest real origin rather than a dead synthetic file.)
- Confirm it also works in `.vyrn` (it should already; pin it so the
  "Ctrl+Click does nothing" report can't regress silently).

## Verification (the real-usage scenario the tests missed)

- **Open a `.vyx` WITHOUT opening its owner first** (the scenario every
  prior test skipped) and assert, via a scripted LSP session that opens
  ONLY the `.vyx`: hover on `format`/`tBinCount` returns the signature;
  semantic tokens are non-empty and classify the functions as `function`
  (not variable); go-to-definition on a call/type/import/component-tag
  jumps to the right source; class completion fires. This is the test
  that would have caught the bug — add it.
- Performance: hover latency on a bin `.vyx` after warm cache is
  interactive (report a rough number before/after the §2 cache).
- Full workspace suite + LSP e2e green; 0 warnings. **Rebuild + HASH-
  VERIFIED redeploy** of `vyrn-lsp.exe` (this touches the LSP crate) —
  report fresh == deployed.

## Out of scope

Multi-root workspaces beyond nearest-app-root discovery, watching the
filesystem for owner changes (discovery is on `.vyx` open/change +
owner change), analyzing every `.vyrn` in a huge repo eagerly (bounded
discovery only), inlay hints, and any change to what generators emit
(RFC-0048 already emits the origins this consumes).
