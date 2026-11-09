//! The options' values, and where each was set.
//!
//! An option has one value every window shares, as Vim's global options
//! have: `:set`, `:setpersist` and the settings menu change it. The store
//! is that value and its provenance, kept by option alone, never by
//! window. Today each window holds a store of its own, as it runs a Lua of
//! its own; once one Lua runs for every window (CLACKA-237 phase 1b) the
//! process holds one, and a window's `:setlocal` overrides sit beside it.

use super::provenance::{SetBy, Source};
use super::{ALL, Info, Options, Value};

/// The options, as they are and as they started, and what set each.
#[derive(Clone, Debug)]
pub(crate) struct Store {
    now: Options,
    /// As the window started them, from startup.toml and the command line,
    /// before config ran: what a reload puts them back to.
    start: Options,
    /// Those the command line set, which config doesn't change as it loads.
    fixed: Vec<&'static Info>,
    /// What last set each, by its place in [`ALL`]: none if nothing has
    /// since the window started.
    set_by: Vec<Option<SetBy>>,
    /// What the config set each to while it loaded, and where: what it's
    /// configured to, whatever's set after.
    configured: Vec<Option<(Value, SetBy)>>,
}

impl Default for Store {
    fn default() -> Self {
        Self::new(Options::default(), Vec::new())
    }
}

impl std::ops::Deref for Store {
    type Target = Options;

    fn deref(&self) -> &Options {
        &self.now
    }
}

/// Options changed as tests need them, without saying what set them.
#[cfg(test)]
impl std::ops::DerefMut for Store {
    fn deref_mut(&mut self) -> &mut Options {
        &mut self.now
    }
}

impl Store {
    /// The options as the window starts them, the command line having set
    /// those in `fixed`.
    pub(crate) fn new(start: Options, fixed: Vec<&'static Info>) -> Self {
        Self {
            now: start.clone(),
            start,
            fixed,
            set_by: vec![None; ALL.len()],
            configured: vec![None; ALL.len()],
        }
    }

    /// The options as the window started them.
    pub(crate) fn start(&self) -> &Options {
        &self.start
    }

    /// Whether the command line set `info`'s option, so config can't as it
    /// loads.
    pub(crate) fn fixed(&self, info: &Info) -> bool {
        self.fixed.iter().any(|fixed| std::ptr::eq(*fixed, info))
    }

    /// Sets `info`'s option to `value`, if it can hold it, noting `by` as
    /// what set it, whether or not that changed it. Returns whether it
    /// changed.
    pub(crate) fn set(
        &mut self,
        info: &'static Info,
        value: Value,
        by: SetBy,
    ) -> Result<bool, String> {
        let changed = self.now.set(info, value.clone())?;
        let index = info.index();
        if by.loading && matches!(by.source, Source::Settings | Source::Config) {
            self.configured[index] = Some((value, by.clone()));
        }
        self.set_by[index] = Some(by);
        Ok(changed)
    }

    /// What last set `info`'s option.
    pub(crate) fn set_by(&self, info: &Info) -> SetBy {
        match &self.set_by[info.index()] {
            Some(by) => by.clone(),
            None if self.start.get(info) != info.default_value() => SetBy::STARTUP,
            None => SetBy::DEFAULT,
        }
    }

    /// What the config set `info`'s option to while it loaded, and where,
    /// if it did.
    pub(crate) fn configured(&self, info: &Info) -> Option<&(Value, SetBy)> {
        self.configured[info.index()].as_ref()
    }

    /// Puts the options back as they started, as a config run again from
    /// the start does.
    pub(crate) fn reset(&mut self) {
        self.now = self.start.clone();
        self.set_by.fill(None);
        self.configured.fill(None);
    }
}

#[cfg(test)]
mod tests {
    use super::super::find;
    use super::super::provenance::Place;
    use super::*;

    #[test]
    fn what_set_each_option_is_kept_and_a_reload_forgets_it() {
        let start = Options {
            fontsize: super::super::Decimal::whole(14),
            ..Options::default()
        };
        let (nu, fsz) = (find("nu").unwrap(), find("fsz").unwrap());
        let mut store = Store::new(start, Vec::new());
        assert_eq!(store.set_by(nu), SetBy::DEFAULT);
        assert_eq!(store.set_by(fsz), SetBy::STARTUP);
        let init = SetBy {
            source: Source::Config,
            loading: true,
            at: Some(Place {
                file: "init.lua".into(),
                line: 2,
            }),
            inner: None,
        };
        assert_eq!(store.set(nu, Value::Bool(false), init.clone()), Ok(true));
        assert_eq!(store.set(nu, Value::Bool(true), SetBy::RUNTIME), Ok(true));
        assert!(store.number);
        assert_eq!(store.set_by(nu), SetBy::RUNTIME);
        // What config set as it loaded stays known past later changes.
        assert_eq!(store.configured(nu), Some(&(Value::Bool(false), init)));
        assert!(store.set(nu, Value::Number(1), SetBy::RUNTIME).is_err());
        store.reset();
        assert_eq!(store.set_by(nu), SetBy::DEFAULT);
        assert_eq!(store.configured(nu), None);
    }
}
