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

- manages projects (workspace folder + session + task list)

- stays in the system tray

This repo is the harness (UI + runtime + permissions). It is not a new LLM.

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

Out of scope for MVP:

- real multi-agent graph (supervisor/workers)

- Playwright / browser-use

- accessibility-tree desktop control

- Python sidecar

- auto-update, signing, installers beyond `tauri build` default

- mobile

## Permissions

- Default: ask before every mutating tool (write, shell)

- Reads inside workspace: auto-allow

- Paths outside workspace: always ask

- Never exfiltrate secrets to the WebView beyond masked settings

- Capabilities JSON must be least-privilege

## Code rules

- Rust: no unwrap in library paths; use Result + thiserror; structured tracing

- TS: strict, no `any`, components small

- IPC: typed commands + events; one module per command domain

- Do not put LLM API keys in localStorage

- Conventional commits

- README in English with run instructions

## Commands

- Frontend: `pnpm install` / `pnpm tauri dev`

- Rust: `cargo test` in src-tauri

- Format: rustfmt + prettier if added

## Done when

`pnpm tauri dev` launches a window + tray, you can pick a workspace, send a message, see a streamed reply, approve a `lsecho` tool call, and find an audit line on disk.