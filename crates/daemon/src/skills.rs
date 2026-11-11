//! One catalogue of Agent Skills for every hosted agent. Disk work belongs
//! on the skills worker, not the daemon's event loop. Only descriptions
//! are advertised; bodies are supplied on explicit or model invocation.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clackatoo_protocol::{AgentCommand, Skill};
use serde_yaml_ng::Value;

const MOST_FILE: u64 = 64 * 1024;
const MOST_SKILLS: usize = 256;
const MOST_ENTRIES: usize = 2048;
const MOST_CATALOGUE: usize = 24 * 1024;

mod skillsets;
pub(crate) mod specialist_edit;
pub(crate) mod specialist_files;

/// The roots that determine a chat's catalogue, in precedence order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Roots {
    pub(crate) cwd: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) config: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct Entry {
    info: Skill,
    body: String,
    hint: Option<String>,
}

/// A snapshot, refreshed when a chat opens, Settings opens, or a skill is
/// loaded through MCP. The same snapshot supplies commands and descriptions.
#[derive(Clone, Debug, Default)]
pub(crate) struct Catalogue {
    entries: BTreeMap<String, Arc<Entry>>,
    invalid: Vec<Skill>,
    pub(crate) specialists: BTreeMap<String, crate::specialists::Specialist>,
    pub(crate) specialist_sources: BTreeMap<String, PathBuf>,
    pub(crate) specialist_texts: BTreeMap<PathBuf, String>,
    pub(crate) specialist_includes: BTreeMap<String, Vec<String>>,
    sets: BTreeMap<String, Option<Vec<PathBuf>>>,
    directories: BTreeMap<PathBuf, Arc<Directory>>,
}

#[derive(Clone, Debug, Default)]
struct Directory {
    entries: BTreeMap<String, Arc<Entry>>,
    invalid: Vec<Skill>,
}

impl Catalogue {
    pub(crate) fn scoped(
        &self,
        specialist: &crate::specialists::Specialist,
    ) -> Result<Self, String> {
        let mut scoped = Self {
            specialists: self.specialists.clone(),
            specialist_sources: self.specialist_sources.clone(),
            specialist_texts: self.specialist_texts.clone(),
            specialist_includes: self.specialist_includes.clone(),
            sets: self.sets.clone(),
            directories: self.directories.clone(),
            ..Self::default()
        };
        let source = format!("Specialist {}", specialist.name);
        for directory in &specialist.extra.directories {
            scoped.include_directory(directory, &source)?;
        }
        if let Some(sets) = &specialist.extra.skillsets {
            for name in sets {
                let directories = self.sets.get(name).ok_or_else(|| {
                    format!(
                        "specialist {} needs missing skillset {name}",
                        specialist.name
                    )
                })?;
                if let Some(directories) = directories {
                    for directory in directories {
                        scoped
                            .include_directory(directory, &format!("{source} · skillset {name}"))?;
                    }
                }
            }
        }
        if let Some(skills) = &specialist.extra.skills {
            for name in skills {
                let entry = self.entries.get(name).ok_or_else(|| {
                    format!("specialist {} needs missing skill {name}", specialist.name)
                })?;
                if !scoped.invalid.iter().any(|bad| bad.name == *name) {
                    scoped
                        .entries
                        .entry(name.clone())
                        .or_insert_with(|| entry.clone());
                }
            }
        } else if specialist.extra.skillsets.is_none() {
            for (name, entry) in &self.entries {
                if !scoped.invalid.iter().any(|bad| bad.name == *name) {
                    scoped
                        .entries
                        .entry(name.clone())
                        .or_insert_with(|| entry.clone());
                }
            }
            for info in &self.invalid {
                if !scoped.entries.contains_key(&info.name)
                    && !scoped.invalid.iter().any(|bad| bad.name == info.name)
                {
                    scoped.invalid.push(info.clone());
                }
            }
        }
        for (name, entry) in &self.entries {
            if !scoped.entries.contains_key(name)
                && !scoped.invalid.iter().any(|bad| bad.name == *name)
            {
                let mut info = entry.info.clone();
                info.problem = Some(format!("not enabled for specialist {}", specialist.name));
                info.implicit = false;
                info.user_invocable = false;
                scoped.invalid.push(info);
            }
        }
        Ok(scoped)
    }

