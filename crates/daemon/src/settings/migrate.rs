//! Moving the daemon's old TOML settings, `daemon/agents.toml`,
//! `machine.toml` and `web.toml`, into `daemon/settings.lua`: once, as the
//! first build with the Luau config starts or upgrades, keeping every
//! value, and the old files beside it as `.bak`.
//!
//! Each key becomes the line that sets it, `daemon.GROUP.KEY = VALUE`,
//! with the comments it had. A line the config can't run, as a key that
//! was never a setting or a value out of its range, is written commented
//! out, with why: what's written always runs. A field an agent or a
//! profile never had is left out alone, as before, keeping the agent. Any
//! of the web app's left out keeps the web app off, as a `web.toml` that
//! went wrong did, until it's put right.
//!
//! Every comment written is a line comment of its own, `-- ` and text
//! with no line breaks or other control characters in it, so nothing from
//! the old files can end one, or open a block comment, and run.
//!
//! It goes in order, each step only once the one before has worked: the
//! web app's password to the state directory, never the config (see
//! [`super::PASSWORD`]); then `settings.lua`, once its text compiles; then
//! the old files to `.bak`, `web.toml`'s without its password. What
//! fails is said in the first window the user is at.
//!
//! Once `settings.lua` is there, a TOML file that's there too is passed
//! over, and said so.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::literal::{self, Literal};
use super::lua::{self, AGENT_FIELDS, Chunk, PROFILE_FIELDS};
use super::{Dirs, HEADER};
use crate::schema::Source;

/// The old files, in the order they're moved, with the table each key at
/// their top goes in, or `None` for those whose top keys are tables.
const FILES: [(&str, Option<&str>); 3] = [
    ("agents.toml", None),
    ("machine.toml", Some("machine")),
    ("web.toml", Some("web")),
];

/// The most lines left out said in a window: the rest are in the file.
const SAID: usize = 3;

/// A line of the file to be: what it sets, with the comments before it.
#[derive(Debug)]
struct Statement {
    /// Its comments' text, each written as line comments.
    comments: Vec<String>,
    code: String,
    /// The key at the file's top it's of.
    key: String,
    /// Why it's commented out, if it is.
    out: Option<String>,
}

impl Statement {
    fn new(comments: Vec<String>, key: &str, code: String) -> Self {
        Self {
            comments,
            code,
            key: key.to_owned(),
            out: None,
        }
    }

