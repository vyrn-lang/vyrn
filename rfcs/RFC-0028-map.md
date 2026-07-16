# RFC-0028 — `Map<String, V>`: The Dictionary Type

- **Status:** Implemented
- **Depends on:** RFC-0011 (array mutation — the subject-first collection
  API this mirrors), RFC-0018/0024 (codec — a Map IS a JSON object),
  RFC-0003 (jsonSchema — `additionalProperties` emit + import round-trip),
  RFC-0004 (ownership — drop/leak rules extend to map storage)
- **Evidence:** TypeScript's index signature `{ [key: string]: number }`
  has no Vyrn spelling; free-form JSON objects, headers, query params,
  i18n bundles, and counters-by-name all currently force `Array<{key,
  value}>` workarounds (O(n), clunky, and their JSON encoding is an array
  of pairs, not an object — wrong on the wire).

---

## Surface

```vyrn
fn main() -> Int64 {
    let mut scores: Map<String, Int64> = [:]     // empty map literal
    scores["alice"] = 10                          // insert
    scores["alice"] = 11                          // update (slot unchanged)
    match scores["bob"] {                         // lookup is HONEST: Option<V>
        Some(n) => print("bob: \{n}"),
        None => print("no bob"),
    }
    if scores.has("alice") { scores.remove("alice") }
    for k in scores.keys() {                      // Array<String>, insertion order
        print("\{k} = \{scores[k]}")
    }
    print("\{scores.length}")
    return 0
}
```

- **Named type, not inline syntax.** TS's anonymous `{ [key: string]: V }`
  is spelled `Map<String, V>` — Vyrn names its types; no indexed-signature
  record syntax is added. (Mixed records — fixed fields PLUS an index
  signature — are deliberately not expressible; model that as a record
  with a `Map` field.)
- **Keys are `String` in v1** (checker rejects other key types with a
  named diagnostic). This matches JSON objects exactly and defers the
  hash/equality-protocol question until something real demands non-string
  keys. Validated string types (e.g. a finite type) are legal keys — they
  are Strings.
- **Values:** any type an `Array<V>` could hold, same rules.

## Semantics (locked)

- **Insertion-ordered, always.** Iteration (`keys()`), printing, and JSON
  encoding all follow first-insertion order (an update in place does NOT
  move the key; remove-then-insert does). This is non-negotiable: parity
  is byte-identical output, and the codec must round-trip key order
  deterministically. No hash-order nondeterminism can ever be observable.
- **Lookup `m[k]` is `Option<V>`** — the missing-key case is in the type,
  per the error-model philosophy (no `undefined`, no trap-on-missing).
  `m[k] = v` inserts or updates. `m.remove(k) -> Bool` (was present),
  `m.has(k) -> Bool`, `m.length -> Int64`, `m.keys() -> Array<String>`
  (a snapshot, safe to mutate the map while iterating the snapshot).
- **Empty literal `[:]`** — contextual like `[]` (needs an expected Map
  type). Non-empty literals `["a": 1, "b": 2]` desugar to insertions in
  written order. (`[]` stays Array-only; `[:]` is unambiguous.)
- **Ownership:** a Map owns its keys and values; `drop m` frees storage
  (recursively, the Array rules); the RFC-0004 leak/consume analysis
  extends structurally. Element accessors copy/borrow exactly as `a[i]`
  does for the same value category.
