//! Open a web link in the OS browser (PLAN 7.20).
//!
//! A command, not a model tool: the operator's click is the gate. The WebView
//! gains no opener, `fs:` or `shell:` permission and never navigates.
//!
//! * [`target`] accepts `http` and `https` only, and normalizes the URL.
//! * [`open`] hands it to the OS URL handler as one argument, never a shell
//!   string — the way [`reveal`](crate::reveal) spawns the file manager.

use std::process::{Command, Stdio};

use reqwest::Url;

use crate::error::{AppError, AppResult};

/// Longest URL accepted. A link a person clicks is not a payload.
const MAX_LEN: usize = 4096;

/// The URL the browser should open, or why it must not.
pub fn target(raw: &str) -> AppResult<Url> {
    let raw = raw.trim();
    let refuse = |reason: &str| AppError::OpenUrl {
        reason: reason.to_owned(),
    };

    if raw.is_empty() {
        return Err(refuse("the link is empty"));
    }
    if raw.len() > MAX_LEN {
        return Err(refuse("the link is too long"));
    }
    // The parser would drop or encode these; a link that carries them was not
    // written to be clicked.
    if raw.chars().any(char::is_control) {
        return Err(refuse("the link holds control characters"));
    }

    let url = Url::parse(raw).map_err(|_| refuse("it is not a web address"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(refuse("only http and https links open in the browser"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(refuse("the link names no host"));
    }
    // Credentials in a URL are a phishing shape (`https://bank.com@evil`).
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refuse("the link carries a user name or password"));
    }
    Ok(url)
}

/// Opens `url` in the OS browser.
///
/// Fire-and-forget, like the file manager: the handler outlives the spawn.
pub fn open(url: &Url) -> AppResult<()> {
    let mut command = browser_command(url.as_str());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().map_err(|error| {
        tracing::error!(error = %error, "browser would not start");
        AppError::Internal {
            what: "could not open the browser",
        }
    })?;
    tracing::debug!(host = url.host_str().unwrap_or_default(), "link opened");
    Ok(())
}

fn browser_command(url: &str) -> Command {
    #[cfg(windows)]
    {
        // `url.dll,FileProtocolHandler` is the shell's URL handler without a
        // shell: no `cmd /c start`, whose parser would read `&` in a query.
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        command
    }

    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(url);
        command
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(raw: &str) -> String {
        match target(raw) {
            Err(AppError::OpenUrl { reason }) => reason,
            other => panic!("expected OpenUrl for {raw:?}, got {other:?}"),
        }
    }

    #[test]
    fn http_and_https_are_accepted_and_normalized() {
        assert_eq!(
            target("https://example.com/a b?q=1&r=\"2\"")
                .expect("https")
                .as_str(),
            "https://example.com/a%20b?q=1&r=%222%22"
        );
        assert_eq!(
            target("  HTTP://Example.COM  ").expect("http").as_str(),
            "http://example.com/"
        );
    }

    #[test]
    fn every_other_scheme_is_refused() {
        for raw in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "data:text/html,<script>1</script>",
            "tauri://localhost/",
            "asset://localhost/x",
            "ms-settings:privacy",
            "mailto:someone@example.com",
            "ftp://example.com/",
        ] {
            assert_eq!(
                refused(raw),
                "only http and https links open in the browser",
                "{raw}"
            );
        }
    }

    #[test]
    fn relative_and_malformed_links_are_refused() {
        assert_eq!(refused(""), "the link is empty");
        assert_eq!(refused("docs/readme.md"), "it is not a web address");
        assert_eq!(refused("//example.com"), "it is not a web address");
        assert_eq!(refused("https://"), "it is not a web address");
    }

    #[test]
    fn credentials_and_control_characters_are_refused() {
        assert_eq!(
            refused("https://bank.example@evil.example/"),
            "the link carries a user name or password"
        );
        assert_eq!(
            refused("https://example.com/\u{7}"),
            "the link holds control characters"
        );
        assert_eq!(
            refused(&format!("https://example.com/{}", "a".repeat(MAX_LEN))),
            "the link is too long"
        );
    }

    #[test]
    fn the_url_is_one_argument() {
        let command = browser_command("https://example.com/?a=1&b=2");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(
            args.last().and_then(|arg| arg.to_str()),
            Some("https://example.com/?a=1&b=2")
        );
    }
}
