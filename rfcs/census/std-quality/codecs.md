# std/codecs.vyrn

Lines: 359. Exports: 7 (`hexEncode`, `hexDecode`, `base64Encode`, `base64EncodeBytes`, `base64Decode`, `urlEncode`, `urlDecode` — all plain functions; no other export kind appears). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

Hex, base64, and percent encoding for `String` values, plus `base64EncodeBytes` for bytes that are not text. Since RFC-0078 M4c these six codecs are the builtins: every engine routes `hexEncode` and friends into this Vyrn source (`std/codecs.vyrn:18-25`). Callers today are `std/http.vyrn:58` (WebSocket handshake base64 at `std/http.vyrn:707`, URL escaping), `site/app/guide.vyrn:48`, `site/app/pagemd.vyrn:28`, and `examples/codecbytes.vyrn:29`, `examples/encoding.vyrn:15`.

Bench command used below, run from `N:\lang`:

```
compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/codecs/b.vyrn
```

## Findings

### 24. Branch predictability — LOW

What: `hexEncode` runs about 1.5x slower than `base64Encode` on the same 16,384-byte ASCII input because it takes two data-dependent branches per byte (`hexDigit` at `std/codecs.vyrn:38-43`, called from `std/codecs.vyrn:98-99`) where base64 uses an alphabet-table lookup (`std/codecs.vyrn:170-173`).
Where: `std/codecs.vyrn:38-43`.
Evidence: bench `hex encode 16k` min 82.76 µs against `b64 encode 16k` min 54.41 µs, both over the same 64-byte seed doubled 8 times; decode costs are near-identical (roundtrip mins 128.75 µs minus 82.76 µs gives about 46 µs for hex decode, 100.06 µs minus 54.41 µs gives about 46 µs for base64 decode), so the gap sits entirely in the encoder's digit branches.
Cost if unfixed: `examples/codecbytes.vyrn:29` and `examples/encoding.vyrn:15` pay it on every hex call; no hot in-repo loop calls hex today.
Smallest fix: replace the two-branch `hexDigit` with a 16-entry lookup table like `b64Alphabet` (`std/codecs.vyrn:130-132`). RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — LOW

What: `urlEncode` cost depends on input content, not just length; a fully reserved byte expands to three output bytes and two extra branches (`std/codecs.vyrn:260-264`) while an unreserved byte passes through one test chain (`std/codecs.vyrn:239-250`).
Where: `std/codecs.vyrn:253-267`.
Evidence: bench `url encode 16k unreserved` min 48.30 µs against `url encode 16k worst case` min 97.87 µs on two 16,384-byte inputs of equal length (`%&+<>?` seed versus `abcdefgh` seed).
Cost if unfixed: `site/app/pagemd.vyrn:28` encodes attacker-controlled page text whose reserved density it does not choose; `std/http.vyrn:58` escapes request components.
Smallest fix: none needed; document the 3x worst-case expansion next to the function. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: every exported call performs at least three allocations by construction: the `bytes(s)` input conversion, the growing `out: Array<UInt8>` pushed without a capacity hint, and the final string rebuild through `ascii` or `decoded` (for example `std/codecs.vyrn:94-95` and `std/codecs.vyrn:102`; the same triple repeats at `109`/`113`/`124`, `165`/`166`/`192`, `199`/`203`/`233`, `254`/`255`/`267`, `274`/`275`/`294`). `b64Alphabet()` also rebuilds its 64-byte table on every `base64EncodeBytes` call (`std/codecs.vyrn:165` calling `std/codecs.vyrn:130-132`).
Where: `std/codecs.vyrn:94-95`.
Evidence: bench `url encode 64b single call` min 547 ns, `b64 encode 64b single call` min 639 ns, `hex encode 64b single call` min 744 ns; the base64 alphabet rebuild sits inside that 639 ns and does not show above timer noise, since base64 still beats hex at 16 KB. Allocation counts themselves NOT MEASURED (no allocator instrumentation available through `vyrn bench`).
Cost if unfixed: `std/http.vyrn:707` allocates the full set once per WebSocket handshake; negligible there. No in-repo caller loops over these functions.
Smallest fix: add a capacity-reserving array constructor so encoders allocate `out` once. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23, 25, 26, 27, 28, 29, 30.
