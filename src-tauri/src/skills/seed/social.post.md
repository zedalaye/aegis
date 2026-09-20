---
version: 1
tools: fs_list, fs_read, fs_write
---

# social.post

## When to use it

When something has happened here that is worth saying in public — a release, a
write-up, a result. One run drafts one post from the files behind it.

It does not publish. Nothing in this build can.

## Inputs required and tools it will call

- What happened, and where it is on disk: a changelog entry, a tag, a merged
  change, a file that exists now and did not before. The post is written from
  that, not from a description of it.
- This house's previous posts, if any are on disk, so the draft sounds like
  whoever writes here rather than like a launch.

Calls `fs_list` and `fs_read` for those, and `fs_write` for the draft. It runs
nothing, reaches no account, and publishes nothing.

## Steps

1. `fs_read` what happened, in the files. If the thing being announced cannot be
   pointed at — a version that is not tagged, a feature not merged, a page not
   published — there is no post to write yet, and that is the answer.
2. Say what it is, in the first sentence, to somebody who has never heard of
   this project. No hook, no thread marker, no question the post then answers,
   no "we've been quiet lately". Those shapes are there to buy attention and
   they are the first thing that ages badly.
3. Only the past tense about this house. What shipped, what changed, what was
   measured. **No dates for anything that has not happened** — "next week",
   "soon", "in the coming months" are commitments published to everybody, and
   nobody will remember they were an aside.
4. Every number, name and claim comes from a file, and the file goes in the
   sources block. A benchmark needs its method beside it or it does not go in.
   This house is subject to the same rule `watch.sweep` applies to everybody
   else's announcements, and it is the same rule.
5. Do not compare with somebody else's product by name. A comparison is a claim
   about a thing you did not measure and cannot correct, made to an audience
   that includes them.
6. Read every sentence once, alone, as though it were the only one quoted. Then
   read the whole thing as somebody who dislikes this project. Rewrite anything
   that is worse under either reading — not to soften it, but because a sentence
   that only works in context will be read out of it.
7. `fs_write` `.aegis/artefacts/social-post-<date>-<subject>.md`: the draft, then
   a **sources** block naming the file behind every claim, then one line saying
   what in the draft is not yet true anywhere. That last line should be empty.
8. Stop. Publishing is the human's, after `never-send-without-review`. Do not
   publish, do not schedule it, do not write the follow-up, and do not draft the
   replies to it.

## How to validate

Every claim has a file in the sources block. Nothing in the post is in the future
tense about this house. No competitor is named. The first sentence makes sense to
somebody with no context. The line about what is not yet true is empty.

## What to return

`skill_return` with `status: done`, the draft in `artefacts`, the files behind it
in `evidence`, and a summary of at most five lines: what it announces and what it
claims.

`status: blocked` when the thing is not on disk yet. A post about something that
is about to be true is the most expensive artefact this pack can produce, because
it is the one that cannot be corrected and the one people screenshot. Say what
would have to exist, and stop.

`status: needs_you` when the post would be the first public word on something —
a price, a licence, a partnership, a person leaving. Those are announcements
before they are posts, and what they say is a decision somebody takes rather than
a draft somebody edits.

## What requires approval

One `fs_write` inside the workspace. Publish is on PLAN 7.4's line with send,
pay, merge, deploy and trade, so it stays a human act however the draft reads;
Aegis has no tool that posts, and a connector installed later would be one more
call put to a person, every time.

## What to do if the source is missing

If what you were asked to announce is not on disk, return `status: blocked` and
name what you looked for. Do not write it from the session's account of what
shipped — the session is where "it's basically done" lives, and this is the one
artefact where that sentence becomes public.

A read you were refused, by the person or by the round limit that ends a turn,
is a claim with no source: leave it out of the draft rather than out of the
sources block, and say so in the summary.
