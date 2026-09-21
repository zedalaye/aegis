# Security posture

Read this before pointing Aegis at anything you care about.

## The approval gate

- **Reads inside the workspace are automatic; mutating calls ask.** Writes, commands, captures,
  memories, delegations and connector calls prompt. A path outside the workspace prompts every time
  and can never be allowed for a session. Credential-shaped names (`.env*`, `*.pem`, `*.key`,
  `id_rsa*`, `.ssh`, `.aws`, `credentials`, …) are asked about even for reads.
- **Paths are judged as the OS will open them.** Symlinks and junctions are resolved before
  containment is decided, and a path that looks contained but leads out through a link is refused
  outright. On Windows, a path segment ending in a dot or a space, or containing `:`, is refused
  (Win32 would open a different name), and existing folders are spelled as the disk spells them, so
  `GIT~1` is judged as `.git`.
- **A path is resolved again just before the tool runs.** If a folder became a link while the dialog
  was open, the call is refused. What remains is the gap between two system calls, not a
  handle-based guarantee.
- **"Allow for this session" is narrow and temporary.** A grant lives in memory, dies with the
  session, and is listed under the transcript with a Revoke button.

  | Grant | Covers |
  | --- | --- |
  | workspace write | any file in the workspace, except under `.git/` and `world/` |
  | world | writes under `world/`, and nothing else |
  | a program by name (`pnpm`) | that program as found on PATH, with any arguments |
  | a program by path (`./gradlew`) | that file only, by its resolved path |
  | `git` | **read-only lines**: `status`, `log`, `diff`, `show`, `blame` and similar, and `branch` when it only lists — with no option before the verb besides `--no-pager` and the like, none of `--output`, `--no-index`, `--contents` or `--ext-diff`, and not in a folder laid out like a bare repository |
  | a connector tool | that one tool, by full name |
  | large reads, screen capture, memory, delegation | that call type |

  Never granted, always asked: paths outside the workspace, writes under `.git/`, applying a skill
  proposal, and any other `git` line.
- **A program allowed for the session can do whatever that program does.** Allowing `pnpm` allows
  its scripts; allowing workspace writes and a build tool lets a session change what the build runs.
  Grant a program the way you would hand over a terminal.
- **An unanswered prompt is parked**, not run and not thrown away. After five minutes the dialog
  closes, the call is kept on the board with everything the dialog showed, and the turn carries on
  having been told so. A prompt whose turn you stop is a refusal, as before.
- **A routine's standing approvals are the one grant that outlives a session**, and you sign them
  when saving the routine — or when you answer *allow standing* on something it parked, which goes
  through the same checks. They cannot exceed what the runbook declares or the identity holds, and
  cannot cover anything outside the workspace, under `.git/` or in `world/`. A write approval can
  be narrowed to one folder and a command approval to one shape (`cargo test …`); the narrow ones
  only ever match where the wide one would, so they inherit every one of those exclusions. A shape
  over a program that runs workspace code (`cargo`, `pnpm`, `make`, …) is still that program
  running that code.
- **A scheduled run parks what it was not signed for.** Nothing runs, the run ends saying so, and
  the question waits on the board until you answer it — *allow once* covers that exact call and
  nothing else, *allow standing* signs it onto the routine, *deny* refuses it. Answering picks the
  run up in its own session. A run may park three calls; a question nobody answers for a week
  closes itself. Amending `world/` is never parked: it is a decision you make in a session.
- **A notification is not a dialog.** Aegis can tell you something is waiting while the window is
  hidden; the notification carries the routine's name and one sentence, never a path, a command
  line or an amount, and nothing is approved by clicking it — the click opens the window.

## Tools

- **`shell_exec` uses no shell.** A program and an argument vector, spawned directly: no
  metacharacters, pipes, globs or `&&`. Risk badges on `rm` or `curl` are presentational. On Windows
  a `.cmd` shim goes through `cmd.exe` with its arguments escaped by the Rust standard library.
- **A command is bounded, not contained.** It has a deadline (two minutes at most), Stop, 64 KB of
  output to the model, and an audit line. **There is no sandbox**: approved tools run as you, with
  your privileges and environment. Treat every approval as if you were typing the command.
- **Screen captures always ask** and show no preview. The PNG is written under the app data
  directory, never into the workspace, and never reaches the model or the provider: the model gets a
  path, a size and a SHA-256. A blank capture (macOS without Screen Recording permission) is refused
  with `E_SCREEN_PERMISSION`.
- **A connector is a program Aegis cannot see into.** Every call asks, a server's `readOnlyHint`
  changes nothing, a grant covers one tool, the server is given no client capabilities (no
  `sampling`, no `roots`), and its tool descriptions reach your prompt. Treat a connector like a
  script you are about to run. Only you can add one.

