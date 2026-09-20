# Handoffs, routines and the board

## Handoffs

One identity can hand work to others and wait for what they return. That is the Chef-de-Cabinet
mode: **a Chief of Staff routes, specialists do the work, and the human decides anything
irreversible.**

What goes out is a brief:

```
goal:                 Draft the release note for 0.4
owner:                Scribe
priority:             normal
inputs:
  - .aegis/artefacts/changelog.md
constraints:
  - no marketing language
definition_of_done:   .aegis/artefacts/release-0.4.md exists and names every user-visible change
approval_needed:      the write
return_format:        artefact
```

What comes back is a report with the same six fields as a [skill return](skills.md#the-return):
`status`, `summary`, `artefacts`, `evidence`, `open_questions`, `next_owner`.

- **Inputs are paths, never paste.** An input that spans several lines is refused: write the text to
  a file and name the path.
- `handoff_delegate` **asks**, naming every owner and goal. A session grant covers the routing only.
- **Each brief opens its own session** under the owner's identity. Briefs run in parallel and appear
  in the sidebar with a `brief` badge.
- A specialist works under **its own** allow-list and holds none of the delegating session's grants.
  When it wants to write, it asks in *its* session; the sidebar row shows *waiting on you*. It cannot
  delegate further.
- With shared files set up, each brief is also written to `.aegis/briefs/`.
- **A board of statuses comes back**, never a transcript:

  ```
  2 briefs: 1 done, 1 blocked; review done

  --- brief 1 — Draft the release note for 0.4 (Scribe)
  brief: .aegis/briefs/3f2a91b8-draft-the-release-note.md
  status: done
  …
  ```

- **When nobody answers.** Each attempt is bounded to five minutes, then cancelled like Stop. A run
  that ends without returning gets one more attempt in the same session; after that the board says
  `needs_you`. The other briefs still count.
- A reviewer brief can run after the others, handed their artefact paths.
- Every audit line a specialist writes carries the delegation id, so one run covers the Chief and
  every specialist.

`cos.loop` is the Chief-of-Staff loop as a runbook in your library: read the board, update the
attention list, route, retry once, ping only when something is irreversible, ambiguous or on a
deadline, write the status, stop. No identity runs it until you grant it.

**Setting it up:** make a **Chief** (`fs_read`, `fs_write`, `handoff_delegate`, and the `cos.loop`
skill) and a narrow specialist such as a **Scribe** (`fs_read`, `fs_write`); set up shared files; open
a session as the Chief. With the scripted provider, `/delegate Scribe` walks the whole path without a
model.

## Routines

A routine fires **one runbook, as one identity, on a schedule**, whether or not the window is open.

```
name:       Morning watch
identity:   Watcher
runbook:    watch.digest
when:       daily at 07:00        (or every N minutes, or when a folder changes)
signed for: write files inside the workspace
budget:     4 runs a day
```

There is no message field: a routine names a runbook, never a prompt.

**The door.** A routine may only name a skill that is:

1. **live** — a `SKILL.md` that parses, not a proposal;
2. **granted** to that identity;
3. **already run under watch by that identity at least once**, checked against `audit.jsonl`.

Write the runbook, run it once and watch, then put it on a clock.

**Nobody is watching**, so a scheduled run never asks: it *parks* instead. What it may do beyond
reading is the list of standing approvals you ticked when saving — the same grants the dialog
creates, checked at save time against what the runbook declares and what the identity holds. Nothing
outside the workspace, under `.git/` or in `world/` can be signed for. A scheduled run cannot
delegate. **Run now** takes exactly the unattended path.

- **Budgets.** Each routine and each identity has a daily ceiling, spent before a run opens its
  session, under the store's lock.
- **Silence.** A run that ends without returning is a silence. Two in a row pause the routine, with
  the reason on its row. `blocked` and `parked` are answers, not silences.
- **Parked.** A call the routine was not signed for does not run and is not thrown away: it is kept
  on the board with everything a dialog would have shown, the run is told to stop and say what it
  needed, and its row reads *parked*. A run may park three calls, and a question nobody answers for
  a week closes itself as `blocked`. The same call parked by a later run of the same routine is the
  same question, not a second row.
- **Bounds.** A run is cut off after 15 minutes. At most two scheduled runs are in flight.
- **Trace.** Each run is an ordinary session with a `routine` badge, and every audit line it writes
  carries the routine id.
- **Folder trigger.** *When a folder changes* compares the newest modification time under one
  workspace directory (the directories' own stamps included) on each tick, starting from where it
  stood when the routine was saved, at most every five minutes.
- An identity that routines fire as cannot be deleted. *Duplicate* clones a role without its
  memories.

## The board

**Board** in the title bar answers, for the open project: *who ran, what did it cost, and why did it
fail.*

| Column | Holds |
| --- | --- |
| **Attention** | someone must act: an approval on screen, a parked call, a run that returned `needs_you`, a routine that gave up |
| **In flight** | running now |
| **Blocked** | stopped short and not waiting on a person: a missing source, a routine that cannot fire, a failed run |

Each line says where it came from: **your `.aegis/status/STATUS.md`**, read structurally (the three
headings and the lines under them; indented examples and whole-line italics are ignored), or **what
the runtime sees** (running turns, pending dialogs, parked calls, paused routines). Nothing on the
board writes `STATUS.md`: correct it in your editor or through an approved write.

**Parked** sits under the columns: one card per question, reading like the approval prompt because
it is the same question asked later. *Allow once* runs that exact call, with the arguments in front
of you, and nothing else — a run that comes back with different arguments asks again. *Allow
standing* signs the approval onto the routine, through the same checks as saving one. *Deny*
records the refusal. All three pick the run up in the session it stopped in, so what it had already
done is not repeated. Aegis can also notify you while the window is hidden: the routine's name and
one sentence, never the path or the command line, and the click opens the window rather than
answering anything.

**Runs** are folded from the tail of `audit.jsonl`, using the widest id on each line:

| A run is | Grouped by |
| --- | --- |
| a delegation | the handoff id — the Chief's call and every specialist's |
| a routine firing | the routine, in the session it opened |
| a runbook | the skill name, in its session |
| a conversation | the session |

- A report wins over a refusal inside it: a runbook denied a write that still returned `done` is
  done.
- A conversation cannot fail: a denial in a chat is your answer.
- A brief, runbook or firing that never reported has failed — unless its session is still running.

Opening a run shows its audit lines, oldest first, and the artefact paths it named.

**Cost** is tokens per turn as the provider reports them, joined to runs by turn id. Aegis asks for
usage (`stream_options.include_usage`); a turn whose server sent none is *unknown*, and a total
including it reads *at least*. Every turn belongs to exactly one run, so the runs add up to the
session. The same figure sits beside the model in the chat header.

The board is a view of the log: runs older than the tail it reads, and sessions you deleted, remain
only in `audit.jsonl`.
