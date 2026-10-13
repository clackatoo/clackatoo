//! Updating from fake releases: a directory laid out as GitHub's API is,
//! signed with a key made for the test, and served from files or over
//! HTTP.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use clackatoo_update::fake::{Fake, Key, Releases, gzip, program, sha256};
use clackatoo_update::verify::{self, SIGNATURE, SUMS};
use clackatoo_update::{Channel, Error, PublicKey, Source, Version, check, install};

/// An installed clackatoo, in a directory of its own.
fn installed(version: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clackatoo");
    std::fs::write(&path, program(version)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, path)
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = (std::fs::read_dir(dir).unwrap())
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn update(source: &Source, key: &PublicKey, target: &Path) -> Result<Version, Error> {
    let release = source.latest(Channel::Stable)?;
    let program = verify::download(&release, key)?;
    install::install(target, &program, release.version)?;
    Ok(release.version)
}

#[test]
fn a_signed_release_is_installed_and_the_old_build_kept() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("2026.10.0", &key), false);
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let (dir, target) = installed("2026.9.4");

    let release = releases.source().latest(Channel::Stable).unwrap();
    assert_eq!(release.version, "9999.1.0".parse().unwrap());
    assert!(release.notes.contains("- fake"));
    assert_eq!(
        update(&releases.source(), &key.public(), &target).unwrap(),
        release.version
    );

    assert_eq!(std::fs::read(&target).unwrap(), program("9999.1.0"));
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(
        std::fs::read(install::previous(&target)).unwrap(),
        program("2026.9.4")
    );
    // Nothing left over beside them.
    assert_eq!(files_in(dir.path()), ["clackatoo", "clackatoo.previous"]);

    // An older release can be asked for by its version.
    let older = releases
        .source()
        .version("2026.10.0".parse().unwrap())
        .unwrap();
    let program_bytes = verify::download(&older, &key.public()).unwrap();
    install::install(&target, &program_bytes, older.version).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), program("2026.10.0"));
    assert_eq!(
        std::fs::read(install::previous(&target)).unwrap(),
        program("9999.1.0")
    );
}

#[test]
fn rolling_back_swaps_the_builds_and_again_swaps_them_back() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let (_dir, target) = installed("2026.10.0");
    assert!(matches!(
        install::rollback(&target),
        Err(Error::NoPrevious(_))
    ));

    update(&releases.source(), &key.public(), &target).unwrap();
    install::rollback(&target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), program("2026.10.0"));
    assert_eq!(
        std::fs::read(install::previous(&target)).unwrap(),
        program("9999.1.0")
    );
    install::rollback(&target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), program("9999.1.0"));
}

#[test]
fn a_release_signed_with_another_key_is_refused() {
    let key = Key::new();
    let stranger = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &stranger), true);
    let (dir, target) = installed("2026.10.0");

    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(matches!(err, Error::BadSignature(_)), "{err}");
    assert_eq!(std::fs::read(&target).unwrap(), program("2026.10.0"));
    assert_eq!(files_in(dir.path()), ["clackatoo"]);
}

#[test]
fn an_old_release_served_under_a_new_tag_is_refused_before_it_runs() {
    let key = Key::new();
    let releases = Releases::new();
    let ran = tempfile::tempdir().unwrap();
    let marker = ran.path().join("ran");
    // Release 2026.10.0, signed in its day, served as 9999.1.0: the files
    // and signature are good, but the signature names 2026.10.0. A
    // program that notes if it's ever run, to show it isn't.
    let mut replayed = Fake::new("9999.1.0", &key);
    replayed.signed_as = Some("2026.10.0");
    replayed.program = format!(
        "#!/bin/sh\ntouch {}\necho \"clackatoo 9999.1.0 (fake 2026-10-13)\"\n",
        marker.display()
    )
    .into_bytes();
    releases.publish(&replayed, true);
    let (dir, target) = installed("2026.10.0");

    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(
        matches!(&err, Error::SignedForAnother { comment, .. } if comment == "clackatoo v2026.10.0"),
        "{err}"
    );
    assert!(!marker.exists(), "the download was run");
    assert_eq!(std::fs::read(&target).unwrap(), program("2026.10.0"));
    assert_eq!(files_in(dir.path()), ["clackatoo"]);
    assert_eq!(
        verify::trusted_comment("9999.1.0".parse().unwrap()),
        "clackatoo v9999.1.0"
    );
}

#[test]
fn a_signature_that_was_tampered_with_is_refused() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let sums = releases.path().join("assets/v9999.1.0").join(SUMS);
    // A line added to the checksums after signing.
    let text = std::fs::read_to_string(&sums).unwrap();
    std::fs::write(
        &sums,
        format!("{text}{}  clackatoo-evil-linux.gz\n", sha256(b"evil")),
    )
    .unwrap();
    let (_dir, target) = installed("2026.10.0");

    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(matches!(err, Error::BadSignature(_)), "{err}");
    // So is a signature that isn't one.
    let signature = releases.path().join("assets/v9999.1.0").join(SIGNATURE);
    std::fs::write(&signature, "untrusted comment: no\nnonsense\n").unwrap();
    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(matches!(err, Error::BadSignature(_)), "{err}");
    assert_eq!(std::fs::read(&target).unwrap(), program("2026.10.0"));
}

