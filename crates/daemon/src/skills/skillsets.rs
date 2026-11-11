//! Local bundle dependencies are resolved before exposing capabilities.
//! Specialists keep their own directories; only plain skillsets merge them.

use super::{HashSet, MOST_ENTRIES, MOST_FILE, Path, PathBuf};
use crate::specialists::{self, Extras, Specialist};
use std::collections::BTreeMap;
use std::io::Read as _;

#[derive(Clone, Debug)]
struct Bundle {
    root: PathBuf,
    source: PathBuf,
    text: Option<String>,
    name: String,
    includes: Vec<String>,
    specialist: Option<Specialist>,
}

#[derive(Default)]
pub(super) struct Packages {
    pub(super) directories: BTreeMap<PathBuf, Vec<(PathBuf, String)>>,
    pub(super) specialists: BTreeMap<String, Specialist>,
    pub(super) sources: BTreeMap<String, PathBuf>,
    pub(super) texts: BTreeMap<PathBuf, String>,
    pub(super) includes: BTreeMap<String, Vec<String>>,
    pub(super) sets: BTreeMap<String, Option<Vec<PathBuf>>>,
}

#[derive(Default)]
struct Resolved {
    specialists: BTreeMap<String, Specialist>,
    sources: BTreeMap<String, PathBuf>,
    texts: BTreeMap<PathBuf, String>,
    includes: BTreeMap<String, Vec<String>>,
    sets: BTreeMap<String, Option<Vec<PathBuf>>>,
    names: BTreeMap<String, PathBuf>,
}

pub(super) fn discover(folders: &[(PathBuf, &str, bool)]) -> Packages {
    let mut packages = Packages::default();
    let mut named = BTreeMap::new();
    let mut roots = Vec::new();
    let mut chosen = HashSet::new();
    let mut budget = MOST_ENTRIES;
    for (root, source, bundles) in folders {
        if !bundles {
            continue;
        }
        let Ok(children) = root.read_dir() else {
            continue;
        };
        let mut children: Vec<_> = children.take(budget).filter_map(Result::ok).collect();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            if budget == 0 {
                break;
            }
            budget -= 1;
            if child.file_name().to_string_lossy().starts_with('.') || !child.path().is_dir() {
                continue;
            }
            match package(&child.path()) {
                Ok(bundle) if chosen.insert(bundle.name.clone()) => {
                    named.insert(bundle.name.clone(), bundle.clone());
                    roots.push((root.clone(), (*source).to_owned(), bundle));
                }
                Ok(_) => {}
                Err(why) => {
                    chosen.insert(child.file_name().to_string_lossy().into_owned());
                    log::warn!("skillset {}: {why}", child.path().display());
                }
            }
        }
    }
    for (root, source, bundle) in roots {
        let mut resolved = Resolved::default();
        let mut left = 256;
        match resolve(&bundle, &named, &mut Vec::new(), &mut resolved, &mut left) {
            Ok(directories) => {
                if bundle.specialist.is_none() {
                    let label = format!("{source} · skillset {}", bundle.name);
                    packages
                        .directories
                        .entry(root)
                        .or_default()
                        .extend(directories.into_iter().map(|dir| (dir, label.clone())));
                }
                for (name, specialist) in resolved.specialists {
                    if let Some(old) = packages.specialists.get(&name) {
                        if old != &specialist {
                            log::warn!(
                                "conflicting specialist {name}; keeping its higher-priority definition"
                            );
                        }
                    } else {
                        if let Some(path) = resolved.sources.get(&name) {
                            packages.sources.insert(name.clone(), path.clone());
                            if let Some(text) = resolved.texts.get(path) {
                                packages.texts.insert(path.clone(), text.clone());
                            }
                        }
                        if let Some(includes) = resolved.includes.get(&name) {
                            packages.includes.insert(name.clone(), includes.clone());
                        }
                        packages.specialists.insert(name, specialist);
                    }
                }
                for (name, dirs) in resolved.sets {
                    packages.sets.entry(name).or_insert(dirs);
                }
            }
            Err(why) => log::warn!("skillset {}: {why}", bundle.root.display()),
        }
    }
    packages
}

