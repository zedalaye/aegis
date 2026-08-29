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

/// Applies the Linux WebView workarounds. No-op on other platforms.
pub fn prepare() {
    #[cfg(target_os = "linux")]
    linux::prepare();
}

#[cfg(target_os = "linux")]
mod linux {
    /// Env vars WebKitGTK / GDK read at init. Set only when unset.
    const DMABUF: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    const COMPOSITING: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";
    const GDK_BACKEND: &str = "GDK_BACKEND";

    pub(super) fn prepare() {
        // PLAN 5.3: blank window on NVIDIA/Wayland. Harmless elsewhere; the
        // user can pre-set the variable to keep DMA-BUF on a machine where
        // it works.
        set_if_unset(DMABUF, "1");

        if is_wsl() {
            // WSLg advertises Wayland *and* X11. GTK3 prefers Wayland, then
            // WebKit tries GBM against a virtual GPU that ZINK cannot use,
            // and the RAIL window Windows shows in the taskbar never paints.
            // X11 + software compositing is the path that actually draws.
            tracing::info!("WSL detected; WebKitGTK will use X11 without compositing");
            set_if_unset(GDK_BACKEND, "x11");
            set_if_unset(COMPOSITING, "1");
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
