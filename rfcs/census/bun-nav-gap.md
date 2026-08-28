# Census — Bun's documentation navigation versus Vyrn

This census compares every leaf entry in Bun's documentation navigation (114
entries across the Runtime, Package Manager, and Test runner sections) against
this repository, on 2026-08-23. For each entry, one researcher group fetched
Bun's page, searched `std/`, `compiler/`, `rfcs/`, `docs/`, `site/`,
`examples/`, and `bench/` (the RFC index at `rfcs/README.md` served as the
map), and filled one row. Repository claims cite `path:LINE`; external claims
cite a URL; each RFC claim names the RFC's own `**Status:**` header.

Two vocabulary notes. First, the four verdicts in the job definition describe
rows that need the owner's attention; sixteen entries are both covered by Vyrn
and already documented on the site, and those rows carry the extra verdict
`OK`. Second, where a researcher left a verdict inconsistent with its own
evidence (an `N/A` entry marked `UNDECIDED`, or a covered-and-documented entry
marked otherwise), the verdict was normalized to match the evidence; the status
and evidence columns are the researchers' words. One Bun page did not fetch
(`https://bun.com/docs/runtime/index.md`, HTTP 404); its row says so and judges
from the sibling `/docs/runtime.md` page, marked `(title only)`.

## Counts

| Status | Count |
|--------|-------|
| HAS | 17 |
| PARTIAL | 36 |
| NONE | 41 |
| N/A | 20 |

| Verdict | Count |
|---------|-------|
| OK | 16 |
| GAP | 31 |
| DOC GAP | 3 |
| NOT WANTED | 16 |
| UNDECIDED | 31 |
| N/A | 17 |

## The full table

Bun's own nav order, with its section headings.

# Runtime sub nav

## Get Started

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Welcome to Bun | Overview of Bun as an all-in-one JS/TS toolkit (runtime, package manager, test runner, bundler) plus runtime concepts and design goals | HAS | Vyrn's landing page carries the claim, a runnable program and the install command on one viewport (site/app/routes/index.vyx:5) with the language pitch at site/app/nav.vyrn:28; only the JavaScript-engine/Node.js explainer portion has no Vyrn counterpart | OK |
| Installation | Install the single dependency-free Bun binary via curl script, PowerShell, package managers, or Docker, plus upgrade, canary, older versions and PATH setup | HAS | Checksum-verifying curl and PowerShell installers at README.md:186-198, served as the `/install` route whose two commands are defined once in site/app/repo.vyrn:185-197, with build-from-source at README.md:218-230 | OK |
| Quickstart | Build a first app: `bun init`, `bun run`, a `Bun.serve` HTTP server, installing a package, and serving static HTML | HAS | The guide's Getting started chapter teaches install-then-run-one-file with all three backends at site/app/guide.vyrn:276-284, the index demo walks `vyrn new demo` then `vyrn run` (site/app/demo.vyrn:219), and an HTTP-server first app exists via `vyrn serve` (RFC-0016, Implemented per rfcs/README.md:75) | OK |
| TypeScript | Install `@types/bun` and set suggested `compilerOptions` so editors accept the `Bun` global, top-level await, JSX and `.ts` imports | N/A | The page is TypeScript-the-language tooling for Bun's JavaScript APIs; Vyrn is its own compiled language with its own type system (RFC-0002) and has no tsconfig or `@types` surface | N/A |
| TypeScript 6 and 7 | Add `"types": ["bun"]` to tsconfig because TypeScript 6/7 no longer auto-discover `@types/*` packages, fixing missing `Bun` global errors | N/A | A TypeScript-version-specific tsconfig migration with no Vyrn meaning: Vyrn compiles `.vyrn` directly and has no tsconfig or external type packages | N/A |
| bun init | Interactively scaffold an empty Bun project: package.json, tsconfig.json, entry point, README and AI-agent rules, then run `bun install` | HAS | `vyrn new <name>` scaffolds vyrn.json + src/main.vyrn + .gitignore at compiler/vyrn-cli/src/main.rs:670-693, stated in Implemented RFC-0010 (M1–M4, rfcs/README.md:69) at rfcs/RFC-0010-modules.md:120, and documented at site/app/guide.vyrn:626-627; unlike `bun init` there are no interactive template choices, only the blank scaffold | OK |
| bun create | Create a project from a React component, a `create-<template>` npm package, a GitHub repo, or local templates, with pre/post-install hooks and flags | NONE | No template-based scaffolding exists: searching template/scaffold/create across compiler/, rfcs/ and site/ finds only the blank `vyrn new`, while `vyrn add github:` pins remote modules into an existing manifest (Implemented RFC-0010, rfcs/README.md:69) rather than stamping out starter projects | UNDECIDED |

## Core Runtime

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Bun Runtime | Executing files with `bun run` (TS/JSX transpiled on the fly), package.json scripts with lifecycle hooks, monorepo `--filter`, stdin eval (`bun run -`), and flags like `--watch`, `--smol`, `--console-depth` (PAGE NOT FETCHED at /docs/runtime/index.md, HTTP 404; judged from the sibling /docs/runtime.md page, title only) | PARTIAL | `vyrn run [file.vyrn]` type-checks and interprets a file (compiler/vyrn-cli/src/main.rs:4) and the site documents it under "The CLI" (site/app/guide.vyrn:592); no script runner, workspace filtering, or stdin-eval mode exists anywhere in the CLI command table (compiler/vyrn-cli/src/main.rs:73) | UNDECIDED |
| Watch Mode | `--watch` (hard process restart on imported-file change using native FS watchers, restart-on-crash, works with `bun test`/`bun build`) and `--hot` (soft in-process reload preserving globals, live HTTP-handler swap without restarting the server) | NONE | NOT FOUND for watch/hot mode across compiler/, std/, site/guide/, rfcs/; no watch flag in the CLI usage table (compiler/vyrn-cli/src/main.rs:73); RFC-0016-server.md:146 lists "hot reload" under Out of scope as additive future work, not implemented | UNDECIDED |
| Debugging | The WebKit Inspector Protocol debugger via `--inspect`/`--inspect-brk`/`--inspect-wait`, the web inspector with breakpoints/console/scopes, VS Code debugging, request logging, and sourcemapped stack traces | PARTIAL | Compile-time diagnostics pin the exact token/line and the VS Code extension ships live diagnostics, hover, go-to-definition, and Run/test CodeLenses (editor/vscode/README.md:5-10); no runtime inspector or step debugger exists — trap output is prefix+message+newline with no file:line (compiler/vyrn-frontend/src/trap.rs:64-71) and no inspect-style attach protocol was found in compiler/ | GAP |
| REPL | Interactive TS/JS evaluation with top-level await, syntax highlighting, persistent history, tab completion, multi-line input, dot-commands, special variables, and `-e`/`-p` non-interactive eval | PARTIAL | The browser playground evaluates Vyrn in-tab through the wasm-compiled compiler front end with live error reporting (site/app/routes/play.vyx:128); no terminal REPL exists — `repl` appears nowhere as a CLI subcommand (compiler/vyrn-cli/src/main.rs:73) and is not found in compiler/, std/, or examples/ | UNDECIDED |
| bunfig.toml | Optional TOML configuration file: global vs local placement, runtime knobs (preload, jsx, smol, logLevel, define, loader, telemetry, env, console depth) and `[serve]`/`[test]` sections | PARTIAL | `vyrn.json` configures main, dependencies, toolchain, audience, artifacts and nativeTarget (compiler/vyrn-frontend/src/manifest.rs:122-143), documented on the site (site/app/guide.vyrn:626-627), and log thresholds are set per module via `logging { level }` (compiler/vyrn-codegen/src/direct.rs:1108-1110); no preload/smol/env-style runtime knobs exist | UNDECIDED |

