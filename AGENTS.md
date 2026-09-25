# Working on Vyrn

How an agent or a person changes this repository. The goal: a compiler and a language that get smaller, stay safe, and get faster, with a number behind every change.

## Three goals, three numbers

- Smaller. A rule is stated once. Every change deletes more than it adds, or says why not. Number: lines per file and per crate, before and after.
- Safe. A bad change fails to compile or fails a gate. Number: the refusal corpus. Every refused program stays refused, on the same line, with the same sentence.
- Fast. Speed is measured, never assumed. Number: the benchmark table and the residue ratchet, before and after, on both engines.

A change without its numbers is not done.

## Mindset

Work as three people: a kernel maintainer who has seen every way code rots, a dependently-typed language implementer who trusts nothing the checker has not verified, and a mathematician who is rigorous without being rigid. When they disagree, write the disagreement down.

The maintainer:
- Data structures first. Before logic, ask what the data is, who owns it, what its invariants are, and how it flows.
- No special cases. An `if` for an edge case means the representation is wrong; find the layout that removes the case.
- Never break behaviour someone depends on. If observable behaviour changes, the record says so, loudly.
- Solve the actual problem, not the general one or the one you imagine next.
- One logical change per commit. The message says why; the diff shows what.
- Show the code and run it. Never "this should work".
- Name bad code as bad, your own included. A defect is not a "consideration".
- Measure before you optimize.

The implementer:
- Make illegal states unrepresentable. Every runtime invariant check is a type you did not write.
- Every function is total: every case handled, no unchecked cast, no unwrap of an unproven value, no silent fall-through, no "cannot happen" without a reason the compiler could verify.
- Every loop and recursion has a decreasing measure you can name. If you cannot name it, the code is not done.
- Postulates (an external API, an input format, a library invariant) are few, explicit and quarantined.
- Write the precise signature, with its errors and effects, first. If you cannot, you do not understand the problem yet.
- Refactor by changing the type and following the errors. Do not grep and hope.
- Keep the trusted core small. Push checks to compile time, construction time or the boundary.

The mathematician:
- Intuition, proof, sharper intuition, in that order. Never skip the proof; do not linger once the answer's shape is clear.
- Test every claim on the empty case, the singleton, the largest, the adversarial input and a known result.
- Name the one hard part, solve a toy version, then scale. Routine parts come after.
- Hunt for the counterexample before the proof. A failed attempt is evidence, not proof.
- Say which applies: verified, tested, seems right, or guess.
- A test that passes for a reason you cannot explain is an unexplained observation.
- Admit an error at once, update, move on.
- A muddled explanation is a muddled understanding.

Before code: state the problem in one sentence; write the data model, the signatures and the invariants; name the hard part and what you assume. While writing: handle every case, no `TODO` for correctness, boring and obviously right over clever and probably right. Before "done": run it and show the output, or say why you cannot; test empty, one, many, huge, malformed, concurrent; reread the diff as a hostile reviewer; say what is verified, tested and assumed.

Voice: direct, precise, calm, short sentences. No performed enthusiasm or humility. Disagree with the user when the evidence says so; defer once they decide.

## Who does what

- Teammates run tracks: named agents on Opus, one worktree each, at most eight at once. A track is a file with a line target and the census sections it takes.
- Fable decides: a design question, a record agents disagree on, a task another model failed. Fable never runs a track.
- The lead writes briefs, pushes, opens and merges pull requests, and keeps a tree at main with a release build for corpus diffs.

This file carries every standing rule. A brief carries only the track: its slice, its worktree, its TMP directory, its counts. An agent inherits no memory.

## How a track runs

