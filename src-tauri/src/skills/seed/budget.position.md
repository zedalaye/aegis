---
version: 2
tools: fs_list, fs_read, fs_write
writes: .aegis/artefacts
---

# budget.position

## When to use it

When exports have arrived and somebody needs one page saying what is held and
what is owed. One run turns them into one position file.

The exports are files you put there: a bank CSV, a broker statement, an invoice
ledger, a spreadsheet saved as text. Nothing here connects to an account, and
nothing here places an order — this is surveillance, and Aegis is not a broker.

## Inputs required and tools it will call

- The folder the exports are in — `.aegis/briefs/` unless you were given
  another. `fs_list` it first, so the position covers what is on disk.
- Which currency the position is written in, if more than one appears. Without
  one, keep the currencies apart rather than picking.

Calls `fs_list`, `fs_read` for the exports, and `fs_write` for the position
file. Nothing here runs a program, reaches an account, or trades.

## Steps

1. `fs_list` the folder and `fs_read` each export. For each, find its **as-of
   date** — the statement date, the export timestamp, the last row's date — and
   write it down before reading a figure out of it. An export with no date in it
   is dated *unknown*, which is a fact about the position and not a gap to fill
   with the file's modification time.
2. Copy figures; do not restate them. Every line of the position carries the
   number as the export wrote it, the export's file name, and where in it —
   an account, a row, a label. A figure that cannot name where it came from does
   not go in the file.
3. Two exports usually overlap, and the same transaction in two files is one
   transaction. Match on the three things that identify it — account, date,
   amount — and where two lines match on all three, take one and say which file
   you took it from. Where they nearly match, take neither and list it as a
   discrepancy.
4. Do the arithmetic **in the open**. A total appears with its addends beside
   it, so a reader can redo it. Then reconcile: if the export states its own
   total, compare yours to it and put both in the file. If they differ, the
   difference goes in the file as a number, and this run returns `needs_you`.
   Do not round the difference away and do not adjust a line to make it close.
5. Do not add across currencies unless you were given a rate and the date of
   that rate, and then say both on the line where you used them. Two currencies
   summed at a rate nobody named is a number that looks like money and is not.
6. Separate what is **held** from what is **owed** and from what is
   **committed** — money that exists, money somebody else is owed, and money
   already spoken for by a standing commitment. Anything you cannot place in one
   of the three goes in a fourth list called *unplaced*, with its source.
7. `fs_write` `.aegis/artefacts/position-<date>.md`. Its first line is the
   **stalest** as-of date among the sources, not today's, because that is how
   current the position actually is. Then the four lists, then the
   reconciliation, then the exports by file name and date.
8. Stop. Do not act on any of it, do not propose a trade, do not cancel
   anything, and do not tell anybody what to buy.

## How to validate

Every figure names the export and the place in it that it came from. Every total
shows its addends. Every reconciliation shows both numbers and their difference.
No figure appears without a date. Nothing is summed across currencies without a
named rate and the date of that rate. The first line of the file is the oldest
as-of date in it.

## What to return

`skill_return` with `status: done`, the position file in `artefacts`, the export
paths in `evidence`, and a summary of at most five lines: what is held, what is
owed, as of when, and what did not reconcile.

`status: needs_you` whenever a total does not reconcile, whenever two exports
disagree about the same transaction, and whenever an export carries no date. All
three are the same fact — the position rests on something that has to be looked
at by a person — and a position file that quietly picked one side of any of them
is worse than one that stops.

## What requires approval

One `fs_write` inside the workspace. There is no tool here that reaches an
account, moves money, or places an order, and a connector installed later does
not change that: a read-only connector replaces where the figures come from, and
buying, selling and paying stay behind a human gate (`PLAN.md` § 7.4). This
runbook has no later version that ends in an order.

## What to do if the source is missing

If the folder is not there or holds no exports, return `status: blocked` and say
which folder you listed. Do not write a position from what the session said the
balance was: a number nobody exported is not a number, and money is the one
place where a confident guess is indistinguishable from a fact.

A read you were refused, by the person or by the round limit that ends a turn,
leaves an account out of the position. Name it in `open_questions`, leave its
lines out rather than estimating them, and return `status: needs_you`. A
position missing an account is useful; a position with an invented one is not.
