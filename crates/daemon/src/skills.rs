//! One catalogue of Agent Skills for every hosted agent. Disk work belongs
//! on the skills worker, not the daemon's event loop. Only descriptions
//! are advertised; a skill's instructions are read from its file each time
//! it's asked for, by the user or the agent.
//!
//! What's found comes from three kinds of folder, trusted differently:
//!
//! - the user's own (`~/.agents`, clackatoo's config directory), read as
//!   they are, links and all;
//! - other products' (`~/.claude/skills`, `~/.codex/skills`, a project's
//!   `.claude/skills`), read only as those products document them, one
//!   folder deep, so what they keep there for themselves isn't offered;
//! - a project's (`.agents` from the chat's working directory up to its
//!   project root, never above), which anyone may have filled: read only
//!   once the user trusts the folder (see [`crate::trust`]), never
//!   through a link, and never in place of what clackatoo builds in.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::hash::{Hash as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clackatoo_protocol::{AgentCommand, Skill};

mod discover;
pub(crate) mod front_matter;
mod reading;
mod skillsets;
pub(crate) mod specialist_edit;
pub(crate) mod specialist_files;

pub(crate) use discover::owned_by_user;
use reading::MOST_FILE;
pub(crate) use reading::Tree;

/// The most skills a catalogue holds, broken ones included.
const MOST_SKILLS: usize = 256;

/// The most folders looked into for one catalogue.
const MOST_ENTRIES: usize = 2048;

/// The most bytes of catalogue a chat's first message carries: about a
/// thousand tokens, which every chat and every chat it starts pays.
const MOST_ANNOUNCED: usize = 4 * 1024;

/// About how many characters of a skill's description are announced.
const MOST_DESCRIBED: usize = 200;

/// The most characters kept of a description, as Settings shows it.
const MOST_DESCRIPTION: usize = 1536;

/// The most bytes of skills an agent's `list_skills` is given.
const MOST_LISTED: usize = 32 * 1024;

/// What ends a catalogue too long to announce whole.
const MORE: &str = "More skills are available through list_skills.\n";

/// What decides a chat's catalogue: where it's read from, and what the
/// user has said of it. Two chats with the same share one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Roots {
    /// The chat's working directory, absolute, with links followed.
    pub(crate) cwd: PathBuf,
    /// The chat's project: the repository `cwd` is in, or else `cwd`.
    /// Project folders are looked for from `cwd` up to here, never above.
    pub(crate) project: PathBuf,
    /// The user's home directory, with links followed.
    pub(crate) home: PathBuf,
    /// clackatoo's config directory, if it has one.
    pub(crate) config: Option<PathBuf>,
    /// Whether the user trusts the project's folders (see
    /// [`crate::trust`]): what they hold isn't loaded otherwise.
    pub(crate) trusted: bool,
    /// The skills never offered, by name, as `skills.disabled` lists them.
    pub(crate) disabled: Arc<[String]>,
}

impl Roots {
    /// The roots of a chat in `cwd`, a trusted project of its own, with no
    /// skill switched off.
    #[cfg(test)]
    pub(crate) fn in_project(cwd: PathBuf, home: PathBuf, config: Option<PathBuf>) -> Self {
        Self {
            project: cwd.clone(),
            cwd,
            home,
            config,
            trusted: true,
            disabled: Arc::default(),
        }
    }

    /// The folder the project's definitions are read below: the project's
    /// root, or the working directory if that's somehow not in it.
    pub(crate) fn project_top(&self) -> &Path {
        if self.cwd.starts_with(&self.project) {
            &self.project
        } else {
            &self.cwd
        }
    }

    /// The folders a project's definitions are looked for in, nearest
    /// first: the working directory and each folder above it, up to the
    /// project's root. Never the home directory, whose folders are the
    /// user's own, nor any folder home is in.
    pub(crate) fn project_folders(&self) -> impl Iterator<Item = &Path> {
        let top = self.project_top();
        (self.cwd.ancestors())
            .take_while(move |folder| folder.starts_with(top) && !self.home.starts_with(folder))
    }
}

