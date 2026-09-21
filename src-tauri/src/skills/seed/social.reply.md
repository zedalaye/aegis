---
version: 2
tools: fs_read, fs_write
writes: .aegis/artefacts
---

# social.reply

## When to use it

When one post deserves an answer and a person will publish it. One run drafts
one reply to one post.

It does not publish, and there is no later version of it that does.

## Inputs required and tools it will call

- The item, as a path or as the line from a `social.scan` list. If you were not
  given one, that is the end of the run: a reply drafted to whichever post was
  most interesting is a reply to the wrong person, and it will read well.
- The file that answers it — the one the scan named. And a handful of this
  house's own previous posts, if any are on disk, to write in the voice that is
  already there rather than one invented today.

Calls `fs_read` for those and `fs_write` for the draft. It lists nothing, runs
nothing, reaches no account, and publishes nothing.

## Steps

1. `fs_read` the item and the file that answers it. Answer the question that was
   asked. Not the question behind it, not the better question, and not the four
   other things in the post you could have said something about.
2. Give the fact and stop. If the fact makes the other person's claim wrong, the
   fact is enough — a sentence explaining that they were wrong is a sentence
   about them rather than about the thing, and it is the sentence that gets
   quoted on its own.
3. Refuse these shapes, whatever the post did: opening with *actually*; the
   correction that is not needed to answer; the joke at somebody's expense; the
   rhetorical question; the reply that is really an announcement. Each of them
   performs better than the plain answer, which is exactly the problem.
4. Every claim carries a file behind it, and a claim with no file does not get
   softer wording — it comes out of the draft. "Should be fixed soon", "we're
   looking at it", "probably next release" are commitments published to
   everybody, and they will be quoted back with a date attached.
5. Write it short: one or two sentences, in the language the post was written
   in. An answer that needs three paragraphs is not a reply — it is either a
   post of its own or a message to one person, and the runbook for a message to
   one person is `reply.draft`.
6. Then read it as a stranger with none of the context, and read it again with
   the post it answers cropped off. If either reading is worse than the plain
   truth, rewrite it. A quotable sentence you did not mean to write is the
   characteristic failure of this artefact.
7. `fs_write` `.aegis/artefacts/social-reply-<handle>-<date>.md`: the item and
   its link or path, the quoted question, the draft, and a **sources** block —
   one line per claim, naming the file it came from.
8. Stop. Publishing is the human's, after `never-send-without-review`, which is
   what that runbook was seeded for. Do not publish, do not schedule, and do not
   tell anybody an answer is coming.

## How to validate

The draft answers the quoted question in its first sentence. Every claim in it
has a line in the sources block. There is no sentence about the other person,
only about the thing. It survives being read with the question cropped off. It
is short enough to be read whole without expanding it.

## What to return

`skill_return` with `status: done`, the draft in `artefacts`, the item and the
source files in `evidence`, and a summary of at most five lines: what was asked,
what the reply says, and what it commits to.

`status: needs_you` when the honest answer is one this house has not decided —
whether something will ship, whether a bug is a bug, what something will cost —
with it in `open_questions` and the draft left unwritten. A public guess is a
commitment with an audience.

And `status: needs_you`, always, when answering would mean disagreeing with a
named person in public. That is a choice about how this house wants to be seen,
it is not reversible by deleting the post, and it is not a choice a runbook
makes on somebody's behalf.

## What requires approval

One `fs_write` inside the workspace. There is no tool here that publishes, and a
connector installed later does not change it: publish is on the same line as
send, pay, merge and deploy (`PLAN.md` § 7.4), so it stays a human act. A reply
is a file until a person posts it.

## What to do if the source is missing

No item, no run: `status: blocked`, and ask which one. Do not pick from the scan
list yourself — which post this house answers is a decision, and it is made by
the person who has to live with the answer.

If the file that would answer it is not there, say so and write nothing: an
answer with no source is the one kind of reply that cannot be taken back and
cannot be defended. Return `status: needs_you` with what you would have needed.