    pub(crate) fn discover(roots: &Roots) -> Self {
        let mut found = Self::default();
        let mut folders = Vec::new();
        // Closest project wins, stopping at the repository boundary.
        for directory in roots.cwd.ancestors() {
            if directory == roots.home {
                break;
            }
            folders.push((directory.join(".agents/skills"), "Project", false));
            folders.push((
                directory.join(".claude/skills"),
                "Project · Claude folder",
                false,
            ));
            folders.push((directory.join(".agents/specialists"), "Project", true));
            folders.push((directory.join(".agents/skillsets"), "Project", true));
            if directory.join(".git").exists() || directory == roots.home {
                break;
            }
        }
        if let Some(config) = &roots.config {
            folders.push((config.join("skills"), "Personal · clackatoo", false));
            folders.push((config.join("specialists"), "Personal · clackatoo", true));
            folders.push((config.join("skillsets"), "Personal · clackatoo", true));
        }
        folders.extend([
            (roots.home.join(".agents/skills"), "Personal", false),
            (roots.home.join(".agents/specialists"), "Personal", true),
            (roots.home.join(".agents/skillsets"), "Personal", true),
            (
                roots.home.join(".claude/skills"),
                "Personal · Claude folder",
                false,
            ),
            (
                roots.home.join(".codex/skills"),
                "Personal · Codex folder",
                false,
            ),
        ]);
        let mut seen = HashSet::new();
        let packages = skillsets::discover(&folders);
        found.specialists = packages.specialists.clone();
        found.specialist_sources = packages.sources.clone();
        found.specialist_texts = packages.texts.clone();
        found.specialist_includes = packages.includes.clone();
        found.sets = packages.sets.clone();
        let mut budget = MOST_ENTRIES;
        for (folder, source, bundles) in folders {
            if bundles {
                for (directory, label) in packages.directories.get(&folder).into_iter().flatten() {
                    found.walk(directory, label, 0, &mut seen, &mut budget);
                }
            } else {
                found.walk(&folder, source, 0, &mut seen, &mut budget);
            }
        }
        // Materialize all selectable package directories on this worker.
        // Scoping a chat on the daemon loop only selects cached entries.
        let paths: std::collections::BTreeSet<_> = packages
            .sets
            .values()
            .flatten()
            .flatten()
            .chain(
                packages
                    .specialists
                    .values()
                    .flat_map(|s| &s.extra.directories),
            )
            .cloned()
            .collect();
        let mut remaining = MOST_ENTRIES;
        for path in paths.into_iter().take(MOST_ENTRIES) {
            let mut directory = Self::default();
            directory.walk(&path, "Skillset", 0, &mut HashSet::new(), &mut remaining);
            found.directories.insert(
                path,
                Arc::new(Directory {
                    entries: directory.entries,
                    invalid: directory.invalid,
                }),
            );
        }
        found
    }

    fn include_directory(&mut self, path: &Path, source: &str) -> Result<(), String> {
        let directory = self
            .directories
            .get(path)
            .ok_or_else(|| format!("missing specialist skill directory {}", path.display()))?;
        for (name, entry) in &directory.entries {
            if !self.invalid.iter().any(|bad| bad.name == *name) {
                let mut entry = entry.as_ref().clone();
                entry.info.source = source.into();
                self.entries
                    .entry(name.clone())
                    .or_insert_with(|| Arc::new(entry));
            }
        }
        for info in &directory.invalid {
            if !self.entries.contains_key(&info.name)
                && !self.invalid.iter().any(|bad| bad.name == info.name)
            {
                let mut info = info.clone();
                info.source = source.into();
                self.invalid.push(info);
            }
        }
        Ok(())
    }

