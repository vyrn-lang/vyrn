# Vyrn RFCs

This directory is the **design record** for Vyrn. It is the north star: when the
implementation and an RFC disagree, that is a bug in one of them, and the RFC is
where the argument gets settled.

The RFCs capture *decisions and their rationale*, not just syntax. Each one lists
its open questions explicitly, so a reader can tell what is settled from what
still needs a prototype to answer. Several RFCs record a decision that was later
reversed; the reversal is written into the same file rather than hidden, because
the evidence that changed the answer is the part worth keeping.

## Where to start

- [RFC-0001 Vision & Principles](RFC-0001-vision.md) — mission, audience,
  non-goals. Every other RFC answers to it.
- [RFC-0003 Validated Types](RFC-0003-validated-types.md) — the signature
  feature: a type carries the rule that makes a value valid.
- [RFC-0004 Capabilities & Memory](RFC-0004-capabilities-and-memory.md) —
  `read` / `modify` / `consume` / `share`. Read §4 and §5 as history: the memory
  model they record was replaced. The current one is
  [RFC-0089](RFC-0089-mutable-value-semantics.md) through
  [RFC-0096](RFC-0096-a-self-referring-type-declares-its-release.md), and
  [`PLAN-memory-model.md`](PLAN-memory-model.md) is the order it landed in.
- [RFC-0021 Generator Imports](RFC-0021-generator-imports.md) — user code that
  runs at compile time and synthesizes a module. RPC, i18n, UI, OpenAPI and
  GraphQL are libraries over it, not compiler features.

## Status

Each RFC carries its own `**Status:**` header, and that header is the authority.
This index copies the header; it does not judge it.

The corpus uses more than four words, because the work is milestoned. A status
may read "Implemented", "Complete as scoped", "Accepted and complete", "Shipped",
"Superseded by RFC-XXXX", or a per-milestone line such as "M1 and M2 shipped;
M3 stopped at its own limit". Read the header, not a legend.

Several RFCs stopped short of their own design on purpose — 0047, 0074, 0075,
0077, 0080, 0082, 0084, 0085, 0086, 0091, 0093, 0095, 0097, 0098 and 0099 among
them. The count is not restated here, because a count restated is a count that
drifts: each header says which milestone landed and what did not, and the
`Status` column below copies it.

## The index

112 RFCs, numbered 0001 to 0113, with one gap. **There is no RFC-0066** — the
number was skipped and never used. The only mention of it in the repository is
this sentence. Closing the gap would mean renumbering thirty files and breaking
every cross-reference, so the gap stays.

The count, the range, the gap, and the rows below are checked against this
directory by `compiler/vyrn-cli/tests/rfc_index.rs`. That test is also what
holds a banner honest: every `RFC-NNNN` this corpus names has to be an RFC that
exists. What it does not check — status text, titles, and whether a banner's
claim is true — is written down in the test's own header.

