//! Capturing what was selected, on a worker: reading files, listing
//! folders, reading snapshots kept, and keeping what a message takes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{ContextMode, ContextRef, ContextSelection, ContextSnapshot};

use super::{Excerpt, Output, Prepared, Result, snapshot, store};

/// The most entries of a folder that are listed, and how deep.
const MOST_LISTED: usize = 1000;
const LISTED_DEPTH: usize = 3;

/// What a worker is to capture, with all it needs to.
pub(super) struct Capture {
    pub prepared: Vec<Prepared>,
    /// Where language packs are installed, for checking a definition.
    pub languages: Option<PathBuf>,
    pub keep: Keep,
}

/// What becomes of the snapshots captured.
pub(super) enum Keep {
    /// They're only shown: a preview.
    Nothing,
    /// They're kept for a draft, which names them by identifier, so one
    /// that can't be kept is no use and the capture fails.
    Draft {
        store: PathBuf,
        in_use: BTreeSet<String>,
    },
    /// They go with the message `text` from the project `root`, and are
    /// kept with it if the store can: the message goes either way.
    Message {
        store: PathBuf,
        root: PathBuf,
        text: String,
        in_use: BTreeSet<String>,
    },
}

/// Captures each selection, in order, and keeps them as asked. A source
/// captured twice is taken once.
///
/// # Errors
/// If a selection the user chose can't be captured, with why, or they
/// come to more than a message may take. A file the message only mentions
/// that can't be captured is left out instead.
pub(super) fn run(job: Capture, cancelled: &dyn Fn() -> bool) -> Result<Output> {
    let mut snapshots: Vec<ContextSnapshot> = Vec::new();
    let mut total = 0;
    for item in job.prepared {
        if cancelled() {
            return Err("Context capture was cancelled.".into());
        }
        let made = match item {
            Prepared::Optional(item) => match capture(*item, job.languages.as_deref(), cancelled) {
                Ok(made) => made,
                Err(_) => continue,
            },
            item => capture(item, job.languages.as_deref(), cancelled)?,
        };
        if snapshots.iter().any(|snapshot| snapshot.id == made.id) {
            continue;
        }
        total += made.text.len();
        if total > ContextSnapshot::MAX_TOTAL {
            return Err("Context exceeds 256 KiB. Narrow or remove an attachment.".into());
        }
        snapshots.push(made);
    }
    let unkept = match job.keep {
        Keep::Nothing => None,
        Keep::Draft { store, in_use } => {
            store::retain(&store, &snapshots, &in_use)
                .map_err(|err| format!("This can't be attached. {err}"))?;
            None
        }
        Keep::Message {
            store,
            root,
            text,
            in_use,
        } => store::remember(&store, &root, &text, &snapshots, &in_use)
            .err()
            .map(|err| err.to_string()),
    };
    Ok(Output::Captured { snapshots, unkept })
}

/// Captures one selection.
fn capture(
    prepared: Prepared,
    languages: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
) -> Result<ContextSnapshot> {
    match prepared {
        Prepared::Stored(wanted, path) => stored(wanted, &path),
        Prepared::Optional(item) => capture(*item, languages, cancelled),
        Prepared::Captured(selection, excerpt) => snapshot(&selection, excerpt),
        Prepared::File(selection, job) => file(&selection, &job, languages, cancelled),
        Prepared::Project(selection, path, root) => folder(&selection, &path, &root),
    }
}

/// The snapshot kept as `wanted` names it: as it was captured, or, where
/// the user now asks for only a reference to it, made again as one.
fn stored(mut wanted: ContextSnapshot, path: &Path) -> Result<ContextSnapshot> {
    let kept = store::read(path, &wanted.id).map_err(|err| err.to_string())?;
    if !matches!(wanted.source, ContextRef::Retained { .. }) {
        // A turn's own snapshot, whose text alone was left in the store.
        wanted.text = kept.text;
        return Ok(wanted);
    }
    if wanted.mode == kept.mode {
        return Ok(kept);
    }
    if kept.mode == ContextMode::Reference {
        return Err(
            "This snapshot kept only a reference. Select its live source to capture contents."
                .into(),
        );
    }
    snapshot(
        &ContextSelection {
            source: kept.source,
            mode: wanted.mode,
            expected: None,
        },
        Excerpt {
            label: kept.label,
            location: kept.location,
            text: kept.text,
            note: kept.note,
            partial: kept.partial,
        },
    )
}