    fn walk(
        &mut self,
        folder: &Path,
        source: &str,
        depth: usize,
        seen: &mut HashSet<PathBuf>,
        budget: &mut usize,
    ) {
        if depth > 4 || self.entries.len() + self.invalid.len() >= MOST_SKILLS {
            return;
        }
        let Ok(real) = folder.canonicalize() else {
            return;
        };
        if !seen.insert(real) {
            return;
        }
        // Compatibility skill roots can also contain native plugin bundles.
        // Their nested skills must not bypass the native-plugin opt-in.
        if folder.join(".claude-plugin").is_dir() || folder.join(".codex-plugin").is_dir() {
            return;
        }
        let path = folder.join("SKILL.md");
        if path.symlink_metadata().is_ok() {
            let path = path.canonicalize().unwrap_or(path);
            match parse(&path, source) {
                Ok(entry) => {
                    if self
                        .invalid
                        .iter()
                        .any(|invalid| invalid.name == entry.info.name)
                    {
                        return;
                    }
                    // A less specific copy never silently changes the chosen one.
                    self.entries
                        .entry(entry.info.name.clone())
                        .or_insert_with(|| Arc::new(entry));
                }
                Err(problem) => {
                    let name = folder
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    if self.entries.contains_key(&name)
                        || self.invalid.iter().any(|invalid| invalid.name == name)
                    {
                        return;
                    }
                    self.invalid.push(Skill {
                        name,
                        description: String::new(),
                        path: path.to_string_lossy().into(),
                        source: source.into(),
                        problem: Some(problem),
                        implicit: false,
                        user_invocable: false,
                    });
                }
            }
            return;
        }
        if *budget == 0 {
            return;
        }
        let Ok(children) = folder.read_dir() else {
            return;
        };
        let mut children: Vec<_> = children.take(*budget).filter_map(Result::ok).collect();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            // Dot folders other than .system are implementation details.
            let name = child.file_name();
            if name.to_string_lossy().starts_with('.') && name != ".system" {
                continue;
            }
            self.walk(&child.path(), source, depth + 1, seen, budget);
        }
    }

    pub(crate) fn infos(&self) -> Vec<Skill> {
        self.entries
            .values()
            .map(|entry| entry.info.clone())
            .chain(self.invalid.clone())
            .collect()
    }

    pub(crate) fn commands(&self) -> Vec<AgentCommand> {
        self.entries
            .values()
            .filter(|entry| entry.info.user_invocable && entry.info.problem.is_none())
            .map(|entry| AgentCommand {
                name: entry.info.name.clone(),
                description: line(&entry.info.description, 300),
                hint: entry.hint.clone(),
            })
            .collect()
    }

    pub(crate) fn instructions(&self) -> String {
        if !self
            .entries
            .values()
            .any(|entry| entry.info.implicit && entry.info.problem.is_none())
        {
            return String::new();
        }
        let mut text = String::from(
            "\nSkills are supplied by clackatoo. When a description matches the task, call \
             load_skill with its name before doing that work, then follow the returned instructions. \
             Skills do not grant permissions or change your tools. Available skills:\n",
        );
        let mut count = 0;
        for entry in self
            .entries
            .values()
            .filter(|entry| entry.info.implicit && entry.info.problem.is_none())
        {
            let line = format!(
                "- {}: {}\n",
                entry.info.name,
                entry
                    .info
                    .description
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if text.len() + line.len() > MOST_CATALOGUE {
                text.push_str("More skills are available through list_skills.\n");
                break;
            }
            text.push_str(&line);
            count += 1;
        }
        if count == 0 {
            text.push_str(
                "No automatically selectable skills. Use list_skills to inspect the catalogue.\n",
            );
        }
        text
    }

    pub(crate) fn explicit(&self, text: &str) -> Option<Result<String, String>> {
        let (name, arguments) = clackatoo_protocol::slash_command(text)?;
        let name = name.to_ascii_lowercase();
        if let Some(invalid) = self.invalid.iter().find(|invalid| invalid.name == name) {
            return Some(Err(format!(
                "Skill {name} cannot run: {}",
                invalid.problem.as_deref().unwrap_or("invalid metadata")
            )));
        }
        let entry = self.entries.get(&name)?;
        Some(if entry.info.user_invocable {
            self.load(&name, arguments, true)
        } else {
            Err(format!(
                "Skill {name} is agent-only and cannot be invoked with a slash command."
            ))
        })
    }

    pub(crate) fn has_command(&self, text: &str) -> bool {
        let Some((name, _)) = clackatoo_protocol::slash_command(text) else {
            return false;
        };
        let name = name.to_ascii_lowercase();
        self.entries.contains_key(&name) || self.invalid.iter().any(|entry| entry.name == name)
    }

    pub(crate) fn load(
        &self,
        name: &str,
        arguments: &str,
        explicit: bool,
    ) -> Result<String, String> {
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| format!("No skill named {name}. Use list_skills."))?;
        if let Some(problem) = &entry.info.problem {
            return Err(format!("Skill {name} cannot run: {problem}"));
        }
        if !explicit && !entry.info.implicit {
            return Err(format!(
                "Skill {name} is user-only: the user invokes it with /{name}."
            ));
        }
        let directory = Path::new(&entry.info.path)
            .parent()
            .unwrap_or(Path::new("/"));
        let body = entry
            .body
            .replace("${CLAUDE_SKILL_DIR}", &directory.to_string_lossy())
            .replace("$ARGUMENTS", arguments);
        let mut text = format!(
            "Use the {name} skill for this request.\nSource: {}\nSkill directory: {}\n\
             Resolve supporting files and scripts relative to this directory. \
             Existing tool permissions still apply.\n\n{body}",
            entry.info.path,
            directory.display(),
        );
        if !arguments.is_empty() {
            let _ = write!(text, "\n\nUser arguments: {arguments}");
        }
        Ok(text)
    }
}

