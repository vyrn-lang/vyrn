# Foreign dependency interop — research

Two questions, answered from the record and the code:

1. How can a Vyrn program **use** a dependency built for another ecosystem?
2. How can a Vyrn program **export** idiomatic bindings to another language?

This is research, not an RFC. It proposes no surface. It records what exists,
what the mechanisms are, what each one demands, and which parts do not fit. Open
questions are marked **open**.

---

## 1. The baseline — what already exists

Nothing below is proposed. Each row is in the tree today.

| Piece | Where | What it gives interop |
|---|---|---|
| `extern fn` declarations | RFC-0012; `checker.rs` signature domain | A body-less function with a checked scalar/`String` signature. |
| Native lowering of an extern | `vyrn-codegen/src/lib.rs:959`–`980` | A real `declare` plus a real `call` at each use site. The symbol is `__vyrn_extern_<name>`. |
| The trap stub | `vyrn-cli/src/main.rs:2654` (`extern_trap_stubs`) | A generated C body per extern that prints the canonical wording and exits. **This is the seam.** A real foreign body attaches exactly here. |
| A named-symbol allowlist | `vyrn-codegen/src/lib.rs:565` (`host_boundary_extern`) | Three externs (`hostNowMillis`, `hostMonotonicNanos`, `hostRandomSeed`) already resolve to real C symbols instead of a trap. The precedent for "an extern that is a real foreign call" is shipped. |
| `declare` of arbitrary C | `vyrn-codegen/src/lib.rs:801`–`960` | The IR already declares `printf`, `strcmp`, `fopen`, `free`, `strcat`. Calling C from the native backend is a solved mechanical problem. |
| The clang link step | `vyrn-cli/src/main.rs:4590` | `clang <out>.ll <out>.shim.c -o <out> -O2 -ffp-contract=off -Wno-override-module -march=… [-pthread -lm]`. Adding an archive to that line is one `cmd.arg`. |
| `export extern fn` | RFC-0012 M2 | A Vyrn function additionally exported to the host. Today the export is wasm-only (an inline `wasm-export-name` attribute). |
| `__vyrn_malloc` / `__vyrn_free` | RFC-0077 M6 | A module that takes or returns a `String` across the boundary exports both. The host allocates arguments and frees results. |
| The `vyrn:exports` custom section | RFC-0012 M3 | The module carries a machine-readable fact about its own exports. A precedent for shipping the interface inside the artifact. |
| `moduleInterface(path)` | RFC-0021; `parser.rs:443`–`620` | Structured reflection over a module's exported surface: `FnInfo`, `ParamInfo`, `TypeInfo`, `Schema`, `Origin`. This is an interface IR that already exists. |
| `gen fn` | RFC-0021 | Sandboxed compile-time Vyrn that reads `readFile` / `listDir` / `moduleInterface` / `contractOf` and returns a module source string. |
| Contract emitters | `std/openapi.vyrn`, `std/graphql.vyrn`, `std/connect.vyrn` | Proof that `moduleInterface` is rich enough to render a non-Vyrn contract document (OpenAPI 3.1, GraphQL SDL). |
| `toJson` / `fromJson` | RFC-0018, RFC-0024 | A canonical, byte-identical value codec with accumulating `Validation<T>`. Payload enums and `Result` cross externally tagged. |
| `std/slots` | RFC-0090 M1 | A generational slab. `Handle<T>` is `{ slot, gen, owner }` — a plain copyable value that owns no heap. |
| `protocol Owned { fn release(consume self) }` | RFC-0086/0092/0096; `std/slots.vyrn:247`, `std/vyx.vyrn:417`, `examples/ownedcontainer.vyrn:15` | A user type declares how it is reclaimed. A declared row wins over the seeded built-in row, and the structural walk stops at a declared name. |
| `impl MustUse for T` | RFC-0086 M3; `examples/mustuse.vyrn` | A user type declares "acquired once, disposed exactly once", proved at compile time. |
| The `Txn` shape | `examples/mustuse.vyrn:42`–`44` | A user record with a declared release **and** a must-use obligation. This is the foreign-handle pattern, already in the corpus with a non-foreign payload. |
| `Task<T>` | RFC-0095 | The proof that the pattern closes a real operating-system handle: `join` consumes, `drop` waits and releases, an undischarged task is refused, and the handle count stayed flat at 72 over 200,000 spawns. |
| `Code`, `Logger` | RFC-0076, RFC-0008 | Compiler-internal opaque types. `Code` is `Type::Named("Code")` lowering to `i64`. They are the precedent for what a user-declarable opaque handle would look like. They are not a surface. |
| String representation | RFC-0089 M1a | One word addressing NUL-terminated UTF-8, with `{len, cap}` in a 16-byte header **behind** the pointer. `cap == 0` marks a data-segment literal that is never freed. |
| The parity exclusion mechanism | `vyrn-cli/tests/parity.rs` | `WASM_ONLY`, `EXPECTED_CHECK_FAILURE`, `KNOWN_DIVERGENT`. A host-dependent feature already has a place to live in the harness. |