/// Captures a file, some of its lines, or a definition in it, from the
/// buffer open on it if there is one.
fn file(
    selection: &ContextSelection,
    job: &crate::server::mentions::Job,
    languages: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
) -> Result<ContextSnapshot> {
    let (path, lines) = &job.mentions[0];
    let selected = Path::new(path);
    let open = job.buffers.iter().any(|(path, ..)| path == selected);
    let real = if open {
        // A buffer's file may not exist yet: only its folder must.
        selected
            .parent()
            .ok_or("Source has no parent.")?
            .canonicalize()
            .map_err(|err| err.to_string())?
            .join(selected.file_name().ok_or("Source has no file name.")?)
    } else {
        selected
            .canonicalize()
            .map_err(|err| format!("Cannot read {path}: {err}"))?
    };
    if !real.starts_with(&job.project) {
        return Err("The selected source resolves outside this project.".into());
    }
    let location = format!(
        "{}{}",
        real.display(),
        clackatoo_protocol::lines_suffix(*lines)
    );
    let excerpt = |text: String, note: &str| Excerpt {
        label: path.clone(),
        location: location.clone(),
        text,
        note: note.into(),
        partial: false,
    };
    if selection.mode == ContextMode::Reference {
        let note = "Reference only; contents were not supplied.";
        return snapshot(selection, excerpt(String::new(), note));
    }
    let signature_only = matches!(
        selection.source,
        ContextRef::Symbol {
            signature: true,
            ..
        }
    );
    let text = if let ContextRef::Symbol {
        name,
        lines,
        signature,
        ..
    } = &selection.source
    {
        definition(job, &real, languages, name, *lines, *signature, cancelled)?
    } else {
        crate::server::mentions::captured_text_of(job, &real, *lines).ok_or_else(|| {
            format!(
                "Cannot include {path}: it is missing, non-text, or exceeds the capture \
                 limit. Select a range or Reference only."
            )
        })?
    };
    if let Some((first, last)) = *lines
        && !signature_only
        && text.lines().count() < (last - first + 1) as usize
    {
        return Err("The selected range extends past the source. Select it again.".into());
    }
    let unsaved = job.buffers.iter().any(|(_, modified, _)| *modified);
    let note = if unsaved {
        "Includes unsaved buffer text."
    } else {
        ""
    };
    snapshot(selection, excerpt(text, note))
}

