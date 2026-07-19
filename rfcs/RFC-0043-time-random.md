# RFC-0043 — Time and Randomness at the Host Boundary

- **Status:** Implemented (see "As landed" below)
- **Depends on:** RFC-0014 (input I/O — the host-boundary model this
  extends; canonical error/effect discipline), RFC-0012 (extern — the
  mechanism), RFC-0004 (effects/purity — time/random are effects), the
  parity harness conventions (`.stdin`/args/cwd — the fixture story this
  mirrors)
- **Evidence (bin dogfood, NOTES):** a real pastebin wants a real
  `created` timestamp; Vyrn has no clock and no randomness, so bin uses a
  persisted monotonic counter and content-addressed ids. The friction
  report named this and asked for a design that doesn't break parity.

---

## The principle: time and randomness are host inputs, not magic

Vyrn's core is deterministic and that is load-bearing — parity is
byte-identical output across interp/native/wasm. But parity has NEVER
meant "no outside world": `readFile`, `args()`, `readLine()` all pull
host-provided values, and the parity harness stays green because it
**controls those inputs** (`.stdin` fixtures, fixed args, `cwd`). Time
and randomness are the same shape: host-provided values the harness
fixes. So:

- **The deterministic core is unchanged.** No implicit clock, no ambient
  RNG. You must *import* the capability, and calling it is a host effect
  (exactly like I/O — forbidden in `gen`/comptime and const-eval by the
  existing purity analysis, no new rule).
- **Parity holds by fixing the input**, not by forbidding the feature —
  the harness injects a fixed clock and fixed seed, so a time/random
  example is a parity citizen whose three backends agree because they
  read the same injected values.

## 1. `std/time` — the clock is an extern

```vyrn
import { now, Instant } from "std/time"

fn stampedTitle(t: String) -> String {
    return "\{t} @ \{format(now())}"
}
```

- **`now() -> Instant`** — `Instant` is milliseconds since the Unix epoch
  as `Int64` (a distinct validated type so it can't be mixed with plain
  counts; `toMillis(i)`/`fromMillis(n)` bridge). It is an `extern`
  the host implements: native `clock_gettime(CLOCK_REALTIME)`, wasi
  `clock_time_get`, browser `Date.now()`.
- **`format(i: Instant) -> String`** and field accessors (`year`,
  `month`, `day`, `hour`, `minute`, `second` — UTC, computed in pure
  Vyrn from the epoch millis so the *formatting* is a parity citizen;
  only `now()` is the host value). ISO-8601 `formatIso(i)`. No timezone
  database in v1 (UTC only; a `+HH:MM` offset arg is the escape).
- **`monotonic() -> Int64`** — a separate host extern for elapsed-time
  measurement (nanoseconds, no epoch meaning), so durations don't misuse
  wall-clock. Deferred if not demanded; named here so `now()` isn't
  abused for timing.

## 2. `std/random` — a pure PRNG, seeded from the boundary

Randomness splits cleanly into a **pure, deterministic generator**
(always a parity citizen) and an **optional host seed** (the only
non-deterministic part):

```vyrn
import { seededRng, nextInt, nextInRange, Rng } from "std/random"

let mut rng: Rng = seededRng(42)          // fully deterministic — parity-safe
let d6 = nextInRange(rng, 1, 6)           // rng threaded (value type, no hidden state)
```

- **`Rng`** is a value-type PRNG state (SplitMix64 / PCG — pure Vyrn);
  `seededRng(seed: Int64) -> Rng`, and each draw returns
  `(value, Rng')` — threaded explicitly (a value, no ambient generator),
  so the whole thing is deterministic and needs no host at all. This is
  the part apps should use for anything reproducible (tests, shuffles
  with a known seed).
- **`randomSeed() -> Int64`** — the ONE host extern (native/wasi/browser
  CSPRNG), used only when you want an unpredictable starting seed:
  `seededRng(randomSeed())`. Isolated so 99% of randomness stays pure.
- No global/ambient `random()` — that would be an implicit host effect
  everywhere and a parity hazard; the explicit seed thread is the point.

## 3. The parity harness fixes the clock and seed (the crux)