Two gaps matter more than the rest:

- **The manifest has no link vocabulary.** `vyrn.json` reads `name`, `main`,
  `dependencies`, `nativeTarget`, `audience`, `server`, `client`, `public`,
  `roles`. There is no `nativeLibs`, `ldflags`, `objects`, or `include`.
- **`vyrn build` refuses unknown arguments** (`main.rs:4460`–`4478`) and the
  `-march` note refuses flag passthrough on principle. No environment variable
  injects a flag, an object, or a library.

---

## 2. Using a foreign dependency

### 2.1 The mechanism survey

| Mechanism | What it is | Fit for Vyrn |
|---|---|---|
| **C ABI + a declaration** | Declare the symbol, call it, link the archive. | **The only mechanism that works today.** The IR already declares C symbols; the link step already runs clang. |
| **Header translation (bindgen)** | Parse a `.h` with libclang, emit declarations. | The parsing does not belong in the compiler. It belongs in a `gen fn` reading a **pre-dumped** header description. Precedent: JSON-Schema type imports (RFC-0010 M2) put schema knowledge in a generator, not the compiler. |
| **cbindgen** | Emit a `.h` from Rust. | Irrelevant inbound. Relevant outbound (§3). |
| **cxx** | A Rust↔C++ bridge that generates a C++ shim compiled by the user's own `cc` build. | The *shape* is right and the implementation is not. Vyrn cannot express mangled names, vtables, templates, or exceptions. A C++ library reaches Vyrn only through a hand-written `extern "C"` facade the user compiles. |
| **uniffi** | UDL or proc-macro → C ABI + a serialization buffer + per-language code. | Inbound: no. Outbound: this is one of the two models for §3. |
| **SWIG** | Parses C/C++ headers, generates wrappers for many languages. | No. It needs a C++ parser and it generates for the wrong direction. |
| **wasm component model + WIT** | A typed IDL (`records`, `variants`, `results`, `options`, `lists`, `strings`, `resources`) plus a canonical ABI that states who allocates and who frees. | **The best type-level fit that exists**, and useless for the native target. See §2.5. |
| **Static linking** | An `.a` / `.lib` joins the clang line. | Fits. Reproducible, hashable, one artifact. |
| **Dynamic linking** | A `.so` / `.dll` resolved at load. | Fits mechanically. Breaks the lock model — see §2.6. |

### 2.2 What the C ABI demands of Vyrn's type domain

The current extern domain (`extern_abi_ll`, `vyrn-codegen/src/lib.rs:583`) is
scalars, `Bool`, `Float32`/`Float64`, `String`, and `Unit`. Against a real C
header that is a narrow domain, and the narrowness is mostly correct.

**What crosses cleanly.**

- Integers and floats. One-to-one, per width.
- `Bool` as `i32`.
- `const char *` **in**. A Vyrn `String` is one word addressing NUL-terminated
  UTF-8. It is already the C representation. No conversion, no copy, no encoding
  step. This is a real advantage over most managed languages and it costs
  nothing.

**What crosses only by copying.**

`const char *` **out** cannot become a `String` by adoption, and the reason is
structural rather than a policy choice. A Vyrn `String` carries `{len, cap}` in
a 16-byte header *behind* the pointer (8 bytes on wasm32). A pointer C returns
has no such header. Reading `len` from it reads whatever precedes the foreign
buffer.

So a returned C string is **always copied** at the call site. This removes the
"must I free it" question at the same time, which is the second reason to do it.
The cost is one allocation per call and it is not negotiable.

**What does not cross, and why.**

- **Pointers.** The language has none — no pointer type, no `transmute`, no
  unchecked indexing. So `void *`, `int *out`, `size_t *len`, and every
  out-parameter in C are unreachable. A C API that returns a length through a
  pointer has to be wrapped by a C shim first.

  The record is precise about the status of this. RFC-0082 withdrew only the
  claim that *containers* need a raw-memory view, and said so: *"Whether a
  raw-memory view should ever exist for its own sake — FFI struct layouts, SIMD
  alignment, zero-copy over foreign buffers. Those are real and this RFC does
  not serve them."* The FFI half of that question is **open**, not answered.
