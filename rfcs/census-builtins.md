# Census — A Builtin Is a Declaration

- **Status:** measurement only. No engine code changed. This is the M0 of a
  proposed RFC; the milestone cut at the end is a recommendation, not a
  decision.
- **Measured at:** `e6ef37c`.
- **Depends on (read first):** RFC-0086 (the compiler asks the type; seeded
  rows; "no second list"), RFC-0078 (the runtime is Vyrn; the primitive census
  in `compiler/vyrn-frontend/tests/primitives.rs`), RFC-0090 (a declaration
  outperformed an intrinsic), RFC-0075 (streams), RFC-0091 (`at` became
  dispatch), RFC-0012 (`extern fn`), RFC-0092 M5 / PR #118.
- **Evidence:** every row is read from the file named in it, or counted over
  the corpus (32 `std/` modules, 180 `examples/*.vyrn`, 20 `.vyx`).

---

## Why a census

RFC-0086 named six hand-written lists that decide a property **of a type**, and
closed them. The same shape survives one level over: a set of hand-written lists
that decide a property **of a name**.

`RESERVED` (`compiler/vyrn-frontend/src/checker.rs:129`) is the largest. It holds
**83** names — not the ~100 the brief estimated. Beside it sit ten more lists of
the same names, each recording one fact the others do not.

PR #118 is the cost. `fromArray` and `fromStep` take ownership of what they are
handed. That fact lived in a doc comment; no rule read it; the native binary
freed the buffer twice and corrupted its heap. The fix was two rows in
`RESERVED_SINKS`. A parameter written `consume` would have carried the same fact
to every rule at once.

---

## The lists

Eleven, at `e6ef37c`. The count is the point: each is complete only while
somebody remembers to extend it.

| list | file:line | names | the fact it records |
|---|---|---|---|
| `checker::RESERVED` | checker.rs:129 | 83 | no declaration may take this name |
| `movecheck::RESERVED_VIEWS` | movecheck.rs:307 | 2 | the result points **into** an argument (`read`) |
| `movecheck::RESERVED_SINKS` | movecheck.rs:325 | 3 | this argument position is `consume` |
| the stream-producer match | movecheck.rs:3362 | 3 | the result carries a `MustUse` obligation |
| `checker::SPAWN_FORBIDDEN` | checker.rs:8330 | 18 | not callable from a spawned function |
| `checker::COMPTIME_FORBIDDEN` | checker.rs:8463 | 11 | not callable from a `gen fn` |
| `parser::METHOD_BUILTINS` | parser.rs:103 | 14 | the surface `.m(..)` spelling of an `@`-name |
| `loader::RT_MODULES` routes | loader.rs:564 | 12 (+4 desugared) | the body is a Vyrn function in `std/` |
| `types::numeric_conv_target` | types.rs:20 | 10 | this type name is also a conversion |
| `codegen::host_boundary_extern` | lib.rs:506 | 3 | this extern maps to a runtime symbol |
| the migration-hint `match` | checker.rs:5256 | 6 | this name was removed; say what replaced it |

Six more are LSP-facing and all live in `symbols.rs`, none in the `vyrn-lsp`
crate: `ALL_BUILTIN_METHODS` (3498, 25 rows — the only hover and signature
table there is), `MACRO_BUILTINS` (2927, 35 rows, colour only),
`CONSTRUCTOR_BUILTINS` (2967), `builtin_type_or_ctor` (1008), the ambient
completion list (1086), and `builtin_methods_of_shape` (3561).

Two more exist and are the **good** kind, because they are keyed by a type and
fed by declarations: `project::seeded_rows` (the `Index` rows behind `a[i]`) and
`tests/primitives.rs::CENSUS` (75 rows, checked against `interp.rs` by
`the_census_is_the_code`, which fails the suite when an arm has no row).

Counting every table that mentions a builtin name — including the test pins, the
per-backend result-type tables and the four dispatch chains — the total is about
**45**.