## File & Module System

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| File Types | Loaders for `.js`/`.ts`/`.tsx`/`.jsx`/`.json`/`.toml`/`.yaml`/`.wasm`/`.css`/`.html` and more, executed or bundled directly | PARTIAL | Vyrn compiles `.vyrn` directly and compiles `.vyx` templates to modules while the program builds (site/app/guide.vyrn:535-538), and imports JSON Schema as validated types (`import type { User } from "./api.schema.json"`, rfcs/RFC-0010-modules.md:86-92, Status Implemented at :3); no loader mechanism for foreign file formats exists and runtime data files parse through the std codecs instead (std/json.vyrn) | UNDECIDED |
| Module Resolution | How import specifiers map to files: extension probing order, ESM/CommonJS interop, node_modules packages with package.json exports conditions, NODE_PATH, custom conditions | HAS | Specifier resolution is TS-style relative to the importing file with `.vyrn` appended, `"std/"` reserved for the standard library, bare specifiers resolved through the manifest import map, and `github:`/`gist:`/`https:` remotes through lock + cache (rfcs/RFC-0010-modules.md:36-44 and 110-123, Status Implemented M1–M4 at :3); the node_modules/CommonJS halves are npm-specific and have no Vyrn meaning; the dependency workflow is documented at site/app/guide.vyrn:624-633 | OK |
| JSX | Built-in JSX/TSX transpilation with configurable factory, fragment, and import-source options, pragma comments, component-tree pretty-printing, and prop punning | HAS | `.vyx` v2 templates are the designed counterpart: a script block of ordinary Vyrn plus a real parsed template block, compiled to a Vyrn module by std/vyx while the importing program builds (rfcs/RFC-0039-vyx-v2.md:3, Status Implemented), with compile-checked classes (RFC-0036); documented at site/app/guide.vyrn:533-546 and /docs/std/vyx.html | OK |
| Auto-install | With no node_modules present, Bun auto-installs every imported package on the fly into a global cache during execution, resolving versions from bun.lock, package.json, or latest | PARTIAL | Dependencies resolve automatically during a build with no install step: bare and inline remote specifiers resolve through the manifest map, and the first resolve fetches and pins into `~/.vyrn/cache/sha256/<hash>` (rfcs/RFC-0010-modules.md:117-119 and 142-156, Status Implemented at :3); the npm-registry/semver-latest half is deliberately absent — "a registry with semver resolution" is out of scope (rfcs/RFC-0010-modules.md:186-191) | NOT WANTED |
| Plugins | Universal plugin API extending runtime and bundler: onStart/onResolve/onLoad/onBeforeParse hooks intercept module resolution and loading, with namespaces and filters | PARTIAL | Generator imports are the deliberate answer: user code runs at compile time and synthesizes a module, and RPC, i18n, UI, OpenAPI and GraphQL are libraries over it, not compiler features (rfcs/RFC-0021-generator-imports.md:3, Status Implemented per rfcs/README.md:80); no hook intercepts arbitrary import resolution of existing files | UNDECIDED |
| File System Router | `FileSystemRouter` resolves Next.js-style pages directories against requests, returning filePath, params, and query | HAS | std/ui reads a `routes/` directory and generates one `route(req) -> Response` with a typed URL helper per route; a `[param].vyx` file binds a path segment (site/app/guide.vyrn:572-574), reference documented at /docs/std/ui.html | OK |

## HTTP server

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Server | Starting an HTTP server with `Bun.serve`: route/fetch handlers, port/hostname config, Unix domain sockets, experimental HTTP/3, idleTimeout, hot route reloading, lifecycle methods like `server.stop()` | HAS | RFC-0016 (Status: Implemented, rfcs/RFC-0016-server.md:3) ships `vyrn serve` with the `handle(req) -> Response` convention and `--port N`, and std/http.vyrn:852 provides `mount(...)` over buffered routes, SSE streams, and WebSockets; documented on the site at /docs/std/http (site/app/docs.vyrn:65) | OK |
| Routing | Defining routes in `Bun.serve`: static paths, `:param` and wildcard patterns, specificity precedence, typed params, static/file/directory responses, and a fallback fetch | PARTIAL | std/http.vyrn:118 defines `Route` with compile-time-checked `{id}` placeholders (std/http.vyrn:287), prefix `surface` mounting as catch-all (std/http.vyrn:254), first-match `mount` with shadow traps (std/http.vyrn:1386); wildcard segments inside a pattern and static-file/directory-serving routes were NOT FOUND in std/http.vyrn | UNDECIDED |
| Cookies | `BunRequest.cookies`, a CookieMap for reading, setting (maxAge/httpOnly/secure/path), and deleting cookies whose changes auto-apply to the response | NONE | NOT FOUND — cookie/set-cookie searched across std/, examples/, site/, docs/, rfcs/; docs/research/vyx-hints.md:376 records "cookie flags … hole — nothing in std/http sets them", Request/Response carry no header access (rfcs/RFC-0016-server.md:59-61), and cookies appear only as future additive work (rfcs/RFC-0016-server.md:143) | GAP |
| TLS | Enabling TLS in `Bun.serve`: key/cert contents, passphrase, custom CA list, Diffie-Hellman params, and SNI with multiple certificates | NONE | NOT FOUND — tls/certificate/rustls greps over std/, compiler/, rfcs/ find no server TLS; RFC-0016's host is plain Rust std::net HTTP (rfcs/RFC-0016-server.md:80-81) and lists TLS as additive future work (rfcs/RFC-0016-server.md:143) | GAP |
| Error Handling | Development-mode in-browser error pages and an `error` callback returning a custom Response when the server throws | PARTIAL | A trap inside `handle` is caught, logged to stderr, and answered with a 500 (rfcs/RFC-0016-server.md:91-94); app-level error pages exist as vyx components (examples/shelf/server/routes/error.vyx:3); no user hook returns a custom error Response from the server host itself | UNDECIDED |
| Metrics | Built-in activity counters: `server.pendingRequests`, `server.pendingWebSockets`, and `subscriberCount(topic)` for WebSocket topics | NONE | NOT FOUND — pendingRequests/subscriberCount/metrics/pending searched across std/, compiler/vyrn-cli/src/, rfcs/, site/; closest surfaces are the per-request stderr access line (rfcs/RFC-0016-server.md:96-97) and hand-rolled module-state counters (rfcs/RFC-0016-server.md:29-37) | UNDECIDED |