- **Structs by value.** Vyrn records are structural and the compiler owns their
  layout. Crossing a struct by value needs a declared C layout *and* the
  platform's argument-classification rules, which differ between the SysV ABI
  and MSVC. Refuse this. It is the single largest source of silent corruption in
  every FFI that allows it.
- **Callbacks.** RFC-0023 refuses function pointers on purpose, and the
  invariant is asserted: every emitted `call` names a symbol. `qsort`,
  `sqlite3_exec`, and libcurl's write callback are therefore not bindable
  without breaking a shipped guarantee. This is the hardest wall in §2 and it is
  open question 1 in §6.
- **Varargs.** No.
- **Arrays.** `Array<UInt8>` is `{ ptr, len, cap }`. Handing its buffer to C
  means handing out an interior pointer, which is a borrow the language cannot
  currently express across the boundary. RFC-0092 makes a projection a borrow
  *inside* Vyrn; nothing carries that fact outward.

The honest conclusion: the useful inbound domain is **scalars, `String` in, and
an opaque handle**. That is small, and it is enough for a large class of real
libraries, because most C libraries are `open` / `do` / `close` over a handle.

### 2.3 The ownership boundary — the hard question

C has no ownership model. Every C API states its rule in prose, in a doc
comment, or nowhere. Vyrn's model is the opposite: ownership is **emitted from
the type**, not inferred (RFC-0089 deleted the inference half of `own.rs`), and
the whole model is compile-time — since RFC-0090 M4 there is no refcount, no
generation slab, no drop flag, and no collector anywhere in the language.

Two of RFC-0089's five rules decide everything at a boundary.

**Rule 2 — the capabilities are the calling conventions.** *"`read` and `modify`
are second-class: a borrowed value cannot be stored in a field, captured by an
escaping closure, put in a container, or returned — and it cannot be consumed…
That one restriction is what makes the whole thing work without lifetime
annotations — a borrow that cannot escape needs no lifetime, because its
lifetime is the call."*

**Rule 3 — *"A function returns an owned value. Always. No borrowed returns."***

Rule 3 is why §2.2's copy is mandatory rather than cautious. There is no
borrowed return in the language to model a `const char *` into static storage
with, so a foreign call that produces one must produce an owned `String`.

So a foreign declaration has to *carry* the rule. Vyrn already has the
vocabulary for it — the capabilities are exactly the answers a C function
gives:

| C convention | Vyrn spelling | Meaning |
|---|---|---|
| callee borrows for the call, does not retain | `read` parameter | Caller keeps and frees. This is already the parameter rule. |
| callee takes ownership and will free | `consume` parameter | Caller may not use it again. |
| callee writes into caller storage | `modify` parameter | By reference, visible to the caller. |
| callee returns a buffer the caller must free | owned return | The binding's caller owns it. |
| callee returns a static or interned buffer | — | No spelling, by rule 3. Copy it. |

Three cases, and only two are safe:

1. **Foreign allocates, foreign frees.** The value is an opaque handle. Wrap it
   in a nominal record over an `Int64` with `impl Owned` and `impl MustUse`, the
   shape `examples/mustuse.vyrn`'s `Txn` already has. Deterministic, checked at
   compile time, and it needs no new *language* feature. **This is the case to
   build.**

   Two details it does need. A declared-`Owned` type has no structural `copy`,
   so it must also declare `impl Copy` or refuse to be copied — the latter is
   what a handle wants. And the release body can only reach the foreign side
   through a foreign call, so this case depends on M1 rather than standing alone.
2. **Vyrn allocates, foreign borrows for the call.** A `read String`. Safe only
   if the callee does not retain the pointer past its own return. Nothing can
   verify that. The declaration is a promise.
3. **Foreign allocates, Vyrn frees** (or the reverse). Wrong unless both sides
   use the same allocator. Vyrn's `__vyrn_malloc` is a checked wrapper over the
   platform `malloc` on native, so it happens to work there and does not work on
   wasm, where the allocator is a bump pointer. Refuse it rather than depend on
   the coincidence.

The precedent for how expensive it is to get this wrong is in the tree.
RFC-0012's `String` ownership at the JS boundary took a measured leak (277 pages
for 20000 calls), a decision, an exported `__vyrn_free`, three new refusals, and
five bug fixes across the repo before it was uniform. The lesson generalizes:
**state the ownership rule per position, once, in the ABI, and make the
language's own rule the answer.** Do not allow a per-library convention.

The precedent that the pattern works on a real operating-system resource is
`Task<T>` (RFC-0095): `join` consumes it, `drop` waits and releases the frame,
the record and the handle, an undischarged task is refused at compile time, and
the measured handle count stayed flat at 72 over 200,000 spawns. That is the
same mechanism a foreign handle would use, with the difference that `Task` is
compiler-owned and a foreign handle would be user-declared.

