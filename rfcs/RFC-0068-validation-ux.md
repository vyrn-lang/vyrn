# RFC-0068 — Structured Validation UX: Issues Survive the Wire

- **Status:** Locked design
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
