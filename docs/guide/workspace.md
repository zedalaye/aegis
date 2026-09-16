# Workspace files and the world

Aegis' own records live in the application-data directory ([Data](data.md)). This page is about
files in **your** workspace folder. They are ordinary files: reached by the same `fs_read` and
`fs_write`, through the same approval dialog, on the same audit log, and committed with the
repository.

A workspace can hold two layers with opposite mutation rules:

- the **cabinet**, under `.aegis/` — in-flight work, rewritten every turn;
- the **world**, `world/` at the root — what the project *is*, which specialists read and never
  write.

## The cabinet (`.aegis/`)

| Directory | Holds |
| --- | --- |
| `.aegis/briefs/` | work going in: one file per piece of work (goal, inputs as *paths*, definition of done), or material a runbook reads |
| `.aegis/status/` | `STATUS.md`: what is true now. A board rewritten in place, not a log |
| `.aegis/artefacts/` | what was produced: a draft, a report, an export, a patch |
| `.aegis/decisions/` | `DECISIONS.md`: one entry per decision, newest last |
| `.aegis/skills/` | this project's own runbooks. Seeded with `inbox.triage` |

**Set up shared files** in the sidebar creates what is missing, with short templates, and never
overwrites a file. Nothing is created until you press it. You can also create the directories
yourself; the panel measures the folder rather than remembering what it did.

The dot is a tidiness convention, like `.github/`, not a hiding place. Two practical costs: Finder
hides dot-directories until you press ⌘⇧., and `rg` needs `--hidden`.

A workspace set up before the `.aegis/` layout has its directories at the root, where the runtime no
longer looks. The panel names them and does not move them: `mv` them into `.aegis/`.

**The agent reads them.** Every request carries `STATUS.md` and the recent end of `DECISIONS.md`
(each capped at 2 KB, with a note saying how much was cut) and the *names* of the files in `briefs/`
and `artefacts/`. Workspace runbooks reach the model through the skill catalog.

**The agent writes them like any other file.** Recording a decision is an ordinary `fs_write`
through the ordinary dialog. There is no privileged path and no hidden store.

A decision that lives only in a transcript cannot be found later, corrected, or survive
compaction; a file can. `COS.md` has the reasoning.

### Versioning

Setting up shared files also makes the folder a git work tree when it is not in one:

| The folder | What the button does |
| --- | --- |
| is already a repository | leaves it |
| is **inside** a repository | leaves it and names the folder that holds the history — never a nested `.git` |
| neither | runs `git init`, and says so |

It runs `git init` and stops: **no commit**, then or ever, and no remote, `.gitignore`, `user.name`
or `user.email`. Aegis' own records are never in this repository.

- A folder set up before versioning existed shows *Make it a git repository*.
- Without `git` on PATH, the directories are still created and the panel says the folder is not
  versioned.
- On a project with a WSL host, the distribution's `git` runs the init. If the distribution cannot
  be reached, the folder stays unversioned; there is no fallback to the Windows `git`.

**To commit**, use your terminal, a Git GUI or your editor — or ask in a session. The identity needs
`shell_exec`, and the dialog shows `git` with its exact arguments. Allowing `git` for the session
covers read-only lines only (`status`, `log`, `diff`, `show`, …); `add`, `commit`, `push`, and any
line that could write a file or run a program, ask every time. There is no Commit button. The first
commit needs a git identity; set it globally, or with `git config` under the same gate.

### Seeing them

**Files** in the title bar shows the open project's folder.

- **Tree** — one folder at a time. `.aegis/` and `world/` are shown and marked. `.git`,
  `node_modules` and what the ignore files name are hidden unless you tick *Show ignored*. A folder
  over 1,000 entries says how many were left out.
- **Preview** — read-only. Markdown is drawn as elements: HTML stays text, remote images do not load,
  and web links are shown but not followed. A link or backticked path to a workspace file opens that
  file. Images are shown; other files get their name, size, type and *Show in folder*. There is no
  Save.
- **Drop a file** on the Files panel, on `.aegis/briefs/` in the tree, or on a project row in the
  sidebar, and the runtime copies it into that project's `.aegis/briefs/`. The original stays where
  it is, and a taken name keeps both files. Drops onto `.aegis/artefacts/`, the rest of `.aegis/`, or
  `world/` are refused. No session starts and no runbook is chosen. A workspace without
  `.aegis/briefs/` refuses and offers to set up the shared files. Each arrival is one audit line,
  *by you*.

## The world (`world/`)

| File | Holds |
| --- | --- |
| `world/essence.md` | what this is, and what it is for |
| `world/schema.md` | the shapes it is made of, as perceived |
| `world/behaviours.md` | how it behaves, including the sins a source showed, each with its perimeter |
| `world/oracle.md` | how a new instance is known to be right |
| `world/decisions.md` | decisions that moved the essence (the operational ones stay in `.aegis/decisions/`) |
| `world/sources.yml` | the dumps, exports and logs all of that was perceived from |

`world/` sits at the root because it is the project rather than a tool's view of it; it should make
sense to someone who has never run Aegis.

**Nothing creates it.** A world starts when you write `world/essence.md`, in your editor or through
an approved `fs_write`. None of the files is required, but a folder holding only `sources.yml` is
not a world.

Once a world exists:

- **Every session is framed.** The system message says to read `world/` before planning, take it as
  given, not reopen the declared sources, and — if the work would change what the thing *is* — stop
  and say which line would have to move. That is an **écart**. The frame carries status (which files
  exist, what is declared, any drift), never `essence.md` itself. The runtime injects it because a
  runbook can be skipped.
- **Delegated work cannot amend it.** An `fs_write` under `world/` from a specialist on a brief is
  refused with no dialog, and the specialist returns `needs_you`. In a session you are in, it is a
  high-risk ask with a session grant of its own: a workspace-write grant never covers `world/`, and a
  world grant covers nothing else. A routine can neither ask nor be signed for it. Only the first path
  segment counts, so `src/world/` is an ordinary folder.
- **A perceived source is not read again.** `sources.yml` declares each source with the length and
  digest it was recorded at:

  ```yaml
  sources:
    - path: sources/legacy-dump.sql
      bytes: 18234112
      sha256: 3f9a…
    - sources/2026-08-prod.log      # declared, nothing perceived from it yet
  ```

  `fs_read` of a source that still matches is **denied**: what it said is in `world/`. A declared
  path that escapes the workspace is refused.
- **A moved source holds the work.** While a declared source has drifted, a brief does not launch
  unless it is the perceive-delta, whose `inputs` name the moved path. Drift is measured at brief
  launch and in the panel, not on every request.

Four runbooks come with it:

- **`world.draft`** reads the project, drafts `schema.md` and `behaviours.md` from evidence it can
  name, then asks you what the project is for and how you would know a new version is right, and
  quotes your answers into `essence.md` and `oracle.md`. Inside a brief its writes are refused like
  any other write to `world/`.
- **`world.perceive-delta`** perceives only the sources that moved.
- **`world.verify`** checks an instance against the oracle as a program.
- **`world.check`** reads the constitution and says where things stand.

They are granted to identities like any runbook. A session on a folder that has no `world/` yet is
still told the two layers and the name `world.draft` — not the steps. Reasoning: `COS.md` *Work*.
Design: `PLAN.md` § 7.2 and § 7.17.