### 2.4 The parity cost

A foreign call is available on the native target and nowhere else. The
interpreter is the reference semantics, so any program that calls a C library
leaves the reference. RFC-0012 already met this and answered it: the
non-supporting backends **trap** with canonical wording, and the example goes in
a harness list.

The same answer works here, mirrored: a `NATIVE_ONLY` list, the interpreter and
wasm both trapping, and the trap wording asserted byte-identical between them.
That keeps `KNOWN_DIVERGENT` empty, which is the invariant that matters.

**Open:** a foreign declaration could carry a Vyrn *model* body used by the
interpreter, so a program with a foreign dependency stays testable and stays in
the parity corpus against its model. That is attractive and it is a new kind of
declaration ("two implementations, one signature"), so it is not free.

### 2.5 The component model, honestly

WIT is the closest thing to a typed IDL that matches Vyrn's own type domain:
records, variants, results, options, lists, strings, and **resources** (an
opaque handle with a declared destructor — exactly case 1 in §2.3). The
canonical ABI states who allocates (`cabi_realloc`) and who frees
(`post-return`), which is the question §2.3 says must be answered once.

Three facts keep it off the near path:

- It buys nothing on the native target, which is where a C library actually is.
- The direct wasm backend (RFC-0077) emits a core module. Emitting component
  metadata is more work than emitting a custom section.
- Vyrn already has a working wasm host story (`wasi-min.js`, zero dependencies).
  Components need a component-aware host.

It stays on the ladder as a late milestone because the *type mapping* is a gift
and because a Vyrn component would be consumable by every component host without
per-language glue. It is not the first thing to build.

### 2.6 Build-system integration

The smallest honest shape, given §1's two gaps:

```json
{
  "foreign": {
    "sqlite3": {
      "declarations": "./sqlite3.vyrn",
      "link": { "static": "vendor/libsqlite3.a", "sha256": "…" }
    }
  }
}
```

- `declarations` is an ordinary Vyrn file of `extern fn` declarations. It is
  hand-written, or produced by a generator from a dumped header description. The
  compiler learns no header format either way.
- `link.static` is a path to a prebuilt archive. `sha256` extends the existing
  lock discipline (`vyrn.lock` already stores `specifier ⇥ url ⇥ sha256` and
  verifies on every load) to a binary.
- The link step appends the archive to the clang line. That is one `cmd.arg`
  next to `add_native_clang_flags`.

Two things this deliberately does not do:

- **It does not build C.** Vyrn has no build system and should not grow one. The
  user brings an archive. If the archive has to be built, a Makefile builds it.
- **It does not accept arbitrary flags.** The `-march` note (`main.rs:83`)
  refuses passthrough because a typo reaches the user as a clang error. The same
  argument applies to `ldflags`. A named archive path is checkable; a flag string
  is not.

Static over dynamic, for v1. A static archive hashes, vendors, and ships as one
file, which is what the remote-import model already guarantees for source. A
`.so` is per-platform, per-version, and resolved at load time, so a locked build
stops being reproducible at the one place the lock exists to protect. **Open:**
per-triple lock entries would make dynamic linking honest, at the cost of a
platform matrix in the lockfile.

### 2.7 Rust crates and C++ libraries — the direct answer

**A Rust crate.** There is no path to using a Rust crate as a Rust crate. Traits,
generics, `Result`, lifetimes, and ownership do not cross a C ABI. The real
answer is: someone publishes a `staticlib` with `#[no_mangle] extern "C"`
functions and a Vyrn declaration file, and Vyrn links it exactly like C. That is
the same work as binding a C library, plus a Rust toolchain the *publisher*
needs and the *consumer* does not.

Vyrn should not run cargo. Making cargo a build dependency of every user project
contradicts the dependency policy that keeps the compiler buildable without
LLVM, clang, or a sysroot.

**A C++ library.** Only through a `extern "C"` facade someone writes and
compiles. This is what `cxx` really does under its generated code, and Vyrn
cannot generate the C++ half because it cannot parse C++. Recording it as a
documented convention is the whole of what is achievable, and it is enough:
"write a C facade, build it into an archive, declare it" is a known and
unsurprising instruction.

---

## 3. Exporting bindings

### 3.1 What unibind does

Source: `packages/unibind/README.md` in `indexable-inc/index`.

The bet: *"UniFFI-style tools settle for a C-ABI lowest common denominator:
every value crosses a serialization shim, and async, cancellation, and resource
cleanup are bolted on. unibind inverts that."*

The pipeline is three stages:

```
annotated module --syn lowering--> Interface IR --backend render--> binding code
```