## Networking

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Fetch | WHATWG fetch for outbound HTTP with extensions for proxying, streaming bodies, timeouts/abort, Unix domain sockets, TLS client certs, and s3:// and file:// protocols | PARTIAL | Outbound HTTP exists today only as generated protocol clients over host externs — `extern fn vyrnRpcCall` (std/rpc.vyrn:520) and `extern fn vyrnConnectCall` (std/connect.vyrn:362) — with no general-purpose fetch of arbitrary URLs; RFC-0016 (Status: Implemented) explicitly lists "outbound HTTP client" among the not-yet-built follow-ups (rfcs/RFC-0016-server.md:145) | GAP |
| WebSockets | Server- and client-side WebSockets: publish/subscribe, message handlers, compression, per-socket context | PARTIAL | Server-side WebSocket ships as `ws(pattern, feed)` (std/http.vyrn:614), part of RFC-0074 (Status "M1, M2, M3a, M3b and M4a shipped", rfcs/RFC-0074-protocol-projections.md:3), published at /docs/std/http; no WebSocket CLIENT and no inbound client-to-server messages exist, which RFC-0074 declines as "a different design … not one this RFC spells" (rfcs/RFC-0074-protocol-projections.md:657-658) | GAP |
| TCP | Raw TCP client/server sockets via `Bun.listen` and `Bun.connect` with TLS, hostname resolution, and socket event handlers | NONE | NOT FOUND — tcp/socket searches over std/, rfcs/, site/, docs/, examples/, compiler/ surface only internal serve-runtime details (rfcs/RFC-0074-protocol-projections.md:679, RFC-0098 Port type at rfcs/RFC-0098-cli.md:40); no user-facing TCP API exists | GAP |
| UDP | `Bun.udpSocket()` bound datagram sockets with send/sendMany batching, connected sockets, drain backpressure, TTL/broadcast options, and multicast | NONE | NOT FOUND — udp/datagram searches over std/, rfcs/, site/, docs/, examples/, compiler/ return zero matches | GAP |
| DNS | A dns module plus node:dns compatibility: record resolution, resolver backend selection, automatic caching with configurable TTL, prefetch, and cache stats | NONE | NOT FOUND — dns/getaddrinfo/resolver searches over std/, rfcs/, site/, docs/, examples/, compiler/; the sole hit is a loopback-host comment in the dev-server host check (compiler/vyrn-cli/src/main.rs:4743) | GAP |

## Data & Storage

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Cookies | Native APIs for working with HTTP cookies (parse, generate, mutate on requests and responses) | NONE | Same surface as the HTTP-server Cookies row above; the only repository trace is the deferral note at rfcs/RFC-0016-server.md:143, which schedules cookies after the server's v1 scope rather than declining them | UNDECIDED |
| File I/O | `Bun.file`/`Bun.write` optimized reads/writes (text/json/bytes/stream), `FileSink` incremental writer, delete, plus readdir/mkdir | PARTIAL | Text-file IO exists today: RFC-0014 (Status: Implemented, rfcs/RFC-0014-input-io.md:3) gives readFile/writeFile/listDir, and std/storage.vyrn:51 adds atomic `writeAtomic` and `fsyncFile`; `writeFile` takes a String only (no binary writes — site/export.vyrn:346,557 route around it) and there is no mkdir/delete/lazy-file-handle surface (site/app/nav.vyrn:95 "no way to CREATE a directory") | GAP |
| Streams | Web ReadableStream/WritableStream including direct streams with backpressure, async-generator bodies, and an incremental byte sink | PARTIAL | Lazy pull streams with combinators exist: std/stream.vyrn:1 (`Stream<T>` unfold/map/filter/take/merge), RFC-0075 (Status: Implemented, rfcs/RFC-0075-streams.md:3), documented at site/app/docs.vyrn:64; the byte-oriented side is missing — no Readable/WritableStream over IO, no sink or high-water-mark backpressure API | GAP |
| Binary Data | TypedArrays, DataView, Buffer, Blob/File cheat sheet plus base64/hex conversions | PARTIAL | Byte arrays are first-class: RFC-0057 byte literals (Status: Implemented, rfcs/RFC-0057-byte-literals.md:3), `Array<UInt8>` throughout, hex/base64 encode-decode plus urlEncode at std/codecs.vyrn:93,164,253, and floatBits/floatFromBits builtins for multi-byte access; no shared-buffer view family (DataView-over-offset, Blob) | UNDECIDED |
| Archive | Reading and writing zip archives and compressed payloads (gzip/deflate) | NONE | NOT FOUND — zip/tar/archive/gzip/deflate across std/, rfcs/, compiler/, site/ hit only toolchain release archives; the repo states it outright: site/export.vyrn:807 "RFC-0014 has readFile, writeFile and listDir, and no compressor behind any of them" | GAP |
| SQL | Unified tagged-template client for PostgreSQL, MySQL, and SQLite with pooling, transactions, and prepared statements | NONE | No database driver exists: sqlite/postgres/mysql binding searches return nothing; injection-safe tagged templates build query text with `$N` params but there is no way to execute them (examples/tagged.vyrn:13, rfcs/RFC-0007-string-templates.md:21) | GAP |
| SQLite | Built-in synchronous SQLite3 driver: Database, prepared/cached statements, parameters, transactions, WAL, serialize | NONE | NOT FOUND — sqlite across std/, rfcs/, site/, examples/ returns zero hits beyond prose about future SQL schema generators (rfcs/RFC-0021-generator-imports.md:166) | GAP |
| S3 | Native S3-compatible object storage client: lazy file refs, upload/download/streaming multipart, presigned URLs, ACLs | NONE | NOT FOUND — s3/presign/object-storage searches across the repo return nothing | GAP |
| Redis | Native Redis client: string/hash/set commands, TTL, auto-pipelining, raw commands, Pub/Sub | NONE | NOT FOUND — redis hits are only the word "rediscover"; no TCP client story beyond std/connect and no RESP protocol anywhere | GAP |

## Concurrency

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Workers | The Web Workers API: `new Worker(url)` on a separate thread, postMessage messaging, terminate, open/close events, ref/unref lifetime control, environment data sharing | PARTIAL | Vyrn has real-thread parallelism but not a message-passing Worker API: RFC-0025 (Status: Implemented, rfcs/RFC-0025-worker-threads.md:3) gives `spawn f(args) -> Task<T>`/`join` lowered to native threads with checker-proven isolation, and `vyrn serve --workers N` serves in parallel (compiler/vyrn-cli/src/main.rs:4201,4256-4260); missing: postMessage, terminate, ref/unref, blob-URL workers, environment-data sharing, and any site documentation of spawn/join (only a gen-fn prohibition note, site/app/guide.vyrn:463) | DOC GAP |

