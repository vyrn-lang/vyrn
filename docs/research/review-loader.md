# Review: the module loader, the manifest, and the supply chain

An external review of Vyrn at `3f4974d`, covering the one area no reviewer has
looked at: `loader.rs`, `project.rs`, `audience.rs`, `contracts.rs`,
`declared.rs`, `prelude.rs`, `symbols.rs`, `origin.rs`, `vyrn-cli/src/remote.rs`,
and the manifest handling in `vyrn-cli/src/main.rs`. Five reviewers, each with
its own values.

This is the trust root. RFC-0010 M4 claims remote imports are sha256-pinned in
`vyrn.lock`, cached in `~/.vyrn`, offline-capable, vendorable, and
"left-pad-proof". RFC-0021 claims a generator is "interpreted, scoped,
deterministic, and pinned". Those claims were attacked, not read.

Every finding carries evidence. A code finding cites `file:line`. A behavioural
finding carries the command that was run and the output it produced. Findings
are ranked **CONFIRMED** (reproduced or measured) above **PLAUSIBLE** (argued
from reading). Where an RFC records a decision with its argument, the entry says
"design critique", not "defect".

The design record was read first: RFC-0010 (modules), RFC-0021 (generators and
the generator cache), RFC-0033 and RFC-0053 (origin maps), RFC-0099 (generator
diagnostics), RFC-0071 (contracts), RFC-0072 (audience and roles).

---

## Top 12 by severity

| # | Severity | Lens | Finding | Ref |
|---|---|---|---|---|
| 1 | **Critical** | Agda | **A client entry point reads a server-only secret, and `vyrn check` says `ok`.** The audience rule is never applied to a generator import edge, and a generated module inherits the audience of the FILE it was given. Mount a `.vyx` under `server/` with `vyxPage("../server/pages/Leak.vyx")` and the client build prints `TOP-SECRET`. `vyrn why` reports that nothing reaches the file. | A5.1 |
| 2 | **Critical** | C systems | **The generator cache serves unauthenticated code.** An entry with zero recorded inputs validates vacuously, so any file dropped in `~/.vyrn/cache/gen` becomes the module the compiler links — permanently, and `emit-gen` shows it as if the generator wrote it. Nothing re-verifies a `gen` entry, while every remote blob beside it is hash-checked on every load. | C2.1 |
| 3 | **Critical** | Rust | **A `vyrn.json` in any ancestor directory aborts the compiler** with a stack overflow (exit 127, no diagnostic). `find_manifest` walks up from the cwd on every command, and the hand-written JSON parser has no depth limit. | R3.1 |
| 4 | High | Rust / Agda | **A JSON typo turns the audience boundary off and the build says `ok`.** A malformed manifest is treated as no manifest, so the rule that keeps server-only code out of a client bundle disappears with a trailing comma. | R3.2 |
| 5 | High | Agda | **The audience of a file is decided by the key STRING.** A second spelling of the same file — different case on Windows, or a directory junction — loses its audience with no diagnostic, while `vyrn why` (which canonicalizes) still calls the file server-only. | A5.2 |
| 6 | High | C systems | **A duplicate line in `vyrn.lock` silently wins.** The second entry for one specifier replaces the first with no diagnostic, so appending a line to the lock — a diff that never touches the original pin — changes the code that is built. | C2.2 |
| 7 | High | C systems | **A damaged `vyrn.lock` line is a silent re-pin, not an error.** Tabs turned to spaces, a truncated write, an unreadable file: all become "this specifier was never pinned", and the next online build fetches whatever upstream serves now and writes it back as the pin. | C2.3 |
| 8 | High | Agda | **A `.vyx` string literal can fail the build at any path, with any wording.** `//@diag` and `//@origin` are scanned out of the generated text with no lexical context, so data a trusted generator copies through becomes a compiler error naming a file outside the project. | A5.3, A5.4 |
| 9 | High | C systems | **`vyrn update` ignores `--offline` and `VYRN_OFFLINE`.** The one command that changes a pin hard-codes `offline: false` and shells out to `curl`. | C2.4 |
| 10 | High | C systems | **The standard library is the one dependency that is not pinned.** It is found by walking up to five parents of the executable looking for a directory named `std`. A planted directory replaces the whole standard library with no diagnostic. | C2.5 |
| 11 | High | PL / Rust | **A module key is an English sentence that three functions parse back.** A project directory named `gen at home` makes the loader link a different module and exit 0. | P4.1, R3.3 |
| 12 | High | Linus | **`resolve_aliases` rewrites a whole module's AST once per renamed name.** 27x at 800 shared private names (2965 ms against a 111 ms control), and the rename is automatic, not requested. | L1.1 |

