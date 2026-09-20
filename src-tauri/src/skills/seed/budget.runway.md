---
version: 1
tools: fs_read, fs_write
---

# budget.runway

## When to use it

When somebody asks how long the money lasts. It reads a position file and the
standing commitments and answers in months, with the arithmetic shown.

It answers a question; it does not decide anything about it. What to cut, what
to sell and what to take on are decisions, and they are somebody's.

## Inputs required and tools it will call

- The position file, as a path — one `budget.position` wrote. If you were not
  given one, that is the end of the run. A runway computed from whatever was
  written most recently is a runway for the wrong month, and it will read
  perfectly.
- The standing commitments: `.aegis/decisions/DECISIONS.md`, a contracts file, a
  subscriptions list — whatever this workspace keeps them in. And expected
  income, if any is written down anywhere.

Calls `fs_read` for those and `fs_write` for the note. It runs nothing, reaches
no account, and moves no money.

## Steps

1. `fs_read` the position file, and read its first line: that is how current
   this answer can be. A runway computed on a three-month-old position is a
   three-month-old runway, and it says so in its own first line.
2. List what goes out, one line each, with **how often** beside it and the file
   that says so. Monthly, quarterly, annual, one-off. Never a rate you inferred
   from a single charge: one appearance of a bill is one appearance, and an
   annual subscription billed in March is invisible for eleven months.
3. Put everything on the same period before adding anything — an annual figure
   divided by twelve, and the division shown. This is the step where a runway
   goes wrong, and it goes wrong quietly.
4. Do the same for what comes in, and count only what a file supports. Work that
   is likely, an invoice that will probably be paid, a client who usually
   renews: none of those is income, and each belongs in a line at the end saying
   what would change the answer.
5. Divide, and show the division: what is held, over what goes out net each
   month, is how many months. Write the numbers out so a reader can redo it.
6. Give a **range**, not a point. The low end assumes nothing uncertain arrives;
   the high end assumes all of it does. Say which assumption each end rests on.
   A single number is what somebody plans against, and the inputs almost never
   support one.
7. `fs_write` `.aegis/artefacts/runway-<date>.md`: how current the position is,
   what goes out, what comes in, the division, the range, and what would narrow
   it. End with the three things that would change the answer most.
8. Stop. Do not recommend a cut, do not propose a sale, and do not rank the
   outgoings by what you would drop first. That is the decision this note exists
   to inform.

## How to validate

Every outgoing names its file and its frequency. Every period conversion shows
its division. Nothing counted as income lacks a file behind it. The answer is a
range, and each end names the assumption it rests on. The note's first line says
how current the position under it is.

## What to return

`skill_return` with `status: done`, the note in `artefacts`, the position file
and the commitment files in `evidence`, and a summary of at most five lines: the
range in months, as of when, and what would narrow it.

`status: needs_you` when the position is older than the period you are dividing
by — a runway from a position older than a month is arithmetic on something that
has already changed — and when a commitment is named nowhere in writing. Say
which in `open_questions`.

## What requires approval

One `fs_write` inside the workspace. Nothing here spends, cancels, sells or
transfers, and no later version of it does: money leaving is behind a human gate
(`PLAN.md` § 7.4), and there is no tool in this build that could.

## What to do if the source is missing

No position file, no run: return `status: blocked` and ask for the path. Do not
build one on the way — that is `budget.position`, it has its own reconciliation,
and a runway resting on figures nobody reconciled is a confident number with
nothing under it.

If the commitments are not written down anywhere, say so and answer only from
what is: a workspace that has not recorded what it pays every month is a fact
worth the first line of the note. A read you were refused, by the person or by
the round limit that ends a turn, is an outgoing you did not count — name it,
leave it out, and return `status: needs_you`, because the runway you would have
written is too long rather than too short.
