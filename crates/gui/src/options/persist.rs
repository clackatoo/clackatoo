//! Saving options where they're kept, as `:setpersist` asks: the window's
//! in `client/settings.lua`, startup.toml's in `client/startup.toml`.
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

use super::settings_lua::Model;
use super::{Info, Scope, Value};
use crate::lua::Wake;

/// What came of saving an option.
#[derive(Debug)]
pub(crate) struct Saved {
    pub(crate) info: &'static Info,
    /// What it was saved as; none if it was taken out.
    pub(crate) value: Option<Value>,
    /// The file it went in, from the config directory: `client/settings.lua`.
    pub(crate) file: &'static str,
    /// Whether it was, or why not.
    pub(crate) result: Result<(), String>,
}

/// One option to save.
struct Job {
    dir: PathBuf,
    info: &'static Info,
    value: Option<Value>,
    reply: Sender<Saved>,
    wake: Wake,
}

/// The file `info`'s option is kept in, from the config directory.
pub(crate) fn file(info: &Info) -> &'static str {
    match info.scope {
        Scope::Window => "client/settings.lua",
        Scope::Startup(_) => "client/startup.toml",
        Scope::Daemon => "the daemon's config",
    }
}

/// Saves `info`'s option as `value`, or with none, takes it out, in the
/// config directory `dir`, then sends what came of it on `reply` and calls
/// `wake`.
pub(crate) fn save(
    dir: &Path,
    info: &'static Info,
    value: Option<Value>,
    reply: Sender<Saved>,
    wake: Wake,
) {
    static WRITER: OnceLock<Option<Sender<Job>>> = OnceLock::new();
    let writer = WRITER.get_or_init(|| {
        let (send, receive) = mpsc::channel::<Job>();
        let spawned = std::thread::Builder::new()
            .name("settings".into())
            .spawn(move || {
                for job in receive {
                    let result = write(&job.dir, job.info, job.value.clone());
                    let saved = Saved {
                        info: job.info,
                        value: job.value,
                        file: file(job.info),
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
        info,
        value,
        reply,
        wake,
    };
    let refused = match writer {
        Some(writer) => writer.send(job).err().map(|failed| failed.0),
        None => Some(job),
    };
    if let Some(job) = refused {
        let saved = Saved {
            info,
            value: job.value,
            file: file(info),
            result: Err("there's no thread to save settings".into()),
        };
        if job.reply.send(saved).is_ok() {
            (job.wake)();
        }
    }
}

/// Saves `info`'s option in its file in `dir`, under a lock on the folder.
fn write(dir: &Path, info: &'static Info, value: Option<Value>) -> anyhow::Result<()> {
    let client = dir.join("client");
    std::fs::create_dir_all(&client)
        .with_context(|| format!("couldn't make {}", client.display()))?;
    let lock = std::fs::File::open(&client)
        .with_context(|| format!("couldn't open {}", client.display()))?;
    lock.lock()
        .with_context(|| format!("couldn't lock {}", client.display()))?;
    match info.scope {
        Scope::Window => settings_lua(&client.join("settings.lua"), info, value),
        Scope::Startup(_) => crate::startup::persist(&client.join("startup.toml"), info, value),
        Scope::Daemon => anyhow::bail!(
            "{} is the daemon's, which has no settings file yet",
            info.name
        ),
    }
}

/// Saves `info`'s option in `settings.lua` at `path`.
fn settings_lua(path: &Path, info: &Info, value: Option<Value>) -> anyhow::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", path.display())),
    };
    if text.is_none() && value.is_none() {
        return Ok(());
    }
    let mut model = Model::parse(text.as_deref().unwrap_or_default())
        .map_err(|why| anyhow::anyhow!("settings.lua's {why}, so it wasn't changed"))?;
    model.set(info, value);
    let rendered = model.render();
    if text.as_deref() == Some(rendered.as_str()) {
        return Ok(());
    }
    replace(path, &rendered)
}

/// Replaces the file at `path` with `text`, all at once, so whatever reads
/// it meanwhile reads either the old file or the new one.
pub(crate) fn replace(path: &Path, text: &str) -> anyhow::Result<()> {
    let dir = path.parent().context("the file is in a directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    let name = path.file_name().context("the file has a name")?;
    let partial = dir.join(format!(
        ".{}.{}.partial",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&partial, text)
        .with_context(|| format!("couldn't write {}", partial.display()))?;
    std::fs::rename(&partial, path).with_context(|| format!("couldn't write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::super::find;
    use super::*;

    /// Saves as the window does, and waits for it.
    fn saved(dir: &Path, name: &str, value: Option<Value>) -> Saved {
        let (reply, answers) = mpsc::channel();
        save(dir, find(name).unwrap(), value, reply, Box::new(|| {}));
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
    }
}
