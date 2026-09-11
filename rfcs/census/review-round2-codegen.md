# Review round two, the code generator and the standard library: 22 findings

Round two of the audit produced 95 findings. These 22 are the ones in
`compiler/vyrn-codegen/`, in `std/` and in the runtime. Each row carries the
commit that closed it, or the rule the finding misread.

Twenty of the 22 were already closed by commit `907354be`, which the audit
itself wrote. One (`F2-085`) is not a defect. One (`F2-001`) was the single
finding round two deferred; it is live and this round fixes it.

## Method

Every row was checked against `main` today, by what the code does, not by the
line the finding names. Four findings name code the text-IR route took with it
when it left in `82671105`; each was fixed first, in `907354be`, and the row
names both. Two rows carry a witness program built and run on this machine.

Counts: 20 fixed, 1 not a defect, 1 live and fixed here.

| id | severity | verdict | evidence | commit |
|---|---|---|---|---|
| F2-001 | medium | live, fixed | `envGet` read the whole environment on every call and held the blob the answer points into. `injected` reaches it from `nowMillis`, `monoNanos` and `randomSeedV`, so the comment's claim ("the two callers run once per process") was false. Witness: 400 `monotonic()` calls with a 100 KB environment exhaust a 32 MB `wasmtime` memory; one call does not. Fixed by reading once into the two dead class heads. The census row `clockRead` in `tests/memory.rs` reads 9,109,504 bytes after 500 calls and 11,534,336 after 2,000 before the fix, 8,323,072 at both after it | `1d9f0379` |
| F2-002 | high | fixed | `stream_from_step` evaluates `args[0]`, `args[1]` and then the step, in written order, and says why | `907354be` |
| F2-003 | medium | fixed | `spawn` copies arguments out of a dying region; the keyword then left the language entirely in `9495000f`, and `gen_spawn` with the text-IR route in `82671105` | `907354be` |
| F2-004 | medium | fixed | a lifted lambda sees its own expected type; `emit_lifted_lambda` left with the text-IR route in `82671105` | `907354be` |
| F2-005 | medium | fixed | LLVM symbols escaped non-ASCII; the route that formed them left in `82671105` and a wasm export name is UTF-8. Witness: `fn héllo(n: Int64) -> Int64` builds to wasm and prints 42 | `907354be` |
| F2-006 | medium | fixed | a `SmallArray` element store releases what it displaces; `direct.rs` now has one `walk` for every array kind and the release is the core's `store_row`, not a per-type arm. Witness: eight `sa[0] = tag() + "x"` stores under `VYRN_LEAK_CHECK=1` report no residue | `907354be` |
| F2-007 | medium | fixed | the `tools/` walk sorts its hits; `discovered_wasmtime_from` goes through the one `tools_walk` that every discovered tool uses | `907354be` |
| F2-008 | medium | fixed | `finish()` refuses a duplicate export by name, `wasm.rs:695` | `907354be` |
| F2-070 | medium | fixed | the strict JSON reader hashes its duplicate-key scan, `jsonread.vyrn:418` | `907354be` |
| F2-079 | medium | confirmed fixed | the element's own attributes are read with `inFor \|\| vhHas(attrs, "v-for")`, and `vyx-hints.vyrn:2160` asserts the `<li v-for id="row">` case | `907354be` |
| F2-080 | medium | fixed | `twAddLeaf` refuses a scalar under a top-level family by name | `907354be` |
| F2-081 | low | fixed | `vhScan` reports an unreadable directory and an unreadable component, and counts neither | `907354be` |
| F2-082 | high | fixed | the accumulator is `__acc` and `scanArgs` refuses a message whose argument is spelled that way | `907354be` |
| F2-083 | medium | fixed | a known procedure with a non-POST method answers 405 `method not allowed` | `907354be` |
| F2-084 | medium | fixed | a Unit procedure gets a `gqlRun_` runner that calls it and resolves `true`, so the SDL's `Boolean` is answered | `907354be` |
| F2-085 | medium | not a defect | ICU has two selector forms. An `explicitValue` is compared against the input number and a keyword resolves through the plural rules; only the second has operands, and only operands are absolute. Folding `=1` onto \|count\| makes `{n, plural, =1 {one file} other {# files}}` say "one file" for -1. The rule is now stated at `compilePlural` | `907354be` |
| F2-086 | medium | fixed | `uiSegNeedsDecode` decodes a static segment that the wire can only carry percent-encoded, and `ui.vyrn:3074` routes `my page` | `907354be` |
| F2-087 | medium | fixed | a text run at the template root counts toward the one-root rule, so a stray word is the loud second-root diagnostic | `907354be` |
| F2-088 | medium | fixed | `vyxFinish` checks the read diagnostics before the empty-directory guard, and says so | `907354be` |
| F2-089 | medium | fixed | the root-slot splice iterates `consume children`, and `vyx.vyrn:4942` asserts it | `907354be` |
| F2-090 | medium | fixed | a `Param` form on a route with no dynamic segment is refused by name, `ui.vyrn:1139` and `ui.vyrn:1450` | `907354be` |
| F2-091 | medium | fixed | the `//@origin` anchor takes the page's real extension | `907354be` |

## The one that was live

`envGet` is the runtime's `env_get`. It asks WASI for the environment in one
go: a pointer array and a blob, and the answer it hands back points into the
blob. The blob was therefore never freed, which is correct, but it was
allocated on every call, which is not. `auditForget` kept it out of the residue
ledger, so the ratchet could not see it and the record said the block was held
for the life of the process.

Three functions reach it on every call through `injected`: `nowMillis`,
`monoNanos` and `randomSeedV`. A program that polls the clock or reseeds in a
loop grew the heap by a blob a turn until `malloc` trapped.

The read now happens once. The pointer array and the count live in the two
dead class heads at `heapBase() + 4` and `+ 8` — `malloc` floors a request at
eight bytes, so classes 1 and 2 cannot exist and the instrument's own note
already says the words are free. Nothing else moved: the wasm manifest moved
two of 174 rows, `clock.vyrn` and `storage.vyrn`, the only two examples that
reach `envGet`, and the `wasm2wat` diff for `clock.vyrn` is confined to that
one function body.