## Process & System

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Environment Variables | Automatic `.env` loading (precedence, quoting, expansion), programmatic read/write via `process.env`, `--env-file` flags, and Bun-specific config variables | NONE | Programs have no environment access: RFC-0014 (Status: Implemented M1+M2) lists "environment variables (trivial to add later)" as future work (rfcs/RFC-0014-input-io.md:132-134), and RFC-0098 (Status: M1 landed) records "there is no environment-variable builtin at all" as its unlanded M4 blocker (rfcs/RFC-0098-cli.md:350-352); grep for getEnv/hostEnv/envVar across std/ and compiler/ finds only compiler-internal VYRN_* tooling vars (compiler/vyrn-cli/src/main.rs:80) | GAP |
| Shell | Bun Shell, a cross-platform bash-like interpreter embedded in the runtime: template literals, pipes, redirection, globs, command substitution, safe escaping, and JS-object stdin/stdout interop | NONE | NOT FOUND — spawn/exec/shell/process searches over std/ and rfcs/ surface only compiler-internal shelling out to curl/git/tar/wasmtime (rfcs/RFC-0010-modules.md:163-164, rfcs/RFC-0102-a-toolchain-is-a-dependency.md:300) and the docs-site syntax highlighter (site/app/code.vyrn:180), neither of which runs shell commands from a Vyrn program | GAP |
| Spawn | `Bun.spawn`/`Bun.spawnSync`: child processes with cwd/env options, pipeable stdio streams, exit handling, kill signals, timeouts, and an IPC channel | NONE | Vyrn's `spawn` keyword creates pure parallel tasks only — "no effects, no module state, no I/O" so output stays byte-identical under any schedule (rfcs/RFC-0025-worker-threads.md:15-18, Status: Implemented) — and no OS child-process builtin exists anywhere; generators are forbidden to spawn by design (site/app/guide.vyrn:463) | GAP |
| WebView | `Bun.WebView`, an experimental headless browser (WKWebView on macOS, Chrome DevTools Protocol elsewhere) for navigation, JS evaluation, input simulation, screenshots, and profiles | NONE | NOT FOUND — webview searches across std/, compiler/, rfcs/, and site/ return nothing; the closest surfaces are std/ui SSR pages and the browser-side differ, which serve pages to a browser rather than drive one | GAP |
| Cron | 5-field cron expression parsing with nicknames and time zones, in-process scheduled callbacks with a stoppable handle, and OS-level registration | NONE | NOT FOUND — cron/schedul/timer/sleep searches over rfcs/, std/, and site/ find only SHA message schedules and host-owned event-loop prose (rfcs/RFC-0016-server.md:24-26); std/time.vyrn exposes clock reading only (`now()` at std/time.vyrn:37, RFC-0043 Status: Implemented) with no timer, sleep, or scheduler primitive | GAP |

## Interop & Tooling

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Node-API | Node-API implemented from scratch so existing `.node` native add-ons load via require() or dlopen | N/A | About Node.js/npm add-on compatibility; Vyrn has no Node runtime or npm ecosystem (modules are git/manifest imports per RFC-0010), and node-api/dlopen searches over std/, compiler/, rfcs/ return nothing | N/A |
| FFI | `bun:ffi` dlopens C-ABI shared libraries with typed symbols, CString marshalling, and function pointers/callbacks | PARTIAL | RFC-0012 (Status "M1 + M2 + M3 implemented", rfcs/RFC-0012-js-interop.md:3) gives `extern fn` imports and `export extern fn` exports across the wasm-JS boundary (syntax at :23); no native shared-library path exists (dlopen/libloading NOT FOUND in std/, compiler/, rfcs/), no C-ABI structs/buffers, and only String plus scalar types cross (ABI table :69) | GAP |
| C Compiler | `cc()` embeds TinyCC to compile C source at runtime and link its symbols into the process | NONE | NOT FOUND — tinycc/runtime-C-compilation searches in std/, compiler/, rfcs/ return nothing; Vyrn's only C-toolchain contact is `vyrn build` invoking external clang on emitted IR (compiler/vyrn-cli/src/main.rs:12, compiler/README.md:158-168) — a build step, never callable from a running Vyrn program | UNDECIDED |
| Transpiler | `Bun.Transpiler` transforms TS/JSX source into vanilla JS and scans a source's import/export lists | N/A | JavaScript/TypeScript tooling: Vyrn compiles `.vyrn` directly to interpreter/native/wasm (compiler/README.md:4-5) and exposes no JS-transpilation API; the nearest analogues are compile-time moduleInterface reflection and generators (RFC-0021) | N/A |

## Utilities

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| CSRF Protection | HMAC-signed, expiring, session-bound tokens (generate/verify) protecting form submissions | NONE | NOT FOUND — csrf searches across std/, rfcs/, site/, docs/, examples/ return nothing | GAP |
| Secrets | Store/retrieve/delete credentials in OS keychains (Keychain, libsecret, Windows Credential Manager) | NONE | NOT FOUND — secret/keychain/credential/keyring searches; RFC-0103 (:31) states "The compiler does not know what a secret is" | UNDECIDED |
| Console | console object inspection depth (`--console-depth`) and console as an async iterable over stdin lines | HAS | Leveled logging shipped in part (RFC-0008, Status "Implemented in part", rfcs/README.md:67) and stdin line reading is implemented (RFC-0014, rfcs/README.md:73); no depth knob exists but print/readLine cover the surface | DOC GAP |
| TOML | `TOML.parse`/`stringify` plus import-as-module for TOML 1.1 files | NONE | No TOML parser anywhere; docs/research/von.md:29 lists TOML among formats rejected in favor of VON (RFC-0097, "M0 and M1 shipped", rfcs/README.md:155) | NOT WANTED |
| YAML | `YAML.parse` for YAML 1.2 including anchors/tags/multi-doc, plus module import | NONE | No YAML parser; std/von.vyrn:22-27 deliberately refuses YAML semantics (barewords, leading-zero octal) and docs/research/von.md:28 rejects the format; VON (RFC-0097) is the answer | NOT WANTED |
| Markdown | GFM parser with options and per-element render callbacks producing HTML/React output | PARTIAL | site/app/markdown.vyrn:1-17 renders a measured finite GFM subset to HTML, but it is the site's internal module (unknown constructs are build errors), not a public std API; `vyrn doc` emits Markdown (RFC-0065, Status Implemented, rfcs/README.md:124); no callback-based rendering | UNDECIDED |
| JSON5 | `JSON5.parse`/`stringify` plus module import for JSON5 (comments, trailing commas, unquoted keys) | NONE | No JSON5 support; the JSON codec is strict (RFC-0018, rfcs/README.md:77); docs/research/von.md:29 groups JSON5 among rejected formats, and VON answers its comment/trailing-comma pain | NOT WANTED |
| XML | `XML.parse`/`stringify` for XML 1.0 in compact/tree shapes, plus module import | NONE | NOT FOUND — no XML module in std/; site/app/feed.vyrn:50 hand-writes RSS XML strings and site/test/feed.test.mjs:6 hand-writes an XML reader for tests | UNDECIDED |
| JSONL | Streaming newline-delimited JSON parser with chunk parsing and byte-offset resume | PARTIAL | examples/vlog.vyrn:1-3 processes NDJSON line-by-line with split+fromJson (dogfooded without friction, rfcs/NOTES-dogfood-vlog.md:20-27); no shared streaming parser or chunk-resume primitive (jsonl/ndjson NOT FOUND in std/) | UNDECIDED |
| HTMLRewriter | CSS-selector-driven HTML transformation: element/text/comment handlers, streamed via Response | NONE | NOT FOUND — htmlRewriter searches return nothing; std/html builds a hyperscript tree (std/html.vyrn, RFC-0026) rather than rewriting arbitrary HTML streams | UNDECIDED |
| Image | Chainable decode/resize/rotate/encode pipeline for JPEG/PNG/WebP/HEIC/AVIF | NONE | NOT FOUND — image searches across std/, rfcs/, site/, docs/; no image decode/encode capability in any module or RFC | UNDECIDED |
| Hashing | Password hashing/verification (argon2/bcrypt), non-crypto hashes (wyhash/crc32/xxhash), incremental CryptoHasher with HMAC | PARTIAL | std/hash.vyrn:18 FNV-1a-64 and :55 SHA-1 (handshake-only) exist, but :43-49 state "Vyrn ships no cryptographic hash … must not hash a password"; no argon2/bcrypt/HMAC anywhere | GAP |
| Glob | Pattern file matching (`**`, `[ab]`, `{a,b}`, negation) with dot/symlink options | PARTIAL | The `listDir` builtin lists one flat directory (std/ui.vyrn:1344) but only under `vyrn run`/generation — every compiled target refuses it (rfcs/RFC-0103-a-target-is-a-capability-set.md:102) — and no pattern matching exists (glob NOT FOUND) | GAP |
| Semver | node-semver-compatible range checks (`satisfies`) and version ordering | NONE | NOT FOUND — no semver code; RFC-0010-modules.md:190 notes the lock "pins exact content, not version ranges", so the module system never needed range resolution | UNDECIDED |
| Color | Parse CSS colors and convert to css/hex/number/rgb/hsl/ANSI-16/256/16m output formats | NONE | NOT FOUND — ansi/colorize searches across std/; std/tw only compiles Tailwind utility classes into stylesheets, with no color-parsing/formatting API | UNDECIDED |
| Utils | Grab-bag: version/revision/env/main, sleep, which, UUID generation, peek, deepEquals | PARTIAL | Args (RFC-0061, rfcs/README.md:120), monotonic and wall clocks (std/time.vyrn:37, RFC-0043), and randomness (std/random.vyrn) exist; NO environment-variable access, no timers/sleep, no PATH lookup (env/sleep/which NOT FOUND in std/) | GAP |

