# B1 — Cut the noise out of every page

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`. Work on a branch named `ox/prose-sweep`. This job
edits `site/`. Do not run it at the same time as B2 or B3.

## Objective

The owner's complaint, in three parts:

1. Sentences like "Fetches once, pins the sha256 in vyrn.lock, writes the alias
   into vyrn.json. The ref is main; a release tag works the same." are noise on a
   page. They explain a mechanism to a reader who came for a command.
2. The text on many pages is too dense and hard to read, and much of it means
   nothing.
3. The documentation carries RFC numbers as text and other internal references
   that a reader outside the project cannot use.

This job removes all three, page by page.

## The rule, which is not negotiable

For every sentence on every page, apply this test in order:

1. **Does a reader who came for this page's task need this sentence to finish
   the task?** If yes, keep it. Shorten it.
2. **Is it a true and useful fact that this reader does not need right now?**
   Then it belongs on the page that owns it. Replace it with a link to that
   page. The link text names the thing, not the action: write
   `[the lockfile](/docs/lockfile)`, not `[read more](/docs/lockfile)`.
3. **Is it a true fact with no page that owns it?** Delete it and record it in
   the report under `Facts with no home`. The owner will decide whether that page
   should exist. Do not invent a page.
4. **Anything else.** Delete it.

The owner offered a `(?)` hover affordance as an alternative to deletion and
said a link to the documentation is better. So: **prefer the link. Never a hover
tooltip.** A hover tooltip is unreachable on a touch screen and unreachable by
keyboard, and this project cares about that. If the sweep finds ten or more facts
that genuinely need to sit next to the text and have no page, report them under
`Facts with no home` and stop. A note component would then be built as a separate
job, as a button with `aria-expanded`, not as a hover.

## RFC references

A user-facing page must not carry `RFC-00NN` as text. Remove every one from
`site/`.

Where the RFC number was the only citation for a claim, either the claim stands
on its own or the sentence goes. The backstage area of the site is exempt: it is
for the project, and RFC numbers there are the subject.

Check which routes are backstage before editing. Read `site/app/nav.vyrn` and
`site/export.vyrn` to find the split. The site has a consumer half and a
backstage half, and this rule applies to the consumer half only.

## Density

After the cuts, every page must satisfy:

- No paragraph over four sentences.
- No sentence over 25 words.
- Active voice. See `.claude/ox/RULES.md` for the style rules.
- No paragraph that only restates the heading above it.
- Every page opens with one sentence saying what the reader can do after reading
  it.

Do not pad a page to meet a length. Deleting is the point.

## The fan-out

Group the pages by route section and give one subagent each section. A subagent
must not spawn a subagent. Two subagents must never edit the same file.

Sections, from `site/app/routes/`:
`index.vyx`, `install.vyx`, `why-vyrn.vyx`, `philosophy.vyx`, `play.vyx`,
`releases.vyx`, `compare.vyx`, `benchmarks.vyx`, `editors.vyx`, `error.vyx`,
`guide/`, `docs/`, `tooling/`, `tooling/std/`, `web/`, `explore/`.

Also `site/guide/*.vyrn`, which holds the runnable guide programs. Their comments
are read by users. Apply the same rule to the comments and change no code.

## After every section

Rebuild and diff. A page whose bytes changed but whose meaning you did not
intend to change is a defect.

```
cd compiler && cargo build --release -p vyrn-cli
cd ../ && compiler/target/release/vyrn run site/export.vyrn out
compiler/target/release/vyrn fmt --check site/app/*.vyrn site/guide/*.vyrn site/export.vyrn
```

Then run the site tests exactly as `.github/workflows/site.yml` runs them, and
the guide tests. Read that file for the commands. Every route must still answer,
every navigation row must still point at a route that exists, and the guide
programs must still pass their tests.

## The report

Write `rfcs/census/prose-sweep.md`:

1. Words before, words after, per section. Use `wc -w` on the rendered text, not
   on the source.
2. `Facts with no home` — every fact deleted under rule 3, with the page it came
   from and one sentence on what it said.
3. `Links added` — every link created under rule 2, with source page and target.
4. `Pages that needed no change`, if any.
5. `RFC references removed`, with a count and the pages.

## What this job must not do

- Do not change any Vyrn code, only text and the markup that holds it.
- Do not add a component.
- Do not change navigation structure.
- Do not delete a whole page. If a page should not exist, say so in the report.
- Open one pull request from `ox/prose-sweep` when every section is done and
  every gate passes. Title it `site: cut the noise`. Remember: no hard-wrapping
  in the body, and no AI attribution anywhere.
