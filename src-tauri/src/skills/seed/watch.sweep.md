---
version: 2
tools: fs_list, fs_read, fs_write
writes: .aegis/artefacts
---

# watch.sweep

## When to use it

When material for the watch has arrived as files and nothing has read it yet.
One run turns what is new in one folder into one entry per item.

The material is whatever you put there: a saved page, a release note, a paper,
an export, a note somebody passed on. There is no tool here that fetches
anything, and nothing in this run reaches the network.

## Inputs required and tools it will call

- The folder the material is in — `.aegis/briefs/` unless you were given
  another. `fs_list` it first, so the sweep covers what is on disk rather than
  what was mentioned.
- Nothing else. What has already been swept is answered by the entry names in
  `.aegis/artefacts/`, not by anybody's memory of last time.

Calls `fs_list` for both folders, `fs_read` for the material, and `fs_write`
for one entry per new item. Nothing here runs a command and nothing here
fetches.

## Steps

1. `fs_list` `.aegis/artefacts/` and read the names. An entry is
   `watch-<source>.md`, where `<source>` is the material's file name without its
   extension. A source already named there has been swept: skip it without
   reading it. That is the whole of the bookkeeping, and it is names rather than
   a ledger so that deleting an entry is how you ask for an item to be read
   again. Where the name is taken by a different file, put the folder in it too.
2. `fs_list` the material folder and take what is left. If nothing is left, the
   sweep found nothing — write no entries and go to *What to return*. That is
   the answer most days.
3. For each new item, `fs_read` it and write down two things separately: **what
   it says** and **what it shows**. A claim is what the source asserts — faster,
   cheaper, the first, the only. Evidence is what somebody else could go and
   check: a number with its method beside it, a repository, a licence, a price,
   a date, a name. Most announcements are all claim. Say so; that is not a
   criticism of the item, it is the fact the entry exists to carry.
4. Quote the claim rather than restating it, and take the date from the source.
   A date inferred from where the file sits is not a date — write *undated*
   instead. An undated source is worth less than a dated one, and a reader of
   the entry should be able to see that.
5. Say who published it and what they sell. A benchmark in a launch post, a
   forecast from somebody holding the position, a study funded by the thing it
   measures: none of that is disqualifying, and all of it belongs in the entry.
6. `fs_write` `.aegis/artefacts/watch-<source>.md` for each: the source's path,
   its date or *undated*, who published it, the quoted claim, what is shown
   behind it, and one line on what it would touch here — a file, a dependency,
   a cost, or nothing. One entry per item, however tempting a combined one is:
   two items in one file is one item that cannot be skipped later.
7. Stop. Do not work out whether any of it matters. That is `watch.impact`, on
   one entry, and it needs this file written first.

## How to validate

Every entry names the file it came from, and nothing else in it claims to be a
source. Every claim is a quotation. No entry carries a date its source did not.
An entry whose source shows nothing says so in those words, rather than
paraphrasing the claim into something that sounds checked.

## What to return

`skill_return` with `status: done`, the entries in `artefacts`, the material's
paths in `evidence`, and a summary of at most five lines: how many items were
new, what they were, and which of them showed anything.

*Nothing new* is a complete run: `status: done`, no artefacts, one line naming
the folder you listed and what was already swept. A sweep that finds something
every time is a sweep reading the same page twice.

`status: needs_you` when a source is somebody's private material — a client's
document, a contract, a message — dropped in the watch folder by mistake. Name
the file, do not enter it, and do not summarize it in the return either.

## What requires approval

The writes are ordinary `fs_write` calls. Reads inside the workspace happen
without asking, and a folder outside it is a path the person is asked about
every time — which is why the material belongs in the workspace before a sweep,
and certainly before one on a clock. Nothing here fetches, downloads,
subscribes, or answers anybody.

## What to do if the source is missing

If the material folder is not there, or holds nothing at all, return
`status: blocked` and say which folder you listed. Do not sweep from what the
session says has been happening: a watch whose entries came out of a
conversation is a watch reporting its own memory back to you.

A read you were refused — by the person, or by the round limit that ends a turn
— leaves that item unswept, and unswept is where it should stay. Write the
entries you could, name the ones you could not in `open_questions`, and return
`status: needs_you`. Half an entry carrying the source's own headline is worse
than no entry.