| RFC | Title | Status |
|-----|-------|--------|
| [0001](RFC-0001-vision.md) | Vision & Principles | Draft |
| [0002](RFC-0002-type-system.md) | Type System | Implemented |
| [0003](RFC-0003-validated-types.md) | Validated Types | Implemented (core) |
| [0004](RFC-0004-capabilities-and-memory.md) | Capabilities & Memory | Implemented in part; §4–§5 superseded by RFC-0090 |
| [0005](RFC-0005-error-handling.md) | Errors, Null & Concurrency | Implemented |
| [0006](RFC-0006-diagnostics.md) | Diagnostics | Draft |
| [0007](RFC-0007-string-templates.md) | String Templates & Safe Interpolation | Implemented (§v2 included) |
| [0008](RFC-0008-logging.md) | Logging | Implemented in part |
| [0009](RFC-0009-error-model.md) | Structured, Accumulating Validation | Implemented |
| [0010](RFC-0010-modules.md) | Modules: `import`/`export`, Manifests & Reproducible Remotes | Implemented (M1–M4) |
| [0011](RFC-0011-array-mutation.md) | In-Place Array Mutation | Implemented |
| [0012](RFC-0012-js-interop.md) | JS Interop (`extern`) | Implemented (M1–M3) |
| [0013](RFC-0013-module-state-event-loop.md) | Module State & the Host-Driven Event Loop | Implemented |
| [0014](RFC-0014-input-io.md) | Input: Args, Stdin, Files, Bytes | Implemented (M1 + M2) |
| [0015](RFC-0015-testing.md) | Testing: `test` Blocks, `assert`, `vyrn test` | Implemented |
| [0016](RFC-0016-server.md) | The Server: `vyrn serve` & the Async Decision | Implemented |
| [0017](RFC-0017-formatter.md) | `vyrn fmt`: the Canonical Formatter | Implemented |
| [0018](RFC-0018-json-codec.md) | The JSON Codec: `toJson` / `fromJson` | Implemented |
| [0019](RFC-0019-rpc.md) | Typed RPC as a Library | Implemented |
| [0020](RFC-0020-i18n.md) | i18n: Typed Translations & Finite String Types | Implemented (M1 + M2) |
| [0021](RFC-0021-generator-imports.md) | Generator Imports: Compile-Time Module Synthesis | Implemented |
| [0022](RFC-0022-ergonomics.md) | Ergonomics Batch | Implemented |
| [0023](RFC-0023-function-values.md) | Function Values, Monomorphized (Closures v1) | Implemented |
| [0024](RFC-0024-enums-on-the-wire.md) | Payload Enums on the Wire (Codec v2) | Implemented |
| [0025](RFC-0025-worker-threads.md) | Worker Threads: Parallel `spawn`, Concurrent `serve` | Implemented |
| [0026](RFC-0026-ui.md) | The UI Layer: `std/html`, Pages, Components, Compiled Reactivity | Implemented (M1–M4) |
| [0027](RFC-0027-import-namespace.md) | `import * as ns`: Namespaced Imports | Implemented |
| [0028](RFC-0028-map.md) | `Map<String, V>`: The Dictionary Type | Implemented |
| [0029](RFC-0029-module-state.md) | Module State Everywhere: Lifting the Root-Only Rule | Implemented |
| [0030](RFC-0030-if-expression.md) | `if` as an Expression | Implemented (M1) |
| [0031](RFC-0031-interface-closure.md) | `moduleInterface`: The Reachable Type Closure | Implemented |
| [0032](RFC-0032-tw.md) | `std/tw`: Theme-Derived Utility Classes as a Checked Type | Implemented |
| [0033](RFC-0033-origin-maps.md) | Origin Maps: Editor Support Inside Generator Inputs | Implemented |
| [0034](RFC-0034-soft-navigation.md) | Soft Navigation: SPA Feel over MPA Truth | Implemented; superseded in part by RFC-0067 |
| [0035](RFC-0035-patch-protocol.md) | The Patch Protocol: Wasm-Side Diffing | Implemented |
| [0036](RFC-0036-vyx-tw.md) | `.vyx` ↔ `Tw`: Compile-Checked Classes in Templates | Implemented |
| [0037](RFC-0037-stored-closures.md) | Stored Function Values by Defunctionalization (Closures v2) | Implemented |
| [0038](RFC-0038-contract-exports.md) | Contract Exports: Connect, OpenAPI, GraphQL SDL | Implemented |
| [0039](RFC-0039-vyx-v2.md) | `.vyx` v2: Vue-Flavored Templates, `.vyx` Pages, Real Parsing | Implemented |
| [0040](RFC-0040-app-ergonomics.md) | App Ergonomics: Generator Identity, RPC Callbacks, `.vyx` Order | Implemented (§1–§5) |
| [0041](RFC-0041-layouts.md) | Layouts, Error Pages, and Head Ownership | Implemented |
| [0042](RFC-0042-template-intelligence.md) | Template Editor Intelligence | Implemented |
| [0043](RFC-0043-time-random.md) | Time and Randomness at the Host Boundary | Implemented |
| [0044](RFC-0044-storage.md) | `std/storage`: Crash-Safe Persistence | Implemented |
| [0045](RFC-0045-bitwise.md) | Bitwise Operators | Implemented |
| [0046](RFC-0046-strings.md) | `std/strings`: The String Library (+ a `slice` builtin) | Implemented; the `slice` builtin has since been removed (RFC-0078/0079/0094) |
| [0047](RFC-0047-semantic-highlighting.md) | Semantic Highlighting, Import Hover, and Grammar Gaps | Implemented (§1–§3); §4 diagnosed, blocked |
| [0048](RFC-0048-vyx-origins.md) | Complete `.vyx` Origins: Script Sections & Real-File Pages | Implemented (§1–§3) |
| [0049](RFC-0049-vyx-owner-discovery.md) | `.vyx` Owner Discovery & Cached Forward-Mapping | Implemented |
| [0050](RFC-0050-lsp-references.md) | LSP: Scope-Aware Highlight, Import-Path Definition, Namespace Colour | Implemented |
| [0051](RFC-0051-hover-quality.md) | Hover Quality: Docs, Members, Record Structure, Class Precision | Implemented |
| [0052](RFC-0052-safelist-css-hover.md) | Safelisted Class Hover Shows the App's Own CSS | Implemented |
| [0053](RFC-0053-generated-error-mapping.md) | Lex/Parse Errors in Generated Code Map Back to Their Source | Implemented |
| [0054](RFC-0054-code-quotes.md) | Code Quotes: Structured Emission and Real Scanning for Generators | Implemented |
| [0055](RFC-0055-benchmarking.md) | Benchmarking: `bench` Blocks, `blackBox`, `vyrn bench` | Implemented |
| [0056](RFC-0056-smallarray.md) | `SmallArray<T, N>`: Small-Buffer Collections | Implemented |
| [0057](RFC-0057-byte-literals.md) | Byte Literals: `'{'` as `UInt8` | Implemented |
| [0058](RFC-0058-string-length.md) | `String.length` Is a Lie: `byteLength` / `charCount()` | Implemented |
| [0059](RFC-0059-std-json.md) | `std/json` + the std Cleanliness Sweep | Implemented |
| [0060](RFC-0060-control-flow.md) | Control-Flow Ergonomics: `break`/`continue`, `if let`/`while let`, `%` | Implemented |
| [0061](RFC-0061-std-args.md) | `std/args`: CLI Argument Parsing | Implemented |
| [0062](RFC-0062-explicit-builtins.md) | Explicit Builtin Imports + Constructor Highlighting | Implemented |
| [0063](RFC-0063-ci-benchmarks.md) | Benchmarks in CI: `--json`, `--compare`, and the bench job | Implemented |
| [0064](RFC-0064-dev-codelens.md) | "Run dev server" CodeLens | Implemented |
| [0065](RFC-0065-vyrn-doc.md) | `vyrn doc`: Markdown API Docs (Mermaid Included) | Implemented |
| [0067](RFC-0067-soft-navigation.md) | Soft Navigation v2: No More Full Reloads | Implemented |
| [0068](RFC-0068-validation-ux.md) | Structured Validation UX: Issues Survive the Wire | Implemented |
| [0069](RFC-0069-universal-pages.md) | Universal Pages: Nuxt-Mode Navigation | Implemented |
| [0070](RFC-0070-lazy-data.md) | Lazy Data: Render Instantly, Fill In When It Arrives | Implemented |
| [0071](RFC-0071-module-contracts.md) | Module Contracts: Conventions You Can See | Implemented (M1, M2, M2b, M2c, M3, M4) |
| [0072](RFC-0072-audience-and-derived-rpc.md) | Audience and Derived RPC: Deleting the Contract File | Implemented (M1–M5) |
| [0073](RFC-0073-generator-symbol-maps.md) | Generator Symbol Maps: Rename Across the Boundary | Implemented (M1) |
| [0074](RFC-0074-protocol-projections.md) | Protocol Projections: Full Fidelity, No Erasure | M1, M2, M3a, M3b, M4a shipped; M4b remains |
| [0075](RFC-0075-streams.md) | `Stream<T>`: Cleanup as an Obligation, Not a Convention | Implemented (M1–M3); M4 given up |
| [0076](RFC-0076-generators-as-wasm.md) | Generators as Wasm: Compile the Generator, Don't Interpret It | Implemented (M1–M7) |
| [0077](RFC-0077-direct-wasm-backend.md) | A Direct Wasm Backend: Stop Going Through LLVM | Implemented (M0–M2p, M5, M6); M3 and M4 struck |
| [0078](RFC-0078-the-runtime-is-vyrn.md) | The Runtime Is Vyrn | Complete as scoped (M1–M5) |
| [0079](RFC-0079-failure-is-a-value.md) | Failure Is a Value, and Crashing Is the Caller's Call | Accepted and complete (M1–M3) |
| [0080](RFC-0080-associated-types-and-generic-impls.md) | Associated Types and Generic Impls | M1 and M2 shipped; M3 shipped in half |
| [0081](RFC-0081-float-formatting-in-vyrn.md) | Float Formatting in Vyrn | Shipped (M1, M2) |
| [0082](RFC-0082-containers-are-vyrn.md) | Containers Are Vyrn: `Array` Is the Primitive | Accepted; M1 shipped; M2 stopped at its own limit |
| [0083](RFC-0083-portable-simd.md) | Portable SIMD, Without `unsafe` and Without Breaking Parity | Accepted; M1–M4 shipped |
| [0084](RFC-0084-static-protocol-dispatch.md) | Protocol Dispatch Is Static Everywhere | M1 and M2 shipped |
| [0085](RFC-0085-graphql-execution.md) | Answering a GraphQL Query | M1, M2, M3, M4a shipped; M4b designed |
| [0086](RFC-0086-the-compiler-asks-the-type.md) | The Compiler Asks the Type | M1 and M3 implemented; M2 blocked |
| [0087](RFC-0087-memory-scenarios.md) | Every Memory Scenario, and What Handles It | Census, closed |
| [0088](RFC-0088-ownership-of-places.md) | Ownership of Places | Superseded by RFC-0089 |
| [0089](RFC-0089-mutable-value-semantics.md) | Mutable Value Semantics | Implemented |
| [0090](RFC-0090-one-model.md) | One Model: Values, and Nothing Else | Implemented |
| [0091](RFC-0091-the-container-protocols.md) | The Container Protocols | M1, M2, M3 implemented; M4 stopped |
| [0092](RFC-0092-a-projection-is-a-borrow.md) | A Projection Is a Borrow | Complete (M0–M5) |
| [0093](RFC-0093-a-take-is-a-move-out-of-a-place.md) | A Take Is a Move Out of a Place | M1 and M2 landed |
| [0094](RFC-0094-a-builtin-is-a-declaration.md) | A Builtin Is a Declaration | Complete (M1, M2, M3) |
| [0095](RFC-0095-a-task-is-owned.md) | A Task Is Owned | M1 and M3 built; M2 priced |
| [0096](RFC-0096-a-self-referring-type-declares-its-release.md) | A Self-Referring Type Declares Its Release | Complete (M1, M2, M3) |
| [0097](RFC-0097-von.md) | VON, Vyrn Object Notation | M0 and M1 shipped; M2–M4 not started |
| [0098](RFC-0098-cli.md) | `std/cli`: The Command Line Is a Record Type | M1 landed; M2–M7 stated |
| [0099](RFC-0099-a-generator-may-report-a-diagnostic.md) | A Generator May Report a Diagnostic | M1 landed; M2 shipped as RFC-0100; M3 unspent |
| [0100](RFC-0100-a-rule-is-a-library.md) | A Rule Is a Library | Implemented |
| [0101](RFC-0101-a-backend-is-an-emitter.md) | A Backend Is an Emitter | Implemented (M1) |
| [0102](RFC-0102-a-toolchain-is-a-dependency.md) | A Toolchain Is a Dependency | Implemented |
| [0103](RFC-0103-a-target-is-a-capability-set.md) | A Target Is a Capability Set | Implemented |
| [0104](RFC-0104-a-benchmark-is-a-claim-about-a-gap.md) | A Benchmark Is a Claim About a Gap | Implemented |
| [0105](RFC-0105-a-site-has-two-audiences.md) | A Site Has Two Audiences | Implemented |
| [0106](RFC-0106-a-consumer-page-is-scanned-not-read.md) | A Consumer Page Is Scanned, Not Read | Implemented |
| [0107](RFC-0107-a-template-component-is-a-library.md) | A Template Component Is a Library | Implemented |
| [0108](RFC-0108-the-string-scan-is-interpreted.md) | The String Scan Is Interpreted | Prototyped; its own question answered no |
| [0109](RFC-0109-a-read-that-does-not-copy.md) | A Read That Does Not Copy | Draft; three of four designs eliminated by measurement, the fourth not chosen |
| [0110](RFC-0110-a-lambda-takes-its-parameters-before-an-arrow.md) | A Lambda Takes Its Parameters Before an Arrow | Implemented; `x -> e`, `(a, b) -> e`, `() -> e` |
| [0111](RFC-0111-a-program-can-write-bytes.md) | A Program Can Write Bytes | Implemented; `writeFileBytes`, `writeStdout` — closes the mandelbrot gap |
| [0112](RFC-0112-a-regular-expression-that-searches.md) | A Regular Expression That Searches | Implemented; `std/regex` in Vyrn — closes the regex-redux gap |
| [0113](RFC-0113-bytes-takes-a-range.md) | `bytes` Takes a Range | Implemented; `slice` 57% of the site build to 9.2% |

