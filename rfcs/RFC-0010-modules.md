# RFC-0010 — Modules: `import` / `export`, Manifests & Reproducible Remotes

- **Status:** Implemented (M1–M4)
- **Depends on:** RFC-0002 (types — `export` gives names cross-file meaning;
  importing an enum brings its variants, a protocol its methods), RFC-0003
  (validated types — the target of JSON-Schema type imports)

> **Motivation.** Every program past one file needs a way to split into modules,
> and every ecosystem needs a way to depend on someone else's code *without*
> the left-pad problem — a dependency that can vanish or change under you. This
> RFC defines Vyrn's module system end to end: a TS-style `import`/`export`
> surface, a loader/linker that flattens many files into the single `Program`
> the rest of the compiler already understands, an optional project manifest,
> and content-addressed, lock-pinned remote imports that build forever offline.
> The design constraint throughout: the checker, interpreter, and both code
> generators must stay **unaware that modules exist**.

---

## The surface

```vyrn
// math.vyrn
export fn square(n: Int64) -> Int64 { return n * n }
export type Point = { x: Int64, y: Int64 }

// main.vyrn
import { square, Point } from "./math"
fn main() -> Int64 { return square(3) }
```

- **`export`** marks a declaration importable: `export fn`, `export type`,
  `export protocol`. A name without `export` is module-private. `export` on a
  top-level `let` (module state, RFC-0013) is rejected — module state is not
  importable in v1.
- **`import { a, b } from "<specifier>"`** brings the named exports into the
  importing module's scope. Importing a name is all-or-nothing per name; there
  is no `import *` and no rename-on-import in v1.
- **Specifier resolution** is TS-style, relative to the importing file with
  `.vyrn` appended (`"./math"` → `math.vyrn` beside the importer). `"std/..."`
  is reserved for the standard library — itself written in Vyrn (`std/math`,
  `std/strings`), so it earns three-way parity for free. Bare specifiers
  (`"money"`) resolve only through a manifest's import map (M3), and remote
  specifiers (`github:`/`gist:`/`https:`) resolve through the lock + cache (M4).

## Visibility & linking (M1)

A **loader/linker** stage sits between the parser and the checker:

1. Parse the root file, collect its `import` declarations.
2. Resolve each specifier through a **`ModuleResolver`** (I/O behind a trait —
   filesystem in the CLI, in-memory maps in tests), parse the target, recurse.
3. Enforce the rules:
   - **Per-module visibility.** A module sees a foreign name *only* if it
     imported it, and only if that name is `export`ed. Importing a name that is
     not exported (or does not exist) is an error naming the module.
   - **Cycles** between modules are rejected (a clear cycle diagnostic, not a
     stack overflow).
   - **Cross-module name collisions** (two imports of the same name, or an
     import shadowing a local declaration) are rejected.
   - **Root-only constructs.** Top-level module state (`let`, RFC-0013), the
     `logging { .. }` config block (RFC-0008), and `main` live in the **root
     module only**; the loader rejects them in imported modules. An imported
     module's `test` blocks (RFC-0015) still type-check but do not run unless
     that module is itself the argument to `vyrn test`.
4. **Link** every module into ONE `ast::Program`: functions, types, protocols,
   and impls are concatenated (each decl tagged with its source `module` for
   diagnostics), imports discharged. Downstream — checker, interpreter, native
   codegen, wasm codegen, the parity harness — never sees an `import` and has no
   concept of a module. This is what keeps `interp == native == wasm` free for
   multi-file programs.

Impl coherence is global (an `impl P for T` anywhere applies everywhere), which
is why linking can merge impl blocks without per-module scoping.

## JSON-Schema type imports (M2)

```vyrn
import type { User } from "./api.schema.json"
```

`import type { .. } from "<*.json>"` synthesizes **validated types** (RFC-0003)
from a JSON Schema document — the exact inverse of the `jsonSchema(T)` emitter:

- numeric `minimum`/`maximum`/`multipleOf` and string `minLength`/`maxLength`/
  `pattern` become `where` clauses;
