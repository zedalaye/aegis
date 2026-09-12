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

> **Status: Phase 19 (the first two domain packs) — the MVP is
> feature-complete, and the post-MVP sequence of `PLAN.md` § 7.3 has started.** The app boots,
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
> every message with what the runtime sent it, and **`/write`**, **`/run`**, **`/capture`** and
> **`/remember`** make it ask for a file write, a real command, a real screenshot and a real
> memory, so the whole gate can be walked through without spending a token or configuring a
> provider.
>
> **The audit log has a window.** The audit-log button in the title bar opens a drawer beside the
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
> creates `.aegis/` in the folder you picked, holding `briefs/`, `status/`, `artefacts/` and
> `decisions/` — only what is missing, never overwriting anything you already have. Once they exist, every request carries
> what `STATUS.md` says and the recent end of `DECISIONS.md`, plus the *names* of your briefs and
> artefacts, capped so a long ledger cannot eat the context window. Asking for a decision to be
> recorded writes `.aegis/decisions/DECISIONS.md` through the ordinary approval dialog: no new tool, no
> privileged path, no hidden store beside your folder. See *Shared workspace files*.
>
> **A session now runs as an identity.** *Settings → Identities* creates one: a name, a line
> saying what it is for, instructions it carries into every request, and — the part that matters
> — a tick-list of the tools it may use. Open a session as it from the **+** next to
> **Sessions**, and that session is bound to it for good. An identity is not shown the tools it was
> not granted, so a "reviewer" with only `fs_list` and `fs_read` never asks to write a file; and
> if it asks anyway, policy refuses before anything runs, with no dialog offering to let it
> through. The audit line names the identity, so *who ran this* is answerable afterwards. With
> only the built-in **Assistant**, the + creates a session as it — the assistant Aegis had
> before identities existed, now with a name. See *Identities*.
>
> **And an identity can follow a runbook.** A skill is a `SKILL.md` — when to use it, the tools
> it will call, the steps, how to check the result, and what to do when the source it needs is
> missing. Aegis creates one in your library on first run and one in each workspace you set the
> shared files up in. What every request carries is the *catalog*: one line per runbook the
> identity may run. The steps are loaded only when it says `skill_run`, for that reply, so twenty
> procedures cost twenty lines of context rather than twenty procedures. A run grants nothing —
> every step is an ordinary tool call through the same dialog — and a runbook calling a tool the
> identity does not hold is refused before its first step rather than halfway through. The run
> ends with a status object that is checked, not believed: a `done` naming a file that is not on
> disk comes back refused. Every audit line in between carries the skill's name. See *Skills*.
>
> **An identity now remembers things, and a long session stops paying for its whole history.**
> A memory is one sentence — a **preference**, an **exception** or a **convention** — belonging
> to one identity and reaching the top of every reply that identity gives, in this session and
> every session after it. The model can record one and is asked first, with the sentence itself
> in the dialog; it can search what it holds; it **cannot delete one**, because correcting a
> memory is yours. *Settings → Memory* is where you read, add and forget them. And when a
> conversation gets long, its older turns fold into a few lines of **state** — the goal, the
> files written, the decisions filed, the blockers left open — while the last few turns stay
> word for word. Nothing is deleted: your transcript stays exactly as it was and the pane still
> scrolls through all of it; what changes is only what the model carries. **Compact** in the
> session header does it now; otherwise it happens on its own once a transcript gets expensive.
> See *Memory* and *Compaction*.
>
> **One identity can now hand work to another, and a clock can start one.** A *handoff* is a
> brief out and a report back — goal, inputs as paths, definition of done — never a transcript to
> read; a delegated run opens in the sidebar as an ordinary session with a `brief` badge, under
> its own identity and the same approval gate. A *routine* fires one granted runbook, as one
> identity, on a clock or when a folder changes, and it runs whether or not this window is open.
> Nobody is watching a scheduled run, so it is never asked anything: what it may do is exactly
> what you signed on the routine, and everything else is refused rather than parked on a prompt
> you cannot see. See *Handoffs* and *Routines*.
>
> **And now there is a board.** *Board* in the title bar answers, for the open project, *who ran,
> what did it cost, and why did it fail* — without opening a chat. Three columns: what wants a
> person, what is running, what stopped short — half of it read structurally out of your own
> `.aegis/status/STATUS.md`, half of it what the runtime can see for itself, with every line saying
> which. Underneath, every run in the recent log: a delegation with its specialists, a morning's
> firing of a routine, a runbook, a conversation — each with who ran it, what it spent, what it
> left on disk, and the audit lines it is replayed from. Nothing there writes: correcting the
> board means editing `STATUS.md`, in your editor or through the same approval dialog as any
> other change to your files. See *The board*.
>
> **And Aegis can now use tools it did not write.** A *connector* is an external MCP server — a
> program on your machine that Aegis starts and asks for a list of tools. Those tools reach the
> model as `<connector>__<tool>` and go through exactly the pipeline `fs_write` goes through:
> offered only to identities that hold them, judged by the same table, audited on the same log.
> The one row that is different is the one that matters — **every connector call is put to you**,
> every time, with no auto-allow and no read-only exemption, because what a program somebody else
> wrote does with its arguments is not something this runtime can check. Allowing one for the
> session covers that one tool and nothing else the connector offers, including anything it adds
> later. Adding a connector starts a program, so only you can do it: there is no tool that
> installs one, and granting its tools to an identity is a second act, on the identity. See
> *Connectors*.
>
> **The first domain pack is three files, and none of them is in the runtime.** `review.diff`,
> `deploy.draft` and `alert.draft` are runbooks in your library — review a range before it goes
> to a client, draft the deploy somebody else runs, turn an alert into a note and a reply nobody
> has sent. Aegis did not learn about clients, forges or hosting to get them: no command, no
> tool, no row of the policy matrix changed, and an install that does no client work has three
> folders it can delete. Each one stops one step short of the thing that cannot be taken back —
> the merge, the deploy, the sent reply — because that step is yours. Nothing is granted by
> being seeded: they reach a model when you make an identity and tick them. See *The delivery
> pack*.
>
> **The second pack reads mail and never sends any.** `mail.triage`, `thread.recap` and
> `reply.draft` turn a message that arrived as a file into a ticket, work out what a thread
> actually agreed, and write the answer you send. Not one of the three declares `shell_exec`, so
> the identity holding them cannot run a command — which is what you want of the one pointed at
> text strangers wrote. A message asking for money to move or for access comes back to you
> whatever it says, and the reply is a file with a line under every claim saying which file it
> came from. See *The intake pack*.

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

1. **Add a project.** The **+** next to *Projects* opens the native folder picker. The folder
   you choose is the workspace: the only place a tool may touch without asking every time. The
   path sits in the title bar; the folder button next to it opens that folder in Explorer,
   Finder, or the desktop's file manager.
2. **Start a session** and send anything. The reply streams in a token at a time; the header
   names what produced it.
3. **Make it ask.** Send a message containing `/write`, `/run` or `/capture`. The prompt names
   the exact file and content, the exact program and arguments, or the display and its size.
   Answer **deny** once to see a refusal land in the transcript without killing the turn, then
   send it again and **allow once**.
4. **Check the record.** Open the audit log from the title bar. Both calls are there — the refused
   one and the allowed one — with the policy's reason and the outcome.
5. **Close the window.** The app stays in the tray; the tray icon brings it back. The quit
   button in the title bar is the only thing that ends it.
