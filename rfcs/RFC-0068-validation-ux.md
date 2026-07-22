# RFC-0068 — Structured Validation UX: Issues Survive the Wire

- **Status:** Implemented
- **Depends on:** RFC-0009 (`Validation<T>` + `Issue` — the i18n error
  model this finally exercises end-to-end), RFC-0019/0021 (std/rpc
  generated stubs), RFC-0032/0036 (themed classes), RFC-0027/std/i18n
- **Evidence (user):** "why validation looks not so good? What about
  translations and color theme" — the screenshot: one red blob at the
  top of the form reading ``body: validation failed for `Body` `` —
  wire-canonical text with a leaked type name, not field-attached, not
  localized, styled by bespoke CSS outside the theme.

Three defects, one arc:

1. **The generated RPC client stub throws the structure away.** The
   wire 422 carries `[{key, path, message}]`; the stub flattens to a
   `String`, so no app CAN render field-attached errors from a server
   response.
2. **Nothing maps issues through i18n** even though `Issue.key`/`path`
   exist for exactly that and the app already ships `std/i18n` strings.
3. **No `danger` color family in the theme**, so error UI lives in
   hand-written CSS with a dark-mode patch.

---

## 1. std/rpc: structured replies (generator change, breaking)

The generated client module gains:

```vyrn
export type RpcIssue = { key: String, path: String, message: String }
export type RpcReply<T> = Done(T) | Invalid(Array<RpcIssue>) | Failed(String)
```

- Every generated stub's callback takes `RpcReply<ContractResult>`
  instead of today's flattened shape: a 2xx decodes to `Done(...)`, a
  422 parses the issues array to `Invalid([...])` (parse failure of the
  issue body itself → `Failed`), transport/HTTP-other → `Failed(msg)`.
- The CLIENT-side prevalidation path (`fromJson … Invalid(iss)`)
  produces the SAME `Array<RpcIssue>` shape (mapped from RFC-0009
  `Issue`), so the app renders one shape regardless of which side
  rejected.
- Breaking for stub consumers; ALL in-repo consumers migrate in this
  RFC (bin, shelf, fullstack clients). `rpcInProcess` mirrors the same
  reply shape (same-named stubs stay drop-in).
- emit-gen diffs reviewed; server side unchanged (the 422 wire format
  is already right).

## 2. The app-side idiom: field-attached, localized

The pastebin becomes the showcase:

- Client model keeps `issues: Array<RpcIssue>` (not a string).
- `CreateForm.vyx` renders per-field: a small script helper
  `fn issueFor(issues, path) -> String` returns the LOCALIZED text for
  the first issue at `path` ("" when clean); each field label gets
  `<span class="text-danger-600" v-if="…">{{ … }}</span>` under it, and
  a form-level line only for issues whose path matches no field.
- Localization: an app helper maps `(key, path)` through the existing
  `std/i18n` strings (`tIssueBodyEmpty()`, `tIssueLangUnknown()`, …)
  with FALLBACK to the wire `message` — unknown future issues degrade
  to today's text, never to nothing. The showcase adds the mapped
  strings to `strings.vyrn` in both locales the app ships.
- The leaked-type-name copy (`validation failed for `Body``) thus never
  reaches the UI for known paths; the canonical wire wording itself is
  NOT changed (parity/tests pin it).

## 3. Theme: a `danger` family

- `theme.json` gains `"danger"` shades (500/600) in `colors`; the
  issues UI uses checked `tw` classes (`text-danger-600` etc.); the
  bespoke `.issues` CSS rules (and their dark-mode patch) are deleted.
  (The tw generator needs NO change — a color family is data.)

## Verification

1. In-browser (Browser pane, dev server): empty submit → field-attached
   localized messages under Body (and Lang for a bad value), no page
   reload, no top-blob; a server-only rejection (bypass client
   prevalidation by direct RPC or crafted draft) renders identically;
   the danger color derives from theme.css (assert computed style).
2. Generated-stub tests: 422 → `Invalid` with parsed issues; malformed
   issue body → `Failed`; 2xx → `Done`; transport error → `Failed`.
   In-process mirror behaves identically (existing rpcInProcess tests
   extended).
