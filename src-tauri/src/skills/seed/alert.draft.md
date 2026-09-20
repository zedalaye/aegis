---
version: 1
tools: fs_list, fs_read, fs_write, shell_exec
---

# alert.draft

## When to use it

When monitoring has fired, or a client says something is broken. It turns the
signal into an incident note and a reply the human may send.

It does not fix anything, and it does not answer anybody.

## Inputs required and tools it will call

- The alert, as a path: the exported alert, the log excerpt, the message
  somebody dropped in `.aegis/briefs/`. If you were given no path, the newest
  unhandled file there.
- Which system it is about, if the alert does not say.

Calls `fs_list` and `fs_read` for the alert and what it points at, `shell_exec`
for read-only checks, and `fs_write` for the two drafts.

## Steps

1. `fs_read` the alert whole, including the parts that repeat. When it started,
   how often it has fired, and what it actually measures are the three facts a
   reply stands on.
2. Establish what is true now, with read-only commands: the service's own health
   output, the last lines of a log, `git log -1` on what is deployed. **Four
   commands, and the fourth is the last.** A count rather than "enough", because
   an alert that names something broken is an invitation to go and fix it, and
   refusing that invitation is most of this runbook's job. If four have not
   established the cause, *that is the finding*: write the note with the cause
   marked unestablished and propose the diagnosis as the next action instead of
   starting it. A run that spends twenty commands has stopped triaging and is
   debugging under another name, unwatched, with nobody expecting it.
3. Keep what you observed and what you infer apart, and keep them apart for the
   rest of the run. Every line of the note is one or the other and says which.
4. `fs_write` `.aegis/artefacts/incident-<date>-<system>.md`: when it started,
   what is affected, what is *not* affected, what is true right now, the
   likeliest cause with how sure you are, and the next action you would take.
   Cite the command or the file behind every observed line.
5. `fs_write` the reply beside it, named for the same incident with `.reply`
   before the extension: what happened, what it means for them, what is being
   done, and when they will hear next. It promises nothing the note does not
   support, and it names no cause the note marked as inferred.
6. Stop. Restarting a service, scaling something, clearing a queue or rolling
   back are not steps here — they are the decision this note exists to inform.
   Sending the reply is the human's, after `never-send-without-review`.

## How to validate

Every observed line in the note cites a command or a file, with the time it came
from, and the note cites **at most four commands** — if it cites more, this was
not a triage. The reply contains no claim the note does not carry. The note says
what is *unaffected*: a report that lists only damage cannot be used to decide
anything.

## What to return

`skill_return` with `status: done`, both files in `artefacts`, the checks you
ran in `evidence`, and a summary of at most five lines: what is affected, what
is not, and the next action. `status: needs_you` when that action is
irreversible or reaches users — which is most of the interesting ones — with it
in `open_questions`.

## What requires approval

Both writes are ordinary `fs_write` calls. Every check is a `shell_exec` put to
the user with its arguments; keep them read-only — a build or a test command
writes, and during an incident it competes with the thing you are diagnosing —
and remember that the "harmless" restart is the one command that destroys the
evidence for what caused it. Nothing here sends: the reply is a file until a
person sends it.

## What to do if the source is missing

If there is no alert at the path you were given and nothing unhandled in
`.aegis/briefs/`, return `status: blocked` and say which path you looked at. Do
not write an incident note from a dashboard you cannot see, and do not
reconstruct the alert from what the conversation said it probably was.

A check you were refused — by the person, or by the round limit that ends a turn
— is not an observation and does not become one. Leave it out of the note, say
in `summary` what you could not check, and return `status: needs_you` if the
next action turns on it. If `.aegis/artefacts/` is not there, write both files
with their directory created and say the shared files are missing.
