# RFC-0043 — Time and Randomness at the Host Boundary

- **Status:** Draft (design locked)
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
