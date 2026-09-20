---
version: 1
tools: fs_read, fs_write
---

# world.perceive-delta

## When to use it

Only when a declared source of this world has moved: a new dump, a log that
grew, an export regenerated. The frame in your system message names the paths,
and so does the project panel. One run covers the paths you were given and
nothing else.

Do not run it to "understand the project". The project is in `world/`, it was
perceived once, and reading the whole dump again is the round-trip this world
exists to have paid once.

## Inputs required and tools it will call

- The paths whose hash moved, as paths. If you were not given any, this runbook
  does not apply.
- `world/essence.md` and `world/schema.md`, so the delta is read against what is
  already known rather than from nothing.

Calls `fs_read` for those, and `fs_write` for the one file it produces.

## Steps

1. `fs_read` the world first — essence, then schema, then behaviours if there is
   one. What is in there is given.
2. `fs_read` the moved sources. Read the *delta*: what is in them that the world
   does not already account for, and what in the world they now contradict. Skip
   whatever only confirms what you have already read.
3. Write `.aegis/artefacts/world-delta-<date>.md`: one section per moved path, and in
   each, what is new, what is contradicted, and — for anything that looks like
   an invariant — the perimeter it holds inside (this contract, this role, this
   class of tasks). A local failure is not an invariant; say so when that is
   what you found.
4. End the file with the amendment you are proposing, written as the lines that
   would change in `world/`, and the `bytes` and `sha256` each moved source
   should now be recorded at in `world/sources.yml`.
5. Stop. Do not write `world/`.

## How to validate

Every claim in the file names the path and the place in it that supports it.
Nothing in it restates what `world/essence.md` already says. Every proposed
invariant carries a perimeter.

## What to return

`skill_return` with `status: done`, the delta file in `artefacts`, and a summary
of at most five lines: which sources moved, what changed in the world's own
terms, and whether the essence would have to move. `status: needs_you` when it
would — that is an écart, and it belongs to the human.

## What requires approval

The write of the delta file is an ordinary `fs_write` under the usual gate. A
write into `world/` is refused outright, whatever this file proposes: amending
the constitution is a human decision, taken by whoever owns the world.

## What to do if the source is missing

If a path you were given is not there, say so and stop: a declared source that
has vanished is an attention item for the human, not something to reconstruct.
Return `status: blocked` with the path in `open_questions`.