Below the line, and worth naming: a corrupt generator-cache entry is permanent
and no command clears it (C2.6, R3.4); the compiler's advice for an audience
error names a library and a directory the project never chose (P4.2); `vyrn deps`
cannot resolve a remote import at all (C2.9); and `vyrn why` drops every
generator edge whose argument is a `.vyx` file — the exact chain that leaks in
finding 1 (A5.9).

Counts: **38 CONFIRMED**, **8 PLAUSIBLE**, of which 9 are design critiques.

---

## Lens 1 — Linus Torvalds: taste and performance

Timings are from `compiler/target/release/vyrn.exe` at `3f4974d`, on synthetic
projects generated in a scratch directory.

### L1.1 CONFIRMED — High. `resolve_aliases` rewrites a whole module's AST once per renamed name

`loader.rs:2273-2282` loops over every entry in `foreign_renames` and calls
`rename_decl_in_module` (`loader.rs:4155-4184`), which ends in
`rewrite_module_refs(p, &map, ns)` with a **one-entry** map — a full walk of
every function body, impl, type, global, test and bench in the target module, to
substitute one name. R renames against a module of size A cost O(R × A).

The rename count is not exotic. RFC-0046 §3 name privacy renames every
non-exported top-level name that also appears in another module
(`loader.rs:2163-2228`), automatically. Two modules, each with N private
functions, the same N names in both:

| N | with renames | control (names distinct) |
|---|---|---|
| 50 | 98 ms | 43 ms |
| 100 | 110 ms | 46 ms |
| 200 | 243 ms | 55 ms |
| 400 | 645 ms | 72 ms |
| 800 | 2965 ms | 111 ms |

Per-doubling ratio reaches 4.60 against the control's 1.54. Holding the AST fixed
at 800 functions per module and varying only how many names collide gives 79 /
109 / 142 / 205 / 339 ms for 0 / 100 / 200 / 400 / 800 collisions — exactly
linear in the rename count at fixed size, which pins the cost to one whole-module
walk per rename. Build the per-module rename map first, then walk each module
once.

Today's repo has only 8 private names shared across `std/*.vyrn`, so this is
latent rather than burning.

### L1.2 CONFIRMED — Medium. `link` rebuilds the "names I own" set from the whole program per module

`loader.rs:3007-3011`, inside the `for m in &modules` loop at `loader.rs:2919`:

```rust
let own: HashSet<&str> = owner
    .iter()
    .filter(|(_, (module, _))| module == &m.key)
    .map(|(n, _)| n.as_str())
    .collect();
```

`owner` holds every top-level name in the program, so this is O(modules × total
top-level names). Isolated by fixing the module count and adding declarations:

| modules | extra decls each | ms |
|---|---|---|
| 4096 | 0 | 487 |
| 4096 | 8 | 1420 |
| 512 | 0 | 89 |
| 512 | 64 | 399 |

