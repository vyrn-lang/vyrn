# RFC-0028 — `Map<String, V>`: The Dictionary Type

- **Status:** Draft (design locked)
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
