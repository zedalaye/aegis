# AGENTS.md — Aegis

The contract for coding agents in this repository. Aegis is a cross-platform desktop AI harness.
No Electron.

## Where things are written down

| Question | File |
| --- | --- |
| Scope, permissions, code rules | **this file** |
| Design decisions in force: IPC, policy matrix, wire protocol, platform risks, phase history | `PLAN.md` |
| Rules of the Chef-de-Cabinet operating mode | `COS.md` |
| Deferred ideas, with their investigation | `IDEAS.md` |
| Messaging-face sketch (not scheduled) | `CONTROL.md` |
| How to use it | `README.md`, `docs/` |
| What changed | `CHANGELOG.md` |

On conflict: this file wins on scope and rules, `PLAN.md` on design and order, `COS.md` on the
mode. Code comments cite `PLAN.md` sections by number (`PLAN 7.13`); keep those numbers stable.

## Stack (non-negotiable)

- UI shell: Tauri 2 + TypeScript + React + Vite
- Runtime: Rust (`src-tauri`), edition 2021, Tauri 2.x only
- Tools: in-process Rust tools behind one `ToolSpec` registry; external tools are MCP servers
  (separate processes)
- Frontend package manager: pnpm
- Never add Electron, Neutralino, Wails, or a bundled Chromium

## Product

This repository is the harness (UI + runtime + permissions), not an LLM. It:

- runs multi-agent sessions in one process: identities, skills, handoffs, routines;
- operates the machine (files, shell, screen capture, git) under an approval gate;
- manages projects: a workspace folder, its sessions, its shared files, an optional execution
  host. Picking a folder mutates nothing. *Set up shared files* lays down `.aegis/` and runs
  `git init` when the folder is not in a work tree, and never commits. A WSL distribution is an
  execution host on the project (`PLAN.md` § 7.12), not a second runtime: `fs_*` stays in this
  process, `shell_exec` lands in the distribution;
- stays in the system tray.

## Architecture

```
ui (React) --invoke/events--> rust runtime
  |- sessions / approvals / audit log
  |- agent loop, providers
  |- tools: fs, shell, screenshot, skill, memory, handoff, connector (MCP client)
  |- skills, handoff bus, scheduler, board
```

The WebView renders UI only. The agent loop and tool execution live in Rust.

## State

The MVP (Phases 0–10) is done. The post-MVP phases of `PLAN.md` § 7.3 (11–19) have landed, and so
have the slices § 7.10–7.24 (§ 7.23 in part), and `world/` (§ 7.2). New work is proposed as a section of
`PLAN.md` § 7 before it is coded: what it settles, what it refuses, its exit.

## Out of scope (do not start, even as a head start)

- Messaging faces / control channels (Keybase, X Chat, Telegram, Discord). `CONTROL.md` is a
  sketch; `PLAN.md` § 7.7.
- Remote access inside the process: embedded Tailscale, a public or `0.0.0.0` listener, inbound
  webhooks. `PLAN.md` § 7.8.
- A fleet of agent processes or a hypervisor / VM control plane. A workspace-scoped executor is a
  later seam, not current work. `PLAN.md` § 7.9.
- Computer-use of the desktop: accessibility-tree control, Playwright / browser-use as the agent's
  hands.
- A Python sidecar; auto-update, signing, installers beyond `tauri build`; mobile.
- An in-app editor or any save path from the WebView.
- A domain as a runtime feature. A domain is a pack: workspace + skills + connectors + an
  identity. A domain never grows `agent/turn/`.

## North star

Operating mode: **Chef de Cabinet** — three roles (Chief of Staff, specialist, human), files as
shared memory, skills as runbooks, handoffs instead of shared transcripts. The CoS does not copy a
human project-management method: generating an instance is cheap, re-perceiving a project that
already lives in files is the waste, and the unit of cost is a round-trip. Work is a world
specialists read and do not write, briefs that are oracle clauses, disposable instances,
verification that is a program, and écarts that escalate to the human. `COS.md` has the rules.
A CoS without shared files, memory and skills recites.

| Workload | Harness job | Not a product in this repo |
| --- | --- | --- |
| Client delivery | operate a repo; draft deploy, monitoring and alert responses | a PaaS or Coolify clone |
| Client intake | triage inbound work into tickets and files | a messaging server |
| Tech / AI / econ watch | scheduled research → artefacts | a scraping farm |
| Budget and portfolio | read-only surveillance, alerts, a status file | a bank or broker |
| Revenue experiments | propose (trading ideas, drafts); never execute | an autonomous trader or poster |
| Wish list | prioritized goals; the CoS tracks, the human decides spend | a shopping agent |

Later surfaces, none of them a second agent loop:

- **Remote access** is the same runtime over the operator's private network (the host joins the
  tailnet; later, optionally, a loopback HTTP/WS on `127.0.0.1` via Tailscale Serve — never
  Funnel). Outbound SSH or MCP to another machine is a tool under the gate.
- **A messaging face** is a UI onto the same sessions, gate and audit, over an outbound-only
  channel (Keybase first).
- **Inbox intake** is a domain pack. WhatsApp is not a control channel.
- **Multi-LLM** is a provider roster with a per-identity binding. Keys stay in the OS keyring or
  the environment.

Do not expose the runtime on the public internet.

## Permissions

- Default: ask before every mutating tool (write, shell, capture, memory, delegation, connector).
- Reads inside the workspace: auto-allow, except credential-shaped names.
- Paths outside the workspace: always ask, never granted for a session.
- Session grants are narrow, session-lifetime and revocable (`PLAN.md` § 3.1).
- An ask nobody can answer is **parked**, never run and never silently dropped: the call does not
  happen, the question is filed outside the workspace, and a person answers it later
  (`PLAN.md` § 7.22). Parking is not a way past the gate — `world/` is refused outright.
- Never expose secrets to the WebView beyond masked settings. No API keys in `localStorage`.
- Capabilities JSON stays least-privilege; a plugin permission there is a review flag.
- Irreversible actions (send, pay, merge, publish, deploy, trade) stay behind a human gate. Later
  domain tools inherit the matrix; they do not get a bypass.
- Writing is not granting. A skill or roster file grants nothing; granting it to an identity is a
  separate act, in Settings.
- There is no OS sandbox: tools run as the user. The approval gate is a human boundary, not
  containment. "Allow for this session" does not mean the agent owns the host.

## Code rules

- Rust: no `unwrap` in library paths; `Result` + `thiserror`; structured `tracing`.
- TypeScript: strict, no `any`, small components.
- IPC: typed commands and events, one module per command domain; `src/ipc/bindings.ts` is
  generated by `ts-rs`.
- The transcript is a session log, not durable memory. Facts that must survive compaction belong
  in workspace files or stores.
- Do not copy a human project-management method (sprints, activity tickets, "don't rewrite") into
  a prompt, a skill or the board. A turn that starts by exploring a world already in `world/` is a
  defect.
- Comments say what is not obvious, briefly. The design argument lives in `PLAN.md`: cite the
  section instead of restating it.
- Conventional commits. README and docs in English.

## Commands

- Frontend: `pnpm install`, `pnpm tauri dev`, `pnpm typecheck`
- Rust (in `src-tauri`): `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt`