/// Why a specialist or a skillset that was found isn't offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// It's a project's, and the user hasn't trusted the project's folder.
    Untrusted,
    /// It's a project's, with the name of a specialist clackatoo builds in.
    BuiltIn,
    /// It can't be read, or what it says can't be used.
    Broken(String),
}

impl std::fmt::Display for Why {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Untrusted => f.write_str(
                "not loaded: this folder isn't trusted (clackatoo trust, or :trust, trusts it)",
            ),
            Self::BuiltIn => f.write_str(
                "not loaded: a project can't replace a built-in specialist; \
                 give it another name, or keep it in your own folders",
            ),
            Self::Broken(problem) => write!(f, "broken: {problem}"),
        }
    }
}

/// A specialist or a skillset that was found but isn't offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unloaded {
    /// Its folder.
    pub(crate) path: PathBuf,
    /// Where it was found, as a skill's source says it.
    pub(crate) source: String,
    pub(crate) why: Why,
}

/// What a specialist needs that the catalogue doesn't have, so a chat
/// can't start as it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Missing {
    #[allow(missing_docs, reason = "said by their names")]
    Skill { specialist: String, name: String },
    /// A skill it names is there, but can't be used: it's broken, or
    /// switched off.
    #[allow(missing_docs, reason = "said above")]
    Unusable {
        specialist: String,
        name: String,
        problem: String,
    },
    #[allow(missing_docs, reason = "said by their names")]
    Skillset { specialist: String, name: String },
    #[allow(missing_docs, reason = "said by their names")]
    Server { specialist: String, name: String },
    /// A folder of skills of its own, which has gone since it was chosen.
    Directory(PathBuf),
}

impl std::fmt::Display for Missing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Skill { specialist, name } => {
                write!(f, "specialist {specialist} needs missing skill {name}")
            }
            Self::Unusable {
                specialist,
                name,
                problem,
            } => write!(
                f,
                "specialist {specialist} needs skill {name}, which can't be used: {problem}"
            ),
            Self::Skillset { specialist, name } => {
                write!(f, "specialist {specialist} needs missing skillset {name}")
            }
            Self::Server { specialist, name } => {
                write!(f, "specialist {specialist} needs missing MCP server {name}")
            }
            Self::Directory(path) => {
                write!(f, "missing specialist skill directory {}", path.display())
            }
        }
    }
}

/// Why a skill's instructions aren't given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Unloadable {
    /// The catalogue has none by the name.
    Unknown(String),
    /// It's there, but can't be used.
    Broken { name: String, problem: String },
    /// Only the user invokes it, and an agent asked.
    UserOnly(String),
    /// Only agents choose it, and the user asked.
    AgentOnly(String),
    /// Its file has gone since it was found.
    Gone(String),
}

impl std::fmt::Display for Unloadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "No skill named {name}. Use list_skills."),
            Self::Broken { name, problem } => write!(f, "Skill {name} cannot run: {problem}"),
            Self::UserOnly(name) => write!(
                f,
                "Skill {name} is user-only: the user invokes it with /{name}."
            ),
            Self::AgentOnly(name) => write!(
                f,
                "Skill {name} is agent-only and cannot be invoked with a slash command."
            ),
            Self::Gone(name) => write!(
                f,
                "Skill {name} is no longer where it was found. Use list_skills."
            ),
        }
    }
}

/// A skill that can be offered: what's shown of it, and where its
/// instructions are read from when it's asked for.
#[derive(Clone, Debug)]
struct Entry {
    info: Skill,
    /// What its arguments are, for the slash menu.
    hint: Option<String>,
    /// The tree its folder is in, and the folder, below it.
    tree: Tree,
    folder: PathBuf,
}

/// The skills found in some folders: each name once, as the first folder
/// to have it has it, whether that copy can be used or not.
#[derive(Clone, Debug, Default)]
struct Found {
    entries: BTreeMap<String, Arc<Entry>>,
    /// Those that can't be used, with why as their `problem`.
    invalid: Vec<Skill>,
}

