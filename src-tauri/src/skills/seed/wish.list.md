---
version: 1
tools: fs_list, fs_read, fs_write
---

# wish.list

## When to use it

When somebody has said what they want and it should be written where it can be
seen. One run updates the goals file and judges nothing on it.

The goals are theirs: a car, a holiday, paying for a model subscription, a
machine, time off. What they are for is not this runbook's business.

## Inputs required and tools it will call

- The goals file, if there is one — `.aegis/artefacts/goals.md` unless you were
  given another path. `fs_list` first: an existing list is edited, never
  rewritten from what the conversation remembers of it.
- What was said this time, as the person said it.

Calls `fs_list`, `fs_read` and `fs_write`. It runs nothing, buys nothing, and
prices nothing by going and looking.

## Steps

1. `fs_read` the existing list before writing a word. What is already on it
   stays on it, in the words it is written in, unless the person said to change
   that entry.
2. Add only what was actually said. Not the thing that would obviously go with
   it, not the cheaper version, not the prerequisite you can see. A list nobody
   recognises as their own is a list they stop reading.
3. Take the ordering from the person, and record it as theirs. If they have not
   said what comes first, write *unordered* at the top and leave the entries in
   the order they arrived. An ordering invented here would be read next month as
   one they chose.
4. Price each entry only from something you were given or can read: a quote, an
   invoice, a page they saved, a figure they said. Anything else is **not
   priced**, in those words. Never an estimate — an estimate is what the funding
   pipeline will divide by, and by then nobody remembers it was made up.
5. Keep the date only if there was one. "Before the summer" is a date somebody
   said and goes in as that phrase; a month and a year you resolved it to is
   not.
6. Say what each entry is waiting on, in one line, when the person said: money,
   a decision, somebody else, nothing.
7. `fs_write` the list: the ordering and whose it is, then one entry per goal —
   what it is, what it costs or *not priced*, the date phrase or none, and what
   it waits on. Keep entries somebody has met, marked as met and dated, at the
   bottom; a list that only ever grows is a list of failures.
8. Stop. Do not propose how to pay for any of it, do not rank by what is
   achievable, and do not suggest dropping anything. How this gets funded is
   `revenue.pipeline`, and what to give up is nobody's call here.

## How to validate

Every entry is something the person said, in words they would recognise. The
ordering is attributed to them or the list says *unordered*. Every price names
where it came from, and everything else says *not priced*. No entry carries a
date nobody uttered. Nothing in the file evaluates whether a goal is a good
idea.

## What to return

`skill_return` with `status: done`, the list in `artefacts`, and a summary of at
most five lines: what was added or changed, and what is still not priced.

`status: needs_you` when two entries conflict — the same money twice, two dates
that cannot both hold — with both quoted and no resolution proposed. Which one
gives way is the whole of what a wish list is for deciding, and it is not a
tie-break a runbook performs.

There is no `blocked` for an empty list. A first run with no file writes the
first version, and a person with one goal has a goals file.

## What requires approval

One `fs_write` inside the workspace. Nothing here spends, orders, subscribes or
books, and there is no tool in this build that could. A goal is a file the
person can open, edit and delete in their own folder — deliberately not
something an identity remembers, which nobody else could read and which would
vanish with the identity.

## What to do if the source is missing

If the path you were given is not there and you were told to update rather than
create, return `status: blocked` and say which path you looked at. Do not
reconstruct somebody's goals from the conversation: a list rebuilt from memory
quietly loses the entries nobody has mentioned lately, which are usually the
ones that mattered longest.

A read you were refused, by the person or by the round limit that ends a turn,
means you have part of the list. Write nothing and return `status: needs_you`:
a partial list written whole is a list with entries silently deleted.
