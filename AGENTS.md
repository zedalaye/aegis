# [AGENTS.md](http://AGENTS.md) — Aegis (working title)

Desktop AI harness. Cross-platform. No Electron.

## Stack (non-negotiable)

- UI shell: Tauri 2 + TypeScript + React + Vite

- Runtime: Rust (src-tauri)

- Tools: MCP client in Rust; tools are separate processes / MCP servers

- Package manager frontend: pnpm

- Rust edition 2021+, Tauri 2.x only

- Never add Electron, Neutralino, Wails, or a bundled Chromium

## Product

Local desktop app that:

- runs multi-agent sessions

- can operate the machine (fs, shell, git) under an approval gate

- manages projects (workspace folder + session + task list + optional
  execution host). The folder's convention files are git-backed by
  default when they are laid down (`PLAN.md` § 7.11); picking a folder
  does not `git init`, and the harness is not a git host. A WSL distro
  is an execution host on the project (`PLAN.md` § 7.12), not a second
  runtime: `fs_*` stays on this process; `shell_exec` lands in the
  distro the operator named. Picking a folder does not imply WSL.

- stays in the system tray

This repo is the harness (UI + runtime + permissions). It is not a new LLM.

Domains (client delivery, inbox, watch, finance, social, revenue, wish list)
are workloads that will sit on the harness later. They are not reasons to
enlarge the MVP runtime. See **North star** and `PLAN.md` § 7.

## Architecture

ui (React) --invoke/events--&gt; rust runtime  
|- sessions / approvals / audit log  
|- MCP client  
|- tools: fs, shell, screenshot  
- optional later: python sidecar (not in MVP)

WebView renders UI only. Agent loop and tool execution live in Rust.

## MVP scope (do this, nothing else)

Must work on macOS, Windows, Linux (best-effort Linux WebView):

1. Tauri 2 app boots, tray icon, show/hide main window

2. Chat panel + session list

3. Fake-then-real agent loop: user message -&gt; runtime -&gt; streamed tokens/events to UI

4. Tools with allow / deny / always-allow-this-session:

   - list/read/write files inside a user-picked workspace

   - run shell command in workspace

   - capture primary monitor screenshot (plugin or crate)

5. Project = workspace path + name + last sessions

6. Audit log of tool calls (jsonl)

7. Settings: provider placeholder (OpenAI-compatible base URL + model + API key in OS keyring or env), no keys in frontend

Out of scope for MVP (do not start, even as a "head start"):

- real multi-agent graph (supervisor / workers / Chef de Cabinet)

- skills, scheduler, per-agent memory, compaction-as-state

- domain connectors (mail, SMS, WhatsApp, GitHub/GitLab, Coolify, monitoring, X, brokers)

- messaging faces / control channels (Keybase, X Chat, Telegram, Discord). A
  face is a later UI onto this runtime, not a reason to enlarge the MVP loop.

- Playwright / browser-use

- accessibility-tree desktop control

- Python sidecar

- auto-update, signing, installers beyond `tauri build` default

- mobile

Until the **Done when** line below is true, ignore the North star for coding.
`PLAN.md` § 7 exists so remaining MVP work does not paint those later phases
into a corner.

## North star (after MVP — do not implement now)

Operating-mode brief: `COS.md` (invariants). Sequence: `PLAN.md` § 7.
This file remains the coding contract. Intended mode: **Chef de Cabinet**
— three roles (CoS, specialist, human), files as shared memory, skills as
runbooks, handoffs instead of shared transcripts. The CoS does **not** copy
a human project-management method (sprints, activity tickets, stand-ups,
cherishing the implementation). Those optimise calendar time and scarce
writing. Agents invert the costs: generating an instance is cheap,
re-perceiving a project that already lives in files is the waste, and the
unit of cost is a round-trip. Work is a world specialists read and do not
write, briefs that are oracle clauses, instances that are disposable,
verification that is a program, écarts (the world would have to change)
that escalate to the human. Read
`COS.md` for those rules; `PLAN.md` § 7.2 for how they sit on this tree.
Do not restate them here. Recurring work that still lives in a chat or a
system prompt is not a skill — do not schedule it (`PLAN.md` § 7.6).

### What the harness must make possible (later)

| Workload | Harness job | Not a product inside this repo |
| --- | --- | --- |
| Client delivery | operate a repo (dev, review), draft deploy / monitor / alert responses | a PaaS or Coolify clone |
| Client intake | triage inbound work (mail, SMS, WhatsApp) into tickets and files | a messaging server |
| Tech / AI / econ watch | scheduled research → artefacts in a workspace | a scraping farm |
| Budget and portfolio | read-only surveillance, alerts, a status file | a bank or broker |
| Revenue experiments | propose (trading ideas, X drafts); never execute | an autonomous trader or poster |
| Wish list | prioritized goals; CoS tracks, human decides spend | a shopping agent |

Three later surfaces, not one:

- **Remote access** is the same runtime over a private network
  (e.g. Tailscale). It is not a second process and not a crate in
  this repo. Preferred path: the host joins the tailnet and you
  drive the existing window (OS remote desktop or Tailscale SSH).
  A later optional loopback HTTP/WS on `127.0.0.1` may mirror the
  same commands as the WebView, reached via Tailscale Serve — never
  Funnel, never `0.0.0.0`. Outbound SSH or MCP to another machine
  is a **tool** under the approval gate, not remote access and not
  a second agent loop. See `PLAN.md` § 7.8.
