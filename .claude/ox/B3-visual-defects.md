# B3 — Five visual defects, one branch

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`. Work on a branch named `ox/visual-defects`. This
job edits `site/`. Do not run it at the same time as B1 or B2.

Fix these five, in this order, each as its own commit. Each one is a real defect
the owner reported. Find the root cause before editing. A fix in the template
that leaves the shared component broken is not a fix.

## 1. The copy button is missing on the editors page

Symptom: `/tooling/editors.html` shows no copy button.

The markup exists. `site/app/routes/tooling/editors.vyx:99` has a `data-copy`
block and line 101 has a `data-copy-btn` button. So either the script that wires
`data-copy` is not on that page, or a rule hides the button, or the block is
inside a branch that does not render.

Find which. There are two files named `editors.vyx`, one at
`site/app/routes/editors.vyx` and one at `site/app/routes/tooling/editors.vyx`.
Confirm which one serves the published URL before you change anything.

Then check every other page that uses `data-copy` and `data-copy-md`. If the
wiring is per-page, that is the root cause and the fix belongs where every page
gets it, not on this one page. List every page that has the same defect and fix
them all.

Add a test that asserts the wiring script is present on every page that emits a
`data-copy` block.

## 2. The install page downloads the script instead of showing it

Symptom: on `/install.html`, the link `View the install script` starts a
download. It says View. It must show.

The link is at `site/app/routes/install.vyx:130`, and
`site/app/repo.vyrn:159` explains that the site serves its own install scripts.

The cause is the content type or the `Content-Disposition` of the served file,
or the file extension. Fix it so the link renders the script as readable text in
the browser, with the same highlighting the rest of the site uses for code if
that is cheap, and as plain readable text if it is not.

Keep a separate way to download it, clearly labelled `Download`, if one is
wanted. The reader must be able to read the script before running it. That is
the whole point of the link.

## 3. A line of equals signs leaks into "On this page"

Symptom: `===========================================================================`
appears in the on-this-page list.

That string is a Markdown setext heading underline. Something in the heading
extractor is treating the underline, or the line above it, as a heading title.
Read `site/app/markdown.vyrn` and `site/app/docshell.vyrn`.

Fix the extractor, not the one document that shows the symptom. Then search
every source document for setext headings and confirm each one now produces the
right title.

The owner also said this kind of entry looks wrong there at all. So while you
are in the extractor, apply these rules and report what changed:

- The on-this-page list holds headings, and nothing else.
- It shows one level of nesting at most.
- A page with fewer than three headings shows no list.
- No entry is empty, and no entry is only punctuation.

Add a test that no on-this-page entry consists only of punctuation.

## 4. Code blocks are too big and too clumsy

Symptom, in the owner's words: code blocks look too clumsy and big.

Measure before you change. Build the site, open three pages with code blocks,
and record: the block padding, the font size, the line height, the border
radius, the margin above and below, and the ratio of the block height to the
number of lines in it.

Then reduce. The target is that a six-line example takes roughly the vertical
space of six lines of body text plus its padding, and no more. Specifically:

- Line height in a code block at most 1.5.
- Font size at most the body size, and at least 13px at the default root size.
- Vertical padding no larger than one line height.
- The block must not carry both a border and a background fill and a shadow.
  Pick one to carry the edge.
- Long lines scroll inside the block. The page never scrolls sideways.
- The copy button does not add height. It sits over the block.

Do the same for the highlighted output, which is produced by `site/app/hl.vyrn`
and its CSS. Do not change what is highlighted, only how it is spaced.

Take a screenshot before and after for three pages and put both in the report.

## 5. The light theme flashes white and is not pastel

Two separate faults.

**The flash.** `site/public/theme.js` is a classic blocking script fetched from a
URL, loaded at `site/export.vyrn:1012`. The browser must complete a request
before it can paint, so on a cold load the page paints the default background
first. The fix is to inline the few lines that resolve the theme into the
document head, so no request stands between parsing and painting. Keep the rest
of `theme.js` external. Read `site/public/theme.js` and split it: the smallest
piece that sets `data-theme` goes inline, the toggle and the listeners stay in
the file.

Also add `<meta name="color-scheme" content="light dark">` so the browser paints
its own default background in the right colour before any CSS applies.

Verify the fix. Load the built page in the browser preview with the cache
disabled and confirm no white frame. If you cannot verify it, say so in the
report rather than claiming it.

**The palette.** The light palette must be pastel, not white. Change the tokens
only. Read the palette block in `site/public/style.css`, at the comment near
line 230.

Constraints that are not negotiable:

- Body text against the page background must reach a contrast ratio of 7 to 1.
- Any other text must reach 4.5 to 1.
- A focus outline must reach 3 to 1 against both the element and the background.
- Check every changed pair with a real contrast calculation and put the numbers
  in the report. A pastel theme that fails contrast is a worse defect than the
  one being fixed.

## Gates

After each commit:

```
cd compiler && cargo build --release -p vyrn-cli
cd ../ && compiler/target/release/vyrn run site/export.vyrn out
compiler/target/release/vyrn fmt --check site/app/*.vyrn site/guide/*.vyrn site/export.vyrn
```

Plus the site test steps from `.github/workflows/site.yml`, and the browser
runtime tests named in `.github/workflows/ci.yml`.

## The report

Write `rfcs/census/visual-defects.md`: for each of the five, the root cause you
found, the fix, the other places that had the same defect, and the evidence.
Include the code block measurements before and after, the screenshots, and the
contrast numbers.

## What this job must not do

- Do not redesign the site.
- Do not change page text. That is B1.
- Do not change metadata. That is B2.
- Open one pull request from `ox/visual-defects`, titled
  `site: five visual defects`. No hard-wrapping in the body. No AI attribution.