## Standards & Compatibility

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Globals | Every global object the Bun runtime exposes (Web/Node/Bun globals such as `Bun`, `fetch`, `process`, `console`, `AbortController`) | N/A | A JS-runtime ambient global-object surface has no Vyrn meaning — Vyrn deliberately exposes almost nothing ambient and moves builtins to explicit imports (RFC-0062, Status: Implemented, rfcs/RFC-0062-explicit-builtins.md:3; RFC-0094 M2 moved sixteen names out of the global namespace, Status: Complete, rfcs/RFC-0094-a-builtin-is-a-declaration.md:3) | NOT WANTED |
| Bun APIs | Overview of Bun's native APIs: HTTP server, shell, file I/O, child processes, TCP/UDP sockets, WebSockets, hashing, SQLite, FFI, DNS, compression, utilities | PARTIAL | The server side maps well: std/http serves routes/SSE/server-push WebSockets (std/http.vyrn:572,606), std/hash gives FNV-1a/SHA-1 (std/hash.vyrn:17), std/stream gives Stream<T> (std/stream.vyrn:1), file IO exists as readFile/writeFile plus atomic writes (std/storage.vyrn:58), time/random/codecs shipped (RFC-0043); the whole outbound half is missing — no HTTP client, TCP/UDP connect, SQLite, native FFI, or DNS (hostFetch/netConnect/hostSocket NOT FOUND across compiler/) | GAP |
| Web APIs | Web-standard APIs implemented server-side: fetch/Request/Response, URL, Workers, Streams, Blob, WebSocket, encoding, crypto, timeouts/intervals, console/performance, events | PARTIAL | Exists: server-side Request/Response types (std/http.vyrn:83), WebSocket but explicitly server-push only (std/http.vyrn:574), Stream<T> (std/stream.vyrn:1), base64/hex/url codecs (std/codecs.vyrn:253); missing: outbound fetch client, URL parser (only urlEncode/urlDecode, std/codecs.vyrn:253), cryptographic hashes (std/hash.vyrn:47 "Vyrn ships no cryptographic hash"), and timers — std/http.vyrn:587 states the event-loop design leaves "no moment for a timer to fire in" | GAP |
| Node.js Compatibility | Per-module status of compatibility with Node.js builtin modules and globals, run against Node's test suite | N/A | Compatibility with npm/Node packages has no Vyrn meaning: Vyrn compiles ahead-of-time with its own module system (RFC-0010, Status: Implemented M1–M4) and its vision explicitly declines the JS-runtime niche (rfcs/RFC-0001-vision.md:32, "Vyrn is native, ahead-of-time") | NOT WANTED |

## Contributing

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Roadmap | Bun's long-term roadmap and priorities hosted as a GitHub issue | HAS | ROADMAP.md:1 ("Vyrn — status & roadmap"), linked from the site's Record section at site/app/routes/docs/index.vyx:68 | OK |
| Benchmarking | How to benchmark Bun: timing APIs, mitata/hyperfine/bombardier tools, heap stats, profiling flags | HAS | RFC-0055 "Benchmarking: `bench` Blocks, `blackBox`, `vyrn bench`", Status Implemented (rfcs/README.md:114); harness in std/bench.vyrn:1-6; documented at site/app/guide.vyrn:613-616, demo site/guide/benches.vyrn:13, and the /benchmarks route (site/app/routes/benchmarks.vyx:2) | OK |
| Contributing | Setting up a dev environment for Bun itself: dependencies, pinned Rust/LLVM toolchain, codegen scripts, CI tooling | N/A | Bun-project contributor workflow with no Vyrn meaning — Vyrn's compiler is an ordinary Rust cargo workspace | N/A |
| Building Windows | MSVC/Scoop prerequisites and Linux cross-compile recipes for building Bun.exe on Windows | N/A | Purely Bun's own build internals; nothing in Vyrn corresponds to building a JS-runtime binary for Windows | N/A |
| Bindgen | Bun's internal schema generator emitting C++ thunks and Rust glue between JavaScriptCore and native code | N/A | Bun-maintainer implementation detail; Vyrn's closest surface (RFC-0012 wasm extern host imports) is a language feature, not this generator | N/A |
| License | Bun is MIT-licensed, explains the LGPL-2 JavaScriptCore obligation, and tabulates linked libraries' licenses | HAS | Dual MIT/Apache-2.0 licensing declared in README.md:324-328 with LICENSE-MIT and LICENSE-APACHE at repo root; contributions dual-licensed by default (README.md:336-338) | OK |

# Package Manager sub nav

## Core Commands

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| bun install | Installing all declared dependencies into node_modules, writing bun.lock, lifecycle scripts, workspaces/--filter, frozen-lockfile CI mode, offline installs | PARTIAL | Manifest dependencies resolve automatically during a build and the first resolve writes vyrn.lock (RFC-0010, Status Implemented M1–M4, rfcs/RFC-0010-modules.md:148-156); `--offline` turns a lock+cache miss into a hard error (compiler/vyrn-cli/src/main.rs:77-78) and `vyrn update --locked` is the pinned-CI fetch (site/app/guide.vyrn:633); no explicit install command and no lifecycle-script machinery exist | UNDECIDED |
| bun add | Adding a named npm package at a version/range/tag to package.json and installing it | HAS | `vyrn add <specifier> [--name alias]` fetches, pins sha256 into vyrn.lock and records the dependency (rfcs/RFC-0010-modules.md:155-156; dispatch compiler/vyrn-cli/src/main.rs:364-366), documented at /tooling/projects (site/app/guide.vyrn:630) with each package page printing its own add line (site/app/packages.vyrn:419-420); npm-style ranges have no counterpart by design (compiler/vyrn-cli/src/remote.rs:8-9) | OK |
| bun remove | Removing a package from every dependency group, updating bun.lock, deleting it from node_modules | NONE | NOT FOUND — remove/uninstall/prune/clean dispatch searches over compiler/vyrn-cli/src return nothing; dependency commands are new/add/update/vendor/deps only (compiler/vyrn-cli/src/main.rs:75); dropping a dependency means hand-editing vyrn.json | UNDECIDED |
| bun update | Updating direct and transitive dependencies to the newest versions their ranges allow, rewriting manifests and lockfile | HAS | `vyrn update [--locked] [alias]` re-resolves a pin and is the only command that changes one (rfcs/RFC-0010-modules.md:155-156; compiler/vyrn-cli/src/main.rs:367-371), documented at /tooling/projects (site/app/guide.vyrn:630-633); range-widening does not exist because refs resolve once and freeze by design (compiler/vyrn-cli/src/remote.rs:8-9) | OK |
| bun dedupe | Collapsing duplicate locked versions of one package onto the smallest satisfying set | N/A | Duplicate versions are an artifact of npm range resolution; Vyrn has no version ranges and no transitive version graph — one pin per specifier and identical bytes share one content-addressed cache entry (rfcs/RFC-0010-modules.md:148-154) — so duplicates cannot arise | N/A |
| bun prune | Deleting node_modules entries the current lockfile would not install | NONE | NOT FOUND — no prune/clean/cache-removal subcommand exists and there is no node_modules to clean; dependencies live in `~/.vyrn/cache/sha256` plus optional vyrn_vendor (compiler/vyrn-cli/src/remote.rs:10-12), which can grow stale but never blocks a build | UNDECIDED |
| bunx | Auto-installing and running npm package executables from a global cache | N/A | There is no npm registry or package-binary ecosystem to run from — "No registry server and no account — a dependency is a URL" (site/app/routes/explore/index.vyx:76); programs ship as compiled artifacts, not installed package bins | N/A |