fn parse(path: &Path, source: &str) -> Result<Entry, String> {
    let file =
        crate::regular::open(path, crate::regular::Links::Refuse).map_err(|err| err.to_string())?;
    let mut text = String::new();
    file.take(MOST_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|err| err.to_string())?;
    if text.len() as u64 > MOST_FILE {
        return Err("SKILL.md is larger than 64 KiB".into());
    }
    let text = text.replace("\r\n", "\n");
    let rest = text
        .strip_prefix("---\n")
        .ok_or("Missing YAML frontmatter")?;
    let (metadata, body) = rest
        .split_once("\n---\n")
        .ok_or("Unclosed YAML frontmatter")?;
    let metadata: Value =
        serde_yaml_ng::from_str(metadata).map_err(|err| format!("Invalid YAML: {err}"))?;
    let string = |key: &str| {
        metadata[key]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let name = string("name").ok_or("Missing name")?;
    if name.len() > 64
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_:".contains(&byte)
        })
    {
        return Err(
            "Skill names use at most 64 lowercase letters, numbers, hyphens, underscores or colons"
                .into(),
        );
    }
    let description = string("description").ok_or("Missing description")?;
    let mut problems = Vec::new();
    if clackatoo_protocol::INTERFACE_COMMANDS.contains(&name)
        || clackatoo_protocol::KNOWN_COMMANDS.contains(&name)
    {
        problems.push("the name is reserved by a built-in command".to_owned());
    }
    for key in [
        "context",
        "agent",
        "model",
        "effort",
        "hooks",
        "allowed-tools",
        "disallowed-tools",
        "background",
        "shell",
        "arguments",
    ] {
        if !metadata[key].is_null() {
            problems.push(format!(
                "{key} requires runtime-specific execution, which shared skills do not support"
            ));
        }
    }
    if body.contains("!`") {
        problems.push("dynamic shell expansion is not supported".into());
    }
    if body.contains("$ARGUMENTS[") {
        problems.push("positional argument expansion is not supported; use $ARGUMENTS".into());
    }
    let boolean = |key: &str, default| -> Result<bool, String> {
        match &metadata[key] {
            Value::Null => Ok(default),
            Value::Bool(value) => Ok(*value),
            _ => Err(format!("{key} must be true or false")),
        }
    };
    let implicit_policy = implicit_policy(path)?;
    Ok(Entry {
        info: Skill {
            name: name.into(),
            description: line(description, 1536),
            path: path.to_string_lossy().into(),
            source: source.into(),
            problem: (!problems.is_empty()).then(|| problems.join("; ")),
            implicit: implicit_policy && !boolean("disable-model-invocation", false)?,
            user_invocable: boolean("user-invocable", true)?,
        },
        body: body.trim().into(),
        hint: string("argument-hint").map(|hint| line(hint, 100)),
    })
}

fn implicit_policy(path: &Path) -> Result<bool, String> {
    let policy_path = path
        .parent()
        .unwrap_or(Path::new("/"))
        .join("agents/openai.yaml");
    if policy_path.exists() {
        let policy = crate::regular::read_small(&policy_path, MOST_FILE)
            .ok_or("Could not read agents/openai.yaml (limit 64 KiB)")?;
        let policy: Value = serde_yaml_ng::from_str(&policy)
            .map_err(|err| format!("Invalid agents/openai.yaml: {err}"))?;
        Ok(match &policy["policy"]["allow_implicit_invocation"] {
            Value::Null => true,
            Value::Bool(allow) => *allow,
            _ => return Err("policy.allow_implicit_invocation must be true or false".into()),
        })
    } else {
        Ok(true)
    }
}

