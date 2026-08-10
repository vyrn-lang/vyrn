# Cleanup census

A census of repository residue at `N:\lang`. It states facts with evidence. It
proposes dispositions. It changes nothing.

- **Repository:** github.com/vyrn-lang/vyrn (public)
- **HEAD at census time:** `d2ba1b9` (2026-08-10)
- **Tracked files:** 536 (`git ls-files | wc -l`)
- **Method:** `git ls-files`, `git check-ignore -v`, `git log -1 -- <path>`,
  file `mtime` for untracked items. No build ran. No test ran.

---

## 1. Root junk

### 1.1 The finding

The repository root holds **32 scratch artifacts**, 4,733,032 bytes (4.6 MB).

**None of them is tracked.** Every one already matches an existing `.gitignore`
rule. This is the central fact of section 1: nothing slipped into git. The
artifacts are local disk clutter in a working tree, not repository pollution.

Evidence:

```
git ls-files -- '*.ll' '*.shim.c' '*.exe' '*.wasm'     -> (empty)
git ls-files | grep -v /                               -> README.md, ROADMAP.md, .gitignore only
git check-ignore -v argsdemo.ll  -> .gitignore:6:*.ll        argsdemo.ll
git check-ignore -v c1.shim.c    -> .gitignore:27:*.shim.c   c1.shim.c
git check-ignore -v argsdemo.exe -> .gitignore:5:*.exe       argsdemo.exe
git check-ignore -v client.wasm  -> .gitignore:26:*.wasm     client.wasm
```

### 1.2 Inventory

| Group | Files | Size | Last touch (mtime) | Ignored by |
|---|---|---|---|---|
| `client.ll` `client.shim.c` `client.wasm` | 3 | 1.7 MB | 2026-07-23 | `*.ll` `*.shim.c` `*.wasm` |
| `c1` `c2` `c5` `c6` (`.ll` + `.shim.c`) | 8 | 776 KB | 2026-08-05 | `*.ll` `*.shim.c` |
| `scratch_ex` `scratch_sa` `scratch_trap` `scratch_trap2` | 8 | 676 KB | 2026-07-21 | `*.ll` `*.shim.c` |
| `stream` `streamops` `streamunfold` | 6 | 596 KB | 2026-08-02 | `*.ll` `*.shim.c` |
| `argsdemo.exe` `argsdemo.ll` `argsdemo.shim.c` | 3 | 496 KB | 2026-08-07 | `*.exe` `*.ll` `*.shim.c` |
| `cp.ll` `cp.shim.c` | 2 | 276 KB | 2026-08-05 | `*.ll` `*.shim.c` |
| `nested.ll` `nested.shim.c` | 2 | 152 KB | 2026-07-18 | `*.ll` `*.shim.c` |

Note the gap: `c3` and `c4` are absent. Somebody already deleted two members of
that series by hand. That is the current cleanup method.

### 1.3 The same pattern below the root

The census found four more clusters of the same kind. All untracked. All
ignored.

| Path | Size | Note |
|---|---|---|
| `compiler/vyrn-codegen-llvm/target/` | **1.6 GB** | Orphan. The crate source was deleted at `b1eef04` ("compiler: delete the Inkwell backend, and say what the workspace rule really is"). Only the build directory survives. `git ls-files compiler/vyrn-codegen-llvm` is empty. |
| `compiler/ro.ll` `compiler/ro.shim.c` | 44 KB | Same shape as the root scratch, one directory down. |
| `examples/protocol_incomplete.shim.c` `examples/simdint.shim.c` `examples/validate_store.shim.c` | 72 KB | Left by manual builds. The parity harness writes to `std::env::temp_dir()`, so these did not come from the harness. |
| `editor/vscode/vela-win32-x64-0.1.0.vsix` | 1.2 MB | Carries the **pre-rename** name `vela`. Predates the 2026-07-16 rename to Vyrn. |

Total reclaimable disk: about **1.6 GB**, dominated by the orphan `target/`.

### 1.4 Why the artifacts appear, and the rule that would stop them

The ignore rules are complete. No new rule is needed. Adding one would change
nothing, because `git check-ignore` already matches every file.

The cause is the working directory. `vyrn build examples/argsdemo.vyrn` writes
its `.ll` and `.shim.c` sidecars beside the invocation, not beside the source.
Run from the repository root, the sidecars land in the repository root.

