---
version: 2
tools: fs_list, fs_read, fs_write
writes: .aegis/artefacts
---

# revenue.pipeline

## When to use it

When somebody needs to see what is funded, what is not, and what each goal is
waiting on. It reports the gap and never claims to close it.

One run covers the goals as they stand today. It is a picture, not a plan.

## Inputs required and tools it will call

- The goals file — the one `wish.list` keeps.
- What is actually there: a position file `budget.position` wrote, or the
  decisions ledger, or whatever this workspace records committed money in.

Calls `fs_list` and `fs_read` for those and `fs_write` for the pipeline. It runs
nothing, spends nothing, and moves nothing.

## Steps

1. `fs_read` the goals file, and keep its ordering exactly. Whose ordering it is
   goes at the top of yours. Re-ranking the goals by what looks achievable is
   the one edit that would make this file feel helpful and make it somebody
   else's list.
2. `fs_read` what is actually available, and take its as-of date. A pipeline is
   as current as the position under it, and it says so in its first line.
3. For each goal in order: what it costs or *not priced*, what of it is covered,
   and the gap. A goal that is not priced has no gap — it has a missing price,
   and that is the line it gets.
4. Say what each goal is waiting on, taking it from the goals file rather than
   deciding: money, a decision, somebody else, nothing. Where it is waiting on
   money and the money is there, say that the wait is over — that is the one
   fact in this file somebody may want today.
5. **A proposal is not income.** A thesis in `.aegis/artefacts/` has no expected
   value, no probability and no line in this file. If one is mentioned at all it
   is in a closing list of *what exists as proposals*, by file name only, with
   no number beside it and no goal attached to it.
6. Do not connect a proposal to a goal, ever — not as a suggestion, not as an
   observation, not as "this would cover the second entry". That sentence is the
   whole reason these are two runbooks, and it is how a plan for a holiday
   becomes an argument for a trade.
7. Write the total gap plainly, and where the nearest unfunded goal is
   concerned, write the **condition** rather than a plan: what would have to be
   true for it to be funded — this much more, by this date, from something that
   already exists. Not a route to it, and not a suggestion about what to give up.
8. `fs_write` `.aegis/artefacts/pipeline-<date>.md`: how current the money
   figures are, the goals in the person's order with cost, covered and gap, what
   each waits on, the total gap, the condition on the nearest one, and the
   proposals by file name. Stop.

## How to validate

The goals are in the order the goals file has them, attributed to whoever set
it. Every money figure carries its as-of date and the file it came from. No
proposal has a number beside it. No line in the file connects a proposal to a
goal. Nothing suggests dropping or reordering anything.

## What to return

`skill_return` with `status: done`, the pipeline in `artefacts`, the goals and
money files in `evidence`, and a summary of at most five lines: how many goals
are funded, the total gap, and what the nearest one is waiting on.

`status: needs_you` when the money figures are older than the goals — a pipeline
built on a stale position is a picture of a month that has ended — and when a
goal's date has passed with a gap still open. The second is not a failure to
report gently: a date somebody set and did not meet is exactly what they asked
this file to show them.

## What requires approval

One `fs_write` inside the workspace. Nothing here spends, transfers, buys or
commits, and no later version of it does. What money moves and when is a human
decision every time (`PLAN.md` § 7.4); this file exists so that decision is
taken by somebody looking at the numbers rather than at a feeling about them.

## What to do if the source is missing

No goals file: `status: blocked`, and the thing to run is `wish.list`. Do not
assemble one from the conversation on the way past.

If there is no position file and no ledger, write the pipeline from the goals
alone, say in the first line that nothing on disk says what is available, and
return `status: needs_you`. A gap computed against a balance nobody exported is
a number that would be acted on, and it is the one kind of wrong this file must
not be.
