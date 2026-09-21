# Identities, memory and compaction

## Identities

A session runs as an identity. **Settings → Identities** is where they are made.

| Field | |
| --- | --- |
| **Name** | unique, case-insensitively |
| **Role** | one line saying what it is for; shown in the picker, and the first thing the model is told about itself |
| **Instructions** | carried into every request. Capped at 2,000 characters: a procedure belongs in a [skill](skills.md) |
| **Provider** | which provider in *Settings → Providers* answers for it |
| **Model** | the model it sends. Empty uses the provider's, and follows it when that changes |
| **Tools** | the allow-list. Anything unticked is refused |
| **Skills** | the runbooks it may load. Granting one ticks `skill_run` and `skill_return` |
| **Model budget** | optional caps in dollars, per run and per day, over everything it runs |

- **A session's identity is fixed.** Pick one from **+** next to *Sessions*. There is no rebinding:
  the transcript is the record of what that identity did.
- **A session's model is not.** The model badge in the header opens a picker: another provider,
  another model, or *Use identity default*. The change applies from the next message and is refused
  while a turn runs. It never changes the identity, its tools, its skills or its memories; a dot on
  the badge marks a session that overrides its identity. Delegated and scheduled sessions start on
  their identity's pair and can be switched the same way.
- **The allow-list is enforced twice.** The model is only shown the tools it holds, and a call for
  any other tool is refused before the approval matrix is read — with no dialog, because a prompt to
  exceed an allow-list should not exist.
- **Granting is not auto-allowing.** An identity holding `fs_write` still puts every write through
  the dialog. A call has to pass both the allow-list and the matrix.
- **The built-in Assistant** holds every tool (connector tools included), no skills and no
  instructions, and cannot be edited or deleted. A session gets it when you do not choose.
  *Duplicate* makes an editable copy with the same perimeter and none of the memories. The Assistant
  always answers from the default provider.
- Deleting an identity that sessions or routines still use is refused, with the count. An edit —
  its provider and model included — reaches its sessions on their next turn, except where a session
  overrides them.
- A roster you apply binds every new identity to the default provider; pick another afterwards.
- **A model budget** is enforced in the runtime, round by round. A *run* is a routine's run or a
  brief; in a session you type into, it is one message's turn. A *day* is UTC and counts everything
  the identity ran. Past a cap, the tools a turn asked for are refused (`E_BUDGET`) and the model
  gets one reply to wrap up; a turn that starts past it sends nothing. A cap needs its model's price
  under *Settings → Providers → Prices*: a capped identity on an unpriced model is refused. A roster
  never carries a budget, and applying one keeps the budgets you set. The built-in Assistant has
  none.

### Founding a cabinet

Aegis creates no identity for you. A session proposes a team as a file, and you apply it.

1. **Duplicate** the Assistant, give the copy a role, and tick `cabinet.found` under Skills.
2. Open a session as the copy, in a project with shared files, and ask for a cabinet. The runbook
   asks what the cabinet is for, whether the project has a world, and whether anything should run
   unattended, then writes `.aegis/roster/PROPOSAL.md` through the ordinary dialog.
3. **Settings → Identities** previews the proposal identity by identity: tools, runbooks, and daily
   run ceiling. Press **Apply roster…**, then **Create them**.

- **Applying is the grant.** The identities are created with exactly the lists shown, each recorded
  as an `agent_create` audit line by you.
- **What you confirmed is what is created.** If the file changed since the preview, the apply is
  refused.
- **All or nothing.** One entry that would be refused refuses the whole roster, with the reason. A
  connector tool counts only if that connector is running.
- **Existing names are skipped, never widened.** The Assistant is never changed.
- **Only identities.** Routines, connectors and `world/` stay yours to create.

No tool applies a roster. The file is markdown: each identity is a `## Name` heading followed by
`- role:`, `- tools:`, `- skills:` and `- runs_per_day:`, so you can write one yourself.

## Memory

A memory is **one sentence an identity keeps**, carried into every later request that identity
makes, in every session. Because that makes it closer to an instruction than a note, its shape is
narrow:

| Kind | For | Example |
| --- | --- | --- |
| `preference` | how someone likes things done | "this client wants everything in French" |
| `exception` | where the usual rule does not apply | "never touch the vendored crate" |
| `convention` | how it is done here | "releases are tagged before the changelog" |

A fact about a project belongs in a [workspace file](workspace.md); a procedure belongs in a skill.
A memory may name its **source** (a path, a ticket, a person); without one, the model is shown it as
a hypothesis.

| | The model | You |
| --- | --- | --- |
| Record one | `memory_write` — **asked first**, showing the whole sentence | *Settings → Memory* |
| Read them | in every request, plus `memory_search` | *Settings → Memory* |
| Correct or forget one | — | *Settings → Memory* |

- **No tool deletes a memory.** What a delete most often removes is a correction you made; the model
  is told to say when one is wrong.
- Writing a sentence already held touches that memory instead of storing a copy.
- An identity holds at most 200. Past that a write is refused rather than something being evicted.
  The 20 most recently confirmed reach the prompt; `memory_search` finds the rest.
- Memories belong to one identity: no tool argument can name another identity.

## Compaction

Every reply pays for the turns before it, so older turns **fold into state** while the recent ones
stay word for word. What the model carries after a fold:

```
Earlier in this session, folded to state. 24 messages are no longer in your context. …

Goal: get the staging deploy working again
Then asked: what about the rollback step · use the 2 GB box
Files written: .aegis/artefacts/checklist.md · .aegis/decisions/DECISIONS.md
Decisions: 2 filed in .aegis/decisions/DECISIONS.md — read it rather than recalling them
Commands run: cargo · git
Skill runs: deploy.draft — done · watch.digest — blocked
Open blockers:
- which registry does staging pull from
Refused earlier: shell_exec ×2. Do not retry a refused call unchanged.
```

- **No model summarizes.** Every line is read off the record: the first request, the paths
  `fs_write` wrote, the programs `shell_exec` ran, the statuses runbooks returned. It is free,
  identical every time, and checkable against `audit.jsonl`. It holds what was done, not what was
  reasoned, which is why the last turns stay raw.
- **Nothing is deleted.** The transcript stays whole on disk and the pane still scrolls through it,
  with a marker and *What it kept*. A later fold is derived from the messages again, never from a
  previous fold.
- It runs at most once per turn, when a transcript passes about 48 KB. **Compact** in the session
  header forces it.
- Memories and the workspace digest are rebuilt into every request, so there is nothing to restore
  after a fold. A fact that must survive belongs in a memory or a file.
