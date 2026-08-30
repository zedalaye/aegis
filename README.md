# Aegis

Local desktop AI harness. Runs agent sessions that can operate your machine — files, shell,
screenshots — behind an explicit approval gate.

Aegis is a harness, not a model. It brings the UI, the runtime, the permission layer and the
audit trail; you point it at an OpenAI-compatible provider.

- **UI shell** — Tauri 2 + TypeScript + React + Vite
- **Runtime** — Rust (`src-tauri`), where the agent loop, tool execution, secrets and policy live
- **No Electron**, no bundled Chromium: the app uses the OS WebView

The WebView renders UI only. No tool ever executes in the browser context, and no API key is
ever sent to it.

> **Status: Phase 12 (agent registry) — the MVP is feature-complete, and the
> post-MVP sequence of `PLAN.md` § 7.3 has started.** The app boots,
> lives in the system tray, remembers the workspace folders you point it at, and holds
> conversations in them: create a session, send a message, watch the reply stream in a token at a
> time, and stop it mid-sentence. Transcripts are on disk and survive a restart.
>
> **There is a model behind it now.** Open **Settings**, give it an OpenAI-compatible base URL, a
> model id and a key, and replies come from that server — streamed, with the tool calls the model
> itself decides to make going through the same gate as everything else. The key goes to your
> operating system's credential store, never to a file Aegis writes and never to the window; the
> panel shows where it came from and four characters of it, and **Test connection** tells you
> which of "wrong address", "wrong key" and "server down" you are looking at. Until you configure
> one, replies come from the scripted provider of Phase 5 — see *Point it at a model*.
>
> The session header names who is working and what is answering: the identity the session was
> opened as, and the model the runtime started the running turn with — or the one settings say
> will answer the next message. With no provider configured it
> reads `scripted provider`, marked, so a reply with no model behind it can never be mistaken
> for one that has.
>
> A tool call that needs your permission asks for it. `fs_list`, `fs_read`, `fs_write`,
> `shell_exec` and `screen_capture` run through the decision matrix; anything it will not allow
> on its own opens a prompt showing the exact path and content that would be written, or the
> exact program, arguments and working directory that would run, and you answer **deny**,
> **allow once** or **allow for this session**. A denial is an ordinary result — the model is
> told, and the turn carries on. Session grants are listed under the transcript while they are
> in force, with a Revoke button beside each; they never touch disk and die with the session.
> Every call is audited whichever way you answer.
>
> A running command's output arrives in the transcript as it is produced, stderr marked apart
> from stdout, capped and scrolled. Stop kills it. So does its deadline — two minutes, or
> whatever shorter one the caller asked for.
>
> **`screen_capture` takes a picture of your primary display**, and only ever after you say so:
> there is no state in which it runs without a prompt. The prompt names the display and both of
> its sizes — the pixels the file would hold, and the points your screen is set to — and says
> what a capture contains. It deliberately shows no preview of what would be captured, because
> taking a picture of the screen to illustrate the question would already have done the thing
> being asked about. Once you allow it, the capture appears in the transcript as a thumbnail you
> can click for the full-size image. The PNG is written under the app data directory, never into
> your workspace, and the model is given its path, its size and a SHA-256 — never the image, so a
> capture does not reach the provider you configured. The audit line records the same three
> things and never the picture.
>
> The scripted provider is still there and still useful: with no base URL configured it answers
> every message with what the runtime sent it, and **`/write`**, **`/run`** and **`/capture`**
> make it ask for a file write, a real command and a real screenshot, so the whole gate can be
> walked through without spending a token or configuring a provider.
>
> **The audit log has a window.** *Audit log* in the title bar opens a drawer beside the
> transcript showing the tail of `audit.jsonl`, newest first: one row per tool call with the
> time, the tool, how it came to run, how it ended, how long it took and how many bytes it
> carried. *More* opens the turn and call ids, the redacted arguments in full, the SHA-256 over
> them, and the file a capture wrote. It reads the log rather than the transcript on purpose —
> the record is kept independently of the story the model tells about a session, and a call that
> shows up in one but not the other is exactly what you would open this to find. *This session*
> and *Everything* switch between the conversation in front of you and the whole log, which
> covers sessions you have since deleted. New lines appear as they are written while the drawer
> is open. Nothing in the window can append to that file or clear it.
>
> **A workspace can now keep shared memory in files.** *Set up shared files* in the sidebar
> creates `briefs/`, `status/`, `artefacts/` and `decisions/` in the folder you picked — only what
> is missing, never overwriting anything you already have. Once they exist, every request carries
> what `STATUS.md` says and the recent end of `DECISIONS.md`, plus the *names* of your briefs and
> artefacts, capped so a long ledger cannot eat the context window. Asking for a decision to be
> recorded writes `decisions/DECISIONS.md` through the ordinary approval dialog: no new tool, no
> privileged path, no hidden store beside your folder. See *Shared workspace files*.
>
> **A session now runs as an identity.** *Settings → Identities* creates one: a name, a line
> saying what it is for, instructions it carries into every request, and — the part that matters
> — a tick-list of the tools it may use. Open a session as it from the picker beside **New
> session**, and that session is bound to it for good. An identity is not shown the tools it was
> not granted, so a "reviewer" with only `fs_list` and `fs_read` never asks to write a file; and
> if it asks anyway, policy refuses before anything runs, with no dialog offering to let it
> through. The audit line names the identity, so *who ran this* is answerable afterwards. Leaving
> the picker alone gets the built-in **Assistant**, which holds every tool — the assistant Aegis
> had before identities existed, now with a name. See *Identities*.