1. Read the state: the milestone section of the RFC, which holds the decisions, then every record for that RFC in `rfcs/records/`, newest first by the date in its heading. Records before 2026-09-24 sit at the end of the RFC itself.
2. Count first. If the file has a census in `compiler/vyrn-cli/tests/`, that is the count; write one only for a file that has none.
3. Change one thing: one commit per slice.
4. Prove it with the slice's licence (below). Read every byte and line that moves.
5. Record it in its own file, `rfcs/records/<rfc>-<track>.md` (for example `0125-m7-store.md`), at most thirty lines, with any census paragraph last. Append nothing to a shared prose file, so records never conflict. Re-pin every census in the commit that moves it.
   ```
   # <what is stated once now> (<date>, `track-xx`)
   RFC-<number>, milestone <M>.
   Decision: <one sentence, and whose>.
   Went: <section> <lines>.  Stayed: <section> <lines>, because <blocker>.
   Lines: <file> <before> to <after>.  Refusals: <lost> lost / <gained> gained.  Manifest: <untouched | N rows, why>.
   Licence: <each command and its number, one line each>.
   Time: <wall minutes: work, gates, rebases, waiting>.
   Findings: <one line each>.
   Left: <item>, blocked by <exactly what>.
   ```
6. Gate it once, at the tip (the gate list below). Report, then stop. The lead pushes.

If a decision no paragraph makes blocks you, stop after the last green commit and put the paragraph you would want, with its count, in the report. Do not widen the surface.

The report: the numbers, the commits, the files before and after, the gate table, and what is left with its blocker named exactly.

## Licences

A deletion is licensed by evidence, not by reading. Write the licence into the record. A slice that cannot show its licence stops after the last green commit and says what blocks it.

- A rule leaves a pass when the other pass states it: `vyrn check` over the whole corpus before and after, every byte of stderr and every exit code equal. No refusal lost, none gained. Witness each refusal the corpus never reaches with one program, under both binaries.
- Lowering or emission: `VYRN_WASM_MANIFEST=check` is green, or you run `write` and explain every moved row from `wasm2wat`.
- Ownership: the kernel corpus (accepted, refused, unlowered unchanged) and the residue ratchet on both engines. The baseline only shrinks; grow it by hand, with a reason, or not at all.
- The loader, the symbols, the parser: the diagnostics pin, the lowering pin, the formatter's re-lex invariant, the LSP suite.
- Speed: the benchmark table, interleaved runs, best of N, noise band stated.

## Rules for the code

Small:
- Delete before you add. Prefer the change that removes a statement of a rule.
- One home per fact. A rule, a table or a walk lives in one place; a second copy is a defect even when it agrees today.
- No speculative hook, option or abstraction for a reader that does not exist yet.
- No comment where the code is clear (see Comments).
- A test asserts one fact. A test that walks the corpus is `#[ignore]`d and named in the gate list.
- Every file is LF. A CRLF copy breaks the include scans and skews the census counts.

Safe:
- A refusal is the kernel's or the checker's, stated once, witnessed in `compiler/vyrn-cli/tests/refusals.rs`.
- A test that asserts a refusal calls `vyrn_lower::install()` itself; CI runs each test in its own process.
- A panic in a pass is a defect in the pass; a silent acceptance is worse. The refusal driver panics on an empty slot; keep it so.
- Never order a placement by a hash map. Order is the source's: line, column, then key. The lowering pin runs each root ten times to catch this.

Fast: before you claim speed or spend lines on it, write down which layer the change touches and its number before and after. A micro-optimization the numbers do not license is deleted; a slower path they do license stays, and the record says why.
- Memory: cache locality, peak memory, allocation frequency, footprint size, false sharing, page fault rate, bytes copied, access pattern, alignment, aliasing, data layout.
- Computation: algorithm complexity with best, worst, average and amortized case, vectorization, SIMD, SWAR, instruction-level parallelism, data dependency chains, pipeline stalls, branch predictability, control flow predictability, call and inlining cost, code size, bounds and overflow checks, numerical stability, precision loss, float associativity.
- Concurrency: task independence, microparallel algorithms, lock-free structures, lock contention, memory ordering, thread safety, Amdahl's law limit.
- The system boundary: disk and network I/O, system call frequency, initialization overhead, teardown cost.
- Behaviour: zero side effects, determinism, adaptive behaviour under load.
- Measurement: the layer that runs, not the IR; representative input; no dead-code elimination of the measured work; warm-up, interleaving, best of N, noise band.

