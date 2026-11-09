//! Saving options where they're kept, as `:setpersist` and the settings
//! menu ask: the window's in `client/settings.lua`, startup.toml's in
//! `client/startup.toml`; and keys bound, as `:mappersist` and the menu
//! ask, in `client/settings.lua`.
//!
//! The files are written off the window's thread, by one thread for the
//! whole process, in the order they're asked for, each by reading it
//! again, changing the one option and replacing it whole, all at once.
//! The `client/` folder is locked meanwhile, so windows in other processes
//! can't save between the read and the write.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use anyhow::Context;
use clackatoo_daemon::settings::file;

use super::settings_lua::{Mapping, Model};
use super::{Info, Scope, Value};
use crate::lua::Wake;

/// What's to be saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// An option, as `value`, or with none, taken out.
    Option {
        info: &'static Info,
        value: Option<Value>,
    },
    /// Keys bound to keys to type, or if not `bound`, taken out.
    Keys { mapping: Mapping, bound: bool },
    /// Options kept in `settings.lua`, saved together, each as its value
    /// or taken out, in one write, as the settings menu saves a control
    /// that sets several.
    Options(Vec<(&'static Info, Option<Value>)>),
}

impl Change {
    /// The file it's kept in, from the config directory.
    pub(crate) fn file(&self) -> &'static str {
        match self {
            Self::Option { info, .. } => file(info),
            Self::Keys { .. } | Self::Options(_) => "client/settings.lua",
        }
    }
}

/// What came of saving a change.
#[derive(Debug)]
pub(crate) struct Saved {
    pub(crate) change: Change,
    /// The file it went in, from the config directory: `client/settings.lua`.
    pub(crate) file: &'static str,
    /// Whether it was, or why not.
    pub(crate) result: Result<(), String>,
}

/// One change to save.
struct Job {
    dir: PathBuf,
    change: Change,
    reply: Sender<Saved>,
    wake: Wake,
}

/// The file `info`'s option is kept in, from the config directory.
pub(crate) fn file(info: &Info) -> &'static str {
    match info.scope {
        Scope::Window => "client/settings.lua",
        Scope::Startup(_) => "client/startup.toml",
        Scope::Daemon => "daemon/settings.lua",
    }
}

/// Saves `change` in the config directory `dir`, then sends what came of
/// it on `reply` and calls `wake`.
pub(crate) fn save(dir: &Path, change: Change, reply: Sender<Saved>, wake: Wake) {
    static WRITER: OnceLock<Option<Sender<Job>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (send, receive) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("settings".into())
            .spawn(move || {
                for job in receive {
                    let result = write(&job.dir, &job.change);
                    let saved = Saved {
                        file: job.change.file(),
                        change: job.change,
                        result: result.map_err(|err| format!("{err:#}")),
                    };
                    if job.reply.send(saved).is_ok() {
                        (job.wake)();
                    }
                }
            });
        match spawned {
            Ok(_) => Some(send),
            Err(err) => {
                log::warn!("no thread to save settings: {err}");
                None
            }
        }
    });
    let job = Job {
        dir: dir.to_owned(),
        change,
        reply,
        wake,
    };
    let refused = match writer {
        Some(writer) => writer.send(job).err().map(|failed| failed.0),
        None => Some(job),
    };
    if let Some(job) = refused {
        let saved = Saved {
            file: job.change.file(),
            change: job.change,
            result: Err("there's no thread to save settings".into()),
        };
        if job.reply.send(saved).is_ok() {
            (job.wake)();
        }
    }
}