3. All in-repo clients migrated and their existing tests green;
   emit-gen diffs reviewed; full suite + LSP + three-way parity green;
   fmt --check clean; 0 new clippy warnings; LSP redeploy only if
   frontend changed (hash-verify either way).

## Out of scope

Changing the canonical wire/trap wording, per-field ARIA wiring beyond
plain visible text, focus management, form-level submit disabling,
retry/backoff on `Failed`, and a generic forms library.

---

## As landed (2026-07-23)

Generator + `std`/examples only — **no** compiler / frontend / language
change (the only Rust touched is `tests/rpc.rs`, which pins the new
emitted surface). The server side and the 422 wire format are untouched:
bin's `main()` smoke still prints the byte-identical
`createPaste(bad) -> 422: {"issues":[{"key":"validate","path":"body",…}]}`.

**§1 — one locked variant name could not be spelled as written.** Enum
variants share ONE global constructor table, and the prelude `Validation`
already owns `Invalid`; a second `Invalid` is a hard "enum variant …
defined twice" check error. So the 422 arm is **`Rejected`**, not
`Invalid`. Everything else is as specified:

```vyrn
export type RpcIssue = { key: String, path: String, message: String }
export type RpcReply<T> =
    | Done(T)
    | Rejected(Array<RpcIssue>)
    | Failed(String)
```

Every generated stub's callback is now `fn(RpcReply<Ret>)`. `rpcIssuesFrom`
(exported by BOTH flavors) maps the RFC-0009 `Issue`s a client-side
`fromJson` prevalidation yields into the same `RpcIssue` shape a 422
carries, so an app renders one shape whichever side rejected.

**emit-gen diff (reviewed).**
- **`rpcClient`** — contract types re-emitted verbatim (unchanged), then
  the NEW `RpcIssue` / `RpcReply<T>` / `rpcIssuesFrom` block; the private
  `RpcIssues` now carries `Array<RpcIssue>`. Each `rpcUnify<Proc>` returns
  `RpcReply<Ret>`: a 200 body decodes `Valid(v) => Done(v)` / a malformed
  2xx body `=> Failed(…)`; 422 `Valid(bag) => Rejected(bag.issues)` / an
  unparseable 422 body `=> Failed(…)`; status 0 / other `=> Failed(…)`.
  The `rpcPending<Proc>` maps, the same-named stubs, `rpcDeliver<Proc>`,
  and the `vyrnRpcDone<Proc>` dispatchers all thread `fn(RpcReply<Ret>)`.
  The locked transport wording (`procedure \`X\` is unreachable`, `… failed
  with status …`) now rides the `Failed` arm — the substrings are
  unchanged, so RFC-0019's pins still hold.
- **`rpcInProcess`** — same `RpcReply` block emitted after the type-import
  line (so the mirror exports the identical reply surface — same-named
  stubs stay drop-in); each stub's `cb(Valid(..))` became `cb(Done(..))`.
  The deterministic flavor only ever produces `Done`, but the type is
  shared so callbacks are literally interchangeable with the wire client.

**Migrated consumers:** `examples/rpc`, `examples/rpcsplit` (in-process),
and the `fullstack` + `shelf` wire clients. Each callback's `Valid`→`Done`,
`Invalid`→`Rejected`, plus a new `Failed(why)` arm; each client's
prevalidation `Invalid` path and raw `IssueBag` demo now flow through
`rpcIssuesFrom`, so their single `renderIssues` takes `Array<RpcIssue>`.
(Note: `shelf/client.vyrn` has a PRE-EXISTING standalone-build quirk —
`assignment to unknown variable \`filter\``, present identically on `main`;
it is a `vyrn dev` island, not in the test suite. RFC-0068 only *removed*
its `Validation`/`RpcReply` mismatches; the quirk is untouched and out of
scope.) `std/connect` is a separate generator with its own `Validation`
reply and is deliberately not changed.