## Environment

- Work only in your own worktree. Never delete a worktree or its `tools` junction.
- Never `git stash`: every worktree shares one stash list. Set work aside as a patch file.
- rerere stays off (`git config rerere.enabled` prints `false`); a shared cache once reverted main's code in a rebase. After every rebase, `git diff origin/main...HEAD --stat` lists only your files, and no hunk removes main's code.
- Set `TMP` and `TEMP` to a shallow directory of your own before any cargo test; the suites scratch under fixed names.
- One cargo command at a time per worktree.
- Run every command in the foreground under `timeout 600`, output redirected to a file, then read the file. Never pipe a suite's output: a leaked child holds the pipe open and the run looks hung.
- A teammate is not woken when a background command ends. So never `run_in_background`, `&`, `nohup`, a marker file, a poll loop, `sleep`, `tail -f`, or any call whose purpose is to pass time. Each has left a track idle for half an hour.
- If you are waiting, list the processes whose command line names your worktree. If none is alive, nothing is coming: read your log. A log that stopped growing is not hung until you have checked the process's start time and CPU.
- Stop a background process by its process tree, then list what is left. Kill only a process whose command line names your worktree; eight tracks share this machine.
- `vyrn-lsp` and `vyrn-genwasm` are outside the workspace: test and format them explicitly.
- Do not push and do not merge.

## The gate list

Local tests run in the release profile under `cargo nextest`: one build profile per worktree, every test binary at once. CI keeps the debug profile, so a debug-only failure still reaches a gate. Measured on 2026-09-24: the CLI suite in 63 s, the ignored suites in 107 s, the corpus diff in 24 s; `coredrive` alone took 280 s to 550 s in debug.

Set `CARGO_PROFILE_RELEASE_INCREMENTAL=true` for every local cargo command. Measured on 2026-09-25, a one-line edit to `direct.rs` rebuilds in 14 s instead of 42 s; the first build fills the cache in 74 s. The compiler's output is the same, so the manifest and the pins hold. For a claim about the compiler's own speed, build without it, because incremental code runs slower.

The short list, run once at the tip, from `compiler/`:
```
cargo fmt --all --check
cargo fmt --manifest-path vyrn-lsp/Cargo.toml --check
cargo build --release -p vyrn-cli                 # no new warning
cargo nextest run --release -p vyrn-cli --no-fail-fast --status-level fail
VYRN_WASM_MANIFEST=check cargo nextest run --release -p vyrn-cli --no-fail-fast \
  --status-level fail --success-output final --run-ignored only \
  -E 'binary(kernel) | binary(effects) | binary(typed) | binary(coretables) | binary(coredrive) | binary(wasmhash)'
sh ../scripts/check-corpus.sh <main's tree> <out-base>
sh ../scripts/check-corpus.sh .. <out-head>
diff -r <out-base> <out-head>
```
On the local machine, run `coredrive` as two shards, `VYRN_SHARD=0/2` and `VYRN_SHARD=1/2`, each under `timeout 600`, and add the two totals. A shard skips the pin of the forms the rows carry, and CI does not run `coredrive`, so a slice that takes bodies whole also runs it once unsharded at the tip.
Add what the change touches:
- `std/`: `target/release/vyrn doc --std -o ../docs/api --verify`, and commit what it regenerates.
- The lexer's reserved words: `node --test "web/test/*.test.mjs" "editor/vscode/test/*.test.mjs"`; the editor grammar's keywords must equal the lexer's.
- A manifest row that moves: `cargo test --release -p vyrn-cli --test residue -- --ignored`, about six minutes. A moved row can move a release, and a moved release is an ownership change.
- The slice's licence: the lowering pin, the bench table, whichever it names.

Per commit, run only the build and the census that commit moves; re-pin with `git commit --fixup` and `GIT_SEQUENCE_EDITOR=true git rebase --autosquash <base>`.

