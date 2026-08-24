# Reading bytes at generation time was quadratic, and the standard library was not at fault

The standard library census recorded `std/von` as unmeasurable and its reader as
carrying two quadratic loops. Both statements were true. Neither was the reason
the reader was slow.

## The measurement that started it

`std/von`'s reader is a `gen fn`, so it runs in the interpreter at compile time
and its cost is build time. A flat VON document, one field per line, read by a
generator that parses it and returns a constant:

| elements | build |
| --- | --- |
| 400 | 0.276 s |
| 800 | 0.889 s |
| 1600 | 3.479 s |

Four times the time for twice the input. Reading the same file without parsing
it was flat at 0.024 s, so the cost was inside `parseVon` and nowhere else.

## What it was not

The census named `fieldLine`, which scanned the fields already stored to find a
duplicate — n(n-1)/2 String comparisons. Real, and worth fixing, and small:
indexing it moved an 800-field document from 1.94 s to 1.54 s.

It also named `lineStartOffset`, which restarted at byte 0 for every number
token. Also real, also worth fixing, also small: a cursor moved the same
document from 3.42 s to 1.94 s.

Both fixed, the document was still quadratic. So the search moved outward.

Five shapes were timed as generators and every one of them was linear: pushing
160,000 integers, pushing enum values with array payloads, `lex()` and the
token copy that follows it, reading an array through a record field, and calling
a function that takes the parser state by `modify`. The one shape that was not
linear differed from the last of those by a single field:

```
type St = { toks: Array<Tok>, n: Int64, i: Int64 }               // linear
type St = { toks: Array<Tok>, n: Int64, i: Int64, src: Array<UInt8> }  // quadratic
```

Same code around it. Narrowing further, with a record carrying one array and a
function called n times that only reads its `n` field:

| elements | `Array<UInt8>` | `Array<Int64>` | `Array<Int32>` |
| --- | --- | --- | --- |
| 1000 | 0.033 s | 0.021 s | 0.030 s |
| 2000 | 0.041 s | 0.024 s | 0.042 s |
| 4000 | 0.085 s | 0.027 s | 0.091 s |
| 8000 | 0.277 s | 0.032 s | 0.278 s |

Not bytes. SIZED integers. `Int32` behaves exactly as `UInt8` does, and `Int64`
is free.

## What it was

Every typed boundary asks the interpreter whether coercing this value would
change it. `Array<Int64>` answers from the type alone. A sized integer cannot:
the type wraps, so only the values can say. The check therefore reads every
element — and a parser passes its state to a helper on every token, so an O(n)
walk ran in O(n^2).

The rebuild this check exists to avoid was already fixed once (RFC-0082 M2,
RFC-0107 M2). The check itself was still O(n).

## The size of it

`std/jsonread` under a `gen fn`, on an n-element JSON array:

| elements | before | after |
| --- | --- | --- |
| 1000 | 0.663 s | 0.123 s |
| 2000 | 2.144 s | 0.214 s |
| 4000 | 8.908 s | 0.423 s |
| 8000 | 36.382 s | 0.817 s |

Quadratic to linear, and 44x at 8,000 elements. Every generator that reads JSON
or VON at compile time was paying it. `std/scan`, `std/strings` and `std/text`
have the same shape and were paying it too.

The fix memoizes the answer on the array's identity, the way `lineAt` already
memoizes its line-start table, for the two element types a parser's state
carries: a sized integer and a record that only needs its name stamped.

The handle has to be WEAK. A strong one owns the array a second time, so
`Rc::make_mut` can no longer edit in place and every element store clones the
row. An existing performance test,
`an_element_store_does_not_restamp_its_row`, failed exactly that way on the
first attempt.

## What this says about the census

The census read the code accurately and attributed the cost to what it could
see. Both loops it named were real. Neither was more than a fifth of the number.
The cost was one level down, in the interpreter, in a check that the standard
library cannot see and did not write.

A reading of source can find a quadratic loop. It cannot rank two quadratics
against each other, and it cannot find a third that is not in the file. Only
measurement does that, and the census marked this module `NOT MEASURED`.

Native is unaffected: the same program benches at 2.17 µs against 2.35 µs for
the `Int64` shape that was always free. This was compile time, not run time.
