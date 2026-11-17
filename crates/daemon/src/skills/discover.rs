//! Finding what a chat's catalogue holds: which folders are read, in
//! what order, how far into each, and what a `SKILL.md` must say.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clackatoo_protocol::Skill;
use serde_yaml_ng::Value;

use super::reading::Tree;
use super::{
    Catalogue, Entry, Found, HashSet, MOST_DESCRIPTION, MOST_ENTRIES, MOST_SKILLS, Roots, Unloaded,
    Why, brief, front_matter, line, mix, skillsets,
};

/// The file a skill's folder holds.
const SKILL: &str = "SKILL.md";

/// How many folders deep a skill's folder may be in clackatoo's own skill
/// folders, so skills can be grouped.
const DEEPEST: usize = 4;

/// What a folder that's searched holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Holds {
    /// Skills, each in a folder of its own, which may be grouped in
    /// folders, [`DEEPEST`] deep at most.
    Skills,
    /// Another product's skills, read as that product documents them:
    /// `NAME/SKILL.md` and no deeper. What else the product keeps there,
    /// such as the skills it syncs or bundles for itself, isn't offered.
    OthersSkills,
    /// Specialists and skillsets, each in a folder of its own.
    Packages,
}

/// A folder that's searched.
#[derive(Clone, Debug)]
pub(super) struct Folder {
    pub(super) tree: Tree,
    /// Where it is, below `tree`.
    pub(super) below: PathBuf,
    /// Where what's found in it is said to come from.
    pub(super) label: &'static str,
    pub(super) holds: Holds,
}

/// The project's folders that are searched, nearest first, and within a
/// folder, clackatoo's own before another product's, and skills on their
/// own before those in bundles.
fn project_folders(roots: &Roots) -> Vec<Folder> {
    let top = roots.project_top();
    let tree = Tree::project(top);
    let mut folders = Vec::new();
    for folder in roots.project_folders() {
        let below = folder.strip_prefix(top).unwrap_or(Path::new(""));
        for (name, label, holds) in [
            (".agents/skills", "Project", Holds::Skills),
            (
                ".claude/skills",
                "Project · Claude folder",
                Holds::OthersSkills,
            ),
            (".agents/specialists", "Project", Holds::Packages),
            (".agents/skillsets", "Project", Holds::Packages),
        ] {
            folders.push(Folder {
                tree: tree.clone(),
                below: below.join(name),
                label,
                holds,
            });
        }
    }
    folders
}

/// The user's own folders that are searched: clackatoo's config
/// directory, then `~/.agents`, then other products'.
fn personal_folders(roots: &Roots) -> Vec<Folder> {
    let mut folders = Vec::new();
    if let Some(config) = &roots.config {
        let tree = Tree::personal(config);
        for (name, holds) in [
            ("skills", Holds::Skills),
            ("specialists", Holds::Packages),
            ("skillsets", Holds::Packages),
        ] {
            folders.push(Folder {
                tree: tree.clone(),
                below: name.into(),
                label: "Personal · clackatoo",
                holds,
            });
        }
    }
    let tree = Tree::personal(&roots.home);
    for (name, label, holds) in [
        (".agents/skills", "Personal", Holds::Skills),
        (".agents/specialists", "Personal", Holds::Packages),
        (".agents/skillsets", "Personal", Holds::Packages),
        (
            ".claude/skills",
            "Personal · Claude folder",
            Holds::OthersSkills,
        ),
        (
            ".codex/skills",
            "Personal · Codex folder",
            Holds::OthersSkills,
        ),
    ] {
        folders.push(Folder {
            tree: tree.clone(),
            below: name.into(),
            label,
            holds,
        });
    }
    folders
}

/// Whether the folder at `path` is the daemon's user's: one that isn't,
/// such as a shared scratch directory, is never read as a project.
fn owned_by_user(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|folder| folder.uid() == rustix::process::geteuid().as_raw())
}

/// What a walk through folders of skills has used up.
struct Walk {
    /// The folders looked into, so one reached twice is read once.
    seen: HashSet<PathBuf>,
    /// How many more folders may be looked into.
    budget: usize,
}

impl Walk {
    fn new() -> Self {
        Self {
            seen: HashSet::new(),
            budget: MOST_ENTRIES,
        }
    }
}

