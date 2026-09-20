---
version: 1
tools: fs_list, fs_read, fs_write
---

# watch.digest

## When to use it

When the watch is due to report: a week, a morning, a clock. It reads the
entries written since the last digest and reports only what that one did not.

This is the runbook of the pack that belongs on a clock, and the only one here
written for a reader who was not in the room.

## Inputs required and tools it will call

- Nothing. `fs_list` `.aegis/artefacts/` and the run has what it needs: the
  entries `watch.sweep` wrote, and the digests written before this one.
- The period, if you were given one. Without one the period is *since the last
  digest*, which is what that digest's own closing list answers.

Calls `fs_list` and `fs_read` in `.aegis/artefacts/`, and `fs_write` for the
digest — when there is one to write.

## Steps

1. `fs_list` `.aegis/artefacts/`. Digests are `watch-digest-<date>.md` and
   entries are `watch-<source>.md`. `fs_read` the newest digest **first** and
   read the list of entry names at the end of it. That list is where the watch
   got to, and it is the only thing that keeps this run cheap.
2. Take the entries that list does not name. Those are the run. Do not re-read
   the ones it does, and do not open the sources any of them came from: an entry
   is what its source was read into, and reading both is paying twice for one
   item.
3. If nothing is left, write no file. *Nothing new since <date>* is the answer,
   and it is the answer most of the time. One digest per empty week is a folder
   nobody opens and a watch nobody believes.
4. Sort what is left into **changed**, **worth reading** and **noise**, each item
   in exactly one. *Changed* is something now true that was not — a version
   shipped, a price moved, a licence changed, a company bought. *Worth reading*
   is an argument somebody here would be better for having read. *Noise* is the
   rest, one line each, because "eleven of these arrived" is information and
   eleven summaries of them are not.
5. Cap the first two lists at five items each. Past five, the sixth is noise by
   the definition above, whatever it is about. A digest that lists everything has
   handed the sorting back to the reader, which was the work.
6. Every line names the entry it came from — the entry, not the source, because
   the entry names the source and carries what was actually shown. A digest
   citing a page directly is a digest whose claims cannot be checked without
   leaving the folder.
7. `fs_write` `.aegis/artefacts/watch-digest-<date>.md`: the period covered, the
   three lists, and — last — every entry name this digest looked at, including
   the ones it filed as noise. That closing list is what the next run reads
   first, so an entry left out of it is an entry reported twice.
8. Stop. A digest reports; it decides nothing. An item in it that looks like it
   changes what this project does is one run of `watch.impact` on that entry,
   started by somebody.

## How to validate

Every line of the digest names an entry file. No entry is in two lists. Nothing
in it appeared in the previous digest. The closing list holds every entry the
run looked at, so its length is the number of entries considered. The digest
fits on a screen.

## What to return

`skill_return` with `status: done`, the digest in `artefacts`, the entry names
in `evidence`, and a summary of at most five lines: the period, what changed,
and how many items were noise.

An empty period is also `status: done`: no artefact, and one line saying nothing
is new since the last digest and which digest that was. A quiet week is this
runbook working, not failing — `blocked` would put a routine two silences from
pausing itself over it.

`status: needs_you` for one thing only: an entry saying something has happened
to something this project currently relies on — a dependency abandoned, a
licence changed under it, a service closing. That is not a line to be read on
Friday.

## What requires approval

One `fs_write` inside the workspace, and nothing else. That matters more here
than in the rest of the pack, because this is the runbook meant to fire
unattended, and an unattended run is never asked anything: what it may do beyond
reading is exactly what was signed on the routine, and everything else is
refused rather than put to somebody. A version of this that wanted a folder
outside the workspace, or a command, would be a routine that failed every
morning at the same time.

## What to do if the source is missing

If `.aegis/artefacts/` holds no entries at all, return `status: blocked` and say
so: there is nothing to digest, and the thing to run is `watch.sweep`. If it
holds entries and no digest, this is the first one — cover everything, and say
that in its first line.

If some entries could not be read — a refusal, the round limit that ends a turn
— the digest covers the ones you read, says which it could not in its first
line, and leaves those out of the closing list so the next run picks them up.
Return `status: needs_you`.