The same declarations cost more when spread over more modules. Fitting gives
about 5.5 ns per (declaration × module). That term also explains the whole-project
curve — N modules each importing one other: 256/512/1024/2048/4096/8192/16384/32768
modules take 75/100/171/290/574/1215/3189/9583 ms, and the per-doubling ratio
leaves 2.00 exactly where the predicted N² term crosses the linear one. Real
projects sit far below the crossover, so this is a taste finding: group `owner`
by module once, before the loop. `loader.rs:2932-2938` repeats the same scan per
namespace import (L1.7).

### L1.3 CONFIRMED — Low. SHA-256 exists twice, byte for byte, in two crates that already depend on each other

`vyrn-frontend/src/hash.rs:18` and `vyrn-cli/src/remote.rs:43` are the same
90-line FIPS 180-4 implementation with the same `K` table and the same NIST
vector test (`hash.rs:64`, `remote.rs:380`). `vyrn-cli/Cargo.toml:12` already
declares the frontend dependency and `hash::sha256_hex` is `pub`. The frontend's
own doc comment states the duplication as a fact without giving a reason. RFC-0010's
"zero new crates" argues for writing SHA-256 once; it does not argue for twice.

Both copies also do `let mut msg = data.to_vec()` before hashing — a full copy of
every input, paid on every generator-cache input hash (`loader.rs:1710-1720`) and
every remote blob verification — and end with eight `format!` calls per digest.

### L1.4 PLAUSIBLE — Medium. Every module's AST is walked for a check that only applies to aliased imports

`loader.rs:2102` computes `program_ref_names(&m.program)` unconditionally per
module. That function (`loader.rs:4097-4148`) walks every body, impl, type,
global, test and bench and builds a `HashSet<String>` of referenced names. Its
only consumer is the loop below at `loader.rs:2103-2126`, which does nothing
unless an import has an alias. Most modules have none, so most of these walks and
their allocated sets are discarded. Hoisting the walk behind
`imports.iter().flat_map(|i| &i.names).any(|n| n.alias.is_some())` is two lines.

The loader also scans every module's AST again at `loader.rs:1274-1277` for the
injected-runtime decision, so a module is walked at least twice after parsing for
two features most modules never use.

### L1.5 PLAUSIBLE — Medium. The runtime-injection probe reads each runtime module, then the loader reads it again

`loader.rs:1300`:

```rust
if !states.contains_key(&target) && resolver.read(&target).is_err() { continue; }
```

The whole file is read and allocated to answer "does this exist?", then dropped;
four lines later `visit` reads it again (`loader.rs:891`). `FsResolver::read`
(`main.rs:518`) is a bare `std::fs::read_to_string` with no cache, so both reads
hit the disk. The runtime table lists `std/json` (15 KB), `std/jsondec` (16 KB),
`std/text` (16 KB) and `std/num` (35 KB), and its own comment says nearly every
program links `std/num` because `print` triggers it. Measured cost of the whole
injection is 4 ms for a `print`-only program, so the doubled read is a few
hundred microseconds — the objection is the shape. `ModuleResolver` has no
"exists" question, and inventing one by reading a file and throwing it away is
the wrong answer to it; `visit` already accepts `Some(source)`.

### L1.6 PLAUSIBLE — Medium. `moduleInterface` materializes each reflected module's source about three times

`interp.rs:1795-1828` reads the module, pushes `source.clone().into_bytes()`,
then runs a fresh `loader::load` through a `RecordingResolver`, which clones the
entire source on every successful read (`loader.rs:145-153`). `into_reads`
(`loader.rs:135-143`) then deduplicates by path — an admission that one link
reads the same key more than once — and each survivor is cloned twice more at
`interp.rs:1825-1826`. Each call starts a new load with a fresh `states` map, so
the reflected module's whole import closure, std included, is re-read per call.
The loader already knows this pattern hurts: the `HASH_MEMO` doc
(`loader.rs:1686-1691`) records "a root that imports seven generators validates
the same std modules seven times — 8.6 ms of a 20 ms load" and memoizes the hash
path. The load path got no such memo.

### L1.7 PLAUSIBLE — Low. Namespace-import visibility scans the whole program name table per namespace import

