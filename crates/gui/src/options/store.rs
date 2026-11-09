//! The options' values, and where each was set.
//!
//! An option has one value every window shares, as Vim's global options
//! have: `:set`, `:setpersist`, the settings menu and the config change it.
//! The process of windows holds one store (see [`crate::client`]), and
//! each window reads it through a [`View`] of its own.
//!
//! A few options, those whose schema says `local`, can be set for one
//! window alone, with `:setlocal`, over the value every window shares, as
//! Vim's window-local options can. So can the font a window was opened
//! with, `--font` and `--font-size`: what the command line set for it is
//! its own. Those values, and what set each, are kept here too, by window,
//! so every option's provenance has one home.

use crate::options::Window as _;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::provenance::{SetBy, Source};
use super::{ALL, Info, Options, Value};

/// The store every window of the process shares.
pub(crate) type Shared = Rc<RefCell<Store>>;

/// A window's own values, by option, as [`ALL`] lists them.
type Locals = Vec<Option<(Value, SetBy)>>;

/// The options, as they are and as they started, what set each, and the
/// values windows have of their own.
#[derive(Clone, Debug)]
pub(crate) struct Store {
    now: Options,
    /// As the process started them, from startup.toml, before config ran:
    /// what a reload puts them back to.
    start: Options,
    /// What last set each, by its place in [`ALL`]: none if nothing has
    /// since the process started.
    set_by: Vec<Option<SetBy>>,
    /// What the config set each to while it loaded, and where: what it's
    /// configured to, whatever's set after.
    configured: Vec<Option<(Value, SetBy)>>,
    /// Each window's own values, by its number: `:setlocal`'s, and the
    /// command line's.
    locals: HashMap<u32, Locals>,
    /// What the command line set for each window as it opened: what a
    /// reload, or `:set nu&`, puts back.
    opened: HashMap<u32, Locals>,
    /// How many times anything here has changed, so a window can tell
    /// whether it's up to date.
    changes: u64,
}

impl Default for Store {
    fn default() -> Self {
        Self::new(Options::default())
    }
}

impl std::ops::Deref for Store {
    type Target = Options;

    fn deref(&self) -> &Options {
        &self.now
    }
}

impl Store {
    /// The options as the process starts them.
    pub(crate) fn new(start: Options) -> Self {
        Self {
            now: start.clone(),
            start,
            set_by: vec![None; ALL.len()],
            configured: vec![None; ALL.len()],
            locals: HashMap::new(),
            opened: HashMap::new(),
            changes: 0,
        }
    }

    /// Makes it shared, for every window of the process.
    pub(crate) fn shared(self) -> Shared {
        Rc::new(RefCell::new(self))
    }

    /// How many times anything here has changed.
    pub(crate) fn changes(&self) -> u64 {
        self.changes
    }