/// The text of the definition `name` on `lines` of the file at `real`,
/// or only its signature: checked against the file as it is now, so a
/// definition that moved is refused rather than captured as other lines.
fn definition(
    job: &crate::server::mentions::Job,
    real: &Path,
    languages: Option<&Path>,
    name: &str,
    lines: (u32, u32),
    signature: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<String> {
    let full = crate::server::mentions::source_of(job, real)
        .ok_or("Cannot validate that symbol. Select a file range instead.")?;
    let languages = languages.ok_or("Install the source language pack to capture symbols.")?;
    let mut languages = clackatoo_syntax::Languages::new(languages.to_owned());
    let pack = languages
        .find(real)
        .ok_or("No language pack for this symbol.")?;
    let outline =
        crate::outline::with_parser(&pack, |parser| parser.extract(&pack, &full, cancelled))?;
    let definition = outline
        .definitions
        .into_iter()
        .find(|found| found.name == name && found.line == lines.0 && found.end_line == lines.1)
        .ok_or("That definition moved. Select it again.")?;
    if signature {
        return Ok(definition.signature);
    }
    Ok(full
        .split_inclusive('\n')
        .skip((lines.0 - 1) as usize)
        .take((lines.1 - lines.0 + 1) as usize)
        .collect())
}

/// Lists the folder at `path`, under the project `root`: names only, as
/// deep and as many as the limits allow, leaving out what the project's
/// ignore files do.
fn folder(selection: &ContextSelection, path: &Path, root: &Path) -> Result<ContextSnapshot> {
    let path = path.canonicalize().map_err(|err| err.to_string())?;
    if !path.starts_with(root) || !path.is_dir() {
        return Err("Choose a directory within this project.".into());
    }
    let mut text = String::new();
    let mut listed = 0;
    let mut partial = false;
    for entry in super::search::walk(&path, Some(LISTED_DEPTH), true) {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
            continue;
        }
        if listed == MOST_LISTED {
            text.push_str("[Further entries omitted]\n");
            partial = true;
            break;
        }
        listed += 1;
        if let Ok(relative) = entry.path().strip_prefix(&path) {
            text.push_str(&relative.display().to_string());
            text.push('\n');
        }
    }
    snapshot(
        selection,
        Excerpt {
            label: format!("Project outline · {}", path.display()),
            location: path.display().to_string(),
            text,
            note: "Directory listing, at most 1000 entries and three levels; file contents \
                   are not included."
                .into(),
            partial,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text the loop had in memory, chosen as an attachment.
    fn chosen(text: &str) -> Prepared {
        Prepared::Captured(
            ContextSelection {
                source: ContextRef::Diagnostics { path: None },
                mode: ContextMode::Contents,
                expected: None,
            },
            Excerpt {
                label: "Diagnostics".into(),
                location: "project".into(),
                text: text.into(),
                note: String::new(),
                partial: false,
            },
        )
    }

    fn message(store: PathBuf) -> Keep {
        Keep::Message {
            store,
            root: PathBuf::from("/project"),
            text: "explain".into(),
            in_use: BTreeSet::new(),
        }
    }

    fn captured(job: Capture) -> (Vec<ContextSnapshot>, Option<String>) {
        match run(job, &|| false).unwrap() {
            Output::Captured { snapshots, unkept } => (snapshots, unkept),
            Output::Found(_) => panic!("a capture finds nothing"),
        }
    }

    #[test]
    fn a_message_goes_with_its_context_though_the_store_cannot_keep_it() {
        let dir = tempfile::tempdir().unwrap();
        // The store's folder can't be made: a file is in its way.
        let blocked = dir.path().join("state");
        std::fs::write(&blocked, "not a folder").unwrap();
        let (snapshots, unkept) = captured(Capture {
            prepared: vec![chosen("one error")],
            languages: None,
            keep: message(blocked.join("context/snapshots.sqlite")),
        });
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].text, "one error");
        assert!(unkept.is_some(), "the store's failure is said, not thrown");
        // A store that works keeps them, and says nothing.
        let store = dir.path().join("context/snapshots.sqlite");
        let (snapshots, unkept) = captured(Capture {
            prepared: vec![chosen("one error")],
            languages: None,
            keep: message(store.clone()),
        });
        assert_eq!(unkept, None);
        assert_eq!(
            store::read(&store, &snapshots[0].id).unwrap().text,
            "one error"
        );
    }

    #[test]
    fn a_draft_attachment_that_cannot_be_kept_fails_as_the_draft_names_it_by_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("state");
        std::fs::write(&blocked, "not a folder").unwrap();
        let result = run(
            Capture {
                prepared: vec![chosen("an answer")],
                languages: None,
                keep: Keep::Draft {
                    store: blocked.join("snapshots.sqlite"),
                    in_use: BTreeSet::new(),
                },
            },
            &|| false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn a_mention_that_cannot_be_captured_is_left_out_but_a_chosen_source_holds_the_message() {
        let dir = tempfile::tempdir().unwrap();
        let missing = || {
            Prepared::Project(
                ContextSelection {
                    source: ContextRef::Project {
                        path: "gone".into(),
                    },
                    mode: ContextMode::Contents,
                    expected: None,
                },
                dir.path().join("gone"),
                dir.path().to_owned(),
            )
        };
        let (snapshots, _) = captured(Capture {
            prepared: vec![Prepared::Optional(Box::new(missing())), chosen("kept")],
            languages: None,
            keep: Keep::Nothing,
        });
        assert_eq!(snapshots.len(), 1);
        let held = run(
            Capture {
                prepared: vec![missing()],
                languages: None,
                keep: Keep::Nothing,
            },
            &|| false,
        );
        assert!(held.is_err());
    }

    #[test]
    fn a_folder_listing_leaves_out_what_is_ignored_and_never_reads_a_pipe_for_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src/built")).unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::write(root.join("src/built/out.o"), "").unwrap();
        std::fs::write(root.join("src/.ignore"), "built\n").unwrap();
        // The `ignore` crate's own reader would wait on this for ever.
        rustix::fs::mknodat(
            rustix::fs::CWD,
            root.join(".gitignore"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let (done, listed) = std::sync::mpsc::channel();
        let job = Capture {
            prepared: vec![Prepared::Project(
                ContextSelection {
                    source: ContextRef::Project { path: ".".into() },
                    mode: ContextMode::Contents,
                    expected: None,
                },
                root.clone(),
                root.clone(),
            )],
            languages: None,
            keep: Keep::Nothing,
        };
        std::thread::spawn(move || {
            let _ = done.send(captured(job));
        });
        let (snapshots, _) = listed
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the listing never waits on a pipe");
        let text = &snapshots[0].text;
        assert!(text.contains("src/main.rs"), "{text}");
        assert!(!text.contains("built"), "{text}");
    }
}