The harness gains two controlled inputs beside `.stdin`/args:

- **`VYRN_FIXED_TIME`** (env; and a per-example `.time` fixture like
  `.stdin`): when set, the `now()` host shim returns exactly this epoch
  millis on every call, in ALL three backends (the native/wasi/browser
  shims each honor it; the interpreter reads the same). Default in the
  harness: a fixed constant (e.g. `1_700_000_000_000`). `monotonic()`
  returns a fixed base + a deterministic per-call increment.
- **`VYRN_FIXED_SEED`** (env): `randomSeed()` returns this fixed value
  under the harness. (The pure `Rng` needs nothing — it is already
  deterministic.)
- Consequence: an example calling `now()`/`randomSeed()` is a normal
  parity citizen — interp == native == wasm because they read the same
  injected constants. Documented alongside the existing harness
  conventions; a new `examples/clock.vyrn` proves it three-way
  (formats a fixed instant, draws from a seeded Rng, and — guarded by the
  fixed-seed env — from `randomSeed()`).

## 4. Servers & bin adoption

- **bin**: `Paste.created` becomes a real `Instant` (stamped `now()` at
  create), rendered `format(created)` — the pastebin shows a real
  timestamp instead of a counter. The persisted store already round-trips
  `Int64` via the codec, so `Instant` (an `Int64` newtype) persists
  unchanged; restart-survival keeps the real timestamps. The content-
  addressed id story is unchanged (ids stay hashes, not time-based).
- **serve**: `now()` works inside `handle`/procedures (a host effect,
  like logging). Whether request-arrival time should also be a field on
  `Request` is deferred — `now()` covers the need; a per-request stamp is
  a serve-runtime convenience to add only if demanded.

## Effects, comptime, workers

