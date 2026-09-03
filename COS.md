# Chef of Staff — operating-mode brief

> **Role.** Invariants of the Chef-de-Cabinet mode. Not a plan, not a coding
> contract.
>
> | Question | File |
> | --- | --- |
> | Rules of the mode | **this file** |
> | When, in what order, after MVP | `PLAN.md` § 7 |
> | Stack, MVP scope, permissions | `AGENTS.md` |
>
> On conflict: `AGENTS.md` wins on *what now*; `PLAN.md` wins on *how and
> when*; this file wins on the rules of the mode once we are in it. Do not
> implement from this file. Do not add phases, IPC, or domains here.

The CoS orchestrates **contracts and files**, not memories. Skills freeze the
*how*, the workspace freezes the *what*, tools freeze the *real*, the context
window keeps only *now*. Invert that and compaction destroys the team.

## Roles

Three identities only.

- **Chief of Staff** — routes, prioritizes, escalates. Almost never does the
  work. It knows who exists, who owns what, what is in-flight, and when to
  wake the human. It does not know "the whole codebase + the whole life +
  three weeks of chats".
- **Specialist** — one narrow job (inbox, code, research, review, …).
- **Human** — irreversible decisions, quality bar, memory correction,
  essence, taste. Not the re-reading of disposable code.

One agent = one perimeter + one definition of done + an explicit list of
things it must not do. A generalist "that helps with everything" is the
first thing that rots. Assembling the roster is a harness act
(`PLAN.md` § 7.14), not a fourth identity: the built-in Assistant drafts
a proposal, the human applies, then there is a Chief, specialists, and
the human. The CoS may *see* state. It may not merge to prod or
send the client email unless that identity was granted those tools.

## Memory

Four layers, never one. If everything lives in the context window, compaction
kills the team. Chat is never the database.

| Layer | Holds | Lives | Who reads |
| --- | --- | --- | --- |
| Source of truth | tickets, mail, calendar, PRs, git | the real tools | everyone, live |
| Shared workspace | world, briefs, status, decisions, artefacts | files in the project folder | the whole team |
| Role memory | preferences, exceptions, "how we do it here" | per-agent store | that agent; CoS as a summary |
| Session context | the current chat | the model window | that agent, today |

Shared memory is files + an index + the origin tools. Each agent then has a
*narrow* memory. A shared transcript is not shared memory. A bank that every
identity — or every other product — writes into is the same mistake with a
database.

The harness must provide these operations:

- **write** — "this decision goes in `DECISIONS.md`, not the thread"
- **read** — retrieve at session start and after compaction
- **flush** — save facts before compact
- **dream / consolidate** — dedupe memory on a slow clock. Not decay, not an
  extractor mining the transcript for new facts: silent eviction would
  sometimes drop a human's correction
- **forget** — the human can correct or delete a stale hypothesis. Not a tool
- **cite** — an important decision points at a file or ticket, not "I remember that"

If a fact must survive ten compactions, it does not belong in chat. File or
skill.

## Work

The CoS does not run a human project-management method. Sprints, activity
tickets, stand-ups, velocity, and cherishing an implementation are ceremony
built around bodies that cannot fork and writing that is scarce. Agents
invert that: generating is cheap, **re-perceiving is expensive**, and the
cost function is round-trips — tokens spent reconstructing what a file
already holds.

A workspace may hold a **world** (`world/`): what the thing *is*, including
the sins a dump showed, and how a new instance is known to be right. It is
opt-in. A watch folder or a wish list has no oracle; empty templates there
are theatre. A sin has a perimeter (this contract, this role, this class of
tasks). A local failure is not an invariant. Promoting one, or deleting one,
is amending the world — the same class as irreversible. There is no agent of
care, and no decay clock on `world/`.

Specialists **read** the world. They do not write it. They do not reopen
declared source artefacts (a dump, logs) to "understand the project". If
the task cannot be done without changing the essence, that is not their
job: `needs_you`, one sentence, `next_owner` the CoS. Changing the world
is a human decision, the same class as irreversible. Hashing `world/` to
police writes is the lockfile of a harness that had no policy; this
harness *is* the policy. Source artefacts may still be hashed, because
nobody writes them in a session — the operator drops a new dump, and that
is the only legitimate re-perception, bounded to the delta.