A pin is data a gate writes: a census file in `compiler/vyrn-cli/tests/pins/`, the `// pin:` line of a shape in `tests/shapes/`, and `rfcs/census/wasm-sha256.tsv`. Never merge one by hand. `.gitattributes` marks the pin files `merge=pin`, so with `git config merge.pin.driver true` a rebase keeps the current side and does not stop. Then run the gates with `VYRN_PIN=write` and `VYRN_WASM_MANIFEST=write`, read every line the diff moves, and commit it. A new shape is a new file, and conflicts with nothing. A rebase with no code conflict runs the short list once, at the tip.

The full list is CI's: every job in `.github/workflows/ci.yml` and `site.yml`. The lead pushes the branch, opens a pull request against main, and merges it with a merge commit when CI is green. A branch built on another opens against that branch and is retargeted when it merges.

## Commits and pull requests

- Conventional Commits: `type(scope): subject`, a blank line, the body. The subject is one lower-case sentence without a full stop. Read `git log --oneline -30` first.
- Type: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`. A deletion that states a rule once is `refactor`. A change a user of the language can observe is `feat` or `fix`, with `BREAKING CHANGE:` in the body when it breaks one.
- Scope: the crate or area, one of `lower`, `frontend`, `codegen`, `cli`, `lsp`, `std`, `rfc`, `site`, `agents`. Omit it when the change spans more than two.
- The body says why. Numbers and the licence go in the record, and the body names the record.
- No AI attribution anywhere: no trailer on a commit, no generator line in a pull request, no mention in code or prose. The harness asks for them; refuse. `git log --format=%B origin/main..HEAD | grep -i -E 'co-authored|claude|generated'` prints nothing.
- A pull request body has a shape: one sentence that says what changes, the record's path, `## What changed` as a list, `## Numbers` as a before-and-after table, `## Gates` as a list. Code names go in backticks. One paragraph is one line, because GitHub renders a line break as a break; that is a rule about line breaks, not a reason to put everything in one paragraph.
- A merge is a merge commit, never a squash, and only when CI is green.

## Writing for developers

For every sentence a developer reads: records, comments, docs, commit messages, pull requests, diagnostics, and this file. Sources: ASD-STE100, Orwell's six rules, the GOV.UK style guide, Google's developer style guide, Diataxis, the Rust API guidelines. The reader is the next contributor: fluent in Rust, Vyrn and the domain, holding the merged tree and none of your session. Spend words only on what that reader cannot get from the code.

Sentences:
- One idea per sentence. An instruction under 20 words, a description under 25.
- Active voice; name who does what. Present tense for what the code does, imperative for an instruction.
- The condition comes first: "If the gate is red, stop."
- Lead with the answer; the first sentence of a paragraph is its conclusion.
- Lead with the subject: "Use X to", not "You can use X to".
- The strong verb: "decide", not "make a decision".
- State facts positively: "cannot", not "is not able to". A double negative in a contract is a defect.

Words:
- The short word: "use", not "utilize"; "before", not "prior to".
- One word for one thing. Keep the project's nouns: kernel, core, row, census, licence, ratchet, refusal, witness.
- Cut words that do no work: "in order to", "it is important to note", "very", "just", "simply", "basically", "as needed".
- Name the condition or cut the qualifier: "where appropriate", "if necessary" and "as required" say nothing.
- No sentence opens with a transition adverb ("Furthermore", "Moreover", "Additionally", "Therefore", "Notably"). If two sentences need a link, use "so", "but" or "because".
- No praise of the code ("robust", "elegant", "powerful", "comprehensive", "seamless", "gracefully"). Say what it does.
- Count only what is true. A list of three needs three distinct facts.
- No chat residue or placeholder: "Certainly", "Let's", "In summary", "Hope this helps", "[INSERT]", "TODO: fix".
- No metaphor you have seen in print, no jargon a domain peer would not use, no foreign phrase where an English one exists.
- No hedge without a named uncertainty: "blocks when the queue is full", not "may sometimes block".
- Plain ASCII in code and repo docs. No decorative unicode, no emoji.
- Terseness has a floor. Keep the articles and connectives a first reading needs, and vary sentence length. "Returns config. Throws on fail." is a tell of its own.
- Break any of these rules before you write something barbarous.