- **Messaging face** (Keybase first for confidentiality; X Chat as the
  other E2EE-shaped adapter; then Telegram or Discord) is a UI onto this
  runtime — same sessions, same approval gate, same audit. It is not an
  agent and not an inbox. Preferred first face: Keybase, via the local
  `keybase` CLI already logged in (`chat api` / `api-listen`): chat is
  E2EE, the client is outbound-only, nothing listens on a public port.
  X Chat (Chat XDK + `GET /2/activity/stream`) is the same outbound
  shape with client-side E2EE and an official Rust core; it is not a
  second agent and not the Phase 19 social pack. Telegram long-poll and
  Discord Gateway fit that outbound shape with weaker confidentiality.
  See `PLAN.md` § 7.7.
- **Inbox intake** (mail, SMS, WhatsApp) is a domain pack later
  (`PLAN.md` § 7.3 Phase 19). WhatsApp is not a control channel.

Do not expose the runtime on the public internet. A face that needs an
inbound webhook (WhatsApp Cloud API, Telegram `setWebhook`, X
`POST /2/webhooks`, ngrok) is the wrong shape.

Multi-LLM: the MVP has one OpenAI-compatible provider. Later, a roster of
providers and a per-agent binding (CoS on one model, a coding specialist
on another). Keys stay in the OS keyring / env, never in the WebView.

### After-MVP order (fixed — `PLAN.md` § 7)

1. Shared workspace convention (cabinet files under `.aegis/`, plus an
   optional `world/` at the root) +
   per-agent memory + skill runner
2. Then Chef de Cabinet (handoff bus, fan-out / fan-in, status board)
3. Then scheduler of routines
4. Then MCP connectors
5. Then domain packs as skills, not new runtime features

A CoS without (1) recites. Do not build (2) first.

Chrome polish is `PLAN.md` § 7.10. Workspace versioning (`git init` on scaffold,
never an auto-commit) is § 7.11. Execution host (WSL) is § 7.12, and it has
landed: opt-in per project, nothing infers it from a `\\wsl$\` folder, `fs_*`
is unchanged, and a distro that cannot take the command refuses rather than
falling back to this computer. Skill promotion
(a proposal file, then apply; writing is still not granting) is § 7.13. Cabinet
founding (a founder skill writes a roster proposal; apply is the grant; no
wizard, no seeded identities) is § 7.14. A read-only workspace explorer
(preview in, save out; a drop lands a brief) is § 7.15: the agent is in
the system, the operator is not. The `world/` constitution was the
missed half of Phase 11 (`PLAN.md` § 7.2), not a Phase 20, and it has landed:
opt-in, nothing scaffolds it, the frame is a harness injection, and a
specialist's write into it is refused rather than asked. None of these is a
step in this list, and none is a reason to delay (1)–(3). There is no in-app
editor. A skill is a file: the operator's editor, or `fs_write` under the
gate. Granting it to an identity is a separate act.

## Permissions

- Default: ask before every mutating tool (write, shell)

- Reads inside workspace: auto-allow

- Paths outside workspace: always ask

- Never exfiltrate secrets to the WebView beyond masked settings

- Capabilities JSON must be least-privilege

- Irreversible actions (send, pay, merge, publish, deploy, trade) stay
  behind a human gate. Later domain tools inherit this matrix; they do
  not get a bypass.

- The MVP has no OS sandbox: tools run as the user. The approval gate is
  a human boundary, not containment. Do not treat "allow this session"
  as "the agent owns the host". A fleet of agent processes and a
  hypervisor control plane are **not products in this repo** — not
  because the work is useless, but because they do not belong in the
  harness: multi-agent is in-process (CoS + specialists + files);
  `docker` / `ssh` / `sbx` on PATH are already `shell_exec` under the
  gate; WSL is not a program the model calls — wrapping is
  `shell_exec`'s when the project has that host (`PLAN.md` § 7.12); a
  later workspace-scoped executor (`PLAN.md` § 7.9) is containment for
  *our* tools, not a VM manager and not computer-use of the desktop.

## Code rules

- Rust: no unwrap in library paths; use Result + thiserror; structured tracing

- TS: strict, no `any`, components small

- IPC: typed commands + events; one module per command domain

- Do not put LLM API keys in localStorage

- Do not treat the chat transcript as durable memory. Facts that must
  survive compaction belong in workspace files or stores. (MVP still
  persists the transcript; it is a session log, not the source of truth
  for decisions.)

- Do not copy a human project-management method into a prompt, a skill,
  or the board. Sprints, activity tickets, and "don't rewrite" are the
  wrong scarcity. A turn that starts by exploring a world already in
  `world/` is a defect, not professionalism.

- Conventional commits

- README in English with run instructions

## Commands

- Frontend: `pnpm install` / `pnpm tauri dev`

- Rust: `cargo test` in src-tauri

- Format: rustfmt + prettier if added

## Done when

`pnpm tauri dev` launches a window + tray, you can pick a workspace, send a message, see a streamed reply, approve a `lsecho` tool call, and find an audit line on disk.