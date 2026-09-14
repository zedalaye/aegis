# Aegis

Local desktop AI harness. It runs agent sessions that can operate your machine — files, shell,
screenshots — behind an explicit approval gate, and it audits every tool call.

Aegis is a harness, not a model: you point it at a provider.

- **UI** — Tauri 2 + TypeScript + React + Vite, on the OS WebView (no Electron, no bundled Chromium)
- **Runtime** — Rust (`src-tauri`): the agent loop, tools, policy, secrets and audit

The WebView renders UI only. No tool runs in the browser context, and no API key reaches it.

**Status:** the MVP is complete, the six domain packs have landed, and the post-MVP work of
`PLAN.md` § 7 is under way. [`CHANGELOG.md`](CHANGELOG.md) has the history.

## Requirements

| | Version | Notes |
| --- | --- | --- |
| Node.js | ≥ 20.19 | 24 LTS recommended |
| pnpm | 10.x | `corepack enable pnpm`; pinned by `packageManager` in `package.json` |
| Rust | ≥ 1.85 stable | via `rustup`, MSVC host on Windows. 1.85 is the floor the screen-capture crate needs |

Plus one platform toolchain:

- **Windows** — Visual Studio Build Tools (*Desktop development with C++*) and the WebView2
  runtime.
- **macOS** — Xcode Command Line Tools (`xcode-select --install`).
- **Linux** (best-effort) — WebKitGTK 4.1 and the tray, TLS, Secret Service and screen-capture
  development packages. On Debian/Ubuntu (a WSL2 Ubuntu works as a compile host):

  ```sh
  sudo apt install \
    build-essential curl wget file pkg-config clang libclang-dev patchelf \
    libwebkit2gtk-4.1-dev libsoup-3.0-dev \
    libayatana-appindicator3-1 libayatana-appindicator3-dev librsvg2-dev \
    libssl-dev libdbus-1-dev \
    libxcb1-dev libxrandr-dev libpipewire-0.3-dev \
    libwayland-dev libegl-dev libgbm-dev libdrm-dev
  ```

  See [Troubleshooting](docs/troubleshooting.md#linux) for the errors a missing package produces.

## Run it

```sh
pnpm install
pnpm tauri dev
```

This starts Vite on port 1420 and builds the Rust binary; the first build takes a few minutes.

### A two-minute walkthrough

No provider is needed: the scripted provider exercises the whole runtime.

1. **Add a project** with **+** next to *Projects*. The folder you pick is the workspace.
2. **Start a session** and send anything. The reply streams.
3. **Make it ask.** Send `/write`, `/run` or `/capture`. Deny once, send again, allow once.
4. **Open the audit log** from the title bar: both calls are there, with the policy's reason.
5. **Close the window.** Aegis stays in the tray; Quit is in the title bar. After a restart,
   everything is where you left it.
6. **File a decision.** Press *Set up shared files*, then ask for a decision to be recorded. See
   [Workspace files](docs/guide/workspace.md).
7. **Narrow an identity.** Make a *Reviewer* with only `fs_list` and `fs_read`, open a session as
   it, send `/write`: refused, with no prompt. See [Identities](docs/guide/identities.md).
8. **Run a skill.** Grant `never-send-without-review` to an identity and send `/skill`. See
   [Skills](docs/guide/skills.md).
9. **Remember something** with `/remember`, then correct it in *Settings → Memory*.
10. **Delegate.** Give an identity `handoff_delegate`, make a *Scribe* with `fs_read`, and send
    `/delegate Scribe`. See [Handoffs](docs/guide/handoffs.md).
11. **Schedule it.** Once an identity has run a skill, add a routine for it in
    *Settings → Routines* and press **Run now**.
12. **Look back.** **Compact** a long session from its header, then open **Board**: every run
    above is there, with its cost.
13. **Add a connector** (needs Node). *Settings → Connectors*: program `npx`, arguments `-y`,
    `@modelcontextprotocol/server-filesystem` and a folder. See
    [Connectors](docs/guide/connectors.md).

## Point it at a model

Open **Settings** in the title bar.

| | |
| --- | --- |
| **Authentication** | an OpenAI-compatible API key, a **Gemini** AI Studio key, or an existing **Claude Code**, **Codex CLI** or **Grok CLI** login on this machine |
| **Base URL** | for an API key, the endpoint up to where `/chat/completions` would begin: `https://api.openai.com/v1`, `http://127.0.0.1:11434/v1`, … Leave it empty for Gemini or a CLI login unless you override the endpoint |
| **Model** | the model id, as that server spells it |
| **API key** | saved to the OS credential store. Leave empty to keep the stored one |

- **Test connection** sends one short completion and tells a wrong address from a wrong key from
  an unknown model.
- **Anthropic's API** is recognised by its base URL (`https://api.anthropic.com`): turns go to
  `/v1/messages` with prompt caching, not to the compatibility layer, which drops caching.
- **Gemini** talks to `generativelanguage.googleapis.com` in its own dialect. It replaces the
  single configured provider like any other kind.
- **A CLI login** reads that CLI's credential file, refreshes the token and writes it back, and
  presents itself as that CLI. That may be outside the provider's terms for your account.
- **Without a credential store** (headless Linux, a locked keychain, a dev build whose signature
  keeps changing), set `AEGIS_API_KEY` in the environment and restart. No other variable is read.
- With no base URL, replies come from the scripted provider.

## Build and test

```sh
pnpm tauri build                                   # bundle in src-tauri/target/release/bundle/
cd src-tauri && cargo test                         # also regenerates src/ipc/bindings.ts
cd src-tauri && cargo clippy --all-targets -- -D warnings
pnpm typecheck
```

- `src/ipc/bindings.ts` is generated from the Rust payload types by `ts-rs` during `cargo test`
  (destination set in `src-tauri/.cargo/config.toml`). Do not edit it; run the Rust tests before
  `pnpm typecheck` after touching an IPC type.
- Builds are not signed or notarized.
- Everything runs headless. The connector tests use the test binary itself as a real MCP server
  over stdio. The screen-capture tests accept either a PNG whose digest matches its audit line or
  a refusal with `E_SCREEN_PERMISSION`, and nothing else.

## Documentation

| | |
| --- | --- |
| [Data and execution hosts](docs/guide/data.md) | where Aegis stores things; running commands in WSL |
| [Workspace files and the world](docs/guide/workspace.md) | `.aegis/`, versioning, the Files panel, `world/` |
| [Identities, memory and compaction](docs/guide/identities.md) | allow-lists, founding a cabinet, memories, folding long sessions |
| [Skills](docs/guide/skills.md) | runbooks, the catalog, proposals, returns |
| [Handoffs, routines and the board](docs/guide/handoffs.md) | delegation, schedules, runs and cost |
| [Connectors](docs/guide/connectors.md) | external MCP servers |
| [Domain packs](docs/guide/packs.md) | delivery, intake, watch, budget, social, revenue |
| [Security posture](docs/security.md) | what the gate does and does not protect |
| [Troubleshooting](docs/troubleshooting.md) | platform issues |
| [Architecture](docs/architecture.md) | source layout |

For contributors: `AGENTS.md` (scope and code rules), `PLAN.md` (design of record), `COS.md`
(the operating mode), `IDEAS.md` (deferred work), `CONTROL.md` (messaging-face sketch).

## License

MIT