`vyrn build` already accepts `-o out` (`compiler/vyrn-cli/src/main.rs:60`).

Three options, cheapest first:

1. **Convention only.** Build into a scratch directory: `vyrn build x.vyrn -o
   ../scratch/x`. Costs nothing. Depends on discipline.
2. **A tracked empty scratch directory.** Add `scratch/` with a `.gitkeep`, and
   an ignore rule `scratch/*` + `!scratch/.gitkeep`. The repository already uses
   this exact pattern for `examples/bin/data/` (`.gitignore:15-21`).
3. **A CLI change.** Place the `.ll` and `.shim.c` sidecars beside the `-o`
   target rather than in the working directory. This is the only option that
   removes the failure mode instead of routing around it. It needs a check of
   the current sidecar path logic before anyone commits to it.

**Recommendation: option 1 now, option 3 as a small follow-up.** Do not add
ignore rules. The ignore file is not the problem.

---

## 2. `rfcs/`

`rfcs/` holds 102 Markdown files: 95 RFCs, 3 dogfood note files, 1 plan file,
3 census files, and `README.md`.

### 2.1 Status classification

Every one of the 95 RFCs carries a `- **Status:**` header. None is missing.

| Class | Count | Files |
|---|---|---|
| Implemented, in whole or in named part | 86 | RFC-0007, RFC-0010 through RFC-0087, RFC-0089 through RFC-0096 |
| Header says "Draft", body says implemented | 6 | RFC-0002, RFC-0003, RFC-0004, RFC-0005, RFC-0008, RFC-0009 |
| Genuinely Draft | 2 | RFC-0001 (vision), RFC-0006 (diagnostics) |
| Superseded | 1 | RFC-0088 → RFC-0089 |
| Refused / Withdrawn | **0** | — |
| Stale draft that went nowhere | **0** | — |
| Duplicate | **0** | — |

**No RFC is junk.** The default disposition — KEEP — holds for all 95. Nothing
in `rfcs/` qualifies for deletion.

### 2.2 The six headers that contradict themselves

Each of these six says "Draft" and then says the opposite in the same sentence.
The word "Draft" is the stale part, not the note that follows it.

| File | Header, as written | Last touch |
|---|---|---|
| RFC-0002 | `Draft — **structural records implemented end to end in v0.1**` | `87d0533` 2026-08-04 |
| RFC-0003 | `Draft — **core implemented in v0.1** (see below)` | `40c1439` 2026-07-16 |
| RFC-0004 | `Draft — capabilities and structured concurrency ship. **§4 and §5's…` | `259330b` 2026-08-09 |
| RFC-0005 | `Draft — **`Option`, `Result`, `match`, and `?` implemented in v0.1**` | `40c1439` 2026-07-16 |
| RFC-0008 | `Draft — **leveled logger + threshold + single sink…` | `40c1439` 2026-07-16 |
| RFC-0009 | `Draft — **`Issue` + `Validation<T>` implemented**` | `40c1439` 2026-07-16 |

RFC-0004 is the interesting one. Its "Draft" is now wrong in a stronger sense:
the memory model it left open was settled by the RFC-0087 to RFC-0091 arc, and
RFC-0004's §4 Path B was **deleted** by RFC-0090 M4. See section 3.

Disposition: **update the six headers**, one word each. Keep the notes.

### 2.3 The RFC-0034 / RFC-0067 pair

Two RFCs share a slug:

- `RFC-0034-soft-navigation.md` — Status: `Implemented`
- `RFC-0067-soft-navigation.md` — Status: `Implemented`

RFC-0067 replaces RFC-0034's design and says so:

> line 77: `**The v2 model (replacing RFC-0034's body morph).**`
> line 112: `(This *changes* RFC-0034, which…`
> line 131: `**Prefetch dropped (v2 simplification).** RFC-0034's hover/focus prefetch…`

RFC-0034 does not point forward. `grep -n "0067" RFC-0034-soft-navigation.md`
returns nothing.

This is not a duplicate to delete. It is a missing back-reference. A reader who
opens RFC-0034 alone will implement a model that no longer ships.

Disposition: **add one line** to RFC-0034's header, in the form the repository
already uses for RFC-0088: `Superseded in part by RFC-0067`.

