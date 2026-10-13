use std::os::unix::fs::PermissionsExt;

use super::*;

fn op(op: FileOp) -> Result<(), FileOpError> {
    run(op, None, &[]).1
}

fn path(dir: &Path, name: &str) -> String {
    dir.join(name).display().to_string()
}

/// A temporary directory, by its real path.
fn temp() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap();
    (dir, real)
}

/// What's at `path` as it is now, to write over it with.
fn stamp(path: &Path) -> Option<FileStamp> {
    fs::symlink_metadata(path)
        .ok()
        .map(|meta| file_stamp(&meta))
}

/// Moving `from` to `to`, in `dir`, writing over what's there now if
/// `overwrite`.
fn moving(dir: &Path, from: &str, to: &str, overwrite: bool) -> FileOp {
    FileOp::Move {
        from: path(dir, from),
        to: path(dir, to),
        overwrite: stamp(&dir.join(to)).filter(|_| overwrite),
    }
}

fn copying(dir: &Path, from: &str, to: &str, overwrite: bool) -> FileOp {
    FileOp::Copy {
        from: path(dir, from),
        to: path(dir, to),
        overwrite: stamp(&dir.join(to)).filter(|_| overwrite),
    }
}

/// The names in `dir`, in order.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = (fs::read_dir(dir).unwrap().flatten())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Whether the tests run as someone a file's permissions don't stop, as
/// root isn't stopped.
fn unstoppable(dir: &Path) -> bool {
    let locked = dir.join("probe");
    fs::write(&locked, "").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let read = File::open(&locked).is_ok();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&locked).unwrap();
    read
}

#[test]
fn files_are_made_moved_copied_and_deleted_and_nothing_is_written_over() {
    let (_held, dir) = temp();
    let dir = &dir;
    let create = |name: &str, is_dir| {
        op(FileOp::Create {
            path: path(dir, name),
            dir: is_dir,
        })
    };
    create("a/b/c.txt", false).unwrap();
    create("d", true).unwrap();
    assert!(dir.join("a/b/c.txt").is_file() && dir.join("d").is_dir());
    assert!(matches!(
        create("d", false),
        Err(FileOpError::Exists { .. })
    ));
    fs::write(dir.join("a/b/c.txt"), "c").unwrap();

    op(moving(dir, "a/b/c.txt", "d/e.txt", false)).unwrap();
    assert_eq!(fs::read_to_string(dir.join("d/e.txt")).unwrap(), "c");
    fs::write(dir.join("f.txt"), "ff").unwrap();
    assert_eq!(
        op(moving(dir, "f.txt", "d/e.txt", false)),
        Err(FileOpError::Exists {
            path: path(dir, "d/e.txt"),
            what: "a file of 1 byte".into(),
            stamp: stamp(&dir.join("d/e.txt")).unwrap(),
        })
    );
    assert_eq!(fs::read_to_string(dir.join("d/e.txt")).unwrap(), "c");
    op(moving(dir, "f.txt", "d/e.txt", true)).unwrap();
    assert_eq!(fs::read_to_string(dir.join("d/e.txt")).unwrap(), "ff");
    assert!(
        op(moving(dir, "d", "d/inside", false)).is_err(),
        "into itself"
    );

    std::os::unix::fs::symlink("e.txt", dir.join("d/link")).unwrap();
    op(copying(dir, "d", "g", false)).unwrap();
    assert_eq!(fs::read_to_string(dir.join("g/e.txt")).unwrap(), "ff");
    assert_eq!(
        fs::read_link(dir.join("g/link")).unwrap(),
        Path::new("e.txt")
    );
    assert!(
        op(copying(dir, "d", "d/again", false)).is_err(),
        "into itself"
    );

    let delete = |name: &str| {
        op(FileOp::Delete {
            path: path(dir, name),
            trash: false,
        })
    };
    delete("g").unwrap();
    assert!(!dir.join("g").exists());
    assert!(delete("g").is_err(), "nothing there");
    assert!(
        op(FileOp::Create {
            path: "relative".into(),
            dir: false,
        })
        .is_err()
    );
    // Nothing is left behind by copies on their way.
    assert_eq!(names(dir), ["a", "d"]);
}

