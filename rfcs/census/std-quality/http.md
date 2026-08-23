# std/http.vyrn

Lines: 1822. Exports: 12 functions (11 `export fn`, 1 `export gen fn http`). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

The module also exports 7 types (`Handler`, `Surface`, `IsMissing`, `Route`, `Feed`, `Live`, `Socket`) and 3 protocols (`Policy`, `Wire`, `Frames`).

## What this module is for

A caller turns procedure modules into REST routes. `http("./pastes")` generates, at compile time, one placeholder-checked path type, one adapter, and one constructor per procedure. The projection file wraps those constructors in `Route` values, tunes them with the `Policy` combinators (`cacheFor`, `etag`, `lastModified`, `vary`, `status`, `createdAt`, `notFoundWhen`), mounts SSE feeds with `sse` and WebSockets with `ws`, and answers requests through `mount`. `event` encodes one SSE frame. All of it is ordinary Vyrn over reflection; the only host effects are the two `serveStream` calls (`std/http.vyrn:724`, `std/http.vyrn:803`).

## Findings

All timings below come from one run of
`compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/http/b.vyrn`
(native backend, cwd `N:\lang`; each printed figure is the minimum over samples for one execution of a bench body containing an inner loop, divided here by that loop count). Scratch file kept outside the repository.

### 2. Algorithm complexity — MEDIUM

What: `mount` re-runs the whole mount-shadow audit on every request, and the audit compares every route against every earlier route, so each HTTP request pays O(live + sockets + sum over group pairs of |g_i| x |g_j|) `httpSubsumes` calls, each of which splits both patterns into fresh segment arrays.

Where: `std/http.vyrn:853` calls `httpCheckMount` first thing; the cross-group diff loop is `std/http.vyrn:1380-1393` (`for earlier in groups { for a in earlier { for b in g { httpSubsumes(a, b) ... } } }`), the within-list pair loops are `std/http.vyrn:1337-1372`. The module itself states the policy at `std/http.vyrn:840-842`: "The check runs on every call rather than once".

Evidence: same request, 500 iterations per bench body. One group whose matching route sits last behind 63 fillers: 16.66 ms / 500 = 33 us per request. Two groups of 32 with a request that matches nothing: 216.09 ms / 500 = 432 us per request. The 32x32 = 1024 `httpSubsumes` comparisons cost about 400 us more per request than scanning the same 64 routes in one group. Scan-only growth stays near-linear in route count: 8 routes 2.03 ms / 500 = 4 us, 256 routes 11.76 ms / 100 = 118 us per request.

Cost if unfixed: every request to a multi-group app pays the audit before any route runs; `examples/bin/server.vyrn:81` mounts four groups plus one feed list and one socket list on every request today.

Smallest fix: memoize the audit result on the first `mount` call of the process, or move the check behind an explicit `mountChecked`, so steady-state requests only scan. `RECOMMENDATION, NOT A DECISION`.

### 8. Allocation frequency — MEDIUM

What: every successful buffered response body is decoded from JSON at least once even when no combinator needs it, and up to three times when `lastModified` or `createdAt` is set; additionally, every route match attempt allocates two segment arrays because patterns are re-split per request.

Where: `std/http.vyrn:1061-1068` (`httpMapMissing` runs on every non-prefix answer) reaches `std/http.vyrn:1098-1122` (`httpErrOf` -> `httpSingle` -> `parseJson` at line 1118) before the default predicate `httpNeverMissing` (`std/http.vyrn:154-156`) is ever consulted. When `modified` or `location` is set, `std/http.vyrn:948-949` reach `httpPayload` (`std/http.vyrn:1136-1141`), which parses the same body twice more (`httpSingle(body, "Ok")` and `httpBodyFields`). Pattern re-splitting per request is `std/http.vyrn:1231-1232`.

Evidence: `mount` returning a 200 with an empty body costs 472.05 us / 500 = 0.94 us per request. The same route returning a ~4 KiB JSON body costs 2.84 ms / 500 = 5.7 us per request; building that body alone costs 1.05 ms / 500 = 2.1 us, so the mandatory `parseJson` of a body no default route uses accounts for roughly 2.7 us per request at 4 KiB. The triple-parse count for `lastModified` routes follows from the call chain above and was not timed separately: NOT MEASURED.

Cost if unfixed: `examples/bin/server/api/pastes.http.vyrn:24` sets both `etag()` and `lastModified("created")` on `GET(byId("/{id}"))`, so that route's handler output is fully JSON-decoded three times per request before the response ships.

Smallest fix: record on the `Route` whether any combinator needs body fields (a flag `notFoundWhen`/`lastModified`/`createdAt` already set), and skip `httpErrOf` when none did; cache the parsed fields per response instead of re-parsing. `RECOMMENDATION, NOT A DECISION`.

### 7. Peak memory use — LOW

What: `event` holds about four simultaneous copies of the payload — the input string, the CR-normalized copy, the array of line strings, and the joined output — because it normalizes line endings unconditionally and rebuilds the frame by concatenation.

Where: `std/http.vyrn:554-555`: `replace(replace(data, "\r\n", "\n"), "\r", "\n")` runs two full-string passes and allocates even when the payload contains no carriage return, then `split` materializes every line and `joinWith` builds the final buffer again.

Evidence: `event("1", "msg", d)` costs 729.70 us / 500 = 1.5 us per frame at 64 bytes and 40.02 ms / 500 = 80 us per frame at ~4 KiB of multiline data (build baseline for the same 4 KiB input: 165.92 us / 2000 = 0.08 us, so the cost is in `event`'s own copying). The copy count is read from the code; the actual peak-bytes multiple is NOT MEASURED.

Cost if unfixed: every SSE frame in a live tail pays it; `examples/bin/server/api/pastes.http.vyrn:49` calls `event` once per pasted row in `tailStep`, so a large paste inflates that frame's transient memory several-fold.

Smallest fix: return `data` unchanged when `indexOf(data, "\r")` finds nothing, and build the frame into one pre-sized byte array. `RECOMMENDATION, NOT A DECISION`.

## No finding

No finding: 1, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
