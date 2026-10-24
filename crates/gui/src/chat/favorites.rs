//! Model favorites are local to the client. Writes run off the UI thread and
//! merge one change under a file lock, so other windows keep their favorites.
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

type Favorites = BTreeSet<(String, String)>;

fn read(path: &Path) -> Favorites {
    let mut text = String::new();
    let Ok(file) = std::fs::File::open(path) else {
        return Favorites::new();
    };
    if file.take(64 * 1024).read_to_string(&mut text).is_err() {
        return Favorites::new();
    }
    serde_json::from_str(&text).unwrap_or_default()
}

type Key = (String, String);
struct Store {
    sender: std::sync::mpsc::Sender<(Key, bool)>,
    pending: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<Key, bool>>>,
}

fn store() -> Option<&'static Store> {
    static STORE: std::sync::OnceLock<Option<Store>> = std::sync::OnceLock::new();
    STORE
        .get_or_init(|| {
            let path = crate::tutor::state_dir()?.join("clackatoo/model-favorites.json");
            let (sender, receiver) = std::sync::mpsc::channel::<(Key, bool)>();
            let pending =
                std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new()));
            let writing = pending.clone();
            let worker = std::thread::Builder::new()
                .name("model-favorites".into())
                .spawn(move || {
                    for (key, selected) in receiver {
                        match change(&path, key.clone(), selected) {
                            Ok(()) => {
                                if let Ok(mut pending) = writing.lock()
                                    && pending.get(&key) == Some(&selected)
                                {
                                    pending.remove(&key);
                                }
                            }
                            Err(error) => log::warn!("could not save model favorite: {error}"),
                        }
                    }
                });
            match worker {
                Ok(_) => Some(Store { sender, pending }),
                Err(error) => {
                    log::warn!("could not start model favorites writer: {error}");
                    None
                }
            }
        })
        .as_ref()
}

pub(super) fn load() -> Favorites {
    if cfg!(test) {
        return Favorites::new();
    }
    let mut favorites = crate::tutor::state_dir().map_or_else(Favorites::new, |dir| {
        read(&dir.join("clackatoo/model-favorites.json"))
    });
    if let Some(store) = store()
        && let Ok(pending) = store.pending.lock()
    {
        for (key, selected) in pending.iter() {
            if *selected {
                favorites.insert(key.clone());
            } else {
                favorites.remove(key);
            }
        }
    }
    favorites
}

pub(super) fn save(key: Key, selected: bool) {
    if cfg!(test) {
        return;
    }
    if let Some(store) = store() {
        if let Ok(mut pending) = store.pending.lock() {
            pending.insert(key.clone(), selected);
        }
        if let Err(error) = store.sender.send((key, selected)) {
            log::warn!("could not save model favorite: {error}");
        }
    }
}

fn change(path: &Path, key: (String, String), selected: bool) -> anyhow::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("lock"))?;
    lock.lock()?;
    let mut favorites = read(path);
    if selected {
        favorites.insert(key);
    } else {
        favorites.remove(&key);
    }
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec(&favorites)?)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changes_merge_with_other_windows_and_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("favorites.json");
        let a = ("Claude Code".into(), "sonnet".into());
        let b = ("Codex".into(), "model".into());
        change(&path, a.clone(), true).unwrap();
        change(&path, b.clone(), true).unwrap();
        change(&path, a, false).unwrap();
        assert_eq!(read(&path), BTreeSet::from([b]));
    }
}