## Identities, skills and delegation

- **An allow-list narrows; it never auto-allows.** A tool outside it is refused with no prompt, and a
  tool inside it still goes through the gate.
- **A skill is a procedure, not a permission.** Every step is an ordinary call. A `SKILL.md` someone
  sent you is instructions your model will follow, with whatever you ticked.
- **Writing is not granting.** A skill or roster proposal does nothing until you apply it. Applying a
  roster is the grant, and its preview shows every list first.
- **Delegation is one approval, for the routing.** Each specialist runs in its own session with none
  of your grants, so its writes and commands prompt again, and it cannot delegate further.
- **Delegated work cannot amend `world/`**, and a workspace-write grant never reaches it. The limits:
  only the first path segment counts; `shell_exec` is not covered by this rule (its own prompt is);
  and a declared source that has *changed* is readable again by design.

## What reaches your provider

- **Memories**, in every request of the identity holding them. The dialog shows the whole sentence;
  the model cannot delete a memory or read another identity's.
- **Shared files**: `STATUS.md` and the end of `DECISIONS.md` (2 KB each), and the *names* of files
  in `briefs/` and `artefacts/`.
- **Compaction** changes only what is sent. The transcript stays whole, and the folded state is
  derived from the record, not written by a model.
- **Keys stay out of the WebView.** The key sits in the OS credential store or `AEGIS_API_KEY`, is
  read only by the Rust runtime, and is never written to `settings.json`, logged or audited. The UI
  sees where it came from and four characters. Never put a key in `localStorage`.
- **A base URL is where your key goes.** There is no fallback endpoint, and no key is borrowed from
  another tool's variable. Over `http://` the key crosses the network in clear text; keep that for a
  local server.
- **CLI logins** (Claude Code, Codex, Grok) reuse that CLI's credentials and present Aegis as the
  CLI. That may be outside the provider's terms for your account; check before relying on it.

## What reaches TypeSafe

Only with a TypeSafe key set (*Settings → Decision model*); without one, nothing is sent.

- **Approval annotations** send what the dialog shows — the tool, summary, reason and preview (a
  write's first 4 KB, a command line, a connector's arguments) — once per dialog, while *annotate
  approval dialogs* is on. Never the transcript or the system prompt. The answer is advice: it
  cannot allow, deny or remove a button.
- **`jev_eval`** sends the files an eval names as inputs, after you approve the call. The questions
  come from the signed `eval.yml`, not the model; a `PROPOSAL.yml` never runs.
- **`jev_ask`** sends the state and questions the model wrote, shown in full in the dialog.
- The TypeSafe key is used only for TypeSafe, and the chat key is never sent there.

## Files, git and the window

- **Nothing commits for you.** *Set up shared files* may run `git init`; the runtime never makes a
  commit or writes a remote, a `.gitignore` or a git identity. A commit is a `git` command you
  approve.
- **The window shows files; it does not write them.** *Files* uses the same containment, the window
  holds no filesystem or opener permission, and markdown is drawn as elements (no HTML, no remote
  images). The only write is a file you drop, copied by the runtime into `.aegis/briefs/`. Tauri
  widens the `asset:` scope for dropped paths; Aegis narrows it back to the captures and attachments
  directories on every drop.
- **Chat is markdown drawn by the same parser, and still never HTML.** A reply cannot add an element
  the parser does not know: raw HTML stays text, and there is no `<a href>` anywhere, so nothing in a
  reply can navigate the window.
- **A web link opens in your browser only when you click it.** The address is shown next to the
  label. The runtime opens `http` and `https` addresses only — `file:`, `javascript:`, `data:`,
  `tauri:`, an address with a user name in it, and anything else are refused — and hands it to the
  system as one argument, never a shell line. The window holds no opener permission.
- **Images in a reply never come from the network.** A remote image is a link you can click, so a
  reply cannot use one to tell a server you read it. A workspace image is read by the runtime; a
  capture is shown only when this session made it.
- **Images you attach, and captures, go to the model you chose.** An attached image is copied into
  the app's data folder, never your workspace, and a capture is sent on the next request after you
  approved it. The bytes go from the runtime to the provider over HTTPS; they are not written into
  `sessions.json`, the audit log, or anything the window receives. A model that refuses images fails
  the turn with the provider's message.
- **Capabilities are least-privilege.** The window holds no plugin permission; every privileged
  operation is a policy-gated Rust command.

## The record

Every tool call is one line in `audit.jsonl` — allowed, refused or failed — with the identity, skill
run, routine, delegation, policy reason and outcome. Arguments are stored as a SHA-256 plus a
redacted copy that keeps paths and replaces content with its size. Nothing in the UI can write to or
clear the log.

Design: `PLAN.md` § 3 (matrix and grants), § 5.4 and § 7.4.
