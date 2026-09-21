---
version: 2
tools: fs_list, fs_read, fs_write
writes: .aegis/artefacts
---

# revenue.thesis

## When to use it

When an idea for making money should be written down properly enough to be
wrong. One run writes one proposal, with what would show it false.

A trade idea, a thing to sell, a way to charge for something that is free. It is
an argument on disk. Nothing here executes anything.

## Inputs required and tools it will call

- The idea, in one sentence, from a person. This runbook does not generate ideas
  to fill a folder.
- Whatever is on disk that bears on it: a watch entry, a note, a page somebody
  saved, earlier theses in `.aegis/artefacts/`.

Calls `fs_list` and `fs_read` for those and `fs_write` for the thesis. It runs
nothing, reaches no account and places no order.

**It does not read the position file, and it does not read the wish list.** Both
are about this house rather than about the world, and a thesis written with
either open is a thesis sized by what there is to lose or aimed at what somebody
wants to buy.

## Steps

1. Write the claim in one sentence, in the present or past tense about the
   world: what is true that other people have not priced in, what somebody would
   pay for that nobody is charging for. If it takes a paragraph, it is more than
   one claim, and each gets its own file.
2. Say what would have to be true for it to work, as things somebody could go
   and check. Not "if adoption continues" — a number, a shipped version, a
   published price, a filing, a contract.
3. Write the **falsifier**: what would show this is wrong, and by when it would
   show it. A thesis with no falsifier is not an idea, it is a mood, and this is
   the step that decides whether the file is worth keeping.
4. Write what being wrong costs, in the units it would be paid in — money,
   months, a reputation with somebody named, an opportunity that closes.
5. Say who is on the other side of it and why they are there. Somebody is
   selling what you would buy, or not charging for what you would charge for,
   and the reason is usually not stupidity.
6. **No size, no allocation, no expected return, no entry or exit.** Not "a
   small position", not "worth a few percent", not "10x if it works". Those are
   the order, they are the human's, and a number attached to a thesis is the
   part people read.
7. Do not say what this would pay for. Not the car, not the runway, not the
   subscription. A thesis explained by what it would fund is an argument written
   backwards, and it is the failure this pack exists to keep apart.
8. `fs_write` `.aegis/artefacts/thesis-<date>-<subject>.md`: the claim, the
   conditions, the falsifier and its date, the cost of being wrong, who is on
   the other side, and the files you read. Then stop.

## How to validate

The claim is one sentence about the world. Every condition is checkable rather
than judged. There is a falsifier and it carries a date. There is no size, no
allocation, no expected return and no target. Nothing in the file names a goal
this would pay for.

## What to return

`skill_return` with `status: done`, the thesis in `artefacts`, what you read in
`evidence`, and a summary of at most five lines: the claim, the falsifier, and
what being wrong costs.

`status: needs_you` when writing the falsifier shows there is not one — when
nothing over any horizon would tell you the idea was wrong. Say so plainly. That
is the single most useful sentence this runbook can produce, and it is worth
more than the file it did not write.

`status: blocked` when nobody gave you an idea. Do not go and find one. A folder
of theses this ran up on its own is a folder that gets read as research.

## What requires approval

One `fs_write` inside the workspace. There is no tool here that trades, sells,
posts or transfers, and no later version of this runbook ends in one: execution
of money movement is always a person's (`PLAN.md` § 7.3), and trade sits on
§ 7.4's line with send, pay, merge, publish and deploy.

## What to do if the source is missing

If a file you were pointed at is not there, say so and write the thesis without
it, marking what is unsupported. An argument that admits its gaps is usable; one
that quietly fills them is the kind that survives right up until money moves.

**Never delete or rewrite a thesis that turned out wrong.** When a falsifier
fires, append the date and what happened to the file that predicted it. A folder
of theses whose losers were edited out is the most misleading artefact this
whole library could hold, and the ones that were wrong are the only reason to
keep any of them.
