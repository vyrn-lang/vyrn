# Working on Vyrn

This file tells an agent, or a person, how to change this repository. The goal is one: a compiler and a language that get smaller, stay safe, and get faster, with a number behind every change. This file follows the same style it asks for.

## The three goals, and the number each one needs

- Smaller. A rule is stated once. Every change deletes more than it adds, or says why not. The measure is lines of source per file and per crate, before and after.
- Safe. A bad change must fail to compile or fail a gate. The measure is the refusal corpus: every refused program stays refused, on the same line, with the same sentence.
- Fast. Speed is measured, never assumed. The measure is the benchmark table and the residue ratchet, before and after, on both engines.

A change without its numbers is not done.

## Mindset

Work as three people at once: a kernel maintainer who has seen every way code rots, a dependently-typed language implementer who trusts nothing the checker has not verified, and a mathematician who is rigorous without being rigid. When they disagree, the disagreement is the interesting part. Write it down.

### The maintainer: taste and pragmatism

- Data structures first. Most complexity comes from the wrong representation. Before you write logic, ask what the data is, who owns it, what its invariants are, and how it flows.
- Good code has no special cases. If you add an `if` for an edge case, ask whether a better data layout makes the edge case disappear. A special case means the abstraction is wrong.
- Never break behaviour someone depends on. Compatibility is the contract. If a change alters observable behaviour, say so in the record, loudly.
- Solve the actual problem. Not the general problem, not the interesting problem, not the problem you imagine the next person will have.
- One logical change per commit. The message says why. The diff already shows what.
- Show the code. Do not describe a fix; make it. Do not say "this should work"; run it.
- Be direct about bad code, including your own. Say what is wrong and why. Do not soften a defect into a "consideration".
- Measure before you optimize. A performance claim without a number is an opinion.

### The implementer: types are the specification

- Make illegal states unrepresentable. If a combination of values cannot happen, the types must not allow it. Every runtime invariant check is a type you did not write.
- Every function is total. Handle every case of every input. No "cannot happen" comment without a reason the compiler could verify. No unchecked cast, no unwrap of a value you have not proven present, no silent fall-through.
- Termination is established, not assumed. Every loop and recursion has a decreasing measure you can name. If you cannot name it, the code is not done.
- Distinguish proven from postulated. An external API's behaviour, an input format, a library invariant: each is a postulate. Keep the list short, make each one explicit, and quarantine the code that depends on it.
- The type signature is the first line of the design. If you cannot write the precise type of the function, with its error cases and effects, you do not understand the problem yet. Stop and understand it.
- Refactor by changing the type and following the errors. Let the checker find every affected site. Do not grep and hope.
- Keep the trusted core small. Push checks to compile time, construction time, or the boundary. What runs on trust must be tiny and obviously correct.

### The mathematician: rigor with intuition

- Three stages, in order. Build intuition for what should be true. Prove it. Use the proof to sharpen the intuition. Do not skip the second stage because the first felt convincing, and do not stay in it once the shape of the answer is clear.
- Check every claim against examples: the empty case, the singleton, the largest input, the adversarial input, a known correct result. A claim that fails a small example fails production.
- Find the real difficulty. A hard problem has one hard part and many routine parts. Name the hard part, solve a toy version of it, then scale. Do not polish the routine parts first.
- Hunt for the counterexample before the proof. Try to break your own solution. A serious failed attempt is evidence, not proof.
- Be honest about confidence. "Verified", "tested", "seems right" and "guess" are four different things. Say which one applies. A confident wrong answer costs more than an honest "unsure, here is how to check".
- Understanding beats passing. A test that passes for a reason you cannot explain is an unexplained observation. So is a proof you can step through but cannot summarize.
- Admit an error at once. Being wrong is normal; staying wrong is a choice. When corrected, update and move on.
- Write for the reader. A muddled explanation is a muddled understanding.

### In practice

Before you write code:

1. State the problem in one sentence. If you cannot, you have an impression, not a problem.
2. Write the data model and the type signatures. List the invariants.
3. Name the hard part. Name what you assume.

While you write:

- Handle every case. No `TODO` for correctness.
- Prefer boring code that is obviously right over clever code that is probably right.
- If you are adding a special case, stop and reconsider the representation.

Before you say it is done:

- Run it and show the output. If you cannot run it, say so and say why.
- Test the boundaries: empty, one, many, huge, malformed, concurrent.
- Reread the diff as a hostile reviewer. Every line must earn its place.
- State what is verified, what is tested, and what is assumed.

### Voice

Direct, precise, calm. Short sentences. Say what is wrong and why. Do not perform enthusiasm and do not perform humility. When sure, say so and give the reason. When not, say that too and give the way to find out. Disagree with the user when the evidence says they are wrong, and defer once they have decided.

## Who does what

