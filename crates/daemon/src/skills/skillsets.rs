//! Skillsets and specialists, as folders: a `skillset.toml` or a
//! `SPECIALIST.md`, the skills beside it, and the other bundles it
//! includes. What a bundle includes is worked out before any of it is
//! offered. Specialists keep their own folders of skills; only plain
//! skillsets merge theirs into what every chat gets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::discover::{Folder, Holds};
use super::reading::Tree;
use super::{HashSet, MOST_ENTRIES, Unloaded, Why, mix};
use crate::specialists::{self, Budget, Extras, Limits, Specialist};

/// The most bundles one bundle's includes may lead to, in all.
const MOST_INCLUDED: usize = 256;

/// How many bundles deep includes may go.
const DEEPEST_INCLUDE: usize = 32;

/// A skillset or a specialist, as its folder defines it.
#[derive(Clone, Debug)]
struct Bundle {
    /// The tree its folder is in, and the folder, below it.
    tree: Tree,
    below: PathBuf,
    /// Its folder, with links followed where they may be.
    root: PathBuf,
    /// The file that defines it.
    source: PathBuf,
    /// That file's text, if it's a `SPECIALIST.md`, which Settings edits.
    text: Option<String>,
    name: String,
    includes: Vec<String>,
    specialist: Option<Specialist>,
}

/// Everything the bundles found amount to.
#[derive(Default)]
pub(super) struct Packages {
    /// The folders of skills every chat gets, with where each is said to
    /// come from, by the place among the folders searched of the one its
    /// skillset was found in.
    pub(super) directories: BTreeMap<usize, Vec<(PathBuf, String)>>,
    pub(super) specialists: BTreeMap<String, Specialist>,
    /// The file each specialist is defined in.
    pub(super) sources: BTreeMap<String, PathBuf>,
    /// The text of each of those that's a `SPECIALIST.md`.
    pub(super) texts: BTreeMap<PathBuf, String>,
    pub(super) includes: BTreeMap<String, Vec<String>>,
    /// Each skillset's folders of skills, or `None` for a specialist.
    pub(super) sets: BTreeMap<String, Option<Vec<PathBuf>>>,
    /// Where each folder of skills is read from: its tree, and where it
    /// is below it.
    pub(super) places: BTreeMap<PathBuf, (Tree, PathBuf)>,
    /// The bundles found but not offered, by name, and why.
    pub(super) unloaded: BTreeMap<String, Unloaded>,
    /// A fingerprint of the bundles in the user's own folders.
    pub(super) personal: u64,
}

impl Packages {
    /// The folders of skills a chat can be given: every skillset's, and
    /// every specialist's own.
    pub(super) fn selectable(&self) -> BTreeSet<&PathBuf> {
        let sets = self.sets.values().flatten().flatten();
        let own = (self.specialists.values()).flat_map(|specialist| &specialist.extra.directories);
        sets.chain(own).collect()
    }
}

/// What one bundle and all it includes amount to.
#[derive(Default)]
struct Resolved {
    specialists: BTreeMap<String, Specialist>,
    sources: BTreeMap<String, PathBuf>,
    texts: BTreeMap<PathBuf, String>,
    includes: BTreeMap<String, Vec<String>>,
    sets: BTreeMap<String, Option<Vec<PathBuf>>>,
    places: BTreeMap<PathBuf, (Tree, PathBuf)>,
    /// The folder of each bundle met, by name: two of one name are refused.
    names: BTreeMap<String, PathBuf>,
}

/// A bundle found in one of the folders searched.
struct Chosen {
    /// The place of that folder among those searched.
    at: usize,
    /// Where it's said to come from.
    label: &'static str,
    bundle: Bundle,
}

/// What finding the bundles has come to so far.
#[derive(Default)]
struct Finding {
    packages: Packages,
    /// The bundles that can be included by name.
    named: BTreeMap<String, Bundle>,
    /// The names taken: by a bundle chosen, or one that can't be read.
    taken: HashSet<String>,
    /// What the user's own definition of a specialist allows, where a
    /// project's of the same name may be the one used.
    floors: BTreeMap<String, (Limits, Budget)>,
    /// The specialists a project defines, which those floors hold.
    from_project: BTreeSet<String>,
}

