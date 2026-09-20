---
version: 1
tools: fs_list, fs_read, fs_write
---

# social.scan

## When to use it

When an export of mentions or a timeline is on disk and somebody has to decide
which of it, if any, is worth answering. Most of it is not.

The export is a file you put there. Nothing here connects to an account, reads a
live feed, follows anybody, or posts.

## Inputs required and tools it will call

- The export, as a path — `.aegis/briefs/` unless you were given another.
- The criterion: what this house answers, written down. A line in
  `.aegis/decisions/DECISIONS.md`, a note from the human, a file of standing
  policy. If no file says what is worth answering, this runbook does not apply,
  and saying so is the run.

Calls `fs_list` and `fs_read` for those and `fs_write` for the list. It runs
nothing, reaches no account, and answers nobody.

## Steps

1. `fs_read` the criterion first, before the export, and quote it in the file
   you write. Reading the posts first and deciding afterwards is how the
   criterion becomes whatever the loudest post was about.
2. `fs_read` the export. Take each item once: a quoted or reposted item is the
   same item, and a thread is one item, not one per message.
3. Keep exactly two reasons to answer, and require the post to meet one of them:
   somebody asked a question this house can answer **from a file**, or somebody
   is relying on something of ours that is wrong in a way we can correct with a
   fact. Nothing else qualifies.
4. **Being wrong is not a reason.** Not a bad take, not a misreading of the
   field, not a claim you could refute. A post nobody addressed to us, that
   nothing of ours depends on, is not an item however answerable it looks — and
   it will look very answerable, because that is what the material is written
   for.
5. Cap the list at three. If more than three qualify, keep the three where an
   answer would be most useful to the person who wrote them, and say how many
   you dropped. A list of eleven is a list nobody works through.
6. For each item: the handle, the date, the quoted sentence that qualifies it,
   which of the two reasons it meets, and the file that would answer it. An item
   with no file behind the answer is not on the list — that is a question for
   the human, not a draft waiting to happen.
7. `fs_write` `.aegis/artefacts/social-scan-<date>.md`: the quoted criterion, the
   items, how many were considered, and how many were dropped. Quote one sentence
   per item, not the post — other people's writing does not need to be copied
   into a repository to be found by the link beside it.
8. Stop. Do not draft anything. That is `social.reply`, one item at a time, and
   somebody chooses which.

## How to validate

The criterion in the file is a quotation from a file, not a sentence written
during this run. Every item names one of the two reasons and the file that would
answer it. No item is there because the post is wrong. The list is at most three
and says how many were considered.

## What to return

`skill_return` with `status: done`, the list in `artefacts`, the export path in
`evidence`, and a summary of at most five lines: how many items were considered
and which few qualified.

**None qualifying is the ordinary answer**, and a complete one: `status: done`,
no artefact, one line saying how many were read and that none met the criterion.
A scan that finds something worth answering every time is a scan manufacturing
obligation, and a queue of drafts nobody asked for is how a person ends up
posting more than they meant to.

`status: needs_you`, and no item written, when a post is about this house and
hostile — an accusation, a pile-on, somebody angry. That is not a draft; it is a
person's decision about whether to answer at all, and it is the one place in this
pack where speed makes things worse.

## What requires approval

One `fs_write` inside the workspace. Nothing here posts, replies, follows,
likes, reports or blocks, and there is no tool in this build that could. A
connector installed later replaces where the export comes from, not what may be
done with it (`PLAN.md` § 7.6).

## What to do if the source is missing

No export, or no file naming the criterion: `status: blocked`, saying which. Do
not scan from what the session has heard about; do not infer the criterion from
the posts. A criterion derived from what is in front of you selects the loudest
thing in front of you.

A read you were refused, by the person or by the round limit that ends a turn,
means the scan covered part of the export. Say which part, and return
`status: needs_you` only if what you could not read is where a question about
this house would have been.