The dispatch chains themselves: `Checker::call` runs from checker.rs:5203 to its
fallthrough at 7097 — **1894 lines** of `if name == "…"`. `interp.rs`,
`codegen/lib.rs` and `direct.rs` each hold another; `direct.rs` holds two, an
emit chain (5418) and a type-prediction chain (4388).

### The fourth engine does not agree with the other three

Nothing asserts `direct.rs`'s builtin coverage against `RESERVED` the way
`the_census_is_the_code` asserts the interpreter's. Five names are live in the
checker, the interpreter and the textual backend, and **absent from the direct
wasm backend**: `alen`, `fsyncFile`, `assert`, `assertEq`, `blackBox`. Three of
those are explained (`assert`/`assertEq`/`blackBox` run under `vyrn test` and
`vyrn bench`, which are interpreter paths). Two are not: `fsyncFile` is an
ordinary I/O builtin, and `alen` — see the `delete` bucket below.

### One list already rotted, and it was noticed by accident

`RESERVED_VIEWS` held `get`. RFC-0090 M4 deleted the `get` builtin and took the
name out of `RESERVED` in the same stroke — but `RESERVED_VIEWS` matches on the
**call**, not on a builtin table. So every user function called `get` handed back
a value that owned nothing, and a `Slots<String>` read through `std/slots` leaked
silently. The doc comment at movecheck.rs:296 records it.

`COMPTIME_FORBIDDEN` has the same exposure today: `SPAWN_FORBIDDEN` is pinned by
`spawn_forbidden_names_are_reserved` (checker.rs:10860) and `COMPTIME_FORBIDDEN`
is pinned by nothing.

---

## The buckets

| bucket | count | meaning |
|---|---|---|
| **module-extern** | 31 | belongs in a `std/` module as a plain declaration, imported |
| **prelude-extern** | 14 | a signature with capabilities; body stays intrinsic; seeded |
| **syntax** | 18 | not a function — a constructor, a type name, or a keyword |
| **teaching-hint** | 8 | removed; the row exists so the hint fires |
| **derive-intrinsic** | 6 | per-type synthesis; stays intrinsic, should carry a signature |
| **seeded-protocol** | 4 | type-directed lowering that should be protocol dispatch |
| **delete** | 2 | a second spelling of something the language already has |

83 names, each in exactly one bucket.

### The table

Columns: **C** = checker arm, **I** = `interp.rs`, **T** = textual backend
(`codegen/lib.rs`). The direct wasm backend (`codegen/direct.rs`) carries every
name in this table except the five named above.
"routed" = no engine arm at all; the call becomes a call to a `std/` function.
**own** = where the ownership or capability contract lives today.
**LSP** = 23 of the 83 have hover text; the column is not repeated per row, the
list is under Q-extra below.