- `required` vs optional steers `Option<T>`;
- `$defs`, `#/$defs/..` and root `#` `$ref`s resolve (recursion included);
- an `enum` of strings becomes a payload-less Vyrn enum;
- a constrained field becomes a synthetic `User.age`-style refinement type,
  exactly as inline field `where` desugars.

The round-trip with the emitter is **byte-exact**, and any schema keyword Vyrn
cannot express is a hard error rather than a silent drop. The emitter side is
correspondingly faithful: named nested types render as `$ref`s into a `$defs`
section (recursion is a real `$ref`, `"#"` for the root — not a lossy comment),
sized integers carry their width bounds as part of the wire contract, and
payload-less enums emit `enum` arrays.

## Project manifest & import maps (M3)

An optional **`vyrn.json`** (`name` / `main` / `dependencies`), found by walking
up from the cwd, makes a directory a project:

- `vyrn run` / `check` / `build` need **no file argument** inside a project
  (they use `main`).
- **Bare import specifiers** (`import { x } from "money"`) resolve through the
  `dependencies` map — an import map whose targets are relative-to-manifest
  paths or `std/` for now (and remote specifiers under M4).
- `vyrn new <name>` scaffolds a runnable project; `vyrn deps` prints the
  resolved module graph.
- Bare `vyrn run file.vyrn` stays **manifest-free forever** — a single file is
  always runnable without ceremony.

## Reproducible remote imports (M4)

Remote specifiers — `github:owner/repo@ref/path`, `gist:user/id[@rev]/file`, and
`https://...` — are usable inline or as manifest `dependencies` targets, and are
**reproducible by construction**:

- **Pinning.** The first resolve writes `vyrn.lock`: `specifier ⇥ immutable-url
  ⇥ sha256`. A floating ref (a branch/tag) is frozen to a specific commit via
  `git ls-remote` at pin time, so a pin never drifts.
- **Content-addressed cache.** Fetched content lives in
  `~/.vyrn/cache/sha256/<hash>` and is **hash-verified on every load** —
  tampering or corruption fails loudly, and two specifiers resolving to the same
  bytes share one cache entry.
- **`vyrn add <spec> [--name alias]`** fetches, pins, and records a dependency;
  **`vyrn update [alias]`** is the *only* command that changes a pin;
  **`vyrn vendor [--check]`** copies the lock's blobs into `./vyrn_vendor/`, so
  a committed checkout builds forever even if upstream is deleted (any copy of a
  file with the locked hash restores it).
- **Offline & sandboxed.** `--offline` / `VYRN_OFFLINE=1` builds never touch the
  network. Remote modules are sandboxed: relative imports stay inside the pinned
  base — no local-filesystem paths, no bare specifiers escaping the module.
- **Zero new crates.** SHA-256 is hand-rolled (checked against the NIST vectors);
  fetching shells out to `curl` / `git ls-remote`. All of it lives in `vyrn-cli`.

## Editor support

Multi-file programs get real editor feedback: `symbols::analyze_linked` runs the
same loader over a read-only `EditorResolver`, so an open document's imports are
resolved and its diagnostics reflect the linked program (a problem inside an
imported module surfaces at line 0 with an `in <file>: ..` prefix). Hover and
go-to-definition cross module boundaries for imported names, and `.member`
completion offers an imported protocol's methods and record's fields. The editor
path never fetches the network — it reads only pinned remotes already in the
vendor directory or the cache.

## The three backends & parity

There is nothing module-specific in the backends: linking produces one
`Program`, so a multi-file program is checked, interpreted, and compiled
(native + wasm) exactly like a single-file one, and stays a first-class citizen
of the three-way parity corpus. `std/` being written in Vyrn is the proof: the
standard library is just more modules, verified byte-identical across all three
backends like any example.

## Out of scope (future)

`import *` / namespace imports, rename-on-import, re-exports (`export { x } from
..`), importable module state, per-crate impl coherence, a registry with
semver resolution (the lock pins exact content, not version ranges), and
parallel/network-cached fetching.
