---
version: 2
tools: fs_read, fs_write
writes: .aegis/artefacts
---

# watch.impact

## When to use it

When one thing in the watch looks like it might change what this project does.
One run takes one entry and says what would have to be true here.

It decides nothing and recommends nothing. What comes out is what would have to
change and what that would cost, which is what a person needs in order to
decide.

## Inputs required and tools it will call

- The entry, as a path — one `watch.sweep` wrote. If you were not given one,
  that is the end of the run. A note written about whatever looked most
  interesting is a note about the wrong thing, and it will read well.
- This project's own account of itself: `world/essence.md` if there is one, the
  decisions ledger, the board. Those are what "here" means, and with none of
  them this run has nothing to compare against.

Calls `fs_read` for those and `fs_write` for the note. It lists nothing, runs
nothing, and changes nothing about the project.

## Steps

1. `fs_read` the entry. Work from what it recorded as **shown**, not from what
   it quoted as claimed. A claim is a reason to look; it is not a fact about
   this project's options.
2. `fs_read` this project's account of itself, before writing a word. What the
   project is, what it has already decided, and what it is doing now are three
   different files, and a note written without them is a description of the item
   with our name pasted on it.
3. Name what here it touches, as paths: a dependency in a manifest, a decision
   in the ledger, a constraint in the essence, a bill somebody pays monthly. If
   you cannot name a file, the honest answer is that it touches nothing here,
   and that is a good thing for a watch to produce.
4. Write the condition, not the conclusion: **what would have to be true** for
   this to be worth doing. A number nobody has, a version that has not shipped,
   a licence somebody would have to accept, a migration nobody has costed. Each
   of those is something a person could go and find out.
5. Cost it in the units this project actually pays in — files that would be
   rewritten, a dependency added or dropped, an interface other people depend
   on, money per month — and cost doing nothing beside it. Never "a moderate
   effort".
6. If what it touches is `world/`, stop there and say so. That the constitution
   would have to change is the most useful thing this note can conclude and the
   one thing it must not act on: it is an écart, it belongs to a person, and no
   run of this writes `world/` — least of all one on a clock, which is not
   offered that approval at all.
7. `fs_write` `.aegis/artefacts/impact-<entry>.md`: the entry, what it touches
   by path, what would have to be true, what the change would cost, and what
   doing nothing would cost. No recommendation, and no order of work.
8. Stop. The decision is somebody's, and whoever takes it files it in the
   decisions ledger — not this run, and not as a suggestion phrased as one.

## How to validate

Every "it touches" line names a path in this project. Every condition is
something that could be found out rather than judged. No sentence in the note
recommends a course of action. The cost of doing nothing is there, because a
note that prices only the change is an argument for the change.

## What to return

`skill_return` with `status: done`, the note in `artefacts`, the entry and the
project files you read in `evidence`, and a summary of at most five lines: what
it touches, what would have to be true, and what it would cost.

A note saying it touches nothing here is a good run, and `done`. Most of what a
watch turns up touches nothing here; a note finding consequences in every item
is the sweep manufacturing work at the other end of the pack.

`status: needs_you` when the answer turns on a decision rather than a fact —
whether to take the cost, whether the constraint still holds, whether this is
the year for it — with the question in `open_questions` and no recommendation
attached to it. And always when the essence would have to change: that is an
écart, and it is the human's.

## What requires approval

One `fs_write` into `.aegis/artefacts/`, under the usual gate. A write into
`world/` is refused outright however clearly this note argues for it — amending
the constitution is a human decision (`PLAN.md` § 7.4), and a scheduled run is
never offered that approval.

## What to do if the source is missing

No entry, no run: return `status: blocked` and ask for the path. Do not take the
newest entry, and do not work from the digest — a digest line is a pointer, and
a note written from a pointer is written from a summary of a summary of a page.

If the project has no `world/`, no ledger and no board, say so and write only
what the manifest and the files on disk support: a project that has not written
down what it is is itself worth a line in the note. A read you were refused, by
the person or by the round limit that ends a turn, is a comparison you did not
make — leave that line out and return `status: needs_you`.
