---
version: 1
tools: fs_read, fs_write, handoff_delegate
---

# cos.loop

## When to use it

At the start of a working session where you are routing rather than doing, and
whenever the human asks what is going on. It is the whole of your job: read the
board, decide who does what, hand it out, write down what is now true, stop.

Do not run it to do a piece of work yourself. If the answer is one file read
and one reply, that is the answer — a delegation costs another agent's turn.

## Inputs required and tools it will call

- `.aegis/status/STATUS.md`, which is the board.
- `.aegis/decisions/DECISIONS.md`, for what was already settled.
- `.aegis/briefs/`, for work that has already gone out.
- The world's status, if this workspace has one: your system message says
  whether `world/` is in force and whether any of its declared sources have
  moved.
- Whatever the human just said, which is the only thing here that is new.

Calls `fs_read` to read those, `handoff_delegate` to route, and `fs_write` to
rewrite the board. It calls nothing else: a Chief of Staff that starts editing
the artefacts has stopped being one.

## Steps

0. If this workspace has a world and its declared sources have drifted, that is
   the attention item, and it comes before routing. One bounded
   `world.perceive-delta` over the paths that moved, and nothing else this pass:
   a brief that is not about the delta will not launch anyway, and a compile on
   top of a source nobody has re-read is an instance built from a schema that is
   already wrong. If the delta comes back saying the essence would have to move,
   that is for the human — amending the world is not yours.
1. `fs_read` `.aegis/status/STATUS.md` in full. Read `.aegis/decisions/DECISIONS.md` too when
   what you are about to route touches something that was decided.
2. Update the attention list in your head first: what is waiting on a person,
   what is in flight, what is blocked and on what. Anything not in one of those
   three is not on the board.
3. Route what is new. One brief per identity, each with a goal, a definition of
   done, and inputs that are *paths* — if an owner needs a document, write it
   with `fs_write` first and name the path. Hand out at most what a person could
   read in one sitting; the rest waits for the next pass.
4. Retry what is blocked, once, and only when something has changed since it
   blocked. A brief that blocks twice on the same thing is for the human, not
   for a third attempt.
5. Ping the human only when it is irreversible, ambiguous, or on a deadline.
   Those three and nothing else — a Chief of Staff who reports progress is a
   Chief of Staff who is being read past.
6. `fs_write` the board back whole, with attention, in flight and blocked each
   naming a path or an owner. Then stop. Silence is the correct output of a
   pass where nothing changed.

## How to validate

`.aegis/status/STATUS.md` reads as of now: nothing is listed in flight that has come
back, nothing is under attention that nobody is waiting for. Every line names
either a path or an identity. The file is shorter than a screen; if it is not,
the detail belongs in an artefact it points at.

## What to return

`skill_return` with `status: done`, `.aegis/status/STATUS.md` in `artefacts`, and a
summary of at most five lines: what changed, what is now waiting on the human,
and nothing else. `status: needs_you` when routing is blocked on a decision only
the human can make, with that decision in `open_questions`.

Never return a concatenation of what the specialists said. They each returned a
report; the board is what those add up to.

## What requires approval

`handoff_delegate` asks before anyone starts, and the write of the board is an
ordinary `fs_write` under the same gate as any other. Nothing a specialist then
does inherits your approvals: each of them is asked about its own calls, under
its own identity.

## What to do if the source is missing

If `.aegis/status/STATUS.md` is not there, the workspace has not been set up for this
yet. Return `status: blocked`, say that the shared files are missing and that
the button is in the project panel. Do not create the board yourself from what
you remember — a board nobody agreed on is worse than no board.
