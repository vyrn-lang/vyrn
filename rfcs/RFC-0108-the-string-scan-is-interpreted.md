# RFC-0108 — The String Scan Is Interpreted

- **Status:** **Proposed.** Nothing is implemented. The measurements below are
  real and reproducible; the design is not settled, and §6 names the one
  question a prototype has to answer before this earns implementation effort.
- **Depends on:** RFC-0094 (builtins as declarations — the direction this RFC
  argues WITH, not against; see §5), RFC-0021 (generators — the comptime
  sandbox, where the same loops run at compile time), RFC-0076 (generators as
  wasm — the engine that already exists, measured in §3 and found not to pay for
  a one-shot build), RFC-0029 (module state — the memoisation that closed the
  algorithmic half of this problem).
- **Evidence (user):** "why it takes so long", "but it shoudn't be SO SLOW",
  "`Site / build (pull_request) Successful in 10m` isn't it still slow?"

## 1. What happened before this RFC

The site's CI job was about thirty minutes. It is now ten, and none of that came
from making the language faster. It came from deleting waste:

| what was wrong | fix | measured |
| --- | --- | --- |
| every gate re-rendered the same 80 pages, stamps and all | cache the render in module state | export suite 7m12 to 1m57 |
| `programs()` opened `run-1.json` and rescanned it on every call, from inside every loop the benchmarks template draws | memoise the read and the parse | `/benchmarks` 22.3s to 1.13s |
| `rowOf` walked a layer, `layer` walked every module, `tallest` called `deepest()` in its loop CONDITION | compute the layout once | `/docs/graph` 6.7s to under 1s |
| four accessors each reopened every package manifest | one cached read | six `/explore` pages 12s to about 4s |

That is the whole of the cheap work. **There is no remaining algorithmic waste
of that kind in the site**, and this RFC exists because the next ten minutes are
not waste — they are real work running slowly.

## 2. Where the ten minutes sits

A Site job is 632s:

| step | time |
| --- | --- |
| The site's own tests | 392s (about 195s of it the cold generator phase) |
| Render every page | 77s (warm — it reuses the tests step's generator cache) |
| Build the CLI | 54s |
| Playground module and checks | 70s |

Locally, with a warm generator cache, a full export of 80 pages is 56.9s. Cold,
it is 181.1s: **118s of that is generators**, and about 110s of THAT is
`std/vyx` compiling 25 `.vyx` route files — about 4.4s per template.

## 3. Two fixes that do not work, measured before proposing this one

**Persisting `~/.vyrn/cache/gen` across CI runs.** Refused by the design, on
purpose. Generator entries are authenticated with a per-user secret kept outside
the cache directory, and `loader.rs` names "a cache directory restored from a CI
artifact" as one of the things it refuses. A generated module is compiler INPUT
with no trusted anchor — unlike the blob cache, which re-hashes each module
against a `vyrn.lock` sha that lives in the project and is reviewed like source.
Restoring the key alongside the cache would make a blob from a CI cache service
into trusted compiler input. That is a supply-chain surface, not a speedup.

*(Correction to a belief this RFC's author held: the generator cache DOES
invalidate when a generator's own source changes. The key deliberately omits the
sources — hashing them cost 37 ms of a 94 ms keystroke — but a hit re-hashes the
generator's whole recorded transitive closure before it is used. Verified:
editing a generator between two runs changes the output with no cache clear.)*

**Building the CLI with `--features wasm-gen` (RFC-0076).** The engine exists
and is tested. It does not pay here:

| cold generator phase | time |
| --- | --- |
| interpreted (what `site.yml` builds today) | 134.6s |
| `--features wasm-gen` | 108.0s |

Twenty per cent, and the saving is eaten by adding cranelift to a build step
that is currently 54s. The LSP sees 4x from the same engine because it compiles
each generator once and reuses it across keystrokes; a one-shot build pays the
compile and throws it away. **RFC-0076 was right for the LSP and is wrong for
CI**, and that is a property of one-shot versus interactive, not a defect.

## 4. What is actually slow

`std/strings`'s `indexOf` and `split`, and `std/strpred`'s `contains`, are
per-byte `while` loops written in Vyrn and executed one interpreter step at a
time:

    while i + nl <= s.byteLength {
        let mut j = 0
        while j < nl && s[i + j] == needle[j] {
            j = j + 1
        }
        if j == nl { return Some(i) }
        i = i + 1
    }

Measured directly: **about 1.5 MB/s** of interpreted scanning (20 full scans of
a 90,890-byte document, 1.4s). Rust's `memmem` does this at GB/s.

Everything the site does above these is string surgery: the `.vyx` compiler
parses templates, `std/html` serialises trees, the export's stamp pipeline
splices into rendered documents.

### How much does it actually cost?

Not a guess — a slope. One extra minimal pass over the same bytes was added to
`indexOf`, `split` and `contains`, and a warm export re-measured:

| | warm export |
| --- | --- |
| baseline | 56.9s |
| one extra interpreted pass per scanned byte | 64.4s |

**7.5s per added pass.** The real loops do roughly two to three times the work
per byte of that burn loop (outer bounds test, index arithmetic, inner compare
setup), which puts scanning at **15 to 19s of a 56.9s render: 26 to 33 per
cent**.

That is worth having and it is **not** the whole problem. This RFC does not
claim otherwise. The other two thirds are tree building, string concatenation
and `Map` lookups — also interpreted, and not addressed here.

## 5. The proposal, and the tension with RFC-0094

RFC-0094 M2 deliberately moved `contains`, `startsWith`, `endsWith` and `slice`
OUT of the reserved builtin list and into `std/strpred`. **This RFC does not ask
to move them back**, and a new reserved name would be the wrong shape.

What is proposed instead: these functions stay ordinary exported Vyrn functions
with the signatures they have now, and gain a NATIVE BODY — declared in Vyrn,
implemented in the compiler, the way `seeded_rows` already builds an
`ast::Function` in Rust. The names, the module, the import line and the
documentation do not move. A reader cannot tell, except by the clock.

The smallest version is one primitive: byte-substring search. `indexOf`,
`lastIndexOf`, `split`, `replace`, `contains`, `startsWith` and `endsWith` are
all expressible over it, and only `indexOf` needs the native body.

Three consumers benefit without any of them changing: the `.vyx` compiler, the
site's render, and `vyrn fmt`. So does every user program that touches text.

A native body needs three backends to stay honest under the parity gate:
interpreter, native lowering and wasm lowering. Anything less makes
`interp == native == wasm` a claim this function does not keep.

## 6. The open question a prototype must answer

**Does the `.vyx` compile phase benefit at all?** §4's 26 to 33 per cent is
measured on the RENDER phase. The generator phase is 110s of the 118s and it is
*lexing*, which walks bytes with indexing rather than searching for substrings.
If template compilation does not use substring search, this RFC fixes a third of
57s and none of 110s — and the honest conclusion would be that the ceiling here
is roughly one minute of a ten-minute job.

The prototype that answers it: a native `indexOf` in the INTERPRETER ONLY,
behind an environment variable, with no native or wasm lowering. That is enough
to measure both phases and is not enough to ship. Record the answer here before
any milestone is written.

**Second question, cheaper:** is `s[i]` byte indexing itself the significant
cost? If the generator phase is dominated by indexed reads rather than searches,
the primitive this RFC proposes is the wrong primitive, and the measurement
above will say so.

## 7. What this RFC is not

- Not a request for a new reserved word or a new builtin name (§5).
- Not a claim that the site build becomes fast. §4 bounds it at a third of one
  phase, and §6 admits the other phase may get nothing.
- Not a prerequisite for anything. Ten minutes is a working CI job; this is an
  optimisation with a measured ceiling, and it should be judged against that
  ceiling and not against the hope that started it.