| name | bucket | C | I | T | own | why |
|---|---|---|---|---|---|---|
| `print` | seeded-protocol | 5395 | 4190 | 8246 | `SPAWN_FORBIDDEN` | its parameter is a union of every scalar — that union is a `Show` protocol |
| `logger` | module-extern | 5423 | 4487 | 8294 | `SPAWN_FORBIDDEN` | `logger(String) -> Logger`, monomorphic; 5 corpus files |
| `trace` `debug` `info` `warn` `error` | module-extern | 5443 | 4491 | 8300 | both forbid lists | `(Logger, String) -> Unit`; one arm serves five names |
| `args` | module-extern | 5477 | 4606 | 8375 | both forbid lists | `() -> Array<String>`; `std/args` already exists (RFC-0061) |
| `readLine` | module-extern | 5486 | 4613 | 8380 | both forbid lists | `() -> Option<String>` |
| `readFile` | module-extern | 5495 | 4647 | 8498 | `SPAWN_FORBIDDEN` | `(String) -> Result<String, String>` |
| `writeFile` | module-extern | 5739 | 4741 | 8578 | both forbid lists | `(String, String) -> Result<Bool, String>` |
| `renameFile` | module-extern | 5762 | 4769 | 8625 | both forbid lists | monomorphic |
| `fsyncFile` | module-extern | 5784 | 4798 | 8671 | both forbid lists | monomorphic |
| `readFileBytes` | module-extern | 5803 | 4824 | 8709 | both forbid lists | monomorphic |
| `listDir` | module-extern | 5583 | 4694 | 8471 refuses | `SPAWN_FORBIDDEN` | monomorphic |
| `lineAt` `colAt` | module-extern | 5530 | 4935 | 8453 | `SPAWN_FORBIDDEN` | `(Array<UInt8>, Int64) -> Int64`; a stated cache, not a capability |
| `contains` `startsWith` `endsWith` | module-extern | 5855 | routed | routed | nowhere | **the body is already `std/strpred`** |
| `slice` | module-extern | 5883 | routed | routed | nowhere | routed, and its **return type is already read out of the declaration** (checker.rs:5913) |
| `chars` | module-extern | 6005 | routed | routed | nowhere | body is `text$charsV` |
| `hexEncode` `hexDecode` `base64Encode` `base64Decode` `urlEncode` `urlDecode` | module-extern | 5969 / 5985 | routed | routed | nowhere | body is already `std/codecs` |
| `boxStream` | module-extern | 6378 | 5082 | 9468 | nowhere | `(consume Stream<T>) -> Int64`; 1 corpus file |
| `unboxStream` | module-extern | 6397 | 5091 | 9486 | movecheck.rs:3362 | produces a `MustUse`; return type comes from `expected` |
| `pullAt` | module-extern | 6397 | 5100 | 9512 | nowhere | return type comes from `expected` |
| `serveStream` | module-extern | 6467 | 5127 | 8239 traps | nowhere | `(consume Stream<String>) -> Unit`; 1 corpus file |
| `bytes` | prelude-extern | 6005 | 4587 | 8355 | `RESERVED_VIEWS` | `(read String) -> Array<UInt8>`; the view all four runtime modules stand on |
| `stringFromBytes` | prelude-extern | 5827 | 4854 | 8791 | `SPAWN_FORBIDDEN` | the only `Array<UInt8> -> String` there is |
| `floatBits` `floatFromBits` | prelude-extern | 5933 | 4896 | 8333 | nowhere | one instruction each; monomorphic |
| `parse` | prelude-extern | 6114 | 4924 | 8986 | nowhere | `(String) -> Option<Int64>`; wraps on overflow where `std/num` refuses |
| `panic` | prelude-extern | 5376 | 4180 | 8219 | nowhere | `(String) -> Never` |
| `assert` | prelude-extern | 5293 | 3842 | absent | test-gate in the arm | `(Bool) -> Unit` + a context capability no signature carries |
| `assertEq` | prelude-extern | 5316 | 3858 | absent | test-gate in the arm | needs `<T: Eq>`; the language has no such protocol |
| `blackBox` | prelude-extern | 5351 | 3855 | 8188 | bench-gate in the arm | `<T>(T) -> T` |
| `pop` | prelude-extern | 6494 (`@pop`) | 3960 | 9559 | `mut_array_receiver` | `(modify Array<T>) -> Option<T>`; RESERVED so `METHOD_BUILTINS` can never give the name back |
| `swapRemove` | prelude-extern | 6508 (`@swapRemove`) | 3986 | 9680 | `mut_array_receiver` | `(modify Array<T>, Int64) -> T`; as `pop` |
| `fromArray` | prelude-extern | 6285 | 5059 | 9417 | `RESERVED_SINKS` **+** movecheck.rs:3362 | `(consume Array<T>) -> Stream<T>` — **PR #118's row** |
| `fromStep` | prelude-extern | 6322 | 5063 | 9436 | `RESERVED_SINKS` **+** movecheck.rs:3362 | `(Int64, Int64, consume fn(..) -> Option<T>) -> Stream<T>` — PR #118's other row |
| `close` | prelude-extern | 6434 | 5115 | 9402 | `SPAWN_FORBIDDEN` | `(consume Stream<T>) -> Unit` |
| `at` | seeded-protocol | 6203 | 5004 | 9188 | `RESERVED_VIEWS` | **already dispatch** (RFC-0091 M2); the residue is the seeded `Array`/`Map`/`String` rows |
| `push` | seeded-protocol | 6158 | 4994 | 9100 | `RESERVED_SINKS` | polymorphic over `Array` and `SmallArray`, rebuilding the same kind |
| `value` | seeded-protocol | 6833 | 5140 | 9940 | nowhere | boxes a scalar by its static type — RFC-0007 §v2 asks for exactly this protocol |
| `toJson` | derive-intrinsic | 6773 | 3926 | 7858 | nowhere | the walk needs the argument's static type; the writer is `std/json` |
| `fromJson` | derive-intrinsic | 6798 | 3936 | 7889 | nowhere | first argument is a **type name**, not a value |
| `schemaOf` | derive-intrinsic | 6706 | 3881 | 7831 | nowhere | argument is a type name |
| `jsonSchema` | derive-intrinsic | 6752 | 3906 | 7843 | nowhere | argument is a type name |
| `contractOf` | derive-intrinsic | 6730 | 3897 | 8491 refuses | nowhere | argument is a contract name |
| `moduleInterface` | derive-intrinsic | 5607 | 4729 | 8481 refuses | nowhere | reads the module graph |
| `Some` `None` `Ok` `Err` | syntax | 6876 / 3506 / 6913 | 5183–5185, 3667 | 10005–10042 | n/a | constructors of the compiler's own `Option`/`Result` |
| `match` | syntax | none | none | none | n/a | a keyword; the row exists so `fn match` is impossible |
| `Int64` `Int32` `Int16` `Int8` `UInt8` `UInt16` `UInt32` `UInt64` `Float64` `Float32` | syntax | 6669 | 4147 | 7906 | n/a | type names; the call form is `numeric_conv_target`, part of the type system |
| `F32x4` `I32x4` `F64x2` | syntax | 4892 | 4217–4248 | 7916 | n/a | type names; the constructor half **is** signature-expressible, but the name must stay reserved as a type either way |
| `len` `concat` `str` `join` `list` `toString` | teaching-hint | 5257–5287 | none | none | n/a | removed; the arm quotes the replacement |
| `Int` `Float` | teaching-hint | none | none | none | n/a | the hint is at the **type** position (parser.rs:3091, 3101) |
| `array` | delete | 6133 | 4993 | 9094 | nowhere | `[]` is the same thing, and `[]` is a declaration since RFC-0086 (`FromElements`). 7 uses in 2 files, both of which exist to test contextual typing |
| `alen` | delete | 6263 | 5049 | 9379 | nowhere | **`.length` replaced it**: 677 uses in 75 files against **0** for `alen(`. No desugar produces it, and the wasm backend never implemented it |

