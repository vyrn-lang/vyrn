# RFC-0102 — A Toolchain Is a Dependency

- **Status:** **M3 landed** (the pin, all three tools; the report and the clang
  key). M4 proposed. See "M1 — as landed", "M2 — as landed" and "M3 — as landed"
  under Milestones for what shipped, what it is called, and where the
  implementation corrected this document.
- **Depends on:** RFC-0010 M4 (reproducible remote imports — `vyrn.lock`, the
  content-addressed cache under `~/.vyrn`, `vyrn add/update/vendor`, `--offline`),
  RFC-0021 (the generator cache, keyed on its recorded inputs), RFC-0077 M5 (the
  direct wasm backend, which deleted the largest consumer of the wasi toolchain),
  RFC-0083 (the arc that recorded two flag drifts and pre-empted a third).
- **Principle:** the compiler already knows how to acquire a thing it did not
  write, verify it, pin it, and refuse to guess. It does this for code. It does
  not do it for the programs it executes.

---

## The question

`vyrn build` runs clang. The parity harness runs wasmtime. The shim compile
reads a wasi sysroot and a builtins archive. None of those four is pinned to a
version, and nothing in the repository states which version a build used.

Vyrn settled this question for code in RFC-0010 M4. A dependency is named in
`vyrn.json`, resolved once, and frozen in `vyrn.lock` as
`specifier ⇥ url ⇥ sha256`. The bytes live in `~/.vyrn/cache/sha256/<hex>` and
the hash is checked on every load, so a tampered cache fails loudly and any copy
of the file that hashes right can restore a vanished upstream
(`compiler/vyrn-cli/src/remote.rs:1-25`). `--offline` forbids the network and a
lock-plus-cache hit needs none.

The tools got none of that. This RFC asks what stops the same machinery from
covering them, and the answer is: one JSON key, one lock-line convention, and a
`read_blob` that returns bytes.

## The evidence

### The inventory: 34 discovery sites

Every place the compiler workspace discovers, configures, or executes an
external program, counted: **34 distinct sites — 15 in production code, 19 in
test harnesses.**

Six environment variables name a tool or a tool's directory:

| var | read at | what it selects |
| --- | --- | --- |
| `CLANG` | `compiler/vyrn-codegen/src/toolchain.rs:841` | the C compiler and linker driver |
| `WASI_SYSROOT` | `compiler/vyrn-codegen/src/toolchain.rs:901` | wasi-libc headers and archives |
| `WASI_BUILTINS` | `compiler/vyrn-codegen/src/toolchain.rs:905` | `libclang_rt.builtins-wasm32.a` |
| `VYRN_WASMTIME` | `compiler/vyrn-codegen/src/toolchain.rs:758` | the wasm runtime |
| `VYRN_NODE` | `compiler/vyrn-cli/tests/memory.rs:130`, `wasmabi.rs:32`, `wasmio.rs:27` | the JavaScript runtime |
| `VYRN_REQUIRE_TOOLS` | `compiler/vyrn-codegen/src/toolchain.rs:807` | whether a missing tool skips or panics |

Two more select a directory the build reads rather than a program it runs:
`VYRN_STD` and `VYRN_WEB`, both through `root_near_exe`
(`compiler/vyrn-frontend/src/manifest.rs:56-83`), which falls back to walking up
to five levels from the executable.

Eight distinct binaries are spawned: `clang`
(`toolchain.rs:848` as a probe, `toolchain.rs:916`, `vyrn-cli/src/main.rs:3186`,
`:4645`), `wasmtime` (four argv spellings across
`compiler/vyrn-cli/tests/parity.rs` plus three codegen test files), `node`
(three test files), `git` (`compiler/vyrn-cli/src/remote.rs:69`), `curl`
(`remote.rs:107`), the built artifact itself, and — in CI only — `tar` and
`python3`. `wasm-ld` is never spawned directly; it is reached through clang's
`-Wl,` flags. Nothing uses `wasm-opt`.

**Not one of those sites reads a version.** `find_clang` runs
`clang --version` at `toolchain.rs:848` and throws the output away: the exit
status alone decides whether the bare name `clang` is used. On Windows the last
resort is a literal path, `C:\Program Files\LLVM\bin\clang.exe`
(`toolchain.rs:859`).

This is the first correction the code makes to the brief that asked for this
RFC. Discovery is **not** scattered and ad hoc. It was scattered, and
`vyrn-codegen/src/toolchain.rs` collected it — `require_tools` says so in its own
doc comment: *"a rule with two copies is a rule with two answers"*
(`toolchain.rs:801-805`). Centralization is done. What is missing is not a place
to put the lookup. It is a **version for the lookup to find**.

### Exhibit 1: four wasmtimes, four pinning disciplines

| where | version | pinned by |
| --- | --- | --- |
| `compiler/vyrn-genwasm/Cargo.lock:916-917` | **47.0.2** | a lockfile, verified, reproducible |
| `.github/workflows/ci.yml:242` | **46.0.1** | a string in a shell line inside YAML |
| `.github/workflows/ci.yml:235` | **46** | the cache key `wasm-tools-wasi25-wasmtime46` |
| `compiler/vyrn-cli/tests/common/mod.rs:266` | **46.0.1** | a hardcoded path, Windows and x86_64 only |

One repository runs two major versions of one runtime. The generator engine
embeds wasmtime 47.0.2 as a crate; the parity harness spawns wasmtime 46.0.1 as
a binary. The version that is right is the one that went through a lockfile,
and it got there because Cargo treats a compiler dependency as a dependency.

`common/mod.rs:266` is the sharpest line in the inventory:

```rust
Some(root.join("tools/wasmtime-v46.0.1-x86_64-windows/wasmtime.exe"))
```