/// Pasting a directory into the one it's in names the one it's in: it
/// mustn't be written over, taking what's being pasted with it.
#[test]
fn what_holds_what_is_moved_or_copied_is_never_written_over() {
    let (_held, dir) = temp();
    let dir = &dir;
    fs::create_dir_all(dir.join("app/app/src")).unwrap();
    fs::write(dir.join("app/app/src/main.rs"), "fn main() {}").unwrap();
    fs::write(dir.join("app/Cargo.toml"), "[package]").unwrap();
    for overwrite in [false, true] {
        for asked in [
            moving(dir, "app/app", "app", overwrite),
            copying(dir, "app/app", "app", overwrite),
        ] {
            let refused = op(asked).unwrap_err().to_string();
            assert!(
                refused.contains("can't be written over by what's in it"),
                "{refused}"
            );
        }
    }
    assert!(dir.join("app/app/src/main.rs").is_file());
    assert!(dir.join("app/Cargo.toml").is_file());
}

#[test]
fn a_folder_is_never_written_over_and_says_what_it_holds() {
    let (_held, dir) = temp();
    let dir = &dir;
    fs::create_dir_all(dir.join("old/inner")).unwrap();
    fs::write(dir.join("old/inner/kept.txt"), "kept").unwrap();
    fs::write(dir.join("new"), "new").unwrap();
    fs::create_dir(dir.join("newdir")).unwrap();
    for overwrite in [false, true] {
        for asked in [
            moving(dir, "new", "old", overwrite),
            copying(dir, "new", "old", overwrite),
            moving(dir, "newdir", "old", overwrite),
        ] {
            let refused = op(asked).unwrap_err().to_string();
            assert!(
                refused.contains("is a folder with 2 items: a folder is never written over"),
                "{refused}"
            );
        }
    }
    assert_eq!(
        fs::read_to_string(dir.join("old/inner/kept.txt")).unwrap(),
        "kept"
    );
}

#[test]
fn a_file_open_with_changes_not_written_is_never_written_over() {
    let (_held, dir) = temp();
    let dir = &dir;
    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join("b.txt"), "b").unwrap();
    let open = |modified| Open {
        path: dir.join("b.txt"),
        modified,
        disk: DiskStamp::now(&dir.join("b.txt")).unwrap(),
    };
    let (_, refused) = run(moving(dir, "a.txt", "b.txt", true), None, &[open(true)]);
    let refused = refused.unwrap_err().to_string();
    assert!(
        refused.contains("is open with changes not written"),
        "{refused}"
    );
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b");
    // With none, it is, and the buffer is told its file was.
    let (changed, done) = run(copying(dir, "a.txt", "b.txt", true), None, &[open(false)]);
    done.unwrap();
    assert_eq!(changed.replaced, Some(dir.join("b.txt")));
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "a");
}

/// A tree to move: `a.txt`, `sub/b.txt` and a link.
fn tree(dir: &Path) -> PathBuf {
    let from = dir.join("from");
    fs::create_dir_all(from.join("sub")).unwrap();
    fs::write(from.join("a.txt"), "a").unwrap();
    fs::write(from.join("sub/b.txt"), "b").unwrap();
    std::os::unix::fs::symlink("a.txt", from.join("link")).unwrap();
    from
}

#[test]
fn a_move_between_file_systems_copies_deletes_what_it_copied_and_buffers_save_after() {
    let (_held, dir) = temp();
    let from = tree(&dir);
    let to = dir.join("to");
    let a = from.join("a.txt");
    let open = [Open {
        path: a.clone(),
        modified: false,
        disk: DiskStamp::now(&a).unwrap(),
    }];
    let mut changed = Changed::default();
    move_across(&from, &to, false, &open, &mut changed).unwrap();
    assert!(!from.exists());
    assert_eq!(names(&to), ["a.txt", "link", "sub"]);
    assert_eq!(fs::read_to_string(to.join("sub/b.txt")).unwrap(), "b");
    let [follow] = &changed.follow[..] else {
        panic!("{changed:?}");
    };
    assert_eq!((&follow.from, &follow.to), (&a, &to.join("a.txt")));
    // Its next save isn't refused: it's told the file it is now.
    let now = DiskStamp::now(&to.join("a.txt")).unwrap();
    assert_eq!(follow.disk, Restamp::To(now));
    assert_eq!(names(&dir), ["to"], "nothing left on its way");
}