fn resolve(
    bundle: &Bundle,
    named: &BTreeMap<String, Bundle>,
    stack: &mut Vec<PathBuf>,
    out: &mut Resolved,
    left: &mut usize,
) -> Result<Vec<PathBuf>, String> {
    if stack.contains(&bundle.root) {
        return Err(format!("dependency cycle at {}", bundle.name));
    }
    if stack.len() >= 32 || *left == 0 {
        return Err("bundle dependency limit exceeded".into());
    }
    *left -= 1;
    if let Some(old) = out.names.get(&bundle.name) {
        if old != &bundle.root {
            return Err(format!("conflicting bundle name {}", bundle.name));
        }
        return Ok(out
            .sets
            .get(&bundle.name)
            .cloned()
            .flatten()
            .unwrap_or_default());
    }
    stack.push(bundle.root.clone());
    let mut directories = vec![bundle.root.join("skills")];
    let selected = bundle
        .specialist
        .as_ref()
        .and_then(|s| s.extra.skillsets.as_ref())
        .into_iter()
        .flatten();
    for reference in bundle.includes.iter().chain(selected) {
        let child = if reference.contains('/') || reference.starts_with('.') {
            let path = Path::new(reference);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err("bundle paths must stay inside their containing bundle; use a registered name for other skillsets".into());
            }
            let child = package(&bundle.root.join(path))?;
            if !child.root.starts_with(&bundle.root) {
                return Err("bundle dependency leaves its containing folder".into());
            }
            child
        } else {
            named
                .get(reference)
                .cloned()
                .ok_or_else(|| format!("missing bundle {reference}"))?
        };
        let inherited = resolve(&child, named, stack, out, left)?;
        if child.specialist.is_none() {
            directories.extend(inherited);
        }
    }
    stack.pop();
    let mut unique = HashSet::new();
    directories.retain(|path| unique.insert(path.clone()));
    out.names.insert(bundle.name.clone(), bundle.root.clone());
    if let Some(mut specialist) = bundle.specialist.clone() {
        specialist.extra.directories.clone_from(&directories);
        out.sources
            .insert(bundle.name.clone(), bundle.source.clone());
        if let Some(text) = &bundle.text {
            out.texts.insert(bundle.source.clone(), text.clone());
        }
        out.includes
            .insert(bundle.name.clone(), bundle.includes.clone());
        out.specialists.insert(bundle.name.clone(), specialist);
        out.sets.insert(bundle.name.clone(), None);
    } else {
        out.sets
            .insert(bundle.name.clone(), Some(directories.clone()));
    }
    Ok(directories)
}

/// Validate the proposed role against the same package graph used at launch.
/// It is injected in memory before any file is replaced.
pub(super) fn validate_edit(roots: &super::Roots, target: &Path, text: &str) -> Result<(), String> {
    let mut named = BTreeMap::new();
    let mut left = MOST_ENTRIES;
    for (base, below, _) in super::specialist_files::locations(roots) {
        for folder in [
            base.join(&below),
            base.join(below.with_file_name("skillsets")),
        ] {
            let Ok(children) = folder.read_dir() else {
                continue;
            };
            let mut children: Vec<_> = children.take(left).filter_map(Result::ok).collect();
            children.sort_by_key(std::fs::DirEntry::file_name);
            for child in children {
                left = left.saturating_sub(1);
                if let Ok(bundle) = package(&child.path()) {
                    named.entry(bundle.name.clone()).or_insert(bundle);
                }
            }
        }
    }
    let (name, _, includes, specialist) = super::specialist_files::parse(text)?;
    let root = target
        .parent()
        .ok_or("Missing specialist directory")?
        .to_owned();
    let proposed = Bundle {
        root,
        source: target.to_owned(),
        text: Some(text.into()),
        name: name.clone(),
        includes,
        specialist: Some(specialist),
    };
    named.insert(name, proposed.clone());
    resolve(
        &proposed,
        &named,
        &mut Vec::new(),
        &mut Resolved::default(),
        &mut 256,
    )
    .map(|_| ())
}

