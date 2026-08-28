# std/args.vyrn

Lines: 226. Exports: 6 (`cli`, `cliOf`, `flag`, `opt`, `positionals`, `rest`; plus one non-fn export, `type Args` at line 22). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A CLI reads its command line through this module. `cli()` snapshots `args()`, then `flag` answers presence of a spelled-out name, `opt` reads `--name value` and `--name=value`, `positionals` returns the free tokens, and `rest` returns what follows a terminator. There is no spec, no help generation, and no unknown-flag rejection; RFC-0061 fixes that scope. Callers today: `examples/vlog.vyrn:45` imports `cli`, `cliOf`, `flag`, `opt`, `positionals`, `Args`; `examples/argsdemo.vyrn:9` and `site/guide/cliargs.vyrn:3` also import it.

## Findings

### 2. Algorithm complexity — LOW

What: every probe rescans the whole argv from index 0, so k probes over n tokens cost O(k·n) instead of one O(n) pass.
Where: `std/args.vyrn:37-46` — each of `flag` (line 51), `opt` (line 73), and `positionals` (line 106) starts with a private `terminatorAt` scan, then walks argv again.
Evidence: doubling the token count roughly doubles the time per bench body (8 `opt` probes): `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/args/b.vyrn` from N:\lang printed min 6.21 µs (64 tokens), min 11.87 µs (128 tokens), min 23.32 µs (256 tokens). Each probe is a full scan to the option at the tail, which proves the linear per-probe loop at `std/args.vyrn:74-95`.
Cost if unfixed: `examples/vlog.vyrn:431-542` calls `flag` once and `opt` three or more times per run; each call repeats two full scans. At realistic argv sizes the cost is microseconds once per process, so the impact stays small.
Smallest fix: compute `terminatorAt` once in `cliOf` and cache it on `Args`. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — MEDIUM

What: `opt` runs `indexOf(tok, "=")` on every token and allocates a prefix substring for every token that contains `=`, even when the token is not the wanted option.
Where: `std/args.vyrn:77-80` — the equals test precedes the name check, so any positional like `key=value` takes the branch and pays an `indexOf` scan plus a `substring(tok, 0, eq)` allocation per token per probe.
Evidence: same command as above; at 256 tokens the eq-laden argv bench printed min 131.03 µs against min 23.32 µs for plain tokens — 5.6× slower for the identical probe count and token count. The only difference is that every filler holds `=` and enters the branch at `std/args.vyrn:78`.
Cost if unfixed: any caller whose positionals contain `=` (config-style arguments such as `key=value`) pays the factor; `examples/vlog.vyrn:45` is such a caller shape because it parses `--contains=boom` style tokens next to free words.
Smallest fix: test `startsWith(name) && tok.byteLength > name.byteLength && tok[name.byteLength] == "="` before cutting, or compare bytes without building the prefix substring. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