    /// Sets `info`'s option to `value` for every window, if it can hold
    /// it, noting `by` as what set it, whether or not that changed it.
    /// Returns whether it changed.
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
        self.changes += 1;
        Ok(changed)
    }

    /// What last set `info`'s option for every window.
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

    /// Notes that `info`'s option was saved in its file as `value`, or
    /// taken out of it with none: it's configured so now, as the config
    /// run again would set it. Not if the user's own config sets it as it
    /// loads, which outdoes what's saved.
    pub(crate) fn saved(&mut self, info: &'static Info, value: Option<Value>) {
        let configured = &mut self.configured[info.index()];
        if configured
            .as_ref()
            .is_some_and(|(_, by)| by.config_loading())
        {
            return;
        }
        *configured = value.map(|value| (value, SetBy::SAVED));
        self.changes += 1;
    }

    /// What `info`'s option is configured to for every window, whatever's
    /// been set since: as the config set it as it loaded, or it's been
    /// saved since, or else as the process started it.
    pub(crate) fn configured_value(&self, info: &Info) -> Value {
        match self.configured(info) {
            Some((value, _)) => value.clone(),
            None => self.start.get(info),
        }
    }

    /// Puts `info`'s option back as the process started it, for every
    /// window, forgetting what set it since. Returns whether that changed
    /// it.
    pub(crate) fn restore(&mut self, info: &'static Info) -> bool {
        let start = self.start.get(info);
        self.set_by[info.index()] = None;
        self.changes += 1;
        self.now.set(info, start).unwrap_or(false)
    }

    /// Sets `info`'s option to `value` for `window` alone, noting `by` as
    /// what set it, if it can hold it. Only an option whose schema says
    /// `local` can be, but for what the command line sets as a window
    /// opens.
    pub(crate) fn set_local(
        &mut self,
        window: u32,
        info: &'static Info,
        value: Value,
        by: SetBy,
    ) -> Result<(), String> {
        if !info.local && by.source != Source::Startup {
            return Err(format!(
                "{} is the same in every window, so :setlocal can't set it: :set does",
                info.name
            ));
        }
        info.check(&value)?;
        let local = Some((value, by));
        if local
            .as_ref()
            .is_some_and(|(_, by)| by.source == Source::Startup)
        {
            let opened = (self.opened.entry(window)).or_insert_with(|| vec![None; ALL.len()]);
            opened[info.index()].clone_from(&local);
        }
        let locals = (self.locals.entry(window)).or_insert_with(|| vec![None; ALL.len()]);
        locals[info.index()] = local;
        self.changes += 1;
        Ok(())
    }

    /// Takes `window`'s own value of `info`'s option away, so it has every
    /// window's again. Returns whether it had one.
    pub(crate) fn drop_local(&mut self, window: u32, info: &Info) -> bool {
        let dropped = (self.locals.get_mut(&window))
            .and_then(|locals| locals[info.index()].take())
            .is_some();
        if dropped {
            self.changes += 1;
        }
        dropped
    }

    /// `window`'s own value of `info`'s option, and what set it, if it has
    /// one.
    pub(crate) fn local(&self, window: u32, info: &Info) -> Option<&(Value, SetBy)> {
        self.locals.get(&window)?[info.index()].as_ref()
    }

    /// The options as `window` has them: every window's, but for its own.
    pub(crate) fn of(&self, window: u32) -> Options {
        let mut options = self.now.clone();
        for (info, local) in ALL
            .iter()
            .zip(self.locals.get(&window).into_iter().flatten())
        {
            if let Some((value, _)) = local {
                // It was checked as it was set.
                let _ = options.set(info, value.clone());
            }
        }
        options
    }

    /// The options as `window` started them: as the process did, but for
    /// what its command line set.
    pub(crate) fn start_of(&self, window: u32) -> Options {
        let mut options = self.start.clone();
        for (info, opened) in ALL
            .iter()
            .zip(self.opened.get(&window).into_iter().flatten())
        {
            if let Some((value, _)) = opened {
                // It was checked as it was set.
                let _ = options.set(info, value.clone());
            }
        }
        options
    }

    /// What last set `info`'s option as `window` has it.
    pub(crate) fn set_by_in(&self, window: u32, info: &Info) -> SetBy {
        match self.local(window, info) {
            Some((_, by)) => by.clone(),
            None => self.set_by(info),
        }
    }

    /// Puts `window`'s own value of `info`'s option back as the window
    /// opened: what its command line set, or none.
    pub(crate) fn restore_local(&mut self, window: u32, info: &Info) {
        let opened = (self.opened.get(&window)).and_then(|opened| opened[info.index()].clone());
        if let Some(locals) = self.locals.get_mut(&window) {
            locals[info.index()] = opened;
        } else if opened.is_some() {
            let mut locals = vec![None; ALL.len()];
            locals[info.index()] = opened;
            self.locals.insert(window, locals);
        }
        self.changes += 1;
    }

    /// Forgets `window`'s own values, as it's closed.
    pub(crate) fn forget(&mut self, window: u32) {
        self.opened.remove(&window);
        if self.locals.remove(&window).is_some() {
            self.changes += 1;
        }
    }

    /// Puts the options back as they started, as a config run again from
    /// the start does, forgetting what set them, and what `:setlocal` set
    /// in each window: each window's own are what its command line set as
    /// it opened, if anything.
    pub(crate) fn reset(&mut self) {
        self.now = self.start.clone();
        self.set_by.fill(None);
        self.configured.fill(None);
        self.locals.clone_from(&self.opened);
        self.changes += 1;
    }
}

/// A window's view of the store it shares with the other windows: the
/// options as it has them, kept up to date as they change, and what set
/// each.
#[derive(Clone, Debug)]
pub(crate) struct View {
    shared: Shared,
    window: u32,
    /// The options as this window has them, as they were when it last
    /// looked.
    now: Options,
    /// What the store's [`changes`](Store::changes) were then.
    seen: u64,
}

impl Default for View {
    fn default() -> Self {
        Self::new(Store::default().shared(), 1)
    }
}

impl std::ops::Deref for View {
    type Target = Options;

    fn deref(&self) -> &Options {
        &self.now
    }
}

