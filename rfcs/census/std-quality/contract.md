# std/contract.vyrn

Lines: 587. Exports: 3 (`checkContract` :446, `suppliesMember` :527, `matchedMember` :544; no other export kinds). Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A generator declares a `contract` naming the exports a module may carry, with types, optionality, default bodies, and alternative signatures (RFC-0071). `checkContract` compares that declaration against a reflected `ModuleInterface` and returns RFC-0009 `Issue`s: missing required members, type mismatches, unknown exports with a did-you-mean, and open-rule shape violations. `suppliesMember` and `matchedMember` answer single-name questions ("does this module supply member `data`, and at which declared signature"). Every generator in the standard library routes its surface checks through here: `std/ui` (:1071, :1113), `std/vyx` (:2976), `std/rpc` (:186), and through them `std/http`, `std/graphql`, `std/connect`. All of it runs at generation time; none of it runs in shipped binaries.

## Findings

### 2. Algorithm complexity — LOW

What: `checkContract` is quadratic in the number of contract members: `memberNames` de-duplicates with a linear `includes` scan over a growing array, and the main loop re-scans every member for alternatives and re-scans every member/export pair for unknown exports.
Where: `std/contract.vyrn:324` (linear `includes` inside the build loop), `std/contract.vyrn:456-457` (`memberNames` then `alternatives` per name, each a full pass over `c.members`), `std/contract.vyrn:488-489` (`hasMember` and `openRule`, a full pass each per unknown export). With M members and E exports the loop at :456 proves O(M² + E·M); `didYouMean` (:281) adds one `editDistance` per `MemberInfo` per unknown export, and `editDistance` (`std/strings.vyrn:389-432`) is itself O(L²) time and space per call.
Evidence: command `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/contract/b.vyrn` from N:\lang: `typo 16 members` min 18.68 µs, `typo 128 members` min 230.90 µs, `typo 512 members` min 2.05 ms — 8× more members costs 12× and then 8.9×, tracking the quadratic term.
Cost if unfixed: `std/ui.vyrn:1071` runs this check once per page at site-generation time, and `std/vyx.vyrn:2976` plus `std/rpc.vyrn:186` run it per component and per API module; at real contract sizes (the `Page` contract holds 16 alternatives of 4 names, `std/ui.vyrn:332-368`) the measured cost is about 19 µs per page, so the waste is microseconds, not seconds.
Smallest fix: build one pass that buckets members by name into a map-like structure instead of three full rescans per name. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: two paths allocate far more than the question they answer needs. First, `matchedMember` copies the entire export surface — `moduleExports` calls `.copy()` on every name, parameter spelling, and return spelling (:230-232) — then linear-scans it, to answer a query about one name. Second, `didYouMean` computes `editDistance` against every `MemberInfo` entry (:279-287) rather than against distinct member names, so a name declared at four shapes buys four heap-allocated `(L+1)×(L+1)` distance matrices (`std/strings.vyrn:391-394`) where one would do.
Where: `std/contract.vyrn:549` (`matchedMember` builds the full copy via `moduleExports`), `std/contract.vyrn:230-232` (per-string `.copy()`), `std/contract.vyrn:279-287` (per-entry, not per-name, `editDistance`). `suppliesMember` (:528) inherits the first cost on every call.
Evidence: same bench command as above: `matchedMember one query over 512 exports` min 142.56 µs, versus 14.17 µs for the whole `clean 16 members` contract check — one single-name query costs ten times a full small-contract check. For the redundancy half: `typo 32 names x1 alt` min 38.83 µs against `typo 32 names x4 alts` min 96.38 µs — the same 32 distinct names cost 2.5× more when declared the way `std/ui`'s `Page` contract actually declares its members, four shapes each (`std/ui.vyrn:332-335`).
Cost if unfixed: `std/ui.vyrn:1113` calls `matchedMember` once per inspected page during generation, so every page compile pays the full interface copy.
Smallest fix: compare the queried name against exports before copying spellings (copy lazily after `exportIndex` hits), and run `didYouMean` over the output of `memberNames` instead of `c.members`. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
