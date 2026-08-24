# The `vyrn bench` name collision — diagnosed

The standard library census could not measure `std/von` because
`vyrn bench` fails on any program importing it. That was recorded as a compiler
defect, narrowed, and not diagnosed. It is diagnosed here, and it is not a
`std/von` problem.

## What it actually is

**A private function name in a user's module collides with a private function
name in `std/jsonread`, and `vyrn bench` fails to compile.** The user never
imports `std/jsonread`. The bench harness does it for them.

Twenty-three names are affected. Several are ordinary words a person would
reach for without thinking:

```
maxDepth  newParser  cur       ahead     step      errAt     skipWs
isHex     hexVal     pushUtf8  readHex4  parseString  parseNumber
parseKeyword  parseValue  parseArray  newKeySet  ksFind  ksPlace
ksAdd     parseObject  parseOr   parseErr  nest
```

`std/von` is affected because it happens to define a private `errAt`, the same
name `std/jsonread` uses. Nothing about `std/von` is otherwise special.

## The minimal reproduction

Two files. Neither mentions `std/jsonread`.

`mc.vyrn`:

```
type MyState = { toks: Array<Int64>, n: Int64, i: Int64, src: Array<UInt8> }

fn errAt(p: MyState, msg: String) -> String {
    return msg + p.n.toString() + p.toks.length.toString()
}

export fn useC() -> String {
    let a = MyState { toks: [1, 2], n: 2, i: 0, src: [] }
    return errAt(a, "c")
}
```

`mcmain.vyrn`:

```
import { useC } from "./mc.vyrn"

fn work() -> Int64 { return 1 }

bench "t" {
    blackBox(work())
}

fn main() -> Int64 { return 0 }
```

`vyrn bench mcmain.vyrn` fails. `vyrn build` and `vyrn run` on the same files
succeed. Rename `errAt` to anything outside the list above and the bench passes.
The import is never used and it still fails.

## How it was found

The error, `field \`toks\` missing during coercion`, is raised at
`compiler/vyrn-codegen/src/lib.rs:2817`, in `coerce`, which rebuilds one record
type into another and errors when a target field has no source field. Patching
that message temporarily to print both types gave the answer in one run:

```
field `toks` missing during coercion
  (from Named("Parser") to Named("VonP"); source fields ["src", "n", "pos", "line", "col"])
```

`Parser` is `std/jsonread`'s private record type. `VonP` is `std/von`'s. Two
unrelated private types from two modules, and the compiler is coercing one into
the other — because the two modules' private `errAt` functions collapsed into
one symbol, so the surviving body is called with the other module's record.

Two hypotheses were tested and killed before this one. It is not a `gen fn`
problem: `std/vyx` has `gen fn` exports and benches cleanly. It is not the
`consume`-parameter-into-record shape: a standalone program with that structure
compiles and benches cleanly.

## Why only `vyrn bench`

`compiler/vyrn-cli/src/main.rs:2178` injects `import { parseJson } from
"std/jsonread"` into the program it is about to run, and the bench harness pulls
in `benchJson` and `BenchResult` with their transitive `std/time` and
`std/json`. `vyrn build` injects none of that, so the two modules never meet and
the collision never forms.

That also means the affected name list is not fixed at twenty-three. It is
whatever private names the injected modules hold, and it grows when the harness
grows.

## What this is, stated plainly

**Private function names are not module-scoped in the path `vyrn bench` uses.**
A user cannot name a private function `step`, `cur`, `ahead`, `nest` or
`parseValue` and run a benchmark, and the error they get names a type from a
module they never imported.

The failure is loud here. Whether a collision of two functions with COMPATIBLE
record shapes would be caught at all, rather than silently calling the wrong
body, is the question worth answering next, and this census does not answer it.

Renaming `std/von`'s `errAt` makes the bench pass and is NOT the fix. It moves
one module out of the way of a hole every user program stands in front of.
