//! Markdown specialist packages and safe, scoped Settings writes.
use super::{MOST_FILE, Path, PathBuf, Roots};
use crate::specialists::Specialist;
use clackatoo_protocol::{SpecialistDefinition, SpecialistScope};
use serde_json::{Value, json};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) const NAME: &str = "SPECIALIST.md";

pub(super) fn read(
    path: &Path,
) -> Result<(String, String, Vec<String>, Specialist, String), String> {
    let text = contents(path)?.ok_or("SPECIALIST.md is missing")?;
    let (name, description, includes, specialist) = parse(&text)?;
    Ok((name, description, includes, specialist, text))
}

pub(crate) fn parse(text: &str) -> Result<(String, String, Vec<String>, Specialist), String> {
    let normalized = text.replace("\r\n", "\n");
    let rest = normalized
        .strip_prefix("---\n")
        .ok_or("Missing YAML frontmatter")?;
    let (header, body) = rest
        .split_once("\n---\n")
        .ok_or("Unclosed YAML frontmatter")?;
    let mut metadata: Value =
        serde_yaml_ng::from_str(header).map_err(|e| format!("Invalid YAML: {e}"))?;
    let fields = metadata
        .as_object_mut()
        .ok_or("Frontmatter must be a mapping")?;
    let name = fields
        .remove("name")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or("Missing name")?;
    check_name(&name)?;
    let description = match fields.remove("description") {
        None => String::new(),
        Some(Value::String(s)) => s,
        _ => return Err("description must be a string".into()),
    };
    let includes = match fields.remove("includes") {
        None => Vec::new(),
        Some(v) => serde_json::from_value::<Vec<String>>(v)
            .map_err(|_| "includes must be a list of bundle names")?,
    };
    if fields.contains_key("instructions") {
        return Err("Put instructions in the Markdown body".into());
    }
    fields.insert("instructions".into(), json!(body.trim_matches('\n')));
    let specialist = super::skillsets::parse_specialist(&name, &description, &metadata)?;
    Ok((name, description, includes, specialist))
}

pub(crate) fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_".contains(&c))
    {
        return Err("Use a name of 1–64 lowercase letters, digits, hyphens or underscores".into());
    }
    Ok(())
}

