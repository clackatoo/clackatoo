//! Specialists as Markdown files: reading a `SPECIALIST.md`, where they're
//! kept, and the writes Settings makes, which never leave those folders.

use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{SpecialistDefinition, SpecialistScope};
use serde_json::{Value, json};

use super::reading::Tree;
use super::{MOST_FILE, Roots, front_matter};
use crate::specialists::Specialist;

/// Numbers the files written beside the one they replace.
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The file that makes a folder a specialist.
pub(crate) const NAME: &str = "SPECIALIST.md";

/// The file that makes a folder a skillset, which may define a
/// specialist too.
const MANIFEST: &str = "skillset.toml";

/// Reads `text`, a `SPECIALIST.md`: the specialist's name, its
/// description, the bundles it includes, and the specialist. Its front
/// matter is the specialist's settings, and its body the instructions.
pub(crate) fn parse(text: &str) -> Result<(String, String, Vec<String>, Specialist), String> {
    let (header, body) = front_matter::split(text).map_err(|unsplit| unsplit.to_string())?;
    let mut metadata: Value =
        serde_yaml_ng::from_str(header).map_err(|err| format!("Invalid YAML: {err}"))?;
    let fields = metadata
        .as_object_mut()
        .ok_or("Frontmatter must be a mapping")?;
    let name = fields
        .remove("name")
        .and_then(|name| name.as_str().map(str::to_owned))
        .ok_or("Missing name")?;
    check_name(&name)?;
    let description = match fields.remove("description") {
        None => String::new(),
        Some(Value::String(description)) => description,
        _ => return Err("description must be a string".into()),
    };
    let includes = match fields.remove("includes") {
        None => Vec::new(),
        Some(includes) => serde_json::from_value::<Vec<String>>(includes)
            .map_err(|_| "includes must be a list of bundle names")?,
    };
    if fields.contains_key("instructions") {
        return Err("Put instructions in the Markdown body".into());
    }
    let body = body.replace("\r\n", "\n");
    fields.insert("instructions".into(), json!(body.trim_matches('\n')));
    let specialist = super::skillsets::parse_specialist(&name, &description, &metadata)?;
    Ok((name, description, includes, specialist))
}

/// Checks `name` can be a specialist's, or a skillset's: it's a folder's
/// name too.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    let fits = !name.is_empty()
        && name.len() <= 64
        && (name.bytes())
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_".contains(&c));
    if !fits {
        return Err("Use a name of 1–64 lowercase letters, digits, hyphens or underscores".into());
    }
    Ok(())
}

/// A folder specialists, or skillsets, are kept in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    /// The folder nothing of it is outside: a project's folder, the
    /// user's home, or clackatoo's config directory.
    pub(crate) base: PathBuf,
    /// Where it is, below `base`.
    pub(crate) below: PathBuf,
    pub(crate) scope: SpecialistScope,
}

impl Location {
    /// How its files are read: a project's without following links.
    pub(super) fn tree(&self) -> Tree {
        match self.scope {
            SpecialistScope::Project => Tree::project(&self.base),
            SpecialistScope::Personal => Tree::personal(&self.base),
        }
    }

    /// The folder itself.
    fn folder(&self) -> PathBuf {
        self.base.join(&self.below)
    }
}

/// Where specialists are kept, the one that wins first: the project's
/// folders, nearest first, then clackatoo's config directory, then home.
pub(crate) fn locations(roots: &Roots) -> Vec<Location> {
    let place = |base: &Path, below: &str, scope| Location {
        base: base.to_owned(),
        below: below.into(),
        scope,
    };
    let mut places: Vec<_> = (roots.project_folders())
        .map(|folder| place(folder, ".agents/specialists", SpecialistScope::Project))
        .collect();
    if let Some(config) = &roots.config {
        places.push(place(config, "specialists", SpecialistScope::Personal));
    }
    places.push(place(
        &roots.home,
        ".agents/specialists",
        SpecialistScope::Personal,
    ));
    places
}

/// Where specialists and skillsets are kept: [`locations`], then the
/// `skillsets` folder beside each.
pub(super) fn package_locations(roots: &Roots) -> Vec<Location> {
    let mut places = locations(roots);
    let sets: Vec<_> = (places.iter())
        .map(|place| Location {
            below: place.below.with_file_name("skillsets"),
            ..place.clone()
        })
        .collect();
    places.extend(sets);
    places
}

