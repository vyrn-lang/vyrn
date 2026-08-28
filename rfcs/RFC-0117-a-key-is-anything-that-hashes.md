# RFC-0117 — a key is anything that hashes

- **Status:** M1 and M3 Implemented — `Map<Int64, V>` in all four execution
  paths (2026-08-26), `Hashable` declared in `std/hash` with the `Int64` and
  `String` impls, and the wire form chosen and built (2026-08-28): §5's
  stringified keys, canonical decimal, schema-described, pinned three ways by
  `examples/wirekey.vyrn`. See "M1 — as landed" for the three places that
  milestone deviated from this document and why. M2 is what remains, reduced
  to user heapless types and parked on real demand (its scalar half dissolved
  — see the milestone note). The direction is the user's (2026-08-26): a
  hashable protocol, not an `Int64` special case.
- **Evidence:** the corrected census re-measurement
  (rfcs/census/knucleotide-needs-an-integer-key.md, "Corrected"): a rolling
  integer key costs 2.2–2.5 ns a window against 11 ns for the byte-keyed hit
  path and 32 ns with real misses — the counting loop has a further 3–5x
  behind an integer key, and the counting loop is the benchmark. And the
  design norm: every general-purpose language hashes keys generically; keys
  that must be `String` was RFC-0028's v1 economy, not a position.

## 1. The surface

```vyrn
export protocol Hashable {
    /// Equal values return equal hashes. Beyond that the value is the
    /// implementation's; the map mixes and masks it itself.
    fn hash(self) -> UInt64
}

let mut seen: Map<Int64, Int64> = [:]
seen.tally(key, 1)                       // every Map operation, any legal K
let n = seen[key]                        // Option<Int64>, exactly as today
for k in seen.keys() { ... }             // Array<K>, insertion order
```

A `Map<K, V>` key is any type that is **`Hashable` and heapless**. `String`
remains a key exactly as it is — its probe, its `tallyBytes`, its wire form
are untouched — and gains a `Hashable` impl that names the FNV-1a the runtime
already runs, so the protocol describes what exists rather than adding a
second truth.