## The other documents here

The rest of this directory is not RFCs. These are records of measurement and of
friction, and the RFCs above cite them. The table is checked for membership by
the same test as the index: a file here that nothing links to fails it.

| File | What it is |
|------|------------|
| [`PLAN-memory-model.md`](PLAN-memory-model.md) | The execution plan for the memory-model arc: ten phases, in the order they landed. Complete, and it records the chain that continued past it into RFC-0092 and RFC-0093. |
| [`NOTES-dogfood-bin.md`](NOTES-dogfood-bin.md) | Friction record from writing `examples/bin`, the pastebin that survives restarts — the first persistent app. |
| [`NOTES-dogfood-shelf.md`](NOTES-dogfood-shelf.md) | Friction record from writing `examples/shelf`, the full-stack app. |
| [`NOTES-dogfood-vlog.md`](NOTES-dogfood-vlog.md) | Friction record from writing `examples/vlog.vyrn`, the CLI and text app. |
| [`census-0106-m3-craft.md`](census-0106-m3-craft.md) | The adversarial re-audit of the consumer pages after RFC-0106 M3's first round was rejected: every defect found, and whether it was fixed or deferred. |
| [`census-strings.md`](census-strings.md) | How the fastest implementations build strings, and four Vyrn redesigns the measurements killed. Feeds RFC-0108. |
| [`census-builtins.md`](census-builtins.md) | Measurement of every reserved builtin name. Became RFC-0094. |
| [`census-call-arguments.md`](census-call-arguments.md) | Measurement of call-argument shapes, then implemented; §9 records what landed. |
| [`census-regions.md`](census-regions.md) | Measurement of `region { .. }` use. Its recommendation closed two rows of the builtins census. |

## Process

1. A change to language semantics starts as an edit to the relevant RFC.
2. Open questions get resolved by writing the smallest prototype that answers
   them, then recording the answer back in the RFC.
3. Only once an RFC section is accepted does it earn implementation effort.
4. When the work lands, the RFC's own status header is updated to say which
   milestones shipped and which did not. That header is what this index reads.
