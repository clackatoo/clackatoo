//! Moving the daemon's old TOML settings, `daemon/agents.toml`,
//! `machine.toml` and `web.toml`, into `daemon/settings.lua`: once, as the
//! first build with the Luau config starts or upgrades, keeping every
//! value, and the old files beside it as `.bak`.
//!
//! Each key becomes the line that sets it, `daemon.GROUP.KEY = VALUE`,
//! with the comments it had. A line the config can't run, as a key that
//! was never a setting or a value out of its range, is written commented
//! out, with why: what's written always runs. The web app's password
//! goes to the state directory, not the config (see
//! [`super::PASSWORD`]).
//!
//! Once `settings.lua` is there, a TOML file that's there too is passed
//! over, and said so.

use std::path::{Path, PathBuf};

use super::literal::{self, Literal};
use super::lua::{self, Chunk};
use super::{Dirs, HEADER};
use crate::schema::Source;

/// The old files, in the order they're moved, with the table each key at
/// their top goes in, or `None` for those whose top keys are tables.
const FILES: [(&str, Option<&str>); 3] = [
    ("agents.toml", None),
    ("machine.toml", Some("machine")),
    ("web.toml", Some("web")),
];

/// A line of the file to be: what it sets, with the comments before it.
#[derive(Debug)]
struct Statement {
    comments: Vec<String>,
    code: String,
    /// Why it's commented out, if it is.
    out: Option<String>,
}

impl Statement {
    fn new(comments: Vec<String>, code: String) -> Self {
        Self {
            comments,
            code,
            out: None,
        }
    }

    fn left_out(comments: Vec<String>, code: String, why: String) -> Self {
        Self {
            comments,
            code,
            out: Some(why),
        }
    }
}

/// What the old files come to.
#[derive(Debug, Default)]
pub(crate) struct Converted {
    /// `settings.lua`'s text.
    pub(crate) text: String,
    /// The old files there, by their paths.
    pub(crate) files: Vec<PathBuf>,
    /// `web.toml`'s password's hash, if it had one.
    pub(crate) password: Option<String>,
    /// What was left out, and why, to log.
    pub(crate) left_out: Vec<String>,
}

/// Moves the old files into `settings.lua`, if they're there and it isn't,
/// saying what it did in `notes`: or if it couldn't be written, its text,
/// to run as it would have been. A TOML file there beside `settings.lua`
/// is said, and passed over.
pub(crate) fn once(dirs: &Dirs, notes: &mut Vec<String>) -> Option<String> {
    let settings = dirs.daemon.join("settings.lua");
    if settings.exists() {
        let stray: Vec<&str> = (FILES.iter())
            .map(|(name, _)| *name)
            .filter(|name| dirs.daemon.join(name).exists())
            .collect();
        if !stray.is_empty() {
            let said = format!(
                "the daemon passes over {}: its settings are in daemon/settings.lua and \
                 daemon/init.lua now",
                named(&stray)
            );
            log::warn!("{said}");
            notes.push(said);
        }
        return None;
    }
    let converted = convert(dirs)?;
    for why in &converted.left_out {
        log::warn!("daemon/settings.lua: left out {why}");
    }
    let names: Vec<String> = (converted.files.iter())
        .filter_map(|file| Some(file.file_name()?.to_string_lossy().into_owned()))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    if let Err(err) = super::write_private(&settings, &converted.text) {
        let said = format!(
            "{} couldn't be written, so {} were read as it would have said: {err}",
            settings.display(),
            named(&names)
        );
        log::error!("{said}");
        notes.push(said);
        return Some(converted.text);
    }
    if let (Some(hash), Some(state)) = (&converted.password, &dirs.state)
        && let Ok(None) = super::password(state)
        && let Err(err) = super::set_password(state, Some(hash))
    {
        log::error!(
            "the web app's password couldn't be moved to {}: {err}",
            state.display()
        );
    }
    for file in &converted.files {
        let kept = backup(file);
        match std::fs::rename(file, &kept) {
            Ok(()) => log::info!(
                "moved {} into {}, keeping it as {}",
                file.display(),
                settings.display(),
                kept.display()
            ),
            Err(err) => log::warn!("{} couldn't be kept as .bak: {err}", file.display()),
        }
    }
    notes.push(format!(
        "the daemon's settings moved from {} into daemon/settings.lua; the old files are kept \
         as .bak",
        named(&names)
    ));
    None
}

