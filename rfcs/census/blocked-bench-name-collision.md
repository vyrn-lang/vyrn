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

`bench_native` in `compiler/vyrn-cli/src/main.rs` loaded a synthetic root
importing `std/bench`, which pulls in `std/time`, `std/json` and `std/jsonread`,
then merged every declaration that load produced into the user's program —
"skipping any name the program already has". The key was the bare name. So a std
module's PRIVATE function was dropped whenever the root program happened to
declare the same name, and the module's own calls then bound to the root's body.

`vyrn build` and `vyrn run` load once, and a single load already prevents this:
the loader auto-renames a private declaration whose name appears in another
module (name-privacy, RFC-0046 §3). The second load is what hid the user's
program from that rule.

That also means the affected name list is not fixed at twenty-three. It is
whatever private names the injected modules hold, and it grows when the harness
grows.

## What this is, stated plainly

**Private function names are not module-scoped in the path `vyrn bench` uses.**
A user cannot name a private function `step`, `cur`, `ahead`, `nest` or
`parseValue` and run a benchmark, and the error they get names a type from a
module they never imported.

Renaming `std/von`'s `errAt` makes the bench pass and is NOT the fix. It moves
one module out of the way of a hole every user program stands in front of.

## The compatible-shape case is silent, and it corrupts the numbers

The question this census first left open — whether a collision of two functions
with COMPATIBLE shapes is caught at all — has an answer, and it is worse than the
loud case.

`std/bench` formats every duration it prints with a private `twoDecimals`. A root
program declaring its own:

```
fn twoDecimals(value: Int64, unit: Int64) -> String {
    return "XX"
}
```

benched clean, exit 0, and printed:

```
bench "slow"   min XX µs   median XX µs   mean XX µs   (464 samples x 16 iters)
```

The harness called the user's function to format its own timings. No error, no
warning. The same substitution under `--json` writes the wrong numbers into a
report, and `--compare` reads that report to decide whether a benchmark
regressed.

The loud coercion error was the lucky case: it needed the two record shapes to
disagree. When they agree, the wrong body runs.

## The fix

One load instead of two. `bench_native` re-reads the user's source with
`import { benchOne } from "std/bench"` APPENDED — appended, not prepended, so
every original line keeps its number — and loads that. The sixty-line merge is
deleted. The loader then sees the root program and every injected module
together, which is the only condition its name-privacy rename needs.

`std/von` benches. So does every one of the twenty-three names. Two regression
tests in `compiler/vyrn-cli/tests/benching.rs` pin both faces: that a root
`twoDecimals` no longer formats the report, and that a root `cur` and `step` of
an unrelated record shape compile.

Both tests are `#[ignore]`d because the native path needs clang.

Why no gate caught this is worth stating exactly, because the loose version of
the sentence is wrong. CI has two bench steps. `bench --check` runs on every
operating system and blocks the build, and it never loaded the harness at all —
it runs each body under the interpreter against the unmerged program. The
`benchmarks` job DOES run the native path, `--json` and `--compare` over the
whole corpus. So the gate existed. It saw nothing because no example under
`examples/` declares a name that collides with a private of `std/bench`,
`std/time`, `std/json` or `std/jsonread`, and a defect that only fires on the
user's choice of name cannot be caught by a corpus of the project's own files.

That is the general shape: the corpus tests the compiler against code the
project wrote, and this defect is triggered by code the project did not write.
The two new tests are the first bench tests that pick the name on purpose.
