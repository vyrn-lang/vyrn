# RFC-0024 — Payload Enums on the Wire (Codec v2)

- **Status:** Implemented
- **Depends on:** RFC-0018 (the codec — this is its named v2), the jsonSchema
  emitter/importer (the tagging decision must land in both, together)
- **Evidence:** RFC-0018 and RFC-0019 both defer exactly this; RPC contracts
  model errors as records+`Option` because `Result<T, E>` is not codable.

> **Implementation notes (as landed).** Shipped across all three backends,
> byte-identical — encoded bytes AND every `Issue`'s key/path/message — and
> covered by `examples/enumcodec.vyrn` (the three-way parity showcase).
>
> - **Codability** (`vyrn_frontend::codec`): a payload enum is codable when every
>   variant's payloads are (the checker names the offending `<type> (payload of
>   variant \`Name\`)`); `Result<T, E>` is codable when both payloads are. `Option`
>   is **not** routed through the enum path (it keeps its `null`/absent form);
>   `Option<Result<..>>` / `Option<Enum>` are now codable (a payload variant never
>   encodes as `null`, so no ambiguity), but a nested `Option<Option<..>>` stays a
>   hazard. `Validation<T>` is rejected explicitly by name.
> - **Encode / decode:** the interpreter walks values directly; native/wasm reuse
>   the RFC-0018 C-shim JSON DOM with per-type generated IR. A **named payload
>   enum** gets its own `@__vyrn_enc_T` / `@__vyrn_dec_T` function (the
>   per-record-type precedent), so a self-referential payload (through a
>   record/array/its own type) resolves to a call — recursion-safe. `Result<T, E>`
>   is **inlined** at each concrete site (its `T`/`E` are known there, and it can
>   never be self-referential without a name, so inlining terminates). A
>   pure-nullary enum keeps its exact RFC-0018 string encoding and its
>   `{"enum":[..]}` schema — no function is generated, a regression pin holds the
>   bytes.
> - **Generic-instance mechanics.** Monomorphic named enums are the per-type-IR
>   case above. `Result<T, E>` monomorphizes by inlining per concrete
>   instantiation (`Result<User, String>` follows the record codec's existing
>   per-concrete-type resolution). Generic *user* enums (`type Box<T> = ..`) are
>   left to the concrete-use-site inline path, matching the existing limitation
>   that generic **records** also have no standalone per-instance codec function.
> - **Tuple schema (the `prefixItems` decision).** A tuple payload emits the
>   honest draft-2020-12 fixed-tuple form `{"type":"array","prefixItems":[..],
>   "items":false}` — `items:false` is load-bearing (it forbids extra elements and
>   is what the importer round-trips). The importer recognizes EXACTLY the emitted
>   `oneOf` shape (nullary `{"const":"Name"}`, single-property tagged object, tuple
>   via `prefixItems`+`items:false`) back into an enum decl; any other `oneOf` is a
>   hard error. `emit → import → re-emit` is pinned byte-exact, including a mixed
>   nullary+payload enum and `Result<User, String>` in a record (a `Result` and an
>   `Ok`/`Err` two-single-payload enum emit identically, so the importer maps both
>   to an enum and re-emission stays exact).
> - **RPC ripple.** `std/rpc`'s contract checks already deferred return shape to
>   codability (they only constrain the single request parameter, which `fromJson`
>   still needs by name), so a `Result`-returning procedure is legal with no
>   generator change. The one enabling surface change is that a transparent alias
>   `type DeleteResult = Result<Bool, String>` now type-checks (so a `Result` can
>   be **named** and handed to `fromJson`/`jsonSchema`); `match`/`assignable`/the
>   `Ok`/`Err` inference resolve such aliases. `examples/fullstack` gains
>   `deleteUser(req) -> DeleteResult` proving a **200** with `{"Ok":true}` /
>   `{"Err":..}` end-to-end (`422` stays reserved for request validation).

---

## The tagging decision (locked)

Externally tagged, arity-shaped — the encoding that keeps the existing
payload-less form as its degenerate case:

```vyrn
type Shape = | Circle(Int64) | Rect(Int64, Int64) | Unit
```

| variant | JSON |
|---|---|
| `Unit` (nullary) | `"Unit"` — a bare string, **identical to payload-less enums today** |
| `Circle(5)` (one payload) | `{"Circle":5}` — unwrapped |
| `Rect(2, 3)` (two+) | `{"Rect":[2,3]}` — an array |

- `Result<T, E>` becomes codable for free: `{"Ok":<T>}` / `{"Err":<E>}`.
  Generic payloads follow the instantiation (`Result<User, String>` — the
  codec already monomorphizes per concrete type).
- A **pure-nullary enum keeps today's compact encoding unchanged** (bare
  strings; its jsonSchema stays `{"enum":[...]}`) — zero migration.
- Codability of an enum = codability of every payload (checker names the
  offending variant/payload otherwise). `Validation<T>` itself stays
  non-codable v1 (its `Invalid` carries `Array<Issue>` — allowing it is a
  one-line follow-up once this lands, if wanted; not load-bearing now).

## Decode semantics (locked)

- A payload variant decodes from an object with **exactly one key**; that
  key must be a known variant. Wrong shape / unknown variant →
  `json.type` Issue: ``expected one of `Circle`, `Rect`, `Unit`, found …``
  (kind, not value — the RFC-0018 uniformity rule).
- Payload arity and types check with paths `shape.Circle` (single) /
  `shape.Rect[1]` (tuple index); `where` clauses on validated payloads run
  and accumulate, as everywhere.
- A nullary variant decodes from its bare string (and ONLY that — no
  `{"Unit":null}` alternate spellings; one wire form per value).

## The schema story (lands together, round-trip exact)

- **Emitter:** a payload enum emits `oneOf`: nullary variants as
  `{"const":"Unit"}`; payload variants as single-property objects
  (`{"type":"object","properties":{"Circle":<payload schema>},
  "required":["Circle"]}`, tuple payloads via `prefixItems`), replacing the
  current honest-lossy `$comment`.
- **Importer:** recognizes exactly that `oneOf` shape back into a Vyrn
  enum — emit → import → re-emit stays byte-exact (the pinned law). Any
  other `oneOf` remains a hard error (exactly-or-not-at-all).

## Ripples (part of this RFC)

- `std/rpc` generation-time contract checks defer to the codability
  predicate — `Result`-returning procedures become legal automatically.
  Document the semantics: an `Err` return is an application-level outcome —
  **200** with `{"Err":…}` on the wire (422 remains reserved for request
  *validation* failures). The fullstack demo gains a `Result`-returning
  procedure to prove it end-to-end.
- `fromJson`/`toJson` per-type generated IR grows an enum path (tag switch;
  the interp mirrors). Round-trip law extends over the new domain
  (`fromJson(T, toJson(x)) == Valid(x)` incl. nested enums-in-records,
  enums-in-arrays, `Option<Result<…>>`).

## Out of scope

Renaming variants on the wire (serde-style attributes), internally/
adjacently tagged alternates, `Validation<T>` codability (noted above),
protobuf numbering (still reserved for the Connect work).