## Publishing & Analysis

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| bun publish | Packs a package into a tarball and publishes it to the npm registry, with access/tag/dry-run/OTP options | NONE | Publishing presupposes a registry; RFC-0010 keeps "a registry with semver resolution" out of scope (rfcs/RFC-0010-modules.md:186-191) and the site says "No registry server and no account" (site/app/routes/explore/index.vyx:76); publish/pack commands NOT FOUND | NOT WANTED |
| bun outdated | Table of dependencies with newer versions available (Current/Update/Latest) with name globs and filters | NONE | No version ranges exist to compare — the lock pins exact content by sha256 (rfcs/RFC-0010-modules.md:148-150) and semver resolution is declined (:189-190); `vyrn update [alias]` moves a pin but nothing checks for newer versions (outdated/latest/semver NOT FOUND) | NOT WANTED |
| bun why | Explains why a package is installed by printing the dependency chain, with top/depth filters | PARTIAL | `vyrn deps` prints the resolved module graph (rfcs/RFC-0010-modules.md:120-121), from which chains are visible; no per-package chain explanation, glob filter, or depth control exists | UNDECIDED |
| bun audit | Checks installed packages for known security vulnerabilities via the npm advisory endpoint, with optional auto-fix | NONE | No vulnerability/advisory story exists (audit/security/advisory/CVE searches return only code-review audits); the nearest concept is tamper detection via sha256 verification on every load (rfcs/RFC-0010-modules.md:151-154) — integrity checking, not advisories | UNDECIDED |
| bun info | Displays npm registry package metadata — versions, description, homepage, dependencies — with per-property and JSON output | PARTIAL | The site's /explore package pages list each package's specifier, alias, exports and exact `vyrn add` line (site/app/packages.vyrn:58-61,419-420); no CLI command queries a package's metadata or emits JSON | UNDECIDED |

## Workspace Management

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Workspaces | Monorepo development via a workspaces glob key, cross-package references, hoisted de-duplicated installs, and script running across packages | NONE | Manifest schema has no workspaces concept — fields are main, dependencies, toolchain, audience, artifacts, nativeTarget only (compiler/vyrn-frontend/src/manifest.rs:114-144); RFC-0049 defers "multi-root workspaces" for LSP owner discovery (rfcs/RFC-0049-vyx-owner-discovery.md:99); nearest capability is multiple artifacts in one manifest (compiler/vyrn-cli/tests/project.rs:202-206) | UNDECIDED |
| Catalogs | Shared dependency versions defined once in root catalog maps, referenced via `catalog:` protocols, lockfile-integrated | NONE | NOT FOUND — catalog searches return only unrelated uses; vyrn.json dependencies map alias-to-specifier directly (compiler/vyrn-frontend/src/manifest.rs:123) with no version-sharing indirection, and versions are immutable sha256 pins (compiler/vyrn-cli/src/remote.rs:6-8), not ranges to centralize | UNDECIDED |
| bun link | Registers a local package globally so other projects can symlink it into node_modules | PARTIAL | Consuming a local directory as a dependency works today via a relative path in vyrn.json's dependencies (compiler/vyrn-cli/tests/project.rs:60); no global register/link/unlink command exists (CLI subcommand set at compiler/vyrn-cli/src/main.rs:75) | UNDECIDED |
| bun pm | npm-facing utilities: pack, bin, ls, licenses, diff, whoami | PARTIAL | Dependency introspection exists as `vyrn deps [artifact]` printing every artifact's resolved module graph plus pinned toolchain rows (compiler/vyrn-cli/src/main.rs:34-41); pack/licenses/diff/whoami presuppose an npm registry and node_modules layout Vyrn lacks (site/app/routes/explore/index.vyx:76) | N/A |

## Advanced Configuration

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| bun patch | Persistently patches node_modules packages via commit-able patch files tracked in patchedDependencies | N/A | Presupposes npm node_modules packages; Vyrn remotes are immutable sha256-pinned sources (rfcs/RFC-0010-modules.md:148-150) where forking or `vyrn vendor` covers the need (rfcs/RFC-0010-modules.md:157-159) | N/A |
| bun --filter | Selects monorepo workspace packages by name/path glob or dependency relation for install/run/outdated | N/A | npm/yarn workspaces concept with no Vyrn meaning — the manifest has no workspace or monorepo surface (workspace/monorepo/filter NOT FOUND in compiler/vyrn-frontend/src/manifest.rs and compiler/vyrn-cli/src/main.rs) | N/A |
| Global cache | Every downloaded package stored once in a global cache and hardlinked/cloned into projects | HAS | The same design shipped: content-addressed cache at `~/.vyrn/cache/sha256/<hash>`, hash-verified on every load, one shared entry for identical bytes (rfcs/RFC-0010-modules.md:151-154, Status Implemented M1–M4 at :3); documented at site/app/guide.vyrn:451 | OK |
| Global virtual store | Off-by-default mode where package files live once globally and projects hold thin symlinks | N/A | node_modules layout optimization with nothing to dedupe — fetched content is already deduplicated globally by sha256 across all projects (rfcs/RFC-0010-modules.md:152-154) | N/A |
| Isolated installs | pnpm-style linker preventing phantom dependencies via a central store plus symlinks | N/A | Solves phantom deps created by hoisted node_modules; Vyrn has no ambient lookup — every import is an explicit specifier and remote modules are sandboxed to their pinned base with no bare-specifier escapes (rfcs/RFC-0010-modules.md:160-162), so isolation holds by construction | N/A |
| Lockfile | bun.lock text format and configuration, migration from older lockfiles, opt-out flags | HAS | vyrn.lock pins `specifier / immutable-url / sha256`, sorted and diff-friendly by design (rfcs/RFC-0010-modules.md:148-150, Status Implemented at :3; compiler/vyrn-frontend/src/manifest.rs:271-272), documented at site/app/guide.vyrn:624 and site/app/packages.vyrn:434-437 | OK |
| Lifecycle scripts | Default-secure handling: lifecycle scripts run only for allow-listed packages | N/A | npm packages run shell code at install time; Vyrn dependencies are compiled source files and its tools arrive as sha256-verified pinned archives with no script step (compiler/vyrn-codegen/src/toolpin.rs:5-8), so there is nothing to gate | N/A |
| Scopes and registries | Default registry plus per-scope private registries with credentials in config | N/A | Scoped npm registries have no Vyrn meaning; RFC-0010 declines the direction: "a registry with semver resolution" is out of scope (rfcs/RFC-0010-modules.md:186-191) | NOT WANTED |
| Overrides and resolutions | Force metadependency versions via npm overrides / Yarn resolutions | N/A | Metadependencies and semver ranges do not exist — each specifier pins exact content (rfcs/RFC-0010-modules.md:148-150), and registry machinery is out of scope (:188-190) | NOT WANTED |
| Security Scanner API | Plugin hook running npm-distributed scanners over packages before install, reporting advisories and cancelling installs | N/A | Vyrn installs no executable packages and authenticates every remote fetch against its vyrn.lock sha256 pin, refusing tampered content loudly (compiler/vyrn-cli/src/remote.rs:6-9,175-179); no scanner plugin surface exists | N/A |
| .npmrc support | Layers npm .npmrc files (registries, scopes, tokens, cache, linker) into config below bunfig.toml | N/A | Pure npm-config compatibility — project configuration is vyrn.json + vyrn.lock (compiler/vyrn-frontend/src/manifest.rs:1-2), and npmrc is NOT FOUND across compiler/, site/, std/, rfcs/, docs/ | N/A |

