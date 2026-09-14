# Troubleshooting

## Windows

- **`pnpm` is blocked in PowerShell** (`UnauthorizedAccess`). Corepack installs a `pnpm.ps1` shim
  that the default execution policy refuses. Run `pnpm.cmd tauri dev`, or allow local scripts once:
  `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`.
- **Blank window or no start.** WebView2 may be missing: install the *Evergreen WebView2 Runtime*.
  Installers built by `pnpm tauri build` download it when absent.
- **`Failed to unregister class Chrome_WidgetWin_0. Error = 1412` on quit.** Chromium's own
  shutdown message (`ERROR_CLASS_HAS_WINDOWS`), also printed by Chrome. Harmless.
- **SmartScreen warns about a built binary.** Builds are unsigned.
- **The Rust build loops or crawls.** Real-time antivirus (ESET especially) locks the files `rustc`
  and `cargo` write: `rustup` retries renames, toolchains corrupt, builds take minutes. Exclude
  `%USERPROFILE%\.rustup\`, `%USERPROFILE%\.cargo\` and `src-tauri\target\`.
- **`pnpm`, `npm`, `npx` through `shell_exec` or a connector.** These are `.cmd` shims, which
  `CreateProcess` cannot start. Aegis resolves them through `PATHEXT` and launches them via
  `cmd.exe` with the arguments escaped by the Rust standard library; the arguments are still a
  vector, not a command line.
- **`echo` and `dir` are not found.** They are `cmd.exe` builtins, not programs. Run `cmd` with
  `["/c", "dir"]` — and note that `cmd` then parses those arguments itself.
- **Accented output.** Console programs write the OEM code page (850, 437, …). Aegis tries UTF-8
  first and falls back to that code page.
- **Colours and progress bars disappear.** ANSI escape sequences are stripped from what the model
  and you see, so progress bars do not stack up.
- **Stop kills the whole process tree** (`taskkill /T`). On macOS and Linux only the process Aegis
  started is killed; a script that spawned a build may leave it running.
- **`E_EXEC_HOST` on a WSL project.** The distribution was removed or renamed, WSL is not running,
  or it cannot see the workspace. Pick another host or *This computer* in the sidebar. Aegis never
  falls back on its own.
- **WSL commands start slower.** Each call first checks the working directory inside the
  distribution (about 150 ms warm), because `wsl --cd` silently starts in `/` otherwise.

## macOS

- **Captures are black or show only the desktop.** Grant *System Settings → Privacy & Security →
  Screen Recording*. Under `pnpm tauri dev` the permission is tied to the dev binary and may need
  granting again after a rebuild. A fully blank capture is refused as `E_SCREEN_PERMISSION`; a
  wallpaper-only capture cannot be told apart from a tidy desktop and is not detected.
- **Keychain prompts after every rebuild.** Each dev build changes the ad-hoc signature. Use
  `AEGIS_API_KEY` during development; Settings shows the key source as `env`.

## Linux

- **Blank window on Wayland + NVIDIA.** A WebKitGTK DMA-BUF renderer bug. Aegis sets
  `WEBKIT_DISABLE_DMABUF_RENDERER=1` when it is unset; export another value to keep DMA-BUF.
- **No tray icon.** Install the runtime library `libayatana-appindicator3-1`; GNOME also needs the
  AppIndicator extension. Without a tray the window is the only surface, and closing it quits.
- **Captures on Wayland.** wlroots compositors (`wlr-screencopy`) work; GNOME and KDE usually refuse,
  and the call fails with `E_SCREEN_PERMISSION`. X11 sessions work.
- **Linking fails with `-lgbm`, `-lEGL` or `-lwayland-client`.** The screen-capture crate links
  them even if you never capture: install `libgbm-dev`, `libegl-dev`, `libwayland-dev` (see the apt
  line in the README).
- **`pkg-config` cannot find `libsoup-3.0` or `webkit2gtk-4.1`.** Install the `-dev` packages.
  WebKitGTK 4.0 / soup2 is the Tauri 1 stack and will not do.
- **No keyring.** Without a running Secret Service (gnome-keyring, kwallet), use `AEGIS_API_KEY`.
- **WSL2 (WSLg): a taskbar icon but no window.** `DRI3 error: Could not get DRI3 device` means
  WebKit has no GPU. Under WSL Aegis skips the tray, sets X11, software GL and disables the WebKit
  sandbox before GTK starts (unless you exported those variables), and raises the window after the
  compositor maps it. The log should show `WSL: skipping the tray`, then
  `WSL: delayed raise after compositor map`. If the window is reported visible and still not seen,
  use Alt+Tab or the taskbar icon; check WSLg itself with `xeyes`. *Hide to tray* is not offered
  there, since nothing would bring the window back.
