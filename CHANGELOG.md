# Changelog

What changed, newest first. Section names in *italics* refer to `README.md`.

## Unreleased

### Security

The approval gate, hardened after a review on 2026-09-14. Each item was a way for a call to run
under an approval that did not describe it.

- **Windows path spellings no longer slip past the gate.** Win32 strips a trailing dot or space
  from a path segment and reads `:` as a stream separator, and a short name such as `GIT~1` opens
  the long-named folder. So `.git.\hooks\pre-commit` wrote a git hook under the workspace write
  grant instead of being asked about every time, `world.\` let a delegated run amend the
  constitution it is refused, and `credentials.` was read without a prompt. A segment with a
  trailing dot or space, or a `:`, is now refused (`E_PATH_INVALID`), and the part of a path that
  exists is spelled the way the disk spells it before any rule reads it.
- **A program named by a path has a grant of its own.** Shell grants were keyed on the basename,
  so `scripts\git.cmd` — a file the session could have written — ran under a grant on `git`. A
  bare name is still keyed on the program; a path is keyed on where it resolves.
- **A `git` session grant covers read-only git, and only that.** It used to cover every verb but a
  list of those known to move the tree, so after `git status` was approved,
  `git -c core.fsmonitor=<program> status`, `git config`, an alias, `git diff --output=<file>` and
  `git diff --no-index` all ran without a prompt. The grant now covers an allow-list of read-only
  verbs (`status`, `log`, `diff`, `show`, `blame`, …, and `branch` when it only lists), with no
  option before the verb beyond `--no-pager` and its kind, none of `--output`, `--no-index`,
  `--contents` or `--ext-diff`, and not in a folder laid out like a bare repository, whose
  configuration the model could have written. Every other line is asked about every time.
- **A path is walked again just before the tool touches it.** Policy resolves a path when the call
  arrives, and the dialog may be open for minutes; a folder swapped for a link in between would
  have carried the call out of the workspace. `fs_list`, `fs_read`, `fs_write` and the working
  directory of `shell_exec` now refuse when the path no longer resolves to what was decided. What
  remains is the gap between two system calls, not a handle-based guarantee.

### Fixed

- The scope sentence of the handoff grant no longer has a run of spaces in the middle of it.

## 0.1.0 — the MVP, through Phase 19

What the status block of `README.md` recorded, phase by phase, until it moved here.

**Status: Phase 19 complete (all six domain packs) — the MVP is
feature-complete, and the post-MVP sequence of `PLAN.md` § 7.3 has started.** The app boots,
lives in the system tray, remembers the workspace folders you point it at, and holds
conversations in them: create a session, send a message, watch the reply stream in a token at a
time, and stop it mid-sentence. Transcripts are on disk and survive a restart.

**There is a model behind it now.** Open **Settings**, give it an OpenAI-compatible base URL, a
model id and a key, and replies come from that server — streamed, with the tool calls the model
itself decides to make going through the same gate as everything else. The key goes to your
operating system's credential store, never to a file Aegis writes and never to the window; the
panel shows where it came from and four characters of it, and **Test connection** tells you
which of "wrong address", "wrong key" and "server down" you are looking at. Until you configure
one, replies come from the scripted provider of Phase 5 — see *Point it at a model*.

The session header names who is working and what is answering: the identity the session was
opened as, and the model the runtime started the running turn with — or the one settings say
will answer the next message. With no provider configured it
reads `scripted provider`, marked, so a reply with no model behind it can never be mistaken
for one that has.

A tool call that needs your permission asks for it. `fs_list`, `fs_read`, `fs_write`,
`shell_exec` and `screen_capture` run through the decision matrix; anything it will not allow
on its own opens a prompt showing the exact path and content that would be written, or the
exact program, arguments and working directory that would run, and you answer **deny**,
**allow once** or **allow for this session**. A denial is an ordinary result — the model is
told, and the turn carries on. Session grants are listed under the transcript while they are
in force, with a Revoke button beside each; they never touch disk and die with the session.
Every call is audited whichever way you answer.

A running command's output arrives in the transcript as it is produced, stderr marked apart
from stdout, capped and scrolled. Stop kills it. So does its deadline — two minutes, or
whatever shorter one the caller asked for.

**`screen_capture` takes a picture of your primary display**, and only ever after you say so:
there is no state in which it runs without a prompt. The prompt names the display and both of
its sizes — the pixels the file would hold, and the points your screen is set to — and says
what a capture contains. It deliberately shows no preview of what would be captured, because
taking a picture of the screen to illustrate the question would already have done the thing
being asked about. Once you allow it, the capture appears in the transcript as a thumbnail you
can click for the full-size image. The PNG is written under the app data directory, never into
your workspace, and the model is given its path, its size and a SHA-256 — never the image, so a
capture does not reach the provider you configured. The audit line records the same three
things and never the picture.

The scripted provider is still there and still useful: with no base URL configured it answers
every message with what the runtime sent it, and **`/write`**, **`/run`**, **`/capture`** and
**`/remember`** make it ask for a file write, a real command, a real screenshot and a real
memory, so the whole gate can be walked through without spending a token or configuring a
provider.

**The audit log has a window.** The audit-log button in the title bar opens a drawer beside the
transcript showing the tail of `audit.jsonl`, newest first: one row per tool call with the
time, the tool, how it came to run, how it ended, how long it took and how many bytes it
carried. *More* opens the turn and call ids, the redacted arguments in full, the SHA-256 over
them, and the file a capture wrote. It reads the log rather than the transcript on purpose —
the record is kept independently of the story the model tells about a session, and a call that
shows up in one but not the other is exactly what you would open this to find. *This session*
and *Everything* switch between the conversation in front of you and the whole log, which
covers sessions you have since deleted. New lines appear as they are written while the drawer
is open. Nothing in the window can append to that file or clear it.

**A workspace can now keep shared memory in files.** *Set up shared files* in the sidebar
creates `.aegis/` in the folder you picked, holding `briefs/`, `status/`, `artefacts/` and
`decisions/` — only what is missing, never overwriting anything you already have. Once they exist, every request carries
what `STATUS.md` says and the recent end of `DECISIONS.md`, plus the *names* of your briefs and
artefacts, capped so a long ledger cannot eat the context window. Asking for a decision to be
recorded writes `.aegis/decisions/DECISIONS.md` through the ordinary approval dialog: no new tool, no
privileged path, no hidden store beside your folder. See *Shared workspace files*.

**And those files get a history.** Setting them up also makes the folder a git repository, if it
is not already in one — because a `STATUS.md` rewritten in place with nothing behind it means
yesterday's board is gone and the transcript is the only log again, which is the thing the
convention exists to stop being. It is `git init` and nothing else: no remote, no `.gitignore`,
no name and email invented for you, and **no commit, then or ever**. A folder that is already
inside a repository is left alone rather than given a second one. When you want a snapshot you
ask for it — in your own terminal, or in the session, where `git commit` is an ordinary command
in the approval dialog. There is no Commit button, and nothing commits on a timer. See
*Shared workspace files*.

**A session now runs as an identity.** *Settings → Identities* creates one: a name, a line
saying what it is for, instructions it carries into every request, and — the part that matters
— a tick-list of the tools it may use. Open a session as it from the **+** next to
**Sessions**, and that session is bound to it for good. An identity is not shown the tools it was
not granted, so a "reviewer" with only `fs_list` and `fs_read` never asks to write a file; and
if it asks anyway, policy refuses before anything runs, with no dialog offering to let it
through. The audit line names the identity, so *who ran this* is answerable afterwards. With
only the built-in **Assistant**, the + creates a session as it — the assistant Aegis had
before identities existed, now with a name. See *Identities*.

**And an identity can follow a runbook.** A skill is a `SKILL.md` — when to use it, the tools
it will call, the steps, how to check the result, and what to do when the source it needs is
missing. Aegis creates one in your library on first run and one in each workspace you set the
shared files up in. What every request carries is the *catalog*: one line per runbook the
identity may run. The steps are loaded only when it says `skill_run`, for that reply, so twenty
procedures cost twenty lines of context rather than twenty procedures. A run grants nothing —
every step is an ordinary tool call through the same dialog — and a runbook calling a tool the
identity does not hold is refused before its first step rather than halfway through. The run
ends with a status object that is checked, not believed: a `done` naming a file that is not on
disk comes back refused. Every audit line in between carries the skill's name. See *Skills*.

**An identity now remembers things, and a long session stops paying for its whole history.**
A memory is one sentence — a **preference**, an **exception** or a **convention** — belonging
to one identity and reaching the top of every reply that identity gives, in this session and
every session after it. The model can record one and is asked first, with the sentence itself
in the dialog; it can search what it holds; it **cannot delete one**, because correcting a
memory is yours. *Settings → Memory* is where you read, add and forget them. And when a
conversation gets long, its older turns fold into a few lines of **state** — the goal, the
files written, the decisions filed, the blockers left open — while the last few turns stay
word for word. Nothing is deleted: your transcript stays exactly as it was and the pane still
scrolls through all of it; what changes is only what the model carries. **Compact** in the
session header does it now; otherwise it happens on its own once a transcript gets expensive.
See *Memory* and *Compaction*.

**One identity can now hand work to another, and a clock can start one.** A *handoff* is a
brief out and a report back — goal, inputs as paths, definition of done — never a transcript to
read; a delegated run opens in the sidebar as an ordinary session with a `brief` badge, under
its own identity and the same approval gate. A *routine* fires one granted runbook, as one
identity, on a clock or when a folder changes, and it runs whether or not this window is open.
Nobody is watching a scheduled run, so it is never asked anything: what it may do is exactly
what you signed on the routine, and everything else is refused rather than parked on a prompt
you cannot see. See *Handoffs* and *Routines*.

**And now there is a board.** *Board* in the title bar answers, for the open project, *who ran,
what did it cost, and why did it fail* — without opening a chat. Three columns: what wants a
person, what is running, what stopped short — half of it read structurally out of your own
`.aegis/status/STATUS.md`, half of it what the runtime can see for itself, with every line saying
which. Underneath, every run in the recent log: a delegation with its specialists, a morning's
firing of a routine, a runbook, a conversation — each with who ran it, what it spent, what it
left on disk, and the audit lines it is replayed from. Nothing there writes: correcting the
board means editing `STATUS.md`, in your editor or through the same approval dialog as any
other change to your files. See *The board*.

**And Aegis can now use tools it did not write.** A *connector* is an external MCP server — a
program on your machine that Aegis starts and asks for a list of tools. Those tools reach the
model as `<connector>__<tool>` and go through exactly the pipeline `fs_write` goes through:
offered only to identities that hold them, judged by the same table, audited on the same log.
The one row that is different is the one that matters — **every connector call is put to you**,
every time, with no auto-allow and no read-only exemption, because what a program somebody else
wrote does with its arguments is not something this runtime can check. Allowing one for the
session covers that one tool and nothing else the connector offers, including anything it adds
later. Adding a connector starts a program, so only you can do it: there is no tool that
installs one, and granting its tools to an identity is a second act, on the identity. See
*Connectors*.

**The first domain pack is three files, and none of them is in the runtime.** `review.diff`,
`deploy.draft` and `alert.draft` are runbooks in your library — review a range before it goes
to a client, draft the deploy somebody else runs, turn an alert into a note and a reply nobody
has sent. Aegis did not learn about clients, forges or hosting to get them: no command, no
tool, no row of the policy matrix changed, and an install that does no client work has three
folders it can delete. Each one stops one step short of the thing that cannot be taken back —
the merge, the deploy, the sent reply — because that step is yours. Nothing is granted by
being seeded: they reach a model when you make an identity and tick them. See *The delivery
pack*.

**The second pack reads mail and never sends any.** `mail.triage`, `thread.recap` and
`reply.draft` turn a message that arrived as a file into a ticket, work out what a thread
actually agreed, and write the answer you send. Not one of the three declares `shell_exec`, so
the identity holding them cannot run a command — which is what you want of the one pointed at
text strangers wrote. A message asking for money to move or for access comes back to you
whatever it says, and the reply is a file with a line under every claim saying which file it
came from. See *The intake pack*.

**The third pack is the one that runs while you are asleep.** `watch.sweep`, `watch.digest` and
`watch.impact` turn material you dropped in a folder into entries, report only what the last
report did not, and say what one item would actually cost this project. It is the first pack
whose § 7.3 line says *scheduled*, so `watch.digest` is built for *Settings → Routines* — which
means it is never asked anything at four in the morning, and what it may do beyond reading is
exactly what you signed. Nothing here fetches: a `curl` signed once and fired daily is an
outbound channel with nobody on it. And a quiet week writes no file at all, because a digest
that always has five items is a digest inventing them. See *The watch pack*.

**The fourth pack watches money and cannot touch it.** `budget.position`, `budget.runway` and
`budget.alert` turn exports you dropped on disk into one page saying what is held and what is
owed, answer how long it lasts as a range rather than a number, and say when a line you set has
been crossed. It is the first pack whose material is arithmetic, so a figure is either copied
from a line naming its export or shown as a sum you can redo, and a total that will not
reconcile comes back to you. It declares three tools — list, read, write — and § 7.3's reason is
blunt: *not a broker*. Aegis has nothing that buys, sells, transfers or pays. See *The budget
pack*.

**The fifth pack drafts what you post and cannot post it.** `social.scan`, `social.reply` and
`social.post` find the few things worth answering, draft one answer, and draft one post about
something that has already happened. It is the first pack whose artefact is addressed to nobody
in particular — a mistaken mail is fixed by a second mail, and nothing fixes a post — so both
drafts end by handing the file to `never-send-without-review`. Its adversary is the material:
the sharp reply performs best, so *being wrong* is excluded from the criterion by name, and
*none worth answering* is the ordinary answer. See *The social pack*.

**The sixth pack keeps what you want and what might pay for it — apart.** `wish.list`,
`revenue.thesis` and `revenue.pipeline` hold your goals in your own ordering, write one money
idea well enough to be wrong, and show what is funded and what the gap is. It is the only pack
whose material has not happened, so nothing in it may read as a fact: an ordering nobody stated
is *unordered*, a price nobody looked up is *not priced*, and a thesis carries what would show it
false or it is not written. A thesis may not read your wish list and the pipeline gives a
proposal no number, because the worst artefact here would be a trade argued for by a holiday.
See *The revenue and wish-list pack*.

**That completes Phase 19.** Six domains, eighteen runbooks, and `agent/turn.rs` never opened:
see *Phase 19, and what it did not do*.
