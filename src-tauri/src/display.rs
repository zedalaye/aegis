//! Linux WebView display workarounds (PLAN 5.3).
//!
//! WebKitGTK is the window. On some Linux setups — NVIDIA + Wayland, and
//! WSL2/WSLg especially — its DMA-BUF / GBM path fails (`ZINK: failed to
//! choose pdev`, `egl: failed to create dri2 screen`) and the process stays
//! up with a taskbar icon and nothing on screen. The documented workaround
//! is environment variables that have to exist **before** GTK initialises,
//! which is `Builder::build`, not `setup`. [`prepare`] runs at the top of
//! [`crate::run`] for that reason.
//!
//! Nothing here overrides a variable the user already exported, so a
//! workaround can still be turned off from the shell.

use tauri::{Runtime, WebviewWindow};

/// Applies the Linux WebView workarounds. No-op on other platforms.
pub fn prepare() {
    #[cfg(target_os = "linux")]
    linux::prepare();
}

/// Whether this process is running under WSL / WSLg.
pub fn is_wsl() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::is_wsl()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Pins the main window onto the WSLg screen. `center: true` plus a
/// nonsense X11 screen size parks it off-screen; WSLg still puts a
/// taskbar button on the Windows side, which looks like "the app is
/// running but has no window".
pub fn place_main<R: Runtime>(window: &WebviewWindow<R>) {
    #[cfg(target_os = "linux")]
    linux::place_main(window);
    #[cfg(not(target_os = "linux"))]
    let _ = window;
}

/// Logs what GTK thinks the window is, so a "taskbar icon, no window"
/// report can tell an off-screen window from a 0×0 one from a hide.
pub fn describe_main<R: Runtime>(window: &WebviewWindow<R>) {
    let visible = window.is_visible().ok();
    let minimized = window.is_minimized().ok();
    let size = window.inner_size().ok();
    let pos = window.outer_position().ok();
    tracing::info!(?visible, ?minimized, ?size, ?pos, "main window");
}

#[cfg(target_os = "linux")]
mod linux {
    use tauri::{PhysicalPosition, PhysicalSize, Runtime, WebviewWindow};

    /// Env vars WebKitGTK / GDK read at init. Set only when unset.
    const DMABUF: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    const COMPOSITING: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";
    const GDK_BACKEND: &str = "GDK_BACKEND";
    const LIBGL_SOFTWARE: &str = "LIBGL_ALWAYS_SOFTWARE";
    const WEBKIT_SANDBOX: &str = "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS";

    pub(super) fn prepare() {
        // PLAN 5.3: blank window on NVIDIA/Wayland. Harmless elsewhere; the
        // user can pre-set the variable to keep DMA-BUF on a machine where
        // it works.
        set_if_unset(DMABUF, "1");

        if is_wsl() {
            // WSLg advertises Wayland *and* X11. GTK3 prefers Wayland, then
            // WebKit tries GBM against a virtual GPU that ZINK cannot use.
            // X11 + llvmpipe + no bwrap sandbox is the path that can draw.
            // Do not set `WEBKIT_FORCE_SANDBOX=0`: current WebKitGTK ignores
            // it and warns. Do not set `GTK_CSD=0`: without client chrome
            // and with an unpainted WebView the window has nothing opaque
            // for WSLg to show — a taskbar icon and no pixels.
            tracing::info!("WSL detected; WebKitGTK will use X11, software GL, no sandbox");
            set_if_unset(GDK_BACKEND, "x11");
            set_if_unset(COMPOSITING, "1");
            set_if_unset(LIBGL_SOFTWARE, "1");
            set_if_unset(WEBKIT_SANDBOX, "1");
        }
    }

    pub(super) fn place_main<R: Runtime>(window: &WebviewWindow<R>) {
        if !is_wsl() {
            return;
        }
        // Physical, not logical: a logical (64,64) reported as (0,0), which
        // RAIL then treats as "not placed".
        if let Err(err) = window.set_position(PhysicalPosition::new(120, 80)) {
            tracing::warn!(%err, "could not pin the WSL window on screen");
        }
        if let Err(err) = window.set_size(PhysicalSize::new(1100, 720)) {
            tracing::warn!(%err, "could not size the WSL window");
        }
        if let Err(err) = window.set_always_on_top(true) {
            tracing::warn!(%err, "could not raise the WSL window above others");
        }
    }

    fn set_if_unset(key: &str, value: &str) {
        if std::env::var_os(key).is_some() {
            return;
        }
        tracing::info!(key, value, "set for WebKitGTK");
        // `set_var` became `unsafe` in 1.87 (data race with other threads).
        // This runs from `main` before Tauri starts the event loop.
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var(key, value);
        }
    }

    /// WSLg and classic WSL both set `WSL_DISTRO_NAME`; the kernel release
    /// string is the fallback for a stripped environment.
    pub(super) fn is_wsl() -> bool {
        if std::env::var_os("WSL_DISTRO_NAME").is_some()
            || std::env::var_os("WSL_INTEROP").is_some()
        {
            return true;
        }
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .is_some_and(|s| looks_like_wsl_release(&s))
    }

    pub(super) fn looks_like_wsl_release(osrelease: &str) -> bool {
        let lower = osrelease.to_ascii_lowercase();
        lower.contains("microsoft") || lower.contains("wsl")
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux::looks_like_wsl_release;

    #[test]
    fn wsl2_kernel_release_is_recognised() {
        assert!(looks_like_wsl_release("5.15.167.4-microsoft-standard-WSL2"));
        assert!(looks_like_wsl_release("6.6.87.2-microsoft-standard-WSL2"));
    }

    #[test]
    fn a_real_linux_kernel_is_not_wsl() {
        assert!(!looks_like_wsl_release("6.8.0-40-generic"));
        assert!(!looks_like_wsl_release("6.12.9-arch1-1"));
    }
}