1. **Annotation.** `#[unibind::export]` on a Rust module; `#[unibind::record]`,
   `#[unibind::error]`, `#[unibind::object]` on the types.
2. **IR.** One parse into a language-agnostic `Interface`, validated by
   `unibind-core`. *"The interface definition stays write-once."*
3. **Render.** Each backend emits through its ecosystem's incumbent library:
   pyo3 for Python, napi-rs for TypeScript, rustler for Elixir. Each language
   gets native semantics — real exception hierarchies, native async and
   cancellation, RAII-shaped cleanup.

Four details are worth more than the headline:

- **The IR is embedded in the built artifact.** `.unibind_ir`, a link section
  (`__DATA,__unibind_ir` on Apple), wasm-bindgen style. An out-of-process
  generator, `unibind-gen`, reads it back and writes the host files — `.pyi`,
  `py.typed`, `index.d.ts`, `schemas.ts`, `index.js` — *"never at macro time"*.
  The IR is the contract between the compiler-side and the file-writing side.
- **The JVM backend has no incumbent to lean on**, so it does what the headline
  rejects: *"every exported function becomes one `extern "C"` symbol with the
  uniform shape `fn(args: *const u8, len: usize, out: *mut RawBuf)`, values
  cross in a length-prefixed wire format"*, plus one generated Java class using
  the FFM API. The key sentence: *"Symbol names and wire layouts are rendered by
  the same crate on both sides, so they cannot drift apart."*
- **Zod schemas are generated, not hand-written**, because *"a `.d.ts` is erased
  at runtime, so anything reaching a binding as `unknown` … still needs a
  check"*, and generating it from the same IR means *"a schema cannot drift from
  the type it validates."*
- **Ownership at the boundary is stated positionally**: *"Borrowed forms
  (`&str`, `&Path`, `&[u8]`) are argument-only; returns and record fields own
  their data."*

### 3.2 Where Vyrn is like unibind and where it is not

**Like it, and further along:**

- Vyrn already has the IR. `moduleInterface(path)` returns `FnInfo` /
  `ParamInfo` / `TypeInfo` / `Schema` / `Origin`. unibind needed an annotation
  layer to identify the surface; Vyrn's `export` keyword already is that layer,
  and `contract` (RFC-0071) narrows it further.
- Vyrn already renders non-Vyrn contracts from that IR: OpenAPI 3.1
  (`std/openapi.vyrn`), GraphQL SDL (`std/graphql.vyrn`), Connect
  (`std/connect.vyrn`). The "one IR, N renderers" seam is proven, not proposed.
- unibind generates Zod schemas to recover the runtime check a `.d.ts` erases.
  **Vyrn gets that for free and gets it more precisely**, because a validated
  type *is* the check: `min`, `max`, `multipleOf`, `minLength`, `maxLength`,
  `pattern` are all fields of `Schema` (`parser.rs:414`–`438`), and `jsonSchema`
  already emits them. unibind derives a schema from a type; Vyrn's type carries
  the schema.
- Vyrn already embeds a fact about its exports in the artifact
  (`vyrn:exports`). Generalizing that to a full interface section is the same
  move unibind made with `.unibind_ir`, in a repo that has already made it once.

**Unlike it, and this is the decisive difference:**

unibind can avoid the C ABI because **its source language is the same language
the incumbent binding libraries are written in**. pyo3, napi-rs, and rustler are
Rust crates; a Rust crate can depend on them. Vyrn is not Rust. There is no pyo3
for Vyrn and there will not be one.

So Vyrn **must** have a uniform low-level boundary — a C ABI or a wasm
component — at the bottom. The unibind bet does not transfer.

What *does* transfer is the upper half, and it transfers completely: **one IR,
one uniform wire, and N renderers that each emit idiomatic host-side files.**
That is precisely unibind's own JVM backend, which is the honest template for
every Vyrn target rather than the exception it is for unibind. Its sentence is
the design rule to copy: both sides of the boundary are rendered by the same
generator, so they cannot drift apart.

### 3.3 The wire: JSON now, scalars later

Two candidate low-level ABIs.

**A. One string in, one string out.**

```c
const char *vyrn_call_getUser(const char *request_json);
```

Vyrn already has every part of this. `toJson` is canonical and byte-identical
across backends. `fromJson` returns `Validation<T>`, runs every `where` clause,
and accumulates one `Issue` per failure with a dotted path. `Result` and payload
enums cross externally tagged (RFC-0024). `rpcInProcess("./api")` is *already*
the in-process, no-wire double of exactly this, and it is in the parity corpus.

So the first export milestone is close to free: it is `rpcInProcess` with the
transport changed from `fetch` to a function call.

