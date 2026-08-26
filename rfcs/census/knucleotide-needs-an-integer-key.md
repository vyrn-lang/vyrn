# k-nucleotide is a String allocated per window, and the key cannot be anything else

Status: **re-measured 2026-08-26, after RFC-0116, then CORRECTED the same day.
The premise below is gone — `tallyBytes` builds the key once per distinct
fragment, not once per window — but the first re-measurement's verdict of "no
performance case left" was wrong: it charged the integer path an O(k) repack
per window that no integer-keyed program performs. The rolling key runs 5–14x
below the byte-keyed counting loop; see "Corrected". The performance case is
real, and the two design questions at the bottom now have a number attached.
Everything from here to "Re-measured" is the 2026-08-25 record, kept as
written.**

RFC-0104 attributed k-nucleotide's 2.9x to "a heap `String` key manufactured per
window position". That is right, it is the dominant cost, and the program cannot
avoid it: `Map` keys must be `String`.

## The split

19,989 windows of width 12, interpreted, each phase timed on its own:

| phase | time |
| --- | --- |
| building the keys, no map touched | **59.6 ms** |
| the map's insert and lookup, one key reused | **17.3 ms** |

Key construction is 3.4 times the map work. (The map figure is a floor: one
reused key never grows the table, so a real run hashes more and rehashes. The
direction is what matters, and the direction is not close.)

## What the program can do, and what it cannot

**Can:** stop building the key a byte at a time. Today `countKmers` pushes `k`
bytes into an `Array<UInt8>` and calls `stringFromBytes`. Over a `String`
sequence, `substring(s, i, i + k)` is one call and RFC-0113 made it a memcpy.
Measured: 129.1 ms against 101.3 ms for the same 19,989 windows, **22 per cent**.

**Cannot:** stop allocating. Every one of those windows still becomes a heap
`String`, because that is the only thing a `Map` will take:

```
a `Map` key must be `String` in v1, found `Int64` (RFC-0028; validated string
types are allowed)
```

The C reference allocates nothing. A k-mer over four bases is two bits a base,
so `k <= 32` packs into one integer and the table is keyed on that. That is not
a cleverness the benchmark is entitled to and Vyrn is not — it is the ordinary
way to count k-mers, and the language cannot express it.

## The decision this needs

`Map<Int64, V>` — RFC-0028's deferred second key type. It is not a free
extension, and the question is not the hash:

- **What does it serialize to?** `Map<String, V>` is a JSON object and round
  trips through `std/json` byte-exactly, which is a property RFC-0028 built and
  `jsonSchema` reflects as `additionalProperties`. A JSON object's keys are
  strings. `Map<Int64, V>` either stringifies its keys on the way out — and
  then `{"7": 1}` reads back as a `Map<String, V>` unless the decoder is told
  otherwise — or it is a different shape (an array of pairs), and `Map` stops
  having one wire form.
- **Which key types?** Stopping at `Int64` is arbitrary; admitting every scalar
  is a wider change to hashing and to the codec.

Neither is hard. Both are choices about the standard library's surface rather
than about a benchmark, which is why this file records the measurement and stops.

## What was NOT done

The 22 per cent program change above is not applied. On its own it moves 2.9x to
roughly 2.6x, leaves the allocation in place, and would have to be undone the day
an integer key lands — `countKmers` would build a number, not a substring. It is
recorded here so that whoever takes the decision knows the program half is
cheap and already measured.

## Re-measured after RFC-0116 (2026-08-26)

Method: one probe program, five phases over the same synthetic sequence, each
timed on its own. `refill` is the window loop alone (no map). `pack` is the
window loop plus 2-bit packing to an `Int64` — the loop floor of the
`Map<Int64, V>` program that cannot be written. `tallyB hits` runs `tallyBytes`
over an ACGT-tiled sequence (four distinct k-mers, so the miss path never
runs); `tallyB distinct` runs it over the game's fasta LCG read from its high
bits; `stringT` is the pre-0116 path, a fresh `String` per window into `tally`.

Native, 2,000,000 bases:

| phase | k=12 | k=18 |
| --- | --- | --- |
| refill | 9.3 ms | 3.0 ms |
| pack | 13.9 ms | 20.3 ms |
| tallyB hits | 22.1 ms | 23.5 ms |
| tallyB distinct | 127.8 ms | 106.0 ms |
| stringT distinct | 263.5 ms | 263.8 ms |

Two facts stand from this pass:

- **The hit path costs 11 ns a window** (22.1 ms over two million probes:
  FNV over the window's bytes plus one length-aware compare).
- **stringT against tallyB is 2.1–2.5x** — the census's cost, already banked
  by RFC-0116 without a new key type.

The third conclusion this pass drew — that the packing loop's 14–20 ms put the
integer program's floor at the byte program's whole map cost, closing the case
— did not survive the day. See "Corrected".

## Corrected: the packing floor was measured wrong (same day)

The `pack` phase above rebuilds the integer key from scratch for every window:
an inner loop over all k bytes. No integer-keyed program does that. The C
reference maintains the key **incrementally** — two bits shift in, the top two
age out, O(1) per position at any k. The user caught this ("I don't think
other languages convert keys to strings"); re-measured with a rolling key,
`key = (key * 4 + code) mod 4^k`, same binary, same 2,000,000 bases:

| phase | k=12 | k=18 |
| --- | --- | --- |
| roll (integer key maintained, no map) | 4.2 ms | 5.0 ms |
| tallyB hits (same run) | 20.9 ms | 22.8 ms |
| tallyB distinct (same run) | 63.4 ms | 65.8 ms |

The rolling key costs 2.2–2.5 ns a window — **5x under the byte-keyed hit path
and 14x under the counting loop with real misses**. An `Int64`-keyed map would
add a cheap integer hash and probe on top of that floor; a plausible whole-loop
estimate is 3–5x faster than today's `tallyBytes` path, and the counting loop
IS this benchmark. The byte-keyed probe cannot close that gap structurally:
it must touch k bytes per window (refill, hash, compare) where the rolling key
touches one.

So the honest position is the reverse of the morning's: `tallyBytes` banked
2–4x without a new key type, and `Map<Int64, V>` has a further ~3–5x on this
benchmark behind it. The decision remains the two design questions — the wire
form and the key-type set — but they now carry a real number, not zero.

## What the re-measurement caught instead

Two real defects, both fixed the same day:

- **The interpreter was quadratic in distinct keys — still.** `m.tally(k, n)`
  desugars to `m = @tally(m, k, n)`, and evaluating that receiver argument
  clones the map's `Rc` to refcount 2, so the builtin's `Rc::make_mut`
  deep-copied the whole table (pairs and index) on every call: 0.9 s for 5,000
  distinct keys, 3.3 s for 10,000, 15.8 s for 20,000, against 76 ms for the
  same loop over four keys. The `xs.push(v)` desugar had an in-place
  fast path for precisely this shape; `tally`, `tallyBytes` and `append`
  (RFC-0115/0116, newer than that fix) did not. They do now: the 20,000-key
  phase fell from 15.8 s to 76 ms, 209x, and distinct-key tallying is linear
  in this engine too.
- **The example's bench sequence was ACGT tiled.** `syntheticSequence` read the
  LCG through `state % 4`, and 3877 and 29573 are both 1 mod 4, so the low bits
  count 0,1,2,3 and every k-mer table it fed held four keys. The comment beside
  it claimed the opposite. It reads the high bits now, the way the game's own
  fasta does.