# Test runner sub nav

## Getting Started

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Test runner | Built-in Jest-compatible runner: recursive discovery of `*.test.*` files, name-pattern filtering, CI annotations, JUnit output, per-test timeouts, retries, bail, watch mode, lifecycle hooks | HAS | RFC-0015 (Status: Implemented, rfcs/RFC-0015-testing.md:3): `test_cmd` runs a root file's `test` blocks with `--name <substring>` filtering and exit 1 on failure (compiler/vyrn-cli/src/main.rs:3490-3501), documented at site/app/guide.vyrn:609-612 leading to /tooling/testing | OK |
| Writing tests | Jest-like authoring API: test/describe suites, async tests and done callbacks, timeouts, retry/repeats, skip/todo/only/if modifiers, parametrized test.each | PARTIAL | Top-level `test "name" { body }` declarations with builtins assert/assertEq usable inside test bodies, plus `--name` substring filter (rfcs/RFC-0015-testing.md:32-43,60, exercised in examples/testing.vyrn:21-35); missing: describe grouping, timeouts, retries, all modifiers, test.each — expected-trap tests, fixtures/setup-teardown, parallelism, and snapshots are listed under Out of scope (rfcs/RFC-0015-testing.md:101-106) | NOT WANTED |
| Test configuration | Runner configuration through a `[test]` section: discovery root, preload, ignore patterns, JUnit reporter, concurrency globs, seed, retry, coverage thresholds | PARTIAL | The only knob is the CLI `--name <substring>` filter (compiler/vyrn-cli/src/main.rs:3498-3501) plus manifest-aware defaulting to vyrn.json's main (rfcs/RFC-0015-testing.md:61-62); no config section, coverage, reporter, preload, timeout, or retry keys exist, and most of these configure what RFC-0015 marks out of scope (:101-106) | NOT WANTED |