### 2.4 The numbering gap

**RFC-0066 does not exist and never did.**

```
git log --oneline --all -- 'rfcs/RFC-0066*'   -> (empty)
grep -rn "0066" rfcs/ ROADMAP.md README.md    -> (empty)
```

The numbers run 0001 to 0096 with exactly this one hole. No other gap. Nothing
references the number. It is a skipped counter, not a lost file.

Disposition: **keep the gap.** Renumbering 30 files to close one hole would
break every cross-reference in the corpus. Record the gap in the index instead.

### 2.5 Partly-shipped RFCs

Twelve RFCs record work that stopped short. Their headers already say so. They
are listed here because a reader of the index needs them, not because anything
is wrong with them.

| RFC | What the header records |
|---|---|
| RFC-0047 | §1–§3 as-landed; §4 diagnosed, blocked |
| RFC-0074 | M1, M2, M3a, M3b, M4a shipped; M4b remains |
| RFC-0075 | M1–M3 shipped; M4 given up |
| RFC-0077 | M0–M2p, M5, M6; M3 and M4 struck |
| RFC-0080 | M1, M2 shipped; M3 shipped in half |
| RFC-0082 | M1 shipped; M2 stopped at its own limit |
| RFC-0084 | M1 and M2 shipped |
| RFC-0085 | M1, M2, M3, M4a shipped; M4b designed |
| RFC-0086 | M1 and M3 implemented; M2 blocked |
| RFC-0091 | M1, M2, M3 implemented; M4 stopped |
| RFC-0093 | M1 and M2 landed |
| RFC-0095 | M1 and M3 built; M2 priced |

### 2.6 The non-RFC files

| File | Size | Last touch | Class |
|---|---|---|---|
| `PLAN-memory-model.md` | 38.5 KB | `d91188f` (RFC-0092 M5) | Header says `**COMPLETE.** Phases 0 to 9, fifteen PRs; **Phase 10 closes the tail.**` and then records that the chain continued into RFC-0092 and RFC-0093. Accurate. **Keep as history.** |
| `NOTES-dogfood-bin.md` | 26.6 KB | `4b5c1d3` | Friction record for `examples/bin`. The app is live and tested (7 test files reference it). **Keep.** |
| `NOTES-dogfood-shelf.md` | 20.8 KB | `e50e0be` | Friction record for `examples/shelf`. App live, 3 test files reference it. **Keep.** |
| `NOTES-dogfood-vlog.md` | 18.5 KB | `b887d83` | Friction record for `examples/vlog.vyrn`. The example exists and is in the parity sweep. **Keep.** |
| `census-builtins.md` | 29.7 KB | `26469be` | Status: measurement only. Became RFC-0094. **Keep.** |
| `census-call-arguments.md` | 32.4 KB | `d2ba1b9` (HEAD) | Status: measured, then implemented; §9 records what landed. **Keep.** |
| `census-regions.md` | 26.5 KB | `ca23563` | Status: measurement only; recommendation closes two census rows. **Keep.** |

All seven are current. None is stale. None duplicates an RFC.

### 2.7 `rfcs/README.md` — the index is badly stale

**Last touch: `0827cd7`, 2026-07-16.** That is 25 days before HEAD, and 70 RFCs
ago.

Three specific defects:

1. **The reading-order table stops at RFC-0025.** It lists 25 rows. The
   directory holds 95 RFCs. **70 RFCs are absent from the index**: RFC-0026
   through RFC-0096, less the RFC-0066 gap. Everything the project built after
   2026-07-16 — the UI layer, the memory-model arc, the direct wasm backend,
   streams, SIMD, containers, contracts — is invisible to a reader who starts
   here, which is exactly what the repository's own `README.md:41` tells a
   reader to do.

2. **The table's own order is wrong.** RFC-0024 appears between RFC-0018 and
   RFC-0019 (lines 33–34). The heading calls the table "Reading order".

3. **The status prose is frozen at RFC-0025.** Lines 50–65 narrate the state of
   the project as of RFC-0025 and end with "RFC-0025 (worker threads) is
   **Implemented** too". The status legend (lines 45–48) lists four states:
   Draft, Accepted, Implemented, Superseded. The corpus in practice also uses
   "Complete as scoped", "Accepted and complete", "Shipped", and per-milestone
   partial states. The legend does not describe what the headers say.

