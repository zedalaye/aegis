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

> **Status: Phase 2 (persistence and projects).** The app boots, lives in the system tray, hides
> instead of quitting when the window is closed, and remembers the workspace folders you point it
> at. Sessions, tools, the approval gate and provider integration land in later phases — see
> `PLAN.md` § 6.

---

## Requirements

| | Version | Notes |
| --- | --- | --- |
| Node.js | ≥ 20.19 | 24 LTS recommended |
| pnpm | 10.x | `corepack enable pnpm` — the version is pinned by `packageManager` in `package.json` |
| Rust | ≥ 1.82 stable | `rustup` toolchain, MSVC host on Windows |

Plus one platform toolchain:

- **Windows** — Visual Studio Build Tools with the *Desktop development with C++* workload, and
  the **WebView2 runtime** (see below).
- **macOS** — Xcode Command Line Tools (`xcode-select --install`).
- **Linux** — `webkit2gtk-4.1`, `libayatana-appindicator3`, `librsvg2`, `patchelf` and the usual
  build essentials.

## Run it

```sh
pnpm install
pnpm tauri dev
```

`pnpm tauri dev` starts Vite on port 1420 and builds the Rust binary; the first Rust build takes
a few minutes, later ones are incremental.

## Build it

```sh
pnpm tauri build
```

Output lands in `src-tauri/target/release/bundle/`. The MVP does not sign or notarize anything —
see *Troubleshooting*.

## Tests

```sh
cd src-tauri && cargo test     # Rust: persistence, policy, tools, audit, wire protocol
cd src-tauri && cargo clippy --all-targets -- -D warnings
pnpm typecheck                 # TypeScript, strict
```

---

## Where your data lives

Projects are stored as one small JSON document, `projects.json`, under the application-data
directory:

| | Path |
| --- | --- |
| Windows | `%APPDATA%\dev.aegis.harness\` |
| macOS | `~/Library/Application Support/dev.aegis.harness/` |
| Linux | `~/.local/share/dev.aegis.harness/` |

It holds names and workspace paths — no file contents, and never a key. It is meant to be
readable and is safe to edit by hand while Aegis is closed; a document Aegis cannot parse is
renamed to `projects.corrupt-<timestamp>.json` and the app starts with an empty list rather than
refusing to open. Deleting a project forgets it here; the workspace folder itself is never
touched.

---

## Layout

```
src/           React app — presentation and typed IPC glue only
  ipc/         invoke() / listen() wrappers; bindings.ts mirrors the Rust payload structs
  state/       zustand stores
  components/  layout, chat, sessions, approvals, projects, settings, audit
src-tauri/
  src/
    commands/  one module per IPC command domain
    agent/     turn loop, wire protocol, providers
    tools/     fs, shell, screenshot — behind one ToolSpec registry
    policy/    path containment, decision matrix, per-session grants
    audit.rs   one jsonl line per tool call
    secrets.rs OS keyring, environment fallback, masking
  capabilities/  least-privilege Tauri permission sets
```

`PLAN.md` is the design of record: IPC surface (§ 2), the tool policy matrix (§ 3), the wire
protocol (§ 4), platform risks (§ 5) and the phase order (§ 6). `AGENTS.md` fixes the stack and
the scope.

## Security posture

Read this before pointing Aegis at anything you care about.

- **Reads inside the workspace are automatic. Everything mutating asks.** Writes, shell commands
  and screen captures always prompt; anything touching a path outside the workspace prompts every
  single time and can never be granted for a session.
- **"Allow for this session" is narrow and temporary.** A grant is keyed to a scope — a directory
  subtree, or one shell program by name — never to a tool as a whole. Grants are never written to
  disk, and they die with the session. There is no "allow forever".
- **`shell_exec` does not use a shell.** It takes a program and an argument vector and spawns them
  directly, so there is no metacharacter or quoting layer to defeat. The risk badges on commands
  like `rm` or `curl` are *presentational* — they change the wording of the prompt, not what is
  permitted.
- **There is no sandbox.** Approved tools run as you, with your privileges and environment. The
  real boundary is that you read the exact path, program, arguments and working directory before
  approving. Treat every approval as if you were typing the command yourself.
- **Screen captures are never auto-allowed.** A capture can contain anything on your display.
  Images are written under the app data directory, never into the workspace; the audit log records
  the path, dimensions and a SHA-256, never the image.
- **Keys stay out of the WebView.** The API key lives in the OS keyring (or an environment
  variable) and is read only by the Rust runtime. The UI receives a masked hint — last four
  characters — and nothing else. Never put a key in `localStorage`.
- **Every tool call is audited**, allowed or denied, one JSON line each, with the policy reason
  and the outcome.

## Troubleshooting

### Windows

- **WebView2 missing.** Aegis renders through WebView2, which ships with Windows 11 and current
  Windows 10 but is not guaranteed. If the window opens blank or the app refuses to start, install
  the *Evergreen WebView2 Runtime* from Microsoft. Installers built by `pnpm tauri build` are
  configured to download it automatically when absent.
- **SmartScreen warning on a built binary.** Expected. The MVP does not code-sign, so
  "Windows protected your PC" appears for unsigned output. Signing is out of scope.
- **Antivirus breaks the Rust build.** Real-time scanners — ESET especially — lock the small
  binaries `rustc` and `cargo` produce constantly. Symptoms are `rustup` looping on
  `retrying renaming …`, a corrupted toolchain, or builds that take minutes instead of seconds.
  Add performance exclusions for `%USERPROFILE%\.rustup\`, `%USERPROFILE%\.cargo\` and this
  repo's `src-tauri\target\`.
- **`.cmd` shims via `shell_exec`.** `pnpm`, `npm` and `yarn` on Windows are `.cmd` files, which
  `CreateProcess` cannot launch directly. Aegis resolves the program through `PATHEXT` and
  re-invokes `.cmd` targets through `cmd /c`, keeping arguments as a vector. This is a launcher
  detail, not a shell: your arguments are still not parsed by `cmd`.

### macOS

- **Screenshots return a black or desktop-only image.** The app needs Screen Recording consent,
  which macOS only prompts for on the first capture attempt. Grant it under *System Settings →
  Privacy & Security → Screen Recording*. Under `pnpm tauri dev` the consent is attached to the
  dev binary and is re-evaluated whenever that binary changes, so it can need re-granting after a
  rebuild.
- **Keychain re-prompts after every rebuild.** Each dev build changes the ad-hoc signature, which
  invalidates the keychain ACL. Use the `AEGIS_API_KEY` environment variable during development;
  Settings will honestly report `key_source: "env"`.

### Linux

- **The window is blank on Wayland + NVIDIA.** A known WebKitGTK DMA-BUF renderer bug. Launch
  with `WEBKIT_DISABLE_DMABUF_RENDERER=1`.
- **No tray icon.** Requires `libayatana-appindicator3`, and GNOME additionally needs the
  AppIndicator shell extension. Aegis stays fully usable without a tray — the window is primary.
- **Screenshots fail on Wayland.** Wayland blocks direct framebuffer capture by design. Aegis
  reports `E_SCREEN_PERMISSION` rather than returning a black image. X11 sessions work.
- **No keyring.** Without a running Secret Service (gnome-keyring, kwallet), use the
  `AEGIS_API_KEY` environment variable. This is normal on headless and minimal window managers.

## License

MIT
