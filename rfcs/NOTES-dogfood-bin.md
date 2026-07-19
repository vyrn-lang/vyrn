# Dogfood notes — building `examples/bin` (a pastebin) on the full Vyrn stack

A record of every point of friction found while building **bin** — a real,
usable pastebin — end to end on the complete current stack (`std/html` +
`std/ui` pages + `std/rpc` + `std/i18n` + `std/tw` + `std/vyx` + `Map` +
stored-closure middleware), served by `vyrn dev` and browser-verified. The app
is the pretext; this report is the deliverable.

The headline probe was **PERSISTENCE** — no Vyrn app had ever written to disk
(shelf's store is in-memory). This one owns an `Array<Paste>` + a counter, loads
it from `data/pastes.json` at startup, and re-persists the whole store after
every mutation via `toJson` + `writeFile`. It works, and it survived a real
server restart in the browser — but getting there surfaced a **silent
correctness bug** (global record-field `.push` no-ops), the exact **init-load
story** RFC-0029 predicted, and hard durability caveats. Alongside it: the
**no-clock/no-random** gap (a pastebin wants timestamps; Vyrn has none), the
**no-bitwise-ops** gap (no FNV-1a), and a handful of stack papercuts.

The app works: create (multiline body with `<>&"`) → soft-nav to `/p/<id>` →
escaped `<pre>` view → `/raw/<id>` exact-byte `text/plain` → recent list with
CLDR plurals → **restart → pastes survive** → corrupt file → empty store + logged
warning → 404s → clean console.

---

## TL;DR — top friction items (evidence in one line each)

1. **PERSISTENCE, init-load: works, but only via a helper in ANOTHER module.**
   `store.vyrn`'s initializer does `let mut store: StoreFile = persist.loadStore()`
   — legal ONLY because `loadStore` is imported (RFC-0029: an initializer may
   call an imported fn, never a same-module one). Load-on-init is expressible,
   but the loader MUST live in a separate module. (persistence, as-designed)
2. **BUG (silent) — FIXED in `6a9010f`: `store.pastes.push(x)` on a global
   RECORD-FIELD array NO-OPS.** The counter field-assign beside it persisted; the
   pushed paste vanished (`{"pastes":[],"counter":1}`). The push mutated a copy of
   the field. Whole-global REASSIGNMENT (`store = StoreFile {…}`) works. Cost me a
   "paste vanished after create" before I diagnosed it. **Now fixed**: the
   statement-position `push` write-back desugar covers record-field and
   array-element receivers, not just plain variables; `store.pastes.push(p)` is
   restored in `store.vyrn`. (compiler — real bug; see below)
3. **No clock, no random — `created` is a monotonic counter, not a timestamp.**
   ~~A real pastebin wants "created 3 minutes ago"; Vyrn has neither wall time nor
   randomness (parity determinism), so `created` is a persisted sequence number
   and ids are content-addressed. This is the finding we were fishing for.~~
   **RESOLVED by RFC-0043 (time & randomness at the host boundary).** Time/random
   are host INPUTS the parity harness FIXES (`VYRN_FIXED_TIME`/`VYRN_FIXED_SEED`),
   exactly like `.stdin`/args — so a clock is an effect at the boundary, NOT a
   break in determinism. `std/time` gives `now()` (an `extern` shim-implemented on
   every target: native `timespec_get`, wasi `clock_time_get`) plus a PURE UTC
   breakdown + `format`/`formatIso`; `std/random` gives a pure value-type PRNG
   with one `randomSeed()` host extern. `Paste.created` is now a real wall-clock
   `Instant` (epoch millis, stamped `now()` at create, rendered
   `format(fromMillis(created))` → "created 2023-11-14 22:13:20 UTC"); it
   round-trips the JSON codec unchanged (still an `Int64`) and survives restart.
   Content-addressed ids are UNCHANGED (the pure PRNG is available but ids stay
   hashes). `examples/clock.vyrn` proves the whole surface byte-identical
   three-way. (language gap → closed)
4. **No bitwise operators → no FNV-1a.** Only `+ - * / %` exist (no `^ & | << >>`),
   so FNV's `hash ^= byte` is unwritable. Used a polynomial rolling hash
   (`h = (h*base + byte) % mod`) instead. (language)
5. **`std/ui` dynamic route segments are Int64-ONLY.** The pages generator hard-codes
   `fromJson(UiRouteInt, …)` and `v0: Int64` for every dynamic segment, so string
   ids (`/p/<base36>`) can't be a page — I hand-dispatched `/p/<id>` and
   `/raw/<id>` in the server root (the `/theme.css` precedent). `/raw` also needs
   a `text/plain` content type, which page routes can't set. (generator)
6. **`vyrn serve`/`vyrn dev` run `main()` at startup.** A mutating `main` seeds the
   live store on every restart; I made `main` read-only. (tooling, sharp edge)
7. **Papercuts:** `UInt8`↔`Int64` needs explicit `Int64(b)` (no implicit widening,
   no `UInt8` arithmetic); validated field assign needs a pre-typed value
   (`let c: Created = …`); a String-returning wasm export handed JS a raw pointer
   until declared in `exportReturns`; the `.vyx` parser matches the keyword `props`
   inside a `//` comment; top-level fn names must be globally unique across std
   (`empty` collided with `std/html.empty`).

---

## PERSISTENCE — the headline (init-load, write-after-mutation, durability)

### Init-load: RFC-0029's imported-fn rule is exactly what makes it work

The store's whole state is read from disk in its own initializer:

```vyrn
// store.vyrn
import * as persist from "./persist"
let mut store: StoreFile = persist.loadStore()   // reads data/pastes.json
```

This is legal **only** because `loadStore` is imported from another module.
RFC-0029's `init_restrictions` forbid an initializer from calling a **same-module**
function (only imported modules are guaranteed initialized first) but explicitly
**allow an imported one**. So the "loader helper in another module" the task
hypothesized is not just *a* pattern — it is the *only* pattern. A `fn loadStore()`
in the store module itself would be rejected:

> `initializer of \`store\` may not call \`loadStore\` — a module-state
> initializer runs before \`main\`, so it may use only literals, operators,
> built-ins, and functions imported from another module`

`readFile` (a builtin effect, RFC-0014) runs fine inside that imported helper
during init — no restriction on builtins. So: **module-state init CAN do the
load, provided the loader lives in a sibling module.** Clean, and it composes:
`persist.vyrn` (I/O) ← `store.vyrn` (state) ← `contract.vyrn` (boundary), with
`wire.vyrn` the shared leaf. This is the shelf split (RFC-0031) plus one I/O leaf.

**Friction:** the load must be one imported call returning ONE value. I wanted
`let store: StoreFile = persist.loadStore()` then two globals `pastes`/`counter`
projected from it — but reading `store.pastes` into another global would *move a
field out of a global* (rejected). So a single-struct global it is (which then
tripped the push bug below). Splitting into two flat globals would instead force
two imported loaders (double file read+parse). Neither is terrible; both are
consequences of "no field-move out of module state".

### Write-after-mutation: the silent `.push`-on-a-global-field BUG — FIXED (`6a9010f`)

This compiled and ran, and SILENTLY dropped the write:

```vyrn
store.counter = created                 // persisted (counter went to 1)
store.pastes.push(newPaste)             // NO-OP — the paste vanished
persist.saveStore(snapshot())           // wrote {"pastes":[],"counter":1}
```

`store.pastes` (a field projection of a global record) yielded a **copy**; the
`.push` mutated that copy and discarded it, while the sibling `store.counter =`
assignment (a direct field store) *did* persist. The result was a store that
counted creations it never kept. No error, no warning — the paste was simply
gone (`findByCreated` then returned `Err("paste vanished after create")`).

**Root cause (found & fixed):** the parser's statement-position `push` write-back
desugar (`sq.push(v)` → `sq = push(sq, v)`, in `parser.rs`) only fired when the
receiver was a plain `Expr::Var`. A record-field or array-element receiver fell
through to a discarded `Stmt::Expr(push(..))` — `push` reallocates and returns a
NEW array, which was then thrown away. **All three backends agreed on the wrong
answer** (a parity blind spot: interp == native == wasm all no-op'd), which is
why the harness never caught it and only persistence-to-disk made it visible.

**The fix (`6a9010f`)** extends the desugar to every assignable place, each
lowering to the in-place store the language already implements everywhere:

```vyrn
store.pastes.push(newPaste)   // r.xs.push(v)  → SetField  store.pastes = push(store.pastes, newPaste)
store.counter = created       // (unchanged direct field store)
```

- variable      `sq.push(v)`   → `sq = push(sq, v)`   (Assign, unchanged)
- record field   `r.xs.push(v)` → `r.xs = push(r.xs, v)` (SetField)
- array element  `a[i].push(v)` → `a[i] = push(a[i], v)` (IndexSet)

Any *other* receiver (a temporary like `make().push(v)`, or a deeper chain
`r.a.b.push(v)` / `a[i].f.push(v)`) is now a hard **parse error** naming the
supported places — never a silent copy (silence was the bug). `pop`/`swapRemove`/
`remove` remain variable-only through the checker's existing, non-silent
diagnostic ("needs a plain array variable as its receiver"): they return a value
AND mutate, so there is no statement to write back. The workaround (rebuild the
array + whole-global reassign) is deleted from `store.vyrn`; `examples/fieldmut.vyrn`
locks the behavior in three-way.

**Open item (DEFERRED) — index-assign write-through for a record-field array.**
`push` now writes back through a record field (`r.xs.push(v)`), but the sibling
element-store `r.xs[i] = v` does *not*: it is a hard parse error today ("the left
side of an index assignment `[i] = ..` must be a plain array variable"). This is
an **intentional inconsistency for now** — the safe, non-silent behavior (reject,
don't copy) — but it means a caller who can `r.xs.push(v)` cannot `r.xs[i] = v` and
must hoist the field into a local first. Whether `[i] =` should gain the same
one-level field/element write-back the `push` desugar has is a design question left
open. The current rejection is pinned by `parser.rs`
`index_assign_on_a_record_field_array_is_rejected` so the behavior can't drift while
undecided.

### Durability caveats (the std/storage evidence)

`writeFile` (RFC-0014) is truncate-then-write with **no atomic rename and no
fsync**. Honest consequences, all reported, none worked around (there's no way
to at the language level):

- **A crash mid-write corrupts the store.** The file is truncated first; a crash
  before the write completes leaves a partial/empty JSON. `loadStore` then treats
  it as a corrupt file → empty store + warning (verified by hand-corrupting the
  file). So we degrade safely on *read*, but we can still *lose the whole store*
  on a mistimed crash because there is no write-to-temp-then-rename.
- **Whole-file rewrite every mutation.** Each create re-serializes and rewrites
  the entire store (`toJson(snapshot())`). Fine for a demo; O(store) per write is
  the wrong shape for anything real.
- **No fsync / no durability barrier.** `Ok(true)` from `writeFile` means "handed
  to the OS", not "on disk".

These are exactly the primitives a `std/storage` RFC would add: atomic replace
(temp + rename), append/segment logs, and a durability signal. `writeFile` is the
right floor; it is not a database.

---

## NO CLOCK / NO RANDOM — the pastebin's structural gap

A pastebin wants two things Vyrn deliberately does not have:

- **A real timestamp.** `created` should be wall-clock time ("2 minutes ago").
  Vyrn has no clock (determinism = parity), so `created` is a **monotonic counter
  persisted in the store** — a sequence number (`paste #1`, `#2`), not a time.
  The UI says "paste #N" because it *cannot* say "created at HH:MM". This is the
  single sharpest "real app" gap the whole exercise found.
- **Randomness for ids.** No `random()` either, so ids are **content-addressed**:
  a polynomial hash of `body + title`, base36, short prefix, extended on collision
  (proven: two identical-content pastes produced `0a6ixq` then `0a6ixq7` — the
  collision-extension path fired). Content addressing is arguably *better* than
  random here (dedup-friendly, deterministic tests), so this gap stung less than
  the clock — but a pastebin that let two different bodies collide would need real
  entropy or a store-counter suffix.

**The parity tension, stated plainly:** a clock/RNG makes `interp == native ==
wasm` byte-identical output impossible — that is *why* they're absent. Possible
designs, none free:
1. **Host-injected time via `extern`** (the browser already has `Date.now`; a WASI
   host has a clock). Time becomes an effect at the program boundary, excluded from
   `where`/consteval, and parity examples simply don't call it (like the I/O
   examples that need fixtures). This fits the existing effect taxonomy best.
2. **A capability value** threaded from `main`/`handle` (`Clock`, `Random`) —
   explicit, testable (inject a fake), but viral through signatures.
3. **Seeded, store-persisted RNG** for ids specifically — deterministic *and*
   collision-resistant, no host needed; doesn't solve timestamps.

My recommendation: (1) for time as a boundary effect, (3) for ids. Do NOT hack a
clock into the compiler — the determinism guarantee is load-bearing for the whole
parity story.

---

## `std/ui` pages: Int64-only dynamic segments

`std/ui`'s pages generator (`uiEmitDynMatch`/`uiEmitRender`) hard-codes every
dynamic segment as `Int64`:

```
match fromJson(UiRouteInt, segs[k]) { … }     // always UiRouteInt
… "v" + i + ": Int64"                          // params always Int64
```

Paste ids are content-addressed base36 **strings**, so `/p/[id]` and `/raw/[id]`
**cannot be `std/ui` pages**. I hand-dispatched both in the server root
(`hasPrefix(path, "/p/")` → `getPaste`), which is the established `/theme.css` /
`/openapi.json` precedent — but it means the app's two most important routes get
none of the pages machinery (no `load`/`Params`, no generated 404). `/raw` *also*
needs `contentType: "text/plain"`, which page routes hard-code to `text/html`, so
even an Int64 id couldn't have used a page for raw. Two concrete asks for
`std/ui`: **String (validated-string) route params**, and **loader control over
the Response content-type**.

## `vyrn serve`/`vyrn dev` run `main()` at startup

`interp::serve` calls `main` if present (compiler/vyrn-frontend/src/interp.rs
~L567) before entering the serve loop. My first `main` created a paste as a
persistence smoke — which then **seeded the live served store on every dev
restart** (a phantom "hello" paste appeared in the browser). I made `main`
read-only (list + a 422 demo + GET checks, like shelf's). Fine once understood,
but surprising: for a serve app, `main` is a startup hook, not dead code. Either
`serve` should skip `main`, or the docs should say "main runs once at boot".

---

## LANGUAGE / STD PAPERCUTS (smaller, mechanical)

- **No `UInt8` arithmetic, no implicit widening.** `bytes(s)` is `Array<UInt8>`;
  `Int64(b)` is the ONLY way to use a byte in arithmetic (`h*base + b` fails:
  "arithmetic needs matching numeric operands, found Int64 and UInt8"). And
  building a byte from an `Int64` (`48 + n` for a digit) can't be returned as a
  `UInt8` — I read digits from a lookup-`bytes` table instead (the only
  Int64→UInt8 that type-checks is *reading an existing byte*). Byte-level code is
  common (hashing, parsing) and this makes it clumsy.
- **Validated field assign needs a pre-typed value.** `store.counter = store.counter + 1`
  is rejected (`counter` is `Created`); needs `let c: Created = store.counter + 1;
  store.counter = c`. Same shelf finding (`books[i].rating = s`), now on a plain
  field.
- **No structural copy.** `copyPaste` / `snapshot` are hand-written deep copies so
  a value can leave the store without moving the global (shelf's `copyBook`
  again). A derive-able `.clone()` for records would delete a lot of boilerplate.
- **`match` arms are single expressions; `assert`/`assertEq` are test-only.** I
  couldn't factor test assertions into a helper (`assertEq` outside a `test` block
  is rejected), so a round-trip test decodes via a plain helper and asserts
  inline. Minor, but it shapes how tests are written.

## GENERATOR / TOOLING PAPERCUTS

- **String-returning wasm export → raw pointer in JS.** Calling
  `app.exports.takeNav()` directly returned `1363792` (a memory pointer), so the
  soft-nav went to `/1363792` → 404. Fix: declare the export in vyrn-dom's
  `exportReturns: { takeNav: "string" }` so the glue decodes it. Discoverable only
  by reading vyrn-dom.js — nothing warns you. (RFC-0012 String ABI asymmetry; the
  built-in `vyrnView`/`vyrnPatch` are pre-declared, user exports aren't.)
- **`.vyx` parser matches `props` inside a `//` comment.** A script comment
  containing the word "props" made `vyxParseScript` find the keyword there, look
  for `{`, hit `(`, and fail with `VYX_BAD_PROPS`. The keyword scan isn't
  comment-aware. (Worked around by rewording the comment.)
- **Top-level fn names must be globally unique across ALL linked modules, incl.
  std.** My `persist.empty()` collided with `std/html.empty()`
  (`\`empty\` is defined in both … — top-level names must be unique across the
  program`). The shelf NOTES flagged this for *locals*; it bites *top-level fns*
  too. Short names (`empty`, `text`, `el`) are landmines.
- **`rpcClient` requires an `on<Proc>` handler for EVERY procedure.** The client
  only calls `createPaste`, but `onListPastes`/`onGetPaste` must still exist (no-op)
  or checking fails (`call to unknown function onListPastes`). Same as shelf's
  `onTagCounts`. Fine, but the coupling is invisible until it errors.
  **RESOLVED (RFC-0040 §2):** each stub now takes a completion callback at the call
  site (`api.createPaste(req, |res| …)`); a procedure the client never calls needs
  no handler at all. `on<Proc>` is retired. (Caveat found during RFC-0040 §6
  verification: the callback clients do not yet build to `wasm`/native — a
  pre-existing RFC-0023×RFC-0037 codegen gap, see RFC-0040 as-landed "downstream
  wall"; SSR is unaffected.)

---

## WHAT WORKED WELL (calibration — do not change these)

- **The persistence round-trip itself.** `toJson`/`fromJson` over the whole
  `StoreFile { pastes: Array<Paste>, counter }` is byte-clean, and `fromJson`
  returning `Invalid` (never trapping) on a hand-corrupted file is *exactly* the
  right shape for a startup loader — one `match` and you degrade to empty + a
  logged warning. The codec is the reason persistence was ~30 lines.
- **Validated types across every boundary.** A bad create (`body:""`, `lang:"klingon"`)
  produced two path-tagged Issues (`body`, `lang`) as a 422, both client- and
  server-side, with zero glue. Content caps (`Body <= 100000`, finite `Lang`) are
  enforced on decode for free.
- **RFC-0029 stateful contract + generated dispatch.** `rpcHandle` from
  `rpcServer("./contract")` over a store-backed contract — no hand-written
  dispatch (shelf's #1 pain, now gone). Page loaders read the store directly (the
  home list is real SSR).
- **`text()` auto-escaping.** The `<pre>` body rendered `&lt;vyrn&gt; &amp;`
  correctly with no manual escaping; `/raw` served the exact unescaped bytes. Two
  content types, one data source, both correct first try.
- **`<select>` and multiline `<textarea>` in `.vyx` + vyrn-dom.** Both "verify if
  it's a gap" items just worked: the select's `@change` delivered the option value,
  the textarea's `@input` delivered the full multiline text (newlines + quotes +
  `<>&` intact end to end into the store).
- **Soft nav + prefetch.** After create, `window.vyrnNav.navigate("/p/<id>")` did
  a client-side transition; recent-list links carry `data-nav="prefetch"`. The
  patch-protocol loop (`vyrnView`/`vyrnPatch` + `diff`) drove the create island.
- **CLDR Ukrainian plurals.** `1 вставка` / `2 вставки` / `5 вставок` (one/few/many)
  rendered correctly server-side on the About page, English `1 paste`/`N pastes`
  on the home count.
- **`openapi("./contract")` for free.** One import + one route line → a 5.8 KB
  OpenAPI 3.1 document at `/openapi.json`.
- **`vyrn dev`.** One command built the client to wasm, served `public/`, mounted
  the runtimes, and routed `/rpc/*` — smooth (once `WASI_SYSROOT`/`WASI_BUILTINS`
  were set).

---

## Prioritized next-RFC candidates

| # | Candidate | Evidence in bin | Scope | Kind |
|---|-----------|-----------------|-------|------|
| ~~**P0 (bug)**~~ **FIXED `6a9010f`** | ~~Fix `.push`/method-mutation on a global record-field (silently mutates a copy)~~ — done: push write-back now covers record-field & array-element places; deeper chains are a named parse error | "paste vanished after create"; counter persisted, data didn't | small–medium | compiler |
| **P1** | **`std/storage` — durable writes** (atomic temp+rename, fsync/durability signal, maybe append log) | whole-file rewrite per mutation; crash-mid-write loses the store; no fsync | large | library + runtime |
| **P1** | **Time as a boundary effect** (host-injected `now()` via `extern`/capability, excluded from `where`/parity) | `created` is a counter, not a timestamp — a pastebin's core field is fake | medium | language + runtime |
| **P2** | **`std/ui`: String/validated-string route params + loader Response content-type control** | `/p/<id>`, `/raw/<id>` can't be pages (Int64-only, text/html-only) | medium | generator |
| **P2** | **Randomness / entropy source** (seeded, capability, or store-persisted) for non-content ids | content addressing works but can't mint an unguessable/opaque id | medium | language + runtime |
| **P2** | **Bitwise operators** (`^ & | << >>`) OR a `std/hash` (FNV/SipHash) | no FNV-1a possible; hand-rolled polynomial hash | small (ops) / medium (lib) | language / library |
| **P3** | **`UInt8`↔`Int64` ergonomics** (implicit widening in arithmetic, or `UInt8` arithmetic) | every byte op needs `Int64(b)`; can't build a byte from an int | small | language |
| **P3** | **Derive-able structural copy (`.clone()`) for records** | hand-written `copyPaste`/`snapshot` deep copies | medium | language |
| **P3** | **`serve`/`dev` should skip `main`** (or document it runs) | a mutating `main` seeded the live store on every restart | small | tooling |
| **P3** | **Warn on String-returning user exports not in `exportReturns`** (or auto-decode) | `takeNav()` → raw pointer → nav to `/1363792` | small | runtime/tooling |
| **P3** | **`.vyx` keyword scan should skip comments; clearer top-level-name-collision diag** | `props` matched in a comment; `empty` vs `std/html.empty` | small | generator/tooling |

---

## Verification summary

Browser (via `vyrn dev`, port 8092), all checks green:

- **Create** a paste (multiline body with `<>&"`) via the hydrated island → typed
  `createPaste` stub → **soft-nav to `/p/096nfp`** (after fixing the String-export
  decode).
- **Escaped view:** the `<pre>` rendered `fn main() -&gt; … &lt;vyrn&gt; &amp;
  …`; the raw bytes at **`/raw/096nfp` served `text/plain; charset=utf-8`** exact
  (newlines/quotes/`<>&` intact).
- **List + plurals:** home showed `2 pastes` (en) with both entries; About page
  showed uk `1 вставка / 2 вставки / 5 вставок`.
- **RESTART SURVIVAL (headline):** stopped and restarted `vyrn dev`; the home page
  reloaded **both pastes from `data/pastes.json`** (`2 pastes`, `/p/096nfp` #2,
  `/p/0gv2z3` #1). The content-address **collision-extension** also fired
  end-to-end (`0a6ixq` → `0a6ixq7` for identical content across a restart).
- **Corrupt file:** hand-corrupted `data/pastes.json`, restarted → server logged
  `[WARN] store: could not decode data/pastes.json — starting with an empty store`
  and served `No pastes yet` — no crash.
- **404s:** `/p/<unknown>`, `/raw/<unknown>`, `/totally/unknown` → 404. Console
  clean (no errors) throughout.

In-language tests (`vyrn test`): `store.vyrn` 3/3 (id stability + content
addressing, `StoreFile` codec round-trip, corrupt-JSON `Invalid` no-trap);
`client.vyrn` 3/3 (invalid-draft accumulated Issues, valid-draft decode, a `.vyx`
view snapshot).

Suite: **880 workspace tests pass** (baseline unchanged — no `compiler/`/`std/`/
top-level `examples/*.vyrn` files were touched). The three-way parity harness is
unaffected: it reads only top-level `examples/*.vyrn`, so `examples/bin/` (a
subdirectory) is auto-excluded, exactly like `examples/shelf/`. `data/pastes.json`
is gitignored (runtime data, not source).