impl Catalogue {
    /// Reads the catalogue of a chat with these `roots`, from disk: for a
    /// worker, never the event loop.
    pub(crate) fn discover(roots: &Roots) -> Self {
        let mut found = Self::default();
        let mut folders = Vec::new();
        let project = project_folders(roots);
        if !owned_by_user(roots.project_top()) {
            // Someone else's folder: not even looked into.
            if !project.is_empty() {
                log::debug!(
                    "skills: {} isn't this user's, so it's not read as a project",
                    roots.project_top().display()
                );
            }
        } else if roots.trusted {
            folders.extend(project);
        } else {
            found.list_untrusted(&project);
        }
        folders.extend(personal_folders(roots));

        let packages = skillsets::discover(&folders);
        found.specialists.clone_from(&packages.specialists);
        found.specialist_sources.clone_from(&packages.sources);
        found.specialist_texts.clone_from(&packages.texts);
        found.specialist_includes.clone_from(&packages.includes);
        found.sets.clone_from(&packages.sets);
        for (name, unloaded) in &packages.unloaded {
            found
                .unloaded
                .entry(name.clone())
                .or_insert_with(|| unloaded.clone());
        }
        found.discover_mcp(roots, &packages);

        let mut walk = Walk::new();
        let mut personal = packages.personal;
        for (at, folder) in folders.iter().enumerate() {
            let stamp = folder.tree.is_personal().then_some(&mut personal);
            match folder.holds {
                Holds::Skills => {
                    let place = Place::of(folder, DEEPEST);
                    found
                        .skills
                        .walk(&place, &folder.below, 0, &mut walk, stamp);
                }
                Holds::OthersSkills => {
                    let place = Place::of(folder, 1);
                    found
                        .skills
                        .walk(&place, &folder.below, 0, &mut walk, stamp);
                }
                Holds::Packages => {
                    let mut stamp = stamp;
                    for (directory, label) in packages.directories.get(&at).into_iter().flatten() {
                        let Some((tree, below)) = packages.places.get(directory) else {
                            continue;
                        };
                        let place = Place {
                            tree,
                            label,
                            deepest: DEEPEST,
                        };
                        let stamp = stamp.as_deref_mut();
                        found.skills.walk(&place, below, 0, &mut walk, stamp);
                    }
                }
            }
        }
        found.skills.switch_off(&roots.disabled);
        found.personal = personal;

        // Read every folder of skills a specialist or a skillset can be
        // given here, on the worker: scoping a chat on the daemon's loop
        // then only chooses among them.
        let mut walk = Walk::new();
        for path in packages.selectable() {
            let Some((tree, below)) = packages.places.get(path) else {
                continue;
            };
            let mut directory = Found::default();
            let place = Place {
                tree,
                label: "Skillset",
                deepest: DEEPEST,
            };
            // Each is whole by itself: a folder seen for another is read
            // again for this one.
            walk.seen.clear();
            directory.walk(&place, below, 0, &mut walk, None);
            directory.switch_off(&roots.disabled);
            found.directories.insert(path.clone(), Arc::new(directory));
        }
        found
    }

    /// Lists what the project's `folders` hold, by name only, as not
    /// loaded: the project isn't trusted, so nothing in it is read.
    fn list_untrusted(&mut self, folders: &[Folder]) {
        let mut left = MOST_SKILLS;
        for folder in folders {
            for child in folder.tree.children(&folder.below) {
                if left == 0 {
                    return;
                }
                if !child.folder || child.name.starts_with('.') {
                    continue;
                }
                left -= 1;
                let path = folder.tree.path(&folder.below.join(&child.name));
                if folder.holds == Holds::Packages {
                    self.unloaded.entry(child.name).or_insert(Unloaded {
                        path,
                        source: folder.label.to_owned(),
                        why: Why::Untrusted,
                    });
                } else {
                    self.untrusted.push(Skill {
                        name: child.name,
                        description: String::new(),
                        path: path.join(SKILL).to_string_lossy().into_owned(),
                        source: folder.label.to_owned(),
                        problem: Some(Why::Untrusted.to_string()),
                        implicit: false,
                        user_invocable: false,
                    });
                }
            }
        }
    }

    /// Reads the MCP servers set for these `roots`, and those the
    /// specialists and skillsets found bring with them.
    fn discover_mcp(&mut self, roots: &Roots, packages: &skillsets::Packages) {
        let bundle_roots = (packages.selectable().into_iter())
            .filter_map(|directory| directory.parent().map(Path::to_owned))
            .collect();
        self.managed_mcp = crate::managed_mcp::config::Registry::discover(roots, &bundle_roots);
        // A bundle whose skills every chat gets gives every chat its
        // servers too; a specialist's are for chats started as it.
        let public: std::collections::BTreeSet<_> = (packages.directories.values())
            .flatten()
            .filter_map(|(directory, _)| directory.parent().map(Path::to_owned))
            .collect();
        for bound in self.managed_mcp.servers.values_mut() {
            let bundled = !bound.editable;
            let for_everyone = (bound.path.parent()).is_some_and(|root| public.contains(root));
            if bundled && !for_everyone {
                bound.available_by_default = false;
                bound.source = bound.source.replacen("Skillset", "Specialist", 1);
            }
        }
    }
}