A version, an architecture and an operating system, baked into a Rust test
harness. It resolves on exactly one developer's machine. Everywhere else it is
dead and the wasm column disappears with a `SKIP`.

The version string `46.0.1` appears in **seven** places, `25`/`25.0` for
wasi-sdk in **seven** more. Two of those seven are the published website:
`site/app/chart.vyrn:372` and `:401` name "wasmtime 46.0.1" in a benchmark
caption. A runtime upgrade is a seven-file edit in each case, and nothing checks
that all seven moved.

### Exhibit 2: `curl` with no hash

```yaml
curl -sSfL -o sysroot.tar.gz  https://…/wasi-sdk-25/wasi-sysroot-25.0.tar.gz
curl -sSfL -o builtins.tar.gz https://…/wasi-sdk-25/libclang_rt.builtins-wasm32-wasi-25.0.tar.gz
curl -sSfL -o wasmtime.tar.xz https://…/wasmtime/releases/download/v46.0.1/wasmtime-v46.0.1-x86_64-linux.tar.xz
```

`.github/workflows/ci.yml:240-242`. Three network fetches, no checksum, no
lock entry, no record of what arrived.

The project holds itself to a stricter rule than it holds its tools to.
`release.yml:256` writes `SHA256SUMS` for every published `vyrn` archive, and
`install.sh` refuses an archive whose hash does not match, refuses a release
with no `SHA256SUMS`, and refuses an asset not listed in it — three refusals,
each with a test (`install-test.sh`, `install-test.ps1:97-132`). The binary Vyrn
publishes is verified. The binaries Vyrn executes are not.

### Exhibit 3: the version answer is "go read the CI file"

`README.md:238-241`:

> See the `parity` job in `.github/workflows/ci.yml` for the exact wasmtime and
> wasi-sdk versions CI uses.

That is the whole specification. A developer who follows it downloads a tarball
by hand into `tools/`, which `.gitignore:38` ignores under the heading *OS
noise*, beside `Thumbs.db`, with no comment — the only rule in that file with
none. `README.md:264` describes the directory in one line of a layout tree.

The `tools/` convention is not documented anywhere a program can read. It is
**implemented**, in Rust, twice: `tools_wasi_sysroot_from`
(`toolchain.rs:719-742`) takes the first `tools/wasi-sysroot*` directory found
walking up, and `find_wasmtime_from` (`toolchain.rs:757-787`) takes the first
`tools/wasmtime*/wasmtime`. Both sort and take `.next()`, so the pick is
deterministic. Deterministic is not the same as correct: the sort is
lexicographic, so a tree holding `wasi-sysroot-25.0` and `wasi-sysroot-9.0` picks
9.0, and a tree holding 24 and 25 picks 24. **A build silently uses whichever
version sorts first.**

Documentation of that fetch step has already drifted:
`docs/research/cleanup-census.md:465` tells the reader to
*"Re-download from the URLs in `ci.yml:117-119`"*. The URLs are at 240-242. The
citation is wrong by 130 lines, in a file whose subject is the tools directory.

### Exhibit 4: the flag drifts, corrected

The brief cited "three clang-flag drifts" and remembered `-msimd128`. That flag
has never existed in this repository; `git log --all -S"msimd128"` is empty. The
record is more useful than the memory. `compiler/vyrn-cli/src/main.rs:194-204`
states it:

> There are two clang invocations in this file … and they have drifted twice.
> First `-lm`, added to one, and CI kept failing with
> `undefined reference to ceilf` from the other because the parity harness uses
> the second. Then `-O2`, which only `bench_native` passed, so every number
> RFC-0083 recorded described an optimized binary that `vyrn build` never
> emitted.

Two drifts happened, in commits `e55e979` and `4a025de` (`-lm`, fixed twice —
the first fix went into the wrong call site) and `71b59ed` (`-O2`). A third was
pre-empted in `ea58519` when `-march` arrived: RFC-0083:1006-1011 says *"A
benchmark measuring a different `-march` than the artifact would be that same
bug a third time."* So: two suffered, one pre-empted. Three.

The relevant half of this exhibit is the **shape**, not the count. Each drift was
one list written twice, and each was found by CI failing on a platform the author
did not use. `main.rs:229` states the structural version: *"A Windows-only check
structurally cannot see this missing."* A version is the same kind of fact as a
flag. It is written down in seven places today, and nothing compares them.

One hand-copied flag list survives:
`compiler/vyrn-codegen/tests/layout_vs_clang.rs:210-220` spells
`--target=wasm32-wasip1` / `--sysroot=…` / `-nodefaultlibs <builtins> -lc`
longhand, a subset of `shim_wasm`'s list at `toolchain.rs:916-939`, obtained from
nowhere. It is out of scope here and named so it is not lost.

### Exhibit 5: a tool's identity is not in any cache key

`shim_wasm` (`toolchain.rs:885-951`) compiles the C runtime shim with clang and
caches the result under

```rust
let key = format!("shim-{}-{}.wasm", sha256_hex(RUNTIME_SHIM.as_bytes()), SHIM_BASE);
```

`toolchain.rs:889-893`. The key covers the shim source and the memory base. It
does not cover clang. Upgrade clang and the cache still hits, and the build links
a shim the new compiler never saw. Nothing here is wrong today, because the shim
is stable and has one remaining consumer. It is wrong in principle, and it is
wrong for the same reason RFC-0021's generator cache is keyed on **every**
recorded input: a cache key that omits an input is a cache that serves a stale
answer.

### Exhibit 6: an unpinned tool has already changed observable behaviour

`compiler/vyrn-codegen/src/direct.rs:256`:

> `find_clang() == None` made it DECLINE — a `.vyx` keystroke was 54 ms or
> 250 ms depending on whether someone had installed a C toolchain.

