---
version: 1
tools: fs_list, fs_read, fs_write
---

# budget.alert

## When to use it

When a figure has crossed a line somebody set. It says what moved, against what
threshold and since when. It proposes nothing.

One run covers one threshold. The threshold is one somebody wrote down before
the move — not one you draw now around what happened.

## Inputs required and tools it will call

- The threshold, and where it is written: a line in `.aegis/decisions/DECISIONS.md`,
  a limits file, a note from the human. If no file names it, this runbook does
  not apply, and saying so is the run.
- The figure, from a position file or the export it came from — the same figure
  the threshold was written about, not a related one.

Calls `fs_list` and `fs_read` for those and `fs_write` for the note. It runs
nothing, reaches no account, and places no order.

## Steps

1. `fs_read` the threshold where it is written, and quote it, with the date it
   was written. A threshold you cannot quote is a threshold nobody set.
2. `fs_read` the figure and its as-of date. Check it is the figure the threshold
   names — the same account, the same holding, the same currency. A threshold on
   one thing compared against another is the most convincing wrong alert there
   is.
3. Say by how much, and since when: the value now, the value at the threshold,
   the difference, and the last date the figure was on the other side of it.
   Show the subtraction.
4. Say what it is **not**. A number that crossed a line is not a cause: a
   currency move, a fee, a transfer between two accounts you are watching
   separately, a statement that arrived late. Name the ones you could rule out
   from the files and the ones you could not.
5. If crossing the line is what the file said would happen when nothing was
   wrong — a quarterly bill, an annual renewal, a known drawdown — say so in the
   first line. Most crossings are that.
6. `fs_write` `.aegis/artefacts/alert-<figure>-<date>.md`: the quoted threshold
   and its date, the figure and its as-of date, the difference with its
   arithmetic, what it is not, and the one question a person would need answered
   to decide. One question, not a list.
7. Stop. **No proposal.** Not *consider reducing*, not *it may be worth
   reviewing*, not an ordering of options. Aegis has no tool that buys, sells,
   transfers or cancels, this runbook has no later version that ends in one, and
   an alert that ends in a recommendation is a trade being placed one sentence
   at a time.

## How to validate

The threshold in the note is a quotation with the date it was written. The
figure carries its as-of date and is the one the threshold names. The
subtraction is shown. There is no sentence in the note recommending an action,
and there is exactly one question at the end of it.

## What to return

`skill_return` with `status: done`, the note in `artefacts`, the threshold file
and the figure's source in `evidence`, and a summary of at most five lines: what
crossed what, by how much, since when, and the one question.

`status: needs_you` when acting on it would be irreversible and time matters —
which is most of why a threshold was set — with the question in `open_questions`
and still no recommendation attached to it. The human decides and the human
acts; this run has done its whole job by being read in time.

`status: blocked` when no file names the threshold. Do not infer one from the
history of the figure. A line drawn around what already happened turns every
move into a crossing, and a watch that alerts on everything is a watch nobody
reads.

## What requires approval

One `fs_write` inside the workspace. Nothing else, and nothing else is possible:
there is no tool in this build that trades, pays or transfers, and a read-only
connector installed later replaces where the figure comes from rather than what
may be done with it (`PLAN.md` § 7.6). Money moving is a human act, every time
(§ 7.4).

## What to do if the source is missing

If the figure's source is not there, return `status: blocked` and name the path.
Do not alert on a number from the conversation: an alert is the artefact people
act on fastest and check least, which is exactly why it may not rest on
something nobody can go and read.

If you could read the threshold but not the figure, say so and return
`status: needs_you` — a threshold with no current figure beside it is a reason
to go and look, and that is a sentence worth writing. The other way round is
`blocked`.
