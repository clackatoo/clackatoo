//! New releases, as the window sees them.
//!
//! A while after it opens, the window looks for a newer release, at most
//! once a day (see `clackatoo_update::check`), unless the `updatecheck`
//! option is off, and says so quietly in the bar: "clackatoo 2026.10.3 is
//! out: :update". It never installs anything itself: `:update` runs
//! `clackatoo update`, which does.
//!
//! It also says when the daemon is a different release from the window,
//! and which side is older, since a window on another machine is easily
//! left behind, or leaves the daemon behind.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime};

use clackatoo_update::{Source, Version, check};
use winit::event_loop::EventLoopProxy;

use crate::session::SessionEvent;

/// How long after opening the window looks for a release: long enough for
/// the config to have set `updatecheck`, and never while it's opening.
const LOOK_AFTER: Duration = Duration::from_secs(10);

/// Sends [`SessionEvent::UpdateDue`] a while after the window opens.
pub(crate) fn after_opening(proxy: EventLoopProxy<SessionEvent>) {
    let started = std::thread::Builder::new()
        .name("update-wait".into())
        .spawn(move || {
            std::thread::sleep(LOOK_AFTER);
            let _ = proxy.send_event(SessionEvent::UpdateDue);
        });
    if let Err(err) = started {
        log::warn!("won't look for new releases: {err}");
    }
}

/// Looks for a newer release on a thread of its own, and sends
/// [`SessionEvent::NewRelease`] if there is one.
pub(crate) fn look(proxy: EventLoopProxy<SessionEvent>) {
    let started = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let Some(state) = check::state_path() else {
                return;
            };
            match check::newer_release(&Source::from_env(), &state, SystemTime::now()) {
                Ok(Some(version)) => {
                    let _ = proxy.send_event(SessionEvent::NewRelease(version.to_string()));
                }
                Ok(None) => {}
                Err(err) => log::info!("couldn't look for a new release: {err}"),
            }
        });
    if let Err(err) = started {
        log::warn!("won't look for new releases: {err}");
    }
}

/// Who asked for an update: the user, with `:update`, or the window, for a
/// daemon from a newer build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    User,
    ForDaemon,
}

/// What came of `clackatoo update`.
#[derive(Debug)]
pub(crate) struct Updated {
    pub(crate) asked: Asked,
    /// What to say of it, or why it failed.
    pub(crate) said: Result<String, String>,
    /// Whether a new build was put in place, even if something after,
    /// such as moving this machine's daemon onto it, failed.
    pub(crate) installed: bool,
}

/// Runs `program update` on a thread of its own, for `asked`, and sends
/// [`SessionEvent::Updated`] with what came of it.
pub(crate) fn update(program: PathBuf, asked: Asked, proxy: EventLoopProxy<SessionEvent>) {
    let started = std::thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            let (said, installed) = match Command::new(&program).arg("update").output() {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let said = outcome(
                        output.status.success(),
                        &stdout,
                        &String::from_utf8_lossy(&output.stderr),
                    );
                    (said, installed(&stdout, clackatoo_update::CURRENT))
                }
                Err(err) => (
                    Err(format!("couldn't run {}: {err}", program.display())),
                    false,
                ),
            };
            let updated = Updated {
                asked,
                said,
                installed,
            };
            let _ = proxy.send_event(SessionEvent::Updated(updated));
        });
    if let Err(err) = started {
        log::warn!("couldn't update: {err}");
    }
}

/// Whether what `clackatoo update` printed, `stdout`, says a build other
/// than this window's, `ours`, is in place of it: one it put there now, or
/// one already there, as when this window's program was updated after it
/// opened, by another window or `:update`, and said so as up to date.
fn installed(stdout: &str, ours: &str) -> bool {
    stdout.lines().any(|line| {
        let up_to_date = (line.strip_prefix("clackatoo "))
            .and_then(|rest| rest.strip_suffix(" is up to date"))
            .or_else(|| {
                line.strip_prefix("this is clackatoo ")?
                    .strip_suffix(" already")
            });
        line.starts_with("installed clackatoo ")
            || up_to_date.is_some_and(|version| version != ours)
    })
}

