# The standard library

`std/` is Vyrn source. A program imports a module as `"std/<name>"`, and the compiler links it like any other module. `vyrn doc --std` writes the per-function reference to [docs/api](api/index.md); this page says what each module is for and how the runtime underneath fits together. The language itself is in [language.md](language.md).

## The runtime is Vyrn

The compiler emits WebAssembly, and everything that module needs at run time is Vyrn code in two std modules.

- `std/mem` holds the raw primitives: `load8` to `load64`, `store8` to `store64`, the float loads and stores, `copy`, `fill`, `memorySize`, `grow`, `heapBase`, `trap`, and the host imports. Each function is a declaration. The emitter lowers each call to one wasm instruction or one import call and never reads the body. An address is an `Int32`, because the target is wasm32.
- `std/runtime` stands on `std/mem` and holds the allocator, the arena, strings, arrays, the map, integer formatting, UTF-8 checks, the regular-expression runner, and every I/O builtin. The compiler links it into every program.
- Only `std/runtime` may import `std/mem`, and no user module may import either. The loader refuses the import.

A builtin routes to a runtime function with the same name plus `V`: `readFile` calls `readFileV`, because the builtin's own name is reserved. Other builtins route to ordinary std modules. `x.charCount()`, `lineAt` and `colAt` run `std/text`. A float rendered by `print` or `toString()` runs `std/num`'s `f64Str`. `toJson` links `std/json`, and `fromJson` links `std/jsondec`. So one Vyrn source serves every engine, and the engines agree byte for byte.

Inside the runtime a `String` is an `Int32` address of NUL-terminated bytes with a length header. No module outside the runtime holds an address. A `panic` in the runtime prints its trap line with no source site, and its frames do not count against the call-depth limit.

### Memory

The allocator is a segregated free list with four size classes per power of two, from 8 bytes to 2 GiB. A request rounds up to a multiple of eight, and a class wastes at most 25 percent. Every block has an eight-byte header whose first word is the class. `malloc` traps `out of memory` when a request cannot fit in wasm32 memory or `grow` refuses.

- `free` ignores a pointer below `heapBase()`: a string literal in the data segment and an inline `SmallArray` both live there. It also ignores a block of class 0.
- A `region` block allocates from an arena. Arena blocks carry class 0, and `regionExit` frees the whole arena at the closing brace.
- An audited build counts live blocks at exit; the test suites use it to prove that every block is released exactly once.

When the compiler releases each value is the subject of [memory.md](memory.md).

### Maps

A `Map` is an insertion-ordered entry array with a power-of-two open-addressing index. An `Int64` key hashes with the SplitMix64 finalizer and compares by value. A `String` or packed record key hashes with FNV-1a and compares by bytes. The runtime never calls a user `Hashable` impl.

### The host boundary

A program crosses to its host in three places.

| Boundary | Where it is used |
|---|---|
| `wasi_snapshot_preview1` imports (`fd_write`, `fd_read`, `path_open`, `clock_time_get`, `random_get`, `args_get` and the others) | All I/O. `std/mem` declares each with its WASI signature; the emitter drops the imports a program does not reach. |
| The `vyrn` import namespace | `extern fn` declarations. `export extern fn` functions go the other way, as module exports. |
| The `vyrn_gen` imports (`read`, `fetch`) | A `gen fn` running in the compiler reads files through the loader instead of WASI. An ordinary build lowers these to `unreachable` and removes their callers. |

`vyrn run` executes the module under an embedded wasmtime. `vyrn build` translates the same module to C with `wasm2c` and compiles it with `clang`, so a native binary is the same program. In a browser, `web/wasi-min.js` supplies the WASI imports and `web/vyrn-dom.js` applies view patches.

## The prelude

The compiler injects these declarations into every program, so a file names them without an import:

| Declaration | Use |
|---|---|
| `Option<T>`, `Result<T, E>` | Absence and failure. |
| `Issue = { key, path, message }`, `Validation<T> = Valid(T) \| Invalid(Array<Issue>)` | Every problem at once, each with a translation key and a field path. |
| `LoadResult<T> = Missing \| Corrupt(Array<Issue>) \| Loaded(T)` | The result of `load`. |
| `Value`, `Template` | Tagged-template holes and `template"..."`. |
| `Schema` | What `schemaOf<T>()` answers. |
| `ModuleInterface`, `FnInfo`, `ParamInfo`, `TypeInfo`, `Origin` | What `moduleInterface(path)` answers in a generator. |
| `ContractInfo`, `MemberInfo` | What `contractOf(Name)` answers in a generator. |
| `Request`, `Response` | The HTTP surface `vyrn serve` and `std/ui` use. Request header names are lowercase; `Response.headers` holds every header but `Vary`, which has its own field. |

## Modules

### Text

| Module | Provides |
|---|---|
| `std/strings` | `split`, `lines`, `splitWhitespace`, `joinWith`, `substring`, `indexOf`, `lastIndexOf`, `trim`, `trimStart`, `trimEnd`, `toLower`, `toUpper`, `replace`, `repeat`, `padStart`, `padEnd`, `toHex`, `editDistance`, `fromBytesOr`. Offsets are bytes; case and whitespace helpers are ASCII-only and pass other bytes through. |
| `std/strpred` | `startsWith`, `endsWith`, `contains`, and `slice(s, start, end) -> Result<String, SliceError>`, which refuses a cut inside a UTF-8 character. `findPlain` and `findSkipping` are the substring searches the others use. |
| `std/text` | `decodeUtf8`, `chars`, `utf8Width`, and the functions behind `charCount`, `lineAt` and `colAt`. It is the one statement of what UTF-8 admits. |
| `std/regex` | `compile(pattern) -> Result<Regex, String>`, `find`, `countMatches`, `replaceAll`. A Thompson NFA that a search runs as a lazy DFA: linear time, leftmost-longest matches, no anchors, backreferences, lookaround or counted repetition. `=~` is the separate whole-string match compiled at build time. |
| `std/codecs` | Hex, base64 and percent encoding: `hexEncode`, `hexDecode`, `base64Encode`, `base64EncodeBytes`, `base64Decode`, `urlEncode`, `urlDecode`. A decoder answers `None` for bytes that cannot be a `String`. |
| `std/scan` | A comment- and string-aware cursor over foreign text (CSS, ICU messages, templates, SDL) for generators. Offsets are bytes; `line` and `col` stay in step. |

### Numbers, time and randomness

| Module | Provides |
|---|---|
| `std/math` | `min`, `max`, `abs`, `clamp` on `Int64`, and `pi`, `floorF`, `sin`, `cos` on `Float64`. `abs` of `Int64` min saturates. |
| `std/num` | `parseInt64`, `parseUInt64`, `parseFloat64`, `parseFloat32` (each an `Option`), and `f64Str`. Parsing is correctly rounded, with no `strtod` underneath. |
| `std/time` | `now() -> Instant` (UTC milliseconds), `monotonic()`, the calendar breakdown (`civil`, `year` to `second`) and `format`, `formatIso`. UTC only. `now` and `monotonic` are host effects, so a generator cannot call them. |
| `std/random` | `Rng`, `seededRng`, `nextInt`, `nextInRange`: SplitMix64 as a value, so a seeded run reproduces everywhere. `randomSeed()` is the one host effect. Not for secrets. |
| `std/hash` | `fnv1a`, `fnv1aStr` (non-cryptographic), `sha1`, `sha1Hex`, and the `Hashable` protocol a `Map` key type implements. |

### Collections and protocols

| Module | Provides |
|---|---|
| `std/arrays` | `map`, `filter`, `fold`, `any`, `all`, `includes`, `sortWith` (a comparator), `sortBy` (an `Int64` key). |
| `std/slots` | `Slots<T>` and `Handle<T>`: a generational slab. `insert`, `remove`, `get`, `alive`, `count`, `capacity`, `handles`. `s[h]` and `s[h] = v` index it, `for x in s` visits the live elements, and `s.tryAt(h)` reads in place. A stale or foreign handle is dead, never plausible. |
| `std/stream` | `unfold(seed, step)` over a `Cursor` (`cursorGet`, `cursorSet`), and the lazy combinators `map`, `filter`, `take`, `merge`. Each takes a stream and returns one the caller must dispose. |
| `std/fallible` | The `Fallible` protocol that `?` resolves through for a user enum. |