#[test]
fn a_move_between_file_systems_that_fails_leaves_nothing_behind_and_keeps_the_source() {
    let (_held, dir) = temp();
    // Pipes, sockets and devices are refused before anything's copied.
    let from = tree(&dir);
    let fifo = from.join("sub/pipe");
    rustix::fs::mknodat(CWD, &fifo, rustix::fs::FileType::Fifo, 0o600.into(), 0).unwrap();
    let mut changed = Changed::default();
    let refused = move_across(&from, &dir.join("to"), false, &[], &mut changed);
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("is a pipe, socket or device")
    );
    let copying = op(copying(&dir, "from", "copy", false))
        .unwrap_err()
        .to_string();
    assert!(copying.contains("is a pipe, socket or device"), "{copying}");
    assert_eq!(names(&dir), ["from"]);
    fs::remove_file(&fifo).unwrap();
    if unstoppable(&dir) {
        return;
    }
    // One that fails partway takes away what it made, and nothing else.
    let locked = from.join("sub/b.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let failed = move_across(&from, &dir.join("to"), false, &[], &mut changed);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(failed.is_err());
    assert_eq!(names(&dir), ["from"]);
    assert_eq!(names(&from), ["a.txt", "link", "sub"]);
    assert!(changed.follow.is_empty());
}

#[test]
fn a_move_between_file_systems_that_cant_delete_everything_says_so_and_buffers_follow_what_moved() {
    let (_held, dir) = temp();
    if unstoppable(&dir) {
        return;
    }
    let from = tree(&dir);
    let open: Vec<Open> = ["a.txt", "sub/b.txt"]
        .iter()
        .map(|name| Open {
            path: from.join(name),
            modified: false,
            disk: DiskStamp::now(&from.join(name)).unwrap(),
        })
        .collect();
    // What's in `sub` can't be deleted.
    let sub = from.join("sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o555)).unwrap();
    let mut changed = Changed::default();
    let to = dir.join("to");
    let outcome = move_across(&from, &to, false, &open, &mut changed);
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    // The copy is as read-only as what it was copied from.
    let copied_sub = to.join("sub");
    fs::set_permissions(&copied_sub, fs::Permissions::from_mode(0o755)).unwrap();
    let said = outcome.unwrap_err().to_string();
    assert!(said.contains("is in both places"), "{said}");
    assert_eq!(fs::read_to_string(to.join("sub/b.txt")).unwrap(), "b");
    assert_eq!(fs::read_to_string(sub.join("b.txt")).unwrap(), "b", "kept");
    let followed: Vec<&Path> = changed
        .follow
        .iter()
        .map(|follow| follow.from.as_path())
        .collect();
    assert_eq!(
        followed,
        [from.join("a.txt")],
        "only what's gone from where it was"
    );
}

#[test]
fn a_file_changed_while_it_was_copied_is_kept_where_it_was() {
    let (_held, dir) = temp();
    let from = tree(&dir);
    let to = dir.join("to");
    let copying = Copying::now();
    let copied = copy_in_place(&from, &to, false).unwrap();
    // Changed after it was copied, before it's deleted.
    fs::write(from.join("a.txt"), "changed since").unwrap();
    let kept = |copied: &[Copied], copying: &Copying| -> Vec<PathBuf> {
        (copied.iter())
            .filter(|item| match &item.kind {
                Kind::File { source, .. } => {
                    !copying.unchanged(&item.path, source, &item.copy_in(&to))
                }
                _ => false,
            })
            .map(|item| item.path.clone())
            .collect()
    };
    assert_eq!(kept(&copied, &copying), [from.join("a.txt")]);
    // Its copy is as it was made, which a buffer moved to it is told.
    let copy = copied.iter().find_map(|item| match &item.kind {
        Kind::File { copy, .. } if item.rel == Path::new("sub/b.txt") => Some(*copy),
        _ => None,
    });
    assert_eq!(copy, DiskStamp::now(&to.join("sub/b.txt")).unwrap());
}

/// A file changed in the same moment it was copied, keeping its length
/// and its time, is told by its bytes, as git tells a racy file.
#[test]
fn a_file_changed_too_soon_for_its_time_to_tell_is_told_by_its_bytes() {
    let (_held, dir) = temp();
    let from = tree(&dir);
    let to = dir.join("to");
    let copying = Copying::now();
    let copied = copy_in_place(&from, &to, false).unwrap();
    let source = copied
        .iter()
        .find_map(|item| match &item.kind {
            Kind::File { source, .. } if item.rel == Path::new("a.txt") => Some(*source),
            _ => None,
        })
        .unwrap();
    let a = from.join("a.txt");
    assert!(copying.unchanged(&a, &source, &to.join("a.txt")));
    // The same length, written back to the same time.
    let written = fs::metadata(&a).unwrap();
    fs::write(&a, "z").unwrap();
    let times = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: written.atime(),
            tv_nsec: written.atime_nsec(),
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: written.mtime(),
            tv_nsec: written.mtime_nsec(),
        },
    };
    rustix::fs::utimensat(CWD, &a, &times, rustix::fs::AtFlags::empty()).unwrap();
    assert_eq!(
        DiskStamp::now(&a).unwrap(),
        Some(source),
        "its time can't tell"
    );
    assert!(!copying.unchanged(&a, &source, &to.join("a.txt")));
}

