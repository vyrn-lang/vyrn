# Four values, one defect, and the interpreter it was hiding in

`Val::Array` and `Val::Str` were behind an `Rc`. `Val::Map` and `Val::Record`
were not. Reading a binding clones the value, so every read of a map copied the
whole table and every pass of a record copied every field.

The compiled backends never had any of this. It was the interpreter alone —
which is what `vyrn run`, every `gen fn`, every `test` block and every `vyrn
check` of a program with a generator actually use.

## What was measured

| shape | before | after |
| --- | --- | --- |
| 2,000 reads of one key, 8,000-entry map | 1.371 s | 0.022 s |
| 200,000 calls taking a 64-field record | 1.868 s | 0.199 s |
| `examples/knucleotide.vyrn`, fasta n = 4000 | 35.14 s | 0.82 s |
| `std/jsonread` under a `gen fn`, 8,000-element array | 36.38 s | 0.82 s |
| `vyrn run site/export.vyrn`, 80 routes | 62.03 s | 45.47 s |

The last row is the one that matters most, because it is not a microbenchmark
and nobody wrote it to be fast. It is a CI step.

## The four changes

1. **The coercion proof is memoized on the array's identity.** Every typed
   boundary asks whether coercing this value would change it. For an array the
   answer is decided per element, and for a sized integer or a stamped record it
   cannot be decided from the type alone — so the check read the whole array, on
   every call, and an O(n) parser ran in O(n^2). See
   `comptime-parsing-quadratic.md` for how that one was found.
2. **`Val::Map` goes behind an `Rc`.** Cloning a `MapVal` copied both the pair
   vector and the index, so `m[k]` copied the table before the hashed lookup
   ran.
3. **`Val::Record` goes behind an `Rc`.** Cloning copied every field, including
   every field the callee never looks at.
4. **`std/von`'s reader** stopped crossing the document once per number and once
   per field. Real, and the smallest of the four.

Writes go through `Rc::make_mut` in both cases, so a uniquely owned value is
still edited in place.

## The weak handle, which is not a detail

Two of these hold a reference to a value they did not create. The coercion memo
holds one; a strong handle there owns the array a SECOND time, so `Rc::make_mut`
can no longer edit in place and every element store clones the row. An existing
performance test, `an_element_store_does_not_restamp_its_row`, failed exactly
that way on the first attempt and was right to.

A weak handle costs mutation nothing, and it is also what makes the entry safe
to trust: an upgrade that still succeeds, to the same address, proves the
allocation was never written since it was proven.

## What is left in this family

`Val::Enum` holds a bare `Vec<Val>` and `Val::Stream` a `Box`. Enum was
measured and is not a problem in practice — a variant's payload is a handful of
slots, and a big child is itself an `Rc` — so 20,000 passes of a tree with 800
children is flat. Stream is linear by construction.

## Each one is pinned

`compiler/vyrn-cli/tests/places.rs` now holds three interpreter ratios of the
same shape: an element store must not restamp its row, a map read must not copy
the table, and passing a record must not copy its fields. Each is a ratio
between two programs differing in one number, so a loaded machine slows both.
Each was verified to FAIL against the binary from before its fix.