- **Equality:** no `==` on maps in v1 (Arrays don't have it either).

## Wire format & schema (the real motivation)

- **Codec:** `toJson` encodes a `Map<String, V>` as a JSON **object**
  (keys escaped as JSON strings, values via V's codec, insertion order);
  `fromJson` decodes any JSON object into it (document order = insertion
  order), validating each value as `V` with Issue paths `field.<key>`.
  Duplicate keys in input: last wins, matching the record decoder's
  existing policy (verify and mirror whatever it does — single source of
  truth).
- **Schema:** `jsonSchema` emits `{"type":"object",
  "additionalProperties": <schema of V>}`; the importer round-trips
  exactly that shape back to `Map<String, V>` (byte-exact re-emit, the
  RFC-0024 discipline). This is the TS-interop payoff: a contract can now
  carry `{ [key: string]: number }` and it means something.
- `Map` values compose with everything codable: `Map<String,
  Array<User>>`, `Option<Map<…>>`, enum payloads carrying maps (the
  RFC-0026-era payload-coercion fix already handles fat payload values —
  add the parity pin anyway).

## Implementation shape

- Runtime representation: growable insertion-ordered storage —
  `{keys, values, len, cap}` parallel arrays with linear probe or a
  side-index; v1 may be O(n) lookup internally as long as the OBSERVABLE
  semantics above hold (perf is tunable later; semantics are not).
  Interp mirrors with an order-preserving Rust structure (e.g. a Vec of
  pairs or IndexMap — but the OUTPUT is what parity checks).
- Checker: `Type::Map(V)` (String key implicit in v1 but spelled in the
  surface for future-proofing), coercion/validation at boundaries per the
  standard exhaustive-validation rules (validated V types re-validate on
  insert exactly like Array element stores).
- One new example parity citizen (`examples/mapdemo.vyrn`) covering every
  operation, ordering behavior incl. update-in-place vs re-insert, codec
  round-trip, schema emit/import, and a Map inside an enum payload.
- LSP/editor: completion for the method surface; fmt for `[:]` and map
  literals; redeploy vyrn-lsp.exe.

## Out of scope

Non-String keys (hash/eq protocols), map iteration with destructuring
(`for (k, v) in m` — needs tuples; `keys()` carries v1), map equality,
comprehensions, `values()`/`entries()` (add by demand), mixed
record+index-signature types, weak maps.

---

## As-landed notes

Shipped across parser/checker/interp/native/wasm (one shared IR)/codec/schema/
fmt/LSP with zero new dependencies. Full suite green (786 workspace + 8 LSP
tests), 0 warnings; the three-way parity corpus is byte-identical including
`examples/mapdemo.vyrn` (interp == native == wasm, every operation, ordering,
codec, schema, enum-payload Map, validated-V trap).

- **Type surface.** `Type::Map(Box<Type>, Box<Type>)` carries the *key and
  value* spellings so the checker can enforce the String-key rule and still
  future-proof the type. `ensure_type_exists` resolves the key and rejects any
  non-`String` spelling with a named diagnostic (a validated string type
  resolves to `String` and is allowed). A bare `Map<String, V>` may be named by
  a transparent `type` alias (like the `Result`/`Option` aliases) so
  `fromJson`/`jsonSchema` can target it — `assignable`/`unify` resolve such
  aliases structurally.
- **Surface reuse.** `m[k]` desugars to the existing `at(m, k)` and dispatches
  on the receiver type to yield `Option<V>`; `m[k] = v` reuses `IndexSet`;
  `has`/`remove`/`keys` are method-only `@`-names (like `@pop`); `.length` is a
  field like an array's. `[:]` / `["k": v]` are a new `Expr::MapLit`; the parser
  disambiguates on the first `:`.
- **Runtime representation.** Interp: `Val::Map(Vec<(String, Val)>)` — a Vec of
  pairs, so iteration/encode order is deterministic. Compiled (native + wasm,
  one IR): `{ ptr keys, ptr values, i64 len, i64 cap }` — two parallel growable
  buffers sharing one length/capacity; keys are `char*`, values are
  `llt(V)`-stride. Lookup is a linear `strcmp` scan (O(n) v1, per the RFC). Four
  small C-shim helpers (`__vyrn_map_find`/`_reserve`/`_remove_at`/`_keys_copy`)
  handle key comparison and buffer growth by element size; everything else is
  inline IR next to the array helpers. **Ordering** is guaranteed by the Vec /
  parallel-array layout plus insert-or-update-in-place (a hit overwrites the
  value slot; a miss appends) and an order-preserving shift on `remove` — a
  remove-then-insert therefore moves the key to the end, exactly as specified.
- **Ownership.** `Map` is a heap value (`DropKind::FreeMap`): auto-dropped like
  an array (a `mut` map keeps its identity and its buffers). `drop m` frees both
  backing buffers; elements are a **safe leak**, mirroring `Array`'s
  `AfreeArr` exactly (the RFC's "recursively" reads as "follow Array's rules",
  which is what parity actually pins). `keys()` copies the key pointers into a
  fresh `Array<String>` snapshot.
- **Boundary validation.** A validated `V` re-validates on every `m[k] = v`
  (the `emit_map_set` / interp-coerce path runs the `where` clause and traps
  with the canonical `validation failed for \`T\`` wording, byte-identical
  across backends), exactly like an array element store.
- **Codec.** Encodes as a JSON **object** (keys JSON-escaped, insertion order,
  values via V's codec). Decodes any JSON object: document order = insertion
  order, each value validated at path `field.<key>`, reusing the locked
  `json.type`/`validate` Issue vocabulary. **Duplicate keys: first wins** —
  this mirrors the record decoder's *actual* policy: `JsonV::get` /
  `__vyrn_vj_get` both return the **first** matching member (the RFC's
  "last wins" aside was superseded by "mirror whatever the record decoder does";
  the code is first-wins, so the map is too). Verified byte-exact including
  re-emit: `{"a":1,"b":2,"a":9}` → `{"a":1,"b":2}` on all three backends.
- **Schema.** Emits `{"type":"object","additionalProperties":<V schema>}`; the
  importer round-trips exactly that shape back to `Map<String, V>` (constrained
  values become a synthetic `<name>.value` validated type, mirroring array
  `items`). Emit → import → re-emit is byte-exact.
- **Surprises Array's architecture did not cover.** (1) Codegen `coerce` has no
  growable-`Array<From>`→`Array<To>` element revalidation, so `MapLit` infers
  `V` from the first value and validation rides the `m[k] = v` insert path
  instead; a validated-`V` map is filled by inserts, not literals, in practice.
  (2) A bare `Map`/`Array` had no way to be a `type` alias — added the
  transparent-alias allowance (and the matching `assignable`/`unify` resolution)
  so `fromJson`/`jsonSchema` can name one. (3) The `>`/`=` lexer greedily forms
  `>=`, so `Map<String, Int64>= x` (no space) mis-lexes — this is the
  pre-existing generic-`>=` quirk shared with `Array<T>= x`, not new; write the
  space.