impl Found {
    /// Whether a skill called `name` has been found, usable or not: a
    /// later, less specific copy never takes its place.
    fn has(&self, name: &str) -> bool {
        self.entries.contains_key(name) || self.is_invalid(name)
    }

    /// Whether a skill called `name` was found that can't be used.
    fn is_invalid(&self, name: &str) -> bool {
        self.invalid.iter().any(|invalid| invalid.name == name)
    }

    /// How many skills have been found, usable or not.
    fn len(&self) -> usize {
        self.entries.len() + self.invalid.len()
    }

    /// Takes `entry`, unless its name is taken.
    fn offer(&mut self, entry: &Arc<Entry>) {
        if !self.has(&entry.info.name) {
            self.entries
                .insert(entry.info.name.clone(), Arc::clone(entry));
        }
    }

    /// Takes `info`, a skill that can't be used, unless its name is taken.
    fn refuse(&mut self, info: Skill) {
        if !self.has(&info.name) {
            self.invalid.push(info);
        }
    }

    /// Takes everything in `other` whose name isn't taken, said to come
    /// from `source`.
    fn include(&mut self, other: &Self, source: &str) {
        for entry in other.entries.values() {
            let mut entry = entry.as_ref().clone();
            source.clone_into(&mut entry.info.source);
            self.offer(&Arc::new(entry));
        }
        for info in &other.invalid {
            let mut info = info.clone();
            source.clone_into(&mut info.source);
            self.refuse(info);
        }
    }

    /// Stops offering the skills called `disabled`, which stay listed with
    /// why.
    fn switch_off(&mut self, disabled: &[String]) {
        for name in disabled {
            if let Some(entry) = self.entries.remove(name) {
                let mut info = entry.info.clone();
                info.problem = Some("switched off by the skills.disabled option".to_owned());
                info.implicit = false;
                info.user_invocable = false;
                self.invalid.push(info);
            }
        }
    }
}

/// A snapshot of what's on disk, read off the event loop. The same
/// snapshot supplies commands, descriptions and specialists.
#[derive(Clone, Debug, Default)]
pub(crate) struct Catalogue {
    skills: Found,
    /// A project's skills that aren't loaded as its folder isn't trusted:
    /// shown in Settings, and no part of anything else.
    untrusted: Vec<Skill>,
    pub(crate) specialists: BTreeMap<String, crate::specialists::Specialist>,
    pub(crate) specialist_sources: BTreeMap<String, PathBuf>,
    pub(crate) specialist_texts: BTreeMap<PathBuf, String>,
    pub(crate) specialist_includes: BTreeMap<String, Vec<String>>,
    /// The specialists and skillsets found but not offered, by name.
    pub(crate) unloaded: BTreeMap<String, Unloaded>,
    sets: BTreeMap<String, Option<Vec<PathBuf>>>,
    directories: BTreeMap<PathBuf, Arc<Found>>,
    /// All file IO happens during discovery; selecting a session is in-memory.
    pub(crate) managed_mcp: crate::managed_mcp::config::Registry,
    /// A fingerprint of everything found in the user's own folders, which
    /// every chat's catalogue shares: when it changes, the catalogues read
    /// for other folders are out of date too.
    personal: u64,
}