/// Saves `change` in its file in `dir`, under a lock on the folder.
fn write(dir: &Path, change: &Change) -> anyhow::Result<()> {
    let client = dir.join("client");
    let settings = client.join("settings.lua");
    let (info, value) = match change {
        Change::Option { info, value } => (*info, value.clone()),
        Change::Options(options) => {
            if let Some((info, _)) = (options.iter()).find(|(info, _)| info.scope != Scope::Window)
            {
                anyhow::bail!("{} isn't kept in settings.lua", info.name);
            }
            let adds = options.iter().any(|(_, value)| value.is_some());
            return locked(&client, || {
                settings_lua(&settings, adds, |model| {
                    (options.iter()).try_for_each(|(info, value)| model.set(info, value.clone()))
                })
            });
        }
        Change::Keys { mapping, bound } => {
            let changed = locked(&client, || {
                let mut changed = false;
                settings_lua(&settings, *bound, |model| {
                    changed = model.bind(mapping.clone(), *bound);
                    Ok(())
                })?;
                Ok(changed)
            })?;
            // Taking out what isn't there takes nothing out.
            if !*bound && !changed {
                anyhow::bail!(
                    "client/settings.lua doesn't bind {} in those modes",
                    crate::keymap::display(&mapping.keys)
                );
            }
            return Ok(());
        }
    };
    // Whoever asked, nothing an option can't hold is written.
    if let Some(value) = &value {
        info.check(value).map_err(anyhow::Error::msg)?;
    }
    locked(&client, || match info.scope {
        Scope::Window => settings_lua(&settings, value.is_some(), |model| model.set(info, value)),
        Scope::Startup(_) => crate::startup::persist(&client.join("startup.toml"), info, value),
        // Saved by the daemon, which may be on another machine.
        Scope::Daemon => anyhow::bail!("{} is the daemon's, which saves it itself", info.name),
    })
}

/// Runs `change` with the folder `client` locked, so whatever saves
/// settings there, in any process, `:setpersist` or `clackatoo config`,
/// can't save between another's read and its write.
pub(crate) fn locked<T>(
    client: &Path,
    change: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let _lock = file::lock(client).map_err(anyhow::Error::msg)?;
    change()
}

/// Changes `settings.lua` at `path` as `change` changes what it says,
/// which `adds` something, or only takes something out, which needs no
/// file made.
fn settings_lua(
    path: &Path,
    adds: bool,
    change: impl FnOnce(&mut Model) -> Result<(), String>,
) -> anyhow::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", path.display())),
    };
    if text.is_none() && !adds {
        return Ok(());
    }
    let mut model = Model::parse(text.as_deref().unwrap_or_default())
        .map_err(|why| anyhow::anyhow!("settings.lua's {why}, so it wasn't changed"))?;
    change(&mut model).map_err(anyhow::Error::msg)?;
    let rendered = model.render();
    if text.as_deref() == Some(rendered.as_str()) {
        return Ok(());
    }
    replace(path, &rendered, PRIVATE)?;
    // The windows go by what they saved already: it's no change to run the
    // config again for.
    crate::config_watch::wrote(path, text.as_deref(), &rendered);
    Ok(())
}

/// Who may read and write a new settings.lua: only its owner.
pub(crate) const PRIVATE: u32 = file::PRIVATE;

