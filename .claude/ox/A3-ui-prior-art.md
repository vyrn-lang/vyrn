# A3 — What a Vyrn component library must get right

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the output files this job writes.

## Objective

The owner intends to build a Vyrn component library in the shape of `reka-ui`
and `nuxt-ui`, with compile-time validation that prevents the defects
`nuxt/hints` and `html-validate` catch at run time. Before any of that is
designed, this job collects the ground truth: what those libraries ship, what
they get wrong, and what the validators check.

This job designs nothing and writes no Vyrn code.

## The five sources

Give each source its own group of subagents. A subagent must not spawn a
subagent. Run up to 32 subagents at a time.

### Source 1 — reka-ui components

Repository: `https://github.com/unovue/reka-ui`. Documentation:
`https://reka-ui.com`.

For every component it ships, one row:

| component | what it is | the accessibility contract it promises | the DOM structure it requires | the state it owns | the props that can be set to a contradictory pair |

The last column is the point of the job. A component that accepts both
`modelValue` and `defaultValue`, or both `disabled` and `required`, has a
combination that is meaningless. List every such pair you find, because each one
is a candidate for a Vyrn compile-time error.

### Source 2 — reka-ui defects

Read the open issues at `https://github.com/unovue/reka-ui/issues`. Read
`https://github.com/unovue/reka-ui/issues/2721` in full, and quote the actual
problem it describes.

Group every issue into a defect class. A defect class is a sentence of the form
"X goes wrong when Y". For each class:

| class | issues in it | what goes wrong | could a compiler have caught it | what the compiler would need to know |

The fourth column is `YES`, `NO`, or `PARTLY`, and the fifth column is what
makes it interesting. Answer it concretely. "The compiler would need to know
that a `Trigger` must have exactly one `Content` sibling in the same tree" is a
useful answer. "Better type safety" is not.

### Source 3 — nuxt-ui

Repository: `https://github.com/nuxt/ui`. For every component, one row with the
same columns as Source 1, plus a column `overlaps reka-ui`, because nuxt-ui is
built on reka-ui and the overlap tells the owner what a single Vyrn library
would need.

Also record how nuxt-ui does theming, and how a consumer overrides one part of
one component. That mechanism is the thing a compile-time library has to
replace.

### Source 4 — nuxt/hints

Read `https://raw.githubusercontent.com/nuxt/hints/refs/heads/main/README.md` in
full. It is a list of rules.

For every rule, one row:

| rule | what it forbids | why | when it can be checked | Vyrn today |

`when it can be checked` is one of `COMPILE TIME`, `RUN TIME`, `EITHER`. Justify
`COMPILE TIME` by naming the information the compiler would need and saying
whether a `.vyx` file carries it.

`Vyrn today` says whether `std/vyx-hints.vyrn` already implements the rule. Cite
the line. That file exists and is 35 KB. Read it before claiming a rule is
missing.

### Source 5 — html-validate

Read the rule index at `https://html-validate.org/rules/`. For every rule, one
row with the same five columns as Source 4.

Mark any rule that overlaps a nuxt/hints rule, and say which is stricter.

## The output

Five files under `rfcs/census/ui/`:

- `reka-components.md`
- `reka-defects.md`
- `nuxt-ui.md`
- `nuxt-hints-rules.md`
- `html-validate-rules.md`

Then one rollup, `rfcs/census/ui/README.md`, with:

1. A count of components, defect classes, and rules from each source.
2. A section `Rules a Vyrn compiler could enforce`, listing every rule from
   sources 4 and 5 marked `COMPILE TIME`, each with the information the compiler
   needs. This is the section the owner will read first.
3. A section `Rules that need run time`, with the reason each one does.
4. A section `Defects a type system could have prevented`, from source 2.
5. A section `What std/vyx-hints.vyrn already covers`, with line citations, and
   what it does not.

## What this job must not do

- Do not write Vyrn components.
- Do not write an RFC.
- Do not change `std/vyx-hints.vyrn`.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