---

## Requirements

| | Version | Notes |
| --- | --- | --- |
| Node.js | ≥ 20.19 | 24 LTS recommended |
| pnpm | 10.x | `corepack enable pnpm` — the version is pinned by `packageManager` in `package.json` |
| Rust | ≥ 1.85 stable | `rustup` toolchain, MSVC host on Windows. 1.85 is the edition-2024 floor the screen-capture crate needs |

Plus one platform toolchain:

- **Windows** — Visual Studio Build Tools with the *Desktop development with C++* workload, and
  the **WebView2 runtime** (see below).
- **macOS** — Xcode Command Line Tools (`xcode-select --install`).
- **Linux** — `webkit2gtk-4.1`, `libayatana-appindicator3`, `librsvg2`, `patchelf` and the usual
  build essentials, plus `libssl-dev` (the HTTPS client uses the system TLS stack),
  `libdbus-1-dev` (the credential store talks to a Secret Service over D-Bus), and the
  screen-capture crate's stack: `libxcb1-dev`, `libxrandr-dev`, `libpipewire-0.3-dev`,
  `libwayland-dev`, `libegl-dev`, `libgbm-dev`, `libdrm-dev`, `libclang-dev` and `clang`. None
  of these are needed on Windows or macOS, which have their own capture and credential APIs
  built in. This is the heaviest of the three Linux dependency sets and it exists for one tool;
  Linux is best-effort here (`AGENTS.md`).

  On Debian/Ubuntu the `-dev` packages are what `pkg-config` and the linker look for.
  `libsoup-3.0-dev` is WebKitGTK 4.1's HTTP stack — not an Aegis dependency of its own, but a
  missing soup is the usual "I installed webkit2gtk and it still won't compile" error (4.0
  wanted soup2; 4.1 wants soup3). `libgbm-dev` / `libegl-dev` / `libwayland-dev` are the
  Wayland side of `xcap`: the crate compiles without them, then `cc` fails with
  `unable to find library -lgbm` (or `-lEGL`, or `-lwayland-client`). A WSL2 Ubuntu is a valid
  compile host with the same list.

  ```sh
  sudo apt install \
    build-essential curl wget file pkg-config clang libclang-dev patchelf \
    libwebkit2gtk-4.1-dev libsoup-3.0-dev \
    libayatana-appindicator3-1 libayatana-appindicator3-dev librsvg2-dev \
    libssl-dev libdbus-1-dev \
    libxcb1-dev libxrandr-dev libpipewire-0.3-dev \
    libwayland-dev libegl-dev libgbm-dev libdrm-dev
  ```

## Run it

```sh
pnpm install
pnpm tauri dev
```

`pnpm tauri dev` starts Vite on port 1420 and builds the Rust binary; the first Rust build takes
a few minutes, later ones are incremental.

### Walk through it in two minutes

No provider and no key needed — the scripted provider is enough to exercise the whole runtime.