/// Where a new specialist called `name` is saved: the user's own go in
/// the first of their folders, and a project's at the project's root.
pub(crate) fn destination(roots: &Roots, scope: SpecialistScope, name: &str) -> PathBuf {
    let places = locations(roots);
    let found = match scope {
        SpecialistScope::Personal => places.iter().find(|place| place.scope == scope),
        SpecialistScope::Project => places.iter().rev().find(|place| place.scope == scope),
    };
    found
        .map_or_else(|| roots.cwd.join(".agents/specialists"), Location::folder)
        .join(name)
        .join(NAME)
}

/// Whether the file at `path` is the project's or the user's own.
pub(crate) fn source_scope(roots: &Roots, path: &Path) -> SpecialistScope {
    let in_project =
        (roots.project_folders()).any(|folder| path.starts_with(folder.join(".agents")));
    if in_project {
        SpecialistScope::Project
    } else {
        SpecialistScope::Personal
    }
}

/// Whether Settings changes the specialist called `name` in place, in
/// the file at `path` that defines it: only a `SPECIALIST.md` in a folder
/// of that name. One a `skillset.toml` defines is copied instead, as
/// writing a `SPECIALIST.md` beside it would put the manifest, and the
/// version it gives, out of use.
pub(crate) fn editable(path: &Path, name: &str) -> bool {
    let in_own_folder = (path.parent())
        .and_then(Path::file_name)
        .is_some_and(|folder| folder == name);
    in_own_folder && path.file_name().is_some_and(|file| file == NAME)
}

/// The `SPECIALIST.md` that `edit`, Settings' draft, amounts to. A field
/// left empty is left out, so it takes its default, rather than being
/// written as empty text.
fn render(edit: &SpecialistDefinition) -> Result<String, String> {
    let too_big = || format!("A specialist's file can't exceed {} KiB", MOST_FILE / 1024);
    check_name(&edit.name)?;
    if edit.settings.len() as u64 > MOST_FILE {
        return Err(too_big());
    }
    let mut value: Value = serde_json::from_str(&edit.settings).map_err(|e| e.to_string())?;
    let fields = value
        .as_object_mut()
        .ok_or("Specialist settings must be an object")?;
    let body = match fields.remove("instructions") {
        Some(Value::String(body)) => body,
        None => String::new(),
        _ => return Err("instructions must be text".into()),
    };
    fields.remove("replace");
    fields.retain(|_, value| match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        _ => true,
    });
    fields.insert("name".into(), json!(edit.name));
    let yaml = serde_yaml_ng::to_string(&value).map_err(|e| e.to_string())?;
    let text = format!("---\n{yaml}---\n\n{body}\n");
    if text.len() as u64 > MOST_FILE {
        return Err(too_big());
    }
    parse(&text)?;
    Ok(text)
}