    fn left_out(comments: Vec<String>, key: &str, code: String, why: String) -> Self {
        Self {
            comments,
            code,
            key: key.to_owned(),
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
    /// What was left out, and why, a line each.
    pub(crate) left_out: Vec<String>,
}

/// Moves the old files into `settings.lua`, if they're there and it isn't,
/// saying what it did in `notes`: or if it couldn't, its text, to run as
/// it would have been, the old files left as they are. A TOML file there
/// beside `settings.lua` is said, and passed over.
pub(crate) fn once(dirs: &Dirs, notes: &mut Vec<String>) -> Option<String> {
    let settings = dirs.daemon.join("settings.lua");
    if settings.exists() {
        pass_over(dirs, notes);
        return None;
    }
    let converted = convert(dirs, lua::TIME)?;
    for why in &converted.left_out {
        log::warn!("daemon/settings.lua: left out {why}");
    }
    let names: Vec<String> = (converted.files.iter())
        .filter_map(|file| Some(file.file_name()?.to_string_lossy().into_owned()))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut failed = |what: String| {
        let said = format!(
            "{} weren't moved into daemon/settings.lua, so they're read as they would have \
             been, and kept: {what}",
            named(&names)
        );
        log::error!("{said}");
        notes.push(said);
    };
    // Never written unless it compiles, whatever was in the old files.
    if let Err(why) = lua::compiles(&converted.text) {
        let said = format!(
            "kept your TOML files, {}, as moving them came to something that doesn't \
             compile ({}); the daemon goes by its defaults until that's put right",
            named(&names),
            one_line(&why)
        );
        log::error!("{said}");
        notes.push(said);
        return None;
    }
    // The password first: with it nowhere, the old file keeps it.
    if let Some(hash) = &converted.password {
        let Some(state) = &dirs.state else {
            failed("there's no state directory for the web app's password".to_owned());
            return Some(converted.text);
        };
        match super::password(state) {
            // `clackatoo web password` set one since: it stands.
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(err) = super::set_password(state, Some(hash)) {
                    failed(format!("the web app's password couldn't be kept: {err}"));
                    return Some(converted.text);
                }
            }
            Err(why) => {
                failed(why);
                return Some(converted.text);
            }
        }
    }
    if let Err(err) = super::write_private(&settings, &converted.text) {
        failed(format!("{} couldn't be written: {err}", settings.display()));
        return Some(converted.text);
    }
    for file in &converted.files {
        let kept = backup(file);
        match keep(file, &kept) {
            Ok(()) => log::info!(
                "moved {} into {}, keeping it as {}",
                file.display(),
                settings.display(),
                kept.display()
            ),
            Err(err) => {
                let said = format!("{} couldn't be kept as .bak: {err}", file.display());
                log::warn!("{said}");
                notes.push(said);
            }
        }
    }
    let mut said = format!(
        "the daemon's settings moved from {} into daemon/settings.lua; the old files are kept \
         as .bak",
        named(&names)
    );
    if !converted.left_out.is_empty() {
        let shown: Vec<&str> = (converted.left_out.iter())
            .take(SAID)
            .map(String::as_str)
            .collect();
        let _ = write!(said, ". Left out: {}", shown.join("; "));
        if converted.left_out.len() > SAID {
            let _ = write!(
                said,
                ", and {} more, as settings.lua says",
                converted.left_out.len() - SAID
            );
        }
    }
    notes.push(said);
    None
}

/// Says, in `notes`, which of the old files are there beside
/// `settings.lua`, passed over.
fn pass_over(dirs: &Dirs, notes: &mut Vec<String>) {
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
}

/// Keeps `file` as `kept`: renamed, but for a `web.toml` with a password,
/// whose copy is written without it, so no hash stays in the config.
fn keep(file: &Path, kept: &Path) -> std::io::Result<()> {
    if file.file_name().is_some_and(|name| name == "web.toml")
        && let Ok(Some(text)) = lua::read(file)
        && let Ok(mut document) = text.parse::<toml_edit::DocumentMut>()
        && document.remove("password").is_some()
    {
        super::write_private(kept, &document.to_string())?;
        return std::fs::remove_file(file);
    }
    std::fs::rename(file, kept)
}

/// What `settings.lua` would be, moved from the old files, if they're
/// there and it isn't, writing nothing, each run of it to check it taking
/// `limit` at most.
pub(crate) fn text(dirs: &Dirs, limit: Duration) -> Option<String> {
    if dirs.daemon.join("settings.lua").exists() {
        return None;
    }
    Some(convert(dirs, limit)?.text)
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

/// The old files in `dirs`, moved into `settings.lua`'s text, each run of
/// it to check it taking `limit` at most: `None` if none is there.
pub(crate) fn convert(dirs: &Dirs, limit: Duration) -> Option<Converted> {
    let mut converted = Converted::default();
    let mut sections = Vec::new();
    for (name, group) in FILES {
        let path = dirs.daemon.join(name);
        let text = match lua::read(&path) {
            Ok(Some(text)) => text,
            Ok(None) => continue,
            Err(why) => {
                converted.left_out.push(one_line(&why));
                converted.files.push(path);
                sections.push((name, vec![unread(&why)]));
                continue;
            }
        };
        converted.files.push(path);
        let statements = statements(
            name,
            group,
            &text,
            &mut converted.password,
            &mut converted.left_out,
        );
        sections.push((name, statements));
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
    converted.text = runnable(&path, &head, &mut sections, &mut converted.left_out, limit);
    Some(converted)
}

/// `agents.toml`'s `text` alone, moved: for the fuzzer, which checks what
/// it comes to runs.
#[cfg(feature = "fuzzing")]
pub(crate) fn agents_text(text: &str) -> String {
    let mut left_out = Vec::new();
    let statements = statements("agents.toml", None, text, &mut None, &mut left_out);
    let mut sections = vec![("agents.toml", statements)];
    runnable(
        Path::new("daemon/settings.lua"),
        HEADER,
        &mut sections,
        &mut left_out,
        lua::TIME,
    )
}

/// The file `sections` make after `head`, with each line the config
/// can't run commented out, saying why, in `left_out` too; and with
/// any of `web.toml`'s left out, its mode too, so the web app stays off.
fn runnable(
    path: &Path,
    head: &str,
    sections: &mut [(&str, Vec<Statement>)],
    left_out: &mut Vec<String>,
    limit: Duration,
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
        let Err(failure) = lua::run(&[chunk], &[], limit) else {
            break;
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
            break;
        };
        let why = failure
            .said
            .split_once(": ")
            .filter(|(place, _)| place.starts_with(&*path.to_string_lossy()))
            .map_or(failure.said.as_str(), |(_, why)| why)
            .to_owned();
        let statement = &mut sections[section].1[at];
        left_out.push(one_line(&format!("{}: {why}", statement.code)));
        statement.out = Some(why);
    }
    keep_the_web_off(sections, left_out);
    render(head, sections).0
}

/// Leaves `web.toml`'s mode out too if any of its other lines was, so the
/// web app stays off until they're put right, as it did when `web.toml`
/// went wrong: never on with a default in place of what was meant.
fn keep_the_web_off(sections: &mut [(&str, Vec<Statement>)], left_out: &mut Vec<String>) {
    let Some((_, statements)) = sections.iter_mut().find(|(name, _)| *name == "web.toml") else {
        return;
    };
    let wrong: Vec<String> = (statements.iter())
        // The password moved isn't wrong: it has no code.
        .filter(|statement| {
            statement.out.is_some() && !(statement.key == "password" && statement.code.is_empty())
        })
        .map(|statement| statement.key.clone())
        .collect();
    if wrong.is_empty() {
        return;
    }
    if let Some(mode) =
        (statements.iter_mut()).find(|statement| statement.key == "mode" && statement.out.is_none())
    {
        let why = format!(
            "the web app stays off until daemon.web.{} {} put right",
            wrong.join(", daemon.web."),
            if wrong.len() == 1 { "is" } else { "are" }
        );
        left_out.push(one_line(&format!("{}: {why}", mode.code)));
        mode.out = Some(why);
    }
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
        text.push('\n');
        commented(&mut text, &format!("From {name}:"));
        for (at, statement) in statements.iter().enumerate() {
            for comment in &statement.comments {
                commented(&mut text, comment);
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
                    commented(&mut text, &format!("Left out: {why}"));
                    commented(&mut text, &statement.code);
                }
            }
        }
    }
    (text, lines)
}