RFC-0077 M5 removed the C toolchain from `vyrn build --target wasm` entirely, and
that is the second correction to the brief: **the wasm build path needs no clang,
no sysroot and no builtins archive today.** The surface this RFC covers is
therefore smaller than it looks. What still executes a tool is: `vyrn build`
(native, clang), `vyrn bench` (native, clang), the parity harness (wasmtime), the
codegen integration tests (clang, sysroot, builtins, wasmtime), and three test
files (node). That is the list this RFC pins.

## The design

### One key, and the split it inherits

`vyrn.json` gains a `toolchain` object. A key is a tool name; a value is a
version string.

```json
{
  "main": "src/main.vyrn",
  "toolchain": {
    "wasmtime": "46.0.1",
    "wasi-sysroot": "25.0",
    "wasi-builtins": "25.0"
  }
}
```

`vyrn.lock` gains one line per tool per platform, in the format it already has —
`specifier ⇥ url ⇥ sha256`, tab-separated, sorted, one parser
(`compiler/vyrn-frontend/src/manifest.rs:191-277`):

```text
tool:wasmtime@46.0.1/x86_64-linux	https://…/wasmtime-v46.0.1-x86_64-linux.tar.xz	9f3c…
tool:wasmtime@46.0.1/x86_64-windows	https://…/wasmtime-v46.0.1-x86_64-windows.zip	1ab7…
tool:wasi-sysroot@25.0/any	https://…/wasi-sysroot-25.0.tar.gz	4d20…
```

**No lock format change.** The specifier is an opaque string to `Lock::load`, so
a `tool:` key rides the existing reader, the existing sort, the existing
diff-friendliness, and the existing refusal to treat a damaged lock as an
unpinned project (`manifest.rs:201-211`).

The manifest/lock split is not a new invention either. It is exactly what
`dependencies` does: the project writes an intent, the lock records what that
intent resolved to. A project that pins `"wasmtime": "46.0.1"` writes one line.
The per-platform URLs and hashes are the lock's problem, and the lock is
generated.

`/any` is a real platform value, not a placeholder. The wasi sysroot and the
builtins archive are wasm32 **target** libraries: the same file on every host.
`ci.yml:204-206` already says so, in the argument for an ARM leg it does not
build. One entry, every machine.

### Where the URL comes from

For a module, `resolve_to_url` understands three specifier schemes
(`remote.rs:58-103`). For a tool, the resolver holds a small table: tool name and
platform in, URL out. Three entries at M1 — `wasmtime`, `wasi-sysroot`,
`wasi-builtins` — because those are the three the repository fetches.

A tool name the table does not know is a refusal that names the tools it does
know. It is **not** a fall-through to PATH. Inventing a URL scheme for arbitrary
third-party tools is future work and is stated as such below.

### Fetch, verify, unpack

The fetch is the one `remote.rs:106-118` already performs: `curl -sL --fail`,
then `sha256_hex`, then compare against the lock, then `write_blob`. The
mismatch message is the one that exists — *"the upstream changed under an
immutable URL; refusing to build"* (`remote.rs:152-156`).

One code change is needed, and it is small.
`manifest.rs:325-336` `read_blob` verifies a hash and then returns
`Result<String, String>`, rejecting anything that is not UTF-8. A tool is a
tarball. `read_blob` splits into a bytes core and the UTF-8 wrapper that today's
two callers keep using. The hash check stays in the core, so it stays a property
of the cache rather than of whichever caller opened it — which is the reason
that function is shared in the first place (`manifest.rs:338-344`).

An archive is then unpacked to `~/.vyrn/tools/<sha256>/`, a directory derived
from a verified blob and deletable at any time. It goes beside `~/.vyrn/bin`,
`std`, `web` and `cache`, which is what `install.sh:117-123` already builds.

Unpacking shells out to `tar`. That is the argument `remote.rs:18-20` already
made and is already load-bearing for `curl` and `git`: the tool is ubiquitous,
and taking a crate for it would cost more than it buys. `tar` ships with Windows
10 and later, every Linux userland, and macOS.

### Discovery is demoted, and never a fallback

The rule has one sentence: **a pin is consulted first; an environment variable
overrides it and says so; PATH is used only when there is no pin.**

The order becomes:

1. `$CLANG` / `$VYRN_WASMTIME` / `$WASI_SYSROOT` / `$WASI_BUILTINS` / `$VYRN_NODE`
   — the explicit escape hatch. A build that takes this path **reports** it.
2. The pin in `vyrn.lock`, resolved through vendor, then cache, then network.
3. PATH and the `tools/` walk, only when the manifest declares no `toolchain`.

What is deliberately absent is a fourth step. A pinned tool that cannot be
resolved **fails**. It does not fall back to PATH. That is the same rule
`Lock::load` was hardened to obey and for the same reason: a lock that failed
toward the network was the one artifact whose whole job is that it cannot drift
(`manifest.rs:201-211`), and a pin that failed toward PATH would be that bug with
a different tool at the end of it.

The `tools/` walk keeps working for a project with no pin, so a clone of this
repository behaves as it does now until this repository writes its own
`toolchain` key. When it does, `tools_wasi_sysroot_from`'s lexicographic sort
stops deciding anything, which removes the 24-over-25 hazard rather than fixing
it.

### No pinned artifact for this platform

The refusal names the tool, the version, this platform, and the platforms the
lock does cover. `install.sh:38-42` is the model and the wording to match:

```text
error: wasmtime 46.0.1 is pinned, and vyrn.lock has no entry for aarch64-windows.
  Pinned platforms: x86_64-linux, aarch64-linux, aarch64-macos, x86_64-windows.
  Add one with `vyrn update wasmtime`, or point $VYRN_WASMTIME at a binary
  you trust.
```