/// What `settings.lua` would be, moved from the old files, if they're
/// there and it isn't, writing nothing.
pub(crate) fn text(dirs: &Dirs) -> Option<String> {
    if dirs.daemon.join("settings.lua").exists() {
        return None;
    }
    Some(convert(dirs)?.text)
}

/// `names`, as a list: `a`, `a and b`, `a, b and c`, each in `daemon/`.
fn named(names: &[&str]) -> String {
    let names: Vec<String> = names.iter().map(|name| format!("daemon/{name}")).collect();
    match names.as_slice() {
        [] => String::new(),
        [only] => only.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// Where `file` is kept: `.bak` beside it, or with a number, so nothing
/// kept before is lost.
fn backup(file: &Path) -> PathBuf {
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    let mut kept = file.with_file_name(format!("{name}.bak"));
    let mut number = 2;
    while kept.exists() {
        kept = file.with_file_name(format!("{name}.bak.{number}"));
        number += 1;
    }
    kept
}

/// The old files in `dirs`, moved into `settings.lua`'s text: `None` if
/// none is there.
pub(crate) fn convert(dirs: &Dirs) -> Option<Converted> {
    let mut converted = Converted::default();
    let mut sections = Vec::new();
    for (name, group) in FILES {
        let path = dirs.daemon.join(name);
        let text = match lua::read(&path) {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            Err(why) => {
                converted.left_out.push(why.clone());
                converted.files.push(path);
                sections.push((name, vec![unread(&why)]));
                continue;
            }
        };
        converted.files.push(path);
        sections.push((
            name,
            statements(name, group, &text, &mut converted.password),
        ));
    }
    if converted.files.is_empty() {
        return None;
    }
    let names: Vec<&str> = sections.iter().map(|(name, _)| *name).collect();
    let mut head = format!(
        "{HEADER}\n-- Moved here from {} on {}, kept beside it as .bak.\n",
        named(&names),
        jiff::Zoned::now().date()
    );
    if converted.password.is_some() {
        head.push_str(
            "-- The web app's password is kept in the state directory, as web-password.\n",
        );
    }
    let path = dirs.daemon.join("settings.lua");
    converted.text = runnable(&path, &head, &mut sections, &mut converted.left_out);
    Some(converted)
}

/// `agents.toml`'s `text` alone, moved: for the fuzzer, which checks what
/// it comes to runs.
#[cfg(feature = "fuzzing")]
pub(crate) fn agents_text(text: &str) -> String {
    let mut sections = vec![(
        "agents.toml",
        statements("agents.toml", None, text, &mut None),
    )];
    runnable(
        Path::new("daemon/settings.lua"),
        HEADER,
        &mut sections,
        &mut Vec::new(),
    )
}

/// The file `sections` make after `head`, with each line the config
/// can't run commented out, saying why, in `left_out` too.
fn runnable(
    path: &Path,
    head: &str,
    sections: &mut [(&str, Vec<Statement>)],
    left_out: &mut Vec<String>,
) -> String {
    let count: usize = sections
        .iter()
        .map(|(_, statements)| statements.len())
        .sum();
    for _ in 0..=count {
        let (text, lines) = render(head, sections);
        let chunk = Chunk {
            path: path.to_owned(),
            text: text.clone(),
            source: Source::Settings,
        };
        let Err(failure) = lua::run(&[chunk], &[]) else {
            return text;
        };
        let line = (failure.at)
            .filter(|at| Path::new(&*at.file) == path)
            .map(|at| at.line);
        let wrong = line.and_then(|line| {
            (lines.iter())
                .find(|(_, range)| range.contains(&line))
                .map(|(at, _)| *at)
        });
        let Some((section, at)) = wrong else {
            // Nowhere to put it: said as the daemon runs it.
            return text;
        };
        let why = failure
            .said
            .split_once(": ")
            .filter(|(place, _)| place.starts_with(&*path.to_string_lossy()))
            .map_or(failure.said.as_str(), |(_, why)| why)
            .to_owned();
        let statement = &mut sections[section].1[at];
        left_out.push(format!("{}: {why}", statement.code));
        statement.out = Some(why);
    }
    render(head, sections).0
}

/// The file's text, and the lines each statement that runs is on, by
/// section and place in it.
#[allow(clippy::type_complexity, reason = "said above")]
fn render(
    head: &str,
    sections: &[(&str, Vec<Statement>)],
) -> (String, Vec<((usize, usize), std::ops::RangeInclusive<u32>)>) {
    let mut text = head.to_owned();
    let mut lines = Vec::new();
    for (index, (name, statements)) in sections.iter().enumerate() {
        text.push_str(&format!("\n-- From {name}:\n"));
        for (at, statement) in statements.iter().enumerate() {
            for comment in &statement.comments {
                text.push_str(comment);
                text.push('\n');
            }
            match &statement.out {
                // Comments alone.
                None if statement.code.is_empty() => {}
                None => {
                    let first = line_of(&text);
                    text.push_str(&statement.code);
                    text.push('\n');
                    lines.push(((index, at), first..=line_of(&text) - 1));
                }
                Some(why) => {
                    text.push_str(&format!("-- Left out: {why}\n"));
                    for line in statement.code.lines() {
                        text.push_str(&format!("-- {line}\n"));
                    }
                }
            }
        }
    }
    (text, lines)
}

/// The line, from 1, that the end of `text` is on.
fn line_of(text: &str) -> u32 {
    u32::try_from(text.matches('\n').count() + 1).unwrap_or(u32::MAX)
}

/// What's written for a file that couldn't be read.
fn unread(why: &str) -> Statement {
    Statement::left_out(Vec::new(), String::new(), why.to_owned())
}

/// The statements `text`, the file called `name`, comes to, its top keys
/// going in `group`, or for `agents.toml`, by its tables. A password is
/// taken out, into `password`.
fn statements(
    name: &str,
    group: Option<&str>,
    text: &str,
    password: &mut Option<String>,
) -> Vec<Statement> {
    let document = match text.parse::<toml_edit::DocumentMut>() {
        Ok(document) => document,
        Err(err) => {
            let mut why = format!("{name} isn't TOML, so nothing in it was moved");
            if let Some(first) = err.to_string().lines().next() {
                why.push_str(&format!(" ({first})"));
            }
            return vec![unread(&why)];
        }
    };
    let mut statements = Vec::new();
    let table = document.as_table();
    for (key, item) in table {
        let comments = before(table, key, item);
        match (group, key) {
            (Some("web"), "password") => {
                *password = item
                    .as_str()
                    .map(str::to_owned)
                    .filter(|hash| !hash.is_empty());
                statements.push(Statement::left_out(
                    comments,
                    String::new(),
                    "the password's hash, now kept in the state directory".to_owned(),
                ));
            }
            (Some(group), key) => {
                statements.push(set(comments, &format!("daemon.{group}"), key, item))
            }
            (None, "agents" | "profiles" | "subagents" | "upgrade") => {
                let Some(inner) = item.as_table_like() else {
                    statements.push(Statement::left_out(
                        comments,
                        format!("{key} = {}", item.to_string().trim()),
                        format!("{key} is a table"),
                    ));
                    continue;
                };
                statements.extend(
                    comments
                        .into_iter()
                        .map(|comment| Statement::new(vec![comment], String::new())),
                );
                for (inner_key, inner_item) in inner.iter() {
                    let comments = (inner.key(inner_key))
                        .map(|key| comments_in(key.leaf_decor()))
                        .unwrap_or_default()
                        .into_iter()
                        .chain(table_comments(inner_item))
                        .collect();
                    statements.push(set(
                        comments,
                        &format!("daemon.{key}"),
                        inner_key,
                        inner_item,
                    ));
                }
            }
            (None, key) => statements.push(Statement::left_out(
                comments,
                format!("{key} = {}", item.to_string().trim()),
                format!("{name} has no {key}"),
            )),
        }
    }
    statements
}

/// `key` in `table` set to `item`, as a statement: a table's own
/// comments, about its fields, go before it, as its literal has none.
fn set(mut comments: Vec<String>, table: &str, key: &str, item: &toml_edit::Item) -> Statement {
    if let Some(fields) = item.as_table() {
        for (name, field) in fields {
            comments.extend(
                fields
                    .key(name)
                    .map(|key| comments_in(key.leaf_decor()))
                    .unwrap_or_default(),
            );
            if let Some(after) = field.as_value().and_then(|value| suffix(value.decor())) {
                comments.push(format!("{after} ({name})"));
            }
        }
    }
    let path = literal::path(table, key);
    match literal_of(item) {
        Some(value) => {
            let before = path.len() + 3;
            let mut code = format!("{path} = {}", value.written(before));
            if let Some(after) = item.as_value().and_then(|value| suffix(value.decor())) {
                code.push_str(&format!(" {after}"));
            }
            Statement::new(comments, code)
        }
        None => Statement::left_out(
            comments,
            format!("{path} = {}", item.to_string().trim()),
            "a value Luau can't hold".to_owned(),
        ),
    }
}

/// `item` as a Luau value, if it has one.
fn literal_of(item: &toml_edit::Item) -> Option<Literal> {
    match item {
        toml_edit::Item::None => None,
        toml_edit::Item::Value(value) => value_literal(value),
        toml_edit::Item::Table(table) => Some(Literal::Table(
            (table.iter())
                .map(|(key, item)| Some((key.to_owned(), literal_of(item)?)))
                .collect::<Option<_>>()?,
        )),
        toml_edit::Item::ArrayOfTables(tables) => Some(Literal::List(
            (tables.iter())
                .map(|table| literal_of(&toml_edit::Item::Table(table.clone())))
                .collect::<Option<_>>()?,
        )),
    }
}

fn value_literal(value: &toml_edit::Value) -> Option<Literal> {
    use toml_edit::Value as V;
    Some(match value {
        V::String(text) => Literal::Text(text.value().clone()),
        V::Integer(number) => Literal::Number(number.value().to_string()),
        V::Float(number) if number.value().is_finite() => {
            Literal::Number(format!("{:?}", number.value()))
        }
        V::Float(_) => return None,
        V::Boolean(on) => Literal::Bool(*on.value()),
        V::Datetime(when) => Literal::Text(when.value().to_string()),
        V::Array(items) => Literal::List(items.iter().map(value_literal).collect::<Option<_>>()?),
        V::InlineTable(table) => Literal::Table(
            (table.iter())
                .map(|(key, value)| Some((key.to_owned(), value_literal(value)?)))
                .collect::<Option<_>>()?,
        ),
    })
}

/// The comments before `key`, `item` in `table`: its own, and if it's a
/// table, its header's.
fn before(table: &toml_edit::Table, key: &str, item: &toml_edit::Item) -> Vec<String> {
    (table.key(key))
        .map(|key| comments_in(key.leaf_decor()))
        .unwrap_or_default()
        .into_iter()
        .chain(table_comments(item))
        .collect()
}

/// The comments before a table's header, if `item` is a table.
fn table_comments(item: &toml_edit::Item) -> Vec<String> {
    item.as_table()
        .map(|table| comments_in(table.decor()))
        .unwrap_or_default()
}

/// The comment lines in what's before something, `#` as `--`.
fn comments_in(decor: &toml_edit::Decor) -> Vec<String> {
    let prefix = (decor.prefix())
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or_default();
    (prefix.lines())
        .map(str::trim)
        .filter_map(|line| line.strip_prefix('#'))
        .map(|comment| format!("--{comment}"))
        .collect()
}

/// The comment after a value, `#` as `--`.
fn suffix(decor: &toml_edit::Decor) -> Option<String> {
    let suffix = decor.suffix()?.as_str()?.trim();
    Some(format!("--{}", suffix.strip_prefix('#')?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs() -> (tempfile::TempDir, Dirs) {
        let temp = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            daemon: temp.path().join("config/clackatoo/daemon"),
            shared: temp.path().join("config/clackatoo/lua"),
            state: Some(temp.path().join("state")),
        };
        std::fs::create_dir_all(&dirs.daemon).unwrap();
        (temp, dirs)
    }

    fn write(dirs: &Dirs, name: &str, text: &str) {
        std::fs::write(dirs.daemon.join(name), text).unwrap();
    }

    /// Every value agents.toml set is set the same in settings.lua, with
    /// its comments, and the old file is kept as .bak.
    #[test]
    fn agents_toml_moves_whole() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "agents.toml",
            "# My agents.\n\
             [agents.opencode]\n\
             name = \"OpenCode\"\n\
             command = [\"opencode\", \"acp\"] # its ACP\n\
             mode = \"build\"\n\
             \n\
             [agents.claude]\n\
             command = [\"claude-agent-acp\"]\n\
             tasks = false\n\
             \n\
             [profiles.review]\n\
             about = \"Reviews a change\"\n\
             agent = \"codex\"\n\
             model = { claude = \"haiku\", codex = \"gpt-6-luna\" }\n\
             limits = \"read-only\"\n\
             \n\
             [subagents]\n\
             working = 8\n\
             default = 4\n\
             \n\
             [upgrade]\n\
             keep_agents = false\n",
        );
        let mut notes = Vec::new();
        assert_eq!(once(&dirs, &mut notes), None);
        assert!(notes[0].contains("daemon/agents.toml"), "{notes:?}");
        assert!(!dirs.daemon.join("agents.toml").exists());
        assert!(dirs.daemon.join("agents.toml.bak").exists());
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        assert!(text.starts_with(HEADER), "{text}");
        assert!(text.contains("-- My agents."), "{text}");
        assert!(text.contains("-- its ACP"), "{text}");
        let settings = crate::settings::load(&dirs).settings;
        let configured = settings.agents();
        assert_eq!(configured.agents.len(), 2);
        let opencode = &configured.agents[0];
        assert_eq!(opencode.name, "opencode");
        assert_eq!(opencode.title.as_deref(), Some("OpenCode"));
        assert_eq!(opencode.command, ["opencode", "acp"]);
        assert_eq!(opencode.starting_mode.as_deref(), Some("build"));
        assert_eq!(configured.agents[1].tasks, Some(false));
        let review = &configured.profiles[0];
        assert_eq!(review.agent.as_deref(), Some("codex"));
        assert_eq!(review.limits, Some(crate::profiles::Limits::ReadOnly));
        assert_eq!(
            review.models.as_deref(),
            Some(
                &[
                    ("claude".into(), "haiku".into()),
                    ("codex".into(), "gpt-6-luna".into())
                ][..]
            )
        );
        assert_eq!(configured.subagents.working, 8);
        assert_eq!(configured.subagents.default, 4);
        assert!(!configured.keep_agents);
        let by = settings.set_by("subagents.working");
        assert_eq!(by.source, Source::Settings);
        assert!(by.at.unwrap().file.ends_with("daemon/settings.lua"));
    }

    /// machine.toml's name and priority move.
    #[test]
    fn machine_toml_moves_whole() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "machine.toml",
            "name = \"jack-desktop\"\npriority = false\n",
        );
        once(&dirs, &mut Vec::new());
        assert!(dirs.daemon.join("machine.toml.bak").exists());
        let settings = crate::settings::load(&dirs).settings;
        assert_eq!(settings.machine_name().as_deref(), Some("jack-desktop"));
        assert!(!settings.priority());
    }

    /// web.toml moves, but for its password, which goes to the state
    /// directory, readable only by the user.
    #[test]
    fn web_toml_moves_and_its_password_goes_to_the_state_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_temp, dirs) = dirs();
        let hash = crate::web::hash_password("correct horse");
        write(
            &dirs,
            "web.toml",
            &format!(
                "mode = \"tailscale\"\nlogin = \"jack@github\"\npassword = \"{hash}\"\n\
                 expire_days = 30\nlock_after = 5\nask = false\n"
            ),
        );
        once(&dirs, &mut Vec::new());
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        assert!(!text.contains(&hash), "{text}");
        let state = dirs.state.as_ref().unwrap();
        assert_eq!(
            super::super::password(state).unwrap().as_deref(),
            Some(hash.as_str())
        );
        let mode = std::fs::metadata(state.join(super::super::PASSWORD))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let settings = crate::settings::load(&dirs).settings;
        let web = settings
            .web("/data".into(), super::super::password(state).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(web.login.as_deref(), Some("jack@github"));
        assert_eq!(web.password.as_deref(), Some(hash.as_str()));
        assert_eq!(web.lock_after, 5);
        assert!(!web.ask);
        assert_eq!(web.expire_after, std::time::Duration::from_hours(30 * 24));
    }

    /// What the config can't run is kept, commented out, saying why, and
    /// the rest still runs: settings.lua always runs.
    #[test]
    fn what_cant_run_is_commented_out_and_the_rest_moves() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "agents.toml",
            "[agents.broken]\nname = \"No command\"\n\n[agents.fine]\ncommand = [\"x\"]\n\n\
             [subagents]\nworking = 999\nhops = 3\nsurprise = 1\n",
        );
        write(&dirs, "web.toml", "mode = \"public\"\nlock_afer = 3\n");
        let mut notes = Vec::new();
        once(&dirs, &mut notes);
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        assert!(
            text.contains("-- Left out: daemon.agents.broken needs a command"),
            "{text}"
        );
        assert!(text.contains("-- daemon.subagents.working = 999"), "{text}");
        assert!(text.contains("-- daemon.web.lock_afer = 3"), "{text}");
        assert!(text.contains("-- daemon.web.mode = \"public\""), "{text}");
        let loaded = crate::settings::load(&dirs);
        assert_eq!(loaded.notes, Vec::<String>::new(), "it runs");
        let configured = loaded.settings.agents();
        assert_eq!(configured.agents.len(), 1);
        assert_eq!(configured.subagents.hops, 3);
        assert_eq!(configured.subagents.working, 16);
    }

    /// Once settings.lua is there, a TOML file is passed over, and said.
    #[test]
    fn a_toml_file_after_is_passed_over() {
        let (_temp, dirs) = dirs();
        write(&dirs, "settings.lua", "daemon.machine.name = \"lua\"\n");
        write(&dirs, "machine.toml", "name = \"toml\"\n");
        let loaded = crate::settings::load(&dirs);
        assert_eq!(loaded.settings.machine_name().as_deref(), Some("lua"));
        assert!(
            loaded.notes[0].contains("passes over daemon/machine.toml"),
            "{:?}",
            loaded.notes
        );
        assert!(dirs.daemon.join("machine.toml").exists(), "left as it is");
    }

    /// Nothing is moved, and nothing written, without the old files.
    #[test]
    fn nothing_moves_without_the_old_files() {
        let (_temp, dirs) = dirs();
        let loaded = crate::settings::load(&dirs);
        assert!(loaded.notes.is_empty());
        assert!(!dirs.daemon.join("settings.lua").exists());
        assert_eq!(loaded.settings, crate::settings::Settings::default());
    }

    /// A .bak kept before isn't lost.
    #[test]
    fn a_bak_already_there_is_kept() {
        let (_temp, dirs) = dirs();
        write(&dirs, "machine.toml.bak", "older");
        write(&dirs, "machine.toml", "name = \"x\"\n");
        once(&dirs, &mut Vec::new());
        assert_eq!(
            std::fs::read_to_string(dirs.daemon.join("machine.toml.bak")).unwrap(),
            "older"
        );
        assert!(dirs.daemon.join("machine.toml.bak.2").exists());
    }
}