**§2/§3 — the pastebin showcase.** The create island's model is
`Array<RpcIssue>` (not a `String`). `CreateForm.vyx` imports `RpcIssue`
from `rpcClient("../contract")`: the generator-import identity (RFC-0040
§1) resolves the widget's rebased `../contract` and the client's
`./contract` to ONE module, so the widget's prop type and the client's
`api.RpcIssue` are the same nominal type — issues cross the prop boundary
with no shared plain-type module. The widget's script helper
`issueFor(issues, path)` returns the FIRST issue's localized text at a
path (`issueText` maps `(key, path)` through `strings.vyrn` —
`tIssueBodyEmpty` / `tIssueLangUnknown` / `tIssueTitleLong`, added in BOTH
locales — with a fallback to the wire `message`). Each field gets a
`<span class="text-danger-600" v-if=…>` under it; `formIssues` renders a
form-level line only for paths matching no field. `theme.json` gained a
`danger` family (`500 #f87171`, `600 #ef4444`); the bespoke `.issues`
rule + its `@media (prefers-color-scheme: dark)` patch are deleted from
`style.css`, and the stale `issues`/`hint` safelist names dropped. The
leaked type name never reaches the UI for a known path; the wire wording
is unchanged (there is no `dark:` variant in `std/tw`, so one tasteful red
serves both schemes — the RFC's dark-patch deletion assumes exactly this).

**Verification.** Full workspace **1044 passed / 10 ignored**; `tests/rpc`
**12 passed** (new pins: `RpcIssue`/`RpcReply<T>`/`Rejected` arm/
`rpcIssuesFrom`, `fn(RpcReply<User>)` stub + pending map, `Done`/`Rejected`
unify arms, `Failed("procedure \`getUser\` is unreachable")`); three-way
parity **6 passed** (`rpc`/`rpcsplit` are parity citizens — output strings
unchanged); `vyrn-lsp` (excluded) **43 passed / 1 ignored**. `vyrn test
examples/bin/client.vyrn` **6 passed**, pinning per-field extraction, the
i18n mapping (`"Body can't be empty"` present, `"validation failed for
\`Body\`"` absent for the known path), a bad-lang message, and the
unknown-path fallback. `fmt --check` clean on every `.vyrn` (`.vyx`
templates aren't `fmt` targets — the `@event` syntax isn't pure Vyrn); 0
new clippy warnings (only a test file touched; the 54 baseline warnings
are pre-existing). **No frontend Rust → no LSP redeploy:**
`editor/vscode/server/vyrn-lsp.exe` stays
`57569c62bbec95ca7cdcb43f093a001af4836db969d0ef5a55a013f25049a116`
(verified byte-for-byte).

**Browser click-path to verify post-merge** (`cd examples/bin && vyrn
dev`; DevTools open):

1. **Empty submit → field-attached, localized, no reload.** Load `/`,
   leave Body empty (optionally type a Title), click **Create paste**.
   Expect a red line **`Body can't be empty.`** directly under the Body
   textarea (client prevalidation; no page reload, no top blob). Fixing
   the body and resubmitting clears it.
2. **Bad language → its own field message.** With the standard `<select>`
   you can only pick valid langs, so exercise the Lang path by editing the
   draft in the console — `setDraftLang("klingon")` then `submitCreate("")`
   (or POST a crafted body to `/rpc/createPaste`). Expect **`Choose a
   supported language.`** under the Language field. A title over 80 chars
   yields **`Title is too long (max 80 characters).`** under Title.
3. **Server-only rejection renders identically.** Bypass the client
   prevalidation with a direct RPC (`curl -s -X POST
   localhost:<port>/rpc/createPaste -d
   '{"title":"x","body":"","lang":"klingon"}'` returns the 422
   `{"issues":[…"path":"body"…,…"path":"lang"…]}`). Driven through the
   island, the SAME `Body can't be empty.` + `Choose a supported language.`
   attach to their fields — one render path for both rejection sides.
4. **Danger color derives from theme.css.** Inspect a red issue span:
   computed `color` is `rgb(239, 68, 68)` (`#ef4444`, `text-danger-600`),
   and the rule comes from `/theme.css` (the tw-generated stylesheet), not
   inline or `style.css`. No dark-mode CSS remains — the color is the same
   red in light and dark.