### Data formats

| Module | Provides |
|---|---|
| `std/json` | The `Json` tree (`JNum` keeps the number's text, objects keep field order), `emit`, `emitPretty`, `jsonEq`, `sortKeys`. `j.tryField(k)` and `j[i]` read in place. It imports nothing, so a program that only writes JSON links no parser. |
| `std/jsonread` | `parseJson`: strict JSON. Commas are required, trailing commas and duplicate keys are refused, and every error starts `line N, col M:`. |
| `std/json5` | `parseJson5` into the same tree. There is no JSON5 writer. |
| `std/jsondec` | The untyped half of `fromJson`; the compiler generates the typed half per target type. |
| `std/von` | VON, Vyrn's literal grammar as a file format: `parseVon` (a `gen fn`, so a config error is a build error), `toVon`, `emitVon`, `jsonToVon`. |
| `std/storage` | `writeAtomic(path, content)`: a temp file renamed over the target, so a crash leaves the old file or the new one. The builtins `save`, `load` and `loadOr` expand to it with `toJson` and `fromJson`. |

### Programs

| Module | Provides |
|---|---|
| `std/args` | `cli()`, `cliOf(list)`, `flag`, `opt`, `positionals`, `rest`: four probes over the argument list. No spec, no help text. |
| `std/cli` | `cli("./module")`, a generator: every exported record type becomes `parse<Name>(argv) -> Validation<Name>` and `help<Name>()`. Field types decide the flags, and each value is validated by its own type. |
| `std/bench` | The harness `vyrn bench` links; no program imports it. |

### Generators for the web and for services

Each of these is a library of `gen fn`s. The compiler knows none of their domains.

| Module | Provides |
|---|---|
| `std/html` | The view tree (`el`, `text`, `cls`, `attr`, `on`, `keyed`, `empty`), `toHtmlString`, `document`, and `diff`, which yields the `PatchOp` list `web/vyrn-dom.js` applies. Text is escaped; a bad tag or attribute name is refused. |
| `std/vyx` | `components(dir)` compiles `.vyx` single-file components (a `<script>` of Vyrn and a `<template>`) into view functions. |
| `std/ui` | `pages(dir)` turns a routes directory into `route(req) -> Response` with a typed URL helper per route. A page module exports what the `Page` contract allows: `head`, `data` (`Query` or `Lazy`), `Params`. |
| `std/tw` | `tw("theme.json")` generates the utility stylesheet and the `Tw` class type, so a misspelled class is a compile error. |
| `std/icons` | `icons(collection, names)` generates one function per glyph from an Iconify collection pinned in the lock. A misspelled glyph fails the build with the nearest name. |
| `std/i18n` | `i18n(dir)` reads every `<locale>.json`, checks that the locales agree, compiles each ICU message to a function, and exports `Locale`, `TransKey` (a finite string type) and `t`. |
| `std/rpc` | Typed RPC from a module of procedures: `rpc(dir)`, `client(dir)`, `clientInProcess(dir)`, and the single-module forms `rpcServer`, `rpcClient`, `rpcInProcess`. A rejection carries its `Issue`s. |
| `std/http` | Hand-written REST routes over the same procedures: `http(module)`, `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, server-sent events (`sse`) and WebSockets (`ws`). |
| `std/connect` | `connectServer` and `connectClient`: Connect's unary JSON protocol over HTTP/1. |
| `std/openapi` | `openapi(contract)`: an OpenAPI 3.1 document of the procedures. |
| `std/graphql` | `sdl(contract)` writes the SDL; `graphqlServer` executes queries against the procedures. |

### Generator support

| Module | Provides |
|---|---|
| `std/contract` | `checkContract(iface, contractOf(Name))` and the member queries, returning `Issue`s. |
| `std/diag` | `report` and `reportHere`: a generator emits a diagnostic at a file and line it read. |
| `std/hints` | Per-project rule levels and per-line waivers for checking libraries. |
| `std/vyx-hints` | Accessibility, security and performance rules for `.vyx` files, reported through `std/hints`. |
| `std/symbolmap` | The symbol map a generated module exports: each symbol's source declaration, as JSON for tools. |
