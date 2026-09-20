---
version: 1
tools: fs_read, fs_write, shell_exec
---

# world.verify

## When to use it

When an instance has been produced and somebody has to know whether it is right:
after a compile brief comes back, before anything built on it is used, and
whenever the oracle itself has changed.

Not a code review, and not a judgement about the shape of a diff. The question
is one question — does this satisfy `world/oracle.md` — and it has an answer.

## Inputs required and tools it will call

- `world/oracle.md`, which is the criterion.
- The instance, as paths.
- Whatever the oracle names as the way it is checked: a command, a scenario
  file, a characterisation.

Calls `fs_read` for the oracle and the instance, `shell_exec` to run whatever
the oracle says the check is, and `fs_write` for the verdict.

## Steps

1. `fs_read` `world/oracle.md`. Turn it into a list of clauses, each of which is
   either satisfied or not. A clause you cannot decide is a defect in the
   oracle — record it as one rather than deciding it by feel.
2. Run the checks. Where the oracle names a command, `shell_exec` it and keep
   the output. Where it names a scenario, run the scenario. Do not substitute
   reading the code for running the check.
3. Write `.aegis/artefacts/<instance>.verified.md`: one line per clause with *pass*,
   *fail* or *undecidable*, each naming the path or the command output that says
   so. A pass with no evidence is a fail.
4. If a clause fails and nothing in `world/` changed, say so plainly: the
   instance is wrong, not the world. Regenerating it is legal and cheap; arguing
   with the oracle is not yours to do.

## How to validate

Every clause of the oracle appears exactly once in the verdict. Every verdict
line names its evidence as a path or as command output. The file says *pass*
only if every clause did.

## What to return

`skill_return` with `status: done`, the verdict file in `artefacts`, and the
overall pass or fail as the first line of `summary`. `status: needs_you` when a
clause is undecidable as written — that is the oracle needing an amendment,
which is the human's.

## What requires approval

Every command is an ordinary `shell_exec` and is put to the user with its
arguments and working directory. Nothing here writes `world/`: the oracle is
read, never edited.

## What to do if the source is missing

If there is no `world/oracle.md`, return `status: blocked` and say that this
world has no oracle yet. Do not invent one from the instance — an oracle derived
from the thing it is meant to judge says nothing.
