# bench-0104 — the M0 census corpus

RFC-0104's M0 asked one question per Benchmarks Game program: expressible or
not, and what does it need. This directory holds the answers — the probes that
were run, and the expected output each program is checked against.

Nothing here is a benchmark. Nothing here is timed. M1 writes the programs
properly and M2 times them; these files exist so that neither milestone has to
guess what a correct answer looks like.

M1 has landed. The eight expressible programs live in `examples/` —
`nbody.vyrn`, `spectralnorm.vyrn`, `fannkuch.vyrn`, `binarytrees.vyrn`,
`fasta.vyrn`, `revcomp.vyrn`, `knucleotide.vyrn`, `pidigits.vyrn` — where the
three-way parity harness runs them. `compiler/vyrn-cli/tests/benchgame.rs`
compares each one's output against the fixture below on every `cargo test`, so
the probes here are history and the fixtures are live.

M2 has landed too, and it is the two directories the first paragraph does not
mention.

## The harness (M2)

`harness/` holds the other side of the comparison — `c/`, `rust/` and `js/`,
one plainly-written file per program per language — and `run.py`, which builds
all five contestants, checks each against the fixture, checks all five against
each other at the timing size, and only then times anything.

```
cd harness
python run.py                    # build, verify, time ten runs, write the record
python run.py --only nbody --contestants c,rust,vyrn-native --runs 3
python run.py --floor            # also measure the empty-program start-up
```

It needs clang, rustc, node, a release `vyrn` (or `$VYRN`) and the wasmtime the
repository pins in `vyrn.lock`. **CI runs none of it** — `rfcs/**` is
CI-ignored, so this is a by-hand runner and the committed JSON under `results/`
is the record. The numbers, the environment, the noise and the named causes are
in RFC-0104's "M2 — as landed".

The eight Vyrn programs are never edited: `run.py` copies a program into its
build directory and rewrites the one `let` that carries N. That the rewrite
changes nothing else is checked, not assumed — a copy stamped with the fixture N
must still print the fixture.

## The probes

Each is a whole Vyrn program. Run any of them from this directory:

```
vyrn run p-fannkuch.vyrn
vyrn run p-revcomp.vyrn < fasta-1000.expected
```

| probe | the question it answers |
|---|---|
| `p-sqrt.vyrn` | Is there a square root for one `Float64`? (`std/math` has none; `F64x2.sqrt(..).lane(0)` is it; a hand-written Newton iteration lands 1 ULP away) |
| `p-fmt9.vyrn` | The game prints `%.9f` and Vyrn prints six places. Can nine be reached, and are the digits right? |
| `p-nbody.vyrn` | nbody at n = 1000, whole — including `bodies[i].vx = …`, a field written through an array index in the innermost loop |
| `p-spectralnorm.vyrn` | spectral-norm at N = 100, whole — `p-sqrt` and `p-fmt9` end to end against a number the game publishes |
| `p-fannkuch.vyrn` | fannkuch-redux at n = 7, whole — `Int64` arrays, in-place prefix reversal |
| `p-trees.vyrn` | binary-trees at n = 10, whole — a recursive type, and whether the movecheck accepts build-walk-drop |
| `p-fasta.vyrn` | fasta at n = 1000, whole — the game's LCG and the weighted pick, byte for byte |
| `p-revcomp.vyrn` | reverse-complement, whole — all of stdin through `readLine`, a 256-entry byte table |
| `p-mapkey.vyrn` | k-nucleotide's map: what a `Map` key built from a window of bytes costs, and how the table is ordered |
| `p-regex.vyrn` | regex-redux: what `=~` actually answers over a sequence |
| `p-mandelbrot.vyrn` | mandelbrot at 200×200 — the kernel and the bit packing, printed as hex because the bytes cannot leave |
| `p-binout.vyrn` | Can any Vyrn sink carry a binary PBM? |
| `p-pidigits.vyrn` | pidigits: are there arbitrary-precision integers, and is the output reachable without them? |
| `p-spawn.vyrn` | Can `spawn` / `join` express a data-parallel variant of one of these programs? |

Every probe was run three ways — interpreter, native, and wasm under wasmtime —
and all fourteen are byte-identical across the three. One caveat for whoever
writes M2's runner: a native Windows build writes `\r\n`, so the comparison
normalizes line endings before diffing.

## The fixtures

One expected output per program, at the small N M0 fixed, named for M1's
harness: `<program>-<N>.expected`.

| fixture | notes |
|---|---|
| `nbody-1000.expected` | |
| `spectralnorm-100.expected` | |
| `fannkuch-7.expected` | |
| `binarytrees-10.expected` | |
| `fasta-1000.expected` | also the stdin of the three programs below — fasta's output is their input, so there is one copy of it and not two |
| `revcomp-1000.expected` | from `fasta-1000.expected` on stdin |
| `knucleotide-1000.expected` | from `fasta-1000.expected` on stdin |
| `regexredux-1000.expected` | from `fasta-1000.expected` on stdin |
| `mandelbrot-200.expected` | binary (a PBM), so `.gitattributes` marks it `-text` |
| `pidigits-27.expected` | |

### Provenance

`ref/gen.py` writes every fixture and is the provenance: each routine in it is
the game's published algorithm transcribed from its specification — the
constants, the LCG, the output formats — rather than a copy of any entry's
source. `python ref/gen.py` regenerates all ten and must reproduce them byte for
byte.

The transcription is checked four ways against numbers the game itself
publishes: nbody at 1000 prints `-0.169075164` then `-0.169087605`,
spectral-norm at 100 prints `1.274219991`, fannkuch-redux at 7 prints `228` and
`Pfannkuchen(7) = 16`, and pidigits starts `3141592653`. Six of the ten fixtures
are additionally reproduced by a probe in this directory, which is a second
implementation in a second language: nbody, spectral-norm, fannkuch-redux,
binary-trees, fasta and reverse-complement all come out byte-identical, and
`p-mapkey.vyrn` reproduces the first two sections of the k-nucleotide fixture.

`ref/checkhex.py` is the mandelbrot comparison: the probe prints hex because it
cannot print the PBM, and this turns the fixture into the same form.

```
vyrn run p-mandelbrot.vyrn | python ref/checkhex.py
```

One fixture-level decision that the game's own input never exercises:
`knucleotide-1000.expected` breaks equal counts by fragment ascending. The game
publishes no rule for a tie because its input has none at k = 1 or k = 2.

## What is not here

`regexredux-1000.expected` has a fixture and no probe that reproduces it. That
is the census answer for regex-redux, not an omission — see RFC-0104's
"M0 — as landed".
