# RFC-0116 — `tally`

- **Status:** Implemented — `m.tally(k, n)` on `Map<String, Int64>`, in all
  four execution paths, with `push`'s write-back statement form.
- **Evidence:** the benchmarks census work item — "one search per update, so
  `m[k] = m[k] + 1` does not scan the map twice" — and k-nucleotide's count
  loop, which spelled the read-then-store in five lines and two probes.

## The surface

```vyrn
let mut m: Map<String, Int64> = [:]
m.tally(key, 1)     // insert-or-add, one probe
```

A hit adds `n` to the count in place; a miss inserts the key with `n` as its
first count. One `map_find` either way — the fusion a read-then-store cannot
compose, which is why this is a primitive (the core moves 94 → 95 with that
reasoning on the pin).

`Int64` values only: the add is the operation, and the signature can spell it
for no other value type. The map's insertion order is untouched — a hit keeps
the entry where it was, a miss appends, exactly as `m[k] = v` behaves.

## The key is never taken

The callee never takes the key: a hit touches nothing, a miss stores a COPY.
So `m.tally(w, 1)` in a loop leaves `w` the caller's, and a temporary key is
the argument machinery's to free, exactly once. The first draft freed a
surplus key on the hit path — the same key the RFC-0114 argument machinery
frees — and **the free audit caught it before any test did**: the witness
died with `free audit: double or foreign free` on the spot. That is the
standing gate doing for this RFC what it was built to do.

## `tallyBytes` — the key exists only on a miss

`m.tallyBytes(w, n)` is the same insert-or-add keyed by an `Array<UInt8>`.
On the native backend the hit path — the hot one in a counting loop —
compares the bytes where they lie: `__vyrn_map_find_bytes` hashes exactly
`w.length` bytes (FNV-1a, the same constants as `map_hash`, and equal to it
for any byte run that can match a key) and compares length-aware against the
stored keys. No String, no UTF-8 validation, no allocation. Only a miss
builds the key, through the same `bytes_dup` + DFA the language's
`stringFromBytes` uses; bytes that are not a String trap, one wording for
both reasons, byte-identical in all three engines:

> error: tallyBytes: the bytes are not a String — `stringFromBytes` names why

The direct wasm backend validates and builds the key first and then tallies
— the SEMANTICS are parity's contract, the one-probe economy is the native
backend's. The asymmetry is deliberate and this section is its record.

k-nucleotide's whole inner loop moved: one window buffer for the entire
scan (RFC-0115's `reserve` plus element overwrites), one `tallyBytes` per
window, and the fixture's bytes are unchanged. A window that has been seen
costs one probe; the String for a fragment is built once per distinct
fragment, not once per position.

## What it replaced

k-nucleotide's count loop — a `match m[key]` for the old count and an
`m[key] = seen + 1` after it, two probes of the same index for the same key —
is `m.tally(key, 1)`.