fn package(root: &Path) -> Result<Bundle, String> {
    let path = root.join(super::specialist_files::NAME);
    if path.try_exists().map_err(|e| e.to_string())? {
        let (name, _, includes, specialist, text) = super::specialist_files::read(&path)?;
        if root
            .file_name()
            .is_none_or(|folder| folder != name.as_str())
        {
            return Err("Specialist name must match its folder name".into());
        }
        return Ok(Bundle {
            root: root.canonicalize().map_err(|e| e.to_string())?,
            source: path,
            text: Some(text),
            name,
            includes,
            specialist: Some(specialist),
        });
    }
    manifest(&root.join("skillset.toml"))
}

fn manifest(path: &Path) -> Result<Bundle, String> {
    let file =
        crate::regular::open(path, crate::regular::Links::Refuse).map_err(|err| err.to_string())?;
    let mut text = String::new();
    file.take(MOST_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|err| err.to_string())?;
    if text.len() as u64 > MOST_FILE {
        return Err("manifest is larger than 64 KiB".into());
    }
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|err| err.to_string())?;
    for (key, _) in document.iter() {
        if ![
            "format",
            "name",
            "description",
            "version",
            "includes",
            "specialist",
        ]
        .contains(&key)
        {
            return Err(format!("unsupported skillset field {key}"));
        }
    }
    if document.get("format").and_then(toml_edit::Item::as_integer) != Some(1) {
        return Err("format must be 1".into());
    }
    let name = document
        .get("name")
        .and_then(toml_edit::Item::as_str)
        .ok_or("missing name")?;
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
    {
        return Err("name must use 1–64 lowercase letters, numbers, hyphens or underscores".into());
    }
    let description = document
        .get("description")
        .and_then(toml_edit::Item::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or("missing description")?;
    if document
        .get("version")
        .is_some_and(|v| v.as_str().is_none_or(|s| s.trim().is_empty()))
    {
        return Err("version must be a nonempty string".into());
    }
    let includes = match document.get("includes") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("includes must be an array")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("includes entries must be strings")
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let specialist = document
        .get("specialist")
        .map(|item| parse_specialist(name, description, &item_json(item)))
        .transpose()?;
    Ok(Bundle {
        source: path.to_owned(),
        text: None,
        root: path
            .parent()
            .ok_or("missing bundle directory")?
            .canonicalize()
            .map_err(|e| e.to_string())?,
        name: name.into(),
        includes,
        specialist,
    })
}

fn item_json(item: &toml_edit::Item) -> serde_json::Value {
    use serde_json::{Value, json};
    fn value(v: &toml_edit::Value) -> Value {
        if let Some(s) = v.as_str() {
            json!(s)
        } else if let Some(b) = v.as_bool() {
            json!(b)
        } else if let Some(n) = v.as_integer() {
            json!(n)
        } else if let Some(n) = v.as_float() {
            json!(n)
        } else if let Some(a) = v.as_array() {
            Value::Array(a.iter().map(value).collect())
        } else if let Some(t) = v.as_inline_table() {
            Value::Object(t.iter().map(|(k, v)| (k.into(), value(v))).collect())
        } else {
            Value::Null
        }
    }
    if let Some(table) = item.as_table() {
        Value::Object(
            table
                .iter()
                .map(|(k, v)| (k.into(), item_json(v)))
                .collect(),
        )
    } else {
        item.as_value().map_or(Value::Null, value)
    }
}

