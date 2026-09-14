# Domain packs

A **pack** is a kind of work assembled from what Aegis already has: a project folder, runbooks, the
connectors the work needs, and one identity holding them. It is not a runtime feature. Nothing in
`src-tauri` knows what a client, a mailbox or a bank statement is, and no pack added a command, a
tool, a policy row or a screen.

Aegis ships the runbooks, seeded in your library. It installs no connector and creates no identity:
**a pack exists once you make the identity and tick its runbooks.** An install that does none of
this work has folders it can delete.

| Pack | Runbooks | Suggested identity | Its rule |
| --- | --- | --- | --- |
| Delivery | `review.diff`, `deploy.draft`, `alert.draft` | *Delivery*: `fs_list`, `fs_read`, `fs_write`, `shell_exec` | stops one step before the merge, the deploy, the sent reply |
| Intake | `mail.triage`, `thread.recap`, `reply.draft` | *Intake*: `fs_list`, `fs_read`, `fs_write` | runs no command: its inputs were written by strangers |
| Watch | `watch.sweep`, `watch.digest`, `watch.impact` | *Watcher*: `fs_list`, `fs_read`, `fs_write` | runs unattended, so *nothing new* must be cheap |
| Budget | `budget.position`, `budget.runway`, `budget.alert` | *Treasurer*: `fs_list`, `fs_read`, `fs_write` | a figure is copied or shown, never asserted |
| Social | `social.scan`, `social.reply`, `social.post` | *Voice*: `fs_list`, `fs_read`, `fs_write` | public and permanent, so publishing is yours |
| Revenue + wish list | `wish.list`, `revenue.thesis`, `revenue.pipeline` | *Steward*: `fs_list`, `fs_read`, `fs_write` | its material has not happened, so nothing reads as a fact |

**Setting one up** is the same every time: open the work as its own project, press *Set up shared
files*, put the material on disk (usually `.aegis/briefs/` — no pack fetches anything), make the
identity, and grant it the three runbooks. Artefacts land in `.aegis/artefacts/`. A draft meant to be
sent ends by naming `never-send-without-review`; run that on it before you send.

Across packs, the empty answer is ordinary: *no ask*, *nothing new* and *none worth answering* return
`done` with no artefact. A procedure pointed at a pile always finds something unless finding nothing
is cheap.

## Delivery

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `review.diff` | a revision range or a patch, and the files as they now stand | `<branch>.review.md`, ending in *ship*, *change first* or *do not ship* | merge, push, tag, or answer the pull request |
| `deploy.draft` | how the project says it ships: its own runbook, the compose file, the CI workflow | `deploy-<env>-<rev>.md`: the commands in order, each undoable one with how to undo it | deploy, or run a step "to be sure" |
| `alert.draft` | the alert, plus read-only health checks | an incident note citing the command behind each observation, and a draft reply | restart, scale, roll back, or send |

- A project with no account of how it ships gets `blocked`, not a pipeline inferred from framework
  defaults.
- The incident note keeps observed and inferred apart, and the reply names no cause the note marked
  as a guess.
- A project's own deploy procedure belongs in its `.aegis/skills/`. Name it `deploy.draft` to replace
  the library runbook for that project, or anything else (`deploy.this-app`) to have the library
  runbook read it as its source.
- Connectors are optional: the diff comes through `shell_exec`, deploy facts from the project's
  files. A forge connector later replaces the source, not the procedure.

## Intake

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `mail.triage` | one message as a file (an exported `.eml`, a forwarded thread); attachments by name only | `ticket-<date>-<who>.md`: the quoted ask, the quoted date or *no date given*, who was on it, what it waits on | answer it, act on it, or start the work |
| `thread.recap` | one thread, oldest first | `recap-<thread>.md`: a line per message, then **agreed**, **outstanding**, **never answered** | decide anything outstanding |
| `reply.draft` | the ticket, the recap, `STATUS.md`, `DECISIONS.md` | a draft with a **sources** block naming the file behind each claim | send, schedule, or say it is on its way |

- An ask must be a quoted sentence carrying its message's date; "as soon as you can" is not a date.
- **A message about money or access** — new bank details, a new invoice address, a password reset
  nobody asked for — always returns `needs_you`, marked as unverified on a second channel.
- Attachments are named, not read: an `.eml` is mostly base64, and a large one gets cut off inside
  the attachment.
- `thread.recap` counts each quoted sentence once. Silence is not agreement.
- `reply.draft` commits to no date, price or scope that no file supports, and refuses to run without
  a ticket path.
- Putting the ticket on the project's board is the workspace's own `inbox.triage`.

## Watch

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `watch.sweep` | material in `.aegis/briefs/`: a saved page, a release note, a paper, an export | `watch-<source>.md`: the quoted claim, and what the source actually **shows** | fetch anything, or judge whether it matters |
| `watch.digest` | the entries since the last digest, and that digest's closing list | `watch-digest-<date>.md`: **changed**, **worth reading**, **noise** — or no file | report an item twice, or decide anything |
| `watch.impact` | one entry, plus `world/`, the decisions and the board | `impact-<entry>.md`: what it touches by path, what would have to be true, what acting and not acting cost | recommend, or write `world/` |

