---
version: 1
tools: fs_list, fs_read, fs_write
---

# cabinet.found

## When to use it

When someone asks for a cabinet in this project — a Chief of Staff, a reviewer,
the specialists the work needs — and the team does not exist yet or lacks a
role. You write one proposal file and create nobody: a person applies it in
Settings, and applying it is the grant.

Do not run it to change an identity that already exists. Apply skips every name
already on file and never widens one; a wider Reviewer is an edit a person makes
by hand.

## Inputs required and tools it will call

- What the human said in this conversation about three things: which domains
  this cabinet is for (delivery, intake, watch, budget, social, revenue and the
  wish list, or none beyond routing); whether this project has a world, an
  essence written down in `world/`; and whether anything should ever run while
  nobody is watching.
- `.aegis/`, which must already exist. The shared files are laid down by a
  button in the project panel, not by you.
- `.aegis/roster/PROPOSAL.md`, when a roster was proposed before.

Calls `fs_list` to see what the cabinet holds, `fs_read` to read an earlier
proposal, and `fs_write` once, for the proposal. Nothing else: no tool creates
an identity, a routine or a connector, and this runbook does not look for one.

## Steps

1. If the human has not answered all three questions above, ask them and stop.
   Do not infer a domain from the folder: a `package.json` is not a request for
   a delivery team.
2. `fs_list` `.aegis/`. If it is not there, stop — see the last heading.
3. If `.aegis/roster/PROPOSAL.md` exists, `fs_read` it and start from it. Keep
   what it proposes and add only what the answers call for.
4. Propose the two identities every cabinet gets, as in the example below, then
   one specialist per domain the human named, and no other. The human is not a
   row. The built-in Assistant is not a row either; apply never touches it.
5. The Chief of Staff routes. It never holds `shell_exec`, `screen_capture` or
   `handoff_return`, and it is never on a clock: `runs_per_day: 0`, and no
   intended routine names it. A Chief that runs programs is doing the work.
6. The Reviewer reads and never writes: `fs_list, fs_read, skill_run,
   skill_return`, and no `fs_write`, `shell_exec` or `handoff_delegate`. Grant
   it only runbooks whose declared tools it holds — `world.check` when this
   project has a world, otherwise `none`. `review.diff` calls `fs_write` and
   `shell_exec`: when delivery was asked for, it goes to the Delivery
   specialist, and an open question says the Reviewer cannot run it as proposed.
7. A specialist holds its pack's runbooks, the tools they declare, and
   `skill_run, skill_return`. Declared tools, by pack:
   - Delivery: `review.diff, deploy.draft, alert.draft` call `fs_list, fs_read,
     fs_write, shell_exec`.
   - Intake: `mail.triage, thread.recap, reply.draft` call `fs_list, fs_read,
     fs_write`.
   - Watch: `watch.sweep, watch.digest, watch.impact` call `fs_list, fs_read,
     fs_write`.
   - Budget: `budget.position, budget.runway, budget.alert` call `fs_list,
     fs_read, fs_write`.
   - Social: `social.scan, social.reply, social.post` call `fs_list, fs_read,
     fs_write`.
   - Revenue: `wish.list, revenue.thesis, revenue.pipeline` call `fs_list,
     fs_read, fs_write`.
   `runs_per_day` is 0 unless the human said that domain runs unattended. Then
   it is a small number, and the clock goes under Intended routines as one line:
   who, which runbook, how often. Never `cos.loop`. A routine only exists once
   a person has watched that runbook run and saves it in Settings.
8. A connector the work needs (a mailbox, a monitor, a broker) is not a tool
   name in the roster. Put it under Open questions: installing a program is the
   operator's, and apply refuses a connector tool nothing answers to.
9. If the human said this project has an essence and there is no `world/`, name
   `world.draft` under Open questions. Do not write `world/`.
10. `fs_write` `.aegis/roster/PROPOSAL.md` whole, in this shape. Every identity
    has the four fields on dash lines; lists are comma-separated, and `none` is
    an empty one. A line without a dash is prose for the reader and grants
    nothing.

    # Roster

    What this cabinet is for, in the human's words.

    ## Chief of Staff

    - role: routes work to specialists, keeps the board, and asks the human only when it must
    - tools: fs_list, fs_read, fs_write, skill_run, skill_return, handoff_delegate, memory_write, memory_search
    - skills: cos.loop, never-send-without-review, world.check, world.perceive-delta
    - runs_per_day: 0

    ## Reviewer

    - role: reads what the cabinet produced and says what is wrong with it, without changing it
    - tools: fs_list, fs_read, skill_run, skill_return
    - skills: none
    - runs_per_day: 0

    ## Intended routines

    - none

    ## Open questions

    - none

## How to validate

`fs_read` the file back. Every heading but the last two is an identity with
exactly role, tools, skills and runs_per_day. The Chief holds no `shell_exec`
and has `runs_per_day: 0`. No identity holds a runbook whose declared tools it
lacks. Every specialist is for a domain the human named. Settings lists the
proposal with the reason when it will not parse, and nothing is created until a
person applies it.

## What to return

`skill_return` with `status: done`, `.aegis/roster/PROPOSAL.md` in `artefacts`,
and a summary naming each identity proposed and saying that none exists until
the proposal is applied in Settings → Identities. Copy the file's open questions
into `open_questions`. `status: needs_you` when one of the three questions is
still unanswered, with it in `open_questions`.

## What requires approval

The one `fs_write` is put to the human, and its preview is the first time they
see the team. Applying is not a step here and has no tool: it is a person
pressing apply in Settings, and that press is the grant. Never write
`agents.json`, `routines.json`, `connectors.json`, a `SKILL.md` or anything
under `world/`, and never propose a grant for yourself.

## What to do if the source is missing

If `.aegis/` is not there, return `status: blocked` and say that the shared
files are missing and that *Set up shared files* in the project panel lays them
down. Do not create `.aegis/roster/` with the write: a roster in a workspace
nobody set up is a team for a project that has not agreed to have one.