- `now()`/`monotonic()`/`randomSeed()` are host effects: rejected in
  `gen`/comptime and const-eval (the existing purity analysis already
  covers externs — verify and pin, no new machinery). The pure `Rng` is
  usable anywhere (it's ordinary code).
- Spawn/workers: a task calling `now()` is doing host I/O, which the
  isolation analysis already permits (like `print`/file I/O — it does not
  touch module state); pin it. `randomSeed()` likewise.

## Out of scope

Timezone database / DST, locale-aware date formatting (UTC + offset
only), a global ambient `random()`, cryptographic randomness guarantees
beyond "the host CSPRNG seeds it", `Duration` arithmetic types (millis
Int64 math for v1), leap seconds, parsing arbitrary date strings
(formatting only; a parser is a later `std/time` addition).

---

## As landed

**`Instant` newtype.** `std/time` exports `type Instant = Int64 where value >= 0`
(epoch millis, UTC), with `toMillis`/`fromMillis` bridges. Since the extern ABI
domain is scalar-only, `now()` is a thin wrapper: the module-private extern
`hostNowMillis() -> Int64` returns raw millis and `now()` wraps them into
`Instant`. The UTC breakdown (`civil`/`year`/`month`/`day`/`hour`/`minute`/
`second`, via Howard Hinnant's `civil_from_days`) and `format`/`formatIso` are
PURE Vyrn with all constants inlined (no module-level `let`, which would count as
module state and forbid them in `gen`/comptime) — so the formatting is a full
parity citizen, usable anywhere including generators. `monotonic()` wraps
`hostMonotonicNanos()` (nanoseconds).

**`Rng` / draws: record, not tuple.** Vyrn has no tuples, so a draw returns a
record: `type Draw = { value: Int64, rng: Rng }`, threaded explicitly
(`let d = nextInt(rng); use(d.value); rng = d.rng`). `Rng = { state: Int64 }`.

**PRNG algorithm deviation (real wall).** SplitMix64/PCG (named in §2) require
bitwise XOR/shift, and **Vyrn has no bitwise operators** (`^ & | << >>` don't
exist — only `+ - * / %`). So the generator is a multiplicative-congruential
**Lehmer / MINSTD** PRNG: `state' = state * 48271 mod (2^31 - 1)`. It delivers
the same contract (pure value type, deterministic, seed-threaded) with pure
arithmetic; products stay `< 2^47`, far under `Int64`, so there is no overflow
and no interp/native divergence. `seededRng` folds any `Int64` (incl. negatives)
into the valid state range without `abs` overflow. Not cryptographic —
documented in-module.

**Extern lowering across the 3 backends.** `hostNowMillis`/`hostMonotonicNanos`/
`hostRandomSeed` are declared as `extern fn` in source (so the EXISTING purity
analysis treats them as host effects — no new machinery), but they are NOT
ordinary RFC-0012 `vyrn`-namespace imports. A new `host_boundary_extern(name)`
map in codegen routes them to plain C-shim symbols (`__vyrn_now_millis` etc.)
instead of a `vyrn` wasm import, and they get no native trap stub. The shim
implements them on EVERY target: `timespec_get(TIME_UTC)` for the wall clock
(portable across UCRT / glibc / wasi-libc), `clock_gettime(CLOCK_MONOTONIC)` /
`timespec_get` fallback for monotonic, and a CSPRNG seed (`rand_s` on Windows,
`getentropy` on POSIX/wasi). Because these lower to WASI `clock_time_get` /
`random_get` via wasi-libc — NOT the `vyrn` host page — a clock/random program is
a full three-way parity citizen under wasmtime (which supplies WASI clocks/
random natively). The interpreter special-cases the same three extern names.

**Harness injection mechanics.** The C shim, the interpreter, and (for the
browser) `web/wasi-min.js` all honor `VYRN_FIXED_TIME` / `VYRN_FIXED_SEED`:
`now()` returns exactly the injected millis, `randomSeed()` the injected seed,
and `monotonic()` a fixed base + deterministic per-call increment
(`1e9 + n*1e6`, mirrored bit-for-bit in shim and interp). The parity harness sets
both env vars on every backend process (`FIXED_TIME = 1700000000000`,
`FIXED_SEED = 424242`) and additionally forwards them into the wasm guest via
`wasmtime --env` (wasmtime does not inherit host env). Result: `examples/clock.vyrn`
is byte-identical interp == native == wasm (verified: `ok clock.vyrn`, 69 checked
0 failed with the wasm column active). `web/wasi-min.js` gained
`clock_time_get`/`random_get` backed by `Date.now()`/`performance.now()`/
`crypto.getRandomValues`, with optional `hooks.fixedTime`/`hooks.fixedSeed` for
reproducible browser demos.

**Effects/comptime/workers pins.** The three externs are rejected in `gen`/
comptime by the existing extern rule (pinned:
`rfc0043_host_clock_extern_is_rejected_in_a_generator`), and the pure PRNG
arithmetic is accepted in a generator (`rfc0043_pure_prng_is_comptime_usable`).
**Spawn note / RFC-prose correction:** §"Effects, comptime, workers" claims a
task calling `now()` is "permitted (like print/file I/O)". In the actual
isolation analysis, host I/O — `print`, file I/O, and any `extern` — is NOT
isolated, so spawning a function that calls it is REJECTED (see
`parallel.vyrn`'s "no I/O" contract). `now()`/`randomSeed()` are treated
identically and consistently — forbidden in a task exactly as `print` is
(pinned: `rfc0043_spawned_task_calling_the_clock_is_rejected`). The prose's
"permitted" was inaccurate; the landed behavior is the consistent one.

**bin adoption.** `Paste.created` is now a real wall-clock stamp
(`toMillis(now())` at create), rendered `format(fromMillis(created))` →
"created 2023-11-14 22:13:20 UTC". Kept the field typed `Created`
(`Int64 where value >= 0`, re-documented as epoch millis) rather than importing
`Instant` into `wire.vyrn`, so the RPC/OpenAPI type-closure reflection and the
JSON codec are untouched — the timestamp round-trips as a plain `Int64`.
`findByCreated` (which assumed `created` was unique) was replaced with a
last-appended-element lookup, since two pastes can now share a millisecond.
Content-addressed ids unchanged. Verified end-to-end via `vyrn serve`: create →
`created:1700000000000` persisted → restart (fresh process) → timestamp and id
survive, home + paste views render the formatted UTC string.