The platform vocabulary is `install.sh:30-38`'s, unchanged:
`x86_64-linux`, `aarch64-linux`, `aarch64-macos`, `x86_64-windows`. One word,
one meaning; a second spelling of a platform would be a second answer about which
artifact to fetch.

### clang is not pinnable, and the RFC says so

A native `clang` links against the host's libc, the host's linker and the host's
system libraries — the MSVC UCRT on Windows, the Xcode command line tools on
macOS. There is no portable clang tarball that produces a working native binary
on every platform, and pretending otherwise would ship a pin that fails at link
time instead of at resolve time.

So clang stays discovered. Three things change:

1. `find_clang` **captures** `clang --version` instead of discarding it
   (`toolchain.rs:848`). The string is already produced; only the pipe is new.
2. That string joins `shim_wasm`'s cache key (`toolchain.rs:889-893`), which
   closes Exhibit 5.
3. The version, the path and the reason the path was chosen are reported.

If a project needs a pinned clang for a **wasm** target, the wasi-sdk publishes
one per platform, and it enters the table as a fourth tool. That is a later
milestone, not a claim made here.

### The report goes in `vyrn deps`

No `vyrn doctor`. `vyrn deps` already answers "what does this build depend on"
(`compiler/vyrn-cli/src/main.rs:1470-1505`); this RFC's whole thesis is that a
tool is one of those things, and giving it a second command would contradict the
title. The module graph gets a `toolchain:` section under it:

```text
toolchain:
  clang         C:/Program Files/LLVM/bin/clang.exe   clang 22.1.0   (discovered: PATH)
  wasmtime      ~/.vyrn/tools/9f3c…/wasmtime          46.0.1         (pinned)
  wasi-sysroot  $WASI_SYSROOT                         unknown        (override: environment)
```

Three columns, and the third is the one that matters: **what was used and why**.
An override prints as an override, so a machine that disagrees with CI says which
line it disagreed on.

### `--offline` needs no new rule

A tool blob is a blob. `--offline` / `VYRN_OFFLINE=1` already forbids the network
and already produces the correct two errors: locked-but-uncached names the hash
and says any file with that hash will do (`remote.rs:142-148`); unlocked says the
specifier is not in `vyrn.lock` (`remote.rs:162-166`). `vyrn vendor` already
copies pinned blobs into `vyrn_vendor/sha256/` for an air-gapped repository, and
a 205 MB sysroot is a large thing to commit but not a different thing.

### Per-project, not per-user

Per-project, decided, with the reason.

rustup pins per user, with `rustup override` and `rust-toolchain.toml` as
per-directory corrections layered on top. That puts the primary pin outside the
project and makes the project's record the exception.

Vyrn has already answered this question once. `dependencies` is per-project,
`vyrn.lock` is per-project and committed, and the **bytes** are per-user in
`~/.vyrn/cache/sha256` so that ten projects on one machine download a tarball
once. Splitting the pin from the storage is the whole point of content
addressing, and it is already built. A per-user toolchain pin would make two
projects on one machine unable to disagree, which is precisely the thing a
project-level lockfile exists to allow.

### CI consumes the same pin

The repository writes a `vyrn.json` at its root — it has none today — carrying
the `toolchain` key, and `vyrn.lock` beside it. Then `ci.yml:236-243` is deleted:
three `curl` lines, one `tar` line, and the version strings inside them. The
cache step keeps `actions/cache@v4` on `~/.vyrn/tools`, keyed on the hash of
`vyrn.lock` rather than on a hand-written `wasm-tools-wasi25-wasmtime46`.

The Rust test harnesses read the pin through the same resolver rather than
through exported variables, which deletes `ci.yml:251`, `:277-284` and the
hardcoded path at `common/mod.rs:266`. `VYRN_REQUIRE_TOOLS` stays exactly as it
is: it answers a different question (may a check skip?), and its own doc comment
names the failure it prevents — *"a cache that restored an empty directory, a
renamed release asset or a typo in an exported path all read as green"*
(`toolchain.rs:789-805`). A pin removes two of those three. The third is why the
variable survives.

Net: seven copies of a wasmtime version become one, and the one is checked
against a hash.

## What this RFC does not do

Stated flatly, because each of these is a plausible next request and none is a
promise here.

- **It does not build a tool from source.** It fetches a published artifact and
  verifies it. A platform with no published artifact gets the refusal above.
- **It does not wrap a package manager.** No apt, no brew, no winget, no
  installer. Vyrn fetches a tarball into its own cache and reads it there.
- **It does not promise every platform.** It promises that the platforms with a
  lock entry are byte-identical and that the ones without say so.
- **It does not pin clang.** See above. It records clang.
- **It does not make the runtime configurable.** wasmtime is one more pinned
  tool. A second engine would be a second table entry and a declared interface —
  today the interface is "spawn it with `run <module>`", spelled four different
  ways across `parity.rs` (with `--dir .`, with `--env`, with `-W timeout=20s`,
  bare). Consolidating those four spellings is the work that has to happen before
  an engine is swappable, and it is not this RFC. The seam is left; the swap is
  not attempted.
- **It does not benchmark linkers.** A faster linker becomes pinnable the moment
  a tool is pinnable, which is the point. Choosing one, measuring one, or making
  the linker a manifest key is future work.
- **It does not add a crate.** `curl`, `git` and `tar` are spawned, as `curl` and
  `git` already are, for the reason `remote.rs:18-20` already gives.

## Why this is better than rustup

