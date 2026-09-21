# Skills

A skill is a **runbook**: a procedure written down once, so no context window has to re-derive it.
It is not a tool (a verb on the machine) and not a memory (a preference). It sequences tools toward a
definition of done, using only what the identity may already do. **Settings → Skills** lists them.

## The file

One directory per skill, holding a `SKILL.md`. The directory name is the skill's name: lower case,
digits, `.`, `-` and `_`.

```markdown
---
version: 1
tools: fs_read, fs_write
writes: .aegis/artefacts
---

# inbox.triage

## When to use it
## Inputs required and tools it will call
## Steps
## How to validate
## What to return
## What requires approval
## What to do if the source is missing
```

- **All seven headings are required, in that order, with nothing else beside them.** They are the
  contract between the author and the runner; a runbook without *What to do if the source is missing*
  is a runbook whose failure mode is inventing an answer.
- A file that does not parse is listed with the reason and never offered to a model.
- `tools:` lets a run be refused before it starts.
- `writes:` (optional) names the folders the runbook's `fs_write` calls land in, comma-separated,
  relative to the workspace; a segment may end in one `*`. It needs `fs_write` in `tools:`, and it
  can never name `.git/` or `world/`. Putting the runbook on a clock offers exactly those folders as
  standing approvals, and write-anywhere only as a widening.
- The catalog line is the first paragraph of *When to use it*, cut at 160 characters. Keep that
  paragraph short.

## Where they live

| Scope | Lives | For |
| --- | --- | --- |
| **Library** | `skills/` in the application-data directory | how *you* work |
| **Workspace** | `.aegis/skills/` in the project | how *this* project works; travels with the repository |
| **Identity** | the identity's skill allow-list | which of the above it may run |

A workspace runbook shadows a library runbook of the same name, and the panel says so. The seeded
runbooks are ordinary files you can rewrite or delete; each name is seeded once
([Data](data.md)). Nothing is granted by being seeded.

## Catalog in, body on demand

Every request carries the **catalog**: one line per runbook the identity may run — name, version,
origin, when to use it, tools. The steps are loaded only when the model calls `skill_run`, for that
reply. Twenty procedures cost twenty lines. Edits take effect on the next run; **Re-read** refreshes
the panel.

## A skill grants nothing

- A skill the identity was not granted is refused before its file is even located, with no dialog.
- A runbook declaring a tool the identity does not hold fails closed at `skill_run`, naming the tool.
- Every step is an ordinary tool call: same matrix, same dialog, same audit line.

## Proposing a skill

A session can **propose** a runbook instead of writing it live: an ordinary `fs_write` of
`.aegis/skills/<name>/PROPOSAL.md`. A delegated brief hands the same file back as an artefact.

- **A proposal is never run** — not by `skill_run`, not by a routine, even for an identity already
  granted the name.
- **Applying is a copy, signed every time.** Ask a session to write the proposal, byte for byte, to
  `SKILL.md` beside it. Aegis recognises that write as an apply: the dialog is titled *Apply a skill
  proposal*, names the skill, and offers no session grant. A held workspace-write grant does not
  cover it.
- **Applying grants nothing.** Tick the skill on an identity in Settings.
- **Applying never replaces a runbook.** It is refused when a `SKILL.md` already exists, when the
  proposal does not parse, inside a brief, and in a routine.

*Settings → Skills* lists the open project's proposals as *not applied*, *applied*, or *a runbook is
already there*, with any parse error. There is no Apply button, and an applied `PROPOSAL.md` stays
until you delete it.

## The return

A run ends with `skill_return`, which is checked, not believed:

```
status: done | blocked | needs_you
summary:             # five lines max
artefacts:           # paths inside the workspace
evidence:            # a test, a diff, a capture
open_questions:
next_owner:
```

- A `done` naming an artefact that is not on disk, or naming nothing, is refused.
- `blocked` and `needs_you` need at least one open question.
- A refused return leaves the run open for a corrected one.

**Every audit line between `skill_run` and `skill_return` carries the skill's name**, which is what
makes a run budgetable and replayable. A run may span up to four turns of a session. A turn stops
calling tools when it repeats the same calls three times, or after 64 rounds of tools — the same
limit with or without a runbook. Hitting either gives the model one more round to finish; it is not
asked to wait for you to type continue.

Reasoning: `COS.md` *Skills*. Design: `PLAN.md` § 7.6 and § 7.13.