/// The source file, bounded and regular; missing is distinct from unreadable.
pub(crate) fn contents(path: &Path) -> Result<Option<String>, String> {
    let file = match crate::regular::open(path, crate::regular::Links::Refuse) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut text = String::new();
    file.take(MOST_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MOST_FILE {
        return Err("Specialist file exceeds 64 KiB".into());
    }
    Ok(Some(text))
}

/// Canonical package locations, highest priority first. The containing base is
/// opened once; directory traversal beneath it refuses every symlink.
pub(crate) fn locations(roots: &Roots) -> Vec<(PathBuf, PathBuf, SpecialistScope)> {
    let mut out = Vec::new();
    for dir in roots.cwd.ancestors() {
        if dir == roots.home {
            break;
        }
        out.push((
            dir.into(),
            ".agents/specialists".into(),
            SpecialistScope::Project,
        ));
        if dir.join(".git").exists() {
            break;
        }
    }
    if let Some(config) = &roots.config {
        out.push((
            config.clone(),
            "specialists".into(),
            SpecialistScope::Personal,
        ));
    }
    out.push((
        roots.home.clone(),
        ".agents/specialists".into(),
        SpecialistScope::Personal,
    ));
    out
}

pub(crate) fn destination(roots: &Roots, scope: SpecialistScope, name: &str) -> PathBuf {
    let choices = locations(roots);
    let found = match scope {
        SpecialistScope::Personal => choices.iter().find(|(_, _, s)| *s == scope),
        SpecialistScope::Project => choices.iter().rev().find(|(_, _, s)| *s == scope),
    };
    found
        .map_or_else(
            || roots.cwd.join(".agents/specialists"),
            |(base, below, _)| base.join(below),
        )
        .join(name)
        .join(NAME)
}

pub(crate) fn source_scope(roots: &Roots, path: &Path) -> SpecialistScope {
    for dir in roots.cwd.ancestors() {
        if dir == roots.home {
            break;
        }
        if path.starts_with(dir.join(".agents")) {
            return SpecialistScope::Project;
        }
    }
    SpecialistScope::Personal
}

fn package_locations(roots: &Roots) -> Vec<(PathBuf, PathBuf, SpecialistScope)> {
    let mut places = locations(roots);
    let sets: Vec<_> = places
        .iter()
        .map(|(base, below, scope)| (base.clone(), below.with_file_name("skillsets"), *scope))
        .collect();
    places.extend(sets);
    places
}
pub(crate) fn editable(path: &Path, name: &str) -> bool {
    path.parent()
        .and_then(Path::file_name)
        .is_some_and(|folder| folder == name)
        && path
            .file_name()
            .is_some_and(|file| file == NAME || file == "skillset.toml")
}

fn render(edit: &SpecialistDefinition) -> Result<String, String> {
    check_name(&edit.name)?;
    if edit.settings.len() > 128 * 1024 {
        return Err("Specialist draft exceeds 128 KiB".into());
    }
    let mut value: Value = serde_json::from_str(&edit.settings).map_err(|e| e.to_string())?;
    let fields = value
        .as_object_mut()
        .ok_or("Specialist settings must be an object")?;
    let body = match fields.remove("instructions") {
        Some(Value::String(s)) => s,
        None => String::new(),
        _ => return Err("instructions must be text".into()),
    };
    fields.remove("replace");
    fields.insert("name".into(), json!(edit.name));
    let yaml = serde_yaml_ng::to_string(&value).map_err(|e| e.to_string())?;
    let text = format!("---\n{yaml}---\n\n{body}\n");
    if text.len() as u64 > MOST_FILE {
        return Err("Specialist file exceeds 64 KiB".into());
    }
    parse(&text)?;
    Ok(text)
}

/// Atomically edits one SPECIALIST.md. Auxiliary files and bundle contents stay
/// in place. The expected contents protect both creates and updates from races.
pub(crate) fn save(
    roots: &Roots,
    edit: &SpecialistDefinition,
    scope: SpecialistScope,
    path: Option<&str>,
) -> Result<PathBuf, String> {
    use rustix::fs::{Mode, OFlags};
    let text = render(edit)?;
    let mut target = path.map_or_else(|| destination(roots, scope, &edit.name), PathBuf::from);
    if target
        .file_name()
        .is_some_and(|name| name == "skillset.toml")
    {
        target.set_file_name(NAME);
    }
    if path.is_none()
        && scope == SpecialistScope::Project
        && !locations(roots).iter().any(|(_, _, s)| *s == scope)
    {
        return Err("Choose a project workspace before creating a project specialist".into());
    }
    super::skillsets::validate_edit(roots, &target, &text)?;
    let (base, below, _) = package_locations(roots)
        .into_iter()
        .find(|(base, below, _)| base.join(below).join(&edit.name).join(NAME) == target)
        .ok_or("Save specialists inside a registered personal or project specialists folder")?;
    // The configured root is trusted, like daemon/settings.lua's directory;
    // a fresh installation may not have created it yet. Package traversal
    // below that root still refuses symlinks at every component.
    if roots.config.as_ref() == Some(&base) {
        std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    }
    let directory = crate::regular::dir_below(&base, &below.join(&edit.name), true)
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
    fn fixture() -> (tempfile::TempDir, Roots) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let cwd = home.join("repo/sub");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(home.join("repo/.git")).unwrap();
        (
            dir,
            Roots {
                cwd,
                home,
                config: None,
            },
        )
    }
    fn draft(name: &str, body: &str) -> SpecialistDefinition {
        SpecialistDefinition {name:name.into(),settings:json!({"instructions":body,"model":{"claude":"sonnet","codex":"gpt-6-luna"},"skills":[],"limits":{"access":"read-only","seconds":30}}).to_string(),saved:None,locked:None}
    }
    #[test]
    fn files_roundtrip_scopes_and_detect_external_edits_without_lua() {
        let (_dir, roots) = fixture();
        let mut edit = draft("reviewer", "First line\nSecond line: \"quoted\"");
        let path = save(&roots, &edit, SpecialistScope::Personal, None).unwrap();
        let text = contents(&path).unwrap().unwrap();
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
        assert_eq!(contents(&path).unwrap().as_deref(), Some("external edit"));
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
        edit.saved = contents(&path).unwrap();
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
        let before = contents(&path).unwrap();
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
        assert_eq!(contents(&path).unwrap(), before);
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
}
