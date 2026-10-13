//! Notes which commit this build is from, and when it was built, for
//! `clackatoo --version`, so a build says where it came from without git
//! being there when it runs.
//!
//! The commit is git's, or `$CLACKATOO_GIT_HASH`'s, or `unknown` when
//! neither can say, as from a tarball of the source. The date is
//! `$SOURCE_DATE_EPOCH`'s, for builds that must come out the same each
//! time, or else today's, in UTC. Both are worked out again only when the
//! commit changes, so they don't cost a rebuild each time.
//!
//! Also builds the web app into the program: see `build/web_app.rs`.

#[path = "build/web_app.rs"]
mod web_app;

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo::rerun-if-env-changed=CLACKATOO_GIT_HASH");
    println!("cargo::rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let hash = std::env::var("CLACKATOO_GIT_HASH")
        .ok()
        .filter(|hash| !hash.is_empty())
        .or_else(git_hash)
        .unwrap_or_else(|| "unknown".to_owned());
    let seconds = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|seconds| seconds.trim().parse().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs())
        });
    println!("cargo::rustc-env=CLACKATOO_GIT_HASH={hash}");
    println!("cargo::rustc-env=CLACKATOO_BUILD_DATE={}", date(seconds));
    web_app::build();
}

/// The commit checked out, shortened, if git can say. Asks to be run again
/// when it changes: when HEAD moves to another branch, or the branch to
/// another commit.
fn git_hash() -> Option<String> {
    let git = |args: &[&str]| {
        let output = Command::new("git").args(args).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let hash = git(&["rev-parse", "--short=10", "HEAD"])?;
    // In a worktree, `.git` is a file, so git says where HEAD really is.
    for path in [
        git(&["rev-parse", "--git-path", "HEAD"]),
        git(&["rev-parse", "--git-path", "packed-refs"]),
        git(&["symbolic-ref", "-q", "HEAD"])
            .and_then(|branch| git(&["rev-parse", "--git-path", &branch])),
    ]
    .into_iter()
    .flatten()
    {
        let path = PathBuf::from(path);
        if path.exists() {
            println!("cargo::rerun-if-changed={}", path.display());
        }
    }
    Some(hash)
}

/// The day `seconds` after 1970 began, in UTC, as `2026-10-13`.
fn date(seconds: u64) -> String {
    // Howard Hinnant's days_from_civil, backwards.
    let days = i64::try_from(seconds / 86_400).unwrap_or(0) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}