rustup pins `rustc`, `cargo` and the standard library, per user, and stops at the
C boundary. It does not pin the `cc` that builds a `-sys` crate, the linker that
finishes the binary, or any runtime the test suite spawns. A reproducible Rust
build is reproducible up to the moment it shells out, and after that it is the
machine's business. That boundary is why this repository has two wasmtimes: the
one Cargo owns is 47.0.2 and correct; the one Cargo does not own is 46.0.1,
written down in four places, and correct nowhere in particular.

Vyrn does not need a new mechanism to do better. It needs to apply the one it
already trusts for code to the programs it already executes: same manifest, same
lockfile, same content-addressed cache, same `--offline`, same refusal when a
hash disagrees. The claim is not that pinning a toolchain is a new idea. It is
that **the boundary rustup draws is an artifact of Cargo's history, not a law**,
and a language whose dependency machinery is nine months old can put the boundary
somewhere more useful.

## Milestones

- **M1 — the pin.** `toolchain` in `vyrn.json`; `tool:` lines in `vyrn.lock`;
  `read_blob` split to bytes; fetch, verify, unpack to `~/.vyrn/tools/<sha>/`;
  the three-entry table; the honest platform refusal. `wasmtime` only, because it
  is the tool with four versions and the one every parity run needs.
### M1 — as landed

`compiler/vyrn-frontend/src/toolpin.rs` is the new file: the table, the platform
vocabulary, the lock specifier, the unpack, and the resolver. It sits in the
frontend beside `manifest.rs` for the reason `manifest.rs` is there at all — the
editor reads a pin too, and a second reader of a pin is a second answer about
what is pinned. `vyrn-cli` is a **binary** crate, so a resolver living there
would be unreachable from `vyrn-codegen`, which is where discovery already is.

**The manifest key.** `Manifest.toolchain: Vec<(String, String)>`, read beside
`dependencies` from the same parse. Empty when the key is absent, which is what
keeps a project with no pin byte-identical to before.

A tool name the table does not know is refused where it is **written**, not
where it is looked up — `from_doc` returns `Err`, on the channel an unparseable
manifest already travels:

```text
unknown tool `wasmtimee` in vyrn.json's `toolchain` — the tools vyrn can pin are
wasmtime, wasi-sysroot, wasi-builtins
```

That is a correction to this document, which put the refusal in the resolver. A
typo that declared nothing would leave the build on PATH with a `toolchain` key
in the file saying otherwise — the silent fallback the RFC exists to forbid,
arriving through the spelling rather than through the lookup.

**The lock lines**, as written by `vyrn update wasmtime` and read by `Lock::load`
with no format change:

```text
tool:wasmtime@46.0.1/aarch64-linux	https://github.com/bytecodealliance/wasmtime/releases/download/v46.0.1/wasmtime-v46.0.1-aarch64-linux.tar.xz	071c4def…
tool:wasmtime@46.0.1/aarch64-macos	https://…/wasmtime-v46.0.1-aarch64-macos.tar.xz	acee50be…
tool:wasmtime@46.0.1/x86_64-linux	https://…/wasmtime-v46.0.1-x86_64-linux.tar.xz	9ae0b17e…
tool:wasmtime@46.0.1/x86_64-windows	https://…/wasmtime-v46.0.1-x86_64-windows.zip	99f03806…
```

**Which platforms were recorded, and how.** All four, in one `vyrn update
wasmtime` run against the real GitHub release: the command fetches every platform
`tool_platforms` names, hashes each, and writes the blobs into
`~/.vyrn/cache/sha256`. A hash is a fact about an artifact, so a machine that can
reach the network records it for a machine that cannot — which is the only way
the lock CI reads can be the lock a developer wrote. A platform whose asset does
not exist upstream is reported and skipped; all four exist for wasmtime 46.0.1.
The four hashes above are that run's output, verified end to end: the Windows zip
unpacked through `tar` to `~/.vyrn/tools/99f03806…/wasmtime-v46.0.1-x86_64-windows/wasmtime.exe`,
which reports `wasmtime 46.0.1 (823d1b8f2 2026-06-24)`.

**`read_blob`, as split.** `read_blob_bytes` is the core and keeps the hash
check; the UTF-8 step is `as_module_text`, applied by `pinned_blob` one level
**above** the core rather than beside it. This document expected a wrapper at the
same level; the core's only two call sites turned out to be the two halves of
`pinned_blob`'s vendor-then-cache lookup, so wrapping it there would have been
two answers about what a cached module is. `pinned_blob_bytes` is the new public
reader, and `pinned_blob` is `pinned_blob_bytes` plus the UTF-8 step.

**The discovery order**, in `vyrn-codegen`'s new `wasmtime_from`, which returns
the path **and why it was chosen** (`"override: environment"`, `"pinned"`,
`"discovered: tools/"`):

1. `$VYRN_WASMTIME`, when it names a file that exists.
2. The pin: `toolchain.wasmtime` in the `vyrn.json` governing the directory
   asked about, resolved through `vyrn.lock` to a hash and through vendor, then
   cache, to `~/.vyrn/tools/<sha>/`. A pin that cannot be resolved is `Err`.
3. The `tools/` walk, only when no pin exists.

The reason is *returned*, not printed: the report this RFC designs is `vyrn deps`'
`toolchain:` section, and that is M3. `find_wasmtime_from` keeps its
`Option<PathBuf>` signature for the four existing call sites and **panics** on
`Err`, for the reason `require_tools` panics — a run that silently skips the
checks a tool exists for is a green run that proves nothing.

The two refusals, verbatim:

```text
wasmtime 46.0.1 is pinned, and vyrn.lock has no entry for aarch64-windows.
  Pinned platforms: aarch64-macos, x86_64-linux.
  Add one with `vyrn update wasmtime`, or point $VYRN_WASMTIME at a binary you trust.
```

