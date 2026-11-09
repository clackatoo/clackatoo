//! `client/startup.toml`: the window's settings that are rarely changed, or are
//! needed before anything starts, such as the graphics API.
//!
//! The window reads it before starting its GPU and font threads, so it
//! must stay quick to read. Settings given on the command line or in the
//! environment take precedence. A mistake in the file never stops the
//! window opening: each bad setting is reported and left at its default.
//!
//! Its settings are the options declared in startup.toml's
//! [scope](crate::options::Scope::Startup), under their keys there. The
//! font and its size are options in `client/init.lua` too
//! (`client.opt.font` and `client.opt.fontsize`), which outdo this file's:
//! these are what the window starts from, before config has run.
//! `clackatoo config` and `:setpersist` both write it, keeping its
//! comments.

use std::path::{Path, PathBuf};

use anyhow::Context;
use toml_edit::{DocumentMut, Item};

use crate::GraphicsApi;
use crate::options::{self, ALL, Decimal, Info, Kind, Scope, Value};

/// The settings the file gives. `None` means it doesn't say.
#[derive(Debug, Default, PartialEq)]
pub struct ClientConfig {
    pub font: Option<String>,
    pub font_size: Option<f32>,
    pub renderer: Option<GraphicsApi>,
    /// Whether the window asks for a high priority: `true` for `high`.
    pub priority: Option<bool>,
}

/// Every setting: its key, and its option.
fn settings() -> impl Iterator<Item = (&'static str, &'static Info)> {
    ALL.iter().filter_map(|info| match info.scope {
        Scope::Startup(key) => Some((key, info)),
        Scope::Window | Scope::Daemon => None,
    })
}

/// clackatoo's config directory: `$XDG_CONFIG_HOME/clackatoo`, or
/// `~/.config/clackatoo`.
pub fn dir() -> Option<PathBuf> {
    clackatoo_daemon::config_dir()
}

/// Where the file is: `client/startup.toml` in the [config
/// directory](dir), beside the window's `init.lua`.
pub fn path() -> Option<PathBuf> {
    Some(dir()?.join("client/startup.toml"))
}

/// Reads the file, printing what's wrong with it, if anything, to stderr.
/// No file means no settings.
pub fn load() -> ClientConfig {
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
        let Some((_, info)) = settings().find(|&(key, _)| key == name) else {
            problems.push(format!(
                "there's no setting called `{name}`; the settings are {}",
                names()
            ));
            continue;
        };
        let value = match check(name, info, item) {
            Ok(value) => value,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        match (info.name, value) {
            ("font", Value::Text(font)) if font.is_empty() => {}
            ("font", Value::Text(font)) => config.font = Some(font),
            ("fontsize", Value::Decimal(size)) => config.font_size = Some(size.as_f32()),
            ("renderer", Value::Choice(renderer)) => {
                config.renderer = Some(match renderer {
                    "opengl" => GraphicsApi::OpenGl,
                    _ => GraphicsApi::Vulkan,
                });
            }
            ("priority", Value::Choice(priority)) => {
                config.priority = Some(priority == "high");
            }
            _ => unreachable!("every setting is handled"),
        }
    }
    (config, problems)
}

