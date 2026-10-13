//! `client/startup.toml`: the window's settings that are rarely changed, or are
//! needed before anything starts, such as the graphics API.
//!
//! The window reads it before starting its GPU and font threads, so it
//! must stay quick to read. Settings given on the command line or in the
//! environment take precedence. A mistake in the file never stops the
//! window opening: each bad setting is reported and left at its default.

use std::path::{Path, PathBuf};

use anyhow::Context;
use clackatoo_gui::GraphicsApi;
use toml_edit::{DocumentMut, Item, Value};

/// What a setting holds.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Text,
    /// A number from the first to the second, inclusive.
    Number(f64, f64),
    /// One of these words.
    Choice(&'static [&'static str]),
}

/// One setting in the file.
#[derive(Debug)]
struct Setting {
    name: &'static str,
    kind: Kind,
    /// What it's set to when the file doesn't say, as TOML writes it.
    default: &'static str,
}

/// Every setting.
const SETTINGS: [Setting; 3] = [
    Setting {
        name: "font",
        kind: Kind::Text,
        // fontconfig's name for the system's monospace font.
        default: "\"monospace\"",
    },
    Setting {
        name: "font-size",
        kind: Kind::Number(4.0, 200.0),
        default: "12",
    },
    Setting {
        name: "renderer",
        kind: Kind::Choice(&["vulkan", "opengl"]),
        default: "\"vulkan\"",
    },
];

/// The settings the file gives. `None` means it doesn't say.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ClientConfig {
    pub(crate) font: Option<String>,
    pub(crate) font_size: Option<f32>,
    pub(crate) renderer: Option<GraphicsApi>,
}

/// clackatoo's config directory: `$XDG_CONFIG_HOME/clackatoo`, or
/// `~/.config/clackatoo`.
pub(crate) fn dir() -> Option<PathBuf> {
    clackatoo_daemon::config_dir()
}

/// Where the file is: `client/startup.toml` in the [config
/// directory](dir), beside the window's `init.lua`.
pub(crate) fn path() -> Option<PathBuf> {
    Some(dir()?.join("client/startup.toml"))
}

/// Reads the file, printing what's wrong with it, if anything, to stderr.
/// No file means no settings.
pub(crate) fn load() -> ClientConfig {
    let Some(path) = path() else {
        return ClientConfig::default();
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return ClientConfig::default();
        }
        Err(err) => {
            eprintln!("clackatoo: couldn't read {}: {err}", path.display());
            return ClientConfig::default();
        }
    };
    let (config, problems) = parse(&text);
    for problem in problems {
        eprintln!("clackatoo: {}: {problem}", path.display());
    }
    config
}