```text
wasmtime 46.0.1 is pinned for x86_64-windows (sha256 99f0…) but not cached — run
`vyrn update wasmtime` online, `vyrn vendor`, or drop any copy of the archive with
that hash into C:/Users/…/.vyrn/cache/sha256
```

Both are message bodies; the `error: ` prefix in this document's model is the
caller's, as it is for every other string the frontend hands back. The covered
platforms print sorted, because the lock is a sorted map.

**Scope, honestly.** Only `wasmtime` is *consumed* through the pin at M1; the
table's three entries all produce URLs (the wasi-sdk two are `/any`, and their
URL shape is tested), but nothing resolves them yet — `shim_wasm` and
`layout_vs_clang` still read `$WASI_SYSROOT` / the `tools/` walk. That is M2.
This repository still has no root `vyrn.json`, so nothing in it is pinned yet:
that is M4, and until then every gate below ran through steps 1 and 3 of the
order exactly as it did before.

**Gates.** Full workspace `cargo test --release`: 1736 passed, 0 failed. Parity
(`-p vyrn-cli --release --test parity -- --ignored --test-threads=1`) with
`$VYRN_WASMTIME`, `$WASI_SYSROOT`, `$WASI_BUILTINS` and `VYRN_REQUIRE_TOOLS=1`:
40 passed, 0 failed — the env overrides still win, which is step 1 of the order
being exercised by every parity run. `vyrn-lsp`: 76 passed, 0 failed.
`vyrn-genwasm` and `vyrn-play` (`--target wasm32-unknown-unknown`): compile.
`cargo fmt --check` on the workspace and all three excluded crates: clean.

- **M2 — the sysroot and the builtins.** The other two table entries, `/any`
  platform. `shim_wasm` and `layout_vs_clang` resolve through the pin.
### M2 — as landed

Two resolvers beside `wasmtime_from`, in the same file and in the same three
steps: `wasi_sysroot_from` and `wasi_builtins_from`, each returning the path
**and why it was chosen**, each with a `find_*` wrapper that panics on an
unresolvable pin for the reason `find_wasmtime_from` does. The three now share
`pinned_tool_dir`, which is step 2 for any tool — find the `vyrn.json` governing
a directory, read `toolchain.<name>`, resolve through `vyrn.lock` to a hash and
through vendor, then cache, to `~/.vyrn/tools/<sha>/`. `wasmtime_from` lost its
inline copy of that to gain it.

**The real runs.** One `vyrn update` each, against the real wasi-sdk 25 release,
in a project declaring both keys. Each pinned one line, because both artifacts
are `/any`:

```text
tool:wasi-builtins@25.0/any	https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-25/libclang_rt.builtins-wasm32-wasi-25.0.tar.gz	13aca55665321200b9659b292615adf5110ace9e891ab94511badd970553ca18
tool:wasi-sysroot@25.0/any	https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-25/wasi-sysroot-25.0.tar.gz	d09c62c18efcddffe4b2fdd8c5830109cc8e36130cdbc9acdc0bd1b204c942bb
```

`update_tool` needed no change for either: `tool_platforms` already answered
`["any"]`, and `vyrn update wasi-builtins` left the sysroot's line alone because
the retain is keyed on `tool:<name>@`.

Then both were *consumed* through the pin, with `$WASI_SYSROOT` and
`$WASI_BUILTINS` unset and that lock at the worktree root:
`layout_vs_clang` compiled its C program against the unpacked sysroot and linked
the unpacked archive — *126 numbers over 34 shapes agree with clang* — and
`shim_wasm`, with its disk cache emptied first so the compile was real, produced
the module `shim_link` instantiates: *33 boundary signatures instantiated against
the module that defines them*.

**The unpacked layout, which is a correction.** The brief for this milestone
expected one shape, "a directory containing `wasi-sysroot-25.0/` or similar", and
the two artifacts do not agree with each other:

```text
~/.vyrn/tools/d09c62c1…/wasi-sysroot-25.0/{include,lib,share,VERSION}
~/.vyrn/tools/13aca556…/libclang_rt.builtins-wasm32.a
```

The sysroot nests one version-named level down; the builtins archive has no
directory at all. So the answer is not a per-tool layout table, it is
`tool_binary`'s existing rule generalized — *at the top of the unpacked
directory, or one level in, sorted* — now `at_top_or_one_in`, with three public
faces: `tool_binary` (a file, plus `.exe` on Windows), `tool_file` (a file,
exact name), and `tool_root` (the **directory** holding a marker, which is what
`--sysroot=` wants: `include/` finds `wasi-sysroot-25.0` whether or not the
archive nested it).

**`$WASI_BUILTINS` had two spellings, and step 1 is what found it.** Before M2,
`layout_vs_clang` never read the variable: it read `$WASI_SYSROOT` and then went
to `builtins_near_sysroot`, so the `WASI_BUILTINS` line CI exports for that step
was doing nothing there. Making the variable step 1 for real surfaced the
disagreement — CI exports the `.a` **file**, while what the release artifact
unpacks to, and what `.gitignore`'s `tools/` convention holds, is a **directory**
— and an exported directory reaches clang as `wasm-ld: error: … is a directory`.
The override now accepts both: a value that names a directory is resolved to the
`.a` inside it by the same two-level rule the pin uses. A value that names a file
is unchanged, which is CI's case.

That has one visible consequence and it is stated rather than buried: on CI,
`layout_vs_clang` will now link `-nodefaultlibs <builtins> -lc` where it
previously linked without them, because `builtins_near_sysroot` cannot find that
archive in `~/wasm-tools` (it looks one directory down, and the artifact unpacks
flat). That is the link line `vyrn build` and `shim_wasm` use, and it is verified
here with both spellings of the variable.

