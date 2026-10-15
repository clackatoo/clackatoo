//! The token the desktop launched the window with, which lets it take
//! focus as it opens and ends the launcher's spinner.
//!
//! A launcher that knows an app reports its startup (`StartupNotify=true`
//! in the desktop entry) hands the app a token in the environment:
//! `XDG_ACTIVATION_TOKEN` for Wayland, `DESKTOP_STARTUP_ID` for X11, and
//! GNOME sets both, to the same token. The window opens with it. On
//! Wayland, winit activates the new window with it (xdg-activation), and on
//! X11 it sets `_NET_STARTUP_ID` and tells the desktop the startup is over.
//! Either way the window takes focus and the spinner stops.
//!
//! A token is spent once used, so nothing the window starts may inherit
//! it: not the daemon, nor so every shell, nor `xdg-open` or `clackatoo
//! update`. It's taken out of the environment before any of those start,
//! and before any other thread does, which is when changing the
//! environment is safe.
//!
//! winit's own `read_token_from_env` can't be used for this: it reads the
//! environment only once the event loop runs, which is after the daemon
//! may have started, and after the font and GPU threads have, so it's too
//! late to clear it then.

use winit::window::ActivationToken;

/// The variable launchers set for Wayland.
const WAYLAND_VAR: &str = "XDG_ACTIVATION_TOKEN";

/// The variable launchers set for X11.
const X11_VAR: &str = "DESKTOP_STARTUP_ID";

/// The tokens the desktop launched this process with, if it did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchToken {
    /// `XDG_ACTIVATION_TOKEN`'s.
    wayland: Option<String>,
    /// `DESKTOP_STARTUP_ID`'s.
    x11: Option<String>,
}

impl LaunchToken {
    /// Takes the tokens out of the environment, so nothing this process
    /// starts afterwards inherits them.
    ///
    /// Changing the environment is only safe while no other thread could
    /// be reading it, so this must come before this process starts any
    /// thread.
    pub(crate) fn take() -> Self {
        Self::take_from(
            |name| std::env::var(name).ok(),
            winit::platform::startup_notify::reset_activation_token_env,
        )
    }

    /// Reads the tokens with `var`, then clears them with `reset`. An empty
    /// token is none.
    fn take_from(var: impl Fn(&str) -> Option<String>, reset: impl FnOnce()) -> Self {
        let read = |name| var(name).filter(|token| !token.is_empty());
        let tokens = Self {
            wayland: read(WAYLAND_VAR),
            x11: read(X11_VAR),
        };
        reset();
        tokens
    }

    /// The token for a window on Wayland or on X11, as winit itself picks:
    /// only the variable for the display the window is on counts.
    pub(crate) fn for_window(self, wayland: bool) -> Option<ActivationToken> {
        let token = if wayland { self.wayland } else { self.x11 };
        token.map(ActivationToken::from_raw)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::process::Command;

    use super::*;

    /// Takes the tokens from a made-up environment, saying what's left.
    fn take(env: &[(&str, &str)]) -> (LaunchToken, Vec<String>) {
        let env: RefCell<HashMap<String, String>> = RefCell::new(
            (env.iter())
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        );
        let taken = LaunchToken::take_from(
            |name| env.borrow().get(name).cloned(),
            || {
                let mut env = env.borrow_mut();
                env.remove(WAYLAND_VAR);
                env.remove(X11_VAR);
            },
        );
        let mut left: Vec<String> = env.into_inner().into_keys().collect();
        left.sort();
        (taken, left)
    }

    fn raw(token: Option<ActivationToken>) -> Option<String> {
        token.map(ActivationToken::into_raw)
    }

    #[test]
    fn both_tokens_are_read_before_the_environment_is_cleared() {
        let (taken, left) = take(&[
            ("XDG_ACTIVATION_TOKEN", "wayland-token"),
            ("DESKTOP_STARTUP_ID", "x11-token"),
            ("HOME", "/home/me"),
        ]);
        assert_eq!(
            taken,
            LaunchToken {
                wayland: Some("wayland-token".into()),
                x11: Some("x11-token".into()),
            }
        );
        assert_eq!(left, ["HOME"]);
    }

    #[test]
    fn the_window_takes_the_token_for_its_display() {
        let (taken, _) = take(&[
            ("XDG_ACTIVATION_TOKEN", "wayland-token"),
            ("DESKTOP_STARTUP_ID", "x11-token"),
        ]);
        assert_eq!(
            raw(taken.clone().for_window(true)).as_deref(),
            Some("wayland-token")
        );
        assert_eq!(raw(taken.for_window(false)).as_deref(), Some("x11-token"));
        // A launcher that set only the other display's variable gave this
        // one nothing.
        let (x11_only, _) = take(&[("DESKTOP_STARTUP_ID", "x11-token")]);
        assert_eq!(raw(x11_only.for_window(true)), None);
    }

    #[test]
    fn launched_some_other_way_there_is_no_token() {
        let (taken, left) = take(&[]);
        assert_eq!(taken, LaunchToken::default());
        assert!(left.is_empty());
        assert_eq!(raw(taken.clone().for_window(true)), None);
        assert_eq!(raw(taken.for_window(false)), None);
        let (empty, _) = take(&[("XDG_ACTIVATION_TOKEN", ""), ("DESKTOP_STARTUP_ID", "")]);
        assert_eq!(empty, LaunchToken::default());
    }

    /// Set in the copy of the tests [`children_inherit_no_token`] runs, so
    /// [`take_the_real_token_then_start_a_child`] does its work there only.
    const CHILD: &str = "CLACKATOO_TEST_LAUNCH_TOKEN_CHILD";

    /// Runs this test binary again, launched with tokens as a launcher
    /// would, to take them from its real environment. The copy runs just
    /// the one test, on the thread libtest starts for it while its main
    /// thread only waits, so nothing else reads the environment as it
    /// changes, as the window ensures by taking them before any thread
    /// starts.
    #[test]
    fn children_inherit_no_token() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "launch::tests::take_the_real_token_then_start_a_child",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(WAYLAND_VAR, "wayland-token")
            .env(X11_VAR, "x11-token")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("1 passed"), "{stdout}\n{stderr}");
    }

    /// In the copy [`children_inherit_no_token`] runs: takes the tokens as
    /// the window does, then starts a shell, as the daemon's start and every
    /// other child does, and checks it sees neither.
    #[test]
    fn take_the_real_token_then_start_a_child() {
        if std::env::var_os(CHILD).is_none() {
            return;
        }
        let taken = LaunchToken::take();
        assert_eq!(
            taken,
            LaunchToken {
                wayland: Some("wayland-token".into()),
                x11: Some("x11-token".into()),
            }
        );
        assert_eq!(std::env::var_os(WAYLAND_VAR), None);
        assert_eq!(std::env::var_os(X11_VAR), None);
        let output = Command::new("sh")
            .args([
                "-c",
                "printf '%s %s' \"${XDG_ACTIVATION_TOKEN-unset}\" \"${DESKTOP_STARTUP_ID-unset}\"",
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unset unset");
    }
}