1. **Add a project.** *Add workspace…* in the sidebar opens the native folder picker. The folder
   you choose is the workspace: the only place a tool may touch without asking every time.
2. **Start a session** and send anything. The reply streams in a token at a time; the header
   names what produced it.
3. **Make it ask.** Send a message containing `/write`, `/run` or `/capture`. The prompt names
   the exact file and content, the exact program and arguments, or the display and its size.
   Answer **deny** once to see a refusal land in the transcript without killing the turn, then
   send it again and **allow once**.
4. **Check the record.** Open **Audit log** in the title bar. Both calls are there — the refused
   one and the allowed one — with the policy's reason and the outcome.
5. **Close the window.** The app stays in the tray; the tray icon brings it back. *Quit* is the
   only thing that ends it.
6. **Restart.** The project, the session and the transcript are where you left them.
7. **File a decision.** Press *Set up shared files* in the sidebar, then ask for a decision to be
   recorded. It is written to `decisions/DECISIONS.md` through the same approval dialog, and the
   next reply already knows about it. See [Shared workspace files](#shared-workspace-files).
8. **Open a session as someone narrower.** In *Settings → Identities*, make a **Reviewer** with
   only `fs_list` and `fs_read` ticked. Back in the sidebar, pick it in the **as** control and
   press *New session*, then send `/write`. No prompt appears: the write is refused outright
   because that identity does not hold `fs_write`, and the audit line records the refusal against
   it. See [Identities](#identities).

## Point it at a model

Out of the box there is no provider, and replies come from a scripted one that reports what the
runtime actually sent it. To use a real model, open **Settings** in the title bar and fill in:

| | |
| --- | --- |
| **Base URL** | An OpenAI-compatible endpoint, stopping where `/chat/completions` would begin — `https://api.openai.com/v1`, `http://127.0.0.1:11434/v1` for a local server, or whatever your gateway exposes. |
| **Model** | The model id, spelled the way that server spells it. |
| **API key** | Saved to the OS credential store. Leave it empty to keep the one already there. |

**Test connection** sends one sixteen-token completion to the endpoint Aegis actually uses and
reports what came back — which tells a wrong address from a wrong key from a model that server
does not serve. It costs a few tokens; that is the price of an answer you can trust. Clearing the
base URL puts you back on the scripted provider.

Anthropic's own API works through its OpenAI-compatibility layer: base URL
`https://api.anthropic.com/v1`, a Claude model id such as `claude-opus-5`, and your Anthropic API
key. (If your key has access to more than one workspace you may also need to pick one — Aegis
sends no `anthropic-workspace-id` header.)

If this machine has no usable credential store — headless Linux, a locked keychain, a dev build
whose signature keeps changing — set the key in the environment instead and restart:

```sh
# macOS / Linux
export AEGIS_API_KEY="sk-..."
```

```powershell
# Windows PowerShell
$env:AEGIS_API_KEY = "sk-..."
```

Settings then reports the key as coming from `env` and says so in the panel. Aegis reads no other
variable: a key exported for another tool was not chosen for the endpoint you configured here.

## Build it

```sh
pnpm tauri build
```

Output lands in `src-tauri/target/release/bundle/`. The MVP does not sign or notarize anything —
see *Troubleshooting*.

## Tests

```sh
cd src-tauri && cargo test     # Rust: persistence, policy, tools, audit, wire protocol, turns
cd src-tauri && cargo clippy --all-targets -- -D warnings
pnpm typecheck                 # TypeScript, strict
```

Everything runs headless. The screen-capture tests are the one place that depends on the machine,
and they are written so that either answer passes: a capture must produce exactly one PNG whose
digest matches its audit line, *or* be refused with `E_SCREEN_PERMISSION` and produce nothing.
A developer's desktop takes the first branch, a macOS box without Screen Recording consent or a
headless runner takes the second, and the thing being tested — that there is no third outcome —
holds on all of them.

### Generated bindings

`src/ipc/bindings.ts` is generated from the Rust payload structs by
[`ts-rs`](https://github.com/Aleph-Alpha/ts-rs) — **do not edit it by hand.** The export runs as
part of `cargo test`, so regenerating is:

```sh
cd src-tauri && cargo test
```

The destination is set once in `src-tauri/.cargo/config.toml`; each payload type carries
`#[ts(export, export_to = "bindings.ts")]`. Changing a payload in Rust and forgetting to
regenerate shows up as a TypeScript error rather than as `undefined` in the UI, so run the Rust
tests before `pnpm typecheck` when you have touched an IPC type.

---

## Where your data lives

Projects, sessions, identities and settings are stored as four small JSON documents —
`projects.json`, `sessions.json`, `agents.json` and `settings.json` — under the application-data
directory:

| | Path |
| --- | --- |
| Windows | `%APPDATA%\dev.aegis.harness\` |
| macOS | `~/Library/Application Support/dev.aegis.harness/` |
| Linux | `~/.local/share/dev.aegis.harness/` |

`projects.json` holds names and workspace paths. `sessions.json` holds your conversations — the
messages you sent, the replies, the tool calls each turn made, and which identity the session
runs as. `agents.json` holds the identities you have made; the built-in one is not in it, because
it is a constant in the runtime rather than a record you could delete. `settings.json` holds the
base URL and the model id. **None of them holds a key**, and none records whether anything is
*running*: a session interrupted by a crash or a power cut comes back idle, because there is no
turn left to finish it.

Your API key is not in this directory at all. It goes to the operating system's own credential
store, under the service name **Aegis** and the account **provider-api-key** — Credential Manager
on Windows, Keychain on macOS, a Secret Service on Linux — where you can inspect or delete it
without Aegis. Set `AEGIS_API_KEY` in the environment instead and Aegis uses that; the credential
store wins when both are present.

All four documents are meant to be readable and are safe to edit by hand while Aegis is closed.
A document Aegis cannot parse is renamed to `<name>.corrupt-<timestamp>.json` and the app starts
with an empty list rather than refusing to open. Deleting a project forgets it and its sessions;
the workspace folder itself is never touched.

Beside them, `captures/` holds the PNGs `screen_capture` writes — one file per approved capture,
named `capture-<UTC timestamp>-<random>.png`. They are here rather than in your workspace on
purpose: a capture is Aegis' own artefact, and one written into a project folder would end up in
your next commit. Nothing ever deletes them; the folder is yours to empty, and a transcript that
refers to a capture you have deleted says so instead of showing it. This is also the **only**
directory the window is allowed to read a file from, and only through Tauri's `asset:` protocol,
scoped to it at startup — the WebView has no filesystem permission of any kind.

Beside it, `audit.jsonl` records one JSON line per tool call — every call, whether it ran, was
refused or failed. It is append-only and plain text, so `tail -f` works and you do not need Aegis
running to read it. Each line names the session, the identity it ran as, the tool, why policy
decided what it did, and what came of it. It records the *paths* a call touched but never the contents of a file: a log
that quoted every `fs_write` would become the one place on your machine where everything the
agent ever wrote is collected in plain text. A line for a capture carries the file's path, its
pixel size and a SHA-256 of the bytes on disk — enough to say later which capture a call
produced, and to check that the file is still that one, without the log holding a copy of it.
The **Audit log** drawer in the title bar reads the tail of this file — the last 200 lines — and
is the same information in a window; the file is the record, and nothing in the UI writes to it.
Aegis never rotates or trims it; deleting it is yours to do, and a new one starts on the next
tool call.

---

## Shared workspace files

Aegis' own data is above. This is the other half: files that live in **your** workspace folder,
not in Aegis' application-data directory, and that the agent reads at the start of every reply.

The convention is four directories:

| | Holds |
| --- | --- |
| `briefs/` | one file per delegated piece of work — goal, inputs as *paths*, definition of done |
| `status/` | `STATUS.md`: what is true right now. A board, rewritten in place, not a log |
| `artefacts/` | what was produced — a draft, a report, an export, a patch |
| `decisions/` | `DECISIONS.md`: one entry per decision, newest last |

**Set up shared files** in the sidebar creates whatever is missing and seeds each one with a
short template. It never overwrites: a file that is already there is left byte for byte as it
was, and the panel says which files it created and which it kept. Nothing is created until you
press it — a workspace is a folder you already own, usually a repository with its own layout, and
four directories should not appear in it because you pointed an app at it. You can equally make
them by hand, or in a terminal; the panel measures the folder rather than remembering what it did
to it. They are ordinary files: commit them, edit them in your editor, `grep` them.

Once they exist, two things change.

**The agent reads them.** Every request carries the current `STATUS.md`, the recent end of
`DECISIONS.md`, and the *names* of what is in `briefs/` and `artefacts/`. Names, not contents:
a brief is referred to by path and read with `fs_read` if it is needed, so a folder full of long
documents does not quietly consume the context window. The two state files are capped at 2 KB
each in the prompt, and when a file is longer the agent is told how much it is not seeing and
where the rest is.

**The agent writes them the same way you do.** Asking for a decision to be recorded produces an
ordinary `fs_write` — the same approval dialog, the same audit line, the same workspace
containment as any other change to your files. There is no privileged path that edits
`DECISIONS.md` behind the gate, and no hidden store beside your folder.

Why bother: a chat is forgotten and a file is not. A decision that lives only in a transcript
cannot be found later, cannot be corrected, and does not survive the conversation being
compacted or restarted. `COS.md` is the reasoning in full; `PLAN.md` § 7.3 is where this sits in
the sequence.

---

## Identities

A session runs as an identity: a name, what it is for, the instructions it carries, and the tools
it may use. **Settings → Identities** is where they are made.

| | |
| --- | --- |
| **Name** | what the session picker shows. Unique, case-insensitively |
| **Role** | one line saying what this identity is for. It is what the picker shows beside the name, and the first thing the model is told about itself |
| **Instructions** | carried into the system message of every request this identity makes. Capped at 2000 characters on purpose — see below |
| **Tools** | a tick-list of `fs_list`, `fs_read`, `fs_write`, `shell_exec`, `screen_capture`. Everything unticked is refused |
| **Skills** | recorded, not yet runnable. Nothing in this build executes a skill; the runner is the next phase |

Pick one from the **as** control beside *New session* and the session is bound to it. That
binding is permanent: there is no way to move a session to a different identity, because a
transcript is the record of what one identity did, and rewriting whose record it is would leave
`fs_write` calls in the history of something that was never allowed to make one. Working as
someone else is a new session, which costs a click.

**The tool list is enforced twice, and both matter.** The model is only ever *shown* the schemas
for the tools its identity holds, so a reviewer with `fs_list` and `fs_read` does not spend a
round asking for a write it would be refused. And if a call for an ungranted tool arrives anyway
— replayed out of an older transcript, or invented — policy refuses it before anything touches
the machine, with no approval dialog: a prompt offering to let an identity exceed its own
allow-list is a prompt that should not exist. The refusal reaches the model as an ordinary
`E_DENIED` result naming the identity and the tool, so it can say what it was trying to do and
try something else.

**Granting a tool is not the same as auto-allowing it.** An identity with `fs_write` still puts
every write to you through the usual dialog. The allow-list narrows *what an identity could ever
do*; the approval matrix decides *what happens on this call*. They compose — a call has to pass
both.

**The built-in Assistant cannot be edited or removed.** It holds every tool, carries no
instructions, and is what a session gets when you do not choose. It is the assistant Aegis had
before identities existed, written down rather than reinvented, which is why every conversation
from before this phase still opens and still behaves exactly as it did. Removing an identity that
sessions still run as is refused, and the message says how many — delete those sessions first, or
keep it. Editing one reaches its sessions on their next turn.

**Instructions are capped, and the cap is deliberate.** The system message stays a policy summary
plus what is true right now. An identity may say what it is; it may not carry a runbook. Recurring
procedure belongs in a skill — a versioned `SKILL.md` loaded only when it is invoked — which is
`PLAN.md` § 7.6 and the next phase. An identity whose instructions had grown into a runbook would
be exactly the procedure that phase has to take back.

---

## Layout

```
src/           React app — presentation and typed IPC glue only
  ipc/         invoke() / listen() wrappers; bindings.ts is generated from the Rust structs
  state/       zustand stores
  components/  layout, chat, sessions, approvals, projects, agents, settings, audit
src-tauri/
  src/
    commands/  one module per IPC command domain
    agent/     turn loop, wire protocol, providers (scripted, and OpenAI-compatible over SSE),
               event payloads, turn registry
    store/     projects.json, sessions.json, agents.json and settings.json, behind one
               atomic write
    tools/     fs, shell, screenshot — behind one ToolSpec registry
    policy/    path containment, decision matrix, per-session grants
    workspace.rs  the shared-file convention inside a project folder: scaffold, and the
               capped digest every request carries
    approval.rs  pending approvals: the channel a turn parks on until you answer
    audit.rs   one jsonl line per tool call
    secrets.rs OS credential store, environment fallback, masking
  capabilities/  least-privilege Tauri permission sets
```

`PLAN.md` is the design of record: IPC surface (§ 2), the tool policy matrix (§ 3), the wire
protocol (§ 4), platform risks (§ 5), the phase order (§ 6) and what comes after it (§ 7).
`AGENTS.md` fixes the stack and the scope; `COS.md` is the operating mode the § 7 phases build
towards.

## Security posture

Read this before pointing Aegis at anything you care about.

- **Reads inside the workspace are automatic. Everything mutating asks.** Writes, shell commands
  and screen captures always prompt; anything touching a path outside the workspace prompts every
  single time and can never be granted for a session.
- **"Allow for this session" is narrow and temporary.** A grant is keyed to a scope — a directory
  subtree, or one shell program by name — never to a tool as a whole. Grants are never written to
  disk, and they die with the session. There is no "allow forever". While one is in force it is
  listed under the transcript of the session that created it, with a Revoke button; revoking
  restores the prompt from the next call onwards.
- **An unanswered prompt is a refusal.** An approval nobody answers within five minutes is
  refused, as is one whose turn you stop. Neither ends the conversation: the model is told the
  call was refused and carries on.
- **`shell_exec` does not use a shell.** It takes a program and an argument vector and spawns them
  directly, so there is no metacharacter or quoting layer to defeat. Pipes, redirection, globs and
  `&&` are not features: one call runs one program. The risk badges on commands like `rm` or
  `curl` are *presentational* — they change the wording of the prompt, not what is permitted.
- **A command is bounded, not contained.** It is killed at its deadline (two minutes at most) and
  when you press Stop, its output is capped at 64 KB in what the model sees, and every call is
  audited. None of that is a sandbox — see the next point.
- **An identity's tool list narrows what it could ever do; it does not auto-allow anything.** A
  session opened as an identity is never shown the schemas for tools it was not granted, and a
  call for one is refused with no prompt offered — you cannot be asked to let an identity exceed
  its own list. What it is *not* is a second approval layer with a bypass: ticking `fs_write` for
  an identity still puts every write to you through the ordinary dialog. A call has to pass both.
- **There is no sandbox.** Approved tools run as you, with your privileges and environment. The
  real boundary is that you read the exact path, program, arguments and working directory before
  approving. Treat every approval as if you were typing the command yourself.
- **Screen captures are never auto-allowed**, and there is no argument or grant state that makes
  one happen without a prompt. A capture can contain anything on your display — a password
  manager, a private conversation, someone else's face in a call — so the prompt names the
  display and says as much, and it shows no preview, because capturing the screen to illustrate
  the question would already have done the thing being asked about. Images are written under the
  app data directory, never into the workspace. The **image never leaves your machine**: the
  model is handed the path, the size and a SHA-256, and this build cannot read a capture back
  into the conversation, so nothing about it reaches the provider you configured. The audit log
  records those same three things and never the picture. A capture that comes back entirely
  blank — which is how macOS reports a missing Screen Recording permission — is refused as
  `E_SCREEN_PERMISSION` rather than handed to the model as a picture of an empty desktop.
- **The shared files are sent to your provider.** Once `status/` and `decisions/` exist, every
  request carries what is in them — that is the point of them, and it is worth knowing before you
  put something in `STATUS.md` you would not paste into a chat. Only those two files are read,
  capped at 2 KB each; `briefs/` and `artefacts/` contribute file *names* and never content. A
  workspace with none of those directories sends nothing extra, and nothing creates them for you.
- **Keys stay out of the WebView.** The API key lives in the OS credential store (or in
  `AEGIS_API_KEY`) and is read only by the Rust runtime, which attaches it to the request as a
  header marked so it cannot be printed. There is no command that returns a key: the UI can save
  one and clear one, and what it gets back is where the key came from plus four characters of it.
  It is never written to `settings.json`, never logged, and never in an audit line. Never put a
  key in `localStorage`.
- **A base URL is somewhere your key gets sent.** Aegis talks to the server you name and only
  that one; it never falls back to another endpoint, and it never reads a key exported for a
  different tool — which is why the environment variable is `AEGIS_API_KEY` and not
  `OPENAI_API_KEY`. Over `http://` the key crosses the network in clear text. That is allowed so
  that local servers work, and it is worth reserving for a server on your own machine.
- **Every tool call is audited**, allowed or denied, one JSON line each, with the identity that
  made it, the policy reason and the outcome — see *Where your data lives*. Arguments are recorded as a SHA-256 digest plus
  a redacted copy that keeps paths and replaces file content with its size.

## Troubleshooting

### Windows

- **`pnpm` is blocked in PowerShell.** Corepack installs a `pnpm.ps1` shim; the default
  execution policy refuses it (`UnauthorizedAccess` / *l’exécution de scripts est désactivée*).
  That is Windows, not Aegis. Either call the cmd shim, which PowerShell will not wrap:

  ```powershell
  pnpm.cmd tauri dev
  ```

  or, once per user, allow local scripts (the usual developer setting):

  ```powershell
  Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
  ```

- **WebView2 missing.** Aegis renders through WebView2, which ships with Windows 11 and current
  Windows 10 but is not guaranteed. If the window opens blank or the app refuses to start, install
  the *Evergreen WebView2 Runtime* from Microsoft. Installers built by `pnpm tauri build` are
  configured to download it automatically when absent.
- **`Failed to unregister class Chrome_WidgetWin_0. Error = 1412` on quit.** Chromium, not Aegis.
  WebView2 registers several HWNDs under that class; the first destructor calls `UnregisterClass`
  while siblings are still alive (`ERROR_CLASS_HAS_WINDOWS`). Chrome itself prints the same line.
  It is stderr from the runtime after `quit requested`, not a panic and not a failed shutdown.
  Harmless; there is nothing in this process to unregister.
- **SmartScreen warning on a built binary.** Expected. The MVP does not code-sign, so
  "Windows protected your PC" appears for unsigned output. Signing is out of scope.
- **Antivirus breaks the Rust build.** Real-time scanners — ESET especially — lock the small
  binaries `rustc` and `cargo` produce constantly. Symptoms are `rustup` looping on
  `retrying renaming …`, a corrupted toolchain, or builds that take minutes instead of seconds.
  Add performance exclusions for `%USERPROFILE%\.rustup\`, `%USERPROFILE%\.cargo\` and this
  repo's `src-tauri\target\`.
- **`.cmd` shims via `shell_exec`.** `pnpm`, `npm` and `yarn` on Windows are `.cmd` files, which
  `CreateProcess` cannot launch directly. Aegis resolves the program through `PATHEXT`, so a bare
  `pnpm` finds `pnpm.cmd`, and the launch goes through `cmd.exe` with the arguments escaped for
  `cmd`'s own parser by the Rust standard library. This is a launcher detail, not a shell: your
  arguments are still not a command line, and nothing splits or joins them.
- **`echo` and `dir` are not programs.** They are `cmd.exe` builtins, so `shell_exec` cannot find
  them on `PATH` and says so, naming the builtin. Run `cmd` with `["/c", "dir"]` if you want one —
  and note that `cmd` then parses those arguments itself, which the direct path does not.
- **Colour and progress bars are stripped.** A command that writes ANSI escape sequences is
  talking to a terminal; the transcript is not one, so they are removed from both what the model
  reads and what you see. Cursor movement goes with the colour — otherwise every frame of a
  `cargo` or `pnpm` progress bar would stack up in the pane instead of overwriting itself.
- **Accented output from `cmd`, `dir` and friends.** Windows console programs write the system
  OEM code page (850 on a French install, 437 on a US one), not UTF-8. Aegis tries UTF-8 first —
  `git`, `cargo` and `node` all emit it — and decodes through that code page when the output is
  not valid UTF-8, so `numéro` stays `numéro` instead of turning into `num?ro`.
- **Stopping a command stops its children too.** Killing a process on Windows does not kill what it
  started, and every `.cmd` shim runs under a `cmd.exe` that is not itself doing the work — so Stop
  and the deadline kill the whole tree via `taskkill /T`. On macOS and Linux only the process Aegis
  started is killed; a shell script that spawns a build and waits for it can leave the build
  running. Process groups would fix that and are not in the MVP.

### macOS

- **Screenshots return a black or desktop-only image.** The app needs Screen Recording consent,
  which macOS only prompts for on the first capture attempt. Grant it under *System Settings →
  Privacy & Security → Screen Recording*. Under `pnpm tauri dev` the consent is attached to the
  dev binary and is re-evaluated whenever that binary changes, so it can need re-granting after a
  rebuild. macOS reports the refusal by *succeeding* and handing back a blank frame rather than
  by failing, so Aegis checks for one and answers `E_SCREEN_PERMISSION` with that path in the
  message. The half-refused case — your wallpaper but none of your windows — is indistinguishable
  from a tidy desktop and is **not** detected: if a capture comes back showing nothing but the
  desktop, check the consent.
- **Keychain re-prompts after every rebuild.** Each dev build changes the ad-hoc signature, which
  invalidates the keychain ACL. Use the `AEGIS_API_KEY` environment variable during development;
  Settings will honestly report `key_source: "env"`.

### Linux

- **The window is blank on Wayland + NVIDIA.** A known WebKitGTK DMA-BUF renderer bug. Aegis
  sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` itself on Linux when that variable is unset. Export
  it to another value before launch if you need the DMA-BUF path on a machine where it works.
- **No tray icon.** The icon needs the *runtime* library `libayatana-appindicator3-1` (the
  `-dev` package is only for linking). GNOME additionally needs the AppIndicator shell
  extension. A missing `.so` used to panic at startup (`Failed to load ayatana-appindicator3`);
  it now logs and the window is the only surface — closing it quits, because there is nothing
  to come back from. WSL2 Ubuntu typically has no indicator host; that is expected.
- **Screenshots on Wayland.** Wayland blocks direct framebuffer capture by design. Aegis still
  attempts one — compositors built on wlroots answer a `wlr-screencopy` request, and those work
  — and when the compositor refuses, the failure is `E_SCREEN_PERMISSION` naming the session
  type, never a black image passed off as your screen. GNOME and KDE under Wayland are the usual
  refusals. X11 sessions work.
- **The build wants the screen-capture crate's libraries, not just WebKit.** `xcap` links
  PipeWire, XCB, Wayland, EGL and GBM even on a machine that will never take a capture. A
  compile that dies at `unable to find library -lgbm` (or `-lEGL` / `-lwayland-client`) is
  missing `libgbm-dev`, `libegl-dev` and `libwayland-dev`. See the apt block under
  *Requirements*.
- **`pkg-config` cannot find `libsoup-3.0` or `webkit2gtk-4.1`.** Install the `-dev` packages,
  not only the runtime `.so`. WebKitGTK 4.1 links soup3; `libwebkit2gtk-4.0-dev` / soup2 is the
  Tauri 1 stack and will not satisfy this crate. See the apt block under *Requirements*.
- **WSL2: an Aegis (or penguin) icon in the Windows taskbar and no window.** WSLg created a
  RAIL surface; that is not the same as a painted GTK window. Two things used to make it
  worse: AppIndicator loads, WSLg maps the *tray* as the taskbar icon, and `center: true`
  can park the real window off the X11 screen. `DRI3 error: Could not get DRI3 device` means
  there is no GPU for WebKit to draw with. Aegis now skips the tray under WSL, pins the
  window after the compositor maps it (not at `(0,0)` during setup), and — unless you
  already exported them — sets X11, software GL, and disables the WebKit bwrap sandbox
  *before* GTK starts. Rebuild and relaunch. The log should contain `WSL: skipping the tray`,
  then `WSL: delayed raise after compositor map` and a `main window` line with `visible`,
  size and a position that is not `(0, 0)`. If `visible=Some(true)` and you still see
  nothing, Alt+Tab or click the taskbar icon. Confirm WSLg itself with `xeyes` from
  `x11-apps`. The Secret Service D-Bus warning is expected; use `AEGIS_API_KEY`. This is
  still not the Phase 10 walkthrough. *Hide to tray* is omitted: there is no
  AppIndicator host on the Windows taskbar, and hide would leave a process with
  no window and no way back. Close the window or *Quit*.
- **No keyring.** Without a running Secret Service (gnome-keyring, kwallet), use the
  `AEGIS_API_KEY` environment variable. This is normal on headless and minimal window managers.

## License

MIT