`loader.rs:2932-2938` is L1.2 again, inside the per-module import loop, plus a
`String` clone per granted name. `resolve_aliases` already builds the map this
wants — `module_exports` (`loader.rs:1822`) — and does not pass it along.

### What this lens found clean

* **The three caches in scope hit.** `examples/shelf/server.vyrn` with a scratch
  `VYRN_GEN_CACHE_DIR` deleted first: 918 ms cold writing 10 entries, then 275 /
  307 / 298 ms warm reusing all 10. `PARSE_CACHE` (`loader.rs:1001-1031`) and
  `HASH_MEMO` (`loader.rs:1692-1707`) each carry a recorded argument and do what
  they claim.
* **The hand-rolled `normalize` is right to be hand-rolled** (`loader.rs:168-191`,
  design critique): module keys are lexical slash paths that must behave
  identically for in-memory resolvers, which `std::path` would not give.

---

## Lens 2 — C systems reviewer: resource and trust discipline

Every experiment ran against `compiler/target/release/vyrn.exe` built from
`3f4974d`, in a scratch directory outside the repository. No experiment needed
the network except where the output shows a fetch.

The headline claim survives its main attack, and that is worth stating first.
A cache or vendor blob that is mutated after it is written is refused loudly, on
every load, and the verification is sound: `read_blob`
(`compiler/vyrn-cli/src/remote.rs:198-209`) hashes **the same buffer it
returns**, so there is no window between the check and the use.

```
$ cp pad_evil.vyrn vyrn_vendor/sha256/1be76a52...b50b   # keep the name, change the bytes
$ vyrn run --offline
...: cannot load `https://127.0.0.1:9/pad.vyrn`: cached copy at `...\vyrn_vendor/sha256\1be76a52...`
does not match its recorded sha256 — delete it and re-fetch (or restore a good copy: any file
hashing 1be76a52... works)
exit=1
$ vyrn vendor --check
corrupt vendor blob for `https://127.0.0.1:9/pad.vyrn` (1be76a52...)
1 entry not vendored
exit=1
```

The failures are everywhere else: in the cache that is *not* content-addressed,
in the lock file's format, and in the two dependencies that were never pinned at
all.

### C2.1 CONFIRMED — Critical. The generator cache serves code nobody verified, and the entry is permanent

`compiler/vyrn-cli/src/remote.rs:170-179` reads and writes `~/.vyrn/cache/gen/<key>`
as a plain file. The key is `sha256(generator module ++ name ++ args ++ resolved
input roots)` (`loader.rs:1660-1679`) — it does **not** cover the entry's
contents. Validation is `loader.rs:1520-1531`:

```rust
if let Some((inputs, output)) = parse_cache_entry(&cached) {
    if inputs.iter().all(|(path, hash)| {
        current_input_hash(resolver, path).unwrap_or_else(|| ABSENT.to_string()) == *hash
    }) {
        return Ok((gen_key, Some(output)));
    }
}
```

`inputs` comes from the entry. An entry that declares **zero** inputs passes
`all` vacuously, and its `output` is linked into the program as the synthesized
module. Reproduced with `examples/gendemo.vyrn` copied to a scratch directory:

```
$ VYRN_GEN_CACHE_DIR=$S/gencache vyrn run gendemo.vyrn
3
2
dark.txt

$ printf 'v2 0\nexport fn colorCount() -> Int64 { return 999 }\n...\
export fn firstTheme() -> String { return "pwned by a cache entry nobody signed" }\n' \
    > $S/gencache/0d26f0bc38aa19f064812e4c33bfed2927fe9c78f6105cc5ca67c8f5ea606c4a