`Type::Map(k, v)` has carried a key type since RFC-0028 "for future-proofing";
the whole v1 restriction is one checker arm (checker.rs, "a `Map` key must be
`String` in v1"). This RFC replaces that arm with the real rule.

## 2. Which types, and why the line sits there

**In (M1):** the sized integers (`Int8`–`Int64`, `UInt8`–`UInt64`), `Bool`,
and `String` as today. Their `Hashable` impls ship in the prelude's protocol
table alongside `Copy`/`Owned`.

**Out, with reasons, not forever:**

- **`Float64`/`Float32`** — refused with a named diagnostic. `NaN != NaN`
  breaks the reflexivity a key needs, and `+0.0 == -0.0` hash-equal is a trap
  either way. A program that wants float keys has `toBits`-style spellings to
  make the decision explicit; the language should not make it silently.
- **Heap-owning types other than `String`** — refused. A key is stored,
  compared and freed by the map; `String` earned its machinery (RFC-0028,
  RFC-0116) and a second heap key type earns its own when something real
  demands it.
- **User records and enums** — M2. A heapless record of scalar fields can be
  a fine key, but padding bytes mean equality must be generated field-wise,
  not `memcmp`, and the impl obligation (`impl Hashable for Point`) plus the
  generated equality is its own milestone.

## 3. Equality is bits, and the hash is unobservable

For a scalar key, equality is the value's bits — one fixed-width compare, no
protocol needed. That is why v1 needs only `Hashable` and not an `Eq`
obligation; M2's record keys are where field-wise equality gets generated.

The hash never decides anything observable. RFC-0028's law stands: the pairs
vector is the value, first-insertion order decides iteration, encoding and
`keys()`, and the index beside it holds positions only. A different hash
would produce a byte-identical program — which is also why `hash(self)` being
ordinary monomorphized Vyrn code costs parity nothing. The `Int64` impl is a
SplitMix64-style finalizer (std/bitwise already carries the constants); the
`String` impl is FNV-1a as today.

## 4. The runtime, briefly

The map header stays `{keys, vals, len, cap, idx}`. Today `keys` is an array
of `char*`; for a scalar `K` it is an array of `K`'s stride, and the probe —
hash, mask, bucket walk, bit compare — is **emitted monomorphized** in both
compiled backends, the way every generic already lowers (RFC-0101). No new
shim entry points: `map_find`/`map_find_bytes` stay String's; a scalar probe
is a handful of inline instructions and a call would cost more than the work.
The interpreter's `MapVal` grows a key representation beside its `String` one
and keeps the same pairs-plus-index shape.

`tally` generalizes to `Map<K, Int64>` for every legal `K`. `tallyBytes`
stays `String`-keyed — it exists precisely for keys that arrive as bytes.

## 5. The wire form — the open question, held open safely

A `Map<String, V>` **is** a JSON object, byte-for-byte, and that equivalence
is RFC-0028's real motivation. A non-String key breaks it, and there are only
three honest answers:

1. **Stringify the keys** — `{"7": 1}`. Stays an object; needs a canonical
   render/parse pair per key type, and a decoder that knows the target type
   to read keys back. The likely eventual answer.
2. **An array of pairs** — `[[7, 1]]`. Exact and general; `Map` stops having
   one wire shape, and every schema consumer learns a second one.
3. **No wire form yet** — the boundary refuses.

**This RFC took 3 for M1**: the boundary refused while the choice stood open.
**M3 chose 1 (2026-08-28, on the user's "finish"), and it is implemented**:
`Map<Int64, V>` crosses as a JSON object whose keys are the decimal texts
`toString` writes, and the decoder reads keys back through the target type in
CANONICAL form only — `"007"` is an Issue, never a silent alias of `"7"` (two
spellings of one key collapsing into one entry with no Issue saying so is the
leniency the canon rule exists to refuse). The schema says what the keys are
with `propertyNames` (`^-?(0|[1-9][0-9]*)$`) beside the usual
`additionalProperties`. Insertion order survives the round trip, because the
object's fields are written in it and read back in it. Why 1 over 2: the value
stays a JSON object — the shape every generic JSON consumer indexes by key —
where pair arrays would trade that for exactness the canonical-text rule
already provides. The schema IMPORTER still reads such a schema back as
`Map<String, V>` (recognizing the `propertyNames` marker is additive work for
whenever a real import needs it). `examples/wirekey.vyrn` is the three-way
witness.

**Why one, and where configurability lives** (discussed 2026-08-28). What the
two ends of a wire share is a TYPE, so the spelling is one PER TYPE, chosen
where both ends can read it — never per call. A flag on `toJson` lives in one
function body where `fromJson`, `jsonSchema` and the generated clients cannot
see it, which is how a typed contract stops being one. A program that needs a
different scheme spells the scheme as a type — `Array<Entry>` with one
conversion — and the schema, the decoder and parity are automatically honest,
which is this language's general character (`Int64(x)`, `.copy()`: spell the
decision). If per-type choice ever needs to be declarative, the growth path is
a `Wire` protocol — the type itself declaring how it crosses, serde's model —
which keeps the property that matters: attached to the declaration, visible to
every consumer, still one spelling per type. Presentation is a different axis
entirely and IS configurable, as `std/json` library functions over the DOM:
`emitPretty` exists; sorted keys and a JSON5 reader belong beside it, and none
of them create a second wire form because they emit from or read into the one
`Json`. So this section's open question is exactly one line wide: what the
DEFAULT `Map<Int64, V>` spells as.

## 6. What it buys, measured

The corrected census table, native, 2,000,000 bases, one binary, one run:

| phase | k=12 | k=18 |
| --- | --- | --- |
| rolling integer key, no map | 4.2 ms | 5.0 ms |
| `tallyBytes`, all hits | 20.9 ms | 22.8 ms |
| `tallyBytes`, real misses | 63.4 ms | 65.8 ms |

k-nucleotide's counting loop becomes:

```vyrn
key = (key * 4 + code(b)) % modulus     // O(1) per window, any k
m.tally(key, 1)
```

no window buffer, no per-byte hash, no per-byte compare, and the String path
keeps `tallyBytes` for keys that genuinely are bytes.

## M1 — as landed

Three deviations from the sections above, each cheaper than what was written
and none of them surface-visible:

1. **M1's key set is `Int64`, not all the scalars.** A `UInt64` key above
   `Int64`'s ceiling would read back through an `Int64`-shaped slot and print
   wrong; the sized integers need canonicalization on the way in and the
   declared key type on the way out. That machinery is exactly what M2's user
   types need too, so the other scalars moved there rather than shipping
   half-made. Floats stay refused by name as §2 says; every other key spelling
   gets the M1 diagnostic naming this RFC.
2. **The probe is a runtime family beside the string one, not inline
   emission.** §4 said "emitted monomorphized … a call would cost more than
   the work"; what landed is `__vyrn_map_find_i64` and friends in the shim
   (one-line definitions, SplitMix64's finalizer) and a
   `map_find_i64`/`map_slot_i64`/`map_put_i64`/`map_reindex_i64` chain in the
   direct backend's runtime — the same call shape `tallyBytes` already
   measured at 11 ns a probe, at a tenth of the emission surface. If a
   measurement ever shows the call dominating, inlining is a backend change
   with no surface behind it.
3. **The empty literal upgrades lazily.** `[:]` evaluates before any key
   exists; the interpreter represents Int64-keyed maps as their own value
   (`Val::MapI`) and an empty string-keyed `[:]` under an Int64-keyed type is
   settled by the first insert — every read of an empty map has a
   kind-independent answer, so nothing can observe the interim.

Two defects fixed en route, both caught by the M1 witnesses: the prelude's
seeded `@keys` row pinned `Array<String>` whatever the map, so the for-loop's
temporary release freed integer keys as someone's pointers (the row is generic
over the key now); and the wire refusal found its one choke point in
`codec::wire`, which every codec and schema consumer already classifies
through — the diagnostic names the offending map type rather than this RFC,
because `toJson cannot encode Map<Int64, Int64>` is findable and a custom
string at every consumer was not worth the thread.

## 7. Milestones

- **M1** — `Hashable` in the prelude protocol table with scalar impls; the
  checker arm replaced by the real rule (scalar-Hashable-heapless, or
  `String`); interp key representation; monomorphized scalar probe in both
  compiled backends; `tally` over any legal `K`; boundary refusals of §5;
  refusal witnesses (`Float64` key, heap key, wire crossing) in
  EXPECTED_CHECK_FAILURE; a corpus witness counting by rolling `Int64` key;
  three-way parity.
- **M2** — user heapless types as keys: `impl Hashable for T` accepted,
  field-wise equality generated, padding never compared. **The scalar half of
  M2 dissolved on inspection** (2026-08-26): this language never widens
  implicitly — mixed-width arithmetic is spelled `Int64(x)`, `s[i]` needs it
  too (RFC-0022) — so a narrow-int key is one explicit conversion away from
  `Map<Int64, V>`, and even a full-range `UInt64` survives `Int64(u)`
  faithfully, because the wrap is a bijection and equality is what a key
  needs. `Map<Int8, V>` would be implicit sugar the language deliberately does
  not do anywhere else. What remains of M2 is the user types, and they wait
  where RFC-0028 waited: until something real demands one.
- **M3** — the wire form: **done** (2026-08-28). §5's option 1, stringified
  keys in canonical decimal, chosen and implemented — the refusals of M1
  became the codec, the schema names its key rule, and the round trip is
  pinned three ways by `examples/wirekey.vyrn`.
