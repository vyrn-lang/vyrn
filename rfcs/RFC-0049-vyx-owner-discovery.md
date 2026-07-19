# RFC-0049 — `.vyx` Owner Discovery & Cached Forward-Mapping (the LSP unblock)

- **Status:** Implemented (see "As-landed" below)
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

---

## As-landed (Implemented)

LSP-only change (`compiler/vyrn-lsp`, the excluded crate); zero change to
compilation semantics, emitted code, or parity.

### §1 — Owner discovery (algorithm + bound)

When a `.vyx` with no `vyx_owner` entry is opened/changed
(`refresh_document` → `discover_vyx_owner`, publishing) OR a hover/
tokens/def/completion request arrives for one (`handle_request` →
`ensure_vyx_owner`, non-publishing), the server discovers the owner:

1. **App root** (`app_root_for`): walk up from the `.vyx`'s directory
   (≤ `MAX_WALK_UP` = 8 levels) to the nearest ancestor with `vyrn.json`;
   else the nearest ancestor holding a `.vyrn` that imports a
   directory-consuming generator (`pages`/`pagesThemed`/`components`/
   `componentsThemed`); else the `.vyx`'s own directory.
2. **Candidates** (`collect_vyrn` + `candidate_owners`): gather the
   `.vyrn` files under the app root (recursive, skipping
   `.*`/`vyrn_vendor`/`target`/`node_modules`/`public`), capped at
   `MAX_OWNER_CANDIDATES` = 48. Rank by a cheap textual read: a root
   importing a generator **and** naming the `.vyx`'s directory scores
   highest (+6), any generator-importing root next (+2), ties broken by
   path proximity. (For `examples/bin`, this puts `server.vyrn` first for
   `routes/*.vyx` and `client.vyrn` first for `widgets/*.vyx` — one probe,
   no wasted generation.)
3. **Probe** (`probe_owner`): analyze each candidate nearest-first; the
   owner is the first whose `analysis.origins.input_files()` contains this
   `.vyx`. Register the whole owner→inputs map in one shot
   (`install_root`), so sibling `.vyx` under the same root wire up
   together, and the winning analysis is cached (no second generation —
   discovery and the first request share it).
4. **Negative cache** (`vyx_ownerless`): a `.vyx` with no consumer is
   remembered owner-less so discovery does not re-run per keystroke.
   Cleared per-file on a `.vyx` (re)open (explicit retry) and wholesale on
   any `.vyrn` open/change (the project may have gained an owner).

### §2 — Cached synthesized analysis (key + invalidation)

`synth_for(owner, banner)` memoizes the synthesized module's work in
`Server.synth_cache` (a `RefCell<HashMap<owner_url, OwnerSynth>>`):

- **Level 1** — one `generated_modules(owner)` run per owner, keyed by
  `owner_sig` = a hash of the owner's text **plus** every open buffer
  under its directory (the `.vyx`/theme inputs a generator reads). Any
  edit to the owner or a linked input changes the signature and
  regenerates; otherwise the generation is reused across all requests.
- **Level 2** — per generated-module banner, the analyzed synthesized
  `Analysis` + its semantic tokens, filled lazily and reused. Hover,
  semantic tokens, definition and completion all go through this one
  cache (previously each re-ran `generated_modules` + `analyze_linked`
  **per request**).

### §3 — Go-to-definition through `.vyx`

- A call/type/import specifier resolves through the origin map into the
  synthesized module, then to the real decl via the RFC-0027/0031
  cross-file def machinery (e.g. `format` in a page `.vyx` →
  `std/time.vyrn`). A name resolving only into synthesized glue (no source
  file) yields no definition rather than a dead synthetic location.
- A component tag `<Cap>` (`component_tag_definition`, structural, before
  the forward map) → the sibling `Cap.vyx`.
- `.vyrn` definition is unchanged and pinned by the existing e2e.

### Verification / perf / hashes

- **New always-on regression guards** (fast scratch apps, open ONLY the
  `.vyx`): `rfc49_open_only_page_vyx_is_fully_analyzed` (owned via
  `pagesThemed`, app root via `vyrn.json`) and
  `rfc49_open_only_component_vyx_is_fully_analyzed` (owned via
  `componentsThemed`, app root via the generator-import signal). Each
  asserts hover, non-empty semantic tokens with functions classified as
  `function`, definition into `std/time`, and Tw class completion.
- **On-demand live money-shot** (`#[ignore]`d — the harness disables the
  on-disk gen cache, so the first uncached generation of bin's full
  `rpc`+`openapi`+`pages`+`tw`+`i18n` stack is slow):
  `rfc49_live_transcript_examples_bin` opens ONLY
  `examples/bin/routes/index.vyx` and ONLY `widgets/CreateForm.vyx`. Run:
  `cargo test -p vyrn-lsp rfc49_live -- --ignored --nocapture`. Observed:
  - `hover format@7 → fn format(i: Instant) -> String`,
    `fromMillis → fn fromMillis(n: Int64) -> Instant`,
    `pasteTally → fn pasteTally() -> Int64`;
    `tBinCreate → fn tBinCreate() -> String`.
  - `semanticTokens/full`: 39 tokens (page) / 17 (component), all
    functions classified `function` (pre-RFC: 0 tokens — owner-less).
  - `definition format@42 → file:///N:/lang/std/time.vyrn`.
  - class completion: `["p-0","p-1","p-2","p-3", …]` of 1638 labels.
- **Perf (§2)**: on the bin page `.vyx`, hover latency **cold 89.8 s → warm
  0 ms** (gen-cache disabled in the harness; the cold cost is the
  uncached generator run, which in a real editor is served from the warm
  gen cache). Before §2 every hover paid the cold cost; after, all but the
  first hit the in-process cache. (Removing a redundant discovery
  re-generation cut cold from 141 s → 89.8 s.)
- **Suite**: workspace 926 passed / 0 failed; LSP e2e 25 passed + 1
  ignored / 0 failed (was 23); 0 warnings.
- **Redeploy**: `vyrn-lsp.exe` rebuilt `--release` and hash-verified —
  fresh == deployed
  `222ec8bf729981152b75e22d94dac258fb0084d7cb97694103e57c276c6c61cf`.

### Notes / limits

- Discovery is bounded (48 candidates, 8 walk-up levels); a `.vyx`
  consumed by multiple roots resolves to the highest-ranked owner (page/
  component roots naming the `.vyx`'s directory win) — no ambiguity in the
  single-app-root layouts this targets.
- The §2 signature covers open buffers under the owner's directory; a
  disk-only edit to an input that is not open is picked up on the next
  owner/`.vyx` open or change (the editor only tracks open buffers).