/// Writes `comment` as line comments, `-- ` and a line of it each, with
/// every control character in it written out, as `\u{d}`: so it can't end
/// its line and run, nor open a block comment.
fn commented(text: &mut String, comment: &str) {
    if comment.is_empty() {
        return;
    }
    for line in comment.split('\n') {
        text.push_str("-- ");
        for ch in line.chars() {
            if ch.is_control() {
                let _ = write!(text, "\\u{{{:x}}}", u32::from(ch));
            } else {
                text.push(ch);
            }
        }
        text.push('\n');
    }
}

/// `text` on one line, for a window or the log: line breaks as spaces,
/// other control characters as `\u{…}`.
fn one_line(text: &str) -> String {
    let mut line = String::new();
    commented(&mut line, &text.replace('\n', " "));
    line.trim_start_matches("-- ").trim_end().to_owned()
}

/// The line, from 1, that the end of `text` is on.
fn line_of(text: &str) -> u32 {
    u32::try_from(text.matches('\n').count() + 1).unwrap_or(u32::MAX)
}

/// What's written for a file that couldn't be read.
fn unread(why: &str) -> Statement {
    Statement::left_out(Vec::new(), "", String::new(), why.to_owned())
}

/// The statements `text`, the file called `name`, comes to, its top keys
/// going in `group`, or for `agents.toml`, by its tables. A password is
/// taken out, into `password`; what's left out is said in `left_out`.
fn statements(
    name: &str,
    group: Option<&str>,
    text: &str,
    password: &mut Option<String>,
    left_out: &mut Vec<String>,
) -> Vec<Statement> {
    let document = match text.parse::<toml_edit::DocumentMut>() {
        Ok(document) => document,
        Err(err) => {
            let mut why = format!("{name} isn't TOML, so nothing in it was moved");
            if let Some(first) = err.to_string().lines().next() {
                let _ = write!(why, " ({first})");
            }
            left_out.push(one_line(&why));
            return vec![unread(&why)];
        }
    };
    let mut statements = Vec::new();
    let table = document.as_table();
    for (key, item) in table {
        let comments = before(table, key, item);
        match (group, key) {
            (Some("web"), "password") => {
                let Some(hash) = item.as_str() else {
                    // Wrong, so the web app stays off, as it did.
                    let code = format!("{key} = {}", item.to_string().trim());
                    let why = "the password is the hash `clackatoo web password` writes, a string";
                    left_out.push(one_line(&format!("{code}: {why}")));
                    statements.push(Statement::left_out(comments, key, code, why.to_owned()));
                    continue;
                };
                *password = Some(hash.to_owned()).filter(|hash| !hash.is_empty());
                statements.push(Statement::left_out(
                    comments,
                    key,
                    String::new(),
                    "the password's hash, now kept in the state directory".to_owned(),
                ));
            }
            // A table of its own, as `daemon.terminals`.
            (Some("machine"), "terminals") if item.is_table_like() => {
                let inner = item.as_table_like().expect("a table");
                statements.extend(
                    comments
                        .into_iter()
                        .map(|comment| Statement::new(vec![comment], key, String::new())),
                );
                for (inner_key, inner_item) in inner.iter() {
                    let comments = (inner.key(inner_key))
                        .map(|key| comments_in(key.leaf_decor()))
                        .unwrap_or_default();
                    statements.push(set(
                        comments,
                        key,
                        "daemon.terminals",
                        (inner_key, inner_item),
                        None,
                        left_out,
                    ));
                }
            }
            (Some(group), key) => {
                statements.push(set(
                    comments,
                    key,
                    &format!("daemon.{group}"),
                    (key, item),
                    None,
                    left_out,
                ));
            }
            (None, "agents" | "profiles" | "subagents" | "upgrade") => {
                let Some(inner) = item.as_table_like() else {
                    let code = format!("{key} = {}", item.to_string().trim());
                    let why = format!("{key} is a table");
                    left_out.push(one_line(&format!("{code}: {why}")));
                    statements.push(Statement::left_out(comments, key, code, why));
                    continue;
                };
                statements.extend(
                    comments
                        .into_iter()
                        .map(|comment| Statement::new(vec![comment], key, String::new())),
                );
                let fields = match key {
                    "agents" => Some(("an agent", AGENT_FIELDS)),
                    "profiles" => Some(("a profile", PROFILE_FIELDS)),
                    _ => None,
                };
                for (inner_key, inner_item) in inner.iter() {
                    let comments = (inner.key(inner_key))
                        .map(|key| comments_in(key.leaf_decor()))
                        .unwrap_or_default()
                        .into_iter()
                        .chain(table_comments(inner_item))
                        .collect();
                    statements.push(set(
                        comments,
                        key,
                        &format!("daemon.{key}"),
                        (inner_key, inner_item),
                        fields,
                        left_out,
                    ));
                }
            }
            (None, key) => {
                let code = format!("{key} = {}", item.to_string().trim());
                let why = format!("{name} has no {key}");
                left_out.push(one_line(&format!("{code}: {why}")));
                statements.push(Statement::left_out(comments, key, code, why));
            }
        }
    }
    statements
}