/// What the bar says came of `clackatoo update`, from what it printed.
fn outcome(succeeded: bool, stdout: &str, stderr: &str) -> Result<String, String> {
    let last = |text: &str| {
        text.lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            .map(str::to_owned)
    };
    if !succeeded {
        let reason = last(stderr).unwrap_or_else(|| "clackatoo update failed".into());
        return Err(reason
            .strip_prefix("clackatoo: ")
            .unwrap_or(&reason)
            .to_owned());
    }
    if let Some(line) = stdout.lines().find(|line| line.contains("up to date")) {
        return Ok(line.trim().to_owned());
    }
    match stdout
        .lines()
        .find_map(|line| line.strip_prefix("installed clackatoo "))
    {
        Some(rest) => {
            let version = rest.split_whitespace().next().unwrap_or_default();
            Ok(format!(
                "clackatoo {version} is installed: open the window again to use it"
            ))
        }
        None => Ok(last(stdout).unwrap_or_else(|| "clackatoo is updated".into())),
    }
}

/// What the window knows of releases, for the bar to say.
#[derive(Debug)]
pub(crate) struct Known {
    /// The newer release that's out, if one is.
    newer: Option<String>,
    /// How the daemon and the window differ, if they do.
    skew: Option<String>,
    /// The program `:update` runs: this one, as it was when the window
    /// opened, since updating renames the old one away.
    pub(crate) program: Option<PathBuf>,
    /// Whether `clackatoo update` is running for the window: one at a
    /// time, so two never race to put a build in place.
    pub(crate) running: bool,
}

impl Known {
    pub(crate) fn new() -> Self {
        Self {
            newer: None,
            skew: None,
            program: std::env::current_exe()
                .inspect_err(|err| log::warn!("`:update` can't find this program: {err}"))
                .ok(),
            running: false,
        }
    }

    /// Notes that the daemon, on `host` if it's on another machine, is
    /// release `version`. Returns what to say about it, if anything new.
    pub(crate) fn daemon_is(
        &mut self,
        version: Option<&str>,
        host: Option<&str>,
    ) -> Option<String> {
        let skew =
            version.and_then(|version| skew_notice(clackatoo_update::CURRENT, version, host));
        let new = (skew != self.skew).then(|| skew.clone()).flatten();
        self.skew = skew;
        new
    }

    /// Notes that `version` is out. Returns what to say about it, if it's
    /// news.
    pub(crate) fn newer_out(&mut self, version: String) -> Option<String> {
        if self.newer.as_ref() == Some(&version) {
            return None;
        }
        let notice = release_notice(&version);
        self.newer = Some(version);
        Some(notice)
    }

    /// Notes that this machine has been updated: the window no longer
    /// needs to say a release is out.
    pub(crate) fn updated(&mut self) {
        self.newer = None;
    }

    /// What the bar says when nothing else has a say: how the daemon
    /// differs, which matters more, or else that a release is out.
    pub(crate) fn notice(&self) -> Option<String> {
        (self.skew.clone()).or_else(|| self.newer.as_deref().map(release_notice))
    }
}

/// What the bar says about a newer release.
pub(crate) fn release_notice(version: &str) -> String {
    format!("clackatoo {version} is out: :update")
}

