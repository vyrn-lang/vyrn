# std/code

The `Code` helpers a generator writes in Vyrn: list forms over code quotes.

A quote holds holes only, and logic lives in ordinary functions that return
`Code`. `join` is the one list form: a hole takes no `Array<Code>`.

```vyrn
import { join } from "std/code"

export gen fn sum(names: Array<String>) -> Code {
    let mut terms: Array<Code> = []
    for n in names {
        terms.push(vyrn"\{name(n)}()")
    }
    return vyrn"export fn total() -> Int64 { return \{join(terms, vyrn" + ")} }"
}
```

## join

```vyrn
fn join(xs: Array<Code>, sep: Code) -> Code
```

Joins `xs` with `sep` between each pair; no element gives empty `Code`.

## arm

```vyrn
fn arm(cond: Code, body: Code) -> Code
```

One arm of an `if` chain ([`ifChain`](#ifchain)): `if cond { body }`.

## ifChain

```vyrn
fn ifChain(arms: Array<Code>) -> Code
```

The `arms` in order, each after the first behind `else`: `if c0 { b0 }
else if c1 { b1 }`. Each arm is one [`arm`](#arm); no arms is no chain.

## ifChainOr

```vyrn
fn ifChainOr(arms: Array<Code>, last: Code) -> Code
```

[`ifChain`](#ifchain) closed by `else { last }`; no arms is `last` alone.
