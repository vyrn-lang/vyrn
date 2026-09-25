#### coredrive runs as shards under the local timeout (2026-09-25, `m7-shard`)
RFC-0125, milestone M7.
Decision: the lead's. `VYRN_SHARD=k/n` makes `coredrive` walk the roots and the shapes whose index in path order is `k` modulo `n`. Unset, it walks the whole corpus, which is what CI runs.
Went: the whole-corpus run as the local gate. Under load it passed the tool's 600 s cap, and the command dropped into the background with no one to wake the track (the m7-small record). Stayed: the unsharded run, in CI and in the gate list.
Lines: `coredrive.rs` 560 to 601. `AGENTS.md` +1 line in the gate list. Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence:
- unsharded, on ab947e5f: the test's stderr was byte-identical to main's, 306 lines. Taken 21,082 of 21,172.
- `VYRN_SHARD=0/2`: 82 programs, taken 10,030 of 10,079, 51 shapes, 72 s. `VYRN_SHARD=1/2`: 86 programs, taken 11,052 of 11,093, 50 shapes, 168 s. The sums (168 programs, 21,082 of 21,172, 101 shapes) equal the unsharded run, which took 220 s on main and 298 s with this change. Other tracks' runs shared the machine throughout.
- `VYRN_SHARD=2/2` panics at once with the sentence that names the form.
Time: about 35 minutes: work 10, runs 25.
Findings:
- A shard skips the check of which forms the rows carry, because that list is a whole-corpus fact. The `break`/`continue` pin and the projection-call pin are filtered to the shard's programs.
- "distinct bodies the rows carry" does not add across shards (1,004 + 1,160 against 1,664), because a body name can occur in both shards.
- The split is by index, not by cost: shard 1 took 2.3 times as long as shard 0.