impl Catalogue {
    /// The catalogue a chat started as `specialist` gets: the skills and
    /// MCP servers the specialist chooses, or all of them if it chooses
    /// none. In memory only.
    pub(crate) fn scoped(
        &self,
        specialist: &crate::specialists::Specialist,
    ) -> Result<Self, Missing> {
        let mut scoped = Self {
            skills: Found::default(),
            ..self.clone()
        };
        let own = specialist.name.clone();
        let source = format!("Specialist {own}");
        for directory in &specialist.extra.directories {
            scoped.include_directory(directory, &source)?;
        }
        for name in specialist.extra.skillsets.iter().flatten() {
            let directories = self.sets.get(name).ok_or_else(|| Missing::Skillset {
                specialist: own.clone(),
                name: name.clone(),
            })?;
            for directory in directories.iter().flatten() {
                scoped.include_directory(directory, &format!("{source} · skillset {name}"))?;
            }
        }
        if let Some(skills) = &specialist.extra.skills {
            for name in skills {
                let unusable = (self.skills.invalid.iter()).find(|info| info.name == *name);
                if let Some(info) = unusable {
                    return Err(Missing::Unusable {
                        specialist: own.clone(),
                        name: name.clone(),
                        problem: info.problem.clone().unwrap_or_default(),
                    });
                }
                let entry = self
                    .skills
                    .entries
                    .get(name)
                    .ok_or_else(|| Missing::Skill {
                        specialist: own.clone(),
                        name: name.clone(),
                    })?;
                scoped.skills.offer(entry);
            }
        } else if specialist.extra.skillsets.is_none() {
            // It chooses nothing: it gets what any chat here gets.
            for entry in self.skills.entries.values() {
                scoped.skills.offer(entry);
            }
            for info in &self.skills.invalid {
                scoped.skills.refuse(info.clone());
            }
        }
        // The rest stay known, so asking for one says why it's refused.
        for entry in self.skills.entries.values() {
            let mut info = entry.info.clone();
            info.problem = Some(format!("not enabled for specialist {own}"));
            info.implicit = false;
            info.user_invocable = false;
            scoped.skills.refuse(info);
        }
        scoped.scope_mcp(specialist)?;
        Ok(scoped)
    }

    /// Leaves the MCP servers `specialist` chooses: those it names, or
    /// those its own bundles bring, or else those every chat gets.
    fn scope_mcp(&mut self, specialist: &crate::specialists::Specialist) -> Result<(), Missing> {
        let extra = &specialist.extra;
        if let Some(names) = &extra.mcp_servers {
            for name in names {
                if !self.managed_mcp.servers.contains_key(name) {
                    return Err(Missing::Server {
                        specialist: specialist.name.clone(),
                        name: name.clone(),
                    });
                }
            }
            self.managed_mcp
                .servers
                .retain(|name, _| names.contains(name));
        } else if extra.skillsets.is_some() || !extra.directories.is_empty() {
            let mut directories = extra.directories.clone();
            for name in extra.skillsets.iter().flatten() {
                directories.extend(self.sets.get(name).into_iter().flatten().flatten().cloned());
            }
            self.managed_mcp.servers.retain(|_, bound| {
                bound.editable
                    || directories.iter().any(|dir| {
                        dir.parent()
                            .is_some_and(|root| bound.path == root.join("mcp.json"))
                    })
            });
        } else {
            self.managed_mcp
                .servers
                .retain(|_, bound| bound.available_by_default);
        }
        for bound in self.managed_mcp.servers.values_mut() {
            bound.available_by_default = true;
        }
        Ok(())
    }

    /// Takes the skills of the folder at `path`, one of a bundle's, said
    /// to come from `source`.
    fn include_directory(&mut self, path: &Path, source: &str) -> Result<(), Missing> {
        let directory = self
            .directories
            .get(path)
            .ok_or_else(|| Missing::Directory(path.to_owned()))?;
        self.skills.include(directory, source);
        Ok(())
    }

    /// Everything found, for Settings: the skills offered, then those
    /// that can't be, with why, and the specialists and skillsets that
    /// weren't loaded, which have nowhere else to be shown. For the user
    /// only: it has names from folders that aren't trusted.
    pub(crate) fn infos(&self) -> Vec<Skill> {
        let unloaded = self.unloaded.iter().map(|(name, unloaded)| Skill {
            name: name.clone(),
            description: String::new(),
            path: unloaded.path.to_string_lossy().into_owned(),
            source: format!("{} · specialist or skillset", unloaded.source),
            problem: Some(unloaded.why.to_string()),
            implicit: false,
            user_invocable: false,
        });
        (self.skills.entries.values())
            .map(|entry| entry.info.clone())
            .chain(self.skills.invalid.iter().cloned())
            .chain(self.untrusted.iter().cloned())
            .chain(unloaded)
            .collect()
    }

