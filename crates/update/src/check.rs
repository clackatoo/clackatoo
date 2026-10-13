//! Looking for a new release now and then, for the window to mention.
//!
//! At most once a day: when it last looked, and what it found, are kept
//! in a small file in the state directory, so every window opened that day
//! says the same without asking GitHub again. It never installs anything.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Channel, Error, Source, Version};

/// How long a look lasts before the next window looks again.
pub const EVERY: Duration = Duration::from_hours(24);

/// The file that says when the last look was, and what it found:
/// `$XDG_STATE_HOME/clackatoo/update-check`, or under `~/.local/state`.
#[must_use]
pub fn state_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))?;
    Some(state.join("clackatoo/update-check"))
}

/// The newest release, if it's newer than this build. Looks it up at
/// `source` unless the file at `state` says it was looked up less than a
/// day before `now`, and then notes the look there.
///
/// A failed look is noted too, so a machine offline doesn't try at every
/// window, and says what the last good one found.
///
/// # Errors
///
/// If the release can't be fetched. A state file that can't be read or
/// written only means looking again.
pub fn newer_release(
    source: &Source,
    state: &Path,
    now: SystemTime,
) -> Result<Option<Version>, Error> {
    let noted = Noted::read(state);
    let latest = match noted {
        Some(noted) if noted.fresh(now) => noted.latest,
        _ => {
            let looked = source
                .latest(Channel::Stable)
                .map(|release| release.version);
            let latest = looked
                .as_ref()
                .ok()
                .copied()
                .or(noted.and_then(|noted| noted.latest));
            Noted { at: now, latest }.write(state);
            looked?;
            latest
        }
    };
    Ok(latest.filter(|&latest| latest > Version::current()))
}

/// What the state file says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Noted {
    /// When the last look was.
    at: SystemTime,
    /// The newest release found, if any has been.
    latest: Option<Version>,
}

impl Noted {
    /// Whether the look was less than a day before `now`, and not after it,
    /// as a clock put back would make it.
    fn fresh(self, now: SystemTime) -> bool {
        now.duration_since(self.at).is_ok_and(|age| age < EVERY)
    }

    /// Reads `checked SECONDS` and `latest VERSION` lines.
    fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut at = None;
        let mut latest = None;
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("checked", seconds)) => {
                    at = Some(UNIX_EPOCH + Duration::from_secs(seconds.parse().ok()?));
                }
                Some(("latest", version)) => latest = version.parse().ok(),
                _ => {}
            }
        }
        Some(Self { at: at?, latest })
    }

    fn write(self, path: &Path) {
        let seconds = self
            .at
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let text = match self.latest {
            Some(latest) => format!("checked {seconds}\nlatest {latest}\n"),
            None => format!("checked {seconds}\n"),
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Written beside it and renamed, so two windows never read half.
        let staged = path.with_extension(format!("{}", std::process::id()));
        let written = std::fs::write(&staged, text).and_then(|()| std::fs::rename(&staged, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
    }
}
