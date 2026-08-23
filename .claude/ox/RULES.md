# Rules for every Ox job in this directory

Read this file before any job prompt. It is not optional and it overrides your
tool defaults.

## Attribution

NEVER add AI attribution to any artifact in this repository. No
`Co-Authored-By` trailer and no other trailer on commits. No "Generated with
Claude Code", no "Co-authored by an AI", no model name, in commit messages, PR
bodies, code comments, or documents. If your tooling adds one, remove it.

## Pull request and issue bodies

NEVER hard-wrap prose. GitHub renders every newline in a PR or issue body as a
visible line break. One paragraph is one physical line, however long.

## Backends

Vyrn has three backends: the interpreter, native through the text IR, and wasm.
They must stay byte-identical. Therefore:

- Do NOT add a native body for a standard library function.
- Do NOT hard-implement any standard library behaviour inside a backend.
- Do NOT write one implementation for one backend and a different one for
  another.

A standard library fix is written in Vyrn, in `std/`, and serves all three
backends. If a task looks like it needs a backend implementation, stop and
report that instead of writing one.

## Writing style

The project uses ASD-STE100, Orwell's rules, and the GOV.UK style guide.

- Short sentences. One idea per sentence.
- Active voice.
- One word, one meaning. Do not vary a term for elegance.
- No stock metaphor. No "under the hood", no "battle-tested", no "the secret
  sauce", no "leverage" as a verb, no "seamless", no "robust", no "powerful".
- Keep the project nouns exactly: Vyrn, `.vyrn`, `.vyx`, `vyrn.json`,
  `vyrn.lock`, RFC-00NN.
- Do not write "we". Write what the thing does.

## Code style

- Vyrn identifiers are lowerCamelCase.
- Rust identifiers are snake_case.
- Vyrn source has no semicolons.

## Background work

Do NOT arm a watcher, poller, or wait loop for CI. Push, report what you
pushed, and end. Do not sleep in a loop. Do not poll a dev server. If you need
a live check, run it in the foreground with a timeout, and diagnose a failure
rather than waiting longer.

## Decide nothing

These jobs collect evidence and produce options. They do not choose. Where a
job asks for a recommendation, give it as a ranked list with the measurement or
the citation that supports each rank, and mark it `RECOMMENDATION, NOT A
DECISION`. Language syntax, standard library shape, and product direction are
decided by the repository owner, not by this job.

## Evidence

Every claim in a census gets a citation:

- A repository claim cites `path/to/file.vyrn:LINE`.
- An external claim cites a URL.
- A performance claim cites a command and its measured output. Run the command.
  Do not estimate. If you did not measure it, write `NOT MEASURED`.

A census with unsourced claims is worse than no census. It will be rejected.

## Gates you must pass before you report done

Run these from the repository root unless noted. Do not skip one because you
think your change could not have broken it.

```
cd compiler && cargo fmt --check
cd compiler && cargo fmt --check --manifest-path vyrn-lsp/Cargo.toml
cd compiler && cargo fmt --check --manifest-path vyrn-genwasm/Cargo.toml
cd compiler && cargo nextest run --no-fail-fast --status-level fail
cd compiler && cargo build --release -p vyrn-cli
compiler/target/release/vyrn doc --std -o docs/api --verify
compiler/target/release/vyrn fmt --check site/app/*.vyrn site/guide/*.vyrn site/export.vyrn
```

If you changed `site/`, also rebuild the site and compare it to the site built
before your change. Report every route whose bytes changed, and say why each
one was meant to change.

```
compiler/target/release/vyrn run site/export.vyrn out
```

If you changed `std/` or the compiler, also run the parity harness:

```
cd compiler && cargo test -p vyrn-cli --test parity -- --ignored
```

If a gate fails, fix it. Do not report a green result from a different working
tree than the one you changed.

## Reporting

Write your output to the exact path the job names. Do not invent a different
path. Do not write a summary into the repository root. Do not open a pull
request unless the job asks for one.

## Fan-out

A job prompt may say "one subagent per module" or "one subagent each". That is
the shape of the work, not a requirement to have a subagent tool. If you can run
subagents in parallel, do. If you cannot, do the same units of work one after
another, in the same order. Never let one unit see another unit's conclusions
before it forms its own, because that is how a wrong answer spreads across a
whole census.

A subagent must not spawn a subagent.

## The release binary already exists

`compiler/target/release/vyrn` is built. Use it. Do not run `cargo build` unless
you changed Rust code, because several jobs may be running beside you and cargo
takes a lock on the target directory.

## The working tree contains 70 repository clones

`.claude/worktrees/` holds about 70 full clones of this repository. Every
repository-wide `grep` or `find` sweeps them and returns numbers roughly 800
times too large.

Always exclude them:

```
grep -rn PATTERN --include=*.vyrn --exclude-dir=worktrees --exclude-dir=target .
```

Better: name the directories you mean. The live source is `std/`, `examples/`,
`site/`, `compiler/`, `rfcs/`, `docs/`, and `web/`.

And state what a count counts. "Files containing a match" and "files searched"
are different numbers, and reporting one as the other has already happened once
in this directory.
