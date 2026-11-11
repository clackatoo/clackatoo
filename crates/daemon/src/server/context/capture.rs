//! Bounded source reads and immutable snapshot construction on a scheduler worker.
use super::{
    ContextMode, ContextRef, ContextSelection, ContextSnapshot, Path, Prepared, Result, snapshot,
    store,
};

#[allow(
    clippy::too_many_lines,
    reason = "bounded disk, retained and directory capture adapters"
)]
pub(super) fn capture(
    prepared: Prepared,
    languages: Option<&Path>,
    context: &crate::work::Context,
) -> Result<ContextSnapshot> {
    match prepared {
        Prepared::Stored(mut snapshot, path) => {
            let retained = store::read(&path, &snapshot.id)?;
            if matches!(snapshot.source, ContextRef::Retained { .. }) {
                if snapshot.mode == retained.mode {
                    return Ok(retained);
                }
                if retained.mode == ContextMode::Reference {
                    return Err("This snapshot retained only a reference. Select its live source to capture contents.".into());
                }
                return self::snapshot(
                    &ContextSelection {
                        source: retained.source,
                        mode: snapshot.mode,
                        expected: None,
                    },
                    retained.label,
                    retained.location,
                    retained.text,
                    retained.note,
                );
            }
            snapshot.text = retained.text;
            Ok(snapshot)
        }
        Prepared::Optional(item) => capture(*item, languages, context),
        Prepared::Captured(selection, label, location, text, note) => {
            snapshot(&selection, label, location, text, note)
        }
        Prepared::File(selection, job) => {
            let (path, lines) = &job.mentions[0];
            let selected = Path::new(path);
            let real = if job.buffers.iter().any(|(path, ..)| path == selected) {
                selected
                    .parent()
                    .ok_or("Source has no parent.")?
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    .join(selected.file_name().ok_or("Source has no file name.")?)
            } else {
                selected
                    .canonicalize()
                    .map_err(|e| format!("Cannot read {path}: {e}"))?
            };
            if !real.starts_with(&job.project) {
                return Err("The selected source resolves outside this project.".into());
            }
            let location = format!(
                "{}{}",
                real.display(),
                clackatoo_protocol::lines_suffix(*lines)
            );
            let note = if job.buffers.iter().any(|(_, modified, _)| *modified) {
                "Includes unsaved buffer text."
            } else {
                ""
            };
            if selection.mode == ContextMode::Reference {
                return snapshot(
                    &selection,
                    path.clone(),
                    location,
                    String::new(),
                    "Reference only; contents were not supplied.".into(),
                );
            }
            let text = if matches!(selection.source, ContextRef::Symbol { .. }) {
                let full = crate::server::mentions::source_of(&job, &real)
                    .ok_or("Cannot validate that symbol. Select a file range instead.")?;
                let languages =
                    languages.ok_or("Install the source language pack to capture symbols.")?;
                let mut languages = clackatoo_syntax::Languages::new(languages.to_owned());
                let pack = languages
                    .find(&real)
                    .ok_or("No language pack for this symbol.")?;
                let outline = crate::outline::with_parser(&pack, |parser| {
                    parser.extract(&pack, &full, &|| context.cancelled())
                })?;
                let ContextRef::Symbol {
                    name,
                    lines,
                    signature,
                    ..
                } = &selection.source
                else {
                    unreachable!()
                };
                let definition = outline
                    .definitions
                    .into_iter()
                    .find(|d| d.name == *name && d.line == lines.0 && d.end_line == lines.1)
                    .ok_or("That definition moved. Select it again.")?;
                if *signature {
                    definition.signature
                } else {
                    full.split_inclusive('\n')
                        .skip((lines.0 - 1) as usize)
                        .take((lines.1 - lines.0 + 1) as usize)
                        .collect()
                }
            } else {
                crate::server::mentions::captured_text_of(&job,&real,*lines).ok_or_else(||format!("Cannot include {path}: it is missing, non-text, or exceeds the capture limit. Select a range or Reference only."))?
            };
            if let Some((_, last)) = lines
                && text.lines().count() < (*last - lines.unwrap().0 + 1) as usize
                && !matches!(
                    selection.source,
                    ContextRef::Symbol {
                        signature: true,
                        ..
                    }
                )
            {
                return Err("The selected range extends past the source. Select it again.".into());
            }
            snapshot(&selection, path.clone(), location, text, note.into())
        }
        Prepared::Project(selection, path, root) => {
            let path = path.canonicalize().map_err(|e| e.to_string())?;
            if !path.starts_with(root) || !path.is_dir() {
                return Err("Choose a directory within this project.".into());
            }
            let mut text = String::new();
            let mut count = 0;
            for entry in ignore::WalkBuilder::new(&path)
                .max_depth(Some(3))
                .follow_links(false)
                .build()
                .take(1001)
            {
                let Ok(entry) = entry else { continue };
                if entry.file_type().is_some_and(|t| t.is_symlink()) {
                    continue;
                }
                count += 1;
                if count > 1000 {
                    text.push_str("[Further entries omitted]\n");
                    break;
                }
                if let Ok(relative) = entry.path().strip_prefix(&path) {
                    text.push_str(&relative.display().to_string());
                    text.push('\n');
                }
            }
            snapshot(&selection,format!("Project outline · {}",path.display()),path.display().to_string(),text,"Directory listing, at most 1000 entries and three levels; file contents are not included.".into())
        }
    }
}