### The `@`-internal names

The lexer rejects `@`, so no source can spell these. They are not in `RESERVED`
and do not need to be — unspellable is stronger than reserved.

| name | desugars from | in RESERVED? | note |
|---|---|---|---|
| `@str` | interpolation, `x.toString()` | no (`str`, `toString` are) | seeded-protocol: its parameter is `print`'s union |
| `@concat` | `a + b` on `String` | no (`concat` is) | prelude-extern; `Measured` in RFC-0078's census |
| `@join` | `t.join()` | no (`join` is) | prelude-extern; `(Task<T>) -> T` |
| `@list` | every tagged template | no (`list` is) | **live** — see Q6 |
| `@slot` | the identity case of `place at` | no | the addressing floor; RFC-0091 leaves it closed on purpose |
| `@pop` `@swapRemove` `@toArray` `@copy` | array/`SmallArray` methods | no | see `pop`/`swapRemove` above |
| `@has` `@remove` `@keys` | `Map` methods | no | seeded-protocol with `at` |
| `@charCount` | `s.charCount()` | no | **routed** to `text$charCountV` |
| `@lane` `@replaceLane` `@anyTrue` `@allTrue` | vector methods | no | prelude-extern; monomorphic per width |
| `@f32x4*` `@i32x4*` `@f64x2*` (splat/load/store/min/max/sqrt/ceil/floor/trunc/nearest) | `F32x4.m(..)` etc. | no | prelude-extern; every one is monomorphic |
| `@panicAt` | the loader stamps `file:line` on every `panic` | no | prelude-extern |
| `@codeText` `@codeSplice` | `vyrn"…"` | no | gen-only; absent from both backends |