/// Where a walk through skills is, and what it's allowed.
struct Place<'a> {
    tree: &'a Tree,
    /// Where what's found is said to come from.
    label: &'a str,
    /// How many folders below where it started a skill's folder may be.
    deepest: usize,
}

impl<'a> Place<'a> {
    fn of(folder: &'a Folder, deepest: usize) -> Self {
        Self {
            tree: &folder.tree,
            label: folder.label,
            deepest,
        }
    }
}

impl Found {
    /// Takes the skills in the folders in `below`, which is `depth`
    /// folders into `place`: a folder with a `SKILL.md` is a skill, and
    /// any other is looked into for more, as far as `place` allows.
    /// Folders are taken in order of name, so what's found when there are
    /// too many doesn't depend on the order the disk lists them in.
    /// `stamp`, if it's given, takes a fingerprint of everything found.
    fn walk(
        &mut self,
        place: &Place<'_>,
        below: &Path,
        depth: usize,
        walk: &mut Walk,
        mut stamp: Option<&mut u64>,
    ) {
        if self.len() >= MOST_SKILLS || !walk.seen.insert(place.tree.real(below)) {
            return;
        }
        for child in place.tree.children(below) {
            if walk.budget == 0 || self.len() >= MOST_SKILLS {
                return;
            }
            // Dot folders are for whatever keeps the folder, not skills.
            if !child.folder || child.name.starts_with('.') {
                continue;
            }
            walk.budget -= 1;
            let folder = below.join(&child.name);
            // A native plugin's bundle: its skills come with tools and
            // hooks that only the product it's for supplies.
            let plugin = [".claude-plugin", ".codex-plugin"]
                .iter()
                .any(|mark| place.tree.has_folder(&folder.join(mark)));
            if plugin {
                continue;
            }
            let read = read(place.tree, &folder, place.label);
            if let Some(stamp) = stamp.as_deref_mut() {
                let seen = read
                    .as_ref()
                    .map(|read| read.as_ref().map(|r| &r.entry.info));
                mix(stamp, format!("{}: {seen:?}", child.name));
            }
            match read {
                Ok(Some(read)) => self.offer(&Arc::new(read.entry)),
                Ok(None) if depth + 1 < place.deepest => {
                    self.walk(place, &folder, depth + 1, walk, stamp.as_deref_mut());
                }
                Ok(None) => {}
                // Its name is taken all the same: a broken copy never lets
                // a less specific one run in its place.
                Err(problem) => self.refuse(Skill {
                    name: child.name,
                    description: String::new(),
                    path: (place.tree.path(&folder).join(SKILL))
                        .to_string_lossy()
                        .into_owned(),
                    source: place.label.to_owned(),
                    problem: Some(problem),
                    implicit: false,
                    user_invocable: false,
                }),
            }
        }
    }
}

/// A skill as its file has it.
pub(super) struct Read {
    pub(super) entry: Entry,
    /// Its instructions: the Markdown after its front matter.
    pub(super) body: String,
}

/// What a skill's front matter may not ask for, as shared skills run
/// nothing themselves: each needs the product it was written for.
const UNSUPPORTED: [&str; 10] = [
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
];

