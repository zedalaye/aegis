---
version: 1
tools: fs_list, fs_read, fs_write, shell_exec
---

# deploy.draft

## When to use it

When something is ready to go to an environment and a person has to decide. The
output is a plan somebody reads and runs; this runbook never deploys.

## Inputs required and tools it will call

- Which revision, and which environment. Both by name — "the latest" is not a
  revision, and "prod" is not an environment unless that is the project's own
  word for it.
- Where the project says how it is deployed: `.aegis/skills/` if this workspace
  has its own deploy runbook, then the README, the compose file, the
  Dockerfile, the CI workflow. Those are the source. What you know about how
  applications like this are usually shipped is not.

Calls `fs_list` and `fs_read` to gather, `shell_exec` for read-only checks, and
`fs_write` for the plan.

## Steps

1. Read the project's own account first. A workspace runbook that says how
   *this* application ships beats everything else here, including this file.
2. Pin the revision. `git log -1 <revision>` so the plan names a commit that
   exists, and say what is in it that is not in what is running.
3. List what the deploy changes beyond code: migrations, environment variables,
   a queue that must drain, a cache to clear, a job to stop first. Name the
   variables. Never read a value into the plan.
4. `fs_write` `.aegis/artefacts/deploy-<environment>-<short revision>.md`: the
   revision, the environment, and the commands in the order a person runs them,
   one per line. Beside each one that changes data, how it is undone. A step
   with no way back is marked as one, in words, on its own line.
5. Say what "it worked" looks like: the check to run afterwards and what it
   should say. A plan with no answer to that is a plan nobody can stop halfway
   through.
6. Stop. Do not run the plan — not even its first read-only step, to be sure.
   The person who decides to deploy is the person who runs it.

## How to validate

The plan names one revision and one environment. Every command is copy-pastable
as written, with no placeholder the reader has to guess at. Every step that
changes data carries a rollback line or is marked irreversible. No secret value
appears anywhere in the file — only names.

## What to return

`skill_return` with `status: done`, the plan in `artefacts`, the revision in
`evidence`, and a summary of at most five lines: what ships, where, and which
steps cannot be undone. `status: needs_you` when the plan cannot be written
without a decision — a migration that drops data, a window that costs users —
with that decision in `open_questions`.

## What requires approval

Reads inside the workspace happen without asking. Each `shell_exec` is put to
the user with its arguments; keep them read-only, and count a build or a test
command as a write — it touches the tree you are describing. Deploying is not a
step in this runbook and there is no version of it in which it is. When a host's
connector exists it will replace where these facts come from, not who presses
go.

## What to do if the source is missing

If the project does not say how it is deployed, return `status: blocked`, name
the files you looked in, and ask for the one that is missing. Do not draft from
the framework's defaults: a plausible deploy for an application that is shipped
some other way is the most expensive artefact in this pack.

A refusal is a missing source: a read denied by the person, or by the round
limit that ends a turn, leaves a plan resting on a file you never opened. Say
which one and return `status: needs_you`. If `.aegis/artefacts/` is not there,
write the plan with its directory created and say the shared files are missing.