/// Replaces the file at `path` with `text`, whole, as the daemon replaces
/// its own settings (see [`file::replace`]).
pub(crate) fn replace(path: &Path, text: &str, mode: u32) -> anyhow::Result<()> {
    file::replace(path, text, mode).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::super::find;
    use super::*;

    /// Saves as the window does, and waits for it.
    fn saved(dir: &Path, name: &str, value: Option<Value>) -> Saved {
        let (reply, answers) = mpsc::channel();
        let info = find(name).unwrap();
        save(dir, Change::Option { info, value }, reply, Box::new(|| {}));
        answers
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("saved")
    }

    #[test]
    fn each_save_keeps_what_others_saved_and_a_file_not_ours_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client/settings.lua");
        // Nothing to take out of no file makes none.
        assert!(saved(dir.path(), "nu", None).result.is_ok());
        assert!(!path.exists());
        // Two windows, saving one after the other.
        assert!(
            saved(dir.path(), "nu", Some(Value::Bool(false)))
                .result
                .is_ok()
        );
        let wrap = saved(dir.path(), "wrap", Some(Value::Bool(false)));
        assert_eq!((wrap.file, wrap.result), ("client/settings.lua", Ok(())));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(super::super::settings_lua::HEADER));
        assert!(text.ends_with("client.opt.number = false\nclient.opt.wrap = false\n"));
        assert!(saved(dir.path(), "nu", None).result.is_ok());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .ends_with("\nclient.opt.wrap = false\n")
        );

        std::fs::write(&path, "client.opt.wrap = false\nprint('mine')\n").unwrap();
        let refused = saved(dir.path(), "nu", Some(Value::Bool(false)));
        assert_eq!(
            refused.result,
            Err("settings.lua's line 2 isn't one clackatoo wrote: print('mine'), so it wasn't changed".into())
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "client.opt.wrap = false\nprint('mine')\n"
        );
        // No file left half written.
        let names: Vec<_> = std::fs::read_dir(dir.path().join("client"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["settings.lua"]);
    }

    #[test]
    fn a_file_is_replaced_whole_keeping_its_mode_or_left_as_it_was() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert!(
            saved(dir.path(), "nu", Some(Value::Bool(false)))
                .result
                .is_ok()
        );
        let path = dir.path().join("client/settings.lua");
        assert_eq!(mode(&path), 0o600, "a new settings.lua is only its owner's");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(
            saved(dir.path(), "wrap", Some(Value::Bool(false)))
                .result
                .is_ok()
        );
        assert_eq!(mode(&path), 0o640);
        // One that can't be replaced, as a folder can't, is left as it is,
        // and nothing half written stays.
        let folder = dir.path().join("folder");
        std::fs::create_dir_all(folder.join("inside")).unwrap();
        assert!(replace(&folder, "text", PRIVATE).is_err());
        assert!(folder.join("inside").is_dir());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn a_link_stays_a_link_and_the_file_it_goes_to_is_saved() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles/clackatoo");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(dir.path().join("client")).unwrap();
        std::fs::write(dotfiles.join("settings.lua"), "-- mine\n").unwrap();
        let settings = dir.path().join("client/settings.lua");
        std::os::unix::fs::symlink("../dotfiles/clackatoo/settings.lua", &settings).unwrap();
        // A link to a file not there yet, as a fresh checkout has.
        let startup = dir.path().join("client/startup.toml");
        std::os::unix::fs::symlink(dotfiles.join("startup.toml"), &startup).unwrap();
        assert!(
            saved(dir.path(), "nu", Some(Value::Bool(false)))
                .result
                .is_ok()
        );
        assert!(
            saved(
                dir.path(),
                "fsz",
                Some(Value::Decimal(super::super::Decimal::whole(13)))
            )
            .result
            .is_ok()
        );
        for link in [&settings, &startup] {
            assert!(
                std::fs::symlink_metadata(link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        assert_eq!(
            std::fs::read_to_string(dotfiles.join("settings.lua")).unwrap(),
            "-- mine\nclient.opt.number = false\n"
        );
        assert_eq!(
            std::fs::read_to_string(dotfiles.join("startup.toml")).unwrap(),
            "font-size = 13\n"
        );
        // A folder the link goes to that can't be written in is named.
        std::fs::set_permissions(&dotfiles, std::fs::Permissions::from_mode(0o500)).unwrap();
        let refused = saved(dir.path(), "wrap", Some(Value::Bool(false))).result;
        std::fs::set_permissions(&dotfiles, std::fs::Permissions::from_mode(0o700)).unwrap();
        let refused = refused.unwrap_err();
        assert!(
            refused.starts_with(&format!(
                "couldn't write in {}, where {} links to",
                dotfiles.display(),
                settings.display()
            )),
            "{refused}"
        );
    }

    #[test]
    fn startup_options_go_to_startup_toml() {
        let dir = tempfile::tempdir().unwrap();
        let renderer = saved(dir.path(), "renderer", Some(Value::Choice("opengl")));
        assert_eq!(
            (renderer.file, renderer.result),
            ("client/startup.toml", Ok(()))
        );
        assert!(
            saved(
                dir.path(),
                "fsz",
                Some(Value::Decimal(super::super::Decimal::whole(14)))
            )
            .result
            .is_ok()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("client/startup.toml")).unwrap(),
            "renderer = \"opengl\"\nfont-size = 14\n"
        );
        assert!(!dir.path().join("client/settings.lua").exists());
        // What an option can't hold is never written, in either file.
        for (name, value) in [
            ("fsz", Value::Decimal(super::super::Decimal::whole(1))),
            ("font", Value::Text("Mono\"\nrenderer = \"opengl".into())),
            ("sbr", Value::Text("\n".into())),
        ] {
            assert!(
                saved(dir.path(), name, Some(value)).result.is_err(),
                "{name}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("client/startup.toml")).unwrap(),
            "renderer = \"opengl\"\nfont-size = 14\n"
        );
        assert!(!dir.path().join("client/settings.lua").exists());
    }
}
