//! `clackatoo priority`: how the daemon on this machine took its priority
//! and its place on the machine as it started (see
//! [`clackatoo_daemon::priority`]), as it wrote it in its runtime
//! directory, and what lingering would change.

use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::priority::{NEEDS_LINGER, STATUS_FILE};

/// What's said in place of [`NEEDS_LINGER`].
const LINGER: &str = "\
Only lingering stands between the daemon and a place of its own, where agents' and \
builds' work can't slow it. Your systemd, which the place would be part of, stops when \
your last login ends, unless you linger; the daemon outlives your logins, so it won't \
take a place that wouldn't. To linger:

    loginctl enable-linger

then `clackatoo upgrade`, which keeps your terminals, gives the daemon its place.
`daemon.machine.priority = false` in daemon/init.lua stops the daemon asking, and windows saying
so.
";

/// Prints what the daemon said, or fails if no daemon is running, or it
/// hasn't said.
pub(crate) fn show() -> anyhow::Result<()> {
    let runtime = RuntimeDir::for_current_user();
    let socket = runtime.socket_path();
    if clackatoo_daemon::client::connect_socket(&socket).is_err() {
        anyhow::bail!(
            "no daemon is listening on {}; start one with `clackatoo daemon`",
            socket.display()
        );
    }
    let file = runtime.path().join(STATUS_FILE);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => anyhow::bail!(
            "the daemon hasn't said yet: it asks as it starts, or it's from before it said \
             (`clackatoo upgrade` moves it onto this build)"
        ),
        Err(err) => anyhow::bail!("{} couldn't be read: {err}", file.display()),
    };
    print!("{}", shown(&text));
    Ok(())
}

/// What's printed: the daemon's lines, with what lingering would change
/// in place of [`NEEDS_LINGER`].
fn shown(text: &str) -> String {
    let mut shown = String::new();
    for line in text.lines() {
        if line == NEEDS_LINGER {
            shown.push('\n');
            shown.push_str(LINGER);
        } else {
            shown.push_str(line);
            shown.push('\n');
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lingering_is_explained_where_the_daemon_says_it_needs_it() {
        let text = format!(
            "daemon: 7\npriority: nice -15, through RealtimeKit\nplace: where it started: you don't linger\n{NEEDS_LINGER}\n"
        );
        let said = shown(&text);
        assert!(said.starts_with("daemon: 7\npriority: nice -15"), "{said}");
        assert!(said.contains("loginctl enable-linger"), "{said}");
        assert!(!said.contains(NEEDS_LINGER), "{said}");
        let placed =
            "daemon: 7\npriority: nice -15, through RealtimeKit\nplace: app-clackatoo-7.scope\n";
        assert_eq!(shown(placed), placed);
    }
}
