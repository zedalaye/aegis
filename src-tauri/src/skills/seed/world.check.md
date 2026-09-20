---
version: 1
tools: fs_list, fs_read
---

# world.check

## When to use it

At the start of any session about to work on a world that did not open as a
Chief of Staff pass: before writing an instance, before answering a question
about what the project *is*, and whenever you are unsure whether what you are
about to do is an écart.

## Inputs required and tools it will call

- Nothing. It reads `world/`.

Calls `fs_list` and `fs_read`, and nothing else. It writes nothing, here or in
any later version of it.

## Steps

1. `fs_list` `world/`. Note which of `essence.md`, `schema.md`, `behaviours.md`,
   `oracle.md` and `decisions.md` are there. A missing one is a fact about this
   world, not an error.
2. `fs_read` the essence, then the schema, then the behaviours. Read the sins
   with their perimeters: a behaviour that binds one contract is not a rule
   about everything.
3. `fs_read` `world/oracle.md` if there is one. That is what any instance will
   be judged against, and it is worth knowing before writing one rather than
   after.
4. Check what you were asked to do against what you have just read. If doing it
   would change the essence, stop: that is an écart, and it is the answer.
5. Do not read what `world/sources.yml` declares. Those have been perceived, and
   a read of one that has not changed is refused.

## How to validate

You can say, in three lines and without opening the files again, what this thing
is, what it is checked against, and which recorded behaviour is closest to what
you were asked to do.

## What to return

`skill_return` with `status: done` and a summary of at most five lines: what the
world says this is, what the oracle checks, and whether the work in front of you
is inside the essence or an écart. `status: needs_you` when it is an écart, with
the line of the essence that would have to move in `open_questions`.

## What requires approval

Nothing. Every read is inside the workspace and happens without asking. There is
no write step, and this is not the place to add one: amending the world is a
human decision.

## What to do if the source is missing

If there is no `world/`, this workspace has no constitution and this runbook
does not apply. Return `status: blocked`, say so in one line, and get on with
the work under the cabinet's own rules.