/// Finds the bundles in `folders`, in order: the first of a name is the
/// one used, whole, and one that can't be read keeps its name from a
/// later one all the same.
pub(super) fn discover(folders: &[Folder]) -> Packages {
    let mut finding = Finding::default();
    let mut chosen = Vec::new();
    let mut budget = MOST_ENTRIES;
    for (at, folder) in folders.iter().enumerate() {
        if folder.holds != Holds::Packages {
            continue;
        }
        for child in folder.tree.children(&folder.below) {
            if budget == 0 {
                break;
            }
            if !child.folder || child.name.starts_with('.') {
                continue;
            }
            budget -= 1;
            chosen.extend(finding.choose(at, folder, child.name));
        }
    }
    // Once every bundle that can be included by name is known.
    for chosen in chosen {
        finding.take(chosen);
    }
    finding.finish()
}

impl Finding {
    /// Reads the bundle in the folder called `name` of `folder`, the one
    /// at `at` among those searched, and returns it if it's the one of
    /// its name that's used.
    fn choose(&mut self, at: usize, folder: &Folder, name: String) -> Option<Chosen> {
        let below = folder.below.join(&name);
        let read = package(&folder.tree, &below);
        if folder.tree.is_personal() {
            let seen =
                (read.as_ref()).map(|bundle| (&bundle.name, &bundle.includes, &bundle.specialist));
            mix(&mut self.packages.personal, format!("{name}: {seen:?}"));
        }
        let unloaded = |path: PathBuf, why: Why| Unloaded {
            path,
            source: folder.label.to_owned(),
            why,
        };
        match read {
            // Refused here, so its name is left for the user's own.
            Ok(bundle) if replaces_built_in(&bundle) => {
                let refused = unloaded(bundle.root, Why::BuiltIn);
                self.packages.unloaded.entry(bundle.name).or_insert(refused);
                None
            }
            Ok(bundle) if self.taken.insert(bundle.name.clone()) => {
                self.named.insert(bundle.name.clone(), bundle.clone());
                Some(Chosen {
                    at,
                    label: folder.label,
                    bundle,
                })
            }
            // A less specific copy: not used, but the user's own still
            // says how much a project's specialist of its name may do.
            Ok(bundle) => {
                if let (true, Some(own)) = (bundle.tree.is_personal(), &bundle.specialist) {
                    let allows = (own.limits, own.extra.budget);
                    self.floors.entry(bundle.name).or_insert(allows);
                }
                None
            }
            Err(problem) => {
                let path = folder.tree.path(&below);
                log::warn!("{}: {problem}", path.display());
                if self.taken.insert(name.clone()) {
                    let broken = unloaded(path, Why::Broken(problem));
                    self.packages.unloaded.insert(name, broken);
                }
                None
            }
        }
    }