The cost is the serialization tax unibind objects to. The honest reply is that
for Vyrn the tax buys the validation — the same call that decodes also enforces
every refinement in the type, and that enforcement is the language's selling
point. It is not a tax paid for nothing.

**B. Scalars by value, strings by pointer.**

This is the existing `extern` ABI (`extern_abi_ll`). It is already implemented,
already parity-tested, and already has a settled ownership rule after RFC-0089.
It should be the fast path for signatures that fit its domain, with A as the
fallback for records, enums, arrays, and `Option`.

The two compose. Start with A because it covers the whole type domain. Add B as
a per-signature optimization once there is something to measure.

### 3.4 Ownership going out

The rules RFC-0089 settled at the wasm boundary transfer to a C ABI unchanged,
and that is the point of having settled them:

- A `String` **parameter** is borrowed. The caller allocates it and the caller
  frees it. An exported function may not store one past its own return, and
  storing one is now refused for every function, not only an exported one.
- A returned `String` is the **caller's**. The host frees it, through the
  exported `__vyrn_free`.
- An `export extern fn` may not return module state, may not return a
  projection, and may not declare a `consume String` parameter.

That is a complete, uniform, already-enforced ownership statement. A C header
and a `.d.ts` can both be generated from it without asking the author anything.

**Handles out** are the one thing missing. unibind's `#[unibind::object]` is a
stateful handle with methods and deterministic cleanup. Vyrn has no way to give
a host an owning handle to a Vyrn value: everything either copies out or lives
in module state.

The mechanism it needs already exists as a library. `std/slots` is a
generational slab whose `Handle<T>` is `{ slot, gen, owner }` — a plain copyable
value owning no heap. A handle out is an `Int64` the host holds, an exported
`release(h)` the host calls, and a dead-handle access that traps instead of
dangling. On the Vyrn side, `impl MustUse` proves the slab entry is disposed
exactly once. **No language feature is required.** That is a strong result and
it should be tested before it is believed.

### 3.5 What does not fit

- **Async.** RFC-0016 decided Vyrn adds no `async`/`await`, and named the
  reason: a wasm module cannot switch stacks, and determinism is the product.
  unibind's phase 2 — coroutines, cancellation that drops the in-flight future —
  has no Vyrn counterpart and should not be faked. Python bindings are
  synchronous, or synchronous with the GIL released.
- **Async iterators.** RFC-0075's `Stream<T>` is **pull-based**, which is the
  same shape as unibind's `UniStream<T>` (*"each `__anext__` polls exactly one
  item, so a consumer that stops early stops the producer with it"*). A pull
  stream renders to a synchronous Python `__next__` and a synchronous JS
  iterator with no thread at all. Only `AsyncIterable` needs one. Ship the
  synchronous form; it is the majority of the value.
- **Callbacks into Vyrn.** Same wall as §2.2. A host calling a *named* export is
  fine and already ships. A host registering a Vyrn closure as a C callback
  needs a function pointer, which RFC-0023 refuses and asserts against.
- **Mutable receivers across the boundary.** unibind rejects `&mut self` on
  every backend. Vyrn should too, for the same reason: two sides holding a
  mutable alias is the case neither language's model covers.

### 3.6 Bindings as generators, or in the compiler

**Generators, decisively — with two compiler-sized exceptions.**

The case for generators is already made in the repo: RPC, i18n, OpenAPI,
GraphQL, and Connect all became libraries over `moduleInterface`, and the record
says the protocol roadmap will never touch the compiler. A binding renderer is
the same shape as an OpenAPI renderer. It reads `FnInfo`, walks `Schema`, and
prints text.

The two exceptions are real and neither is about rendering:

1. **The compiler must emit a C-callable artifact.** `export extern fn` today
   produces a wasm export. A native C symbol, plus
   `vyrn build --emit staticlib|cdylib`, plus forcing out `__vyrn_malloc` /
   `__vyrn_free`, is compiler work. No generator can do it.
2. **A generator cannot write a file.** This is enforced, not incidental:
   `writeFile`, `renameFile`, and `fsyncFile` are in `COMPTIME_FORBIDDEN`
   (`checker.rs:8494`), and RFC-0021 lists "generators writing files" under out
   of scope. RFC-0073 refused a second output channel outright: *"There is no
   `Module`; a `gen fn` returns `String` … adding a second artifact means a new
   generator protocol, a new cache entry to keep in step, and a new way for the
   two to disagree."*

   So a `.d.ts` produced by a generator is a Vyrn `String` inside a synthesized
   module until something runs. The two shipped answers are: redirect
   `vyrn emit-gen`'s stdout, or have the compiled program write the file itself.
   The second is the honest one — a **bindgen is an ordinary Vyrn program**:
   `import { dts } from bindings("./api")`, then `writeFile("index.d.ts", dts())`.
   It needs no new command and no new generator protocol.

   The alternative is `vyrn doc`'s shape — a compiler subcommand that writes a
   directory. That is a precedent for `vyrn bindings -o dir/`. It is more
   convenient and it puts the renderer back inside the compiler, which is the
   thing the last twenty RFCs spent effort removing. **Open**, but the default
   should be the program.