/// The settings in `text`, and what's wrong with it.
fn parse(text: &str) -> (ClientConfig, Vec<String>) {
    let mut config = ClientConfig::default();
    let document = match text.parse::<DocumentMut>() {
        Ok(document) => document,
        Err(err) => return (config, vec![format!("{err}; using the defaults")]),
    };
    let mut problems = Vec::new();
    for (name, item) in document.iter() {
        let Some(setting) = SETTINGS.iter().find(|setting| setting.name == name) else {
            problems.push(format!(
                "there's no setting called `{name}`; the settings are {}",
                names()
            ));
            continue;
        };
        let value = match check(setting, item) {
            Ok(value) => value,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        match (setting.name, value) {
            ("font", Checked::Text(font)) => config.font = Some(font),
            #[allow(clippy::cast_possible_truncation)]
            ("font-size", Checked::Number(size)) => config.font_size = Some(size as f32),
            ("renderer", Checked::Text(renderer)) => {
                config.renderer = Some(match renderer.as_str() {
                    "opengl" => GraphicsApi::OpenGl,
                    _ => GraphicsApi::Vulkan,
                });
            }
            _ => unreachable!("every setting is handled"),
        }
    }
    (config, problems)
}

/// A setting's value, once checked.
enum Checked {
    Text(String),
    Number(f64),
}

/// Checks that `item` is a value `setting` takes.
fn check(setting: &Setting, item: &Item) -> Result<Checked, String> {
    let name = setting.name;
    match setting.kind {
        Kind::Text => match item.as_str() {
            Some(text) if !text.is_empty() => Ok(Checked::Text(text.to_owned())),
            _ => Err(format!(
                "`{name}` must be text in quotes, like {name} = \"…\""
            )),
        },
        Kind::Number(low, high) => {
            #[allow(clippy::cast_precision_loss)]
            let number = item
                .as_float()
                .or_else(|| item.as_integer().map(|number| number as f64));
            match number {
                Some(number) if (low..=high).contains(&number) => Ok(Checked::Number(number)),
                _ => Err(format!("`{name}` must be a number from {low} to {high}")),
            }
        }
        Kind::Choice(choices) => match item.as_str() {
            Some(choice) if choices.contains(&choice) => Ok(Checked::Text(choice.to_owned())),
            _ => Err(format!(
                "`{name}` must be one of {}",
                choices
                    .iter()
                    .map(|choice| format!("\"{choice}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
    }
}

/// The settings' names, for messages.
fn names() -> String {
    SETTINGS
        .iter()
        .map(|setting| setting.name)
        .collect::<Vec<_>>()
        .join(", ")
}

fn setting(name: &str) -> anyhow::Result<&'static Setting> {
    SETTINGS
        .iter()
        .find(|setting| setting.name == name)
        .with_context(|| {
            format!(
                "there's no setting called `{name}`; the settings are {}",
                names()
            )
        })
}

/// The file's text, or none if it doesn't exist.
fn read(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("couldn't read {}", path.display())),
    }
}

fn document(path: &Path) -> anyhow::Result<DocumentMut> {
    read(path)?
        .parse()
        .with_context(|| format!("{} isn't valid TOML", path.display()))
}

/// `clackatoo config`: every setting, as the file would hold it, with the
/// ones it doesn't set commented out with their defaults.
pub(crate) fn show_all(path: &Path) -> anyhow::Result<String> {
    use std::fmt::Write;
    let document = document(path)?;
    let mut out = format!("# {}\n", path.display());
    for setting in &SETTINGS {
        let name = setting.name;
        match document.get(name) {
            Some(item) => writeln!(out, "{name} = {}", item.to_string().trim()),
            None => writeln!(out, "# {name} = {}  # the default", setting.default),
        }
        .expect("writing to a String");
    }
    Ok(out)
}

/// `clackatoo config NAME`: the setting's value, without quotes, or its
/// default.
pub(crate) fn get(path: &Path, name: &str) -> anyhow::Result<String> {
    let setting = setting(name)?;
    let bare = |item: &Item| match item.as_str() {
        Some(text) => text.to_owned(),
        None => item.to_string().trim().to_owned(),
    };
    Ok(match document(path)?.get(name) {
        Some(item) => bare(item),
        None => format!("{} (the default)", setting.default.trim_matches('"')),
    })
}

/// `clackatoo config NAME VALUE`: sets it, keeping the rest of the file,
/// comments and all, as it was.
pub(crate) fn set(path: &Path, name: &str, text: &str) -> anyhow::Result<()> {
    let setting = setting(name)?;
    let value: Value = match setting.kind {
        Kind::Number(..) => match text.parse::<i64>() {
            Ok(integer) => integer.into(),
            Err(_) => text
                .parse::<f64>()
                .map_err(|_| anyhow::anyhow!("`{name}` must be a number"))?
                .into(),
        },
        Kind::Text | Kind::Choice(_) => text.into(),
    };
    let item = Item::Value(value);
    check(setting, &item).map_err(anyhow::Error::msg)?;
    let mut document = document(path)?;
    document[name] = item;
    write(path, &document.to_string())
}

/// `clackatoo config --unset NAME`: back to the default.
pub(crate) fn unset(path: &Path, name: &str) -> anyhow::Result<()> {
    setting(name)?;
    let mut document = document(path)?;
    if document.remove(name).is_some() {
        write(path, &document.to_string())?;
    }
    Ok(())
}

/// Replaces the file with `text`, all at once, so a window starting
/// meanwhile reads either the old file or the new one.
fn write(path: &Path, text: &str) -> anyhow::Result<()> {
    let dir = path.parent().context("the config file is in a directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let partial = path.with_extension("toml.partial");
    std::fs::write(&partial, text)
        .with_context(|| format!("couldn't write {}", partial.display()))?;
    std::fs::rename(&partial, path).with_context(|| format!("couldn't write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_read_and_mistakes_reported_one_by_one() {
        let (config, problems) =
            parse("# Mine\nfont = \"JetBrains Mono\"\nfont-size = 13\nrenderer = \"opengl\"\n");
        assert_eq!(
            config,
            ClientConfig {
                font: Some("JetBrains Mono".into()),
                font_size: Some(13.0),
                renderer: Some(GraphicsApi::OpenGl),
            }
        );
        assert!(problems.is_empty(), "{problems:?}");

        let (config, problems) =
            parse("font-size = \"big\"\nrenderer = \"metal\"\nfont = \"Iosevka\"\ncolour = 1\n");
        assert_eq!(
            config.font.as_deref(),
            Some("Iosevka"),
            "good settings still count"
        );
        assert_eq!((config.font_size, config.renderer), (None, None));
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("`font-size` must be a number from 4 to 200"));
        assert!(problems[1].contains(r#"`renderer` must be one of "vulkan", "opengl""#));
        assert!(problems[2].contains("no setting called `colour`"));
    }

    #[test]
    fn a_broken_file_means_the_defaults() {
        let (config, problems) = parse("font = \"unclosed\n");
        assert_eq!(config, ClientConfig::default());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn setting_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clackatoo/client/startup.toml");
        set(&path, "renderer", "opengl").unwrap();
        assert_eq!(get(&path, "renderer").unwrap(), "opengl");
        assert_eq!(get(&path, "font-size").unwrap(), "12 (the default)");

        std::fs::write(&path, "# My font.\nfont = \"Iosevka\"\n").unwrap();
        set(&path, "font-size", "13.5").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# My font.\nfont = \"Iosevka\"\nfont-size = 13.5\n"
        );
        assert!(set(&path, "renderer", "metal").is_err());
        assert!(set(&path, "colour", "red").is_err());
        assert!(set(&path, "font-size", "huge").is_err());

        unset(&path, "font").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "font-size = 13.5\n"
        );
        let all = show_all(&path).unwrap();
        assert!(all.contains("\nfont-size = 13.5\n"), "{all}");
        assert!(
            all.contains("\n# renderer = \"vulkan\"  # the default\n"),
            "{all}"
        );
    }
}