$ VYRN_GEN_CACHE_DIR=$S/gencache vyrn run gendemo.vyrn
999
999
pwned by a cache entry nobody signed
exit=0
```

Three properties make this worse than a stale cache:

* **It is permanent.** A hit never rewrites the entry, and the generator never
  runs again. Every later build and every LSP keystroke reuses it.
* **The inspection tool agrees with the attacker.** `vyrn emit-gen` prints the
  cache entry, not the generator's output:
  ```
  $ VYRN_GEN_CACHE_DIR=$S/gencache vyrn emit-gen gendemo.vyrn
  // ==== generated by palette("./data") at gendemo.vyrn ====
  export fn colorCount() -> Int64 { return 999 }
  ...
  ```
  RFC-0021 offers `emit-gen` as the way to see what a generator produced. With a
  poisoned entry there is no command that shows the difference.
* **It is the only cache in the design that is not content-addressed.** The
  sha256 blob cache one directory over is verified on every load (see above).
  The `gen` directory holds compiler input with no hash, no signature, and no
  provenance, and `remote.rs:159-167` lets `VYRN_GEN_CACHE_DIR` move it — an
  environment variable in a shell profile or a CI file is enough.

The write side does not need privilege either: any process running as the user
can create the file. RFC-0021 describes the entry as carrying "the generation's
own dependency record" and validating it. It validates the record the entry
chose to carry.

The fix is one line of the same discipline the neighbouring cache already
applies: make the entry content-addressed (key includes the output hash), or
refuse an entry with no recorded inputs, since a real generation always records
at least the generator's own sources (`loader.rs:1638`).

### C2.2 CONFIRMED — High. A duplicate line in `vyrn.lock` silently wins

`Lock::load` (`remote.rs:117-134`) inserts into a `BTreeMap` keyed by the
specifier, with no duplicate check. The last line for a specifier replaces every
line above it, silently.

```
$ cat vyrn.lock
https://127.0.0.1:9/pad.vyrn<TAB>https://127.0.0.1:9/pad.vyrn<TAB>1be76a52...b50b   # the good module
https://127.0.0.1:9/pad.vyrn<TAB>https://127.0.0.1:9/pad.vyrn<TAB>f7cdc5fd...5651   # a different one

$ vyrn run --offline
EVIL:x
exit=0
```

A lock file is reviewed as a diff. This is a change to a dependency that adds a
line and edits none, leaves the original pin present and correct in the file, and
produces no warning at build time. Any tool that greps the lock for a specifier
finds the honest line first. `Lock::save` writes one line per specifier, so the
duplicate disappears the next time anything marks the lock dirty — the evidence
removes itself.

A lock file is a format with exactly one job. Two entries for one specifier is a
hard error, and the line number is available where the collision is detected.

### C2.3 CONFIRMED — High. A damaged lock line is a silent re-pin, not an error

`Lock::load` swallows three distinct failures (`remote.rs:119-127`): an
unreadable file (`if let Ok(text)`), a line that does not split into three
tab-separated fields (`if let (Some, Some, Some)` with no `else`), and the
duplicate above. All three mean "this specifier was never pinned", and an
unpinned specifier is fetched from the network and pinned to whatever arrives.

```
$ cat vyrn.lock        # one entry, tabs replaced by spaces
github:vyrn-lang/vyrn@0000...0000/std/pad.vyrn https://... 1be76a52...