pub(super) fn parse_specialist(
    name: &str,
    description: &str,
    value: &serde_json::Value,
) -> Result<Specialist, String> {
    let fields = value.as_object().ok_or("specialist must be a table")?;
    for key in fields.keys() {
        if ![
            "instructions",
            "agent",
            "model",
            "effort",
            "when_to_use",
            "user_invocable",
            "agent_invocable",
            "delegates_to",
            "skills",
            "skillsets",
            "mcp_servers",
            "limits",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("unsupported specialist field {key}"));
        }
    }
    let instructions = value
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .ok_or("specialist.instructions is required")?;
    let by_agent = |key: &str| -> Result<Vec<(String, String)>, String> {
        match value.get(key) {
            None => Ok(Vec::new()),
            Some(serde_json::Value::String(s)) => Ok(vec![("*".into(), s.clone())]),
            Some(serde_json::Value::Object(map)) => map
                .iter()
                .map(|(k, v)| {
                    v.as_str()
                        .map(|v| (k.clone(), v.into()))
                        .ok_or_else(|| format!("{key} values must be strings"))
                })
                .collect(),
            _ => Err(format!("{key} must be a string or table by provider")),
        }
    };
    let models = by_agent("model")?;
    let inherit_model = models.iter().any(|(_, model)| model == "inherit");
    let agent = value
        .get("agent")
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("agent must be a string")
        })
        .transpose()?;
    Ok(Specialist {
        agent,
        inherit_model,
        name: name.into(),
        description: description.into(),
        instructions: instructions.into(),
        models: models
            .into_iter()
            .filter(|(_, model)| model != "inherit")
            .collect(),
        efforts: by_agent("effort")?,
        limits: specialists::access(value.get("limits"))?.unwrap_or_default(),
        extra: Extras::parse(value)?,
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::skill;
    use super::super::{Catalogue, Roots};
    use super::*;

    fn bundle(root: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("skillset.toml"), format!(
            "format = 1\nname = {name:?}\ndescription = \"A test bundle\"\nversion = \"0.1.0\"\n"
        )).unwrap();
        skill(&root.join("skills/check"), "check", "", body);
    }

    #[test]
    fn bundles_share_commands_and_project_precedence_without_merging_versions() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = home.join("project");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let personal = home.join(".agents/skillsets/release");
        bundle(&personal, "release", "personal");
        skill(
            &personal.join("skills/extra"),
            "extra",
            "",
            "personal extra",
        );
        bundle(
            &repo.join(".agents/skillsets/release"),
            "release",
            "project $ARGUMENTS",
        );
        let roots = Roots {
            cwd: repo.clone(),
            home,
            config: None,
        };
        let catalogue = Catalogue::discover(&roots);
        assert_eq!(catalogue.commands().len(), 1);
        assert!(catalogue.infos()[0].source.contains("skillset release"));
        assert!(
            catalogue
                .explicit("/check argument")
                .unwrap()
                .unwrap()
                .contains("project argument")
        );
        assert!(catalogue.load("extra", "", false).is_err());
        skill(
            &repo.join(".agents/skills/check"),
            "check",
            "",
            "local override",
        );
        assert!(
            Catalogue::discover(&roots)
                .load("check", "", false)
                .unwrap()
                .contains("local override")
        );
    }

    #[test]
    fn unknown_components_and_future_formats_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        bundle(dir.path(), "release", "body");
        let path = dir.path().join("skillset.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{original}\nhooks = []\n")).unwrap();
        assert!(manifest(&path).unwrap_err().contains("hooks"));
        std::fs::write(&path, original.replace("format = 1", "format = 2")).unwrap();
        assert!(manifest(&path).unwrap_err().contains("format"));
    }

    fn package(root: &Path, name: &str, extra: &str) -> Bundle {
        std::fs::create_dir_all(root).unwrap();
        let path = root.join("skillset.toml");
        std::fs::write(
            &path,
            format!("format=1\nname={name:?}\ndescription=\"A bundle\"\n{extra}"),
        )
        .unwrap();
        manifest(&path).unwrap()
    }

    #[test]
    fn nested_roles_keep_their_instructions_and_skills_out_of_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let bundles = root.join(".agents/skillsets");
        let parent = bundles.join("team");
        package(
            &parent,
            "team",
            "includes=[\"./helpers/reviewer\", \"./shared\"]",
        );
        package(&parent.join("shared"), "shared", "");
        skill(&parent.join("shared/skills/common"), "common", "", "COMMON");
        package(
            &parent.join("helpers/reviewer"),
            "reviewer",
            r#"
includes=["./subroles/auditor", "./formatting"]
[specialist]
instructions="REVIEW ROLE"
skills=[]
"#,
        );
        package(
            &parent.join("helpers/reviewer/formatting"),
            "formatting",
            "",
        );
        skill(
            &parent.join("helpers/reviewer/formatting/skills/format"),
            "format",
            "",
            "FORMAT",
        );
        package(
            &parent.join("helpers/reviewer/subroles/auditor"),
            "auditor",
            r#"
[specialist]
instructions="AUDIT ROLE"
skills=[]
"#,
        );
        skill(
            &parent.join("helpers/reviewer/skills/review-code"),
            "review-code",
            "",
            "REVIEW",
        );
        skill(
            &parent.join("helpers/reviewer/subroles/auditor/skills/audit"),
            "audit",
            "",
            "AUDIT",
        );
        let catalogue = Catalogue::discover(&Roots {
            cwd: root,
            home: dir.path().join("home"),
            config: None,
        });
        assert!(catalogue.load("common", "", false).is_ok());
        assert!(catalogue.load("review-code", "", false).is_err());
        assert!(catalogue.load("audit", "", false).is_err());
        let reviewer = &catalogue.specialists["reviewer"];
        assert_eq!(reviewer.instructions, "REVIEW ROLE");
        let scoped = catalogue.scoped(reviewer).unwrap();
        assert!(scoped.load("review-code", "", false).is_ok());
        assert!(scoped.load("format", "", false).is_ok());
        assert!(scoped.load("audit", "", false).is_err());
        assert!(scoped.load("common", "", false).is_err());
        assert!(scoped.explicit("/common").unwrap().is_err());
        assert_eq!(scoped.specialists["auditor"].instructions, "AUDIT ROLE");
        // Scope selection never reads disk on the daemon loop.
        std::fs::remove_dir_all(&parent).unwrap();
        assert!(
            catalogue
                .scoped(reviewer)
                .unwrap()
                .load("review-code", "", false)
                .is_ok()
        );
    }

    #[test]
    fn graphs_reject_cycles_conflicts_and_escaping_paths_and_deduplicate_diamonds() {
        let dir = tempfile::tempdir().unwrap();
        let a = package(&dir.path().join("a"), "a", "includes=[\"b\",\"c\"]");
        let b = package(&dir.path().join("b"), "b", "includes=[\"d\"]");
        let c = package(&dir.path().join("c"), "c", "includes=[\"d\"]");
        let d = package(&dir.path().join("d"), "d", "");
        let mut named: BTreeMap<_, _> = [&a, &b, &c, &d]
            .into_iter()
            .map(|b| (b.name.clone(), b.clone()))
            .collect();
        let mut out = Resolved::default();
        let dirs = resolve(&a, &named, &mut Vec::new(), &mut out, &mut 256).unwrap();
        assert_eq!(dirs.len(), 4);
        assert_eq!(out.sets.len(), 4);
        named.get_mut("d").unwrap().includes.push("a".into());
        assert!(
            resolve(
                &a,
                &named,
                &mut Vec::new(),
                &mut Resolved::default(),
                &mut 256
            )
            .unwrap_err()
            .contains("cycle")
        );
        let bad = package(&dir.path().join("bad"), "bad", "includes=[\"../a\"]");
        assert!(
            resolve(
                &bad,
                &named,
                &mut Vec::new(),
                &mut Resolved::default(),
                &mut 256
            )
            .unwrap_err()
            .contains("stay inside")
        );
        let conflict = package(
            &dir.path().join("conflict"),
            "conflict",
            "includes=[\"./one\",\"./two\"]",
        );
        package(&conflict.root.join("one"), "same", "");
        package(&conflict.root.join("two"), "same", "");
        assert!(
            resolve(
                &conflict,
                &named,
                &mut Vec::new(),
                &mut Resolved::default(),
                &mut 256
            )
            .unwrap_err()
            .contains("conflicting bundle name")
        );
    }
}