**The sort hazard.** Unchanged where nothing is pinned, and gone where something
is. `tools_wasi_sysroot_from`'s lexicographic sort is step 3, so a project with a
`toolchain.wasi-sysroot` key never reaches it and never picks 24 over 25. A
project without one still does, and **this repository is still one of them** —
there is no root `vyrn.json` until M4, so the hazard is live here today. M2
removes the hazard for pinned projects; it does not fix the walk, which is what
"removes rather than fixes" meant.

**Gates.** Full workspace `cargo test --release`: 1736 passed, 0 failed. Parity
(`-p vyrn-cli --release --test parity -- --ignored --test-threads=1`) with
`$VYRN_WASMTIME`, `$WASI_SYSROOT`, `$WASI_BUILTINS` and `VYRN_REQUIRE_TOOLS=1`:
40 passed, 0 failed. The codegen ignored tier (`-p vyrn-codegen -- --ignored`)
with the same four variables, run **twice** — once with `$WASI_BUILTINS` naming
the directory and once naming the `.a`, because that is the difference this
milestone found: 8 passed, 0 failed both times (`layout_vs_clang` 1, `shim_link`
2, `wasm_runs` 5). `vyrn-lsp`: 76 passed, 0 failed. `vyrn-genwasm` and
`vyrn-play` (`--target wasm32-unknown-unknown`): compile. `cargo fmt --check` on
the workspace and all three excluded crates: clean.

The new checks are in `toolchain_pin.rs`, seeded the M1 way — fabricated
archives, hashed into the cache, named by a `tool:` line, no network. They are
called from the one `#[test]` rather than being a second one: `cargo test` runs
tests in threads of one process, and `set_var` beside another thread's `getenv`
is a race whether or not the two agree about which variable they are setting. And
the pinned path has **no host-only branch** — both tools pin `/any`, so every
assertion in it executes on all four platforms, which is the shape M1's
x86_64-linux branch failed CI for lacking.

- **M3 — the report and the clang key.** `vyrn deps` grows its toolchain
  section; `find_clang` captures its version; that version joins the shim cache
  key.
### M3 — as landed

**The version, captured.** `clang_from` is `wasmtime_from`'s shape for the one
tool this RFC does not pin: it answers with the path, the version and **why that
path** — `"override: environment"` for `$CLANG`, `"discovered: PATH"`, and
`"discovered: C:\Program Files\LLVM\bin\clang.exe"` for the Windows last resort,
which is one literal spelled once (a `windows_clang!` macro), because a reason
that disagrees with the path it explains is worse than no reason. `find_clang` is
now `clang_from().map(|(p, _, _)| p)` and its four call sites are unchanged.

The version is **the first line of `clang --version`, trimmed** — whatever the
vendor prints. Apple, Ubuntu and upstream word that line differently, and this
document's model row (`clang 22.1.0`) would have been this repository inventing
a version number for a compiler it did not build. What the gate machine reports
is `clang version 22.1.0 (https://github.com/llvm/llvm-project 4434dabb…)`, and
that whole string is what the column and the key carry.

`clang_from` is **memoized** for the process. That is not a new cost avoided, it
is an old one removed: `find_clang` spawned `clang --version` on *every* call
before M3, and it is now spawned once. The memo is what makes reading the
compiler before the shim cache lookup free.

**The cache key**, Exhibit 5 closed:

```text
shim-<sha256(shim source)>-<SHIM_BASE>-clang<sha256(version line)[..16]>.wasm
```

The compiler enters as a hash of the version line rather than as the line,
because a key is a **filename** and a clang version line carries spaces,
parentheses and a URL. That is a correction to this document's wording ("that
string joins the cache key"): the string decides the key, and sixteen hex digits
of it are what is spelled. `shim_key_clang_component` is public so a check can
assert the key CONTAINS the compiler rather than merely that it changed.

One behaviour did change, and it is stated rather than buried: **`shim_wasm`
reads clang before it consults the cache**, so a machine with no clang and a
warm shim cache now gets `None` where it used to get the cached module. That is
the point of the exhibit — a cached artifact that cannot be attributed to any
compiler is the stale answer the key exists to prevent — and every other path is
identical: no clang has always meant no shim compile, and `find_clang() == None`
still DECLINEs in the direct backend exactly as `direct.rs:276` describes.

**The report.** `vyrn deps` prints the module graph, then:

```text
toolchain:
  clang          clang      clang version 22.1.0 (https://github.com/llvm/llvm-project 4434dabb69916856b824f68a64b029c67175e532)  (discovered: PATH)
  wasmtime       not found  unknown  (not found: $VYRN_WASMTIME, tools/)
  wasi-sysroot   not found  unknown  (not found: $WASI_SYSROOT, tools/)
  wasi-builtins  not found  unknown  (not found: $WASI_BUILTINS, beside the sysroot)
```

That is a project with **no** `toolchain` key, which is every project in this
repository today: the walk and PATH are still answers, and a tool that resolves
to nothing prints the chain that was consulted rather than being omitted. The
same project pinning `"wasmtime": "46.0.1"`, with the lock line M1 recorded and
the blob in the content-addressed cache:

```text
toolchain:
  clang          clang                                                                                                                                     clang version 22.1.0 (https://github.com/llvm/llvm-project 4434dabb69916856b824f68a64b029c67175e532)  (discovered: PATH)
  wasmtime       C:/Users/demko/.vyrn/tools/99f038066b16cb3aaf63c1d282a9d7ba7befafbadf7aa8827cc4c712d96bc31a/wasmtime-v46.0.1-x86_64-windows/wasmtime.exe  46.0.1  (pinned)
  wasi-sysroot   not found                                                                                                                                 unknown  (not found: $WASI_SYSROOT, tools/)
  wasi-builtins  not found                                                                                                                                 unknown  (not found: $WASI_BUILTINS, beside the sysroot)
```