The unit of work is an **oracle clause** (a claim that can be evidenced by
paths) or an **écart** (the world would have to change). It is not an
activity ticket. A third column — "tech debt", "architecture spike",
"pick a framework" — is how a rewrite cherishes the instance and ignores
the usage.

The CoS **compiles**. Given a world, emit an instance. Verify against the
oracle as a **program**, not a taste review of the diff. If the oracle
fails and the world did not change, regenerate. Throwing the instance away
is legal. Fan-out is the default when file surfaces do not collide; two
specialists on the same dump multiply perception, not work.

A new language for agents, or dedicated hardware, is a later workload —
not the first object. Intention is `world/` now. An agent IR sits on a
CoS that already compiles. Asking agents to play the human who types is
the failure this section exists to prevent.

How this sits on the tree (digest vs frame vs skills, the missed half of
Phase 11) is `PLAN.md` § 7.2. Do not add phases here.

## Skills

A skill is a versioned runbook (`SKILL.md`), not a memory and not a tool. The
CoS says `run skill:inbox.triage`; it does not re-explain how to triage.
Specialists do not invent the process each time.

Every skill declares these headings. The runner enforces them; the model does
not get to skip one:

1. When to use it
2. Inputs required and tools it will call
3. Steps
4. How to validate
5. What to return (strict format — a handoff result)
6. What requires approval (maps onto the policy matrix; a skill cannot auto-grant)
7. What to do if the source is missing (return `blocked`, do not invent)

Then, and only then, a **routine** (clock or trigger) may fire it. Never
automate a still-fuzzy workflow. Catalog vs body, scopes, and audit are
`PLAN.md` § 7.6.

## Handoff

Without a fixed schema the CoS pastes novels and its context explodes. Every
delegation is the same short object. Inputs are links and files, never a
copy-pasted thread.

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

Every return, the same discipline:

```
status: done | blocked | needs_you
summary:             # five lines max
artefacts:           # paths
evidence:            # tests, screenshot, diff
open_questions:
next_owner:
```

Forbidden: "read my whole thread." Allowed: "here is the file and the
criterion." The CoS aggregates **status**, not histories.

## Loop

Always the same:

1. Read the sources of truth, `world/` if it exists, and `/status`
2. Update the attention list (source drift first; then open oracle clauses;
   then écarts)
3. Route new work — compile briefs, or a bounded perceive-delta, not both
4. Retry what is blocked
5. Ping the human only if irreversible, ambiguous, on a deadline, or the
   essence would have to change
6. Write the new status, then stop

Silent when there is nothing to say. A chatty CoS rots as fast as an
amnesiac one.

Also: **fan-out** (one brief → N specialists in parallel), **fan-in** (a
reviewer or verifier before the human sees the plate), bounded timeout and
retry, escalate after two failures — not twelve creative attempts.

Trust order: the agent proposes and the human watches; then the agent
executes and a verifier checks; a routine runs only where the verifier
already rejects slop. Irreversible actions (send, pay, merge, publish,
deploy, trade) stay behind a human gate.

Compaction, per agent: keep the last N turns raw; compact the rest to
*state* (goal, decisions, files, blockers); after compact, re-inject
retrieved memory, not the novel. The CoS compacts a **board**, never other
agents' transcripts.

A run that does not record who, tokens, tools, skill, artefacts, approval,
and failure cannot be budgeted or replayed. Budget per agent and per
routine. Pause, rewind, fire a role, clone a role without cloning its
rotten memory.

## Bar

The mode is real only when three things exist: shared workspace files, a
per-agent memory store, and a skill runner. Until then a "CoS" recites.
A CoS that has those three and still routes activity tickets recites in a
nicer schema: the cabinet without a world, or with a world specialists
rewrite, is not *Work*. What to build, in what order, is `PLAN.md` § 7.
