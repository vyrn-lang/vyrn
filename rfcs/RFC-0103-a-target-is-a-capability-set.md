# RFC-0103 — A Target Is a Capability Set

- **Status:** **Proposed.** No implementation. Milestones below; a milestone
  that fails its gate says so in this file.
- **Depends on:** RFC-0072 (audience — kept, and demoted to what it is),
  RFC-0071 (module contracts and roles, untouched), RFC-0014 (input I/O — the
  builtins the floor watches), RFC-0043 (time/random), RFC-0044 (storage),
  RFC-0012 (`extern` — the wasm-only direction), RFC-0084 (sse/ws).
- **Evidence (user):** "this doesn't make sense, user still can include secrets
  in client or whatever", "in gradle it is checked too and you should configure
  it too in Vyrn", "it looks like something almost gradle modules just more
  fuzzy? How it actually should be? To be clear, easily usable, reliable".

---

## The problem

RFC-0072 shipped audience: path segments declared in `vyrn.json`, an
edge check, `vyrn why`. It works, and roles and derived RPC stand on it. But
its own document overclaims what the mechanism guarantees. It says a rejected
import prevents "a leaked secret at worst". It does not. Two facts, both
surfaced by the user, bound what any such system can do:

1. **The compiler does not know what a secret is.** A string literal pasted
   into a client module is invisible to every checker ever built. Audience
   checks import edges; it cannot check intent.
2. **A label is configuration.** `server/` means server-only because
   `vyrn.json` says so, and whoever edits `vyrn.json` can say otherwise. A
   check whose premise is user-editable is a fence, not a floor. Gradle has
   exactly this: declared project boundaries, checked conformance, no
   knowledge of what an edge means. Audience as shipped is Gradle with better
   diagnostics.

A fence is worth having. But the honest design names a second layer under it —
one the user cannot relabel, because it is not a label. That layer is the
target.

## The design: a floor and a fence

**The floor (new).** An artifact is a real thing: an entry point and a target.
A target is a capability set — a fact about where the code runs, not a policy
about who should read it. A browser page has no filesystem. That is physics,
not configuration, and `web/wasi-min.js` already lives it: `path_open` answers
`NOENT`, `fd_read` on stdin answers EOF, `args_get` answers an empty list,
while the clock and the CSPRNG work. The floor makes that answer arrive at
compile time instead of runtime: every module reachable from an artifact's
entry must need only capabilities the artifact's target has.

**The fence (shipped, RFC-0072).** The audience map stays exactly as it is:
declared segments, edge check, nearest-wins, `vyrn why`. What changes is the
claim made for it. It prevents the *accidental* class — a client file
importing a helper that imports a config module, five hops the author never
saw. It does not prevent the deliberate class, and its documentation stops
saying it does.

The two layers fail differently, and that is the point. The fence fails when
the manifest is wrong. The floor cannot be wrong that way: nobody can grant a
browser a filesystem by editing JSON.

### §1 Artifacts

```json
{
  "artifacts": {
    "api": { "entry": "server/main.vyrn", "target": "native" },
    "app": { "entry": "client/boot.vyrn", "target": "browser" }
  }
}
```

- `target` is one of `native`, `wasi`, `browser`. The first two exist today as
  build targets; `browser` is today spelled "wasm plus wasi-min.js" and gets a
  name.
- The existing manifest keys are sugar and stay: `main` and `server` are
  native artifacts, `client` is a browser artifact. A project that writes only
  those keys is already using artifacts and never sees the new spelling.
- Opt-in stays absolute: a manifest with no entry-point keys and no
  `artifacts` map gets no floor check, exactly as a manifest with no
  `audience` key gets no fence.

### §2 Capabilities

A capability names a way out of the program. The vocabulary is small and M0's
census fixes it, but the shape is known from `wasi-min.js` and the prelude:

| capability | carried by (today) | native | wasi | browser |
|---|---|---|---|---|
| `fs` | `readFile`, `writeFile`, `readFileBytes`, `std/storage` | yes | yes | **no** — `NOENT` at runtime today |
| `stdin` | `readLine` | yes | yes | **no** — EOF at runtime today |
| `args` | `args` | yes | yes | **no** — empty at runtime today |
| `stdout`/`stderr` | `print`, logging | yes | yes | yes |
| time, randomness | `std/time`, `std/random` | yes | yes | yes — browser clock and CSPRNG back them |
| `extern` | `extern` imports (RFC-0012) | **no** — traps today | host-dependent | yes |

A module's requirement is the union of the capabilities its calls carry —
presence in the source, not reachability of the branch, because the check must
not depend on control flow. An artifact's requirement is the union over its
import closure. The check is one subset test per artifact:

```
requirement(closure(entry)) ⊆ capabilities(target)
```

`extern` is the inverse direction — a capability native lacks — and today it
is a runtime trap. Whether the floor turns that trap into a compile error for
native artifacts is an M0 census question, answered by counting how many
existing programs rely on compiling (not running) extern calls natively.

### §3 The diagnostic

The error shows the chain, because the chain is the whole usability story —
the author never saw hop three:

```
error: artifact `app` (browser) cannot include `server/db.vyrn`: it reads files
  client/boot.vyrn → shared/format.vyrn → server/db.vyrn
   = `readFile` needs `fs`; target `browser` has no filesystem
   = call it through the wire instead: connect("./server/db.vyrn")
```

The remedy names the module that was actually reached, not a fixed path.
RFC-0072's `remedy()` says `client("./server/api")` for every rejection; that
string is replaced by the concrete crossing for the concrete module, in both
the floor's diagnostic and the fence's.

### §4 What this does not do

Stated in the RFC because the absence of these claims is a design decision:

- It does not classify data. A secret written as a literal in a
  browser-artifact module compiles and ships. No compiler can prevent this,
  and this one does not pretend to.
- It does not replace the fence. A server module that holds a secret in a
  plain constant uses no capability; only the audience fence catches its
  import, and only if the manifest declares it. That is Gradle's guarantee,
  and it is the most any declared boundary gives.
- It does not touch parity. The floor is a frontend check that runs before
  any backend; interp, native, and wasm see the same accepted programs.

## Milestones

**M0 — census.** Every builtin and std module that reaches outside the
program, one row each: the capability it carries, its behavior per target
today (verified by running one program per capability in-page and under
wasmtime, not by reading comments). The extern question answered with counts.
Gate: the table in this file has no "unknown" cell.

**M1 — artifacts in the manifest.** The `artifacts` map parsed; `main` /
`server` / `client` mapped onto it as sugar. Gate: every existing example and
test builds unchanged; `examples/shelf` and `examples/bin` gain explicit
artifact maps and behave identically.

**M2 — the floor check.** Requirement inference per module, closure per
artifact, the subset test, the chain diagnostic. Gate: a new example that
deliberately leaks (`client → shared → server file-reader`) is rejected with
the full chain; all existing examples stay green; parity suite untouched.

**M3 — the remedy and the reframe.** `remedy()` replaced by the concrete
crossing; RFC-0072's document amended to the fence claim (accidental class,
not secrets); `vyrn why` learns the capability axis: `vyrn why --capability fs
<artifact>` prints every chain that pulls `fs` in. Gate: no diagnostic in the
tree names a path the project does not contain.

**M4 — dogfood.** The fullstack example (`shelf`) declares both artifacts and
compiles with the floor on; one commit in its history introduces a leak and
shows the rejection. Gate: the leak commit's error message pasted into this
file, unedited.