/// Saves `edit` as a `SPECIALIST.md`: at `path`, if it changes one
/// that's there, or else as a new one of `scope`. Written beside the file
/// and renamed over it, so it's never half there; whatever else its
/// folder holds stays. `edit.saved`, the file as Settings read it, must
/// still be what's there, so an edit made meanwhile is never lost.
pub(crate) fn save(
    roots: &Roots,
    edit: &SpecialistDefinition,
    scope: SpecialistScope,
    path: Option<&str>,
) -> Result<PathBuf, String> {
    use rustix::fs::{Mode, OFlags};
    let text = render(edit)?;
    let target = path.map_or_else(|| destination(roots, scope, &edit.name), PathBuf::from);
    if target.file_name().is_some_and(|name| name == MANIFEST) {
        return Err(format!(
            "{} is defined in its skillset.toml. Make a custom copy to change it.",
            edit.name
        ));
    }
    let new_in_project = path.is_none() && scope == SpecialistScope::Project;
    if new_in_project && !locations(roots).iter().any(|place| place.scope == scope) {
        return Err("Choose a project workspace before creating a project specialist".into());
    }
    let place = package_locations(roots)
        .into_iter()
        .find(|place| place.folder().join(&edit.name).join(NAME) == target)
        .ok_or("Save specialists inside a registered personal or project specialists folder")?;
    let (base, below) = (&place.base, &place.below);
    // A copy of a specialist its folder's skillset.toml defines would put
    // that manifest out of use from beside it.
    let manifest = below.join(&edit.name).join(MANIFEST);
    if !matches!(place.tree().text(&manifest), Ok(None)) {
        return Err(format!(
            "A skillset.toml defines {} in that folder. Save the copy under another name.",
            edit.name
        ));
    }
    super::skillsets::validate_edit(
        roots,
        &place.tree(),
        &below.join(&edit.name),
        &target,
        &text,
    )?;
    // The configured root is trusted, like daemon/settings.lua's directory;
    // a fresh installation may not have created it yet. Package traversal
    // below that root still refuses symlinks at every component.
    if roots.config.as_ref() == Some(base) {
        std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    }
    let directory = crate::regular::dir_below(base, &below.join(&edit.name), true)
        .map_err(|e| e.to_string())?;
    let mut lock = std::fs::File::from(
        rustix::fs::openat(
            &directory,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| e.to_string())?,
    );
    lock.lock().map_err(|e| e.to_string())?;
    let (current, mode) =
        match crate::regular::open_at(&directory, Path::new(NAME), crate::regular::Links::Refuse) {
            Ok(file) => {
                let mode = file
                    .metadata()
                    .map_err(|e| e.to_string())?
                    .permissions()
                    .mode()
                    & 0o777;
                let mut s = String::new();
                file.take(MOST_FILE + 1)
                    .read_to_string(&mut s)
                    .map_err(|e| e.to_string())?;
                (Some(s), mode)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, 0o644),
            Err(e) => return Err(e.to_string()),
        };
    if current != edit.saved {
        return Err(
            "This specialist file changed since it was opened. Reopen Settings and try again."
                .into(),
        );
    }
    let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = format!(".SPECIALIST.{}.{nonce}.partial", std::process::id());
    let result = (|| {
        let fd = rustix::fs::openat(
            &directory,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(mode),
        )
        .map_err(|e| e.to_string())?;
        let mut file = std::fs::File::from(fd);
        file.set_permissions(std::fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        rustix::fs::renameat(&directory, temporary.as_str(), &directory, NAME)
            .map_err(|e| e.to_string())?;
        lock.flush()
            .and_then(|()| lock.sync_all())
            .map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(&directory, temporary.as_str(), rustix::fs::AtFlags::empty());
    }
    result?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::Catalogue;

    /// A home with a repository in it, and the roots of a chat in a
    /// folder of that repository.
    fn fixture() -> (tempfile::TempDir, Roots) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap().join("home");
        let cwd = home.join("repo/sub");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(home.join("repo/.git")).unwrap();
        let roots = Roots {
            project: home.join("repo"),
            ..Roots::in_project(cwd, home, None)
        };
        (dir, roots)
    }

    /// The text of the file at `path`, if it's there.
    fn contents(path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    /// Settings' draft of a specialist called `name`, with `body` for
    /// instructions.
    fn draft(name: &str, body: &str) -> SpecialistDefinition {
        let settings = json!({
            "instructions": body,
            "model": {"claude": "sonnet", "codex": "gpt-6-luna"},
            "skills": [],
            "limits": {"access": "read-only", "seconds": 30},
        });
        SpecialistDefinition {
            name: name.into(),
            settings: settings.to_string(),
            saved: None,
            locked: None,
        }
    }

    #[test]
    fn files_roundtrip_scopes_and_detect_external_edits_without_lua() {
        let (_dir, roots) = fixture();
        let mut edit = draft("reviewer", "First line\nSecond line: \"quoted\"");
        let path = save(&roots, &edit, SpecialistScope::Personal, None).unwrap();
        let text = contents(&path).unwrap();
        assert!(text.contains("---\n") && text.contains("First line\nSecond line"));
        let catalogue = Catalogue::discover(&roots);
        let role = &catalogue.specialists["reviewer"];
        assert_eq!(role.instructions, "First line\nSecond line: \"quoted\"");
        assert_eq!(role.extra.skills, Some(vec![]));
        assert_eq!(role.extra.budget.seconds, Some(30));
        assert_eq!(catalogue.specialist_texts[&path], text);
        assert!(
            save(
                &roots,
                &edit,
                SpecialistScope::Personal,
                Some(path.to_str().unwrap())
            )
            .unwrap_err()
            .contains("changed since")
        );
        edit.saved = Some(text);
        edit.settings = json!({"instructions":"Revised role"}).to_string();
        save(
            &roots,
            &edit,
            SpecialistScope::Personal,
            Some(path.to_str().unwrap()),
        )
        .unwrap();
        let role = &Catalogue::discover(&roots).specialists["reviewer"];
        assert!(role.models.is_empty() && role.extra.budget.seconds.is_none());
        std::fs::write(&path, "external edit").unwrap();
        assert!(
            save(
                &roots,
                &edit,
                SpecialistScope::Personal,
                Some(path.to_str().unwrap())
            )
            .is_err()
        );
        assert_eq!(contents(&path).as_deref(), Some("external edit"));
        let path = save(
            &roots,
            &draft("reviewer", "Project role"),
            SpecialistScope::Project,
            None,
        )
        .unwrap();
        assert_eq!(
            path,
            roots
                .home
                .join("repo/.agents/specialists/reviewer/SPECIALIST.md")
        );
        assert_eq!(
            Catalogue::discover(&roots).specialists["reviewer"].instructions,
            "Project role"
        );
    }
    #[test]
    fn first_personal_save_creates_a_fresh_config_root() {
        let (_dir, mut roots) = fixture();
        roots.config = Some(roots.home.join(".config/clackatoo"));
        let path = save(
            &roots,
            &draft("reviewer", "Review"),
            SpecialistScope::Personal,
            None,
        )
        .unwrap();
        assert_eq!(
            path,
            roots
                .home
                .join(".config/clackatoo/specialists/reviewer/SPECIALIST.md")
        );
        assert_eq!(
            Catalogue::discover(&roots).specialists["reviewer"].instructions,
            "Review"
        );
    }

    #[test]
    fn file_saves_preserve_support_files_and_refuse_symlink_escapes() {
        let (_dir, roots) = fixture();
        let path = save(
            &roots,
            &draft("reviewer", "Original"),
            SpecialistScope::Project,
            None,
        )
        .unwrap();
        let folder = path.parent().unwrap();
        std::fs::write(folder.join("reference.txt"), "Keep me").unwrap();
        let mut edit = draft("reviewer", "Changed");
        edit.saved = contents(&path);
        save(
            &roots,
            &edit,
            SpecialistScope::Project,
            Some(path.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join("reference.txt")).unwrap(),
            "Keep me"
        );
        let root = roots.home.join(".agents/specialists");
        std::fs::create_dir_all(&root).unwrap();
        let outside = roots.home.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        assert!(
            save(
                &roots,
                &draft("escape", "No escape"),
                SpecialistScope::Personal,
                None
            )
            .is_err()
        );
        assert!(!outside.join(NAME).exists());
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(folder.join("reference.txt"), &path).unwrap();
        assert!(
            save(
                &roots,
                &edit,
                SpecialistScope::Project,
                Some(path.to_str().unwrap())
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("reference.txt")).unwrap(),
            "Keep me"
        );
    }
    #[test]
    fn invalid_dependency_edits_leave_the_previous_file_and_permissions_intact() {
        let (_dir, roots) = fixture();
        let path = save(
            &roots,
            &draft("reviewer", "Original"),
            SpecialistScope::Project,
            None,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let before = contents(&path);
        let mut edit = draft("reviewer", "Revised");
        edit.saved = before.clone();
        edit.settings = json!({"instructions":"Revised","includes":["reviewer"]}).to_string();
        assert!(
            save(
                &roots,
                &edit,
                SpecialistScope::Project,
                Some(path.to_str().unwrap())
            )
            .unwrap_err()
            .contains("cycle")
        );
        assert_eq!(contents(&path), before);
        edit.settings = json!({"instructions":"Revised"}).to_string();
        save(
            &roots,
            &edit,
            SpecialistScope::Project,
            Some(path.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn editing_a_local_bundle_retains_relative_dependencies_and_support_files() {
        let (_dir, roots) = fixture();
        let folder = roots.home.join("repo/.agents/skillsets/reviewer");
        std::fs::create_dir_all(folder.join("helpers/checks/skills/check")).unwrap();
        std::fs::write(
            folder.join(NAME),
            "---\nname: reviewer\nincludes: [./helpers/checks]\n---\nRead reference.txt\n",
        )
        .unwrap();
        std::fs::write(folder.join("reference.txt"), "Keep supporting content").unwrap();
        std::fs::write(
            folder.join("helpers/checks/skillset.toml"),
            "format=1\nname='checks'\ndescription='Checks'\n",
        )
        .unwrap();
        std::fs::write(
            folder.join("helpers/checks/skills/check/SKILL.md"),
            "---\nname: check\ndescription: Check\n---\nCheck code\n",
        )
        .unwrap();
        let before = Catalogue::discover(&roots);
        let path = &before.specialist_sources["reviewer"];
        assert!(editable(path, "reviewer"));
        let mut edit = super::super::specialist_edit::definition(&before.specialists["reviewer"]);
        edit.saved = Some(before.specialist_texts[path].clone());
        let mut v: Value = serde_json::from_str(&edit.settings).unwrap();
        v["instructions"] = json!("Revised role");
        v["includes"] = json!(before.specialist_includes["reviewer"]);
        edit.settings = v.to_string();
        save(
            &roots,
            &edit,
            SpecialistScope::Project,
            Some(path.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join("reference.txt")).unwrap(),
            "Keep supporting content"
        );
        let after = Catalogue::discover(&roots);
        assert_eq!(after.specialists["reviewer"].instructions, "Revised role");
        assert!(
            after.specialists["reviewer"]
                .instruction_text()
                .contains(folder.to_str().unwrap())
        );
        assert!(
            after
                .scoped(&after.specialists["reviewer"])
                .unwrap()
                .commands()
                .iter()
                .any(|c| c.name == "check")
        );
    }

    #[test]
    fn markdown_packages_share_dependency_resolution_with_skillsets() {
        let (_dir, roots) = fixture();
        let root = roots.home.join("repo/.agents/skillsets/checks");
        std::fs::create_dir_all(root.join("skills/check")).unwrap();
        std::fs::write(
            root.join("skillset.toml"),
            "format=1\nname='checks'\ndescription='Checks'\nincludes=['reviewer']\n",
        )
        .unwrap();
        std::fs::write(
            root.join("skills/check/SKILL.md"),
            "---\nname: check\ndescription: Check\n---\nCheck code\n",
        )
        .unwrap();
        let edit = draft("reviewer", "Review code");
        let path = save(&roots, &edit, SpecialistScope::Project, None).unwrap();
        let catalogue = Catalogue::discover(&roots);
        assert!(catalogue.specialists.contains_key("reviewer"));
        assert!(catalogue.scoped(&catalogue.specialists["reviewer"]).is_ok());
        // A cycle crossing a plain skillset and a Markdown specialist is rejected.
        std::fs::write(
            path,
            "---\nname: reviewer\nskillsets: [checks]\n---\nReview\n",
        )
        .unwrap();
        assert!(
            !Catalogue::discover(&roots)
                .specialists
                .contains_key("reviewer")
        );
    }

    /// A specialist its folder's `skillset.toml` defines is only copied:
    /// never changed in place, nor shadowed by a file written beside it.
    #[test]
    fn a_specialist_a_manifest_defines_is_copied_and_never_shadowed() {
        let (_dir, roots) = fixture();
        let folder = roots.home.join(".agents/specialists/reviewer");
        std::fs::create_dir_all(&folder).unwrap();
        let manifest = folder.join("skillset.toml");
        std::fs::write(
            &manifest,
            "format=1\nname='reviewer'\ndescription='Reviews'\nversion='2'\n[specialist]\ninstructions='FROM TOML'\n",
        )
        .unwrap();
        let found = Catalogue::discover(&roots);
        assert_eq!(found.specialist_sources["reviewer"], manifest);
        assert!(!editable(&manifest, "reviewer"));
        let edit = draft("reviewer", "Changed");
        let personal = SpecialistScope::Personal;
        let in_place = save(&roots, &edit, personal, Some(manifest.to_str().unwrap()));
        assert!(in_place.unwrap_err().contains("custom copy"));
        let beside = save(&roots, &edit, personal, None);
        assert!(beside.unwrap_err().contains("another name"));
        assert!(!folder.join(NAME).exists());
        let role = &Catalogue::discover(&roots).specialists["reviewer"];
        assert_eq!(role.instructions, "FROM TOML");
        // Under another name, it's a specialist of its own.
        save(&roots, &draft("reviewer-two", "Changed"), personal, None).unwrap();
        assert!(folder.with_file_name("reviewer-two").join(NAME).exists());
    }

    /// What Settings leaves empty isn't written as empty text, and one
    /// limit holds for the draft and the file.
    #[test]
    fn empty_fields_are_left_out_and_a_file_too_big_is_refused() {
        let (_dir, roots) = fixture();
        let mut edit = draft("reviewer", "Review");
        edit.settings = json!({
            "instructions": "Review",
            "description": "",
            "when_to_use": "  ",
            "agent": null,
            "skills": [],
        })
        .to_string();
        let path = save(&roots, &edit, SpecialistScope::Personal, None).unwrap();
        let text = contents(&path).unwrap();
        assert_eq!(text, "---\nskills: []\nname: reviewer\n---\n\nReview\n");
        let mut big = draft("big", "");
        big.settings = json!({ "instructions": "x".repeat(64 * 1024) }).to_string();
        let refused = save(&roots, &big, SpecialistScope::Personal, None).unwrap_err();
        assert!(refused.contains("64 KiB"), "{refused}");
    }
}