/// `key` in `table` set to `item`, as a statement, of `top`, the file's
/// key it's in: a table's own comments, about its fields, go before it,
/// as its literal has none. With `fields`, what a table of the kind named
/// may have, any other field is left out of it alone, saying so, as is a
/// field whose value Luau can't hold; what's left out is said in
/// `left_out` too.
fn set(
    mut comments: Vec<String>,
    top: &str,
    table: &str,
    (key, item): (&str, &toml_edit::Item),
    fields: Option<(&str, &[&str])>,
    left_out: &mut Vec<String>,
) -> Statement {
    let path = literal::path(table, key);
    let mut kept = None;
    if let Some(inner) = item.as_table_like() {
        let mut literal = Vec::new();
        for (name, field) in inner.iter() {
            comments.extend(
                (inner.key(name))
                    .map(|key| comments_in(key.leaf_decor()))
                    .unwrap_or_default(),
            );
            if let Some(after) = field.as_value().and_then(|value| suffix(value.decor())) {
                comments.push(format!("{after} ({name})"));
            }
            let why = match (fields, literal_of(field)) {
                (Some((kind, known)), _) if !known.contains(&name) => format!(
                    "{path}.{name} isn't something {kind} has: it has {}",
                    known.join(", ")
                ),
                (_, Some(value)) => {
                    literal.push((name.to_owned(), value));
                    continue;
                }
                (_, None) => format!("{path}.{name} is a value Luau can't hold"),
            };
            let code = format!("{name} = {}", field.to_string().trim());
            left_out.push(one_line(&format!("{code}: {why}")));
            comments.push(format!("Left out: {why}"));
            comments.push(code);
        }
        kept = Some(Literal::Table(literal));
    }
    let Some(value) = kept.or_else(|| literal_of(item)) else {
        let code = format!("{path} = {}", item.to_string().trim());
        let why = "a value Luau can't hold".to_owned();
        left_out.push(one_line(&format!("{code}: {why}")));
        return Statement::left_out(comments, top, code, why);
    };
    let before = path.len() + 3;
    let code = format!("{path} = {}", value.written(before));
    // On a line of its own, before: never after code.
    if let Some(after) = item.as_value().and_then(|value| suffix(value.decor())) {
        comments.push(after);
    }
    Statement::new(comments, top, code)
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

/// The text of the comments in what's before something, without their
/// `#`, and a space after it.
fn comments_in(decor: &toml_edit::Decor) -> Vec<String> {
    let prefix = (decor.prefix())
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or_default();
    (prefix.lines())
        .map(str::trim)
        .filter_map(|line| line.strip_prefix('#'))
        .map(|comment| comment.strip_prefix(' ').unwrap_or(comment).to_owned())
        .collect()
}

/// The text of the comment after a value, without its `#`.
fn suffix(decor: &toml_edit::Decor) -> Option<String> {
    let suffix = decor.suffix()?.as_str()?.trim();
    let comment = suffix.strip_prefix('#')?;
    Some(comment.strip_prefix(' ').unwrap_or(comment).to_owned())
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
        let settings = crate::settings::load(&dirs, crate::settings::When::Starting).settings;
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

    /// machine.toml's name, priority and terminals move.
    #[test]
    fn machine_toml_moves_whole() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "machine.toml",
            "name = \"jack-desktop\"\npriority = false\n\n\
             # How the terminals behave.\n[terminals]\nnotify_after = 30 # seconds\n\
             vs16 = false\nwide = 1\n",
        );
        let mut notes = Vec::new();
        once(&dirs, &mut notes);
        assert!(dirs.daemon.join("machine.toml.bak").exists());
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        assert!(text.contains("-- How the terminals behave."), "{text}");
        assert!(
            text.contains("daemon.terminals.notify_after = 30\n"),
            "{text}"
        );
        assert!(text.contains("-- daemon.terminals.wide = 1"), "{text}");
        assert!(notes[0].contains("daemon.terminals.wide = 1"), "{notes:?}");
        let settings = crate::settings::load(&dirs, crate::settings::When::Starting).settings;
        assert_eq!(settings.machine_name().as_deref(), Some("jack-desktop"));
        assert!(!settings.priority());
        assert!(!settings.on("terminals.vs16"));
        assert_eq!(settings.number("terminals.notify_after"), 30);
        // As machine.toml had it, a [terminals] that isn't a table moves
        // as anything wrong does: left out, saying so.
        let (_temp, dirs) = super::tests::dirs();
        write(&dirs, "machine.toml", "terminals = 3\n");
        once(&dirs, &mut Vec::new());
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        assert!(text.contains("-- daemon.machine.terminals = 3"), "{text}");
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
        let settings = crate::settings::load(&dirs, crate::settings::When::Starting).settings;
        let web = settings
            .web("/data".into(), super::super::password(state).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(web.login.as_deref(), Some("jack@github"));
        assert_eq!(web.password.as_deref(), Some(hash.as_str()));
        assert_eq!(web.lock_after, 5);
        assert!(!web.ask);
        assert_eq!(web.expire_after, std::time::Duration::from_hours(30 * 24));
        // No hash stays in the config folder, the .bak included.
        let kept = std::fs::read_to_string(dirs.daemon.join("web.toml.bak")).unwrap();
        assert!(!kept.contains(&hash), "{kept}");
        assert!(kept.contains("lock_after = 5"), "{kept}");
    }

    /// A key, or a comment, from the old files can never end its comment
    /// and run: every comment is lines of its own, with no line breaks or
    /// other control characters in them, and none opens a block comment.
    #[test]
    fn nothing_from_the_old_files_runs_as_code() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "machine.toml",
            "\"x\\ndaemon.machine.name = 'pwned'\" = 1\n\
             \"y\\rdaemon.machine.priority = false\" = 2\n\
             #[[ a comment opening a block\n\
             name = \"desk\" #]] closing it\n",
        );
        write(
            &dirs,
            "agents.toml",
            "\"z\\ndaemon.web.mode = 'public'\" = 3\n\
             [agents.\"a\\nb\"]\ncommand = [\"x\"]\n\"q\\ndaemon.web.mode = 'public'\" = 1\n",
        );
        let mut notes = Vec::new();
        once(&dirs, &mut notes);
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        for line in text.lines() {
            for injected in [
                "daemon.machine.name = 'pwned'",
                "daemon.machine.priority = false",
                "daemon.web.mode = 'public'",
            ] {
                assert!(
                    !line.trim_start().starts_with(injected),
                    "{line:?} in
{text}"
                );
            }
            assert!(!line.trim_start().starts_with("--["), "{line:?}");
            assert!(!line.contains('\r'), "{line:?}");
        }
        assert!(text.contains("-- [[ a comment opening a block"), "{text}");
        let settings = crate::settings::load(&dirs, crate::settings::When::Starting).settings;
        assert_eq!(settings.machine_name().as_deref(), Some("desk"));
        assert!(settings.priority());
        assert_eq!(
            settings.value("web.mode"),
            crate::schema::Value::Choice("off")
        );
        // The agent named with a line break is kept, by that name.
        assert_eq!(settings.agents().agents[0].name, "a\nb");
        for note in &notes {
            assert!(!note.contains('\n') && !note.contains('\r'), "{note:?}");
        }
    }

    /// A field an agent or profile never had is left out alone: the agent
    /// stays, as before the Luau config.
    #[test]
    fn an_unknown_field_leaves_out_only_itself() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "agents.toml",
            "[agents.mine]\ncommand = [\"mine-acp\"]\ncolour = \"red\"\n\n\
             [profiles.review]\nabout = \"x\"\nwhen = 3\n",
        );
        once(&dirs, &mut Vec::new());
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        assert!(
            text.contains("-- Left out: daemon.agents.mine.colour isn't something an agent has"),
            "{text}"
        );
        assert!(text.contains("-- colour = \"red\""), "{text}");
        let configured = crate::settings::load(&dirs, crate::settings::When::Starting)
            .settings
            .agents();
        assert_eq!(configured.agents[0].command, ["mine-acp"]);
        assert_eq!(configured.profiles[0].about.as_deref(), Some("x"));
    }

    /// Any of web.toml left out keeps the web app off until it's put
    /// right, never on with a default, and the window is told what.
    #[test]
    fn web_toml_that_cant_move_whole_keeps_the_web_app_off() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "web.toml",
            "mode = \"tailscale\"\nlogin = \"me@github\"\nlock_after = 0\n",
        );
        let mut notes = Vec::new();
        once(&dirs, &mut notes);
        let text = std::fs::read_to_string(dirs.daemon.join("settings.lua")).unwrap();
        assert!(
            text.contains("-- daemon.web.mode = \"tailscale\""),
            "{text}"
        );
        assert!(
            text.contains("the web app stays off until daemon.web.lock_after"),
            "{text}"
        );
        let settings = crate::settings::load(&dirs, crate::settings::When::Starting).settings;
        assert!(settings.web("/data".into(), None).unwrap().is_none());
        assert!(
            notes[0].contains("Left out: daemon.web.lock_after = 0"),
            "{notes:?}"
        );
    }

    /// A password that isn't a string is wrong: the web app stays off.
    #[test]
    fn a_password_that_isnt_a_string_keeps_the_web_app_off() {
        let (_temp, dirs) = dirs();
        write(&dirs, "web.toml", "mode = \"tailscale\"\npassword = 123\n");
        let mut notes = Vec::new();
        once(&dirs, &mut notes);
        let settings = crate::settings::load(&dirs, crate::settings::When::Starting).settings;
        assert_eq!(
            settings.value("web.mode"),
            crate::schema::Value::Choice("off")
        );
        assert!(notes[0].contains("password = 123"), "{notes:?}");
        assert_eq!(
            super::super::password(dirs.state.as_ref().unwrap()).unwrap(),
            None
        );
    }

    /// What's left out as the lines are made, a field an agent never had
    /// or a value Luau can't hold, is said in the window too; such a value
    /// leaves out only its field.
    #[test]
    fn what_is_left_out_as_the_lines_are_made_is_said() {
        let (_temp, dirs) = dirs();
        write(
            &dirs,
            "agents.toml",
            "surprise = 1\n[agents.mine]\ncommand = [\"mine-acp\"]\nmode = nan\ncolour = 1\n",
        );
        let mut notes = Vec::new();
        once(&dirs, &mut notes);
        let note = &notes[0];
        assert!(
            note.contains("surprise = 1: agents.toml has no surprise"),
            "{note}"
        );
        assert!(note.contains("mode = nan"), "{note}");
        assert!(
            note.contains("colour = 1: daemon.agents.mine.colour"),
            "{note}"
        );
        let agents = crate::settings::load(&dirs, crate::settings::When::Starting)
            .settings
            .agents()
            .agents;
        assert_eq!(agents[0].command, ["mine-acp"]);
        assert_eq!(agents[0].starting_mode, None);
    }

    /// The password first, then settings.lua, then the old files: when one
    /// can't, nothing after it is done, the old files stay, and it's said.
    #[test]
    fn a_step_that_fails_leaves_the_old_files_as_they_are() {
        let (temp, mut dirs) = dirs();
        let hash = crate::web::hash_password("correct horse");
        write(
            &dirs,
            "web.toml",
            &format!("mode = \"tailscale\"\npassword = \"{hash}\"\n"),
        );
        // A state directory that can't be made: a file is in its way.
        let blocked = temp.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        dirs.state = Some(blocked.join("state"));
        let mut notes = Vec::new();
        let ran = once(&dirs, &mut notes);
        assert!(ran.is_some(), "run as it would have been");
        assert!(!dirs.daemon.join("settings.lua").exists());
        assert!(dirs.daemon.join("web.toml").exists());
        assert!(!dirs.daemon.join("web.toml.bak").exists());
        assert!(notes[0].contains("weren't moved"), "{notes:?}");
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
        let loaded = crate::settings::load(&dirs, crate::settings::When::Starting);
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
        let loaded = crate::settings::load(&dirs, crate::settings::When::Starting);
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
        let loaded = crate::settings::load(&dirs, crate::settings::When::Starting);
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