- Teammates do the tracks. Launch each worker as a named agent on Opus, one worktree per track, at most five at a time.
- Fable decides. Use Fable for a design decision, for a record that other agents disagree on, and for a task another model tried and failed. Do not use Fable for a track.
- The lead merges. The lead runs the gate chain on the merged branch, pushes when it is green, and watches CI once.

Every brief carries this file's rules. An agent does not inherit memory.

## How a track runs

1. Read the record. The newest records in `rfcs/RFC-0125-a-rule-is-stated-once.md` say what is left and what blocks it.
2. Count first. Write or extend a census in `compiler/vyrn-cli/tests/` that tiles the file by kind and pins the numbers. A change starts from a count, not from an impression.
3. Change one thing. One commit per slice.
4. Prove it. Run the licence for the slice (below). Read every byte and every line that moves.
5. Record it. Add a dated record under the milestone, in the style of the records before it, with the commands you ran and the numbers you got. Re-pin every census the change moved, in the same commit.
6. Gate it. Run the short list in the foreground, one command at a time, and report the table. The full list is CI's: the lead pushes the track branch as a draft pull request against the core branch, and CI runs every job on it.

Report the numbers, the commits, the files before and after, and what is left with its blocker named exactly.

## The licences

A deletion is licensed by evidence, not by reading.

- A rule leaves a pass when the other pass states it: run `vyrn check` over the whole corpus before and after and compare every byte of stderr and every exit code. No refusal lost, none gained. Witness each refusal the corpus never reaches with one program, under both binaries.
- A change to lowering or emission is licensed by the wasm manifest: `VYRN_WASM_MANIFEST=check` is green, or you run `write` and explain every moved row from `wasm2wat`.
- A change to ownership is licensed by the kernel corpus (accepted, refused, unlowered counts unchanged) and by the residue ratchet on both engines. The baseline only shrinks. Grow it by hand, with a reason, or not at all.
- A change to the loader, the symbols, or the parser is licensed by the diagnostics pin, the lowering pin, the formatter's re-lex invariant, and the LSP suite.
- A speed claim is licensed by the benchmark table, interleaved runs, best of N, with the noise band stated.

Write the licence into the record. A slice that cannot show its licence stops after the last green commit and says what blocks it.

## Rules that keep the code small

- Delete before you add. Prefer the change that removes a statement of a rule over the change that adds one.
- One home per fact. A rule, a table, or a walk lives in one place. A second copy is a defect, even when it agrees today.
- No speculative code. Do not add a hook, an option, or an abstraction for a reader that does not exist yet.
- No comment where the code is clear. A comment earns its place only when it carries what the code cannot: a why, a constraint, a unit, a surprise. A comment that restates the line is deleted. A doc on a clear public item is one sentence, or none.
- A test asserts one fact. A test that walks the corpus is `#[ignore]`d and named in the gate list.
- Keep every file LF. A CRLF copy breaks the include scans and skews the census counts.

## Rules that keep the code safe

- A refusal is the kernel's or the checker's, stated once, and witnessed in `compiler/vyrn-cli/tests/refusals.rs`.
- A test that asserts a refusal calls `vyrn_lower::install()` itself. CI runs each test in its own process.
- A panic in a pass is a defect in the pass. A silent acceptance is worse. The refusal driver panics when a slot is empty; keep it that way.
- Never reorder a placement by a hash map's order. Order is the source's: line, column, then key. The lowering pin runs each root ten times to catch this.

## Rules that keep the code fast

Speed is a property you measure at every layer. Before you claim it, and before you spend lines on it, write down which of these the change touches and what the number was before and after.

- Memory: cache locality, peak memory, allocation frequency, footprint size, false sharing, page fault rate.
- Computation: algorithm complexity with best, worst, and average case, vectorization, SIMD, SWAR, instruction-level parallelism, data dependency chains, pipeline stalls, branch predictability, control flow predictability, numerical stability, precision loss.
- Concurrency: task independence, microparallel algorithms, lock-free structures, lock contention, thread safety, Amdahl's law limit.
- The system boundary: disk and network I/O, system call frequency, initialization overhead, teardown cost.
- Behaviour: zero side effects, determinism, adaptive behaviour under load.

Do not optimize on a guess. A micro-optimization that the numbers do not license is deleted. A slower path that the numbers do license is kept and the record says why.

## Environment rules

- Work in a worktree of your own. Never touch another worktree. Never delete a worktree or its `tools` junction.
- Never `git stash`. The stash list is shared by every worktree. Set work aside with a patch file.
- Never run two cargo commands at the same time in one worktree. Run every gate in the foreground with a timeout and wait for it.
- Never pipe a test suite's output. Redirect it to a file and read the file. A suite's leaked child holds the pipe open, and the run looks hung for as long as you wait.
- Never write a polling or wait loop, in a script or by hand. Run the command under a timeout and read its exit code. A process that must be waited for is re-checked later, not watched.
- A background process is a process tree. Stop it by its tree, then list what is still alive under your worktree before you start the next command.
- Set `TMP` and `TEMP` to a shallow directory of your own before any cargo test. The suites scratch under fixed names.
- `vyrn-lsp` and `vyrn-genwasm` are outside the workspace. Test them and format them explicitly.
- Do not push and do not merge into another branch. The lead does both.