**The one thing worth taking from unibind's out-of-process design:** the
interface should be readable from the *built artifact*, not only from the
source. `unibind-gen` reads `.unibind_ir` out of a cdylib. Vyrn's equivalent is
a `vyrn:interface` custom section (wasm) and a `.vyrn_interface` object section
(native), carrying the serialized `ModuleInterface` as JSON. That decouples the
renderer from the compiler version and makes a shipped binary self-describing.
`vyrn:exports` proves the mechanism works.

---

## 4. The hard question, restated

Everything above reduces to one sentence.

**Vyrn's ownership is a fact the compiler emits from a type. Across a foreign
boundary it becomes a promise a human writes.**

RFC-0082 states the boundary the language holds: *"Vyrn serves the
abstraction-building uses of `unsafe` and none of the abstraction-escaping
ones."* A foreign call is abstraction-escaping by definition. That does not make
it forbidden. It makes it the one place where the rule has to be replaced by
something else, and the something else is a written declaration.

Inbound, the promise is about the foreign library: does it retain this pointer,
does it free this buffer, may I free what it returned. Nothing can check it. The
containment strategy is to make the promise **narrow, explicit, and local**:

- narrow — scalars, `String` in, and opaque handles only;
- explicit — the capability is written in the declaration, so the compiler
  enforces the Vyrn half even though it cannot enforce the foreign half;
- local — the declarations live in one named file per foreign dependency, so the
  trust boundary is a file listing rather than a property of the whole program.

Outbound, the promise is already discharged. RFC-0089 made ownership at the host
boundary uniform: parameters borrowed, returns owned by the caller, no stored
parameters, no returned projections, no `consume String`. A C header and a
`.d.ts` can both be generated from that without asking anyone a question. This
is the part where Vyrn is ahead of the languages it would bind to, and it got
there by paying for it once.

---

## 5. Milestone ladder

Each milestone states the smallest evidence that would make it believable.

### M1 — Call one C function

The narrowest possible slice. A declaration names a real symbol instead of
trapping; the link step takes one archive.

- A foreign declaration file of `extern fn`s, marked as foreign so the symbol is
  the C name rather than `__vyrn_extern_<name>`. `host_boundary_extern` already
  proves the mapping; generalize it from a hardcoded list to a declaration.
- `vyrn.json` grows `foreign`, with `declarations` and `link.static`.
- `extern_trap_stubs` skips a foreign extern; the archive joins the clang line.
- Domain: `Int64`, sized ints, `Float32`/`Float64`, `Bool` only. No `String` yet.

**Evidence.** An example calls two functions in a three-line C archive built by
hand and checked in as source plus a build note. The native binary produces the
right answer. The interpreter and the wasm module both trap with the canonical
wording, byte-identical to each other, asserted by a `NATIVE_ONLY` harness list.
`KNOWN_DIVERGENT` stays empty.

### M2 — Strings, and the ownership statement

- `read String` in: pass the existing NUL-terminated pointer. No copy.
- Returned `const char *`: copied into a Vyrn `String` at the call site. The
  foreign buffer is never adopted and never freed.
- A foreign declaration that names `consume String` is refused, with a reason
  naming the allocator mismatch.

**Evidence.** A leak test: one million calls passing and returning strings, RSS
flat, measured and recorded the way RFC-0012's 20000-call measurement was. A
refusal test with the exact diagnostic text. A round-trip test with non-ASCII
input proving no encoding step exists.

### M3 — A foreign resource handle

The case that makes real libraries usable. It builds on M1 and M2; the release
body is a foreign call.

- A nominal record over an `Int64`, the `examples/mustuse.vyrn` `Txn` shape.
- `impl Owned for T { fn release(consume self) { cClose(self.h) } }`.
- `impl MustUse for T`, and no `impl Copy`.

**Evidence.** Open, use, and close a real C resource. Three compile-time
refusals: never disposed, disposed twice, used after disposal. One runtime
measurement showing the foreign `close` runs exactly once per open, counted by
the C side, in the shape RFC-0095 used for task handles.

The claim to test is that this milestone needs **no compiler change beyond M1**.
If it holds, the language's declared-release mechanism is already a complete
foreign-resource story. If it does not, the gap it exposes is the most valuable
finding on this ladder.