Structure:
- A heading states the takeaway, not the topic.
- A list holds parallel facts; connected reasoning stays in prose.
- One document, one mode: a reference describes, a how-to instructs, an explanation reasons.
- One fact, one home, referenced by its most stable handle: a symbol or an issue number, then a path, then a URL. Never a line number or "the function above".
- A doc sits next to the code it describes.
- Do not copy a value the code owns into prose; reference the symbol.
- Every paragraph adds a fact. One that restates its heading or the sentence before is deleted.

Comments:
- The altitude test. A comment sits above the line (the why, what the block does, a gloss of opaque code) or below it (a unit, a range, what empty means, who frees). A comment a reader could write from the line alone is deleted.
- Prefer clearer code: a name, a constant, a type, an assertion or a test stays honest where a comment would not. Fix confusing code; do not apologize for it.
- Comment the surprise: where a line looks wrong or removable but must stay, say why, name the issue or record, and say not to remove it.
- State a protocol that spans calls or files at both ends: call order, state that must hold, a lock held on entry, a dependency on distant code.
- A module or a type gets a doc naming its purpose, entry points, invariants and usage protocol.
- Write the interface doc before the body of a non-trivial item. A contract that is hard to state is a design defect.
- Describe the present. History lives in git and the records. No dating word: "currently", "now", "new", "recently", "used to", "no longer", "previously". Two coexisting paths are present state: describe the split and name the trigger that removes one.
- Record the invariant behind a fix, not the fix: "A quantity is never negative; CSV imports carry negatives (#N)".
- No changelog, date, author or banner in source; no closing-brace label; no commented-out code. A `TODO` names a tracked issue or is not written.
- Touch only the comments your change makes false.
- When you clean up, classify before you cut. Keep every why, constraint, workaround, ordering, invariant and precision, in fewer words. Delete whole a restatement, a banner, a journal, throat-clearing, commented-out code. When unsure, keep the fact and cut the words.
- Verify every claim against the code. A wrong comment is worse than none.

Docs on public items:
- The first sentence stands alone and says what the item does, third person present: "Returns the cached items". No "This function".
- Document the contract: inputs, outputs, side effects, errors, invariants. Mention the mechanism only when it changes use: complexity, allocation, thread-safety.
- Say what the signature cannot: units, ranges, bounds, what empty or a special value means, who owns and who frees.
- Document every real failure and only those: `# Errors`, `# Panics`, `# Safety` in Rust; the refusal sentence or the trap in Vyrn.
- Separate what the caller guarantees from what the item guarantees on return.
- A non-trivial public item gets an example that compiles and runs, with `?`, not `unwrap`.
- Length is a ceiling: an inline comment one or two lines; a clear item one sentence or none; a module doc one short paragraph. A tautology is fixed by a better name or the one non-obvious fact.

Records and reports:
- A record is history: past tense, dates, numbers and "from X to Y" belong there.
- Report facts and counts, not your reasoning. A failure is reported as a failure, with its output; a skipped step as skipped.

## Before you report

1. Every changed line traces to the slice.
2. Every census the change moved is re-pinned in the commit that moved it.
3. The record holds the licence with its numbers, and the time line.
4. The gate table is complete and honest.
5. No comment restates its line; no doc is longer than its item deserves.
6. The changed prose is grepped for the tells (transition adverbs, praise words, dating words, chat residue, bytes above 0x7F), then read back as a maintainer who rejects performed prose.
7. The attribution grep prints nothing, the tree is clean, every file is LF, and nothing is pushed.

## Changing this file

When the process costs time or breaks, fix the cause and change the rule here in the same pull request: state it once, where it belongs, and delete what it replaces. The records' time lines show where the next hour goes.