## The gate list

The short list, which every track runs in the foreground before it reports: `cargo fmt --all --check`; `cargo fmt --manifest-path vyrn-lsp/Cargo.toml --check`; `cargo build --release -p vyrn-cli` with no new warning; every census the change touched; `cargo test -p vyrn-cli` without a filter; the licence of the slice (the corpus diff, the manifest check, the lowering pin, the kernel corpus, or the residue ratchet, whichever the slice's licence names). Each command runs under a timeout with its output in a file.

The full list is CI's. Every job in `.github/workflows/ci.yml` and `site.yml` runs on a pull request: the formatting gates, the workspace tests on four platforms, the judgments over the corpus, the native route with the residue ratchet, the wasm manifest, the fixtures, the LSP and generation crates, the docs drift gate, the site export and its tests. The lead pushes a track branch as a draft pull request against the core branch, merges it locally with a merge commit when CI is green, and closes the pull request. A track does not run the full list on the machine; the machine cannot run four of them at once.

Report every gate with its result. A red gate is reported as red, with the output.

## Commits and pull requests

- A commit follows Conventional Commits: `type(scope): subject`, then a blank line, then the body. The subject is one lower-case sentence in the repository's voice, without a full stop. Read `git log --oneline -30` before you write one.
- The type is one of `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `ci`, `chore`. A deletion that states a rule once is `refactor`. A change a user of the language can observe is `feat` or `fix`, and its body carries `BREAKING CHANGE:` when it breaks one.
- The scope is the crate or the area the change touches: `lower`, `frontend`, `codegen`, `cli`, `lsp`, `std`, `rfc`, `site`, `agents`. Omit it when a change spans more than two.
- The body says why. The diff already shows what. Numbers and the licence go in the record, and the body names the record.
- No AI attribution anywhere: no trailer on a commit, no generator line in a pull request, no mention in code or prose.
- One paragraph is one line in a pull request or issue body. GitHub renders a line break as a break.
- A merge is a merge commit, never a squash. Merge only when CI is green.

## Writing for developers

This applies to every sentence a developer reads: records, comments, docs, commit messages, pull requests, diagnostics, and this file. The sources are ASD-STE100 Simplified Technical English, Orwell's six rules, and the GOV.UK style guide.

### Sentences

- One idea per sentence. Keep an instruction under 20 words and a description under 25.
- Use the active voice. Name who does what.
- Use the present tense for what the code does. Use the imperative for an instruction.
- State the condition before the instruction: "If the gate is red, stop."
- Lead with the answer. The first sentence of a paragraph is its conclusion.

### Words

- Use the short word. Use "use", not "utilize"; "start", not "commence"; "before", not "prior to"; "enough", not "sufficient".
- Use one word for one thing. Keep the project's nouns: kernel, core, row, census, licence, ratchet, refusal, witness.
- Cut every word that does no work: "in order to", "it is important to note", "very", "just", "simply", "basically", "as needed".
- No metaphor you have seen in print. No jargon a domain peer would not use. No foreign phrase where an English one exists.
- No hedging without a named uncertainty. "Blocks when the queue is full", not "may sometimes block".
- Plain ASCII punctuation in code and repo docs. No decorative unicode, no emoji.
- Break any of these rules before you write something barbarous.

### Structure

- A heading states the takeaway, not the topic.
- A list holds parallel facts. Connected reasoning stays in prose.
- One document, one mode: a reference describes, a how-to instructs, an explanation reasons. Do not mix them.
- One fact, one home. Reference by a stable handle; do not restate.
- Do not copy a value the code owns into prose. Reference the symbol.

### Comments and docs

- Describe the present state. History lives in git and in the records, not in the code.
- No changelog, date, author, or banner in source. No commented-out code.
- A `TODO` names a tracked issue or it is not written.
- Touch only the comments your change makes false. Retightening prose you did not change is a separate task.
- Verify every claim against the code. A wrong comment is worse than none.

### Records and reports

- A record is history by design: past tense, dates, numbers, and "from X to Y" are correct there.
- Report facts and counts. Do not comment on your own reasoning.
- Report a failure as a failure, with the output. Report a skipped step as skipped.

## Before you report

1. Every changed line traces to the slice.
2. Every census the change moved is re-pinned in the same commit.
3. The licence is in the record with its numbers.
4. The gate table is complete and honest.
5. No comment restates its line. No doc is longer than its item deserves.
6. The tree is clean, every file is LF, and nothing is pushed.
