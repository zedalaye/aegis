---
version: 1
tools: fs_list, fs_read, fs_write
---

# world.draft

## When to use it

When a person asks for help founding this workspace's world, or amending one
after a delta was perceived. Only in a session somebody is sitting in: inside a
brief every write to `world/` is refused, and that refusal is the point — a
specialist does not change what the project is.

Not for a workspace that has nothing to protect. A watch folder or a wish list
has no essence and no oracle, and six templates in one are worse than nothing.
If you cannot say in one line what this project *is*, say so and stop.

## Inputs required and tools it will call

- Whatever the project already says about itself: a README, the module headers,
  an existing `world/`, `.aegis/decisions/DECISIONS.md`.
- What the human tells you it is for. That part is not in the files.

Calls `fs_list` and `fs_read` to gather, and `fs_write` for each file it
drafts. Every write is put to the person, and they may allow the rest of the
session in one answer — that answer is theirs to give, not something to ask for
twice.

## Steps

1. `fs_read` `world/` first if it is there. You are amending, not starting
   over: what is already written stands unless the human says it moves.
2. Gather. Read the README, the entry points, the module headers, the decisions
   already filed. Do not open anything `world/sources.yml` declares — those are
   perceived already, and a read of one that has not changed is refused.
3. Ask the human two questions and wait for the answers: what is this for, and
   how would you know a new version of it was right. Do not guess either. They
   are `essence.md` and `oracle.md`, and they are the two files that make the
   others worth having.
4. Draft what the evidence supports, one `fs_write` at a time, smallest first:
   `world/schema.md` (the shapes you actually found, named as they are named in
   the code) and `world/behaviours.md` (how it behaves — and for anything that
   reads like a rule, the perimeter it holds inside: this contract, this role,
   this class of tasks. A local failure is not an invariant).
5. Write `world/essence.md` and `world/oracle.md` from the human's answers, in
   their words. Quote them rather than improving them.
6. Say what you left empty and why. A world with four files is a world; a world
   with six files two of which you invented is a liability.

## How to validate

Every line in `schema.md` and `behaviours.md` can be traced to a file you read,
and you can name which. Nothing in `essence.md` or `oracle.md` is yours. No
file restates another. A person who has never seen this project can read
`essence.md` and say what it is for.

## What to return

`skill_return` with `status: done`, every file you wrote in `artefacts`, and a
summary of at most five lines: what the world now says, and what is still
blank. `status: needs_you` when the human has not answered step 3 — that is not
a blocker to work around, it is the work.

## What requires approval

Every write into `world/` is put to the person, at high risk, and they may
allow the rest of the session in one answer. Nothing else here is granted by
it: a standing approval for the constitution covers the constitution.

If a write is refused outright rather than asked about, you are inside a brief.
Stop and return `needs_you`: founding or amending a world is not delegated
work.

## What to do if the source is missing

If there is nothing to read — an empty folder, no README, no code — say so and
return `status: blocked`. A world drafted from nothing is six files of
plausible prose, which is the most expensive possible thing to have to unlearn.
