# Chef of Staff — operating-mode brief

The rules of the Chef-de-Cabinet mode. Not a plan and not a coding contract: `AGENTS.md` wins on
scope, `PLAN.md` on design and order, this file on the rules of the mode. Code cites these section
names (`COS.md` *Handoff*); keep them.

The CoS orchestrates **contracts and files**, not memories. Skills freeze the *how*, the workspace
freezes the *what*, tools freeze the *real*, and the context window keeps only *now*. Invert that
and compaction destroys the team.

## Roles

Three, and only three.

- **Chief of Staff** — routes, prioritizes, escalates. Almost never does the work. It knows who
  exists, who owns what, what is in flight, and when to wake the human — not the whole codebase,
  the whole life and three weeks of chats.
- **Specialist** — one narrow job (inbox, code, research, review, …).
- **Human** — irreversible decisions, the quality bar, memory correction, the essence, taste. Not
  re-reading disposable code.

One agent = one perimeter + one definition of done + an explicit list of what it must not do. A
generalist "that helps with everything" is the first thing that rots. The CoS may *see* state; it
may not merge to production or send the client email unless that identity was granted those tools.

Assembling the roster is a harness act, not a fourth role: an identity the human made drafts a
proposal, and the human applies it (`PLAN.md` § 7.14).

## Memory

Four layers, never one. Chat is never the database.

| Layer | Holds | Lives | Who reads |
| --- | --- | --- | --- |
| Source of truth | tickets, mail, calendar, PRs, git | the real tools | everyone, live |
| Shared workspace | world, briefs, status, decisions, artefacts | files in the project folder | the whole team |
| Role memory | preferences, exceptions, "how we do it here" | per-agent store | that agent; the CoS as a summary |
| Session context | the current chat | the model window | that agent, today |

Shared memory is files plus the origin tools; each agent then has a *narrow* memory. A shared
transcript is not shared memory, and neither is one bank every identity writes into.

The harness provides:

- **write** — "this decision goes in `DECISIONS.md`, not the thread"
- **read** — at session start and after compaction
- **flush** — save facts before compacting
- **consolidate** — dedupe on a slow clock. No decay and no extractor mining transcripts: silent
  eviction would drop a human's correction
- **forget** — the human corrects or deletes a stale hypothesis. Not a tool
- **cite** — an important decision points at a file or a ticket, not "I remember that"

If a fact must survive ten compactions, it is a file or a skill, not chat.

## Work

The CoS does not run a human project-management method. Sprints, activity tickets, stand-ups,
velocity and cherishing an implementation are built around people who cannot fork and writing
that is scarce. For agents, generating is cheap, **re-perceiving is expensive**, and the cost is
round-trips: tokens spent reconstructing what a file already holds.

A workspace may hold a **world** (`world/`): what the thing *is*, including the sins a source
showed, and how a new instance is known to be right. It is opt-in; a watch folder or a wish list
has no oracle, and empty templates there are theatre. A sin has a perimeter (a contract, a role, a
class of tasks); a local failure is not an invariant. Promoting or deleting one amends the world,
which is in the same class as irreversible. There is no agent of care and no decay clock on
`world/`.

Specialists **read** the world and do not write it, and they do not reopen declared sources to
"understand the project". If the task cannot be done without changing the essence, that is an
**écart**: `needs_you`, one sentence, `next_owner` the CoS. Changing the world is a human decision.
Policy enforces this, not a checksum of `world/`. Declared sources may still be hashed, because only
the operator replaces them — that is the one legitimate re-perception, bounded to the delta.

The unit of work is an **oracle clause** (a claim evidenced by paths) or an **écart** (the world
would have to change). Not an activity ticket, and not a third column of tech debt, spikes or
framework choices.

The CoS **compiles**: given a world, emit an instance, and verify it against the oracle as a
**program**, not by a taste review of the diff. If the oracle fails and the world did not change,
regenerate; throwing the instance away is legal. Fan out when file surfaces do not collide; two
specialists on the same dump multiply perception, not work.

A new language or dedicated hardware for agents is a later workload, not the first object.
Intention is `world/` now. How this sits on the tree is `PLAN.md` § 7.2.

## Skills

A skill is a versioned runbook (`SKILL.md`), not a memory and not a tool. The CoS says
`run skill:inbox.triage`; it does not re-explain how to triage.

Every skill declares these headings, and the runner enforces them:

1. When to use it
2. Inputs required and tools it will call
3. Steps
4. How to validate
5. What to return (strict format — a handoff result)
6. What requires approval (maps onto the policy matrix; a skill cannot auto-grant)
7. What to do if the source is missing (return `blocked`, do not invent)

Then, and only then, a **routine** (clock or trigger) may fire it. Never automate a still-fuzzy
workflow. Catalog, body, scopes and audit: `PLAN.md` § 7.6.

## Handoff

Without a fixed schema the CoS pastes novels and its context explodes. Every delegation is the same
short object, and inputs are links and files, never a pasted thread:

```
goal:
owner:
priority:
inputs:              # paths / URLs, not paste
constraints:
definition_of_done:
approval_needed:
return_format:       # status | artefact | question
```

Every return has the same discipline:

```
status: done | blocked | needs_you
summary:             # five lines max
artefacts:           # paths
evidence:            # tests, screenshot, diff
open_questions:
next_owner:
```

Forbidden: "read my whole thread." Allowed: "here is the file and the criterion." The CoS
aggregates **status**, not histories.

## Loop

Always the same:

1. Read the sources of truth, `world/` if it exists, and `/status`
2. Update the attention list: source drift first, then open oracle clauses, then écarts
3. Route new work — compile briefs, or a bounded perceive-delta, not both
4. Retry what is blocked
5. Ping the human only if irreversible, ambiguous, on a deadline, or the essence would change
6. Write the new status, then stop

Silent when there is nothing to say: a chatty CoS rots as fast as an amnesiac one.

Also: **fan-out** (one brief → N specialists in parallel), **fan-in** (a reviewer or verifier
before the human sees the plate), bounded timeout and retry, escalate after two failures — not
twelve creative attempts.

Trust order: the agent proposes and the human watches; then the agent executes and a verifier
checks; a routine runs only where the verifier already rejects slop. Irreversible actions (send,
pay, merge, publish, deploy, trade) stay behind a human gate.

Compaction, per agent: keep the last turns raw, compact the rest to *state* (goal, decisions, files,
blockers), then re-inject retrieved memory, not the novel. The CoS compacts a **board**, never other
agents' transcripts.

A run that does not record who, tokens, tools, skill, artefacts, approval and failure cannot be
budgeted or replayed. Budget per agent and per routine. Pause, rewind, fire a role, clone a role
without cloning its rotten memory.

## Bar

The mode is real only when three things exist: shared workspace files, a per-agent memory store,
and a skill runner. Until then a "CoS" recites. A CoS that has those three and still routes activity
tickets recites in a nicer schema: a cabinet without a world, or with a world specialists rewrite,
is not *Work*.
