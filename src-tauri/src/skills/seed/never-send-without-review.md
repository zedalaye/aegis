---
version: 1
tools: fs_read, fs_write
---

# never-send-without-review

## When to use it

Before anything leaves this machine: an email, a message, a post, a reply, a
commit somebody else will read. Run it on a draft that exists as a file, not
on an idea in the conversation.

## Inputs required and tools it will call

- The draft, as a path in the workspace. A path, never pasted text.
- Who it goes to and what it is meant to achieve — one line each.

Calls `fs_read` to read the draft and whatever it cites, and `fs_write` to put
the review beside it.

## Steps

1. `fs_read` the draft in full. If it cites a file, a ticket or a number, read
   that too rather than trusting the draft's account of it.
2. Check four things, in this order: is every factual claim supported by
   something you read; does it do what it was meant to do; is anything in it
   irreversible once sent; and is there anything in it the recipient should
   not see.
3. Write the review to `.aegis/artefacts/<draft name>.review.md`. Say what you
   checked, quote the lines you would change, and end with one of *send*,
   *change first* or *do not send*.
4. Stop. Sending is not a step, here or in any later version of this skill.

## How to validate

The review file exists and names the draft it is about. Every objection quotes
the line it is about. A verdict of *send* means you found nothing in step 2,
not that you found nothing worth saying.

## What to return

`skill_return` with `status: done`, the review file in `artefacts`, and the
verdict as the first line of `summary`. `status: needs_you` when the draft
turns on a decision only the human can make, with that decision in
`open_questions`.

## What requires approval

Writing the review is an ordinary `fs_write` and is put to the user like any
other. Sending is not part of this skill and has no tool in this build; when
one exists it stays behind the human gate, and this skill still stops at the
verdict.

## What to do if the source is missing

If the draft is not at the path you were given, return `status: blocked` with
the path you tried in `open_questions`. Do not reconstruct the draft from the
conversation and review that: a review of a draft nobody will send is worse
than no review, because it reads like one.