## Test Execution

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Runtime behavior | Runtime integration: env defaults, per-test/global/infinite timeouts, unhandled-error tracking with non-zero exits, flag integration, exit codes, CI detection | PARTIAL | `vyrn test [file]` runs root-file tests sequentially with pass/fail reporting and exit 1 on failure (rfcs/RFC-0015-testing.md:45-59, Status: Implemented at :3; compiler/vyrn-cli/src/main.rs:3542), documented at site/app/guide.vyrn:610; no timeout flag exists — a hung test body stalls `vyrn test` indefinitely | GAP |
| Finding tests | Discovery via glob patterns, exclusions, positional path filters, regex name filtering, root config, execution order | PARTIAL | Name filter only: `vyrn test <file> --name "<substring>"` (rfcs/RFC-0015-testing.md:60; parsed at compiler/vyrn-cli/src/main.rs:3499-3500), declaration-order execution (:47), deliberately zero filename conventions — tests live in ANY .vyrn file (:93-94); no recursive multi-file discovery, glob filters, or root config — one invocation runs exactly one root file (even the repo's CI shells out per-file, site/app/routes/index.vyx:27) | GAP |
| Parallel & isolated test runs | File-level worker processes with isolation, in-file async overlap, and CI shard splitting | NONE | Test-runner parallelism is explicitly deferred: RFC-0015's Out of scope lists "fixtures/setup-teardown, test parallelism, coverage" (rfcs/RFC-0015-testing.md:101-105, Status: Implemented at :3); only the single-threaded run_tests loop exists (main.rs:3542); language-level worker threads exist (spawn/join, RFC-0025) but the test command does not use them | NOT WANTED |

## Test Features

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Lifecycle hooks | beforeAll/beforeEach/afterEach/afterAll scoped to describe block, file, or whole run via preload setup | NONE | NOT FOUND (hook-name greps over std/, compiler/, rfcs/, examples/); tests are flat top-level `test "name" { }` blocks with no setup/teardown form — fixtures/setup-teardown is under Out of scope in RFC-0015 (rfcs/RFC-0015-testing.md:104) | NOT WANTED |
| Mocks | mock() functions with call tracking, spyOn() spies, and mock.module() live module replacement | NONE | NOT FOUND — mock/spyOn greps over std/, compiler/, examples/, bench/ return zero relevant hits; no mocking, spying, or module-replacement facility exists; RFC-0015's surface is test/assert/assertEq only | UNDECIDED |
| Snapshots | Serializing values into snapshot files or inline, auto-comparison on later runs, update flags, property matchers, error snapshots | PARTIAL | Golden-value checking exists only as hand-written asserts: component snapshots via `assertEq(toHtmlString(..), "..")` in `vyrn test` (rfcs/RFC-0026-ui.md:403) and .vyx view snapshots in dogfood notes (rfcs/NOTES-dogfood-shelf.md:351); no snapshot-file or inline-snapshot machinery — snapshot testing is under Out of scope in RFC-0015 (rfcs/RFC-0015-testing.md:105) | NOT WANTED |
| Dates and times | Faking the clock in tests: setSystemTime/useFakeTimers, reset, timezone switching | PARTIAL | The clock is a host-boundary extern honoring injected fixed time: `extern fn hostNowMillis()` documented "honoring `VYRN_FIXED_TIME` so the parity harness can inject a fixed clock" (std/time.vyrn:25, RFC-0043 Status: Implemented) — a test process CAN pin the clock via env, but there is no in-test setSystemTime/reset API, and the site's testing chapter (site/app/guide.vyrn:609-612) never mentions VYRN_FIXED_TIME | DOC GAP |

## Specialized Testing

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| DOM testing | Headless DOM/component testing via happy-dom global registration plus React Testing Library render/query assertions, custom elements, event simulation, preload setup | PARTIAL | Components are pure view functions designed to be snapshot-tested in-language: std/html.vyrn:14-18 ("components are snapshot-testable in `vyrn test` via `assertEq(toHtmlString(..), "..")`"), confirmed by RFC-0026 (Status: Implemented M1–M4, rfcs/RFC-0026-ui.md:3, :61) with real .vyx view snapshots recorded (rfcs/NOTES-dogfood-shelf.md:318-319); everything interactive is missing — no simulated DOM environment, no event dispatch, no query API — and the site's testing page documents only plain test blocks | GAP |

## Reporting

| Bun entry | What Bun's page covers | Vyrn status | Evidence | Verdict |
|---|---|---|---|---|
| Code coverage | Built-in coverage: per-file percent table with uncovered lines, failing thresholds, text/lcov reporters, ignore patterns, sourcemap handling, CI recipes | NONE | Coverage is under Out of scope in RFC-0015 (rfcs/RFC-0015-testing.md:104-106, Status: Implemented at :3); coverage/lcov greps across compiler/vyrn-cli and std find no test-coverage surface — `vyrn test` accepts only `[--name <substring>]` (compiler/vyrn-cli/src/main.rs:73) | NOT WANTED |
| Test Reporters | Selectable output formats: console and dots, JUnit XML via reporter flags, GitHub Actions annotations, custom reporters | PARTIAL | One fixed human-readable reporter exists — `test "<name>" ... ok` lines plus `N passed, M failed`, exit 1 on failure, continuing past failures (rfcs/RFC-0015-testing.md:50-59, Status: Implemented at :3); the CLI surface is only `vyrn test [file.vyrn] [--name <substring>]` with no reporter selection, no JUnit/XML output, no CI annotations, and no machine-readable output — `--json` exists only on bench/routes (compiler/vyrn-cli/src/main.rs:19,944) | GAP |

RECOMMENDATION, NOT A DECISION

## The twenty largest gaps

Ranked by how much of a real Vyrn program each blocks. Each entry says what a
Vyrn user cannot do, then the smallest change that would fix it.

1. **Outbound HTTP (Fetch).** A Vyrn program cannot call any HTTP API; the only outbound calls are generated RPC/connect clients. Smallest fix: one host extern `fetch(url: String, body: String) -> Result<Response>` behind the capability set, with the response shaped like std/http's `Request` twin.
2. **Child processes (Spawn).** A program cannot run git, ffmpeg, or any external tool; `spawn` is reserved for pure tasks. Smallest fix: a host extern trio (spawn with argv, read stdout, wait for exit) gated to `vyrn run` targets first.
3. **Environment variables.** Twelve-factor configuration is impossible; two RFCs already record the hole (RFC-0014 future work, RFC-0098 M4 blocker). Smallest fix: one `extern fn getEnv(name: String) -> Option<String>`, which unblocks RFC-0098 M4.
4. **Timers and scheduling (Cron).** There is no sleep, timer, or scheduler primitive; the host owns the loop and leaves no moment for a callback (std/http.vyrn:587). Smallest fix: a host-side `sleep(millis)` extern for `vyrn run` programs, deferring in-server scheduling.
5. **TLS on the server.** `vyrn serve` speaks plain HTTP, so no deployed server can accept HTTPS traffic. Smallest fix: terminate TLS in the Rust host in front of `handle`, configured by two vyrn.json keys.
6. **SQLite (and SQL generally).** Persistence stops at std/storage's key-value model; query text can be built but never executed (examples/tagged.vyrn:13). Smallest fix: three externs (open, exec, query-to-rows) wrapping one embedded database, exposed as `std/db`.
7. **TCP client.** Without sockets, Redis/Postgres drivers and every binary protocol stay unwritable, which is why five rows above are holes. Smallest fix: connect/read/write/close externs; everything else is then writable in Vyrn.
8. **WebSocket inbound messages.** The server can push but cannot receive; RFC-0074 records the receive direction as a different, undesigned problem. Smallest fix: extend `ws(pattern, feed)` with an inbound message event on the existing feed channel.
9. **Cookie support.** Session authentication cannot be written; responses carry no headers and RFC-0016 defers cookies past v1 (rfcs/RFC-0016-server.md:143). Smallest fix: `getCookie`/`setCookie` on the existing handle arguments and Response pair.
10. **Cryptographic password hashing.** std/hash states a password must not pass through it (std/hash.vyrn:43-49), leaving login flows with no honest answer. Smallest fix: one verified argon2id host extern plus verify, documented as the exception to the no-crypto rule.
11. **Binary file writes.** `writeFile` accepts String only, so writers of images, archives, or wasm bytes are stuck (site/export.vyrn:346 works around it). Smallest fix: a `writeBytes(path, Array<UInt8>)` sibling beside the existing atomic writer.
12. **Byte streams and sinks.** `Stream<T>` covers lazy values but no backpressured byte IO, so proxies and file pumps cannot stream. Smallest fix: a `Sink` type with chunked writes over the same host externs as file IO.
13. **Test discovery across files.** `vyrn test` runs exactly one root file, so even this repository shells out per file (site/app/routes/index.vyx:27). Smallest fix: accept a directory argument and run every reachable `test` block once each.
14. **Test timeouts.** One hung test body hangs CI forever; there is no knob. Smallest fix: a default per-test watchdog in the interpreter with a generous default and a `--timeout` override.
15. **Machine-readable test output.** No JSON/JUnit output means no CI annotations and no dashboards; `--json` already exists on bench to copy (RFC-0063). Smallest fix: `vyrn test --json` emitting the pass/fail ledger.
16. **Component/event testing.** Snapshot asserts cover pure rendering, but clicks and state transitions are untestable (the DOM testing row). Smallest fix: a `fire(el, "click")` helper in std/html that drives the existing reactivity in-process.
17. **Pattern matching over paths (Glob).** Tools cannot select files by pattern, and listDir refuses on compiled targets outright (RFC-0103:102). Smallest fix: one `matchGlob(pattern, path)` pure function in std/strings; directory walking stays capability-gated.
18. **CSRF tokens.** Form-protecting a std/http app has no primitive at all. Smallest fix: HMAC token generate/verify in std/ over the existing SHA-1 handshake primitives once a keyed-hash extern lands.
19. **Archive/compression.** No compressor sits behind any IO function (site/export.vyrn:807), so bundles and downloads stay uncompressed. Smallest fix: gzip deflate/inflate as two host externs; zip containers can then be a std library.
20. **Watch mode.** Editing requires a manual restart; hot reload is recorded as additive future work (RFC-0016:146). Smallest fix: `vyrn serve --watch` restarting the host process on a source-directory mtime change, before any in-process swap.

## Documentation gaps

Three entries describe things Vyrn has today that the site does not document.

- **Workers / parallelism.** `spawn f(args) -> Task<T>` and `join` exist and are checker-proven (RFC-0025, Status: Implemented; lowering in compiler/vyrn-codegen/src/toolchain.rs:496). The site's guide mentions `spawn` only to forbid it inside generators (site/app/guide.vyrn:463). Add a route under /docs (for example /docs/concurrency.html) describing spawn/join, task ownership (RFC-0095), and `serve --workers`.
- **Console / leveled logging.** The `logging { level }` config block (RFC-0008, Status "Implemented in part"; thresholds applied at compiler/vyrn-codegen/src/direct.rs:1108-1110) and stdin line reading (RFC-0014) have no site route of their own. Add a section to the CLI chapter (near site/app/guide.vyrn:592) or a /docs/std/logging.html reference page.
- **Fixed-time testing.** `VYRN_FIXED_TIME` lets a test process pin the clock (std/time.vyrn:25, RFC-0043). The testing chapter (site/app/guide.vyrn:609-612, route /tooling/testing) documents test/assert/assertEq/--name only. Add a paragraph there covering deterministic time under tests.