/// Reads the skill in `folder`, below `tree`, said to come from `label`:
/// `None` if the folder has no `SKILL.md`, and why if it has one that
/// can't be a skill at all. One that can, but can't be run, is a skill
/// with a `problem`.
pub(super) fn read(tree: &Tree, folder: &Path, label: &str) -> Result<Option<Read>, String> {
    let text = match tree.text(&folder.join(SKILL)) {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(None),
        Err(unread) => return Err(unread.to_string()),
    };
    let (header, body) = front_matter::split(&text).map_err(|unsplit| unsplit.to_string())?;
    let metadata: Value =
        serde_yaml_ng::from_str(header).map_err(|err| format!("invalid YAML: {err}"))?;
    let string = |key: &str| {
        metadata[key]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let boolean = |key: &str, default: bool| -> Result<bool, String> {
        match &metadata[key] {
            Value::Null => Ok(default),
            Value::Bool(value) => Ok(*value),
            _ => Err(format!("{key} must be true or false")),
        }
    };
    let name = string("name").ok_or("it has no name")?;
    let named_well = name.len() <= 64
        && (name.bytes()).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_:".contains(&byte)
        });
    if !named_well {
        return Err(
            "skill names use at most 64 lowercase letters, numbers, hyphens, underscores or colons"
                .into(),
        );
    }
    // A skill is known by its folder, which is what decides the copy that
    // wins, so the two must agree, as the Agent Skills format asks.
    let folder_name = folder.file_name().and_then(|name| name.to_str());
    if folder_name != Some(name) {
        return Err(format!(
            "its name, {name}, isn't its folder's, {}",
            folder_name.unwrap_or_default()
        ));
    }
    let description = string("description").ok_or("it has no description")?;

    let mut problems = Vec::new();
    if clackatoo_protocol::INTERFACE_COMMANDS.contains(&name)
        || clackatoo_protocol::KNOWN_COMMANDS.contains(&name)
    {
        problems.push("the name is reserved by a built-in command".to_owned());
    }
    for key in UNSUPPORTED {
        if !metadata[key].is_null() {
            problems.push(format!(
                "{key} requires runtime-specific execution, which shared skills do not support"
            ));
        }
    }
    let body = body.replace("\r\n", "\n");
    if front_matter::runs_commands(&body) {
        problems.push("dynamic shell expansion is not supported".into());
    }
    if body.contains("$ARGUMENTS[") {
        problems.push("positional argument expansion is not supported; use $ARGUMENTS".into());
    }
    let implicit = implicit_policy(tree, folder)? && !boolean("disable-model-invocation", false)?;
    let entry = Entry {
        info: Skill {
            name: name.to_owned(),
            description: brief(description, MOST_DESCRIPTION),
            path: (tree.real(folder).join(SKILL))
                .to_string_lossy()
                .into_owned(),
            source: label.to_owned(),
            problem: (!problems.is_empty()).then(|| problems.join("; ")),
            implicit,
            user_invocable: boolean("user-invocable", true)?,
        },
        hint: string("argument-hint").map(|hint| line(hint, 100)),
        tree: tree.clone(),
        folder: folder.to_owned(),
    };
    Ok(Some(Read {
        entry,
        body: body.trim().to_owned(),
    }))
}

/// Whether the skill in `folder` may be chosen by an agent, as Codex's
/// `agents/openai.yaml` beside it says, if it has one.
fn implicit_policy(tree: &Tree, folder: &Path) -> Result<bool, String> {
    let policy = match tree.text(&folder.join("agents/openai.yaml")) {
        Ok(Some(policy)) => policy,
        Ok(None) => return Ok(true),
        Err(unread) => return Err(format!("agents/openai.yaml: {unread}")),
    };
    let policy: Value = serde_yaml_ng::from_str(&policy)
        .map_err(|err| format!("invalid agents/openai.yaml: {err}"))?;
    match &policy["policy"]["allow_implicit_invocation"] {
        Value::Null => Ok(true),
        Value::Bool(allow) => Ok(*allow),
        _ => Err("policy.allow_implicit_invocation must be true or false".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::tests::skill;

    /// With more folders than may be looked into, those taken are the
    /// first by name, whatever order the disk lists them in.
    #[test]
    fn folders_are_taken_in_order_of_name_before_the_budget_runs_out() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["delta", "alpha", "charlie", "bravo"] {
            skill(&dir.path().join(name), name, "", "x");
        }
        let tree = Tree::personal(dir.path());
        let place = Place {
            tree: &tree,
            label: "Personal",
            deepest: DEEPEST,
        };
        let mut walk = Walk {
            seen: HashSet::new(),
            budget: 2,
        };
        let mut found = Found::default();
        found.walk(&place, Path::new(""), 0, &mut walk, None);
        let names: Vec<_> = found.entries.keys().map(String::as_str).collect();
        assert_eq!(names, ["alpha", "bravo"]);
    }

    /// A folder that isn't the daemon's user's is never a project to
    /// read, as the file system's root isn't, but for root itself.
    #[test]
    fn only_the_users_own_folder_is_read_as_a_project() {
        let dir = tempfile::tempdir().unwrap();
        assert!(owned_by_user(dir.path()));
        assert!(!owned_by_user(&dir.path().join("absent")));
        if !rustix::process::geteuid().is_root() {
            assert!(!owned_by_user(Path::new("/")));
            let top = dir.path().canonicalize().unwrap();
            let roots = Roots {
                project: PathBuf::from("/"),
                ..Roots::in_project(top.clone(), PathBuf::from("/no/such/home"), None)
            };
            skill(&top.join(".agents/skills/planted"), "planted", "", "x");
            // Its folders would be searched, were the project the user's.
            assert!(project_folders(&roots).len() > 4);
            assert!(Catalogue::discover(&roots).infos().is_empty());
        }
    }
}