#[test]
fn a_build_that_isnt_what_the_signed_checksums_say_is_refused() {
    let key = Key::new();
    let releases = Releases::new();
    let mut fake = Fake::new("9999.1.0", &key);
    fake.wrong_sum = true;
    releases.publish(&fake, true);
    let (dir, target) = installed("2026.10.0");

    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(matches!(err, Error::BadChecksum(_)), "{err}");
    assert_eq!(std::fs::read(&target).unwrap(), program("2026.10.0"));
    assert_eq!(files_in(dir.path()), ["clackatoo"]);

    // And a build swapped after the release was signed.
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let build = releases
        .path()
        .join("assets/v9999.1.0")
        .join(verify::build_name());
    std::fs::write(&build, gzip(&program("9999.1.0 evil"))).unwrap();
    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(matches!(err, Error::BadChecksum(_)), "{err}");
}

#[test]
fn a_build_that_wont_run_or_says_another_version_is_not_installed() {
    let key = Key::new();
    let releases = Releases::new();
    let mut fake = Fake::new("9999.1.0", &key);
    fake.program = program("2026.10.7");
    releases.publish(&fake, true);
    let (dir, target) = installed("2026.10.0");
    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(matches!(err, Error::OtherVersion { .. }), "{err}");
    assert_eq!(files_in(dir.path()), ["clackatoo"]);

    // Not a program at all, as one built for another machine looks.
    fake.program = b"\x7fELF not really".to_vec();
    releases.publish(&fake, true);
    let err = update(&releases.source(), &key.public(), &target).unwrap_err();
    assert!(matches!(err, Error::WontRun(_)), "{err}");
    assert_eq!(std::fs::read(&target).unwrap(), program("2026.10.0"));
    assert_eq!(files_in(dir.path()), ["clackatoo"]);
}

#[test]
fn a_release_without_a_build_for_this_machine_or_a_version_that_doesnt_exist() {
    let key = Key::new();
    let releases = Releases::new();
    let mut fake = Fake::new("9999.1.0", &key);
    fake.no_build = true;
    releases.publish(&fake, true);
    let release = releases.source().latest(Channel::Stable).unwrap();
    let err = verify::download(&release, &key.public()).unwrap_err();
    assert!(matches!(err, Error::MissingAsset(..)), "{err}");
    let err = releases
        .source()
        .version("2026.9.0".parse().unwrap())
        .unwrap_err();
    assert!(matches!(err, Error::NoSuchRelease(_)), "{err}");
    let err = releases.source().latest(Channel::Nightly).unwrap_err();
    assert!(matches!(err, Error::NoNightlies), "{err}");
    let err = Source::new("ftp://example.com")
        .latest(Channel::Stable)
        .unwrap_err();
    assert!(matches!(err, Error::BadUrl(_)), "{err}");
}

#[test]
fn releases_are_fetched_over_http() {
    let key = Key::new();
    let mut releases = Releases::new();
    releases.serve_over_http();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let (_dir, target) = installed("2026.10.0");

    update(&releases.source(), &key.public(), &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), program("9999.1.0"));
    let err = releases
        .source()
        .version("2026.9.0".parse().unwrap())
        .unwrap_err();
    assert!(matches!(err, Error::NoSuchRelease(_)), "{err}");
}

#[test]
fn a_directory_is_a_source_too() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let source = Source::new(releases.path().to_str().unwrap());
    assert_eq!(source, releases.source());
    let release = source.latest(Channel::Stable).unwrap();
    assert_eq!(release.version, "9999.1.0".parse().unwrap());
}

#[test]
fn the_window_looks_for_a_release_at_most_once_a_day() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new("9999.1.0", &key), true);
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().join("clackatoo/update-check");
    let now = SystemTime::now();
    let newer = |source: &Source, at| check::newer_release(source, &state, at);

    assert_eq!(
        newer(&releases.source(), now).unwrap(),
        Some("9999.1.0".parse().unwrap())
    );
    assert!(
        std::fs::read_to_string(&state)
            .unwrap()
            .contains("latest 9999.1.0")
    );

    // Within the day, what was found is said again, without asking.
    let gone = Source::new(state_dir.path().join("nowhere").to_str().unwrap());
    let later = now + Duration::from_hours(23);
    assert_eq!(
        newer(&gone, later).unwrap(),
        Some("9999.1.0".parse().unwrap())
    );

    // A day on, it asks again. Failing is noted, so the next window that
    // day doesn't ask either, and what was found before still stands.
    let tomorrow = now + Duration::from_hours(25);
    assert!(newer(&gone, tomorrow).is_err());
    let soon_after = tomorrow + Duration::from_mins(1);
    assert_eq!(
        newer(&gone, soon_after).unwrap(),
        Some("9999.1.0".parse().unwrap())
    );

    // A newer release, found the next day.
    releases.publish(&Fake::new("9999.2.0", &key), true);
    let next_day = tomorrow + Duration::from_hours(25);
    assert_eq!(
        newer(&releases.source(), next_day).unwrap(),
        Some("9999.2.0".parse().unwrap())
    );

    // A clock put back asks again rather than trusting a look "from the
    // future".
    releases.publish(&Fake::new("9999.3.0", &key), true);
    assert_eq!(
        newer(&releases.source(), now).unwrap(),
        Some("9999.3.0".parse().unwrap())
    );
}

#[test]
fn a_release_no_newer_than_this_build_isnt_mentioned() {
    let key = Key::new();
    let releases = Releases::new();
    releases.publish(&Fake::new(clackatoo_update::CURRENT, &key), true);
    let state_dir = tempfile::tempdir().unwrap();
    let state = state_dir.path().join("update-check");
    let found = check::newer_release(&releases.source(), &state, SystemTime::now()).unwrap();
    assert_eq!(found, None);
}