And that same pinned project with `$VYRN_WASMTIME` set over the pin — the line a
machine that disagrees with CI points at:

```text
toolchain:
  clang          clang                                                       clang version 22.1.0 (https://github.com/llvm/llvm-project 4434dabb69916856b824f68a64b029c67175e532)  (discovered: PATH)
  wasmtime       N:/lang/tools/wasmtime-v46.0.1-x86_64-windows/wasmtime.exe  unknown  (override: environment)
  wasi-sysroot   not found                                                   unknown  (not found: $WASI_SYSROOT, tools/)
  wasi-builtins  not found                                                   unknown  (not found: $WASI_BUILTINS, beside the sysroot)
```

**Three corrections to the model block**, all visible above:

1. The path column prints the **path**, not the variable name. This document's
   model shows `$WASI_SYSROOT`, which says which step answered and hides which
   file it answered with. The reason column already says the step, so printing
   the variable twice would cost the one fact the row exists to carry.
2. The version column is **not padded**. A clang version line runs past a
   hundred characters, and aligning every other row to it spends a screen of
   spaces on one parenthesis. Name and path are padded; the version is not.
3. A pin that cannot be resolved prints as a row, not as an error: path
   `unresolved`, the pinned version, and `(pinned, unresolved: <the refusal, on
   one line>)`. A report that omitted it would be the silent fallback this RFC
   exists to forbid, arriving through the reporter.

The version column is the pin for a pinned tool — the manifest is where a
version is written down — and `unknown` for an override or a walk, which is what
this document's model already showed. clang is the exception because its probe
was already being run and thrown away; running the other three to ask would
spend four spawns on a report.

**What `vyrn deps` still needs, and does not have.** It requires a manifest with
a `main`, and **no example project in this repository declares one** — they
declare `artifacts`, or the `server`/`client` sugar RFC-0103 M1 made of them. So
the "no toolchain key" output above is a scaffolded project, not
`examples/shelf`. Teaching `deps` to take an artifact entry is a separate change
and is not made here; the toolchain section is unaffected by it either way.

**Gates.** Full workspace `cargo test --release`: 1737 passed, 0 failed. Parity
(`-p vyrn-cli --release --test parity -- --ignored --test-threads=1`) with
`$VYRN_WASMTIME`, `$WASI_SYSROOT`, `$WASI_BUILTINS` and `VYRN_REQUIRE_TOOLS=1`:
40 passed, 0 failed. The codegen ignored tier (`-p vyrn-codegen -- --ignored`)
with the same four variables, run with `~/.vyrn/cache/shim` **deleted first** so
the compile was real: 9 passed, 0 failed (`layout_vs_clang` 1, `shim_link` 2,
`wasm_runs` 5, plus one unrelated corpus check), and the cache came back holding
exactly

```text
shim-fcebe7fa226239cb3bcbd3034de06d01021dc6a84573894965fdf3d15c9162ac-33554432-clanged0eb075dbe2df0a.wasm
```

— the compiler component present in the name of the module `shim_link`
instantiated. `vyrn-lsp`: 76 passed, 0 failed. `vyrn-genwasm` and `vyrn-play`
(`--target wasm32-unknown-unknown`): compile. `cargo fmt --check` on the
workspace and all three excluded crates: clean.

The new checks follow M2's rule. The clang half joins `toolchain_pin.rs`'s single
`#[test]` because `$CLANG` is step 1 there too, and a second test setting it
beside the first is the same `set_var` race. The `vyrn deps` half is in
`vyrn-cli/tests/project.rs`, where every case runs `vyrn` as a **child** process,
so the override is set on the child and no thread in this process reads a
variable another wrote. Neither has a host-only branch: a machine with clang and
one without both get an assertion, and the key component is a pure function of a
string.
- **M4 — CI consumes the pin.** Delete `ci.yml:236-243`, `:251`, `:277-284` and
  `common/mod.rs:266`. Cache `~/.vyrn/tools` on the lock's hash. The acceptance
  test is a version bump: one edit to `vyrn.json`, one `vyrn update`, and no
  other file in the repository mentions the old number.

M4's acceptance test is the RFC's acceptance test. If a wasmtime upgrade still
touches seven files, this did not work.

## Alternatives refused

- **A `toolchain.json` of its own.** A second manifest is a second walk-up, a
  second parse, a second unreadable-file question, and a second answer about
  which directory a project starts at. `vyrn.json` ignores keys it does not know
  (`manifest.rs:90`), so this key costs no file and no migration.
- **A `toolchain.lock` of its own.** Same objection, plus the existing lock
  format already fits. `Lock::load` reads three tab-separated fields and does not
  care what the first one means.
- **Per-user pinning, like rustup.** It makes two projects on one machine unable
  to disagree, and it puts the pin outside the artifact a team reviews.
- **URLs in the manifest.** It would put four platform URLs and four hashes in
  every project's `vyrn.json`, which is what a lock is for. The manifest carries
  intent; the lock carries what the intent resolved to. That is `dependencies`,
  unchanged.
- **Falling back to PATH when a pinned tool is missing.** The whole value of a
  pin is that its absence is loud. A silent fallback would let a green CI run
  prove nothing, which is the exact defect `require_tools` exists to close and
  which merge `89c765e` found in eight tests at once.
- **Keeping the `tools/` walk as the primary mechanism and adding a version
  check to it.** The walk cannot verify what it finds, cannot fetch what is
  missing, and cannot be committed. Version-checking a directory listing would
  make the wrong mechanism louder.
- **Vendoring the tools into the repository.** 250 MB of binaries per platform
  in git. `vyrn vendor` already offers this to any project that wants it, per
  project, by choice, and does not impose it.