### M4 — Export a C ABI

- `export extern fn` gains a native C-linkage symbol.
- `vyrn build --emit staticlib|cdylib`.
- `__vyrn_malloc` / `__vyrn_free` exported on the native target under the same
  rule as wasm.
- A generated `.h` from `moduleInterface`, written by an ordinary Vyrn program.

**Evidence.** A C program includes the generated header, links the archive,
calls a Vyrn function, and round-trips a `String` with flat RSS over a million
calls. A `ctypes` script does the same from Python with no glue crate.

### M5 — The interface travels with the artifact

- `vyrn:interface` custom section (wasm) and `.vyrn_interface` object section
  (native), carrying the serialized `ModuleInterface`.

**Evidence.** A reader recovers the interface from a built artifact and it is
byte-identical to `moduleInterface` on the source. The section is additive: a
host that does not know the name still runs the module, which is the property
`vyrn:exports` already demonstrated.

### M6 — The first idiomatic renderer: TypeScript and Node

A `gen fn` over `moduleInterface` emitting the unibind file set:
`index.d.ts` from `FnInfo` plus `///` docs, `schemas.ts` from `Schema` (free,
because the type carries the refinements), `index.js` decoding error enums into
one base class per enum and one subclass per variant per RFC-0024's tagging,
handles as classes with `close()` and `[Symbol.dispose]`.

**Evidence.** A Node conformance suite in the shape of
`packages/unibind/conformance-ts`: 64-bit integers round-tripped past 2^53 and
at the width limits as `bigint`, one disposal per handle, error class identity
per variant, and `tsc --strict` over the generated `.d.ts` against the running
module.

### M7 — A second renderer proves the seam: Python

`.pyi`, `py.typed`, and a `ctypes` wrapper over the cdylib. Exceptions from the
error enums. Context managers from the `Owned` handles.

**Evidence.** A stdlib-only `runner.py` asserting the same semantics as M6, in
CI. The seam is proven only if M7 adds no change to the IR or to M4's ABI.

### M8 — Header translation as a generator

A `gen fn` reading a **dumped** C header description (clang's JSON AST, produced
out of band by the library publisher) and emitting the declaration module M1
takes by hand.

**Evidence.** A declaration module generated for one real library is
byte-identical to the hand-written one from M1–M3. The compiler learns no header
format: this is the JSON-Schema-import precedent applied again.

### M9 — WIT and the component model (gated, open)

Emit a WIT world from `moduleInterface` and a component from the direct wasm
backend.

**Evidence.** `wasm-tools validate --features component-model` passes, and a
component host calls the module with records, variants, and lists crossing with
no hand-written glue.

The gate is whether M6 and M7 make the uniform C ABI look adequate. If they do,
M9 buys reach rather than correctness and can wait.

---

## 6. Open questions

1. **Callbacks.** RFC-0023 refuses function pointers and asserts the absence.
   Taking the address of a *named*, non-capturing top-level function to hand to
   C is a value leaving the language, not dynamic dispatch inside it. Is that a
   bounded exception, or does it reopen the bill RFC-0023 refused?
2. **The interpreter's reference status.** Should a foreign declaration carry a
   Vyrn model body, so a program with a foreign dependency stays in the parity
   corpus against its model? Two implementations under one signature is a new
   kind of declaration.
3. **The mandatory copy.** Rule 3 and the string header together force a copy on
   every `const char *` a foreign call returns. Is there a workload where that
   allocation is unacceptable, and if so does it want a raw-memory view rather
   than a new return spelling?
4. **Dynamic linking and the lock.** Per-triple lock entries would make a `.so`
   or `.dll` reproducible. Is a platform matrix in `vyrn.lock` worth it?
5. **`moduleInterface` outside generation.** It is generation-only today
   (`checker.rs:5746`). A bindgen that is an ordinary program would want it at
   run time. Should it be lowered by the compiling backends, or does M5's
   artifact section make the question moot?
6. **`vyrn bindings` as a subcommand.** `vyrn doc` is the precedent for a
   compiler subcommand that writes a directory. Convenience against keeping
   renderers out of the compiler.
7. **Structs by value.** Refused here. Is there a subset — a record of scalars,
   one platform, one classification rule — that is safe enough to be worth the
   ABI risk?
8. **A C++ facade convention.** Documented instruction only, or a scaffolded
   `vyrn new --facade` that emits the `extern "C"` skeleton?
9. **`Array<UInt8>` out.** Handing C an interior pointer is a borrow crossing
   the boundary. RFC-0092 made a projection a borrow inside Vyrn; nothing
   carries the fact outward. Does a byte buffer need a spelling, or is a copy
   the answer here too?