    /// What an agent's `list_skills` gets: the skills it's offered, and
    /// those it asked for that can't be used, each with the start of its
    /// description, as JSON, in [`MOST_LISTED`] bytes at most. Nothing
    /// from a folder that isn't trusted is in it, not even a name: only
    /// how many things weren't loaded, which Settings lists.
    pub(crate) fn listing(&self) -> String {
        let shown = (self.skills.entries.values())
            .map(|entry| &entry.info)
            .chain(&self.skills.invalid);
        let mut listed = Vec::new();
        let mut size = 0;
        let mut left_out = 0;
        for info in shown {
            let info = Skill {
                description: brief(&info.description, MOST_DESCRIBED),
                ..info.clone()
            };
            let Ok(json) = serde_json::to_string(&info) else {
                continue;
            };
            if size + json.len() > MOST_LISTED {
                left_out += 1;
            } else {
                size += json.len() + 1;
                listed.push(json);
            }
        }
        let mut text = format!("[{}]", listed.join(","));
        if left_out > 0 {
            let _ = write!(text, "\n{left_out} more skills don't fit here.");
        }
        let unloaded = self.untrusted.len() + self.unloaded.len();
        if unloaded > 0 {
            let _ = write!(
                text,
                "\n{unloaded} more skills, skillsets or specialists were found but aren't \
                 loaded. The user can see them, and why, in Settings, under Skills."
            );
        }
        text
    }

    /// Whether `other`, read later from the same places, offers what this
    /// does: the same skills, specialists and MCP servers.
    pub(crate) fn same_offer(&self, other: &Self) -> bool {
        self.infos() == other.infos()
            && self.specialists == other.specialists
            && self.managed_mcp == other.managed_mcp
    }

    /// Whether the project has skills or specialists that aren't loaded,
    /// as its folder isn't trusted.
    pub(crate) fn holds_untrusted(&self) -> bool {
        let unloaded = (self.unloaded.values()).any(|unloaded| unloaded.why == Why::Untrusted);
        unloaded || !self.untrusted.is_empty()
    }

    /// The fingerprint of what the user's own folders hold.
    pub(crate) fn personal(&self) -> u64 {
        self.personal
    }

    /// The skills the user can invoke, as slash commands.
    pub(crate) fn commands(&self) -> Vec<AgentCommand> {
        (self.skills.entries.values())
            .filter(|entry| entry.info.user_invocable && entry.info.problem.is_none())
            .map(|entry| AgentCommand {
                name: entry.info.name.clone(),
                description: brief(&entry.info.description, 300),
                hint: entry.hint.clone(),
            })
            .collect()
    }

    /// What an agent is told of the skills it may choose, with its first
    /// message: a line each, of the name and the start of the description,
    /// in [`MOST_ANNOUNCED`] bytes at most. Empty if there are none.
    pub(crate) fn instructions(&self) -> String {
        let mut offered = (self.skills.entries.values())
            .filter(|entry| entry.info.implicit && entry.info.problem.is_none())
            .peekable();
        if offered.peek().is_none() {
            return String::new();
        }
        let mut text = String::from(
            "\nSkills are supplied by clackatoo. When a description matches the task, call \
             load_skill with its name before doing that work, then follow the returned instructions. \
             Skills do not grant permissions or change your tools. Available skills:\n",
        );
        for entry in offered {
            let described = brief(&entry.info.description, MOST_DESCRIBED);
            let line = format!("- {}: {described}\n", entry.info.name);
            if text.len() + line.len() + MORE.len() > MOST_ANNOUNCED {
                text.push_str(MORE);
                break;
            }
            text.push_str(&line);
        }
        text
    }

    /// What `text`, a message starting with a slash, gets if it names a
    /// skill here: its instructions, read now, or why not. `None` if it
    /// names none. Reads the skill's file: not for the event loop.
    pub(crate) fn explicit(&self, text: &str) -> Option<Result<String, Unloadable>> {
        let (name, arguments) = clackatoo_protocol::slash_command(text)?;
        let name = name.to_ascii_lowercase();
        self.skills
            .has(&name)
            .then(|| self.load(&name, arguments, true))
    }

