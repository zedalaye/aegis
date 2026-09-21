# Changelog

What changed, newest first. The user guide is in `docs/`; the design reasoning is in `PLAN.md`.

## Unreleased

### Security

The approval gate, hardened after a review on 2026-09-14. Each item was a way for a call to run
under an approval that did not describe it.

- **Windows path spellings no longer slip past the gate.** Win32 strips a trailing dot or space from
  a path segment and reads `:` as a stream separator, and a short name such as `GIT~1` opens the
  long-named folder. So `.git.\hooks\pre-commit` wrote a git hook under the workspace-write grant
  instead of being asked every time, `world.\` let a delegated run amend the constitution it is
  refused, and `credentials.` was read without a prompt. A segment with a trailing dot or space, or
  a `:`, is now refused (`E_PATH_INVALID`), and the existing part of a path is spelled the way the
  disk spells it before any rule reads it.
- **A program named by a path has a grant of its own.** Shell grants were keyed on the basename, so
  `scripts\git.cmd` — a file the session could have written — ran under a grant on `git`. A bare name
  is still keyed on the program; a path is keyed on where it resolves.
- **A `git` session grant covers read-only git, and only that.** It used to cover every verb except
  a list of those known to move the tree, so after `git status` was approved,
  `git -c core.fsmonitor=<program> status`, `git config`, an alias, `git diff --output=<file>` and
  `git diff --no-index` all ran without a prompt. The grant now covers an allow-list of read-only
  verbs (`status`, `log`, `diff`, `show`, `blame`, …, and `branch` when it only lists), with no option
  before the verb beyond `--no-pager` and similar, none of `--output`, `--no-index`, `--contents` or
  `--ext-diff`, and not in a folder laid out like a bare repository. Every other line is asked every
  time.
- **A path is resolved again just before the tool touches it.** A folder swapped for a link while
  the dialog was open would have carried the call out of the workspace. `fs_list`, `fs_read`,
  `fs_write` and the working directory of `shell_exec` now refuse when the path no longer resolves to
  what was decided.

### Added

- **More than one provider.** *Settings → Providers* holds up to 16, each with its own label,
  authentication, base URL, model and key (`provider-api-key:<id>` in the credential store; the
  default provider keeps `provider-api-key`). An identity picks a provider and a model, and the model
  badge in a session's header overrides them for that session without changing its identity. A
  provider still in use cannot be deleted. An existing `settings.json` becomes the default provider
  (`PLAN.md` § 7.19).
- **An optional decision model.** *Settings → Decision model* takes a TypeSafe key for Jev, which
  answers typed questions with probabilities and never writes a reply. With it, approval dialogs
  carry an advisory line on how destructive, outbound or irreversible a call looks — the buttons and
  the policy do not change — and two tools become available to identities that are granted them:
  `jev_eval` runs a signed `.aegis/evals/<name>/eval.yml` and returns routes and escalations, and
  `jev_ask` sends model-written questions. Both are asked every time. Drafts are
  `PROPOSAL.yml`, applied by copying them onto `eval.yml` under an ask with no session grant.
  *Set up shared files* creates an empty `.aegis/evals/`. Without a key nothing changes
  (`PLAN.md` § 7.18).

- **A call nobody could answer is parked, not thrown away.** A scheduled run that needs something it
  was not signed for stops at that call, keeps what it has already done, and leaves the question on
  the board under *Parked* — with the path, the diff or the command line the dialog would have
  shown. An approval left unanswered for five minutes parks the same way instead of being refused,
  so walking away from the screen no longer costs the turn. Answering *allow once* runs that exact
  call and nothing else, *allow standing* signs the approval onto the routine (through the same
  checks as saving one), *deny* records the refusal; all three pick the run up in the session it
  stopped in. A run may park three calls, and a question nobody answers for a week closes itself.
  Amending `world/` is never parked (`PLAN.md` § 7.22).
- **Aegis can tell you something is waiting while the window is hidden.** An OS notification for a
  parked call, a run that returned `needs_you` and a routine that stopped itself — the routine's
  name and one sentence, never a path, a command line or an amount, and at most one per routine per
  hour. Clicking it opens the window; nothing is approved from a notification. In the window,
  *Board* in the title bar wears the number of calls waiting for you. A desktop that will not post
  notifications — Windows wants the application installed, not run from a development build — says
  so once in the log and changes nothing else (`PLAN.md` § 7.22).

- **Markdown in the chat.** Replies and your messages render headings, lists, code blocks and tables,
  with the parser the file preview already used: HTML stays text. A web link opens in your browser
  when you click it, and a workspace link opens the file in *Files*. Relative images and this
  session's captures are drawn; remote images are never loaded (`PLAN.md` § 7.20).
- **The model sees images.** A screen capture you approve is sent to the model on the next request,
  and *Attach* (or a drop on the message box) adds PNG, JPEG, GIF or WebP images to a message. Large
  images are downscaled before they are sent. The Codex login does not carry images yet, and says so
  to the model (`PLAN.md` § 7.20).

### Changed

- **A turn no longer stops after eight tool rounds and asks you to type continue.** Repeating the
  same calls three times is a loop (`E_TOOL_LOOP`) and stops those calls; a progressing turn may run
  up to 64 rounds, with or without a runbook. Hitting either guard gives the model one more round to
  finish. The recovery is not a human "continue" (`PLAN.md` § 7.16).
- **A session on a project with no `world/` is told what that word means.** One short paragraph:
  `world/` is the constitution, `.aegis/` is the cabinet, founding is `world.draft`. It does not
  grant the runbook, and it does not appear once a world exists (`PLAN.md` § 7.17).

### Fixed

- The scope sentence of the handoff grant no longer has a run of spaces in the middle.

### Documentation

- The README is a single page; the user guide moved to `docs/guide/`, with `docs/security.md`,
  `docs/troubleshooting.md` and `docs/architecture.md`.
- `AGENTS.md` states the current scope instead of the MVP's; `PLAN.md` is condensed to the decisions
  in force, with its section numbers unchanged; `IDEAS.md`, `COS.md` and `CONTROL.md` are tightened.
- `PLAN.md` § 7.21–7.30 propose the autonomy ladder: parked asks and notifications, narrow standing
  grants, run checkpoints, gates run by the harness, budgets (allocated or earned), the CoS on a
  clock, ingestion, and mandates for irreversible acts. The first of them, § 7.22, has landed; the
  rest are proposed, not built.

### Internal

- The 26 seeded runbooks are Markdown files (`src-tauri/src/skills/seed/`), embedded at build time,
  instead of string literals in Rust.
- Large modules are split without changing behaviour: the turn loop (`agent/turn/`), the policy
  table by tool family (`policy/matrix/`), tool-call parsing (`policy/parse.rs`), Gemini
  (`agent/provider/motosan/gemini.rs`), `shell_exec`'s output and program lookup, session payloads,
  and `AppState` by domain. Inline test modules over 300 lines moved to sibling `tests.rs` files.
- The stylesheet is eleven files imported in cascade order from `src/styles/global.css`.
- The OAuth refresh paths no longer `expect` a client.

## 0.1.0

Everything up to the end of Phase 19 and the § 7.10–7.15 slices of `PLAN.md`.

### The MVP (Phases 0–10)

- A Tauri 2 app that lives in the tray, remembers workspace folders, and keeps sessions whose
  transcripts survive a restart. Replies stream token by token and can be stopped.
- An OpenAI-compatible provider, with the key in the OS credential store and **Test connection**
  telling a wrong address from a wrong key. A scripted provider (`/write`, `/run`, `/capture`,
  `/remember`) exercises the whole gate without a model. Later: Anthropic's native Messages API with
  prompt caching, Gemini, and Claude Code / Codex / Grok CLI logins.
- The approval gate for `fs_list`, `fs_read`, `fs_write`, `shell_exec` and `screen_capture`: deny,
  allow once, or allow for the session. A denial is an ordinary result; session grants are listed
  and revocable, and never touch disk.
- `shell_exec` streams output, marks stderr, and is stopped by Stop or its deadline.
- `screen_capture` always asks, shows no preview, and gives the model a path, size and SHA-256 —
  never the image.
- The audit drawer: the tail of `audit.jsonl`, per session or everything, read-only.

### After the MVP (Phases 11–19)

- **Shared workspace files** (Phase 11): `.aegis/briefs/`, `status/`, `artefacts/`, `decisions/`
  and `skills/`, created by *Set up shared files*, read into every request, written through the gate.
  Setting them up runs `git init` when the folder is not in a work tree, and never commits (§ 7.11).
- **Identities** (Phase 12): a name, a role, instructions and a tool allow-list; a session is bound
  to one for good, and the audit line names it.
- **Skills** (Phase 13): `SKILL.md` runbooks with seven required headings, a one-line catalog in
  every request, bodies loaded on `skill_run`, and a checked `skill_return`.
- **Memory and compaction** (Phase 14): one-sentence memories per identity, asked before they are
  recorded and never deleted by the model; long sessions fold older turns into derived state without
  deleting anything.
- **Handoffs** (Phase 15): briefs out and reports back, each brief in its own session under its own
  identity, bounded and retried once, with `cos.loop` as a runbook.
- **Routines** (Phase 16): one granted, already-witnessed runbook on a clock or a folder trigger,
  unattended, with standing approvals signed on the routine, budgets and self-pausing.
- **The board** (Phase 17): attention, in flight and blocked, from `STATUS.md` and the runtime; runs
  folded from the audit log with their token cost.
- **Connectors** (Phase 18): external MCP servers over stdio; every call asks; grants per tool.
- **Domain packs** (Phase 19): delivery, intake, watch, budget, social, and revenue plus wish list —
  eighteen runbooks, and no change to the runtime.

### Slices (§ 7.10–7.15, and `world/`)

- **Chrome** (§ 7.10): icon title bar and a button that reveals the workspace in the file manager.
- **Execution host** (§ 7.12): a project can run its commands in a WSL distribution, never falling
  back to Windows.
- **Skill proposals** (§ 7.13): `PROPOSAL.md`, then an apply that is signed every time and grants
  nothing.
- **Cabinet founding** (§ 7.14): `cabinet.found` writes a roster proposal; applying it in Settings
  creates the identities with the lists shown.
- **Workspace explorer** (§ 7.15): a read-only tree and preview of the project, and file drops that
  land in `.aegis/briefs/`.
- **The world** (§ 7.2): an opt-in `world/` constitution that framed sessions read, delegated runs
  cannot write, and whose declared sources are not re-read while unchanged.