pub(crate) fn line(text: &str, most: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::server::unhidden(&text).chars().take(most).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn skill(root: &Path, name: &str, extra: &str, body: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("SKILL.md"), format!("---\nname: {name}\ndescription: >\n  A useful procedure\n  for this project.\n{extra}---\n{body}")).unwrap();
    }

    #[test]
    fn compatibility_folders_do_not_import_native_plugin_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join(".claude/skills/native-bundle");
        std::fs::create_dir_all(native.join(".claude-plugin")).unwrap();
        skill(&native.join("skills/hidden"), "hidden", "", "native skill");
        skill(
            &dir.path().join(".agents/skills/shared"),
            "shared",
            "",
            "shared skill",
        );
        let catalogue = Catalogue::discover(&Roots {
            cwd: dir.path().to_owned(),
            home: dir.path().to_owned(),
            config: None,
        });
        assert!(catalogue.load("hidden", "", false).is_err());
        assert!(catalogue.load("shared", "", false).is_ok());
    }

    #[test]
    fn closest_project_wins_and_catalogue_is_lazy() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        skill(
            &home.join(".agents/skills/build"),
            "build",
            "",
            "PERSONAL BODY",
        );
        skill(
            &repo.join(".agents/skills/build"),
            "build",
            "",
            "PROJECT BODY $ARGUMENTS",
        );
        let roots = Roots {
            cwd: repo,
            home,
            config: None,
        };
        let found = Catalogue::discover(&roots);
        assert_eq!(found.infos().len(), 1);
        assert!(!found.instructions().contains("PROJECT BODY"));
        let loaded = found.load("build", "release", false).unwrap();
        assert!(loaded.contains("PROJECT BODY release"));
        assert!(!loaded.contains("PERSONAL BODY"));
        assert!(
            found
                .explicit("/build release")
                .unwrap()
                .unwrap()
                .contains("PROJECT BODY release")
        );
    }

    #[test]
    fn invocation_controls_and_unsupported_execution_are_respected() {
        let dir = tempfile::tempdir().unwrap();
        skill(
            &dir.path().join(".agents/skills/manual"),
            "manual",
            "disable-model-invocation: true\n",
            "manual body",
        );
        skill(
            &dir.path().join(".agents/skills/forked"),
            "forked",
            "context: fork\n",
            "forked body",
        );
        skill(
            &dir.path().join(".agents/skills/automatic"),
            "automatic",
            "user-invocable: false\n",
            "automatic body",
        );
        let roots = Roots {
            cwd: dir.path().into(),
            home: dir.path().into(),
            config: None,
        };
        let found = Catalogue::discover(&roots);
        assert!(!found.instructions().contains("manual:"));
        assert!(found.load("manual", "", false).is_err());
        assert!(found.explicit("/manual").unwrap().is_ok());
        assert!(found.load("forked", "", true).is_err());
        assert!(found.explicit("/automatic").unwrap().is_err());
        assert!(found.load("automatic", "", false).is_ok());
        assert_eq!(found.commands().len(), 1);
    }

    #[test]
    fn codex_invocation_policy_is_preserved_and_broken_overrides_do_not_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let folder = repo.join(".agents/skills/manual");
        skill(&folder, "manual", "", "manual body");
        std::fs::create_dir_all(folder.join("agents")).unwrap();
        std::fs::write(
            folder.join("agents/openai.yaml"),
            "policy:\n  allow_implicit_invocation: false\n",
        )
        .unwrap();
        skill(
            &home.join(".agents/skills/broken"),
            "broken",
            "",
            "wrong fallback body",
        );
        let broken = repo.join(".agents/skills/broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(
            broken.join("SKILL.md"),
            "---\nname: broken\n---\nMissing description",
        )
        .unwrap();
        let found = Catalogue::discover(&Roots {
            cwd: repo,
            home,
            config: None,
        });
        assert!(found.load("manual", "", false).is_err());
        assert!(found.explicit("/MANUAL").unwrap().is_ok());
        assert!(found.explicit("/broken").unwrap().is_err());
        assert!(!found.instructions().contains("wrong fallback"));
    }
}