/// Options changed as tests need them, without saying what set them.
#[cfg(test)]
impl std::ops::DerefMut for View {
    fn deref_mut(&mut self) -> &mut Options {
        &mut self.now
    }
}

impl View {
    /// `window`'s view of `shared`.
    pub(crate) fn new(shared: Shared, window: u32) -> Self {
        let (now, seen) = {
            let store = shared.borrow();
            (store.of(window), store.changes())
        };
        Self {
            shared,
            window,
            now,
            seen,
        }
    }

    /// The store, shared with every window.
    pub(crate) fn shared(&self) -> Shared {
        self.shared.clone()
    }

    /// The window it's of.
    pub(crate) fn window(&self) -> u32 {
        self.window
    }

    /// The store, shared with every window.
    pub(crate) fn store(&self) -> std::cell::Ref<'_, Store> {
        self.shared.borrow()
    }

    /// Changes the store, then looks again. Returns what `change` did,
    /// and whether this window's options changed.
    pub(crate) fn change<R>(&mut self, change: impl FnOnce(&mut Store, u32) -> R) -> (R, bool) {
        let done = change(&mut self.shared.borrow_mut(), self.window);
        (done, self.follow())
    }

    /// Looks at the store again, if it's changed since this last did.
    /// Returns whether this window's options changed.
    pub(crate) fn follow(&mut self) -> bool {
        let store = self.shared.borrow();
        if store.changes() == self.seen {
            return false;
        }
        self.seen = store.changes();
        let now = store.of(self.window);
        drop(store);
        let changed = now != self.now;
        self.now = now;
        changed
    }

    /// What last set `info`'s option as this window has it.
    pub(crate) fn set_by(&self, info: &Info) -> SetBy {
        self.store().set_by_in(self.window, info)
    }

    /// The options as this window started them.
    pub(crate) fn start(&self) -> Options {
        self.store().start_of(self.window)
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
        let mut store = Store::new(start);
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
        // Put back as it started, nothing set it since; what config set as
        // it loaded is still known.
        store.set(nu, Value::Bool(false), SetBy::RUNTIME).unwrap();
        assert!(store.restore(nu));
        assert!(store.number);
        assert_eq!(store.set_by(nu), SetBy::DEFAULT);
        assert!(store.configured(nu).is_some());
        store.reset();
        assert_eq!(store.set_by(nu), SetBy::DEFAULT);
        assert_eq!(store.configured(nu), None);
    }

    #[test]
    fn a_window_has_its_own_values_over_every_windows() {
        let (nu, ts, fsz) = (
            find("nu").unwrap(),
            find("ts").unwrap(),
            find("fsz").unwrap(),
        );
        let shared = Store::default().shared();
        let (mut one, mut two) = (View::new(shared.clone(), 1), View::new(shared.clone(), 2));
        let ((), changed) = one.change(|store, window| {
            store
                .set_local(window, nu, Value::Bool(false), SetBy::RUNTIME)
                .unwrap();
        });
        assert!(changed && !one.number);
        assert!(!two.follow() && two.number, "the other window's is its own");
        // Only an option that's local can be set so, but for the command
        // line's.
        assert!(
            (shared.borrow_mut())
                .set_local(1, ts, Value::Number(8), SetBy::RUNTIME)
                .is_err()
        );
        let size = Value::Decimal(super::super::Decimal::whole(20));
        (shared.borrow_mut())
            .set_local(2, fsz, size.clone(), SetBy::STARTUP)
            .unwrap();
        // Every window's value changes for each, but not over their own.
        (shared.borrow_mut())
            .set(nu, Value::Bool(true), SetBy::RUNTIME)
            .unwrap();
        (shared.borrow_mut())
            .set(
                fsz,
                Value::Decimal(super::super::Decimal::whole(9)),
                SetBy::RUNTIME,
            )
            .unwrap();
        let whole = super::super::Decimal::whole;
        assert!(one.follow() && !one.number && one.fontsize == whole(9));
        assert!(two.follow() && two.number && two.fontsize == whole(20));
        assert_eq!(one.set_by(nu), SetBy::RUNTIME);
        assert_eq!(two.set_by(fsz), SetBy::STARTUP);
        // A reload forgets :setlocal's, but not the command line's, which
        // it puts back.
        assert!(shared.borrow_mut().drop_local(2, fsz));
        assert!(two.follow() && two.fontsize == whole(9));
        shared.borrow_mut().reset();
        assert!(one.follow() && one.number);
        assert!(two.follow() && two.fontsize == whole(20));
    }
}