4. **The seven non-RFC files are unlisted.** No mention of `PLAN-memory-model.md`,
   the three `NOTES-dogfood-*.md`, or the three `census-*.md`.

Disposition: **rewrite `rfcs/README.md`.** This is the single highest-value doc
change the census found. It is also low risk: the file is an index, the facts
are all in the RFC headers, and no code reads it.

Suggested shape: keep the prose short, replace the hand-written narrative with
one table of all 95 RFCs (number, title, status word taken from the header), a
second short table for the seven non-RFC documents, and one line recording the
RFC-0066 gap. A generated index would stay current; a hand-written one drifted
in 25 days and will drift again.

---

## 3. Stale docs

### 3.1 `README.md` — stale in seven places

Last touch `553390e`, 2026-08-09. Recent, but the Status section was not
revisited.

| Line | Claim, as written | Current fact |
|---|---|---|
| 8 | "**Vyrn** is a working codename. It is easy to change: the name appears only in these docs and in the crate names under `compiler/`." | False since the rename. The name is in the CLI (`vyrn`), the file extension (`.vyrn`), the manifest (`vyrn.json`), the lock file, `~/.vyrn`, the `VYRN_*` environment variables, the wasm import namespace, `vyrn-lsp.exe`, the VS Code extension, the org and repo name (`vyrn-lang/vyrn`), and 141 example files. |
| 11 | "Vyrn compiles ahead-of-time to native code through LLVM." | Half true, and misleading. The native target still routes textual IR through `clang`. The **wasm target emits the module directly** — no LLVM, no clang, no sysroot (RFC-0077 M5). `ROADMAP.md:18-20` states this correctly; `README.md:11` does not. |
| 41–48 | The `rfcs/` tree listing shows RFC-0001 through RFC-0006. | 95 RFCs. |
| 52 | "`vyrn-codegen/` ← LLVM IR emission via Inkwell (feature-gated)" | The Inkwell backend was **deleted** at `b1eef04`. `vyrn-codegen/` now holds `direct.rs`, `layout.rs`, `toolchain.rs` and `wasm.rs` — the textual IR emitter **and** the direct wasm backend. |
| 49–54 | The repository-layout block lists `rfcs/`, `compiler/`, `examples/`. | Omits `std/` (32 modules), `docs/` (33 committed API pages), `web/` (the browser demos), `editor/` (the VS Code extension), `bench/`. |
| 61–62 | "(34 examples, 145 tests)" | **141** top-level examples in `examples/`; **1,979** `#[test]` attributes under `compiler/`. The README figure is 23 % of the example count and 7 % of the test count. |
| 120–121 | "`s.length` counts *bytes*… `\"é\".length == 2`" | `String.length` was **removed** by RFC-0058. `compiler/vyrn-frontend/src/checker.rs:3828` returns an error for `Type::Str if field == "length"`. The replacements are `byteLength` and `charCount`. This snippet, copied from the README, does not compile. |
| 144–152 | "**Generational references (RFC-0004, Path B)** — a `Ref<T>` is a freely-copyable handle… See `examples/genref.vyrn`" | **Path B was deleted** by RFC-0090 M4. `examples/genref.vyrn:1-7` now reads: "Generational handles — `std/slots` (RFC-0090 M1)… A `Handle<T>` is a freely-copyable value of three words". The type is `Handle<T>` from `std/slots`, not `Ref<T>`. `compiler/vyrn-frontend/tests/primitives.rs:542` records "94 -> 90 when RFC-0090 M4 deleted Path B". |
| 175–178 | "See `compiler/README.md`… and the status of the Inkwell backend (now also builds and runs against an LLVM 22 dev SDK…)" | The backend does not exist. |

Disposition: **update.** The pitch, the design pillars and most of the feature
bullets are sound. Seven claims need replacing and the counts need a source. A
counted claim in prose ("145 tests") rots on a schedule; consider stating the
gate instead of the number, the way `ROADMAP.md:13-15` names the harness.

### 3.2 `ROADMAP.md` — 724 lines, mostly current, two stale regions

Last touch `3a90907`, 2026-08-09. The body is in far better condition than
`README.md`. The RFC-0077 wasm paragraph (lines 17–22) is accurate and current.

