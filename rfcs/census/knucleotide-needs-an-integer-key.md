# k-nucleotide is a String allocated per window, and the key cannot be anything else

Status: **measured 2026-08-25, not fixed. Needs a language decision.**

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