$ vyrn check
...: cannot load `github:vyrn-lang/vyrn@0000.../std/pad.vyrn`:
fetch failed for https://raw.githubusercontent.com/vyrn-lang/vyrn/0000.../std/pad.vyrn
(curl exit Some(22))
exit=1
```

The pin was in the file. The build went to the network. Only a 404 stopped it,
and on success `save_lock` (`main.rs:2223-2233`) would have written the new pin
and printed `pinned new remote imports`, which is the same message a first
resolve prints.

Tabs become spaces through a copy-paste, an editor, a merge tool, or a CI
checkout that filters whitespace. `Lock::save` (`remote.rs:136-142`) also writes
through `std::fs::write` with no temporary file and no rename, so a power loss
during the write leaves a truncated lock — which is the same silent unpinning,
reached without anyone touching the file. The lock is the artifact whose whole
purpose is that it cannot drift, and every way of damaging it fails toward the
network.

`Lock::load` should return a `Result` that names the line it could not read.

### C2.4 CONFIRMED — High. `vyrn update` ignores `--offline` and `VYRN_OFFLINE`

`main.rs:2643-2647` builds the resolver with `offline: false` written in:

```rust
let resolver = remote::RemoteResolver {
    lock: std::cell::RefCell::new(lock),
    project_dir,
    offline: false,
};
```

Both spellings of the flag are accepted and both are ignored. Exit 7 is `curl`
reporting a refused connection, so the process reached the network:

```
$ vyrn update --offline
re-resolving `pad` (https://127.0.0.1:9/pad.vyrn)
error: fetch failed for https://127.0.0.1:9/pad.vyrn (curl exit Some(7))
exit=1

$ VYRN_OFFLINE=1 vyrn update
re-resolving `pad` (https://127.0.0.1:9/pad.vyrn)
error: fetch failed for https://127.0.0.1:9/pad.vyrn (curl exit Some(7))
exit=1
```

`vyrn add` gets this right by accident: it also ignores its own `_offline`
parameter (`main.rs:2531`), but `make_resolver` reads the environment variable
that `main` sets, so the refusal happens anyway:

```
$ vyrn add --offline https://127.0.0.1:9/other.vyrn --name other
error: `https://127.0.0.1:9/other.vyrn` is not in vyrn.lock and this is an offline build
```

Re-resolving a pin offline is impossible, so the right behaviour is a refusal
naming the flag. Silently going to the network is the one thing `--offline`
exists to prevent, and a sandboxed or air-gapped build that runs `vyrn update`
gets a network call it asked not to have.

### C2.5 CONFIRMED — High. The standard library is located by a directory-name search

`std_root()` (`main.rs:536-551`) takes `$VYRN_STD` if set, else walks up to five
parents of the executable and returns the first directory named `std`:

```rust
let mut dir = std::env::current_exe().ok()?;
for _ in 0..5 {
    dir = dir.parent()?.to_path_buf();
    let cand = dir.join("std");
    if cand.is_dir() { return Some(cand.to_string_lossy().replace('\\', "/")); }
}
```

There is no hash, no lock entry, no version, and no output naming the std root
that was used. Reproduced by copying the binary to `rogue/bin/` and putting a
`std/math.vyrn` in `rogue/std/`:

```
$ cat rogue/std/math.vyrn      # export fn abs(x: Int64) -> Int64 { return 1337 }
$ cat rogue/app/m.vyrn         # import { abs } from "std/math"; print(abs(0 - 7))

$ rogue/bin/vyrn.exe run m.vyrn
1337
$ compiler/target/release/vyrn.exe run m.vyrn
7
```

Every remote byte in the design is content-addressed and pinned. The largest
dependency of every Vyrn program — the standard library, which supplies
`std/json`, `std/rpc`, `std/connect`, and the runtime modules the backends inject
(`loader.rs:563`) — is selected by a folder name near the binary. Five levels up
from `C:\Users\me\bin\vyrn.exe` reaches the drive root. `VYRN_STD` overrides it
with no check at all.

The std root belongs in the build's recorded inputs, and `vyrn check` should be
able to print which one it used.

### C2.6 CONFIRMED — Medium. A half-written generator-cache entry breaks the project forever and blames the generator

`gen_cache_put` (`remote.rs:175-179`) writes with `std::fs::write`, no temporary
file, no rename. The entry format (`loader.rs:1737-1744`) records the input count
but nothing about the output's length or hash, so a truncated entry parses. A
power loss, a full disk, or a killed build during the write produces this:

```
$ head -c 700 <entry> > entry.tmp && mv entry.tmp <entry>       # simulate the truncated write
$ vyrn run gendemo.vyrn
generated by palette("./data") at gendemo.vyrn:4:40: expected RBrace, found Eof
exit=1

$ vyrn run gendemo.vyrn                                          # again: identical
generated by palette("./data") at gendemo.vyrn:4:40: expected RBrace, found Eof
exit=1

$ VYRN_NO_GEN_CACHE=1 vyrn run gendemo.vyrn                      # works
3
2
dark.txt

$ vyrn run gendemo.vyrn                                          # and it is still broken
generated by palette("./data") at gendemo.vyrn:4:40: expected RBrace, found Eof
exit=1
```

The diagnostic names the generator and never mentions the cache. The documented
escape hatch runs the generator but does not rewrite the entry
(`loader.rs:1620`, the write is inside `if !no_cache`), so the project stays
broken. There is no `vyrn cache clean`; recovery is deleting a directory under
`~/.vyrn` that no diagnostic names. Writing to a temporary file and renaming
gives atomicity on both platforms and costs two lines.

### C2.7 CONFIRMED — Medium. A locked specifier is never re-derived, so the lock line decides what runs

`read_remote` (`remote.rs:300-330`) uses the lock's URL and hash directly and
calls `resolve_to_url` only on the unlocked path. Nothing checks that the URL
recorded for `github:owner/repo@<sha>/path` is the URL that specifier derives —
even though for a 40-hex ref the derivation is pure string work and needs no
network (`remote.rs:231-247`).

```
$ cat src/main.vyrn
import { pad } from "github:vyrn-lang/vyrn@0000000000000000000000000000000000000000/std/pad.vyrn"

$ cat vyrn.lock
github:vyrn-lang/vyrn@0000...0000/std/pad.vyrn<TAB>https://evil.example.invalid/whatever<TAB>f7cdc5fd...

$ vyrn run --offline
EVIL:x
exit=0
```

The import a reviewer reads says `vyrn-lang/vyrn` at a named commit. The code
that runs is whatever hashes to the lock's third field. That the content pin is
authoritative is a defensible design — every lock file works this way — but two
things make it weaker here than it needs to be: an immutable specifier's URL is
derivable and is not derived, and the hash field is never validated as 64 hex
characters. A lock whose hash field is a path is used as a path:

```
$ cat vyrn.lock          # third field: ../../pad_evil.vyrn
$ vyrn run --offline
...: cached copy at `...\vyrn_vendor/sha256\../../pad_evil.vyrn` does not match its recorded
sha256 — delete it and re-fetch (or restore a good copy: any file hashing ../../pad_evil.vyrn works)
```

The hash check stops the content from being used — no content hashes to a
non-hex string — so this is not an escape. It is an unvalidated path join
(`read_blob`, `remote.rs:199`) that reads a file of the lock's choosing and
distinguishes "exists but mismatched" from "not there" in its error, and a
message that tells the user to find a file hashing `../../pad_evil.vyrn`. One
`is_ascii_hexdigit` check at load time removes the whole class.

### C2.8 CONFIRMED — Medium. `curl` runs with no timeout, no size cap, and no protocol restriction

`fetch` (`remote.rs:271-283`) is `curl -sL --fail <url>` and nothing else. There
is no `--max-time`, no `--max-filesize`, no `--proto '=https'`, and no limit on
the redirect chain beyond curl's default of 50. A host that answers slowly, or
answers forever, hangs or exhausts the build, and the failure looks like a hang
rather than a fetch. This is reachable from any `https:` specifier in any
transitively imported module.

Two smaller notes on the same call. The URL is passed as one argv element with
no shell, so a specifier cannot inject a command; and because every URL is built
with an `https://` prefix (`remote.rs:245-266`) it cannot begin with `-` and be
read as an option. But curl's URL globbing is still on, so one specifier can
become several requests whose bodies are concatenated:

```
$ curl -sL --fail -v "https://127.0.0.1:9/{a,b}.vyrn" 2>&1 | grep Trying
*   Trying 127.0.0.1:9...
*   Trying 127.0.0.1:9...
```

The lock records one specifier and one hash for that concatenation. `--globoff`
is one flag.

The `git ls-remote` shell-out (`remote.rs:234-237`) passes the user's ref as an
argv element after the repository URL. I tested whether git parses an option in
that position, because that would be argument injection:

```
$ git ls-remote <local repo> --get-url        # (nothing: treated as a ref pattern)
$ git ls-remote --get-url <local repo>        # control
C:/.../scratchpad/gitinj/repo
```

`git ls-remote` stops option parsing at the first positional, so the ref cannot
smuggle `--upload-pack`. That is git's discipline, not the caller's; validating
the ref shape is still one line.

### C2.9 CONFIRMED — Medium. `vyrn deps` cannot resolve a remote import

`deps()` (`main.rs:1532`) passes `&FsResolver` — the plain filesystem resolver —
where every building command passes `make_resolver`. A remote key is then handed
to `std::fs::read_to_string`:

```
$ vyrn deps
...: cannot load `github:vyrn-lang/vyrn@0000.../std/pad.vyrn`:
The filename, directory name, or volume label syntax is incorrect. (os error 123)
exit=1
```

RFC-0010 M3 offers `vyrn deps` as the way to print the resolved module graph.
It works for exactly the dependencies that need no review and fails on the ones
that do, with an OS error rather than a diagnostic. It also prints no hash and no
URL, so there is no command in the toolchain that shows what a specifier
currently resolves to.

### C2.10 PLAUSIBLE — Low. The gen cache is one namespace for every project on the machine

Cache entries record local inputs by the key the loader used, which is a
**relative** path whenever the build was started with a relative root — the
common case:

```
$ head -3 ~/.vyrn/cache/gen/0d26f0bc...
v2 5
data/palette.csv	535ced3a...
N:/lang/.../std/text.vyrn	e427b535...
```

The lookup key is built from the same relative strings, so two projects with the
same layout, generator name, and arguments share one entry, and validation
re-reads `data/palette.csv` relative to whatever the current directory is. I
could not turn this into a wrong build: every recorded input must still hash as
it did, which forces the generator's own sources and its inputs to be identical,
and then the output is identical too. It is the reason C2.1 needs no privilege
to aim, though: the key is a pure function of strings an attacker can read off a
project.

### What this lens found correct

* **Verify-then-use is sound.** `read_blob` hashes the buffer it returns
  (`remote.rs:198-209`). There is no TOCTOU window between the check and the
  parse, on any of the three paths (vendor, cache, freshly fetched).
* **The fetched-content check is a refusal, not a repair.** A fetch whose hash
  disagrees with the lock fails the build and names `vyrn update` as the
  deliberate way to accept it (`remote.rs:320-327`).
* **`--offline` holds for every command that builds.** `check`, `run`, `build`,
  `test`, `emit-gen` all route through `make_resolver`, and an unlocked remote
  import offline is a clear diagnostic naming the specifier. Only `update`
  escapes (C2.4).
* **The remote sandbox holds.** A relative import inside a remote module is
  resolved against the pinned base and rejected if it escapes, including through
  `..` that normalization cannot pop (`loader.rs:392-406`); a remote module
  cannot use bare specifiers, because it has no manifest (`loader.rs:418`); and
  `http:` is refused with a message naming `https` (`loader.rs:382-384`).
* **No shell is involved.** `curl` and `git` are invoked through
  `std::process::Command` with argv elements, so no specifier can inject a
  command.
* **The hand-rolled SHA-256 is right.** It is checked against three NIST vectors
  and a 64-byte input crossing the padding boundary (`remote.rs:379-398`); its
  output matched `sha256sum` on every fixture used in this review.
* **`vyrn vendor --check` verifies rather than trusts.** It re-hashes every
  vendored blob and reports a corrupt one by specifier (`main.rs:2673-2685`).
