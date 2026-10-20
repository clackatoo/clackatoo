//! `clackatoo display`: the desktop's display the daemon on this machine
//! gives the programs it starts, and where it came from (see
//! [`clackatoo_daemon::display`]). It reads the file the daemon writes
//! for its shells, so it says just what they follow.

use std::fmt::Write as _;

use clackatoo_daemon::RuntimeDir;
use clackatoo_daemon::display::{self, Origin, Written};

/// Prints the display, or fails if no daemon is running, or it isn't
/// following one.
pub(crate) fn show() -> anyhow::Result<()> {
    let runtime = RuntimeDir::for_current_user();
    // What a daemon that has stopped wrote is no one's.
    let socket = runtime.socket_path();
    if std::os::unix::net::UnixStream::connect(&socket).is_err() {
        anyhow::bail!(
            "no daemon is listening on {}; start one with `clackatoo daemon`",
            socket.display()
        );
    }
    let file = runtime.path().join("shell/display");
    let Some(written) = display::read_file(&file)? else {
        anyhow::bail!("the daemon isn't following the desktop's display: CLACKATOO_DISPLAY is off");
    };
    print!("{}", shown(&written));
    Ok(())
}

/// What's printed: where it came from, then a `NAME=VALUE` line each.
fn shown(written: &Written) -> String {
    let from = match written.origin {
        Origin::Systemd => "from: systemd (the desktop you're logged in to)",
        Origin::Inherited => "from: inherited (the daemon's own environment)",
        Origin::None => "from: none (no desktop's display, so programs get none)",
    };
    let mut text = format!("{from}\n");
    for (name, value) in &written.values {
        let _ = writeln!(text, "{name}={value}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_says_where_the_display_came_from_then_each_value() {
        let written = Written {
            origin: Origin::Systemd,
            values: vec![("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned())],
        };
        assert_eq!(
            shown(&written),
            "from: systemd (the desktop you're logged in to)\nWAYLAND_DISPLAY=wayland-0\n"
        );
        let none = Written {
            origin: Origin::None,
            values: Vec::new(),
        };
        assert!(shown(&none).starts_with("from: none"));
    }
}