6. **Restart.** The project, the session and the transcript are where you left them.
7. **File a decision.** Press *Set up shared files* in the sidebar, then ask for a decision to be
   recorded. It is written to `.aegis/decisions/DECISIONS.md` through the same approval dialog, and the
   next reply already knows about it. See [Shared workspace files](#shared-workspace-files).
8. **Open a session as someone narrower.** In *Settings → Identities*, make a **Reviewer** with
   only `fs_list` and `fs_read` ticked. Back in the sidebar, press **+** next to *Sessions*,
   pick that identity, then send `/write`. No prompt appears: the write is refused outright
   because that identity does not hold `fs_write`, and the audit line records the refusal against
   it. See [Identities](#identities).
9. **Run a skill.** *Settings → Skills* lists the runbooks Aegis found. Edit an identity, click
   `never-send-without-review` under **Skills** — `skill_run` and `skill_return` tick themselves
   — and save. Open a session as it and send `/skill`. It takes two rounds: the runbook is
   loaded, then the run is closed with a `blocked` status, because there is no model behind the
   scripted provider to carry the steps out. Both lines are in the audit log with the skill's
   name on them. See [Skills](#skills).
10. **Make it remember something.** Send `/remember`. The prompt shows the exact sentence — this
    is the one call that touches nothing on your machine and still asks, because a memory reaches
    the top of every later reply. Allow it, send anything else, and the reply now opens with what
    it knows. *Settings → Memory* is where you correct or forget it; there is no tool that can.
    See [Memory](#memory).
11. **Hand work to someone else.** Make a second identity — a **Scribe** with `fs_read` — and
    give the one you are talking to `handoff_delegate`. Send `/delegate Scribe`. The prompt names
    the owners and what each is being asked for; allow it, and two sessions open under Scribe and
    run at the same time. What comes back into your transcript is a board of statuses, not their
    conversations, and both of their sessions are in the sidebar with a `brief` badge if you want
    to read them. See [Handoffs](#handoffs).
12. **Put something on a clock.** *Settings → Identities*: give an identity `fs_read`, `fs_write`
    and a runbook — `never-send-without-review` will do. Open a session as it and send `/skill`
    once, and watch the run: that watching is what the next step checks for. Now *Settings →
    Routines* → **New routine**, pick that identity and that runbook, every 5 minutes, tick *write
    files inside the workspace*, and save. Press **Run now**: a session opens with a `routine`
    badge, the runbook is loaded, a line is written into `.aegis/status/`, and the run closes with a
    status — with no dialog, because you signed for that write when you saved. Untick the approval
    and run it again: the write is refused instead of queued, and the row says `blocked`. Close the
    window; it keeps firing. See [Routines](#routines).
13. **Fold a long session.** Press **Compact** in the session header. Once a conversation has
    more than a few turns, the older ones become a few lines of state — goal, files, decisions,
    blockers — marked in place with *What it kept* beside it. Your transcript is untouched; only
    what the model carries changes. See [Compaction](#compaction).
14. **Ask what happened.** Press **Board** in the title bar. Everything above is on it: the
    routine's refused write under *Blocked* saying why, the delegation as one run naming both
    specialists, the conversation you had, each with what it spent. Open one and you get the
    audit lines it is replayed from — the same rows the drawer draws, because they are the same
    lines. Nothing on that page can be edited, which is the point of it. See
    [The board](#the-board).
15. **Give it a tool this build did not write.** Needs Node. *Settings → Connectors* → **Add
    connector**: name it `Local files`, id `files`, program `npx`, and three arguments one per
    line — `-y`, `@modelcontextprotocol/server-filesystem`, and a folder you do not mind it
    seeing. Save it; the row goes *connected* and fills with the tools that server offers, each
    under a name like `files__read_text_file`. A session opened as the built-in **Assistant** can
    call them straight away; any other identity has to be granted them first, one at a time, under
    *Identities*. Ask for one and the dialog names the connector, the tool, the server's own
    description of it and the exact arguments — and it will ask again next time unless you allow
    that one tool for the session. See [Connectors](#connectors).

## Point it at a model

Out of the box there is no provider, and replies come from a scripted one that reports what the
runtime actually sent it. To use a real model, open **Settings** in the title bar and pick how to
authenticate:

| | |
| --- | --- |
| **Authentication** | An OpenAI-compatible API key, a **Gemini** AI Studio key, or a login already written by **Claude Code**, **Codex CLI**, or **Grok CLI** on this machine (`~/.claude/.credentials.json`, `~/.codex/auth.json`, `~/.grok/auth.json`). |
| **Base URL** | For an OpenAI-compatible API key: an endpoint stopping where `/chat/completions` would begin — `https://api.openai.com/v1`, `http://127.0.0.1:11434/v1` for a local server, or whatever your gateway exposes. `https://api.anthropic.com/v1` is the one special case, and is described below. For Gemini or a CLI login, leave it empty unless you are overriding the default endpoint. |
| **Model** | The model id, spelled the way that server spells it. |
| **API key** | When authentication is "API key" or "Gemini": saved to the OS credential store. Leave it empty to keep the one already there. |

A CLI login is not an API key. Aegis reads the official file, refreshes the access token if it
has expired, and writes the new bundle back so the CLI keeps working. It presents itself as that
CLI; that is widely done and may sit outside the provider's terms.

**Test connection** sends one sixteen-token completion to the endpoint Aegis actually uses and
reports what came back — which tells a wrong address from a wrong key from a model that server
does not serve. It costs a few tokens; that is the price of an answer you can trust. Clearing the
base URL puts you back on the scripted provider.

Anthropic's own API is the one address Aegis reads as an instruction rather than a URL. Give the
base URL as `https://api.anthropic.com`, a Claude model id such as `claude-opus-5`, and your
Anthropic API key, and the turn goes to `/v1/messages` — the real API — rather than to the
`/chat/completions` compatibility layer on the same host. That layer works, but its documentation
says it drops prompt caching, and a turn without caching re-sends the whole transcript at full
price on every round of every tool loop. The native path asks for the cache instead: the tool
schemas, the system prompt, and the conversation so far are each a point the next round can be
served from. (If your key has access to more than one workspace you may also need to pick one —
Aegis sends no `anthropic-workspace-id` header.)

**Gemini** is a fifth authentication kind, not a URL on the OpenAI-compatible path. Pick
"Gemini (Google AI Studio key)", paste an `AIza…` key from
[Google AI Studio](https://aistudio.google.com/apikey), and leave the base URL empty unless you
are pointing at a proxy. Aegis talks to `generativelanguage.googleapis.com` in Gemini's own
dialect (`x-goog-api-key`, `functionResponse.name` is the tool name). It is the same single
provider as the others, not a roster: switching to Gemini overwrites the key in the credential
store, the same as switching any other kind.

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
cd src-tauri && cargo test     # Rust: persistence, policy, tools, audit, wire protocol, turns,
                               # skills, memory, compaction, handoffs, routines, the board,
                               # connectors
cd src-tauri && cargo clippy --all-targets -- -D warnings
pnpm typecheck                 # TypeScript, strict
```

The connector tests spawn a real MCP server over real pipes, and the server is the test binary
itself: `tests/mcp.rs` carries an `#[ignore]`d test that speaks newline-delimited JSON-RPC on
stdin and stdout, and the other tests in that file re-enter `current_exe()` with a filter that
selects it. A Node or Python server would have made the suite depend on a runtime that is not on
every machine; a second `[[bin]]` would have shipped a mock MCP server inside the application.

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

Projects, sessions, identities, memories, routines, connectors and settings are stored as seven
small JSON documents — `projects.json`, `sessions.json`, `agents.json`, `memories.json`,
`routines.json`, `connectors.json` and `settings.json` — under the application-data directory:

| | Path |
| --- | --- |
| Windows | `%APPDATA%\dev.aegis.harness\` |
| macOS | `~/Library/Application Support/dev.aegis.harness/` |
| Linux | `~/.local/share/dev.aegis.harness/` |

`projects.json` holds names and workspace paths. `sessions.json` holds your conversations — the
messages you sent, the replies, the tool calls each turn made, which identity the session runs
as, and what each finished turn spent in tokens, which is where the counters on the board come
from. `agents.json` holds the identities you have made; the built-in one is not in it, because
it is a constant in the runtime rather than a record you could delete. `memories.json` holds what
each identity has learned, each record naming the identity it belongs to — deleting an identity
takes its memories with it, since nothing else can reach them. `routines.json` holds what is on a
clock: which runbook, as which identity, in which project, what it was signed to do unattended,
and how many of today's runs it has spent — the ledger is on disk because a scheduler that forgot
what it had spent when the process died would have no ceiling at all. `connectors.json` holds
the external MCP servers you installed: the id, the program, its arguments and the *names* of the
environment variables it needs — never their values, and never whether it was running.
`settings.json` holds the base URL and the model id. **None of them holds a key**, and none
records whether anything is *running*: a session interrupted by a crash or a power cut comes back
idle, because there is no turn left to finish it, and a connector that was connected when the
process died is not connected now.

Your API key is not in this directory at all. It goes to the operating system's own credential
store, under the service name **Aegis** and the account **provider-api-key** — Credential Manager
on Windows, Keychain on macOS, a Secret Service on Linux — where you can inspect or delete it
without Aegis. Set `AEGIS_API_KEY` in the environment instead and Aegis uses that; the credential
store wins when both are present.

All seven documents are meant to be readable and are safe to edit by hand while Aegis is closed.
A document Aegis cannot parse is renamed to `<name>.corrupt-<timestamp>.json` and the app starts
with an empty list rather than refusing to open. Deleting a project forgets it and its sessions;
the workspace folder itself is never touched.

Beside them, `skills/` holds your runbook library: one directory per skill, each with a
`SKILL.md` in it. Aegis puts twelve there — `never-send-without-review`, `cos.loop`,
`world.draft`, `world.perceive-delta`, `world.verify`, `world.check`, the delivery pack's
`review.diff`, `deploy.draft` and `alert.draft`, and the intake pack's `mail.triage`,
`thread.recap` and `reply.draft` — and offers each **once**, recorded by name in
`skills/.seeded`. Delete one and it stays deleted, because a library is yours; a later version
adding a thirteenth will offer that one and leave the rest alone. A workspace's own runbooks
live in that workspace instead, under `.aegis/skills/`, and travel with it. See *Skills*.

Beside them, `captures/` holds the PNGs `screen_capture` writes — one file per approved capture,
named `capture-<UTC timestamp>-<random>.png`. They are here rather than in your workspace on
purpose: a capture is Aegis' own artefact, and one written into a project folder would end up in
your next commit. Nothing ever deletes them; the folder is yours to empty, and a transcript that
refers to a capture you have deleted says so instead of showing it. This is also the **only**
directory the window is allowed to read a file from, and only through Tauri's `asset:` protocol,
scoped to it at startup — the WebView has no filesystem permission of any kind.

Beside it, `audit.jsonl` records one JSON line per tool call — every call, whether it ran, was
refused or failed. It is append-only and plain text, so `tail -f` works and you do not need Aegis
running to read it. Each line names the session, the identity it ran as, the skill run it was
part of when it was part of one, the routine that started it when a clock did, the tool, why
policy decided what it did, and what came of it. It records the *paths* a call touched — including the ones a runbook or a brief named as
what it produced, which is what lets the board say afterwards what a run left behind — but never
the contents of a file: a log that quoted every `fs_write` would become the one place on your
machine where everything the agent ever wrote is collected in plain text. It does not record
tokens either, and for a different reason: they are spent by a model round rather than by a tool
call, so they are counted on the session instead (see *The board*). A line for a capture carries the file's path, its
pixel size and a SHA-256 of the bytes on disk — enough to say later which capture a call
produced, and to check that the file is still that one, without the log holding a copy of it.
The **Audit log** drawer in the title bar reads the tail of this file — the last 200 lines — and
is the same information in a window; the file is the record, and nothing in the UI writes to it.
Aegis never rotates or trims it; deleting it is yours to do, and a new one starts on the next
tool call.

---

## Where commands run

By default a command runs **here** — in this process, on this operating system. `shell_exec`
looks the program up on your PATH and spawns it, and on Windows that is the Windows toolchain.

That is the wrong toolchain for one common arrangement: a Windows machine whose software projects
live in a WSL distribution. Opening the code folder already works — `fs_read`, `fs_write` and
`fs_list` see those files over `\\wsl$\…`, or under `C:\` which WSL mounts at `/mnt/c` — but the
`git`, the `node`, the `cargo` and the test runner the repository is actually built with are in
the distribution, not on Windows. Running `pnpm test` on the Windows side of that folder is not
the same command.

So a project can name an **execution host**. The picker is *Commands run in* in the sidebar, and
it lists this computer plus whatever `wsl -l -q` reports. It is Windows-only, and the list is
asked for each time you open a project rather than remembered, because you install distributions
in a terminal and this window is never told.

**Nothing is inferred.** A project has no host until you pick one. Opening a folder under
`\\wsl$\` does not set one — picking a folder is picking a folder — and a finance or watch
workspace never gets a distribution. This repository's own default is unchanged.

Once a project has one, exactly one thing changes:

- `shell_exec` runs `wsl -d <distro> --cd <dir> --exec <program> <args>`. There is still no
  shell: `--exec` is what keeps the arguments a vector, so pipes, `&&`, globbing and variable
  expansion are as absent as they were before.
- PATH, HOME and the user are the distribution's. `pnpm` is the Linux binary, not `pnpm.cmd`;
  PATHEXT and the `.cmd` shims apply only when the host is this Windows process. Nothing is run
  as root, and `-u root` is not passed.
- The approval dialog names the distribution, shows the directory **in it** the command will
  start in, and keeps the Windows spelling of the same folder beside it. *Allow for this session*
  is still keyed on the program — `git` is `git` — and never on `wsl.exe`, which is not a program
  you are ever asked about. The model cannot call `wsl`: wrapping is the runtime's, the same way
  launching a `.cmd` shim through `cmd.exe` is.
- **Stop** and the two-minute deadline end the Linux process, not only the relay that launched
  it. A `cargo build` left running in the distribution after you pressed Stop would be a Stop
  button that lied.
- The model is told, in the system message, which distribution it is in and what the workspace's
  path is from inside it — otherwise its first move is to pass a `C:\` path as an argument.

**The file tools do not move.** Containment is still the Windows-canonical workspace, `fs_read`
of a file in that folder still goes through Windows, and a capture is still this display. There
is no second filesystem here, only a second way to spell the same one.

**It never falls back.** A distribution that is not installed, a WSL service that will not answer,
or a folder the distribution has no path for fails with `E_EXEC_HOST` before anything is spawned
— it does not quietly run the command on Windows instead, which would be the wrong operating
system reporting success. The working directory is probed inside the distribution first, because
`wsl --cd` answers a directory it cannot find by starting in `/` and saying nothing: a command
approved for one folder must not run in another.

Clearing the host puts the project back on this computer. Sessions inherit whatever the project
says and cannot override it — which operating system the toolchain lives in is a fact about the
folder, not about a conversation held over it. Routines and delegated runs inherit it too, so an
unattended run at 03:00 uses the same toolchain as the one you watched.

What this is **not**: a second Aegis, a second agent loop, a Linux VM this app manages, or a way
to run Aegis itself under WSLg. It is where `shell_exec` lands, and nothing else. `docker` inside
the distribution is an ordinary `shell_exec` once the host is set. `PLAN.md` § 7.12.

---

## Shared workspace files

Aegis' own data is above. This is the other half: files that live in **your** workspace folder,
not in Aegis' application-data directory, and that the agent reads at the start of every reply.

The convention is five directories, all inside one — `.aegis/` at the top of your folder:

| | Holds |
| --- | --- |
| `.aegis/briefs/` | one file per delegated piece of work — goal, inputs as *paths*, definition of done |
| `.aegis/status/` | `STATUS.md`: what is true right now. A board, rewritten in place, not a log |
| `.aegis/artefacts/` | what was produced — a draft, a report, an export, a patch |
| `.aegis/decisions/` | `DECISIONS.md`: one entry per decision, newest last |
| `.aegis/skills/` | this project's own runbooks, one directory each. Seeded with `inbox.triage` |

**One directory, not five.** These are the **cabinet**: in-flight work, rewritten every turn, the
harness's working surface over your project. Five of them beside your `src/` would be five things
you did not ask for, so they go in one, named after the tool whose working surface they are. The
one thing that does *not* live there is [`world/`](#the-world), which stays at the root because it
is the project rather than the tool's view of it.

The leading dot is a tidiness convention, not a hiding place — the same one `.github/` and
`.vscode/` use. Nothing about it is privileged: these are ordinary files under your workspace root,
reached by the same `fs_read` / `fs_write`, through the same approval dialog, on the same audit
log, and committed with the repository. Two costs worth knowing before you go looking: macOS Finder
hides dot-directories until you press ⌘⇧., and `rg` skips them without `--hidden`.

**Set up shared files** in the sidebar creates whatever is missing and seeds each one with a
short template. It never overwrites: a file that is already there is left byte for byte as it
was, and the panel says which files it created and which it kept. Nothing is created until you
press it — a workspace is a folder you already own, usually a repository with its own layout, and
directories should not appear in it because you pointed an app at it. You can equally make
them by hand, or in a terminal; the panel measures the folder rather than remembering what it did
to it. They are ordinary files: commit them, edit them in your editor, `grep` them.

**If you set a workspace up before this layout**, its five directories are still at the root, where
the runtime has stopped looking. The panel names them and stops there — moving somebody's
`DECISIONS.md` for them is exactly the act the button beside it refuses to do. `mv` them into
`.aegis/` and they are found again.

Once they exist, two things change.

**The agent reads them.** Every request carries the current `STATUS.md`, the recent end of
`DECISIONS.md`, and the *names* of what is in `.aegis/briefs/` and `.aegis/artefacts/`. Not `skills/`: those
reach the model as the skill catalog instead, which says what each runbook is *for* rather than
only what it is called. Names, not contents:
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

## The world

The five directories above are the **cabinet**: in-flight work, rewritten every turn. That is the
point of them. A project that has something worth *protecting* — a shape, a set of rules, a
notion of what "right" means — keeps that in a second layer in the same folder, with the opposite
mutation rule.

This one is **not** under `.aegis/`. It sits at the root of your folder, beside `src/`, and the
asymmetry is the whole reason there are two layers: the cabinet is a view *of* your project and
belongs to whatever tool is holding it, while `world/` is the project. It should still make sense
to somebody who has never run Aegis, and it should outlive it.

| | Holds |
| --- | --- |
| `world/essence.md` | what this is, and what it is for |
| `world/schema.md` | the shapes it is made of, as they were perceived |
| `world/behaviours.md` | how it behaves, including the sins a source showed — each with the perimeter it holds inside |
| `world/oracle.md` | how a new instance is known to be right |
| `world/decisions.md` | the decisions that moved the essence. Not `.aegis/decisions/DECISIONS.md`, which is operational |
| `world/sources.yml` | the dumps, exports and logs all of that was perceived from |

**There is no button, and nothing creates it.** A watch folder or a wish list has no essence, and
five empty templates in one are worse than nothing. A world starts when you write
`world/essence.md` — in your editor, or by asking for it and approving the `fs_write` — and the
panel then reports what is there. None of the six files is required; a world that has only an
essence is a world.

Once it exists, four things change.

**Every session in that folder is framed.** A few lines go into the system message: read `world/`
before planning anything, take it as given, do not reopen the declared sources, and if the work
cannot be done without changing what the thing *is*, stop and say which line would have to move.
That last one is an **écart**, and it is a better answer than a plausible thing built on a world
nobody agreed to change. The frame is injected by the runtime rather than left to a runbook,
because a runbook can be skipped and forgetting this is the whole failure it exists to prevent.
What goes with it is *status* — which files the constitution holds, which are not written yet,
what it declares as sources — never `essence.md` itself. The essence stays on disk, where
`fs_read` reaches it in the one turn that needs it.

What the frame says about **writing** depends on which kind of run it is, and matches what the
gate would actually do. A specialist on a brief is told it does not write `world/` and should
return `needs_you`. A session you are sitting in is told the essence is not its to change on its
own initiative — but that when *you* ask for it, that is the decision being made, and `world.draft`
is the runbook for it.

**Nothing delegated can amend it — and you can, with help.** A specialist working on a brief that
calls `fs_write` under `world/` gets a refusal: no dialog, nothing written, and a message telling
it to return `needs_you` with the one sentence naming what would have to move. That is where the
danger actually is, and it is where the hard rule sits.

In a session you are sitting in, amending the world is an ordinary approval at high risk — and it
offers *allow for this session*, because founding a world is six files and six identical prompts
in a row is how anyone learns to stop reading them. What you sign there is its own narrow thing:
**amending `world/` is not covered by a general workspace-write approval, and a world approval
covers nothing else.** The two never stand in for each other. A scheduled run is offered nothing
at all — it cannot ask you, and it cannot be signed for in advance, because changing what a
project *is* is a decision and a routine has nobody in it to make one.

Only the first path segment counts, so a repository with its own `src/world/` module is untouched.

**A source that has been read once is not read again.** `world/sources.yml` declares what this
world was perceived from, with the length and digest each was recorded at:

```yaml
# what this world was perceived from
sources:
  - path: sources/legacy-dump.sql
    bytes: 18234112
    sha256: 3f9a…
  - sources/2026-08-prod.log      # declared, nothing perceived from it yet
```

`fs_read` of one of those, while it still hashes to what was recorded, is **denied** — not asked
about. What it said is in `world/`, and re-reading it is the round-trip the world exists to have
paid once. Paths are resolved against the workspace and one that escapes it is refused, so
`sources.yml` cannot be turned into a reading list for somewhere else.

**A source that has moved holds the work.** Drop a new dump in and the next request says so. Until
that delta is perceived, a brief handed to a specialist will not launch — unless it is the
perceive-delta itself, which names the moved path in its `inputs`. Routing the work and the
re-perception at once is how a world quietly ends up compiled against a schema nobody re-read.
Drift is measured at those two moments and when you open the panel; it is not hashed on the way
into every reply, so the prompt reports drift it has *seen* and never claims there is none.

Four runbooks come with it. **`world.draft`** is the help: point it at a project and it reads what
is there, drafts `schema.md` and `behaviours.md` from evidence it can name, and then *stops* and
asks you the two questions it has no business answering — what this is for, and how you would know
a new version of it was right. Those become `essence.md` and `oracle.md` in your words, quoted
rather than improved. Run it in a session; inside a brief every one of its writes is refused,
which is the same rule as everything else here.

The other three: **`world.perceive-delta`** (bounded to the paths that moved), **`world.verify`**
(the oracle as a program, not a taste review of a diff) and **`world.check`** (read the
constitution and say where you stand). They are in the library like any other, and like any other
they are granted to an identity or not — writing a runbook is still not granting it.

`COS.md` *Work* is the reasoning; `PLAN.md` § 7.2 is how it sits on this tree.

---

## Identities

A session runs as an identity: a name, what it is for, the instructions it carries, and the tools
it may use. **Settings → Identities** is where they are made.

| | |
| --- | --- |
| **Name** | what the session picker shows. Unique, case-insensitively |
| **Role** | one line saying what this identity is for. It is what the picker shows beside the name, and the first thing the model is told about itself |
| **Instructions** | carried into the system message of every request this identity makes. Capped at 2000 characters on purpose — see below |
| **Tools** | a tick-list of `fs_list`, `fs_read`, `fs_write`, `shell_exec`, `screen_capture`, `skill_run`, `skill_return`. Everything unticked is refused |
| **Skills** | the runbooks it may load. Granting one ticks `skill_run` and `skill_return`, because an identity that cannot load a runbook holds a grant that does nothing |

Pick one from the **+** next to *Sessions* and the session is bound to it. That
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
procedure belongs in a skill — a versioned `SKILL.md` loaded only when it is invoked — because a
procedure in the instructions is paid for on every turn whether it is needed or not. See *Skills*.

**The built-in Assistant holds no skills.** It holds every tool, and that is the point of it: it
is the assistant Aegis had before either allow-list existed. Giving it every runbook the moment
one appeared would change what the default identity means under the sessions already using it. A
skill is always something you granted.

---

## Skills

A skill is a **runbook**: a procedure written down once, so nothing has to re-derive it in a
context window every time. `Settings → Skills` lists the ones this install can run.

It is not a tool and it is not a memory. A tool is a verb on the machine. A memory is a
preference or an exception. A skill *sequences* tools toward a criterion for being done, under
whatever the identity was already allowed to do.

### The file

One directory per skill, holding a `SKILL.md`. The directory's name is the skill's name —
`inbox.triage`, `never-send-without-review`: lower case, digits, `.`, `-`, `_`.

```markdown
---
version: 1
tools: fs_read, fs_write
---

# inbox.triage

## When to use it
## Inputs required and tools it will call
## Steps
## How to validate
## What to return
## What requires approval
## What to do if the source is missing
```

**All seven headings are required, in that order**, and nothing else may sit beside them. That is
stricter than markdown needs to be, on purpose: the headings are the contract between whoever
writes a runbook and whoever runs it, and a file that quietly left out *what to do if the source
is missing* would be a runbook whose failure mode is inventing an answer. A file that does not
parse is listed in the panel with the reason rather than hidden — you are the only person who can
fix it — and it is never offered to a model.

`tools:` is not decoration: it is what lets a run be refused *before* it starts.

### Where they live

| Scope | Lives | For |
| --- | --- | --- |
| **Library** | `skills/` beside your projects file | how *you* work — "never send without review" |
| **Workspace** | `.aegis/skills/` inside the project folder | how *this* project works, and it travels with the repo |
| **Per identity** | the identity's allow-list | which of the above that identity may run |

A workspace runbook shadows a library one of the same name; the panel says when that is
happening. Aegis seeds the library with `never-send-without-review`, `cos.loop`, the four
world runbooks and the six of the two domain packs, and each workspace with `inbox.triage` when
you press *Set up shared files* — all of them are examples of the format in the place you would
look for one, and all of them are ordinary files you can rewrite or delete. `cos.loop` is the
Chief-of-Staff loop; see [Handoffs](#handoffs). The `world.*` four are the constitution's; see
[The world](#the-world). `review.diff`, `deploy.draft` and `alert.draft` are one domain pack and
`mail.triage`, `thread.recap` and `reply.draft` are the next; see
[The delivery pack](#the-delivery-pack) and [The intake pack](#the-intake-pack).

Seeding happens **once per name**. The library records what it has offered in `skills/.seeded`,
so a runbook you deleted does not reappear on the next start, and a runbook a later version adds
reaches an install that already has a library. Nothing here grants anything: a seeded runbook is
a file in a folder, and an identity that may run it is an identity you granted it to.

### Catalog in, body on demand

This is the whole shape, and the reason skills are cheap.

Every request carries the **catalog**: one line per runbook the identity may run — the name, the
version, where it came from, when to use it, and what it will call. The **steps are not there**.
The model asks for them with `skill_run` when it is about to follow them, they arrive for that
reply, and the next one does not carry them unless it runs the skill again. Twenty procedures
cost twenty lines, not twenty procedures.

Editing a `SKILL.md` takes effect on the next run; nothing is cached. Press **Re-read** in the
panel after fixing one.

### A skill grants nothing

Three rules, and none of them is inside the runbook:

- **A skill you were not granted is refused before the file is even located**, with no dialog. A
  prompt offering to let an identity exceed its own allow-list is a prompt that should not exist.
- **A runbook that calls a tool the identity does not hold fails closed**, at `skill_run`, naming
  the tool. Not four rounds in, with half a job done in your folder.
- **Every step is an ordinary tool call.** Same matrix, same approval dialog, same audit line. A
  runbook that says "write the file" produces the write prompt you would have got anyway.

### The return

A run ends with `skill_return`, and it is *checked*, not believed — `COS.md`'s handoff shape:

```
status: done | blocked | needs_you
summary:             # five lines max
artefacts:           # paths inside the workspace
evidence:            # a test, a diff, a capture
open_questions:
next_owner:
```

A `done` naming an artefact that is not on disk is refused, which catches the commonest failure
there is: a model describing a file it never wrote. A `done` pointing at nothing at all is
refused too — it could not be checked afterwards or replayed. A `blocked` or a `needs_you` needs
at least one open question, because escalating with nothing to answer is a dead end for whoever
it reaches. A refused return leaves the run open, so the corrected one is still part of it.

Then **every audit line between the two calls carries the skill's name** — not just the two the
runner makes. That is what makes "what did this runbook actually do, and what was it refused"
answerable later.

Why bother: a procedure that lives in a system prompt is paid for on every turn and drifts every
time someone rephrases it; one that lives in a chat is gone at the next compaction. `COS.md` is
the reasoning in full, `PLAN.md` § 7.6 the argument for it being the efficiency layer.

---

## Memory

A memory is **one sentence an identity keeps**, and it reaches the top of every reply that
identity gives — in this session and in every session after it. That is the whole feature, and
it is why the shape is as narrow as it is: something that is carried into every future request
is closer to an instruction than to a note.

A memory is one of three things, and nothing else:

| Kind | For | Example |
| --- | --- | --- |
| `preference` | how someone likes things done | "this client wants everything in French" |
| `exception` | where the usual rule does not apply | "never touch the vendored crate" |
| `convention` | how it is done here | "releases are tagged before the changelog" |

Anything that is none of those is not a memory. A fact about a project is a **file** in that
workspace (`.aegis/briefs/`, `decisions/` — see *Shared workspace files*), and a procedure is a
**skill**. A store that accepted everything would slowly become the transcript it exists to
replace.

Each memory can name **what it rests on** — a workspace path, a ticket, the person who said it.
That is optional, and its absence is shown: a memory with no source is presented to the model as
a hypothesis, not as proof. It is the difference between "you decided this, it is in
`.aegis/decisions/DECISIONS.md`" and "I think I remember this".

### Who may do what

|  | The model | You |
| --- | --- | --- |
| Record one | `memory_write` — **you are asked first** | *Settings → Memory* |
| Read them | in every request, plus `memory_search` | *Settings → Memory* |
| Correct one | — | *Settings → Memory* |
| Forget one | — | *Settings → Memory* |

**There is no tool that deletes a memory**, and that is deliberate rather than an omission.
Deleting is irreversible, and what a delete most often removes is a correction *you* made. An
agent that could quietly retire the memories it found inconvenient would have a memory exactly as
reliable as its judgement on its worst turn. So the model is told, in its own instructions, that
it cannot delete these and should say so plainly when one is wrong — and you correct it.

**A write is asked about**, like `fs_write`, even though it touches nothing on your machine. The
dialog shows the exact sentence that would be remembered — the whole thing, not a preview, since
it is one sentence — and *Allow for this session* covers the rest of the session if you would
rather not be asked each time. Every write is on the audit log either way.

Writing something already held **touches that memory instead of storing a second copy**, and the
model is told it was already known, so it can stop repeating itself. An identity holds at most
200; past that a write is refused with "forget one first" rather than something being quietly
evicted — what would be evicted is as likely to be your correction as the model's guess. The
twenty most recently confirmed reach the prompt; `memory_search` finds the rest.

Memories belong to **one identity**. There is no view of everybody's, no query that spans two,
and neither tool has an argument that names an identity — so a "reviewer" cannot read what a
"scribe" learned. Deleting an identity forgets what it knew, since nothing else could ever reach
it again.

---

## Compaction

A long conversation gets expensive: every reply pays for every turn before it. So the older turns
**fold into state** and the recent ones stay word for word.

What the model carries after a fold is a handful of lines derived from what actually happened:

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

**Nothing here was summarized by a model.** Every line is read off the record — the first thing
you asked, the paths `fs_write` actually wrote, the programs `shell_exec` actually ran, the
status a `skill_return` actually reported. So it costs nothing, it is the same every time, it
cannot invent a file that was never written, and every claim in it can be checked against
`audit.jsonl`. The price is that it is *thin*: it holds what was done, not what was reasoned,
which is exactly why the last few turns are kept raw.

**Nothing is deleted.** Your transcript stays whole on disk and the pane still scrolls through
all of it; a dashed line marks where the fold is, with *What it kept* beside it. What changes is
only what the model is sent. Fold again later and the state is re-derived from the messages — a
session compacted five times is never a summary of a summary.

It happens on its own, once per turn, when a transcript has grown past about 48 KB. **Compact**
in the session header does it now.

What survives a fold, and always did: what the identity **remembers** (above), and the current
state of your **shared workspace files**. Neither of those was ever part of the conversation —
both are rebuilt into every request — so there is no "restore after compacting" step anywhere in
Aegis, and nothing to forget to call. If a fact has to survive ten of these, it belongs in a
memory or in a file, not in the chat. That rule is `COS.md`'s, and this is the machinery that
makes it true.

---

## Handoffs

One identity can hand work to others and wait for what they return. That is the whole of the
Chef-de-Cabinet mode: **a Chief of Staff routes, specialists do the work, and the human decides
anything irreversible** — three roles, and a specialist that started routing would be a second
Chief of Staff.

What travels between them is a fixed object, not a conversation.

```
goal:                 Draft the release note for 0.4
owner:                Scribe
priority:             normal
inputs:
  - .aegis/artefacts/changelog.md
constraints:
  - no marketing language
definition_of_done:   .aegis/artefacts/release-0.4.md exists and names every user-visible change
approval_needed:      the write
return_format:        artefact
```

and every return has the same six fields, whatever it was asked to do:

```
status: done | blocked | needs_you
summary:              five lines at most
artefacts:            paths
evidence:             a test, a diff, a capture
open_questions:
next_owner:
```

### Inputs are paths, never paste

The one rule with teeth. An entry in `inputs` that spans several lines is **refused**, with a
message saying to write the text to a file and name the path instead. Pasted prose in a brief is
how one agent's context ends up inside another's, and then inside the next one's — which is the
failure the whole shape exists to prevent.

### What actually happens

Each brief opens **a session of its own**, bound to the identity it names, and they run at the
same time. Those sessions are in your sidebar with a `brief` badge; you can open one and read
exactly what it did, because it is an ordinary transcript.

A specialist works under **its own** allow-list, not the Chief of Staff's, and its session holds
none of the Chief of Staff's session grants. If it wants to write a file, it asks you — in its
own session, so the sidebar row shows *waiting on you* and you click the row to answer. It
cannot delegate: the tool is not on its list, and it is refused if it asks anyway.

When the workspace has the shared files set up, each brief is also written into `.aegis/briefs/`, so
what was handed out is on disk and in git rather than only in a transcript.

### What comes back

A board. Not a transcript, and not a concatenation of them:

```
2 briefs: 1 done, 1 blocked; review done

--- brief 1 — Draft the release note for 0.4 (Scribe)
brief: .aegis/briefs/3f2a91b8-draft-the-release-note.md
status: done
summary:
  Wrote the note from the changelog; 9 user-visible changes.
artefacts:
  - .aegis/artefacts/release-0.4.md
evidence: —
open_questions: —
next_owner: —
…
```

There is no code path from a specialist's messages to the Chief of Staff's context. That is
structural rather than careful: the only thing a delegated run can produce is a return, and a
return has no field wide enough to hold a conversation.

### When nobody answers

Every attempt is bounded — five minutes, then it is cancelled the way pressing **Stop** cancels
it. A run that ends without returning gets **one** more attempt, in the same session so nothing
it already did is thrown away, and is told that this is the last one. After that the board says
`needs_you` and names the human. Two failures, not twelve creative retries.

An escalation is not an error. The other briefs still ran, and their statuses are on the same
board; a Chief of Staff's job is to route what worked and put the rest on the attention list.

### The loop is a skill, not a personality

`cos.loop` ships in the skill library beside `never-send-without-review`: read the board, update
the attention list, route what is new, retry what is blocked once, ping only when something is
irreversible, ambiguous or on a deadline, write the status, stop. It is a `SKILL.md` in a folder
you own — read it, change it, delete it. It is not baked into any prompt, and no identity runs it
until you grant it one.

### Setting it up

1. *Settings → Identities*: make a **Chief** with `fs_read`, `fs_write` and `handoff_delegate`,
   and grant it the `cos.loop` skill. Make one or two narrow specialists — a **Scribe** with
   `fs_read` and `fs_write`, say.
2. *Set up shared files* in the sidebar, so there is a board to read and a `.aegis/briefs/` to file in.
3. Open a session as the Chief and ask for something that needs both of them.

With the scripted provider, `/delegate Scribe` hands two briefs to `Scribe` and shows the whole
path without a model: the approval dialog naming the owners, two sessions opening, and a board
coming back. See [the walkthrough](#walk-through-it-in-two-minutes).

### One run id over all of it

Every audit line a specialist writes carries the delegation the brief came from, beside the
identity that made the call — so "who ran, under whose brief, and why did it fail" is answerable
from `audit.jsonl` without opening a chat. It is under **Delegation** in the audit drawer's
detail.

---

## Routines

A routine fires **one runbook, as one identity, on a clock** — and it runs whether or not this
window is open. That is what the tray is for: closing the window puts Aegis away, it does not stop
the machine.

*Settings → Routines* is the whole surface. A routine is four decisions and no prose:

```
name:       Morning watch
identity:   Watcher
runbook:    watch.digest
when:       daily at 07:00        (or every N minutes, or when a folder changes)
signed for: write files inside the workspace
budget:     4 runs a day
```

There is no message field, and there is nowhere to put one. **A routine names a runbook.** If it
could carry a paragraph it would be a chat on a timer, which is the one thing `COS.md` says never
to automate.

### The door

A routine may only name a skill that is

1. **live** — a `SKILL.md` that parses, in the library or in that project's workspace. A
   `PROPOSAL.md` is not one;
2. **granted** — ticked on that identity in *Settings → Identities*. Putting a runbook on a clock
   does not grant it;
3. **already run under watch, at least once, by that identity** — checked against `audit.jsonl`,
   which records a `skill_return` for every run that reached its end.

The third is the one with teeth, and it is the whole discipline of skills in one check: write the
seven headings, run it once and watch what it does, *then* put it on a clock. There is no way
round it in the UI, because it is not enforced in the UI.

### Nobody is watching

A scheduled run cannot raise an approval dialog — there may be no window at all, and a prompt
nobody answers is a turn parked until it times out. So policy **refuses** instead of asking, and
what a run may do beyond reading is exactly what you signed when you saved the routine.

Those standing approvals are the same grants the approval dialog creates when you answer *allow
for this session*, in the same words, stored on the routine and dropped when the run ends. Two
things bound them, both checked when you save:

- the runbook has to **declare** the tool under *Inputs required and tools it will call*;
- the identity has to **hold** it.

Anything outside the workspace, and anything under `.git/`, can never be signed for at all — those
are put to a person every time, and a routine is not a person. A run that wanted one of them
returns `blocked` and says what it needed, which is a runbook doing its job.

**Run now** takes exactly that path, unattended and all. What you see when you press it is what
happens at four in the morning, including the refusals.

### Budgets, pauses and silence

Every routine has a daily ceiling, and so does every identity — across all the routines that fire
as it, because three well-behaved clocks on one role can still spend a night writing. The count is
spent before a run opens a session, under the store's own lock, so two ticks cannot both fire the
last one.

A run that ends **without returning at all** is a silence. Two in a row and the routine pauses
itself, with the reason on its row: escalate after two failures, not twelve creative attempts. A
`blocked` is not a silence — the runbook answered.

Every run is cut off after fifteen minutes, through the same cancellation a **Stop** uses, so a
routine that hangs is a failed run rather than a slot occupied forever. At most two scheduled runs
are in flight at once, across all routines.

### What a run leaves behind

An ordinary session, in your sidebar, with a `routine` badge and a full transcript — "what did it
do at seven this morning" is a click. Every audit line it wrote carries the routine's id beside
the identity and the skill, so a week of a watch routine is one `grep` and so is what it cost.

### A trigger, not just a clock

`when a folder changes` watches one directory inside the workspace — `briefs`, `inbox` — by
looking at it on each tick rather than by holding a filesystem watcher. What it compares is the
newest modification time under that directory, **the directories' own included**: a file added,
moved in, rewritten, renamed or deleted all count, because all of them touch the folder even when
no file in it is new. (A file moved or copied in keeps the time it had elsewhere, which is why the
folder's own stamp is the one that matters.) Saving the routine records where the folder stands at
that moment, so a file dropped in straight afterwards is a change; nothing older than that fires
it, and it fires at most as often as the five-minute floor allows. Sources of truth worth watching for real — mail, a
ticket queue, a pull request — arrive as MCP connectors later; a folder is what this process can
already see change.

### Firing and cloning a role

An identity that routines fire as cannot be deleted while they exist: the refusal names how many,
and repointing or removing them is the decision you take rather than one Aegis takes for you.

*Duplicate* on an identity opens a new one with the same perimeter — role, instructions, tools,
runbooks, budget — and **none of its memories**, which belong to the identity that learned them.
It is also how you make a narrower version of the built-in Assistant, which cannot itself be
edited.

---

## The board

**Board** in the title bar answers one question about the open project: *who ran, what did it
cost, and why did it fail* — without opening a chat.

It is two things stacked. The **board** is three columns; the **runs** underneath it are every
piece of work in the recent log, newest first.

### Attention, in flight, blocked

The three columns are the ones `COS.md` gives a Chief of Staff, and each has one meaning:

| | Holds |
| --- | --- |
| **Attention** | somebody has to do something: an approval on screen, a run that came back `needs_you`, a routine that gave up and named a human |
| **In flight** | something is running right now |
| **Blocked** | something stopped short and is *not* waiting on a person: a runbook that could not find its source, a routine that cannot fire as it stands, a run that failed |

`needs_you` is Attention and not Blocked on purpose. The difference is who has to move next,
which is the only thing a board is read to find out.

Each column is filled from **two places**, and every line says which it came from.

**Your `.aegis/status/STATUS.md`** is the half no runtime can know: a client who has not answered, a
decision waiting on a meeting, work that is late. Aegis reads it structurally — the three
headings, and the lines under each — rather than showing you the file. Bullets and plain lines
both count; an indented example block does not, and neither does a whole line of italics, which
is how the seeded file says a column is empty. A heading it does not recognize simply ends the
section above it, so a `STATUS.md` with extra sections in it loses nothing and invents nothing.

**What this process can see for itself** is the other half: which session has a turn running,
what a dialog is waiting on, which clock stopped itself after two silent runs, which routine
cannot fire because its runbook was un-granted. None of that is in the file, and none of it
should be — a board Aegis rewrote would stop being yours.

Nothing in this window writes `STATUS.md`. Correcting it means editing the file, in your editor
or by asking the agent — which is an ordinary `fs_write` through the approval dialog, on the
audit log, exactly like filing a decision. There is no "mark as done".

### Runs

A **run** is one piece of work, wherever it happened. Aegis folds them out of the tail of
`audit.jsonl` using ids that have been on every line since the phase that introduced them, so
nothing new is recorded to make this work:

| A run is | Held together by | So one row is |
| --- | --- | --- |
| a **delegation** | the handoff id | the Chief of Staff's own call *and* every call every specialist made under its briefs — in their own sessions, under their own identities |
| a **routine's firing** | the routine, in the session it opened | one morning's run, not the routine's whole history |
| a **runbook** | the skill name, in the session that ran it | one `skill_run` to its `skill_return` |
| a **conversation** | the session | everything else you did in it |

The widest id wins, which is the containment order: a specialist running a runbook under a brief
is one delegation, not two rows.

Each row says how it ended in the run's own words — `done`, `blocked`, `needs you`, `failed`, or
just `ran`. Three rules decide that word, and they are worth knowing:

- **A report wins over a refusal inside it.** A runbook that was refused a write, coped, and
  returned `done` **is** done. The refusal is on the row as a number, not as a verdict.
- **A conversation cannot fail.** Denying a call in a chat is your own answer and the turn
  carries on by design; a board that read that as a failed conversation would be calling the
  approval gate working as intended a problem. What went wrong inside one is on the row as counts
  and in the replay line by line.
- **A brief, a runbook or a firing that never reported has failed** — a silence is not an answer
  — unless the session is still running, which is the one thing on the board that comes from this
  process rather than from the record.

Opening a row adds who ran it, which tools it called and how often, what it left on disk, and
what it spent.

### Replay

Opening a run's replay shows **the audit lines themselves**, oldest first — the same rows the
audit drawer draws, because they are the same lines. Nothing is rewritten into a narrative: you
are reading the record, and a record prettied up first would be worth less than the file.

The artefacts a run names are paths — files written, captures taken, and whatever a report
listed as its output. Paths only. The log has never held the contents of a file and does not
start now.

### What it cost

Tokens are counted per **turn**, on the session, as the provider reports them — and they are
deliberately *not* on the audit line. A tool call is not what spends tokens: a model round is,
several calls come out of one round, and a turn that called no tool at all still costs. A counter
built from the log would silently omit every reply that only talked, which is most of them. So
the run's cost is a join: the log says which turns a run made its calls in, and the session says
what those turns spent.

Two consequences worth knowing.

Aegis **asks** for the count: every streaming request carries
`stream_options: {"include_usage": true}`, because an OpenAI-compatible endpoint generally sends
its usage chunk only when asked. If a server still sends none, that turn is recorded as
**unknown** rather than as zero, and every total it is part of reads *at least* — a floor, not a
total. If a server refuses the field outright, the next message fails with a message naming it,
and **Test connection** in Settings says the same thing; it is not a silence.

Every turn is charged to exactly one run, so the runs of a session add up to the session, and the
sessions of a project add up to the total under **Runs**. A conversation that only talked still
gets a row, with no calls and its full cost.

The same number, for one conversation, sits beside the model in the chat header.

### What it is not

The board is a window on the log, not a second copy of it. Runs older than the tail Aegis reads
are in `audit.jsonl` and not here — the file is the record, and `tail -f` still works. Deleting a
session removes it from the board while its lines stay in the log: the log says what was done,
the board is a view of one project's work, and it cannot invent a project for a conversation that
no longer names one.

---

## Connectors

A **connector** is an external MCP server: a program on this machine that Aegis starts, speaks
newline-delimited JSON-RPC to over its stdin and stdout, and asks for a list of tools. Those
tools then reach the model in the same `tools` array as `fs_read` and `shell_exec`, under the
name `<connector-id>__<tool>` — `files__read_text_file`, `git__status`.

*Settings → Connectors* is the whole surface. **There is no tool that installs one**, and there
will not be: adding a connector names a program to start, and a model that could name a program
to start would have `shell_exec` with the dialog taken off it.

### Adding one

Give it a name you will recognize, an **id**, the **program**, and its **arguments one per
line**. The id is a namespace rather than a label — it is the part before the `__` in every tool
the connector offers — so it is lower-case letters, digits and hyphens, and never an underscore.

There is no shell, so the program and its arguments are separate fields, exactly as they are for
`shell_exec`. The filesystem server that the MCP project publishes looks like this:

| Field | Value |
| --- | --- |
| Name | `Local files` |
| Id | `files` |
| Program | `npx` |
| Arguments | `-y`, `@modelcontextprotocol/server-filesystem`, `/path/you/want/it/to/see` |

Save it and it starts. The row then says whether it connected, what the server calls itself,
which protocol version was agreed, and every tool it offers with the server's own description.
If it did not start, the row says why — and carries the last lines the program wrote to its
stderr under *What it printed*, which for a mistyped package name is the only place the real
answer appears.

Nothing retries on its own. A server that died stays dead and the row says so, with a
**Reconnect** button beside it: a respawn loop would hide a broken configuration behind a
connector that is up for four seconds at a time.

A connector is not free. Its tool *descriptions* are written by the server and ride in every
request the identities holding them make — the filesystem server above is fourteen tools and
several kilobytes of prose. Narrowing an identity's list narrows its prompt as much as it narrows
its reach, which is the argument for a specialist that holds three of those fourteen rather than a
generalist that holds all of them (`PLAN.md` § 7.5).

### Secrets, and what the program can read

A connector names the **environment variables it needs, by name** — `GITHUB_TOKEN`, not its
value. Aegis reads them out of its own environment when it starts the child. Nothing you type in
this form is a secret, `connectors.json` never holds one, and there is nowhere in the form to put
one: export the variable in the shell you launch Aegis from, or in your OS's environment, and the
row tells you when one it names is not there.

The child is given **only** those variables plus the platform's minimum — `PATH`, and the handful
each OS needs to start a process at all. Not this process's whole environment. That is stricter
than `shell_exec`, deliberately: a command is read and approved on the spot, and a connector is
started once and answers for the rest of the session.

### Granting its tools

Installing a connector makes its tools *exist*. It grants them to nobody. Tick them on an
identity under *Settings → Identities*, the same way you would grant it `shell_exec` — they
appear there as their own list, one checkbox per tool, while the connector is running.

They are granted **one at a time, by full name**. Holding `git__status` does not hold anything
else the `git` connector offers, and does not hold a tool the server adds tomorrow. The built-in
**Assistant** is the exception, and always has been: its allow-list is not a list somebody wrote,
it is *every tool this build has*, and a connector you installed is one of those.

### The gate

**Every connector call asks.** There is no auto-allow row for one and there is no read-only
exemption. The dialog names the connector, the tool, what the server says the tool does, and the
arguments the model wrote — and it says plainly that those arguments go to the program as they
are, because Aegis has never seen the tool's schema and the tool does not run here. That is less
than the `fs_write` prompt can promise, and saying so is the point.

Servers may annotate a tool as read-only. Aegis reads that annotation and shows it *attributed*
— "the server calls this read-only" — because it is the thing being gated describing its own
gate. It changes the sentence and never the question.

**Allow for this session** covers that one tool, with any arguments. Not the connector: an MCP
server may change its tool list while a session is open, and a grant that covered the connector
would quietly cover something nobody read.

### What is deliberately not implemented

- **No `sampling`.** Aegis advertises no client capabilities at all in the handshake, so a
  server cannot ask your model to generate anything. A server that could drive the model would
  be a second agent loop with no session, no identity and no approval dialog in front of it. One
  that asks anyway gets a JSON-RPC "method not found" — an answer, never a hang.
- **No `roots`.** A connector is not told where your workspace is. If it needs a directory, you
  pass it as an argument you can read.
- **No resources and no prompts.** A connector is here for its tools. A resource pulled into a
  prompt is a channel this phase has no gate for.
- **No HTTP transport.** stdio only. A connector that listened on a socket is the shape
  `PLAN.md` § 7.4 refuses.

Images, audio and embedded resources that come back from a call are *described* rather than
inlined — `[image, image/png, about 40000 bytes — not shown in this build]` — for the reason
`screen_capture` returns a path and a hash: a megabyte of base64 in the transcript is a megabyte
the model cannot use and the context window cannot spare. Text is capped at 64 KB, and the
envelope says when it was cut.

## The delivery pack

A **pack** is a kind of work Aegis can do, assembled out of things it already has: a project
folder, a few runbooks, whatever connectors that work needs, and one identity that holds them.
It is not a feature. There is no `client_deliver` tool, nothing in the runtime knows what a
client is, and adding the next pack will not change that — a domain that arrived as a new branch
in the agent loop would be a domain nobody could remove.

The first one is client delivery, and Aegis ships its three runbooks in your library:

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `review.diff` | a revision range, or a patch file, and the files as they now stand | `.aegis/artefacts/<branch>.review.md`, ending in *ship*, *change first* or *do not ship* | merge, push, tag, or answer the pull request |
| `deploy.draft` | what the project says about how it ships — its own runbook, the compose file, the CI workflow | `.aegis/artefacts/deploy-<env>-<rev>.md`: the commands in order, each undoable one with how | deploy, or run a step of the plan "to be sure" |
| `alert.draft` | the alert, plus read-only health checks | an incident note that cites a command behind every observed line, and a reply beside it | restart, scale, roll back, or send the reply |

The shape is the same in all three: something on disk goes in, two files come out, and the run
stops one step before the act that cannot be taken back. That step is not missing because it is
hard — it is the human gate (`PLAN.md` § 7.4), and a runbook that ended with the merge would be
a runbook that had merged.

### Setting it up

1. **Open the client's repository as a project** and press *Set up shared files*. The pack writes
   into `.aegis/artefacts/`, and the review it produced last week is in the repository beside the
   change it was about.
2. **Make the identity.** *Settings → Identities → +*: call it what the work is — *Delivery* —
   give it a one-line role, and tick `fs_list`, `fs_read`, `fs_write` and `shell_exec`. Nothing
   else. A specialist that can capture your screen is not a specialist.
3. **Grant the three runbooks** on that identity. That is the moment the pack exists; until then
   the files are examples in a folder.
4. **Install the connectors it wants**, if any — a git or forge MCP server under *Settings →
   Connectors*, then grant its tools on the same identity. This step is optional on purpose: the
   runbooks read a diff through `shell_exec` and deploy facts out of the project's own files, so
   the pack works on the first day and a connector later replaces the *source*, not the
   procedure.
5. **Open a session as that identity** and give it a range, a revision or an alert path.

### What stays yours

Merging, deploying and sending. Not by convention — by the same matrix everything else goes
through, and by three runbooks whose last step says to stop. Aegis has no tool that merges, none
that deploys and none that sends a message; when a host's connector gives it one, that call is
put to you every time, like every connector call.

The reply `alert.draft` drafts is a file until you send it, and it is exactly the input
`never-send-without-review` was seeded for. Run one into the other.

### Where this project's own knowledge goes

`deploy.draft` reads the project's account of itself before anything else, and the best place for
that account is `.aegis/skills/` inside the repository, where it travels with the repo in git.
Two ways to write it, and the difference is the name. Call it `deploy.draft` and it **shadows**
the library's: this project's procedure is now the only one, and the generic one never runs here.
Call it anything else — `deploy.this-app` — and the library's runbook reads it as the source it
asks for in step 1. Either is right; the first is for a project whose deploy is nothing like the
others.

That is the division: the library holds how you work, the repository holds how it works, and the
identity decides which of them a session may run.

### The next pack

`PLAN.md` § 7.3 lists six — delivery, intake, watch, budget, social, revenue — one at a time,
each allowed to fail without blocking the next. Intake is the next one, and it has landed; see
[The intake pack](#the-intake-pack). The way to tell this one landed is not that a client got a
reply: it is that nothing in `src-tauri` had to learn what a client is.

---

## The intake pack

The second pack is client intake, and § 7.3 gives it in five words: **mail first — read and
draft, never send.** Three runbooks, which are the three things that sentence contains.

| Runbook | Reads | Writes | Will not |
| --- | --- | --- | --- |
| `mail.triage` | one message that arrived as a file — an exported `.eml`, a forwarded thread in `.aegis/briefs/`, a note passed on. Headers and text; attachments by name only | `.aegis/artefacts/ticket-<date>-<who>.md`: the quoted ask, the quoted date or *no date given*, who was on the message, what it is blocked on | answer it, act on it, or start the work it describes |
| `thread.recap` | the messages of one thread, oldest first | `.aegis/artefacts/recap-<thread>.md`: a line per message, then **agreed**, **outstanding**, **never answered** | decide anything on the outstanding list |
| `reply.draft` | the ticket, the recap, `STATUS.md`, `DECISIONS.md` | the draft beside the ticket, with a **sources** block naming the file behind every claim | send, schedule, or say it is on its way |

### It cannot run a command, and that is the point

Look at the front matter before the prose: no runbook in this pack declares `shell_exec`. They
read files and write files. So the identity you make for intake is one with `fs_list`, `fs_read`
and `fs_write` and nothing else — an identity that cannot run a command, pointed at text people
outside your house wrote.

That makes Delivery and Intake **two identities**, not one specialist with six runbooks ticked.
It is also a test rather than a paragraph: `the_intake_pack_holds_no_tool_that_runs_a_command`
fails if one of these ever grows a convenient `git` call, because that would widen a grant nobody
went back to look at.

### What each one refuses to do

**`mail.triage` will not invent an ask.** The ask has to be a sentence somebody wrote, quoted,
carrying the date of the message it is in — and *no ask* is an available answer, because most mail
is no ask and a triage that finds work in every message is manufacturing it. The same rule kills
the deadline nobody typed: "as soon as you can" is not a date.

**It always comes back to you when the message is about money or access.** A new bank account, a
new address for an invoice, a password reset nobody asked for: `needs_you`, however ordinary the
message reads, with a note that the request has not been verified on a second channel. Those are
the messages worth forging, and they read exactly like the others.

**It does not read your attachments.** An exported message is mostly not text: a 180 KB PDF makes
a 250 KB `.eml`, which fits under the read cap and arrives *whole*, spending the turn on base64 —
and a slightly larger one goes over it, so what comes back is the message cut off inside the
attachment and its last lines are never seen. `mail.triage` names attachments from their filename
and leaves them on disk. That holds even when the attachment is where the new bank details are:
naming the file and handing it back to you is all this run was ever going to do with it.

**`thread.recap` counts each sentence once.** Your mail client quotes the whole thread into every
reply, so a nine-message thread carries one commitment forty times, and read end to end it looks
like a project. It also reads oldest first — a promise lives where it was made, not where someone
restated it — and it separates *agreed* (proposed **and** answered) from *outstanding*. Silence
is not agreement, which is how a client otherwise gets told that a thing they never agreed to was
settled weeks ago.

**`reply.draft` will not commit to something no file says.** A date, a price, a scope, an order of
work: each needs the decisions ledger, the board, a plan `deploy.draft` wrote, or the ticket's own
quotation behind it, and each names its file in the sources block under the draft. A commitment
with nothing behind it does not get softer wording — "I'll look into it" is a commitment and your
client is right to read it as one; it goes in `open_questions` instead.

**And it will not pick its own input.** No ticket path, no run. A reply drafted to whatever was
written most recently is how the wrong client gets answered, and you would not catch it, because
the draft reads perfectly.

### Setting it up

1. **Get the messages onto disk.** There is no mail connector: export the message, forward it into
   `.aegis/briefs/`, or drop it in the workspace. This is deliberate the same way it was in the
   delivery pack — an IMAP or Gmail MCP server starts a program, which only you can do — and a
   connector later replaces where the message comes from, not the procedure.
2. **Make the identity.** *Settings → Identities → +*: call it *Intake*, and tick `fs_list`,
   `fs_read` and `fs_write`. Not `shell_exec`. Not the delivery specialist with three more boxes
   ticked.
3. **Grant the three runbooks** on it. That is when the pack exists.
4. **Run them in order** on one message: `mail.triage`, then `thread.recap` if the conversation
   has any history, then `reply.draft` on the ticket — then `never-send-without-review` on the
   draft, which is exactly what that runbook was seeded for.
5. **Read the draft and send it yourself.** Aegis has no tool that sends.

### Where it stops, and where the board is

`mail.triage` ends at the ticket. Putting the item on the board is `inbox.triage`, the runbook
each workspace gets from *Set up shared files*, and the pack names it rather than repeating it —
how *this* project tracks work is the project's, and it stays in the project.

The ticket is deliberately thin: it cites the message by path instead of pasting it. Tickets land
in a repository, usually the client's, usually in git, and other people's addresses, phone numbers
and attachments do not have to be copied there to be found.

### The four after this one are not here

Watch, budget, social and revenue are not started. Two packs that were pleasant to build are not
two reasons to build a third, and the order in `PLAN.md` § 7.3 exists so each is allowed to fail
without taking the next one with it.

---

## Layout

```
src/           React app — presentation and typed IPC glue only
  ipc/         invoke() / listen() wrappers; bindings.ts is generated from the Rust structs
  state/       zustand stores
  components/  layout, chat, sessions, approvals, projects, agents, skills, memory,
               routines, connectors, board, settings, audit
src-tauri/
  src/
    commands/  one module per IPC command domain
    agent/     turn loop, wire protocol, providers (scripted, and OpenAI-compatible over SSE),
               event payloads, turn registry
    store/     projects.json, sessions.json, agents.json, memories.json,
               routines.json, connectors.json and settings.json, behind one atomic write
    tools/     fs, shell, screenshot, skill, memory, handoff, connector — behind one
               ToolSpec registry
    mcp/       the MCP client: one stdio JSON-RPC connection per external server, and the
               roster of what is running and what each one offers
    policy/    path containment, decision matrix, per-session grants
    skills/    the runbook format and the catalog: a line per skill in context, the body
               only when one is run
    handoff/   the brief that goes out and the report that comes back, the bus that
               carries them (parallel, bounded, two attempts, then the human), and the
               runner that turns a brief into an ordinary session and turn
    schedule/  routines: which one may exist, when it is due, what its run is told —
               and the tick that fires one into an ordinary session nobody is watching
    board/     the structured read of .aegis/status/STATUS.md beside what the runtime can see,
               and the fold of the audit log into runs — who ran, what it cost, why it failed
    workspace.rs  the shared-file convention inside a project folder: scaffold, and the
               capped digest every request carries
    world.rs   the other layer in that same folder: what the project *is*, read and never
               written by a session, and the declared sources it was perceived from
    compact.rs the older half of a transcript, derived into state — no summarizer,
               and nothing deleted
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
  subtree, or one shell program by name — never to a tool as a whole. A grant you make in a
  session is never written to disk and dies with the session. There is no "allow forever". While
  one is in force it is listed under the transcript of the session that created it, with a Revoke
  button; revoking restores the prompt from the next call onwards.
- **A routine's standing approvals are the one thing that outlives a session, and you sign them
  by hand.** A scheduled run cannot ask anybody, so what it may do beyond reading is exactly the
  list you ticked when you saved the routine — the same scopes, in the same words, stored in
  `routines.json` and in force only for that routine's own runs. It cannot exceed what the runbook
  declares it will call or what the identity holds, both checked when you save; nothing outside
  the workspace and nothing under `.git/` can be signed for at all; and every other call an
  unattended run makes is refused outright rather than queued behind a prompt nobody can see. See
  [Routines](#routines).
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
- **A skill is a procedure, not a permission.** Running a runbook grants nothing: every step in
  it is an ordinary tool call that goes through the same matrix and the same dialog it would have
  gone through anyway, and a runbook whose declared tools the identity does not hold is refused
  before its first step rather than partway through. A skill the identity was not granted is
  refused before the file is even located, with no prompt — you cannot be asked to let an
  identity exceed its own list. The runbook itself is a file in a folder you own, so treat it the
  way you would treat a script you are about to run: a `SKILL.md` somebody sent you is
  instructions your model will follow, and what it can reach while following them is whatever you
  ticked.
- **Handing work out is one approval, and it is not a blanket one.** `handoff_delegate` prompts
  because it is the only call that makes *other identities run* — more requests to your provider,
  under other allow-lists, for as long as the deadline allows. The dialog names every owner and
  what each is being asked for. What the approval covers is the routing and nothing else: each
  specialist runs in a session of its own, holding **none** of this session's grants, so
  everything it wants to write, run or capture prompts you again there. A specialist cannot
  delegate in turn — the tool is not on its list, and it is refused if it asks anyway — so a
  delegation is one level deep and its cost is bounded by the number of briefs you saw. Note that
  a specialist's prompt appears in *its* session: the sidebar row says *waiting on you*, and an
  approval nobody answers within five minutes is refused like any other.
- **A world's constitution is not editable by delegated work, and approving writes does not
  approve it.** In a folder that holds a `world/`, an `fs_write` under it from a specialist working
  on a brief is refused with no prompt — you cannot be asked to let a brief rewrite what the
  project *is*. From a session you are sitting in it prompts at high risk and *does* offer "allow
  for this session", but as a **scope of its own**: a general workspace-write approval never
  reaches `world/`, and a world approval never reaches anything else. A scheduled run is offered
  neither — it cannot be signed for the world when you save the routine, and it is refused if it
  tries. An `fs_read` of a file `world/sources.yml` declares is refused too, for as long as it
  still hashes to what was recorded: what it said is in `world/`. Note the limits. Only the first
  path segment is the constitution, so a repository's own `src/world/` is an ordinary folder.
  `shell_exec` is not covered — a command that writes `world/` is stopped by the shell prompt you
  answer, not by this rule. And a source that has *changed* is deliberately readable again, because
  that is how a new dump gets perceived. See [The world](#the-world).
- **There is no sandbox.** Approved tools run as you, with your privileges and environment. The
  real boundary is that you read the exact path, program, arguments and working directory before
  approving. Treat every approval as if you were typing the command yourself.
- **A connector is a program you installed, and Aegis cannot see inside it.** An external MCP
  server runs as you, like every other tool, but with one difference that is worth stating: for
  `fs_write` the runtime resolved the path itself and the dialog shows a fact, while for a
  connector's tool it shows the tool's name, the server's own description of it and the arguments
  the model wrote — and nothing more, because the schema is the server's and the work happens in
  another process. That is why **every connector call prompts**, with no auto-allow row and no
  read-only exemption; a server's `readOnlyHint` is shown attributed to the server and changes
  nothing. A session grant covers one tool by name, never the connector, because a server may add
  a tool while your session is open. Aegis advertises **no client capabilities** in the handshake,
  so no server can ask your model to generate anything (`sampling`) or be told where your
  workspace is (`roots`), and a server's tool *descriptions* reach your system prompt — treat a
  connector the way you would treat a script you are about to run. Adding one is a human act in
  Settings; there is no tool that can. See [Connectors](#connectors).
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
- **A memory is durable and it is sent to your provider.** Everything an identity remembers is in
  the system message of every request it makes, so a memory is worth reading before you allow it
  the way you would read a file before allowing a write — and it is why the dialog shows the whole
  sentence. The model can record one and read its own; it cannot delete one, and it cannot see
  another identity's. Memories live in `memories.json` under the app data directory, never in
  your workspace, and *Settings → Memory* is the one place they are corrected or removed. A
  compaction never touches them: what folds is the conversation.
- **A compaction is not a deletion.** Folding a session changes what the model is sent and
  nothing else. Your transcript stays whole on disk, the audit log still answers for every call,
  and the state the fold produces is derived from the record rather than summarized by a model —
  so it cannot invent a file that was never written.
- **The shared files are sent to your provider.** Once `.aegis/status/` and `.aegis/decisions/` exist, every
  request carries what is in them — that is the point of them, and it is worth knowing before you
  put something in `STATUS.md` you would not paste into a chat. Only those two files are read,
  capped at 2 KB each; `.aegis/briefs/` and `.aegis/artefacts/` contribute file *names* and never content. A
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
  made it, the skill run it belonged to, the policy reason and the outcome — see *Where your data lives*. Arguments are recorded as a SHA-256 digest plus
  a redacted copy that keeps paths and replaces file content with its size. A connector's call is
  one line like any other, under the name the model used (`git__status`).

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
- **A connector that runs `npx` works, and that is not automatic.** `npx`, `uvx` and friends are
  `.cmd` shims, which `CreateProcess` cannot launch directly. Aegis resolves a connector's program
  through `PATHEXT` with the same function `shell_exec` uses, so `npx` finds `npx.cmd` and the
  launch goes through `cmd.exe` with the arguments escaped by the Rust standard library. Your
  arguments are still a vector, not a command line.
- **A command in WSL says the distribution is not there.** `E_EXEC_HOST` means the project names
  an execution host it cannot use — the distribution was uninstalled or renamed, the WSL service
  is not running, or the workspace is somewhere that distribution cannot see. The picker in the
  rail names what is installed; setting it back to *This computer* runs commands on Windows again.
  Aegis never falls back on its own, because a command that was approved for Ubuntu and ran on
  Windows would be a different command.
- **A WSL project's commands are slower to start.** Every hosted call first asks the distribution
  whether the working directory is there (`wsl -d … --exec test -d …`, about 150 ms warm, longer
  when the distribution has to start). That is deliberate: `wsl --cd` answers a directory it
  cannot find by starting in `/` and saying nothing, so without the check a command approved for
  one folder could run in another.
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