- Built for a routine: *Watcher*, `watch.digest`, daily, signed for *write files inside the
  workspace*. Run `watch.sweep` and `watch.digest` by hand first — the routine door requires a
  witnessed run.
- Nothing fetches: a `curl` signed once and fired at 04:00 is an outbound channel with nobody on it.
- An entry is named after its source, so the artefacts folder is the bookkeeping. Delete an entry
  to have its source read again.
- A quiet period writes no digest and returns `done`; a `blocked` would pause the routine.
- When `watch.impact` concludes the essence would have to change, it returns `needs_you`. A routine
  cannot be signed to amend `world/`.
- Run `watch.impact` yourself, on an entry the digest pointed at.

## Budget

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `budget.position` | exports on disk: a bank CSV, a broker statement, an invoice ledger | `position-<date>.md`: **held**, **owed**, **committed**, **unplaced**, each figure citing its export | add currencies at an unnamed rate, or hide a total that does not reconcile |
| `budget.runway` | one position, the standing commitments, written-down income | `runway-<date>.md`: the outgoings, the division shown, and a **range** in months | give a single number, or rank what to cut |
| `budget.alert` | one written threshold and the figure it names | `alert-<figure>-<date>.md`: what crossed what, by how much, since when, and **one** question | propose anything |

- **A figure is copied** (naming the export and the line) **or shown** (addends beside the total).
  A stated total is reconciled, and a difference comes back to you.
- No `shell_exec`: *not a broker*. A program on PATH is a calculator until it is a broker's client.
  Install a read-only connector if you want sums computed by a program.
- A position is dated by its **stalest** input, and `budget.runway` refuses a position older than
  the period it divides by.
- Overlapping exports are de-duplicated on account, date and amount; near matches become
  discrepancies. What fits no heading goes under *unplaced*.
- Every outgoing names its frequency and the file that says so; an annual charge is never read as
  monthly.
- A threshold must be quotable from a dated file that predates the move. Write your thresholds down
  first.

## Social

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `social.scan` | an export of mentions or a timeline, plus a written criterion | `social-scan-<date>.md`: at most three items, each with the sentence that qualifies it and the file that answers it — or no file | list a post because it is wrong |
| `social.reply` | one item, the file that answers it, earlier posts | `social-reply-<handle>-<date>.md`: one or two sentences, with **sources** | correct someone when the fact alone would do |
| `social.post` | the files showing something happened | `social-post-<date>-<subject>.md`: the draft, its sources, and anything in it not yet true | use the future tense about this house |

- Two reasons to answer: a question this house can answer from a file, or someone relying on
  something of ours that a fact fixes. Being wrong on the internet is not one.
- The criterion comes from a dated file, read **before** the export. A hostile post about you returns
  `needs_you` with nothing drafted.
- The runbooks name the shapes to refuse: the opening *actually*, the correction nobody needed, the
  joke at someone's expense, the rhetorical question, the reply that is really an announcement.
- `social.reply` rereads its draft as a stranger and with the question cropped off, and will not
  pick the item — you do.
- `social.post` announces nothing that is not on disk, uses no hooks or thread markers, and names no
  competitor.

## Revenue and wish list

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `wish.list` | the goals file, and what someone actually said | `goals.md`: goals in **their** order, each priced or *not priced*, with what it waits on | invent an order, add goals, or judge a want |
| `revenue.thesis` | an idea stated by a person, and the files that bear on it | `thesis-<date>-<subject>.md`: the claim, its conditions, a dated **falsifier**, the cost of being wrong | read the wish list or the position, or attach a size |
| `revenue.pipeline` | the goals file and the money actually there | `pipeline-<date>.md`: cost, covered and gap per goal, in the person's order | give a proposal a number, or tie one to a goal |

- An ordering nobody stated is **unordered**; a price nobody looked up is **not priced**; a thesis
  with no falsifier is not written.
- The halves are kept apart on purpose: a thesis may not read the goals or the position, and the
  pipeline links no proposal to a goal.
- A goal is a file, not a memory: memories belong to one identity, are capped, and go when it is
  deleted.
- A wrong thesis is never deleted; the outcome is appended to it.
- `revenue.thesis` runs only on an idea you had. Run `wish.list` before the pipeline.

## What stays yours

Merging, deploying, sending, posting, buying, selling, transferring and paying. Aegis has no tool
that does any of them, and a connector that offers one is asked about on every call.

The catalog of seeded runbooks rides in every request of an identity granted them, so it is bounded:
the 24 seeded runbooks at the end of Phase 19 came to 5,556 characters, and a test fails if a catalog
line ever stops being capped.

Design: `PLAN.md` § 7.3, Phase 19.