#[test]
fn a_copy_never_takes_away_what_had_the_name_it_tried() {
    let (_held, dir) = temp();
    let from = tree(&dir);
    let taken = dir.join(".taken");
    fs::create_dir(&taken).unwrap();
    fs::write(taken.join("someone's.txt"), "theirs").unwrap();
    let free = dir.join(".free");
    let named = |attempt| {
        if attempt == 0 {
            taken.clone()
        } else {
            free.clone()
        }
    };
    copy_in_place_named(&from, &dir.join("to"), false, named).unwrap();
    assert_eq!(
        fs::read_to_string(taken.join("someone's.txt")).unwrap(),
        "theirs"
    );
    assert_eq!(names(&dir.join("to")), ["a.txt", "link", "sub"]);
}

#[test]
fn a_file_is_written_over_only_as_it_was_when_it_was_described() {
    let (_held, dir) = temp();
    let dir = &dir;
    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join("b.txt"), "b").unwrap();
    let asked = moving(dir, "a.txt", "b.txt", true);
    // Changed since it was described: described again, and kept.
    fs::write(dir.join("b.txt"), "changed since").unwrap();
    let Err(FileOpError::Exists { what, stamp, .. }) = op(asked) else {
        panic!("not described again");
    };
    assert_eq!(what, "a file of 13 bytes");
    assert_eq!(
        fs::read_to_string(dir.join("b.txt")).unwrap(),
        "changed since"
    );
    let again = FileOp::Move {
        from: path(dir, "a.txt"),
        to: path(dir, "b.txt"),
        overwrite: Some(stamp),
    };
    op(again).unwrap();
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "a");
}

#[test]
fn a_file_open_with_changes_under_another_name_is_never_written_over() {
    let (_held, dir) = temp();
    let dir = &dir;
    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join("b.txt"), "b").unwrap();
    // The same file, by a second name.
    fs::hard_link(dir.join("b.txt"), dir.join("also-b.txt")).unwrap();
    let open = Open {
        path: dir.join("also-b.txt"),
        modified: true,
        disk: DiskStamp::now(&dir.join("also-b.txt")).unwrap(),
    };
    let (_, refused) = run(moving(dir, "a.txt", "b.txt", true), None, &[open]);
    let refused = refused.unwrap_err().to_string();
    assert!(
        refused.contains("is open with changes not written"),
        "{refused}"
    );
    assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b");
}

#[test]
fn a_mount_point_is_never_deleted() {
    // /proc is another file system, mounted on /. Asked of, it's refused
    // before anything is done to it.
    let refused = not_a_mount(Path::new("/proc")).unwrap_err().to_string();
    assert!(
        refused.contains("another file system is mounted at /proc"),
        "{refused}"
    );
    let (_held, dir) = temp();
    assert!(not_a_mount(&dir).is_ok());
    // Nor trashed, nor deleted, with one inside it.
    let mounts = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    if mounts.contains(" /dev/pts ") {
        let refused = no_mounts_in(Path::new("/dev")).unwrap_err().to_string();
        assert!(
            refused.contains("another file system is mounted at /dev/"),
            "{refused}"
        );
    }
}

#[test]
fn a_move_to_another_file_system_really_goes_there() {
    // Only where /tmp is another file system than the tests' directory.
    let (_held, dir) = temp();
    let Ok(away) = tempfile::tempdir_in("/tmp") else {
        return;
    };
    let device = |path: &Path| fs::metadata(path).unwrap().dev();
    if device(&dir) == device(away.path()) {
        return;
    }
    let from = tree(&dir);
    let to = away.path().join("moved");
    let asked = FileOp::Move {
        from: from.display().to_string(),
        to: to.display().to_string(),
        overwrite: None,
    };
    op(asked).unwrap();
    assert!(!from.exists());
    assert_eq!(fs::read_to_string(to.join("sub/b.txt")).unwrap(), "b");
}

#[test]
fn a_file_just_written_moves_between_file_systems_whole() {
    let (_held, dir) = temp();
    let from = dir.join("new.txt");
    fs::write(&from, "just now").unwrap();
    let mut changed = Changed::default();
    move_across(&from, &dir.join("moved.txt"), false, &[], &mut changed).unwrap();
    assert!(!from.exists(), "compared by its bytes, then deleted");
    assert_eq!(
        fs::read_to_string(dir.join("moved.txt")).unwrap(),
        "just now"
    );
}
