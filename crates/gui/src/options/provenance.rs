//! Where an option, or a binding, was last set: its default, as the window
//! opened, `settings.lua`, the user's own config, or the window as it ran;
//! the line that set it, from Luau's debug info; and whether the config
//! was loading then or had finished.
//!
//! It's worked out only when something is set, never when it's read, so
//! keeping it costs nothing on the paths that read options.

use std::path::Path;
use std::sync::Arc;

/// What set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// Nothing: it's as it starts.
    Default,
    /// The window, as it opened: startup.toml, or the command line.
    Startup,
    /// `client/settings.lua`, which clackatoo writes.
    Settings,
    /// The user's own config: `init.lua`, a module it requires, or a
    /// handler or binding of theirs.
    Config,
    /// The window as it ran: `:set`, `:setpersist`, or a key of its own.
    Runtime,
}

/// A line in a file of Lua.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) file: Arc<str>,
    pub(crate) line: u32,
}

impl Place {
    /// As the bar says it: `client/init.lua:3`, the file's path from the
    /// config directory `dir` if it's in it.
    pub(crate) fn said(&self, dir: Option<&Path>) -> String {
        let path = Path::new(&*self.file);
        let path = dir
            .and_then(|dir| path.strip_prefix(dir).ok())
            .unwrap_or(path);
        format!("{}:{}", path.display(), self.line)
    }
}

/// Who last set something, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetBy {
    pub(crate) source: Source,
    /// It was set while the config loaded, `settings.lua` then `init.lua`,
    /// rather than after, by `:set`, a handler or a key.
    pub(crate) loading: bool,
    /// The user's own line that set it: the outermost of their code's
    /// frames, a line of `init.lua` as it loads, or of the handler that
    /// ran.
    pub(crate) at: Option<Place>,
    /// Where it was really set, if that's elsewhere: a helper, in a module,
    /// that `at` called.
    pub(crate) inner: Option<Place>,
}

impl SetBy {
    /// Nothing set it.
    pub(crate) const DEFAULT: Self = Self::by(Source::Default);

    /// The window as it opened.
    pub(crate) const STARTUP: Self = Self::by(Source::Startup);

    /// The window, as it ran, as `:set` or one of its own keys sets
    /// options.
    pub(crate) const RUNTIME: Self = Self::by(Source::Runtime);

    /// Saved in its file, by the settings menu or `:setpersist`, as the
    /// config run again would set it.
    pub(crate) const SAVED: Self = Self {
        source: Source::Settings,
        loading: true,
        at: None,
        inner: None,
    };

    const fn by(source: Source) -> Self {
        Self {
            source,
            loading: false,
            at: None,
            inner: None,
        }
    }

    /// Whether it's the user's own config as it loaded, which outdoes what
    /// `:setpersist` would save: `settings.lua` runs first.
    pub(crate) fn config_loading(&self) -> bool {
        self.source == Source::Config && self.loading
    }

    /// Where the user's code set it, as the bar says it: `client/init.lua:3`,
    /// or `client/init.lua:3 (in lua/keys.lua:8)`.
    pub(crate) fn place(&self, dir: Option<&Path>) -> Option<String> {
        let at = self.at.as_ref()?.said(dir);
        Some(match &self.inner {
            Some(inner) => format!("{at} (in {})", inner.said(dir)),
            None => at,
        })
    }

    /// What set it, as `:verbose set` says it, files' paths from the config
    /// directory `dir`.
    pub(crate) fn said(&self, dir: Option<&Path>) -> String {
        let place = self.place(dir).unwrap_or_else(|| "its config".into());
        match (self.source, self.loading) {
            (Source::Default, _) => "as it starts".into(),
            (Source::Startup, _) => "as the window opened: startup.toml or the command line".into(),
            (Source::Settings, _) => format!("saved in {place}"),
            (Source::Config, true) => format!("set in {place}"),
            (Source::Config, false) => format!("set by {place}, after the config loaded"),
            (Source::Runtime, _) => "set with :set or a key".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_set_it_is_said_from_the_config_directory() {
        let place = |file: &str, line| {
            Some(Place {
                file: file.into(),
                line,
            })
        };
        let dir = Path::new("/home/me/.config/clackatoo");
        let by = SetBy {
            source: Source::Config,
            loading: true,
            at: place("/home/me/.config/clackatoo/client/init.lua", 3),
            inner: place("/home/me/.config/clackatoo/lua/keys.lua", 8),
        };
        assert_eq!(
            by.said(Some(dir)),
            "set in client/init.lua:3 (in lua/keys.lua:8)"
        );
        assert!(by.config_loading());
        let later = SetBy {
            loading: false,
            inner: None,
            ..by
        };
        assert_eq!(
            later.said(None),
            "set by /home/me/.config/clackatoo/client/init.lua:3, after the config loaded"
        );
        assert!(!later.config_loading());
        assert_eq!(SetBy::DEFAULT.said(Some(dir)), "as it starts");
        assert_eq!(SetBy::RUNTIME.said(Some(dir)), "set with :set or a key");
    }
}