/// What the bar says when the daemon, `theirs`, is another release than
/// this window, `ours`, with how to bring the older up to date. The daemon
/// is on `host`, if it's on another machine. `None` if they're the same.
pub(crate) fn skew_notice(ours: &str, theirs: &str, host: Option<&str>) -> Option<String> {
    if ours == theirs {
        return None;
    }
    let (Ok(ours_version), Ok(theirs_version)) =
        (ours.parse::<Version>(), theirs.parse::<Version>())
    else {
        return Some(match host {
            Some(host) => {
                format!("the daemon on {host} is clackatoo {theirs}, and this window {ours}")
            }
            None => format!("the daemon is clackatoo {theirs}, and this window {ours}"),
        });
    };
    Some(match (theirs_version < ours_version, host) {
        (true, Some(host)) => format!(
            "the daemon on {host} is clackatoo {theirs}, older than this window's {ours}: :update {host}"
        ),
        (true, None) => format!(
            "the daemon is clackatoo {theirs}, older than this window's {ours}: `clackatoo upgrade` \
             moves it onto this build"
        ),
        (false, Some(host)) => format!(
            "this window is clackatoo {ours}, older than the daemon on {host}'s {theirs}: :update, \
             then open the window again"
        ),
        (false, None) => format!(
            "this window is clackatoo {ours}, older than the daemon's {theirs}: open the window \
             again to use it"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_is_in_place_if_one_was_put_there_or_was_already_newer_than_this_window() {
        assert!(installed(
            "downloading clackatoo 2026.10.5…\ninstalled clackatoo 2026.10.5 at /x\n",
            "2026.10.4"
        ));
        // This window's program was updated after it opened.
        assert!(installed(
            "clackatoo 2026.10.5 is up to date\n",
            "2026.10.4"
        ));
        assert!(installed(
            "this is clackatoo 2026.10.5 already\n",
            "2026.10.4"
        ));
        // Nothing newer than this window's own build.
        assert!(!installed(
            "clackatoo 2026.10.4 is up to date\n",
            "2026.10.4"
        ));
        assert!(!installed("", "2026.10.4"));
    }

    #[test]
    fn the_older_side_is_offered_an_update() {
        assert_eq!(skew_notice("2026.10.2", "2026.10.2", Some("desk")), None);
        assert_eq!(
            skew_notice("2026.10.2", "2026.10.1", Some("desk")).unwrap(),
            "the daemon on desk is clackatoo 2026.10.1, older than this window's 2026.10.2: \
             :update desk"
        );
        assert_eq!(
            skew_notice("2026.10.2", "2026.11.0", Some("desk")).unwrap(),
            "this window is clackatoo 2026.10.2, older than the daemon on desk's 2026.11.0: \
             :update, then open the window again"
        );
        // Compared as numbers: 10 is newer than 9.
        assert!(
            skew_notice("2026.10.9", "2026.10.10", None)
                .unwrap()
                .starts_with("this window")
        );
        assert!(
            skew_notice("2026.10.10", "2026.10.9", None)
                .unwrap()
                .contains("clackatoo upgrade")
        );
        // A version that isn't one is only reported.
        assert_eq!(
            skew_notice("2026.10.2", "custom", Some("desk")).unwrap(),
            "the daemon on desk is clackatoo custom, and this window 2026.10.2"
        );
    }

    #[test]
    fn the_bar_says_how_an_update_went() {
        assert_eq!(
            outcome(true, "clackatoo 2026.10.2 is up to date\n", ""),
            Ok("clackatoo 2026.10.2 is up to date".into())
        );
        let installed = "downloading clackatoo 2026.10.3…\ninstalled clackatoo 2026.10.3 at /x, \
                         keeping 2026.10.2 as /x.previous\nno daemon is running\n";
        assert_eq!(
            outcome(true, installed, ""),
            Ok("clackatoo 2026.10.3 is installed: open the window again to use it".into())
        );
        assert_eq!(
            outcome(
                false,
                "",
                "clackatoo: release 2026.10.3 isn't signed with the release key\n"
            ),
            Err("release 2026.10.3 isn't signed with the release key".into())
        );
        assert_eq!(
            outcome(false, "", ""),
            Err("clackatoo update failed".into())
        );
    }

    #[test]
    fn a_new_release_is_mentioned_with_how_to_get_it() {
        assert_eq!(
            release_notice("2026.10.3"),
            "clackatoo 2026.10.3 is out: :update"
        );
    }
}