    /// Takes what `chosen` and the bundles it includes amount to: its
    /// skills, if it's a plain skillset, and the specialists it defines.
    fn take(&mut self, chosen: Chosen) {
        let Chosen { at, label, bundle } = chosen;
        let mut resolved = Resolved::default();
        let mut left = MOST_INCLUDED;
        let stack = &mut Vec::new();
        let directories = match resolve(&bundle, &self.named, stack, &mut resolved, &mut left) {
            Ok(directories) => directories,
            Err(problem) => {
                log::warn!("{}: {problem}", bundle.root.display());
                let broken = Unloaded {
                    path: bundle.root,
                    source: label.to_owned(),
                    why: Why::Broken(problem),
                };
                self.packages.unloaded.insert(bundle.name, broken);
                return;
            }
        };
        let packages = &mut self.packages;
        if bundle.specialist.is_none() {
            let label = format!("{label} · skillset {}", bundle.name);
            (packages.directories.entry(at).or_default())
                .extend(directories.into_iter().map(|dir| (dir, label.clone())));
        }
        packages.places.extend(resolved.places);
        for (name, dirs) in resolved.sets {
            packages.sets.entry(name).or_insert(dirs);
        }
        let project = !bundle.tree.is_personal();
        for (name, specialist) in resolved.specialists {
            let source = resolved.sources.get(&name);
            // A project's, inside a bundle of another name, that would
            // take the place of one built in.
            if project && specialists::is_built_in(&name) {
                let folder = source.and_then(|file| file.parent());
                packages.unloaded.entry(name).or_insert_with(|| Unloaded {
                    path: folder.unwrap_or(&bundle.root).to_owned(),
                    source: label.to_owned(),
                    why: Why::BuiltIn,
                });
                continue;
            }
            if let Some(old) = packages.specialists.get(&name) {
                if old != &specialist {
                    log::warn!(
                        "conflicting specialist {name}; keeping its higher-priority definition"
                    );
                }
                if !project {
                    let allows = (specialist.limits, specialist.extra.budget);
                    self.floors.entry(name).or_insert(allows);
                }
                continue;
            }
            if project {
                self.from_project.insert(name.clone());
            }
            if let Some(path) = source {
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

    /// The bundles found, with each specialist a project defines held to
    /// what the user's own of the same name allows: a project never lets
    /// a chat do more than the user's definition does.
    fn finish(mut self) -> Packages {
        for (name, (limits, budget)) in self.floors {
            if !self.from_project.contains(&name) {
                continue;
            }
            if let Some(specialist) = self.packages.specialists.get_mut(&name) {
                specialist.limits = specialist.limits.max(limits);
                specialist.extra.budget = specialist.extra.budget.capped(budget);
            }
        }
        self.packages
    }
}

/// Whether `bundle` is a project's specialist with the name of one
/// clackatoo builds in, whose place only the user's own may take.
fn replaces_built_in(bundle: &Bundle) -> bool {
    let project = !bundle.tree.is_personal();
    project && bundle.specialist.is_some() && specialists::is_built_in(&bundle.name)
}

/// Works out what `bundle` and the bundles it includes amount to, into
/// `out`, and returns its folders of skills: its own, then those of the
/// plain skillsets it includes, in order. `named` are the bundles that
/// can be included by name, `stack` those being worked out, and `left`
/// how many more may be.
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
    if stack.len() >= DEEPEST_INCLUDE || *left == 0 {
        return Err("bundle dependency limit exceeded".into());
    }
    *left -= 1;
    if let Some(old) = out.names.get(&bundle.name) {
        if old != &bundle.root {
            return Err(format!("conflicting bundle name {}", bundle.name));
        }
        let known = out.sets.get(&bundle.name).cloned().flatten();
        return Ok(known.unwrap_or_default());
    }
    stack.push(bundle.root.clone());
    let own = bundle.root.join("skills");
    out.places.insert(
        own.clone(),
        (bundle.tree.clone(), bundle.below.join("skills")),
    );
    let mut directories = vec![own];
    let selected = (bundle.specialist.as_ref())
        .and_then(|specialist| specialist.extra.skillsets.as_ref())
        .into_iter()
        .flatten();
    for reference in bundle.includes.iter().chain(selected) {
        let child = if reference.contains('/') || reference.starts_with('.') {
            inner(bundle, reference)?
        } else {
            (named.get(reference).cloned()).ok_or_else(|| format!("missing bundle {reference}"))?
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

/// The bundle inside `bundle` that it includes by `reference`, a path:
/// never one outside it.
fn inner(bundle: &Bundle, reference: &str) -> Result<Bundle, String> {
    let path = Path::new(reference);
    let leaves = path.is_absolute()
        || (path.components()).any(|part| matches!(part, std::path::Component::ParentDir));
    if leaves {
        return Err(
            "bundle paths must stay inside their containing bundle; use a registered name for other skillsets"
                .into(),
        );
    }
    let below: PathBuf = bundle.below.join(path).components().collect();
    let child = package(&bundle.tree, &below)?;
    // A link in one of the user's own folders may lead anywhere.
    if !child.root.starts_with(&bundle.root) {
        return Err("bundle dependency leaves its containing folder".into());
    }
    Ok(child)
}

/// Checks that `text`, a `SPECIALIST.md` to be saved as `target` in the
/// folder `below` of `tree`, works with the bundles there are, as it
/// would once saved: before any file is replaced.
pub(super) fn validate_edit(
    roots: &super::Roots,
    tree: &Tree,
    below: &Path,
    target: &Path,
    text: &str,
) -> Result<(), String> {
    let mut named = BTreeMap::new();
    let mut left = MOST_ENTRIES;
    for place in super::specialist_files::package_locations(roots) {
        let tree = place.tree();
        for child in tree.children(&place.below) {
            if left == 0 {
                break;
            }
            left -= 1;
            if let Ok(bundle) = package(&tree, &place.below.join(&child.name)) {
                named.entry(bundle.name.clone()).or_insert(bundle);
            }
        }
    }
    let (name, _, includes, specialist) = super::specialist_files::parse(text)?;
    let proposed = Bundle {
        tree: tree.clone(),
        below: below.to_owned(),
        root: tree.real(below),
        source: target.to_owned(),
        text: Some(text.into()),
        name: name.clone(),
        includes,
        specialist: Some(specialist),
    };
    named.insert(name, proposed.clone());
    let mut left = MOST_INCLUDED;
    resolve(
        &proposed,
        &named,
        &mut Vec::new(),
        &mut Resolved::default(),
        &mut left,
    )
    .map(|_| ())
}

/// Reads the bundle in the folder `below` of `tree`: a specialist, if it
/// has a `SPECIALIST.md`, or else what its `skillset.toml` says.
fn package(tree: &Tree, below: &Path) -> Result<Bundle, String> {
    let file = below.join(super::specialist_files::NAME);
    let text = tree
        .text(&file)
        .map_err(|unread| format!("{}: {unread}", super::specialist_files::NAME))?;
    let Some(text) = text else {
        return manifest(tree, below);
    };
    let (name, _, includes, specialist) = super::specialist_files::parse(&text)?;
    if below
        .file_name()
        .is_none_or(|folder| folder != name.as_str())
    {
        return Err("Specialist name must match its folder name".into());
    }
    Ok(Bundle {
        tree: tree.clone(),
        below: below.to_owned(),
        root: tree.real(below),
        source: tree.path(&file),
        text: Some(text),
        name,
        includes,
        specialist: Some(specialist),
    })
}

/// The fields a `skillset.toml` may have.
const MANIFEST_FIELDS: [&str; 6] = [
    "format",
    "name",
    "description",
    "version",
    "includes",
    "specialist",
];

/// Reads the bundle in the folder `below` of `tree` from its
/// `skillset.toml`.
fn manifest(tree: &Tree, below: &Path) -> Result<Bundle, String> {
    let file = below.join("skillset.toml");
    let text = tree
        .text(&file)
        .map_err(|unread| format!("skillset.toml: {unread}"))?
        .ok_or("it has neither a SPECIALIST.md nor a skillset.toml")?;
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|err| err.to_string())?;
    if let Some((key, _)) = (document.iter()).find(|(key, _)| !MANIFEST_FIELDS.contains(key)) {
        return Err(format!("unsupported skillset field {key}"));
    }
    if document.get("format").and_then(toml_edit::Item::as_integer) != Some(1) {
        return Err("format must be 1".into());
    }
    let name = document
        .get("name")
        .and_then(toml_edit::Item::as_str)
        .ok_or("missing name")?;
    if super::specialist_files::check_name(name).is_err() {
        return Err("name must use 1–64 lowercase letters, numbers, hyphens or underscores".into());
    }
    let description = document
        .get("description")
        .and_then(toml_edit::Item::as_str)
        .filter(|description| !description.trim().is_empty())
        .ok_or("missing description")?;
    let versioned_badly = (document.get("version"))
        .is_some_and(|version| version.as_str().is_none_or(|text| text.trim().is_empty()));
    if versioned_badly {
        return Err("version must be a nonempty string".into());
    }
    let includes = match document.get("includes") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or("includes must be an array")?
            .iter()
            .map(|include| {
                include
                    .as_str()
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
        tree: tree.clone(),
        below: below.to_owned(),
        root: tree.real(below),
        source: tree.path(&file),
        text: None,
        name: name.into(),
        includes,
        specialist,
    })
}

/// `item`, of a `skillset.toml`, as the JSON a specialist is read from,
/// so a `[specialist]` table and a `SPECIALIST.md`'s front matter are
/// read by the same rules.
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

/// The fields a specialist may have, besides its name and description.
const SPECIALIST_FIELDS: [&str; 12] = [
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
];

/// The specialist called `name`, described as `description`, that
/// `value` defines: a `[specialist]` table, or a `SPECIALIST.md`'s front
/// matter with its body as `instructions`. A field it doesn't know is
/// refused, so a misspelt limit is never quietly no limit.
pub(super) fn parse_specialist(
    name: &str,
    description: &str,
    value: &serde_json::Value,
) -> Result<Specialist, String> {
    let fields = value.as_object().ok_or("specialist must be a table")?;
    if let Some(key) = (fields.keys()).find(|key| !SPECIALIST_FIELDS.contains(&key.as_str())) {
        return Err(format!("unsupported specialist field {key}"));
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
    use super::super::Missing;
    use super::super::tests::{World, skill};
    use super::*;

    /// Writes a skillset called `name` in `root`, with one skill, `check`.
    fn bundle(root: &Path, name: &str, body: &str) {
        std::fs::create_dir_all(root).unwrap();
        let manifest = format!(
            "format = 1\nname = {name:?}\ndescription = \"A test bundle\"\nversion = \"0.1.0\"\n"
        );
        std::fs::write(root.join("skillset.toml"), manifest).unwrap();
        skill(&root.join("skills/check"), "check", "", body);
    }

    /// Writes a skillset called `name` in `root`, with `extra` lines in
    /// its manifest, and reads it back.
    fn package_in(root: &Path, name: &str, extra: &str) -> Bundle {
        std::fs::create_dir_all(root).unwrap();
        let text = format!("format=1\nname={name:?}\ndescription=\"A bundle\"\n{extra}");
        std::fs::write(root.join("skillset.toml"), text).unwrap();
        let tree = Tree::personal(root.parent().unwrap());
        manifest(&tree, Path::new(root.file_name().unwrap())).unwrap()
    }

    /// Writes a specialist called `name` in `folder`, with `settings`
    /// lines of front matter and `role` for instructions.
    fn specialist(folder: &Path, name: &str, settings: &str, role: &str) {
        std::fs::create_dir_all(folder).unwrap();
        let text = format!("---\nname: {name}\n{settings}---\n{role}\n");
        std::fs::write(folder.join("SPECIALIST.md"), text).unwrap();
    }

    fn resolved(bundle: &Bundle, named: &BTreeMap<String, Bundle>) -> Result<Vec<PathBuf>, String> {
        let mut left = MOST_INCLUDED;
        resolve(
            bundle,
            named,
            &mut Vec::new(),
            &mut Resolved::default(),
            &mut left,
        )
    }

    #[test]
    fn bundles_share_commands_and_project_precedence_without_merging_versions() {
        let world = World::new();
        let personal = world.home.join(".agents/skillsets/release");
        bundle(&personal, "release", "personal");
        skill(
            &personal.join("skills/extra"),
            "extra",
            "",
            "personal extra",
        );
        bundle(
            &world.repo.join(".agents/skillsets/release"),
            "release",
            "project $ARGUMENTS",
        );
        let catalogue = world.found();
        assert_eq!(catalogue.commands().len(), 1);
        assert!(catalogue.infos()[0].source.contains("skillset release"));
        let invoked = catalogue.explicit("/check argument").unwrap().unwrap();
        assert!(invoked.contains("project argument"));
        assert!(catalogue.load("extra", "", false).is_err());
        skill(
            &world.repo.join(".agents/skills/check"),
            "check",
            "",
            "local override",
        );
        let loaded = world.found().load("check", "", false).unwrap();
        assert!(loaded.contains("local override"));
    }

    #[test]
    fn unknown_components_and_future_formats_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("release");
        bundle(&root, "release", "body");
        let path = root.join("skillset.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        let read = || manifest(&Tree::personal(dir.path()), Path::new("release"));
        assert!(read().is_ok());
        std::fs::write(&path, format!("{original}\nhooks = []\n")).unwrap();
        assert!(read().unwrap_err().contains("hooks"));
        std::fs::write(&path, original.replace("format = 1", "format = 2")).unwrap();
        assert!(read().unwrap_err().contains("format"));
    }

    #[test]
    fn nested_roles_keep_their_instructions_and_skills_out_of_the_parent() {
        let world = World::new();
        let parent = world.repo.join(".agents/skillsets/team");
        package_in(
            &parent,
            "team",
            "includes=[\"./helpers/reviewer\", \"./shared\"]",
        );
        package_in(&parent.join("shared"), "shared", "");
        skill(&parent.join("shared/skills/common"), "common", "", "COMMON");
        package_in(
            &parent.join("helpers/reviewer"),
            "reviewer",
            r#"
includes=["./subroles/auditor", "./formatting"]
[specialist]
instructions="REVIEW ROLE"
skills=[]
"#,
        );
        package_in(
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
        package_in(
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
        let catalogue = world.found();
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
        // Choosing a chat's skills, on the daemon's loop, reads nothing:
        // it's the same with the folders gone.
        std::fs::remove_dir_all(&parent).unwrap();
        let scoped = catalogue.scoped(reviewer).unwrap();
        let commands: Vec<_> = (scoped.commands().into_iter())
            .map(|command| command.name)
            .collect();
        assert_eq!(commands, ["format", "review-code"]);
    }

    #[test]
    fn a_specialist_needing_what_isnt_there_says_what() {
        let world = World::new();
        let folder = world.home.join(".agents/specialists/reviewer");
        specialist(&folder, "reviewer", "skills: [absent]\n", "Review");
        let catalogue = world.found();
        let missing = (catalogue.scoped(&catalogue.specialists["reviewer"])).unwrap_err();
        let expected = Missing::Skill {
            specialist: "reviewer".into(),
            name: "absent".into(),
        };
        assert_eq!(missing, expected);
        assert_eq!(
            missing.to_string(),
            "specialist reviewer needs missing skill absent"
        );
    }

    #[test]
    fn graphs_reject_cycles_conflicts_and_escaping_paths_and_deduplicate_diamonds() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().canonicalize().unwrap();
        let a = package_in(&top.join("a"), "a", "includes=[\"b\",\"c\"]");
        let b = package_in(&top.join("b"), "b", "includes=[\"d\"]");
        let c = package_in(&top.join("c"), "c", "includes=[\"d\"]");
        let d = package_in(&top.join("d"), "d", "");
        let mut named: BTreeMap<_, _> = [&a, &b, &c, &d]
            .into_iter()
            .map(|b| (b.name.clone(), b.clone()))
            .collect();
        let mut out = Resolved::default();
        let mut left = MOST_INCLUDED;
        let dirs = resolve(&a, &named, &mut Vec::new(), &mut out, &mut left).unwrap();
        assert_eq!(dirs.len(), 4);
        assert_eq!(out.sets.len(), 4);
        named.get_mut("d").unwrap().includes.push("a".into());
        assert!(resolved(&a, &named).unwrap_err().contains("cycle"));
        let bad = package_in(&top.join("bad"), "bad", "includes=[\"../a\"]");
        assert!(resolved(&bad, &named).unwrap_err().contains("stay inside"));
        let conflict = package_in(
            &top.join("conflict"),
            "conflict",
            "includes=[\"./one\",\"./two\"]",
        );
        package_in(&conflict.root.join("one"), "same", "");
        package_in(&conflict.root.join("two"), "same", "");
        let refused = resolved(&conflict, &named).unwrap_err();
        assert!(refused.contains("conflicting bundle name"), "{refused}");
    }

    /// One that can't be used is kept, with why, for Settings and for
    /// whoever asks to start a chat as it: not only written to the log.
    #[test]
    fn broken_specialists_and_skillsets_are_carried_with_their_reason() {
        let world = World::new();
        let specialists = world.home.join(".agents/specialists");
        specialist(&specialists.join("typo"), "typo", "limit: read-only\n", "x");
        specialist(&specialists.join("renamed"), "other", "", "x");
        specialist(
            &specialists.join("needy"),
            "needy",
            "includes: [absent]\n",
            "x",
        );
        let sets = world.repo.join(".agents/skillsets");
        std::fs::create_dir_all(sets.join("empty")).unwrap();
        std::fs::create_dir_all(sets.join("future")).unwrap();
        std::fs::write(sets.join("future/skillset.toml"), "format = 2\n").unwrap();
        let catalogue = world.found();
        assert!(catalogue.specialists.is_empty());
        let why = |name: &str| catalogue.unloaded[name].why.to_string();
        assert!(why("typo").contains("unsupported specialist field limit"));
        assert!(why("renamed").contains("must match its folder"));
        assert!(why("needy").contains("missing bundle absent"));
        assert!(why("empty").contains("neither"));
        assert!(why("future").contains("format must be 1"));
        assert_eq!(catalogue.unloaded["future"].source, "Project");
        assert_eq!(catalogue.unloaded["typo"].path, specialists.join("typo"));
        let listed = catalogue.infos();
        let typo = listed.iter().find(|info| info.name == "typo").unwrap();
        assert!(typo.problem.as_deref().unwrap().starts_with("broken: "));
        assert_eq!(typo.source, "Personal · specialist or skillset");
    }

    /// A project can't take the place of a specialist clackatoo builds
    /// in, whose name promises what it does, as `explore` promises to
    /// change nothing. The user's own folders can.
    #[test]
    fn only_the_users_own_folders_replace_a_built_in_specialist() {
        let world = World::new();
        let project = world.repo.join(".agents/specialists");
        specialist(
            &project.join("explore"),
            "explore",
            "limits: none\n",
            "REPO",
        );
        // Nested in a bundle of another name, too.
        let team = world.repo.join(".agents/skillsets/team");
        package_in(&team, "team", "includes=[\"./general\"]");
        specialist(&team.join("general"), "general", "", "REPO GENERAL");
        let catalogue = world.found();
        assert!(catalogue.specialists.is_empty());
        assert_eq!(catalogue.unloaded["explore"].why, Why::BuiltIn);
        assert_eq!(catalogue.unloaded["general"].why, Why::BuiltIn);
        assert_eq!(catalogue.unloaded["general"].path, team.join("general"));

        let own = world.home.join(".agents/specialists/explore");
        specialist(&own, "explore", "limits: no-commands\n", "MINE");
        let catalogue = world.found();
        assert_eq!(catalogue.specialists["explore"].instructions, "MINE");
        assert_eq!(catalogue.specialists["explore"].limits, Limits::NoCommands);
    }

    /// A project's specialist with the name of one of the user's own is
    /// the one used, but allows no more than the user's does.
    #[test]
    fn a_projects_specialist_never_allows_more_than_the_users_own_of_its_name() {
        let world = World::new();
        specialist(
            &world.home.join(".agents/specialists/reviewer"),
            "reviewer",
            "limits:\n  access: read-only\n  seconds: 60\n",
            "MINE",
        );
        specialist(
            &world.repo.join(".agents/specialists/reviewer"),
            "reviewer",
            "limits:\n  access: none\n  seconds: 600\n",
            "REPO",
        );
        specialist(
            &world.repo.join(".agents/specialists/builder"),
            "builder",
            "limits: none\n",
            "REPO",
        );
        let catalogue = world.found();
        let reviewer = &catalogue.specialists["reviewer"];
        assert_eq!(reviewer.instructions, "REPO");
        assert_eq!(reviewer.limits, Limits::ReadOnly);
        assert_eq!(reviewer.extra.budget.seconds, Some(60));
        assert_eq!(catalogue.specialists["builder"].limits, Limits::None);
    }

    #[test]
    fn a_projects_bundles_are_read_without_following_links() {
        let world = World::new();
        let outside = world.home.join("outside/reviewer");
        specialist(&outside, "reviewer", "", "OUTSIDE");
        let specialists = world.repo.join(".agents/specialists");
        std::fs::create_dir_all(specialists.join("direct")).unwrap();
        std::os::unix::fs::symlink(&outside, specialists.join("reviewer")).unwrap();
        std::os::unix::fs::symlink(
            outside.join("SPECIALIST.md"),
            specialists.join("direct/SPECIALIST.md"),
        )
        .unwrap();
        let catalogue = world.found();
        assert!(catalogue.specialists.is_empty());
        assert!(!catalogue.unloaded.contains_key("reviewer"));
        let why = catalogue.unloaded["direct"].why.to_string();
        assert!(why.contains("link"), "{why}");
    }
}