/// Checks that `item`, under `key`, is a value `info`'s option takes.
fn check(key: &str, info: &Info, item: &Item) -> Result<Value, String> {
    let value = match info.kind {
        Kind::Text => item.as_str().map(|text| Value::Text(text.to_owned())),
        Kind::Choice(words) => (item.as_str())
            .and_then(|word| words.iter().find(|&&choice| choice == word))
            .map(|&word| Value::Choice(word)),
        Kind::Decimal { min, max } => {
            #[allow(clippy::cast_precision_loss)]
            let number =
                (item.as_float()).or_else(|| item.as_integer().map(|number| number as f64));
            number
                .filter(|number| (f64::from(min)..=f64::from(max)).contains(number))
                .map(|number| {
                    #[allow(clippy::cast_possible_truncation)]
                    Value::Decimal(Decimal::nearest(number as f32, min, max))
                })
        }
        Kind::Bool => item.as_bool().map(Value::Bool),
        Kind::Number { .. } | Kind::List { .. } => None,
    };
    value
        .filter(|value| !matches!(value, Value::Text(text) if text.chars().any(char::is_control)))
        .ok_or_else(|| match info.kind {
            Kind::Decimal { min, max } => format!("`{key}` must be a number from {min} to {max}"),
            Kind::Choice(words) => format!(
                "`{key}` must be one of {}",
                (words.iter())
                    .map(|word| format!("\"{word}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => format!("`{key}` must be text in quotes, like {key} = \"…\""),
        })
}

/// The settings' names, for messages.
fn names() -> String {
    settings()
        .map(|(key, _)| key)
        .collect::<Vec<_>>()
        .join(", ")
}

fn setting(name: &str) -> anyhow::Result<&'static Info> {
    settings()
        .find(|&(key, _)| key == name)
        .map(|(_, info)| info)
        .with_context(|| {
            format!(
                "there's no setting called `{name}`; the settings are {}",
                names()
            )
        })
}

/// `value` as TOML writes it.
fn toml_value(value: &Value) -> toml_edit::Value {
    match value {
        Value::Bool(on) => (*on).into(),
        Value::Number(number) => i64::from(*number).into(),
        Value::Decimal(number) => {
            let shown = number.to_string();
            match shown.parse::<i64>() {
                Ok(whole) => whole.into(),
                Err(_) => number.as_f64().into(),
            }
        }
        Value::List(numbers) => (numbers.iter().map(|&number| i64::from(number))).collect(),
        Value::Choice(word) => (*word).into(),
        Value::Text(text) => text.as_str().into(),
    }
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
pub fn show_all(path: &Path) -> anyhow::Result<String> {
    use std::fmt::Write;
    let document = document(path)?;
    let mut out = format!("# {}\n", path.display());
    for (key, info) in settings() {
        match document.get(key) {
            Some(item) => writeln!(out, "{key} = {}", item.to_string().trim()),
            None => writeln!(
                out,
                "# {key} = {}  # the default",
                toml_value(&info.default_value())
            ),
        }
        .expect("writing to a String");
    }
    Ok(out)
}

/// `clackatoo config NAME`: the setting's value, without quotes, or its
/// default.
pub fn get(path: &Path, name: &str) -> anyhow::Result<String> {
    let info = setting(name)?;
    let bare = |item: &Item| match item.as_str() {
        Some(text) => text.to_owned(),
        None => item.to_string().trim().to_owned(),
    };
    Ok(match document(path)?.get(name) {
        Some(item) => bare(item),
        None => format!(
            "{} (the default)",
            toml_value(&info.default_value())
                .to_string()
                .trim()
                .trim_matches('"')
        ),
    })
}

/// `clackatoo config NAME VALUE`: sets it, keeping the rest of the file,
/// comments and all, as it was. Empty text is the default.
pub fn set(path: &Path, name: &str, text: &str) -> anyhow::Result<()> {
    let info = setting(name)?;
    let value = match info.kind {
        // Any number in the range, as the file can hold.
        Kind::Decimal { .. } => {
            let number: f64 =
                (text.parse()).map_err(|_| anyhow::anyhow!("`{name}` must be a number"))?;
            let item = Item::Value(number.into());
            check(name, info, &item).map_err(anyhow::Error::msg)?
        }
        _ => {
            let item = Item::Value(text.into());
            check(name, info, &item).map_err(anyhow::Error::msg)?
        }
    };
    persist(path, info, Some(value))
}

/// `clackatoo config --unset NAME`: back to the default.
pub fn unset(path: &Path, name: &str) -> anyhow::Result<()> {
    persist(path, setting(name)?, None)
}

/// Saves `info`'s option, which must be startup.toml's, as `value`, or
/// with none or empty text, takes it out of the file, keeping the rest of
/// it, comments and all, as it was.
pub(crate) fn persist(path: &Path, info: &Info, value: Option<Value>) -> anyhow::Result<()> {
    let Scope::Startup(key) = info.scope else {
        anyhow::bail!("{} isn't kept in startup.toml", info.name);
    };
    let mut document = document(path)?;
    let value = value.filter(|value| !matches!(value, Value::Text(text) if text.is_empty()));
    match value {
        Some(value) => document[key] = Item::Value(toml_value(&value)),
        None => {
            if document.remove(key).is_none() {
                return Ok(());
            }
        }
    }
    options::persist::replace(path, &document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_read_and_mistakes_reported_one_by_one() {
        let (config, problems) = parse(
            "# Mine\nfont = \"JetBrains Mono\"\nfont-size = 13\nrenderer = \"opengl\"\npriority = \"normal\"\n",
        );
        assert_eq!(
            config,
            ClientConfig {
                font: Some("JetBrains Mono".into()),
                font_size: Some(13.0),
                renderer: Some(GraphicsApi::OpenGl),
                priority: Some(false),
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

    // The file is the user's, so it's checked on anything at all here, as
    // the fuzzer checks the daemon's `agents.toml` (see `fuzz/`), which
    // can't reach into this program.
    proptest::proptest! {
        #[test]
        fn any_file_is_read_or_refused(text in file()) {
            let (config, _) = parse(&text);
            let size = config.font_size.map_or(12.0, f64::from);
            proptest::prop_assert!((4.0..=200.0).contains(&size));
            proptest::prop_assert!(config.font.is_none_or(|font| !font.is_empty()));
        }

        #[test]
        fn what_is_set_is_read_back(name in "font|font-size|renderer", text in "\\PC{0,20}") {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("startup.toml");
            if set(&path, &name, &text).is_ok() {
                let written = std::fs::read_to_string(&path).unwrap_or_default();
                let (config, problems) = parse(&written);
                proptest::prop_assert!(problems.is_empty(), "{:?}", problems);
                let read = match name.as_str() {
                    "font" => config.font.as_deref() == Some(text.as_str()).filter(|text| !text.is_empty()),
                    "font-size" => config.font_size.is_some(),
                    _ => config.renderer.is_some(),
                };
                proptest::prop_assert!(read, "{} = {:?} isn't read back", name, text);
            }
        }
    }

    /// Files shaped like the settings, or not.
    fn file() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        let name = prop_oneof![
            "font",
            "font-size",
            "renderer",
            "[a-z-]{1,8}",
            "\"[^\"]{0,4}\""
        ];
        let value = prop_oneof![
            "-?[0-9]{1,4}(\\.[0-9]{0,3})?",
            "(nan|inf|-inf|1e400|0x10|1_0)",
            "\"[^\"\\\\]{0,12}\"",
            "(true|\\[\\]|\\{\\}|\\[1, \"a\"\\])",
            "\\PC{0,6}",
        ];
        let line = prop_oneof![
            4 => (name, value).prop_map(|(name, value)| format!("{name} = {value}")),
            1 => "\\[[a-z.]{0,6}\\]",
            1 => "#\\PC{0,10}",
            1 => "\\PC{0,12}",
        ];
        prop::collection::vec(line, 0..6).prop_map(|lines| lines.join("\n"))
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

    #[test]
    fn options_are_saved_under_their_keys_as_the_file_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("startup.toml");
        std::fs::write(&path, "# Mine.\nrenderer = \"opengl\" # fast here\n").unwrap();
        let fsz = options::find("fsz").unwrap();
        persist(
            &path,
            fsz,
            Some(Value::Decimal(Decimal::exact(10.5).unwrap())),
        )
        .unwrap();
        persist(
            &path,
            options::find("font").unwrap(),
            Some(Value::Text("Iosevka".into())),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "# Mine.\nrenderer = \"opengl\" # fast here\nfont-size = 10.5\nfont = \"Iosevka\"\n"
        );
        assert_eq!(parse(&text).0.font_size, Some(10.5));
        persist(&path, fsz, Some(Value::Decimal(Decimal::whole(13)))).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("font-size = 13\n")
        );
        // Empty text is the default, which the file needn't say.
        persist(
            &path,
            options::find("font").unwrap(),
            Some(Value::Text(String::new())),
        )
        .unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("font ="));
        assert!(persist(&path, options::find("nu").unwrap(), None).is_err());
    }
}