Not call names, listed so their `@` is not mistaken for a builtin: `@lambda`,
`@rel`, `@lazy`, `@try`, `@v`, `@e`, `@t`, `@i`, `@b….j` — synthesized binding
and temporary spellings in the parser, `project.rs` and the two backends.

`render`, `rawAt`, `raw` and `lex` (RFC-0054) are builtins that are **not**
reserved: checker.rs:5634 lets any user declaration or binding of that name win.
That is the module-extern arrangement, already shipped, for four names.

---

## The six questions

### Q1 — how many names carry a fact that lives only in prose or a side table?

**Eleven** carry an ownership or capability fact that no signature holds. Each is
a latent PR #118.

| name | the fact | where it lives | what would carry it |
|---|---|---|---|
| `fromArray` | argument 0 is consumed | `RESERVED_SINKS` (added **by** PR #118) | `consume` |
| `fromStep` | argument 2 is consumed | `RESERVED_SINKS` (added by PR #118) | `consume` |
| `push` | argument 1 is consumed | `RESERVED_SINKS` | `consume` |
| `at` | the result points into argument 0 | `RESERVED_VIEWS` | `read` on the receiver |
| `bytes` | the result points into argument 0 | `RESERVED_VIEWS` | `read` on the parameter |
| `close` | its argument is consumed | the interpreter and both backends, separately | `consume` |
| `boxStream` | its argument is consumed | nowhere at all | `consume` |
| `serveStream` | its argument is consumed | nowhere at all | `consume` |
| `unboxStream` | its result is `MustUse` | movecheck.rs:3362, by name | the return type |
| `pop` `swapRemove` | the receiver is written back through | `mut_array_receiver` (checker.rs:8148) | `modify` |

`boxStream` and `serveStream` are the two with **no** row anywhere. Both hand a
`Stream` away for good and neither is in `RESERVED_SINKS`. Each has exactly one
corpus caller today, which is why nothing has corrupted a heap over it yet.

A twelfth class, counted separately because a signature would **not** fix it: the
29 rows of `SPAWN_FORBIDDEN` and `COMPTIME_FORBIDDEN`. Those are effects, and
Vyrn has no effect annotation on a signature at all.

### Q2 — what would the embedded prelude need syntactically?

**Nothing, if it is seeded as AST rather than parsed** — and the repo already
does exactly that. `project::seeded_rows()` (project.rs:101) builds two
`ast::Function` values in Rust, with `Capability::Read` and `Capability::Modify`
on `self`, and comments that it is built as AST "because `@slot` is deliberately
unlexable". A prelude of body-less `Function`s with capabilities and type
parameters costs no grammar and no `include_str!`.

If the prelude were instead written as Vyrn source and parsed, `extern fn` cannot
carry it. The gaps, exactly:

1. **No generics.** `extern_function` (parser.rs:2866) never parses `<T>` and
   clears `self.type_params` (2871, 2910). `fromArray<T>` does not parse.
2. **The ABI type domain.** `check_extern_sig` (checker.rs:2600) allows only
   `Int64`, sized ints, `Float64`, `Float32`, `Bool`, `String`, `Unit`.
   `Array<T>`, `Stream<T>`, `Map`, `Option`, `Result` and records are all
   refused.
3. **`consume String` is refused outright** (checker.rs:2616), with a fix menu —
   correctly, because on the other side of an `extern` the caller is JS.
4. **No return-only type parameter.** Vyrn solves type parameters from
   arguments. `unboxStream(a: Int64) -> Stream<T>`, `pullAt`, `array()`, `None`,
   `Ok`, `Err` all take their result type from `expected`.
5. **No union or overloaded parameter.** `print`, `@str`, `value`, `assertEq`
   and the ten numeric conversions all accept "any scalar".
6. **No type-name argument.** `schemaOf(User)`, `jsonSchema(User)`,
   `fromJson(User, s)` and `contractOf(C)` pass a declaration, not a value.

Capabilities themselves are **not** a gap: `parse_capability` (parser.rs:2539)
already runs on every `extern fn` parameter, and `consume Array<T>` would parse
if 1 and 2 were lifted.

So: seeding AST costs nothing and expresses gaps 1–3. Gaps 4, 5 and 6 bound the
prelude however it is spelled — see Q3.

### Q3 — which names can a generic signature not express?

**Fourteen**, in three groups.

- **Result type from context** (6): `array`, `None`, `Ok`, `Err`, `unboxStream`,
  `pullAt`. A declared `-> Stream<T>` is writable; solving `T` from `expected`
  rather than from an argument is not something the checker does.
- **A union parameter** (4 + 10): `print`, `@str`, `value`, `assertEq`, and the
  ten numeric conversions. Each accepts every scalar and returns something
  different per scalar. `print`, `@str` and `value` are the honest
  seeded-protocol candidates; `assertEq` needs an `Eq` protocol that does not
  exist; the ten conversions are type syntax and should stay there.
- **A type name as an argument** (4): `schemaOf`, `jsonSchema`, `fromJson`,
  `contractOf`. Not a value at all. These are the derive-intrinsic bucket and no
  signature reaches them.

`at` looks like a fourth group and is not. It reads four different receivers
(`Array`/`ArrayN`/`SmallArray`, `Map` → `Option<V>`, `String` → `UInt8`, and a
user container) with a different result for each — but RFC-0091 M2 already made
it protocol dispatch. What is left is three seeded rows, not a signature problem.

### Q4 — which names does the checker consult for a **refusal**?

Almost all of them, but the distinction that matters is what a signature would
give for free.

`Checker::call` refuses on arity and parameter type for **every** builtin with an
arm. A declaration carries both. Those refusals are not the constraint.

What a signature would **not** give, and therefore must be seeded rather than
imported, is:

| kind | names |
|---|---|
| a **context** gate | `assert`, `assertEq` (test-only), `blackBox` (bench/test-only), `render`/`rawAt`/`raw`/`lex`/`@codeText`/`@codeSplice` (gen-only) |
| an **effect** gate | the 18 `SPAWN_FORBIDDEN` + 11 `COMPTIME_FORBIDDEN` names |
| an **ownership** rule | the eleven names of Q1 |
| a **region-escape** rule | `push` (checker.rs:6194) |
| a **literal type name** | `schemaOf`, `jsonSchema`, `fromJson`, `contractOf` |
| **codability** | `toJson`, `fromJson` |
| **inference from context** | `array`, `None`, `Ok`, `Err`, `unboxStream`, `pullAt` |
| a **link** refusal | `slice` — "its module is not in the link" (checker.rs:5916) |

The rule the repo already states, in three places (types.rs:126, 150, 175, and
own.rs:135): *a decision the compiler refuses programs over may not depend on a
module lookup.* `slice` is the one name that violates it today. It is a
**routed** builtin, so `vyrn run` on a bare file with no std root refuses
`slice(s, 0, 3)` with a link error rather than running it. That is the honest
precedent for the whole module-extern bucket: routing already trades the bare
file away, and the repo already accepted the trade once.

### Q5 — cost of import-scoping the module-extern bucket

Distinct corpus files that would gain **one** import line, per group:

| group | names | files |
|---|---|---|
| stream primitives | `boxStream` `unboxStream` `pullAt` `serveStream` | **2** (`std/stream.vyrn`, `std/http.vyrn`) |
| `std/codecs` | the six codecs | **3** |
| source position | `lineAt` `colAt` | **3** |
| `std/text` | `chars`, `.charCount()` | **6** |
| `std/strpred` | `contains` `startsWith` `endsWith` `slice` | **7** |
| logging | `logger` + the five levels | **5** |
| file and process I/O | `readFile` `writeFile` `renameFile` `fsyncFile` `readFileBytes` `listDir` `readLine` `args` | **14** |

Total distinct-file-to-import pairs across the whole bucket: **40**, over a
corpus of 232 files. The four stream primitives — the ones PR #118 was about —
cost **two lines**.

For contrast, the names that must **not** move: `print` appears in **140** files,
`bytes`/`stringFromBytes` in 30, `panic` in 9, `parse` in 9.

### Q6 — what does `value` do, and is `list` half-dead?

**`value`** is the tagged-template box (RFC-0007). The parser emits
`value(<hole>)` for every interpolation hole of a tagged template
(parser.rs:4640); the checker types it and refuses anything but `Int64`, `Bool`
or `String` (checker.rs:6833: "`value` boxes an Int64, Bool, or String"). It is
what makes the safety argument work: a hole can only ever become a bound
parameter, never query structure. The corpus writes it by hand **once**.

Two facts about it are worth recording.

1. RFC-0007 §v2 defers "the extensible value set — letting user types be
   interpolable". That deferral **is** the seeded-protocol bucket. `value` is the
   clearest case in the census of a builtin whose own RFC already asked for it to
   become a protocol.
2. The word `value` has a **second, unrelated meaning** in this language: it is
   the subject identifier inside a `where` refinement predicate
   (`String where value.byteLength >= 3`) — checker.rs:1910, types.rs:1006,
   loader.rs:2416. One word, two meanings, in one reserved name.

**`list` is half-dead, and precisely half.** The surface spelling was removed and
its `RESERVED` row now serves only the hint (checker.rs:5272: "`list([..])` was
removed"). The internal `@list` is fully live: the parser wraps **every** tagged
template's `parts` and `values` arrays in it (parser.rs:4653), and it is typed at
checker.rs:6856, run at interp.rs:5152, and emitted at codegen/lib.rs:9963 and
direct.rs:6253. So the *name* is a teaching-hint and the *builtin* is a live
prelude-extern under an unspellable spelling. Nothing is dead; the halves are
just filed apart.

### Q-extra — what the editor knows

The `vyrn-lsp` crate holds **no** builtin-name knowledge at all. Everything it
serves comes from `symbols.rs`.

- **23 of 83** `RESERVED` names have hover text: the 19 in `ALL_BUILTIN_METHODS`
  (`push`, `at`, `alen`, `fromArray`, `fromStep`, `boxStream`, `unboxStream`,
  `pullAt`, `close`, `serveStream`, `pop`, `swapRemove`, `toString`, `join`,
  `trace`, `debug`, `info`, `warn`, `error`) plus `Some`/`None`/`Ok`/`Err`.
- **35 more** are coloured by `MACRO_BUILTINS` and nothing else — no hover, no
  completion. That set includes `print`, `toJson`, `fromJson`, `schemaOf`,
  `panic`, `assert`, `slice` and every codec.
- **25 are invisible**: no hover, no completion, no colour. `match`, `logger`,
  `contains`, `startsWith`, `endsWith`, `lineAt`, `colAt`, `value`, `list`,
  `blackBox`, and all fourteen type names (`Int`, `Int64`…`UInt64`, `Float`,
  `Float64`, `Float32`, `F32x4`, `I32x4`, `F64x2`).
- Top-level completion offers **6** builtins in total (symbols.rs:1086).

A declaration is what the LSP already knows how to serve. Every name in the
prelude-extern and module-extern buckets would gain hover, signature help,
go-to-def and completion with no LSP change at all — which is 45 of the 83, and
40 of the 60 that have no hover today.

---

## What this buys, counted

| bucket | compiler lines it would delete | bug class it closes |
|---|---|---|
| module-extern (31) | the checker arms (roughly 260 lines for the 12 already-routed names alone), 4 `RESERVED` groups, and the `RT_MODULES` route table itself once the names are ordinary imports | name capture: a user `fn slice` is refused today for no reason a reader can see |
| prelude-extern (14) | `RESERVED_SINKS`, `RESERVED_VIEWS`, the movecheck.rs:3362 producer match, and the per-builtin arity/type checks in **four** engines | the PR #118 class outright — an ownership contract that no rule reads |
| seeded-protocol (4) | the `Array`/`Map`/`String` special cases in `at` and `push`; `print`'s and `@str`'s scalar unions | RFC-0007 §v2's deferral; a user type that cannot be printed or interpolated |
| derive-intrinsic (6) | nothing | nothing — but a declared signature gives them hover, completion and a diagnostic that reads like every other call |
| delete (2) | 2 checker arms, 2 interpreter arms, 2 textual-backend arms, 4 `RESERVED` rows counting the hints | the `alen` shape: a name three engines carry and the fourth does not, that no program writes |
| teaching-hint (8), syntax (18) | nothing | nothing |

---

## Recommended milestone cut

**M1 — the ownership facts become capabilities (prelude-extern, 14 names).**
Seed a prelude of body-less `ast::Function` signatures the way
`project::seeded_rows` already seeds its two, with `consume`/`read`/`modify` on
the parameters. Delete `RESERVED_SINKS`, `RESERVED_VIEWS` and the movecheck
producer match; make `movecheck` read the signature. This is where PR #118 lives,
it needs **no grammar change**, it touches no corpus file, and it closes the two
names (`boxStream`, `serveStream`) that have no row anywhere today.

**M2 — the routed names become imports (module-extern, first 12 names).**
`contains`, `startsWith`, `endsWith`, `slice`, `chars`, `@charCount` and the six
codecs already have their bodies in `std/`. Their only compiler residue is a
checker arm and a `RESERVED` row. Cost: 16 import lines across 16 files. Take the
stream primitives (2 files) in the same milestone; leave I/O and logging for M3,
because 14 files and an effect gate are a different argument.

**M3 — `value`, `print` and `@str` become a `Show`-shaped protocol
(seeded-protocol).** Closes RFC-0007 §v2 by name. Do `value` first: it has one
hand-written call site in the corpus and the smallest blast radius of the three.

**Free, and not a milestone:** delete `alen` (0 corpus uses, no desugar produces
it, one engine never implemented it) and give `array()` the hint `list` already
has. Add the missing `COMPTIME_FORBIDDEN ⊆ RESERVED` test. Three small commits
that need no design.

### What should not be attempted

- **Do not put `at` or `push` on the prelude.** `at` is already protocol
  dispatch; what remains is three seeded container rows, which is the answer, not
  the problem. RFC-0091 M2 already recorded why `@slot` must stay closed.
- **Do not try to declare the derive-intrinsics** (`toJson`, `fromJson`,
  `schemaOf`, `jsonSchema`, `contractOf`, `moduleInterface`). Four of the six
  take a type name rather than a value. A signature cannot say that, and inventing
  a syntax for it would buy six hover strings.
- **Do not move `print`, `bytes`, `stringFromBytes`, `panic` or `parse` into an
  importable module.** `print` alone is 140 corpus files, and the bare-file rule
  applies to all five.
- **Do not build an effect annotation for `SPAWN_FORBIDDEN` /
  `COMPTIME_FORBIDDEN`.** 29 rows across two lists is a real smell, but effects
  on signatures are a language feature, not a milestone of this RFC. The cheap
  fix is one test — `COMPTIME_FORBIDDEN` has no subset-of-`RESERVED` pin where
  `SPAWN_FORBIDDEN` has one, and that asymmetry is exactly how `get` rotted.
- **Do not relax `extern fn`'s ABI domain** to carry the prelude. It is a JS
  boundary and its restrictions are correct for that job. Seed AST instead.

## Is the thesis right?

Partly, and the honest split is 45 to 38.

**Forty-five of 83 names** (14 prelude-extern + 31 module-extern) are a signature
the compiler refuses to let anybody write down. For those the thesis holds
exactly, and eleven of them carry an ownership fact that a `consume` or a `read`
would have carried to every rule at once — which is the bug PR #118 shipped.

**Thirty-eight are not.** 18 are syntax, 8 are teaching hints, 6 need a type name
rather than a value, 4 want a protocol rather than a signature, and 2 should be
deleted. For those a prelude buys hover text.

So the RFC is worth writing, and it is **half the size the brief assumed**. The
M1 above is where the whole of the measured value sits: eleven latent PR #118s,
no grammar change, no corpus change.