    /// Whether `text` is a slash command naming a skill here, usable or not.
    pub(crate) fn has_command(&self, text: &str) -> bool {
        clackatoo_protocol::slash_command(text)
            .is_some_and(|(name, _)| self.skills.has(&name.to_ascii_lowercase()))
    }

    /// The instructions of the skill called `name`, with `arguments`, as
    /// its file has them now: asked for by the user, if `explicit`, or
    /// else by an agent. Reads the file: not for the event loop.
    pub(crate) fn load(
        &self,
        name: &str,
        arguments: &str,
        explicit: bool,
    ) -> Result<String, Unloadable> {
        let broken = |problem: &str| Unloadable::Broken {
            name: name.to_owned(),
            problem: problem.to_owned(),
        };
        if let Some(invalid) = self.skills.invalid.iter().find(|info| info.name == name) {
            return Err(broken(
                invalid.problem.as_deref().unwrap_or("invalid metadata"),
            ));
        }
        let entry =
            (self.skills.entries.get(name)).ok_or_else(|| Unloadable::Unknown(name.to_owned()))?;
        // Read again, so an edit applies and a skill that's gone isn't
        // run from memory.
        let now = match discover::read(&entry.tree, &entry.folder, &entry.info.source) {
            Ok(Some(now)) => now,
            Ok(None) => return Err(Unloadable::Gone(name.to_owned())),
            Err(problem) => return Err(broken(&problem)),
        };
        let info = &now.entry.info;
        if let Some(problem) = &info.problem {
            return Err(broken(problem));
        }
        if explicit && !info.user_invocable {
            return Err(Unloadable::AgentOnly(name.to_owned()));
        }
        if !explicit && !info.implicit {
            return Err(Unloadable::UserOnly(name.to_owned()));
        }
        let directory = Path::new(&info.path).parent().unwrap_or(Path::new("/"));
        let body = now
            .body
            .replace("${CLAUDE_SKILL_DIR}", &directory.to_string_lossy());
        // Where the skill says its arguments go, or else after it.
        let placed = body.contains("$ARGUMENTS");
        let body = body.replace("$ARGUMENTS", arguments);
        let mut text = format!(
            "Use the {name} skill for this request.\nSource: {}\nSkill directory: {}\n\
             Resolve supporting files and scripts relative to this directory. \
             Existing tool permissions still apply.\n\n{body}",
            info.path,
            directory.display(),
        );
        if !placed && !arguments.is_empty() {
            let _ = write!(text, "\n\nUser arguments: {arguments}");
        }
        Ok(text)
    }
}

/// Whether `name` can be a skill's: at most 64 lowercase letters,
/// digits, hyphens, underscores or colons.
pub(crate) fn names_a_skill(name: &str) -> bool {
    let fits =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_:".contains(&byte);
    !name.is_empty() && name.len() <= 64 && name.bytes().all(fits)
}

/// Mixes `item` into `stamp`, a fingerprint of what's been found.
fn mix(stamp: &mut u64, item: impl std::hash::Hash) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    stamp.hash(&mut hasher);
    item.hash(&mut hasher);
    *stamp = hasher.finish();
}

/// `text` on one line, with nothing hidden in it, cut to `most`
/// characters: for names and the like, shown whole or not at all.
pub(crate) fn line(text: &str, most: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::server::unhidden(&text).chars().take(most).collect()
}

/// `text` on one line, with nothing hidden in it, in `most` characters at
/// most: one that's longer is cut at the end of a word, where there's one
/// in its last third, and ends in an ellipsis to say so.
pub(crate) fn brief(text: &str, most: usize) -> String {
    let whole = line(text, usize::MAX);
    if whole.chars().count() <= most {
        return whole;
    }
    let kept: String = whole.chars().take(most.saturating_sub(1)).collect();
    let cut = match kept.rfind(' ') {
        Some(at) if at >= kept.len() - kept.len() / 3 => &kept[..at],
        _ => &kept,
    };
    let cut = cut.trim_end_matches(|c: char| c.is_ascii_punctuation() || c == ' ');
    format!("{cut}…")
}

#[cfg(test)]
mod tests;