| Line | Claim, as written | Current fact |
|---|---|---|
| 9 | "across **53 examples** and **743 tests** (0 warnings)" | **141** examples, **1,979** `#[test]` attributes. Stale by the same mechanism as `README.md:61`. |
| 557–566 | "**Generational references (Path B)** — a `Ref<T>` is a freely-copyable handle… so Path A and Path B are one" | Path B deleted (RFC-0090 M4). Same defect as `README.md:144`. |
| 570 | "(The Inkwell in-memory backend also works now — builds against an…" | Deleted at `b1eef04`. |
| 657–665 | "**Path B — generational references.** ✅ Prototyped… reclaiming aggressively is safe on Path B because a missed…" | Same. The whole "memory model — decided" section (lines 642 onward) predates the RFC-0087→RFC-0091 arc, which replaced the decision it records. |
| 715–724 | The "RFC status" table lists **6 rows**: RFC-0001 to RFC-0006. It is the last thing in the file. | 95 RFCs. The table covers 6 % of them, and its RFC-0004 row ("Surface refinements remain") describes a question the memory-model arc closed. |

Disposition: **update.** Three edits carry most of the value: fix the counts in
the header, rewrite the Path B passages to describe `std/slots`, and either
extend the RFC status table to all 95 or delete it and point at
`rfcs/README.md`. Two indexes of the same corpus, both stale, is worse than one.

### 3.3 `compiler/README.md` — one hard error, one stale model

Last touch `3d013ef`, 2026-08-09. Current in most respects; it already names
RFC-0095 and the linear `Task<T>`.

| Line | Claim, as written | Current fact |
|---|---|---|
| 233–234 | "All three execution paths — the interpreter, the text-IR backend, and the Inkwell backend — must agree." | There are three paths, but they are **interpreter, native (text IR via clang), and direct wasm**. The Inkwell backend is gone. `compiler/vyrn-cli/tests/parity.rs:1-4` names the real three. |
| 43 | "**Generational references** (RFC-0004 §4, Path B): a `Ref<T>` — a freely-copyable…" | Path B deleted. See §3.1. |
| 63, 248 | "`vyrn-codegen` — emits **textual LLVM IR**" | Incomplete, not wrong. `vyrn-codegen` also holds the direct wasm backend since RFC-0077 (`src/direct.rs`, `src/wasm.rs`). The layout block at line 248 does not mention wasm emission at all. |
| 1–2 | "A Rust workspace implementing the **v0.1 subset** of Vyrn." | "v0.1 subset" understates 95 RFCs of shipped work. A judgement call, not a factual error. |

Disposition: **update.** Three lines.

### 3.4 `web/README.md` — current

15.3 KB. It names RFC-0077 M5 at line 6 and describes the direct-emit path
correctly, including the graceful-degradation behaviour and the import counts.
No stale claim found.

Disposition: **keep.**

### 3.5 `docs/api/` — current by construction, not by luck

33 tracked files: `docs/api/index.md` plus 32 pages under `docs/api/std/`.
`std/` holds exactly 32 `.vyrn` modules. The counts match.

More important, CI gates it:

```
.github/workflows/ci.yml:70-74
  # The committed std API docs (docs/api/) must match what `vyrn doc`
  # generates from std/ — regenerated in the same commit as any std doc
  run: cargo run --quiet -p vyrn-cli -- doc --std -o ../docs/api --verify
```

Last touch `d2ba1b9` — the HEAD commit itself.

Disposition: **keep, no action.** This is the only documentation in the
repository that cannot drift. It is the model the other docs do not follow.

### 3.6 `editor/vscode/README.md` — current

Last touch `606d48a`, 2026-08-10. No hits for `inkwell`, `LLVM`, `Path B`,
`codename`, or the stale counts.

Disposition: **keep.**

---

## 4. Other residue

### 4.1 `bench/`

One tracked file: `bench/baseline.json`, 1.2 KB, last touched `79afa5b`
(2026-07-22, RFC-0063). It is the comparison baseline for `vyrn bench --compare`
and the CI benchmark job.

Disposition: **keep.** A baseline is meant to be old. Its age is the point.

### 4.2 `tools/` — 324 MB, correctly ignored, half of it redundant

Ignored by `.gitignore:38 tools/`. Untracked. CI fetches its own copies.

| Item | Size | Still used? |
|---|---|---|
| `wasi-sysroot-25.0/` | 205 MB | Yes. `compiler/vyrn-codegen/src/toolchain.rs:831` and `compiler/vyrn-codegen/tests/layout_vs_clang.rs:145` read `WASI_SYSROOT`. |
| `wasmtime-v46.0.1-x86_64-windows/` | 44 MB | Yes. The parity harness runs the wasm column under it (`VYRN_WASMTIME`). |
| `libclang_rt.builtins-wasm32-wasi-25.0/` | 436 KB | Yes, for the genwasm C shim path. |
| `wasi-sysroot.tar.gz` | **62 MB** | **No.** Already extracted beside it. |
| `wasmtime.zip` | **14 MB** | **No.** Already extracted beside it. |
| `wasi-builtins.tar.gz` | **128 KB** | **No.** Already extracted beside it. |

`.github/workflows/ci.yml:107-108` records that the wasm-parity job "now uses
only the wasmtime out of it — RFC-0077 M5 made `--target wasm` need no clang, no
sysroot and no builtins". The sysroot survives for the genwasm job and the
`layout_vs_clang` verification test, not for the parity column.

Disposition: **delete the three downloaded archives** (76 MB). Keep the three
extracted trees. Keep the ignore rule.

### 4.3 `.claude/`

| Path | Tracked | Note |
|---|---|---|
| `.claude/launch.json` | **Yes** | The only tracked file in the directory. Preview/dev-server configuration. Keep. |
| `.claude/worktrees/` | No | Ignored by `.gitignore:39`. Empty, 4 KB. |
| `.claude/settings.local.json` | No | Ignored by the **user's global** ignore file, not this repository's: `git check-ignore -v` reports the rule comes from `~/.config/git/ignore`, not `.gitignore`. It is therefore ignored on this machine only. A fresh clone on another machine would see it as untracked. |

Disposition: **keep `launch.json`.** Consider adding
`.claude/settings.local.json` to the repository's own `.gitignore`, so the
protection does not depend on one developer's global configuration. Low value,
low cost, one line.

### 4.4 `examples/` — no dead examples

The parity harness does **not** use a hand-written list. It discovers:

```rust
// compiler/vyrn-cli/tests/parity.rs:53-58
let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)…
    .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
```

Every one of the **141** top-level `examples/*.vyrn` files therefore enters the
sweep. Nothing can rot unnoticed by being forgotten.

The three skip lists live in `compiler/vyrn-cli/tests/common/mod.rs` and are
principled, not accumulated:

- `KNOWN_DIVERGENT` — **empty** (`mod.rs:22`), and `ROADMAP.md:16` records that
  it "must stay that way".
- `EXPECTED_CHECK_FAILURE` (`mod.rs:35`) — examples that are supposed to fail a
  check; a separate test asserts each one does fail, with the expected wording.
- `WASM_ONLY` (`mod.rs:131`) — about *hosts*, not divergence, per the comment at
  `mod.rs:124`.

The eleven subdirectories are all reached, by import rather than by the sweep:

| Directory | Reached by |
|---|---|
| `bin/` | 7 test files (`contracts`, `derived`, `memory`, `serve`, `symbolmap`, `universal_pages`, `lsp_e2e`) |
| `shelf/` | `exports.rs`, `genwasm.rs`, `memory.rs` |
| `fullstack/` | `rpc.rs`, `symbolmap.rs` |
| `pages/` | `contracts.rs`, `pages.rs` |
| `twdemo/` | `genwasm.rs`, `tw.rs` |
| `locales/` | `codequotes.rs`, `i18n.rs` |
| `lib/` | imported by `gendemo.vyrn`, `modules.vyrn`, `namespace.vyrn` |
| `rpcsplit/` | imported by `rpcsplit.vyrn` |
| `statemod/` | imported by `statemod.vyrn` |
| `vyxcomp/` | imported by `vyxdemo.vyrn` |
| `data/` | I/O fixture directory (RFC-0014); ignored write targets, tracked read fixtures |

Disposition: **keep everything.** The census found **zero** dead examples. This
is the healthiest part of the repository, and the reason is structural: the
harness reads the directory instead of a list.

### 4.5 `std/` — no dead files

32 modules. All 32 tracked. `git status --porcelain std` is empty: no untracked
file, no stray. Each one has a matching page under `docs/api/std/`, and CI
verifies the match.

Disposition: **keep.**

### 4.6 `web/` — the demo state is sound

14 tracked files (HTML, JS runtimes, `build.ps1`, `README.md`). 11 untracked
`.wasm` modules, 80 KB total, all ignored by `.gitignore:26` and all rebuildable
by `web/build.ps1`.

Disposition: **keep.** Do not delete the `.wasm` files unless the working tree
is being reset; they cost 80 KB and a rebuild costs a toolchain run.

### 4.7 Large ignored build directories

Listed for completeness. These are normal and correctly ignored.

| Path | Size | Verdict |
|---|---|---|
| `compiler/target/` | 4.5 GB | Normal. Keep. |
| `editor/vscode/node_modules/` | 96 MB | Normal. Keep. |
| `compiler/vyrn-codegen-llvm/target/` | 1.6 GB | **Orphan.** See §1.3. |

---

## 5. Disposition table

Ordered by confidence, highest first. "Tracked" is `git ls-files`.

| # | Path | Tracked | Class | Evidence | Risk |
|---|---|---|---|---|---|
| 1 | `compiler/vyrn-codegen-llvm/` | No | **delete** | Crate source deleted at `b1eef04`; only `target/` (1.6 GB) survives; `git ls-files compiler/vyrn-codegen-llvm` empty | None. No source, no build, no reference. |
| 2 | 32 root scratch files (`*.ll`, `*.shim.c`, `argsdemo.exe`, `client.wasm`) | No | **delete** | `git check-ignore -v` matches every one; 4.6 MB; oldest 2026-07-18 | None. Regenerable by `vyrn build`. |
| 3 | `compiler/ro.ll`, `compiler/ro.shim.c` | No | **delete** | `.gitignore:6` and `:27`; 44 KB | None. Regenerable. |
| 4 | `examples/{protocol_incomplete,simdint,validate_store}.shim.c` | No | **delete** | `.gitignore:27`; 72 KB; parity harness writes to `temp_dir()`, so no test reads them | None. Regenerable. |
| 5 | `editor/vscode/vela-win32-x64-0.1.0.vsix` | No | **delete** | Pre-rename `vela` name; `.gitignore:35 *.vsix`; 1.2 MB | None. Rebuild with `scripts/make-vsix.mjs`. |
| 6 | `tools/{wasi-sysroot.tar.gz,wasmtime.zip,wasi-builtins.tar.gz}` | No | **delete** | All three already extracted beside themselves; 76 MB | Low. Re-download from the URLs in `ci.yml:117-119`. |
| 7 | `rfcs/README.md` | Yes | **update** | Last touch `0827cd7` 2026-07-16; index stops at RFC-0025; **70 RFCs unlisted**; 7 non-RFC files unlisted; RFC-0024 out of order at line 33 | None. Index only; no code reads it. Highest value of any item here. |
| 8 | `README.md:61-62`, `ROADMAP.md:9` | Yes | **update** | "34 examples, 145 tests" and "53 examples and 743 tests" against 141 examples and 1,979 `#[test]` attributes | None. Prefer naming the gate over quoting a number. |
| 9 | `README.md:120-121` | Yes | **update** | `s.length` on a String; removed by RFC-0058; `checker.rs:3828` errors on it. The README snippet does not compile. | None. The fix is `byteLength` / `charCount`. |
| 10 | `README.md:52,176-178`; `compiler/README.md:233-234`; `ROADMAP.md:570` | Yes | **update** | Inkwell backend described as live; deleted at `b1eef04` | None. |
| 11 | `README.md:144-152`; `compiler/README.md:43`; `ROADMAP.md:557-566,657-665` | Yes | **update** | Path B `Ref<T>` described as current; deleted by RFC-0090 M4 (`primitives.rs:542`); `examples/genref.vyrn:1-7` now uses `Handle<T>` from `std/slots` | Low. Needs a careful rewrite, not a find-and-replace. |
| 12 | `README.md:8` | Yes | **update** | "working codename… appears only in these docs and in the crate names" — false since the 2026-07-16 rename | None. |
| 13 | `README.md:11` | Yes | **update** | "compiles… through LLVM" omits the direct wasm backend (RFC-0077 M5); `ROADMAP.md:18-20` states it correctly | None. |
| 14 | `README.md:41-54` | Yes | **update** | Layout block lists 6 RFCs of 95, and omits `std/`, `docs/`, `web/`, `editor/`, `bench/` | None. |
| 15 | `ROADMAP.md:715-724` | Yes | **update** | RFC status table holds 6 rows of 95, and is the last thing in the file | Low. Consider deleting it and pointing at `rfcs/README.md` — one index beats two stale ones. |
| 16 | `rfcs/RFC-000{2,3,4,5,8,9}` headers | Yes | **update** | Six headers say "Draft" and then say "implemented" in the same sentence | None. One word each. |
| 17 | `rfcs/RFC-0034-soft-navigation.md` header | Yes | **update** | RFC-0067 says it replaces RFC-0034's model (lines 77, 112, 131); RFC-0034 does not point forward | None. One line, matching RFC-0088's form. |
| 18 | `compiler/README.md:63,248` | Yes | **update** | `vyrn-codegen` described as the textual IR emitter only; it also holds the direct wasm backend (`src/direct.rs`, `src/wasm.rs`) | None. |
| 19 | `.gitignore` | Yes | **keep** | Every root artifact already matches a rule. Nothing slipped through. | None. Adding rules would change nothing. |
| 20 | `.gitignore` — one addition | Yes | **update** (optional) | `.claude/settings.local.json` is ignored only by the user's global ignore file | None. One line removes a machine dependency. |
| 21 | `docs/api/` (33 files) | Yes | **keep** | Gated by `ci.yml:74` (`vyrn doc --std --verify`); last touch is HEAD | None. Cannot drift. |
| 22 | `examples/` (141 top-level + 11 subdirectories) | Yes | **keep** | `parity.rs:53-58` reads the directory; `KNOWN_DIVERGENT` empty; every subdirectory reached by a test or an import | None. Zero dead examples. |
| 23 | `std/` (32 modules) | Yes | **keep** | All tracked, all documented, `git status` clean | None. |
| 24 | `rfcs/` — all 95 RFCs | Yes | **keep** | Every file carries a status header; 0 refused, 0 withdrawn, 0 abandoned drafts, 0 duplicates | None. Historical record. |
| 25 | `rfcs/` — 7 non-RFC documents | Yes | **keep** | 3 dogfood notes (all three subjects still live), 1 plan (marked COMPLETE, records the chain past itself), 3 censuses (2 measurement-only, 1 implemented) | None. |
| 26 | RFC-0066 gap | n/a | **keep** | Number never used; `git log --all` and a full-corpus grep both empty | None. Renumbering would break every cross-reference. Record the gap; do not close it. |
| 27 | `bench/baseline.json` | Yes | **keep** | 1.2 KB; the comparison target for `vyrn bench --compare` and the CI job | None. Old on purpose. |
| 28 | `web/` — 14 tracked, 11 ignored `.wasm` | Mixed | **keep** | `README.md` current (names RFC-0077 M5); `.wasm` rebuildable by `build.ps1` | None. |
| 29 | `editor/vscode/README.md` | Yes | **keep** | Last touch 2026-08-10; no stale claim found | None. |
| 30 | `tools/` extracted trees, `compiler/target/`, `node_modules/` | No | **keep** | Still read by `toolchain.rs:831`, `layout_vs_clang.rs:145`, the parity harness, and the extension build | None. |

---

## 6. What the census found, in four lines

1. **Nothing bad is tracked.** No scratch artifact reached the repository. The
   ignore file works. 4.6 MB of root clutter and 1.6 GB of orphan build output
   sit on one disk, not in git.
2. **`rfcs/` is clean.** 95 RFCs, 95 status headers, zero junk. The **index** is
   the stale part — it is 70 RFCs behind.
3. **The prose docs carry five specific dead facts**: the Inkwell backend, Path B
   `Ref<T>`, `String.length`, "working codename", and two test counts. Each
   appears in more than one file.
4. **`docs/api/` and `examples/` cannot rot**, because a gate reads the directory
   instead of a list. That is the pattern the rest of the documentation should
   copy.
