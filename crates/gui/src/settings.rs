//! The settings menu `:Settings` opens, in two modes. Advanced has every
//! option, and every key bound, to look at and change, in sections down
//! its side, as the docs list them. Easy has most options, on pages of
//! cards, each with a control drawn as such, in words for someone who
//! doesn't know the options' names (see [`easy`] and
//! [`crate::options::easy`]). One search finds settings in both, and the
//! mode used last is the one it opens in.
//!
//! Each option says its value, its default, what it does, and where its
//! value comes from, as `:verbose set` does. A change applies at once and
//! is saved where the option is kept, `client/settings.lua` or, for what a
//! window needs as it opens, `client/startup.toml`, as `:setpersist` saves
//! it. Each option's changes can be undone, one at a time, and an option
//! can be put back as it starts.
//!
//! An option the user's own config sets as it loads is locked: it outdoes
//! what the menu would save, so the menu says where it's set, and Enter
//! opens that line. An option changed since the config loaded, by `:set`
//! or a key, says what it's configured to and what it is now: changing it
//! here changes both. One this window has a value of its own for, as
//! `:setlocal` gives, says so, and `x` takes it away.
//!
//! The keys section lists every binding, and what bound it. Enter on one
//! records keys to do the same, saved in settings.lua as keys bound to the
//! binding's keys (see [`crate::options::settings_lua`]); keys the user's
//! own config binds are locked, as options are.
//!
//! Advanced is drawn as the finder is, over the panes (see
//! [`crate::finder::view`]), Easy as [`view`] lays it out; the menu is the
//! window's alone, as the finder is:
//! the options are the window's process's, so a phone, which talks to the
//! daemon, never sees them.
//!
//! | Key | Does |
//! |---|---|
//! | `j`, `k`; Down, Up; Ctrl+N, Ctrl+P | The next or previous row |
//! | `gg`, `G`; Home, End | The first row or the last |
//! | Tab, Shift+Tab; `}`, `{` | The next section or the one before |
//! | `/` | Starts typing, to search names, short names and what each does |
//! | Enter, Space | Changes the row: turns it on or off, the next choice, a value to type, a font to pick, keys to record; or opens the line a locked one is set in |
//! | `l`, `h`; Right, Left | The next or previous value: on or off, a choice, a number up or down |
//! | `r` | Puts it back as it starts |
//! | `u` | Undoes the row's last change |
//! | `x` | Takes away this window's own value |
//! | `d` | Takes out keys the menu saved |
//! | `o` | Opens the line that set it |
//! | Esc, `q` | Clears what's typed, or else closes the menu |
//! | `m` | Switches to Easy, or back |

mod easy;
pub(crate) mod view;

use crate::options::Window as _;
use std::collections::HashMap;
use std::path::Path;

use crate::desk::{Binding, Keys};
use crate::finder::rank::{Items, Ranking};
use crate::finder::view::{FinderView, PreviewView, Row, SideView, fit};
use crate::input::KeyPress;
use crate::keymap::{Chord, KeyMode, display};
use crate::options::provenance::{Place, Source};
use crate::options::settings_lua::Mapping;
use crate::options::{ALL, Case, Category, Info, Kind, Scope, SetBy, Store, Value};
use crate::picker::{self, Picker};
use crate::render::Ink;
use crate::shortcuts::bindings::Hint;
use crate::vim::Stroke;

/// What the menu needs to know of the window to show its rows and change
/// them: the options as every window shares them, and this one's own, the
/// keys bound, and the daemon's settings, if it's said them.
pub(crate) struct Facts<'a> {
    pub(crate) store: &'a Store,
    pub(crate) window: u32,
    pub(crate) keys: &'a Keys,
    /// clackatoo's config directory, which places are said from.
    pub(crate) dir: Option<&'a Path>,
    /// The keys are an agent's, through native window access, which can't
    /// save keys (see `Desk::automating`).
    pub(crate) automating: bool,
    /// The daemon's settings, as it last said them.
    pub(crate) daemon: Option<&'a DaemonFacts>,
}

/// The daemon's settings, as the window was last told them (see
/// [`clackatoo_protocol::DaemonSettings`]): those of its options this
/// build knows, each with what set it, and what the daemon goes by, if
/// that isn't it yet.
#[derive(Debug, Default)]
pub(crate) struct DaemonFacts {
    /// The daemon's machine, if it isn't this one: then the menu says the
    /// daemon's settings are that machine's.
    pub(crate) elsewhere: Option<String>,
    /// The daemon's config directory, on its machine, which its places are
    /// said from.
    dir: std::path::PathBuf,
    /// Why its config, as its files are now, didn't run, if it didn't.
    pub(crate) failure: Option<String>,
    options: Vec<DaemonValue>,
}

/// One of the daemon's options, as it said it.
#[derive(Debug)]
struct DaemonValue {
    info: &'static Info,
    /// As its config sets it.
    value: Value,
    /// What the daemon goes by, if it isn't that yet.
    running: Option<Value>,
    by: SetBy,
}

impl DaemonFacts {
    /// What `settings` say, of a daemon on another machine, named so, if
    /// `elsewhere`.
    pub(crate) fn new(settings: &clackatoo_protocol::DaemonSettings, elsewhere: bool) -> Self {
        use clackatoo_protocol::OptionSource;
        let place = |place: &clackatoo_protocol::OptionPlace| Place {
            file: place.file.as_str().into(),
            line: place.line,
        };
        let options = (settings.options.iter())
            .filter_map(|option| {
                // A newer daemon's, or one this build can't hold, isn't shown.
                let info = clackatoo_daemon::settings::options::find(&option.name)?;
                let value = info.read(&option.value).ok()?;
                let running = (option.running.as_deref()).and_then(|text| info.read(text).ok());
                let by = SetBy {
                    source: match option.by.source {
                        OptionSource::Default => Source::Default,
                        OptionSource::Settings => Source::Settings,
                        OptionSource::Config => Source::Config,
                    },
                    loading: option.by.source != OptionSource::Default,
                    at: option.by.at.as_ref().map(place),
                    inner: option.by.inner.as_ref().map(place),
                };
                Some(DaemonValue {
                    info,
                    value,
                    running,
                    by,
                })
            })
            .collect();
        Self {
            elsewhere: elsewhere.then(|| settings.machine.clone()),
            dir: settings.dir.clone().into(),
            failure: settings.failure.clone(),
            options,
        }
    }

    fn of(&self, info: &Info) -> Option<&DaemonValue> {
        self.options.iter().find(|each| each.info.name == info.name)
    }

    /// Whether the daemon said its option `info`.
    pub(crate) fn knows(&self, info: &Info) -> bool {
        self.of(info).is_some()
    }

    /// Counts on `info`'s option being saved as `value`, or with none, taken
    /// out, as the daemon is asked to: the menu shows it so at once, and
    /// steps from it, until the daemon says how it went.
    pub(crate) fn counting_on(&mut self, info: &Info, value: Option<Value>) {
        let Some(each) = self
            .options
            .iter_mut()
            .find(|each| each.info.name == info.name)
        else {
            return;
        };
        let in_force = each.running.take().unwrap_or_else(|| each.value.clone());
        each.by = if value.is_some() {
            SetBy::SAVED
        } else {
            SetBy::DEFAULT
        };
        each.value = value.unwrap_or_else(|| info.default_value());
        each.running = (info.restart && in_force != each.value).then_some(in_force);
    }
}

/// Whose the daemon's settings are, as the menu's header says, if they're
/// another machine's: `daemon on desk`.
pub(crate) fn daemon_elsewhere(facts: &Facts) -> Option<String> {
    let machine = facts.daemon?.elsewhere.as_ref()?;
    Some(format!("daemon on {machine}"))
}

/// The option called `name`, the window's or, written `group.name`, the
/// daemon's.
pub(crate) fn find(name: &str) -> Result<&'static Info, String> {
    if name.contains('.') {
        clackatoo_daemon::settings::options::find(name)
            .ok_or_else(|| format!("the daemon has no option ‘{name}’"))
    } else {
        crate::options::find(name)
    }
}

impl Facts<'_> {
    /// The daemon's option `info`, if it's one, as the daemon said it.
    fn daemons(&self, info: &Info) -> Option<&DaemonValue> {
        if info.scope != Scope::Daemon {
            return None;
        }
        self.daemon?.of(info)
    }

    /// `info`'s option as every window has it now; or the daemon's, as the
    /// daemon goes by it.
    fn get(&self, info: &Info) -> Value {
        match self.daemons(info) {
            Some(each) => each.running.clone().unwrap_or_else(|| each.value.clone()),
            None if info.scope == Scope::Daemon => info.default_value(),
            None => self.store.get(info),
        }
    }

    /// `info`'s option as it's configured, which the menu saves: what the
    /// config set as it loaded, or what's been saved since.
    fn configured_value(&self, info: &Info) -> Value {
        match self.daemons(info) {
            Some(each) => each.value.clone(),
            None if info.scope == Scope::Daemon => info.default_value(),
            None => self.store.configured_value(info),
        }
    }

    /// What `info`'s option is configured as, and what configured it, if
    /// anything did.
    fn configured(&self, info: &Info) -> Option<(Value, SetBy)> {
        if info.scope == Scope::Daemon {
            let each = self.daemons(info)?;
            return (each.by.source != Source::Default)
                .then(|| (each.value.clone(), each.by.clone()));
        }
        self.store.configured(info).cloned()
    }

    /// This window's own value of `info`'s option, if it has one.
    fn local(&self, info: &Info) -> Option<&(Value, SetBy)> {
        match info.scope {
            Scope::Daemon => None,
            Scope::Window | Scope::Startup(_) => self.store.local(self.window, info),
        }
    }

    /// What set `info`'s option as this window has it.
    fn set_by_in(&self, info: &Info) -> SetBy {
        match self.daemons(info) {
            Some(each) => each.by.clone(),
            None if info.scope == Scope::Daemon => SetBy::DEFAULT,
            None => self.store.set_by_in(self.window, info),
        }
    }

    /// What last set `info`'s option for every window.
    fn set_by(&self, info: &Info) -> SetBy {
        match self.daemons(info) {
            Some(each) => each.by.clone(),
            None if info.scope == Scope::Daemon => SetBy::DEFAULT,
            None => self.store.set_by(info),
        }
    }

    /// The config directory `info`'s places are said from: the daemon's,
    /// on its machine, for its options.
    fn dir_of(&self, info: &Info) -> Option<&Path> {
        match (info.scope, self.daemon) {
            (Scope::Daemon, Some(daemon)) => Some(&daemon.dir),
            _ => self.dir,
        }
    }

    /// `info`'s option as this window has it.
    fn shown(&self, info: &Info) -> Value {
        match self.local(info) {
            Some((value, _)) => value.clone(),
            None => self.get(info),
        }
    }

    /// `info`'s option as the menu shows and changes it: as this window
    /// has it, or for one read only as a window opens, as it's saved, for
    /// windows opened after.
    fn value(&self, info: &Info) -> Value {
        if info.restart {
            self.configured_value(info)
        } else {
            self.shown(info)
        }
    }

    /// Where the user's own config sets `info`'s option as it loads, if
    /// it does: then it's locked.
    fn locked(&self, info: &Info) -> Option<SetBy> {
        let (_, by) = self.configured(info)?;
        by.config_loading().then_some(by)
    }

    /// What `info`'s option has saved in its file, as far as the window
    /// knows: what settings.lua set as the config loaded, or what's been
    /// saved since; and for startup.toml's, a value the process started
    /// with that isn't the default.
    fn saved(&self, info: &Info) -> Option<Value> {
        match self.configured(info) {
            Some((value, by)) if by.source == Source::Settings => Some(value),
            Some(_) => None,
            None => {
                let start = self.configured_value(info);
                (matches!(info.scope, Scope::Startup(_)) && start != info.default_value())
                    .then_some(start)
            }
        }
    }

    /// Where a binding of `keys` in any of `modes` is set by the user's own
    /// config as it loads, if one is: then it's locked.
    fn keys_locked(&self, modes: &[KeyMode], keys: &[Chord]) -> Option<SetBy> {
        (modes.iter())
            .map(|&mode| self.keys.bound_by(mode, keys))
            .find(SetBy::config_loading)
    }

    /// What bound `bound`'s keys, if one thing bound them in every one of
    /// its modes.
    fn bound_by(&self, bound: &Bound) -> Option<SetBy> {
        let mut each = (bound.modes.iter()).map(|&mode| self.keys.bound_by(mode, &bound.keys));
        let first = each.next()?;
        each.all(|by| by == first).then_some(first)
    }

    /// `by`, as the menu says it.
    fn said(&self, by: &SetBy) -> String {
        by.said(self.dir)
    }

    /// Where `by` set `info`'s option, as the menu says it, or else "your
    /// config".
    fn place(&self, info: &Info, by: &SetBy) -> String {
        (by.place(self.dir_of(info))).unwrap_or_else(|| "your config".into())
    }
}

/// What a key did to the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing to show.
    Nothing,
    /// Something shown changed.
    Changed,
    /// It's to close.
    Close,
    /// Options are to be set, and saved, each as it says.
    Set(Vec<Set>),
    /// The menu changed to this mode, which it's to open in next time.
    Mode(Mode),
    /// This window's own value of `info`'s option is to go.
    ClearLocal(&'static Info),
    /// Keys are to be bound, and saved, or with `bound` false, put back as
    /// the window starts them and taken out of settings.lua, each in turn.
    Keys(Vec<(Mapping, bool)>),
    /// The line at `place` is to be opened in an editor, and the menu
    /// closed.
    Open(Place),
    /// The same, for a line of the daemon's config, on its machine.
    OpenDaemons(Place),
    /// The fonts installed are wanted, for the font picker: see
    /// [`SettingsMenu::fonts`].
    Fonts,
}

impl picker::Outcome for Outcome {
    const NOTHING: Self = Self::Nothing;
    const CHANGED: Self = Self::Changed;

    fn weight(&self) -> Option<u8> {
        match self {
            Self::Nothing => Some(0),
            Self::Changed => Some(1),
            _ => None,
        }
    }
}

/// Which of its two modes the menu shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Pages of cards, with controls, of the options most use.
    Easy,
    /// Every option and every key, as a list to search.
    Advanced,
}

impl Mode {
    /// How it's written in the file it's remembered in.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Advanced => "advanced",
        }
    }

    /// The mode `text` names, as [`Mode::word`] writes it.
    pub(crate) fn read(text: &str) -> Option<Self> {
        match text.trim() {
            "easy" => Some(Self::Easy),
            "advanced" => Some(Self::Advanced),
            _ => None,
        }
    }
}

/// An option to be set to `live` for every window, if it can be as the
/// window runs, and `saved` in its file, or with none, taken out of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Set {
    pub(crate) info: &'static Info,
    pub(crate) live: Value,
    pub(crate) saved: Option<Value>,
}

/// A section of the menu, down its side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Options(Category),
    Keys,
    /// The daemon's options, after the window's.
    Daemon(Category),
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::Keys => "Keys",
            Self::Daemon(Category::Agents) => "The daemon's agents",
            Self::Options(category) | Self::Daemon(category) => category.title(),
        }
    }
}

/// Keys bound in some modes, as the keys section lists them: those bound
/// alike in several modes are one row.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bound {
    modes: Vec<KeyMode>,
    keys: Vec<Chord>,
    binding: Binding,
    label: Option<String>,
    /// What bound them, the same in every one of its modes.
    by: SetBy,
}

impl Bound {
    /// What it does, as the panel of keys says it.
    fn does(&self) -> String {
        match (&self.label, &self.binding) {
            (Some(label), _) => label.clone(),
            (None, Binding::Keys(to)) => format!("types {}", display(to)),
            (None, Binding::Lua { .. }) => "runs a function of your config's".into(),
            (None, Binding::Window(_)) => "one of the window's own".into(),
        }
    }

    /// The keys another binding is to type to do what it does, if keys
    /// can: a function of the config's can't be typed, and keys bound to
    /// keys type those, past the bindings.
    fn typed(&self) -> Option<Vec<Chord>> {
        match &self.binding {
            Binding::Window(_) => Some(self.keys.clone()),
            Binding::Keys(to) => Some(to.clone()),
            Binding::Lua { .. } => None,
        }
    }

    /// What it's known by in the menu's history.
    fn name(&self) -> String {
        format!("{} {}", KeyMode::letters(&self.modes), display(&self.keys))
    }
}

/// A row of the menu.
#[derive(Clone, Debug)]
enum Entry {
    Option(&'static Info),
    Keys(Bound),
}

impl Entry {
    fn section(&self) -> Section {
        match self {
            Self::Option(info) if info.scope == Scope::Daemon => Section::Daemon(info.category),
            Self::Option(info) => Section::Options(info.category),
            Self::Keys(_) => Section::Keys,
        }
    }
}

/// A change the menu made to a row, and what undoing it does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Undo {
    /// Sets the options as they were again, as Easy changes several
    /// together.
    Options(Vec<Set>),
    /// Binds keys again, or takes them out, as each says, in turn.
    Keys(Vec<(Mapping, bool)>),
}

/// The changes the menu has made, to undo: kept by the window while the
/// menu's closed, so it can be opened again to undo them. Each is noted,
/// numbered, under the name of everything it changed, an option's or
/// keys', newest last, so a change Easy made to two options together is
/// undone from either of their rows, in either mode, and from Easy's.
#[derive(Debug, Default)]
pub(crate) struct History {
    by_name: HashMap<String, Vec<(u64, Undo)>>,
    /// How many changes have been noted, which numbers them.
    count: u64,
}

impl History {
    /// Notes how to undo a change of what `names` name.
    fn push(&mut self, names: impl IntoIterator<Item = String>, undo: &Undo) {
        self.count += 1;
        for name in names {
            let changes = self.by_name.entry(name).or_default();
            changes.push((self.count, undo.clone()));
        }
    }

    /// The newest change noted under any of `names`: its number, and how
    /// to undo it.
    fn newest(&self, names: &[String]) -> Option<(u64, &Undo)> {
        (names.iter())
            .filter_map(|name| self.by_name.get(name)?.last())
            .max_by_key(|(number, _)| *number)
            .map(|(number, undo)| (*number, undo))
    }

    /// Takes change `number` out from under every name it's noted under.
    fn remove(&mut self, number: u64) {
        for changes in self.by_name.values_mut() {
            changes.retain(|(each, _)| *each != number);
        }
        self.by_name.retain(|_, changes| !changes.is_empty());
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// What's being changed in a row, while it is.
#[derive(Debug)]
enum Editing {
    /// A value being typed for an option, as `:set` takes it after `=`.
    Value { info: &'static Info, text: String },
    /// A font being picked from those installed.
    Font(FontPicker),
    /// Keys being recorded, to do what a binding does.
    Keys { bound: Bound, keys: Vec<Chord> },
}

/// The settings menu's state.
#[derive(Debug)]
pub(crate) struct SettingsMenu {
    /// What's typed, the row picked and the first shown.
    picker: Picker,
    /// Letters type, as after `/`, rather than act.
    typing: bool,
    /// `g` was typed, waiting for a second `g`.
    g: bool,
    /// Each row's text, which what's typed is matched against.
    items: Items,
    entries: Vec<Entry>,
    ranking: Ranking,
    editing: Option<Editing>,
    /// What to say in place of the count, until the next key, and whether
    /// it's a warning.
    said: Option<(String, bool)>,
    /// How wide an option's name is drawn, the widest's.
    name_cols: usize,
    /// The bindings' changes counted when the rows were made, to make
    /// them again once they change.
    keys_seen: u64,
    pub(crate) mode: Mode,
    /// Where Easy is, and what it's searched by.
    easy: easy::Easy,
    /// What was saved last, or why not, for Easy's foot.
    said_saved: Option<(String, bool)>,
    /// The agents the daemon knows, for Easy's cards: each one's name,
    /// what to show, and whether it's installed.
    agents: Vec<(String, String, bool)>,
    /// The daemon's settings are listed, as it's said them.
    daemon: bool,
}

/// Why an agent's keys don't change keys, as the window says too.
const AGENTS_SAVE_NO_KEYS: &str = "an agent's input can't save keys";

/// What the font picker lists for the system's monospace font, first.
const SYSTEM_FONT: &str = "the system's monospace font";

/// When a change to one of the daemon's options it reads only as it
/// starts takes effect.
pub(crate) const RESTARTS: &str = "takes effect at the daemon's next upgrade or restart";

impl SettingsMenu {
    /// The menu, its rows made from `facts`, with `query` typed, matching
    /// case as `case` says; the bindings' changes counted `keys_seen`.
    pub(crate) fn new(query: &str, case: Case, mode: Mode, facts: &Facts, keys_seen: u64) -> Self {
        let mut menu = Self {
            picker: Picker::new(String::new()),
            typing: false,
            g: false,
            items: Items::default(),
            entries: Vec::new(),
            ranking: Ranking::new(case, false),
            editing: None,
            said: None,
            name_cols: ALL.iter().map(|info| info.name.len()).max().unwrap_or(0),
            keys_seen,
            mode,
            easy: easy::Easy::new(case),
            said_saved: None,
            agents: Vec::new(),
            daemon: false,
        };
        menu.fill(facts);
        if !query.is_empty() {
            let _ = menu.picker.paste(query);
            menu.retyped();
        }
        menu
    }

    /// Makes the rows again, as the bindings have changed since they were
    /// made, or the daemon has said its settings, keeping the row picked
    /// where it can.
    pub(crate) fn follow_keys(&mut self, facts: &Facts, keys_seen: u64) {
        if keys_seen == self.keys_seen && facts.daemon.is_some() == self.daemon {
            return;
        }
        self.keys_seen = keys_seen;
        let picked = self.picked_name();
        self.fill(facts);
        self.ranking.set_query(self.picker.query(), &self.items);
        if let Some(rank) = picked.and_then(|name| {
            (0..self.ranking.len()).find(|&rank| self.name_at(rank) == Some(name.clone()))
        }) {
            self.picker.set_selected(rank);
        }
        self.picker.clamp(self.ranking.len());
    }

    /// Lists every row, in its section's order.
    fn fill(&mut self, facts: &Facts) {
        self.items.clear();
        self.entries.clear();
        self.ranking.clear();
        // `ALL` lists options by category, as the menu does.
        for info in ALL {
            self.items.push(&format!(
                "{} {} {} {}",
                info.name,
                info.short,
                info.description,
                info.category.title()
            ));
            self.entries.push(Entry::Option(info));
        }
        for bound in bindings(facts.keys) {
            let spoken: Vec<String> = bound.keys.iter().map(Chord::spoken).collect();
            self.items.push(&format!(
                "{} {} {} keys",
                display(&bound.keys),
                spoken.join(" "),
                bound.does()
            ));
            self.entries.push(Entry::Keys(bound));
        }
        // The daemon's, as it's said them, after.
        self.daemon = facts.daemon.is_some();
        let daemons = (clackatoo_daemon::settings::options::ALL.iter())
            .filter(|info| facts.daemon.is_some_and(|daemon| daemon.knows(info)));
        for info in daemons {
            self.items.push(&format!(
                "{} daemon.{0} {} {} daemon",
                info.name,
                info.description,
                Section::Daemon(info.category).title()
            ));
            self.entries.push(Entry::Option(info));
        }
        self.ranking.add(&self.items);
        self.picker.clamp(self.ranking.len());
    }

    /// The row ranked `rank`'s name in the history.
    fn name_at(&mut self, rank: usize) -> Option<String> {
        let index = self.ranking.get(rank)?;
        Some(match &self.entries[index] {
            Entry::Option(info) => info.name.to_owned(),
            Entry::Keys(bound) => bound.name(),
        })
    }

    fn picked_name(&mut self) -> Option<String> {
        self.name_at(self.picker.selected())
    }

    /// The row picked.
    fn picked(&mut self) -> Option<Entry> {
        let index = self.ranking.get(self.picker.selected())?;
        Some(self.entries[index].clone())
    }

    /// Handles a key, with what `facts` say of the window, noting changes
    /// in `history`.
    pub(crate) fn key(
        &mut self,
        press: &KeyPress<'_>,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        self.said = None;
        // Keys being recorded are keys, whatever they'd do.
        if let Some(Editing::Keys { .. }) = &self.editing {
            return self.record(press, facts, history);
        }
        picker::press(press, |stroke| {
            let g = std::mem::take(&mut self.g);
            self.stroke(stroke, g, facts, history)
        })
    }

    crate::shortcuts::bindings::described_keys! {
        fn stroke / describe_stroke(&mut self, stroke: Stroke, g: bool) -> Outcome;
        execute(facts: &Facts, history: &mut History);
        prepare {
            // A letter that acts, while nothing's typed or edited.
            let editing = self.editing.is_some();
            let letter = stroke.plain().filter(|_| !self.typing && !editing);
            let easy = self.mode == Mode::Easy;
        }
        match (stroke, letter) => {
            (_, _) if editing =>
                Hint::Text("Type the value, or pick the font"),
                self.edit_stroke(stroke, facts, history);
            (Stroke::Escape | Stroke::Char { ch: 'c', ctrl: true }, _) =>
                Hint::Action("Clear the search, or close the menu"),
                self.escape();
            (_, Some('q')) =>
                Hint::Action("Close the menu"),
                Outcome::Close;
            (Stroke::Enter, _) if self.typing =>
                Hint::Action("Stop typing, keeping the search"),
                {
                    self.typing = false;
                    self.easy.found();
                    Outcome::Changed
                };
            (Stroke::Tab | Stroke::BackTab, _) if self.typing =>
                Hint::Action("Stop typing, keeping the search"),
                {
                    self.typing = false;
                    self.easy.found();
                    Outcome::Changed
                };
            (_, Some('m')) =>
                Hint::Action("Switch between Easy and Advanced"),
                self.switch_mode();
            (Stroke::Enter | Stroke::Char { ch: ' ', ctrl: false }, _) if !self.typing && easy =>
                Hint::Action("Change the setting, or open where it's set"),
                self.easy_act(facts, history);
            (Stroke::Down, _) if easy =>
                Hint::Action("Next setting"),
                self.go(1);
            (Stroke::Up, _) if easy =>
                Hint::Action("Previous setting"),
                self.go(-1);
            (Stroke::Tab, _) if easy =>
                Hint::Action("Next control, on to the next page"),
                self.easy_tab(true);
            (Stroke::BackTab, _) if easy =>
                Hint::Action("Control before"),
                self.easy_tab(false);
            (_, Some('}')) if easy =>
                Hint::Action("Next page"),
                self.easy_page(1);
            (_, Some('{')) if easy =>
                Hint::Action("Page before"),
                self.easy_page(-1);
            (Stroke::Right, _) | (_, Some('l')) if easy && !self.typing =>
                Hint::Action("Next value, or from the pages into the page"),
                self.easy_step(1, facts, history);
            (Stroke::Left, _) | (_, Some('h')) if easy && !self.typing =>
                Hint::Action("Value before"),
                self.easy_step(-1, facts, history);
            (_, Some('r')) if easy =>
                Hint::Action("Put the setting back as it starts"),
                self.easy_reset(facts, history);
            (_, Some('u')) if easy =>
                Hint::Action("Undo the setting's last change"),
                self.easy_undo(facts, history);
            (_, Some('o')) if easy =>
                Hint::Action("Open the line that set it"),
                self.easy_open(facts);
            (_, Some('x' | 'd')) if easy =>
                Hint::None,
                Outcome::Nothing;
            (Stroke::Enter | Stroke::Char { ch: ' ', ctrl: false }, _) if !self.typing =>
                Hint::Action("Change the setting, or open where it's set"),
                self.act(facts, history);
            (_, Some('j')) =>
                Hint::Action("Next setting"),
                self.go(1);
            (_, Some('k')) =>
                Hint::Action("Previous setting"),
                self.go(-1);
            (_, Some('g')) if g =>
                Hint::Action("First setting"),
                self.go_to(0);
            (_, Some('g')) =>
                Hint::Prefix,
                {
                    self.g = true;
                    Outcome::Nothing
                };
            (_, Some('G')) =>
                Hint::Action("Last setting"),
                self.go_to(usize::MAX);
            (Stroke::Tab, _) | (_, Some('}')) =>
                Hint::Action("Next section"),
                self.next_section(true);
            (Stroke::BackTab, _) | (_, Some('{')) =>
                Hint::Action("Section before"),
                self.next_section(false);
            (Stroke::Right, _) | (_, Some('l')) if !self.typing =>
                Hint::Action("Next value"),
                self.step(1, facts, history);
            (Stroke::Left, _) | (_, Some('h')) if !self.typing =>
                Hint::Action("Value before"),
                self.step(-1, facts, history);
            (_, Some('r')) =>
                Hint::Action("Put the setting back as it starts"),
                self.reset(facts, history);
            (_, Some('u')) =>
                Hint::Action("Undo the setting's last change"),
                self.undo(facts, history);
            (_, Some('x')) =>
                Hint::Action("Take away this window's own value"),
                self.clear_local(facts);
            (_, Some('d')) =>
                Hint::Action("Take out keys the menu saved"),
                self.take_out(facts, history);
            (_, Some('o')) =>
                Hint::Action("Open the line that set it"),
                self.open(facts);
            (_, Some('/')) =>
                Hint::Action("Search settings"),
                {
                    self.typing = true;
                    Outcome::Changed
                };
            (_, Some(_)) =>
                Hint::None,
                Outcome::Nothing;
            _ =>
                Picker::hint(stroke, "Search settings"),
                match self.picker.key(stroke, self.ranking.len()) {
                    Some(keyed) => keyed.outcome(|| self.retyped()),
                    None => Outcome::Nothing,
                };
        }
        result
    }

    pub(crate) fn shortcut_context(&self) -> crate::shortcuts::Context {
        crate::shortcuts::bindings::list("Settings", None, |stroke, pending| {
            self.describe_stroke(stroke, pending == Some('g'))
        })
    }

    /// Types `text`, into the search, or the value being typed.
    pub(crate) fn paste(&mut self, text: &str) -> Outcome {
        match &mut self.editing {
            Some(Editing::Value { text: typed, .. }) => {
                crate::vim::list::paste_into(typed, text);
                Outcome::Changed
            }
            Some(Editing::Font(fonts)) => {
                if fonts.picker.paste(text) == picker::Keyed::Typed {
                    fonts.ranking.set_query(fonts.picker.query(), &fonts.items);
                }
                Outcome::Changed
            }
            Some(Editing::Keys { .. }) => Outcome::Nothing,
            None => {
                let keyed = self.picker.paste(text);
                if keyed == picker::Keyed::Typed {
                    self.typing = true;
                }
                keyed.outcome(|| self.retyped())
            }
        }
    }

    /// What's typed changed: the rows are matched against it again.
    fn retyped(&mut self) -> Outcome {
        self.ranking.set_query(self.picker.query(), &self.items);
        self.picker.restart();
        self.easy.retyped(self.picker.query());
        Outcome::Changed
    }

    /// Esc: what's typed goes, or else the menu does.
    fn escape(&mut self) -> Outcome {
        if !self.typing && self.picker.query().is_empty() {
            return Outcome::Close;
        }
        self.typing = false;
        let keep = self.picked_name();
        self.picker.clear();
        self.ranking.set_query("", &self.items);
        self.easy.retyped("");
        // The row picked stays picked, among them all.
        let rank = keep.and_then(|name| {
            (0..self.ranking.len()).find(|&rank| self.name_at(rank).as_deref() == Some(&name))
        });
        self.picker.set_selected(rank.unwrap_or(0));
        Outcome::Changed
    }

    fn go(&mut self, by: isize) -> Outcome {
        if self.mode == Mode::Easy {
            return self.easy_go(by);
        }
        let keyed = self.picker.go(by, self.ranking.len());
        keyed.outcome(|| Outcome::Changed)
    }

    fn go_to(&mut self, rank: usize) -> Outcome {
        if self.mode == Mode::Easy {
            return self.easy_go_to(rank);
        }
        let rank = rank.min(self.ranking.len().saturating_sub(1));
        self.picker.set_selected(rank);
        Outcome::Changed
    }

    /// Moves what's picked by `by` rows, as the mouse's wheel does.
    pub(crate) fn scroll(&mut self, by: isize) -> Outcome {
        match &mut self.editing {
            Some(Editing::Font(fonts)) => (fonts.picker)
                .go(by, fonts.ranking.len())
                .outcome(|| Outcome::Changed),
            _ if self.mode == Mode::Easy => self.easy.scroll(by),
            _ => self.go(by),
        }
    }

    /// Picks the row shown at `row`, as a click does, and changes it if
    /// `open`, as a double click does.
    pub(crate) fn click(
        &mut self,
        row: usize,
        open: bool,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        if let Some(Editing::Font(fonts)) = &mut self.editing {
            if !fonts.picker.click(row, fonts.ranking.len()) {
                return Outcome::Nothing;
            }
            return if open {
                self.edit_stroke(Stroke::Enter, facts, history)
            } else {
                Outcome::Changed
            };
        }
        if self.editing.is_some() || !self.picker.click(row, self.ranking.len()) {
            return Outcome::Nothing;
        }
        if open {
            return self.act(facts, history);
        }
        Outcome::Changed
    }

    /// The first row of the next section, or the one before if not
    /// `forward`, among the rows matching what's typed.
    fn next_section(&mut self, forward: bool) -> Outcome {
        let len = self.ranking.len();
        let sections: Vec<Section> = (0..len)
            .filter_map(|rank| self.ranking.get(rank))
            .map(|index| self.entries[index].section())
            .collect();
        let at = self.picker.selected().min(len.saturating_sub(1));
        let Some(&here) = sections.get(at) else {
            return Outcome::Nothing;
        };
        // The start of the section the row picked is in.
        let start = (0..=at)
            .rev()
            .take_while(|&rank| sections[rank] == here)
            .last()
            .unwrap_or(at);
        let to = if forward {
            (at..len).find(|&rank| sections[rank] != here)
        } else if start == 0 {
            None
        } else {
            let before = sections[start - 1];
            (0..start)
                .rev()
                .take_while(|&rank| sections[rank] == before)
                .last()
        };
        match to {
            Some(rank) => self.go_to(rank),
            None => Outcome::Nothing,
        }
    }

    /// Enter: turns a switch on or off, picks the next choice, starts a
    /// value typed, the font picker or keys recorded, or opens the line a
    /// locked row is set in.
    fn act(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        match self.picked() {
            None => Outcome::Nothing,
            Some(Entry::Option(info)) => {
                if let Some(by) = facts.locked(info) {
                    return self.open_set_by(Some(info), by, facts);
                }
                match info.kind {
                    Kind::Bool | Kind::Choice(_) => self.step(1, facts, history),
                    Kind::Text if info.name == "font" => {
                        self.editing = Some(Editing::Font(FontPicker::default()));
                        Outcome::Fonts
                    }
                    _ => {
                        let text = typed_value(&facts.configured_value(info));
                        self.editing = Some(Editing::Value { info, text });
                        Outcome::Changed
                    }
                }
            }
            Some(Entry::Keys(bound)) => {
                let by = (bound.modes.iter())
                    .map(|&mode| facts.keys.bound_by(mode, &bound.keys))
                    .find(SetBy::config_loading);
                if let Some(by) = by {
                    return self.open_set_by(None, by, facts);
                }
                if bound.typed().is_none() {
                    return self.refuse(
                        "it runs a function of your config's, which settings.lua can't bind: bind more keys to it in init.lua",
                    );
                }
                self.editing = Some(Editing::Keys {
                    bound,
                    keys: Vec::new(),
                });
                Outcome::Changed
            }
        }
    }

    /// Says `why` nothing was done, as a warning.
    fn refuse(&mut self, why: &str) -> Outcome {
        self.said = Some((why.to_owned(), true));
        Outcome::Changed
    }

    /// Opens where `by` set `info`'s option, or keys, if it says, or says
    /// it doesn't: a line of the daemon's config on the daemon's machine.
    fn open_set_by(&mut self, info: Option<&Info>, by: SetBy, facts: &Facts) -> Outcome {
        let daemons = info.is_some_and(|info| info.scope == Scope::Daemon);
        match by.at {
            Some(place) if daemons => Outcome::OpenDaemons(place),
            Some(place) => Outcome::Open(place),
            None => {
                let said = match info {
                    Some(info) => by.said(facts.dir_of(info)),
                    None => facts.said(&by),
                };
                self.refuse(&format!("{said}, at no line clackatoo knows"))
            }
        }
    }

    /// `o`: opens the line that set the row's value, if a line did.
    fn open(&mut self, facts: &Facts) -> Outcome {
        let (info, by) = match self.picked() {
            Some(Entry::Option(info)) => (Some(info), facts.set_by_in(info)),
            Some(Entry::Keys(bound)) => (
                None,
                (bound.modes.iter())
                    .map(|&mode| facts.keys.bound_by(mode, &bound.keys))
                    .find(|by| by.at.is_some())
                    .unwrap_or(SetBy::DEFAULT),
            ),
            None => return Outcome::Nothing,
        };
        if by.at.is_none() {
            return self.refuse("no line of config set it");
        }
        self.open_set_by(info, by, facts)
    }

    /// Changes the option picked to `value`, if it isn't locked, noting
    /// how to undo it.
    fn change(
        &mut self,
        info: &'static Info,
        live: Value,
        saved: Option<Value>,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        let set = Set { info, live, saved };
        self.change_all(vec![set], facts, history)
    }

    /// Changes options together, as `sets` say, unless one's locked,
    /// noting how to undo them as one change, under each of their names.
    /// Those already as they're to be are left alone.
    fn change_all(&mut self, sets: Vec<Set>, facts: &Facts, history: &mut History) -> Outcome {
        let names: Vec<String> = sets.iter().map(|set| set.info.name.to_owned()).collect();
        if let Some((info, by)) =
            (sets.iter()).find_map(|set| Some((set.info, facts.locked(set.info)?)))
        {
            let said = format!(
                "{} sets {}, which outdoes what the menu saves: Enter opens it",
                facts.place(info, &by),
                info.name
            );
            return self.refuse(&said);
        }
        // One read only as a window opens is only saved, so it's as it was
        // if what's saved is; and the daemon's are only saved, as the
        // daemon runs what's saved.
        let changed: Vec<Set> = (sets.into_iter())
            .filter(|Set { info, live, saved }| {
                facts.saved(info) != *saved
                    || !info.restart
                        && info.scope != Scope::Daemon
                        && (facts.get(info) != *live || facts.local(info).is_some())
            })
            .collect();
        if changed.is_empty() {
            return Outcome::Nothing;
        }
        let undo = (changed.iter())
            .map(|set| Set {
                info: set.info,
                live: facts.get(set.info),
                saved: facts.saved(set.info),
            })
            .collect();
        history.push(names, &Undo::Options(undo));
        Outcome::Set(changed)
    }

    /// `l` or `h`: the next value, or the one before, of the option
    /// picked: on or off, a choice, or a number up or down.
    fn step(&mut self, by: i32, facts: &Facts, history: &mut History) -> Outcome {
        let Some(Entry::Option(info)) = self.picked() else {
            return Outcome::Nothing;
        };
        let now = facts.value(info);
        let next = match (info.kind, &now) {
            (Kind::Bool, Value::Bool(on)) => Value::Bool(!on),
            (Kind::Choice(words), Value::Choice(word)) => {
                let at = words.iter().position(|each| each == word).unwrap_or(0);
                let len = i32::try_from(words.len()).unwrap_or(1);
                let next = (i32::try_from(at).unwrap_or(0) + by).rem_euclid(len);
                Value::Choice(words[usize::try_from(next).unwrap_or(0)])
            }
            (Kind::Number { min, max, or_zero }, Value::Number(number)) => {
                let next = match (by > 0, *number) {
                    (true, 0) if or_zero => min,
                    (false, n) if or_zero && n == min => 0,
                    (true, n) => n.saturating_add(1),
                    (false, n) => n.saturating_sub(1),
                };
                if !((min..=max).contains(&next) || or_zero && next == 0) {
                    return self.refuse(&info.wants());
                }
                Value::Number(next)
            }
            (Kind::Decimal { min, max }, Value::Decimal(number)) => {
                let whole = number.as_f64().floor();
                let next = if by > 0 {
                    whole + 1.0
                } else if number.as_f64() > whole {
                    whole
                } else {
                    whole - 1.0
                };
                let next = crate::options::Decimal::exact(next.max(0.0));
                match next.filter(|next| {
                    (crate::options::Decimal::whole(min)..=crate::options::Decimal::whole(max))
                        .contains(next)
                }) {
                    Some(next) => Value::Decimal(next),
                    None => return self.refuse(&info.wants()),
                }
            }
            _ => return Outcome::Nothing,
        };
        self.change(info, next.clone(), Some(next), facts, history)
    }

    /// `r`: puts the option picked back as it starts, taking it out of its
    /// file; or keys the menu saved, back as the window starts them.
    fn reset(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        match self.picked() {
            Some(Entry::Option(info)) => {
                self.change(info, info.default_value(), None, facts, history)
            }
            Some(Entry::Keys(_)) => self.take_out(facts, history),
            _ => Outcome::Nothing,
        }
    }

    /// `u`: undoes the last change the menu made to the row picked.
    fn undo(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        let Some(name) = self.picked_name() else {
            return Outcome::Nothing;
        };
        self.undo_named(&[name], facts, history)
    }

    /// Undoes the last change the menu made, in either mode, to anything
    /// `names` name in its history.
    fn undo_named(&mut self, names: &[String], facts: &Facts, history: &mut History) -> Outcome {
        let Some((number, undo)) = history.newest(names) else {
            return self.refuse(&format!("the menu hasn't changed {}", names.join(" or ")));
        };
        let undo = undo.clone();
        // What the user's own config sets or binds as it loads outdoes what
        // undoing would save, so the change is kept, to undo once it's not.
        let locked = match &undo {
            Undo::Options(sets) => (sets.iter()).find_map(|set| {
                let by = facts.locked(set.info)?;
                Some((facts.place(set.info, &by), set.info.name.to_owned(), "sets"))
            }),
            Undo::Keys(changes) => (changes.iter()).find_map(|(mapping, _)| {
                let by = facts.keys_locked(&mapping.modes, &mapping.keys)?;
                let place = by.place(facts.dir).unwrap_or_else(|| "your config".into());
                Some((place, display(&mapping.keys), "binds"))
            }),
        };
        if let Some((place, what, does)) = locked {
            return self.refuse(&format!("{place} {does} {what} now, so it can't be undone"));
        }
        match undo {
            // Kept, for the user to undo.
            Undo::Keys(_) if facts.automating => self.refuse(AGENTS_SAVE_NO_KEYS),
            Undo::Options(sets) => {
                history.remove(number);
                Outcome::Set(sets)
            }
            Undo::Keys(changes) => {
                history.remove(number);
                Outcome::Keys(changes)
            }
        }
    }

    /// `x`: takes away this window's own value of the option picked.
    fn clear_local(&mut self, facts: &Facts) -> Outcome {
        let Some(Entry::Option(info)) = self.picked() else {
            return Outcome::Nothing;
        };
        if facts.local(info).is_none() {
            return self.refuse(&format!("this window has no {} of its own", info.name));
        }
        Outcome::ClearLocal(info)
    }

    /// `d`: takes keys the menu saved out of settings.lua, back as the
    /// window starts them.
    fn take_out(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        let Some(Entry::Keys(bound)) = self.picked() else {
            return Outcome::Nothing;
        };
        if facts.automating {
            return self.refuse(AGENTS_SAVE_NO_KEYS);
        }
        // Every one of its modes saved there, or it isn't the menu's to take
        // out.
        let saved = facts.bound_by(&bound).map(|by| by.source);
        let (Binding::Keys(to), Some(Source::Settings)) = (&bound.binding, saved) else {
            return self.refuse(&format!(
                "{} isn't saved in settings.lua: only keys the menu saved can be taken out here",
                display(&bound.keys)
            ));
        };
        let mapping = Mapping {
            modes: bound.modes.clone(),
            keys: bound.keys.clone(),
            to: to.clone(),
            desc: bound.label.clone(),
        };
        history.push([bound.name()], &Undo::Keys(vec![(mapping.clone(), true)]));
        Outcome::Keys(vec![(mapping, false)])
    }

    /// A key while keys are recorded: Enter saves them, Esc gives up,
    /// Backspace takes the last back, and any other is recorded.
    fn record(&mut self, press: &KeyPress<'_>, facts: &Facts, history: &mut History) -> Outcome {
        let Some(chord) = Chord::of(press) else {
            return Outcome::Nothing;
        };
        let Some(Editing::Keys { bound, keys }) = &mut self.editing else {
            return Outcome::Nothing;
        };
        let plain = !(chord.ctrl || chord.alt || chord.shift);
        match chord.base {
            crate::keymap::Base::Named(crate::keymap::Named::Escape) if plain => {
                self.editing = None;
                Outcome::Changed
            }
            crate::keymap::Base::Named(crate::keymap::Named::Backspace) if plain => {
                keys.pop();
                Outcome::Changed
            }
            crate::keymap::Base::Named(crate::keymap::Named::Enter) if plain => {
                if keys.is_empty() {
                    self.editing = None;
                    return Outcome::Changed;
                }
                let (bound, keys) = (bound.clone(), keys.clone());
                self.editing = None;
                self.bind(&bound, keys, facts, history)
            }
            _ => {
                keys.push(chord);
                Outcome::Changed
            }
        }
    }

    /// Binds `keys` to do what `bound` does, unless the user's own config
    /// binds them.
    fn bind(
        &mut self,
        bound: &Bound,
        keys: Vec<Chord>,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        if facts.automating {
            return self.refuse(AGENTS_SAVE_NO_KEYS);
        }
        if let Some(by) = facts.keys_locked(&bound.modes, &keys) {
            let said = format!(
                "{} binds {}, which outdoes what the menu saves",
                by.place(facts.dir).unwrap_or_else(|| "your config".into()),
                display(&keys)
            );
            return self.refuse(&said);
        }
        let Some(to) = bound.typed() else {
            return Outcome::Nothing;
        };
        if keys == bound.keys {
            return self.refuse(&format!("{} already does it", display(&keys)));
        }
        // What settings.lua bound the keys to before, to bind again on
        // undoing.
        let before = saved_bindings(facts.keys, &bound.modes, &keys);
        let mapping = Mapping {
            modes: bound.modes.clone(),
            keys,
            to,
            desc: Some(bound.does()),
        };
        let mut undo = vec![(mapping.clone(), false)];
        undo.extend(before.into_iter().map(|before| (before, true)));
        history.push([bound.name()], &Undo::Keys(undo));
        Outcome::Keys(vec![(mapping, true)])
    }

    /// A key while a value is typed, or a font picked.
    fn edit_stroke(&mut self, stroke: Stroke, facts: &Facts, history: &mut History) -> Outcome {
        match &mut self.editing {
            Some(Editing::Value { info, text }) => {
                let info = *info;
                match stroke {
                    Stroke::Escape
                    | Stroke::Char {
                        ch: 'c',
                        ctrl: true,
                    } => {
                        self.editing = None;
                        Outcome::Changed
                    }
                    Stroke::Enter => match info.parse(text.trim()) {
                        Ok(value) => {
                            self.editing = None;
                            self.change(info, value.clone(), Some(value), facts, history)
                        }
                        Err(why) => self.refuse(&why),
                    },
                    stroke => {
                        crate::cmdline::simple::key(text, stroke);
                        Outcome::Changed
                    }
                }
            }
            Some(Editing::Font(fonts)) => match stroke {
                Stroke::Escape
                | Stroke::Char {
                    ch: 'c',
                    ctrl: true,
                } => {
                    self.editing = None;
                    Outcome::Changed
                }
                Stroke::Enter => {
                    let Some(family) = fonts.picked() else {
                        return Outcome::Nothing;
                    };
                    self.editing = None;
                    let info = crate::options::find("font").expect("an option");
                    let value = Value::Text(family);
                    self.change(info, value.clone(), Some(value), facts, history)
                }
                Stroke::Tab => {
                    fonts.all = !fonts.all;
                    fonts.fill(&facts.store.font);
                    Outcome::Changed
                }
                stroke => match fonts.picker.key(stroke, fonts.ranking.len()) {
                    Some(keyed) => keyed.outcome(|| {
                        fonts.ranking.set_query(fonts.picker.query(), &fonts.items);
                        Outcome::Changed
                    }),
                    None => Outcome::Nothing,
                },
            },
            Some(Editing::Keys { .. }) | None => Outcome::Nothing,
        }
    }

    /// Whether letters type into the search.
    #[cfg(test)]
    pub(crate) fn typing(&self) -> bool {
        self.typing
    }

    /// Whether it waits for the fonts installed, which [`fonts`](Self::fonts)
    /// gives it.
    pub(crate) fn wants_fonts(&self) -> bool {
        matches!(&self.editing, Some(Editing::Font(fonts)) if fonts.installed.is_none())
    }

    /// Takes in the font families installed, as `fc-list : family` lists
    /// them, each with whether it's monospace, for the font picker, the one
    /// in use, `font`, picked.
    pub(crate) fn fonts(&mut self, mut installed: Vec<(String, bool)>, font: &str) {
        let Some(Editing::Font(fonts)) = &mut self.editing else {
            return;
        };
        installed.sort_by_key(|(family, _)| family.to_lowercase());
        installed.dedup_by(|a, b| a.0 == b.0);
        fonts.installed = Some(installed);
        fonts.fill(font);
    }

    /// What to draw, for a list `rows` rows tall and `cols` cells wide,
    /// and a preview of so many lines and cells, if there's room for one.
    pub(crate) fn view(
        &mut self,
        facts: &Facts,
        (rows, cols): (usize, usize),
        preview: Option<(usize, usize)>,
    ) -> FinderView {
        let picked = self.picked();
        let side = self.side(picked.as_ref(), rows, cols);
        let cols = cols.saturating_sub(side.cells()).max(1);
        if let Some(Editing::Font(fonts)) = &mut self.editing {
            return fonts.view(&facts.store.font, side, (rows, cols), preview);
        }
        let len = self.ranking.len();
        let range = self.picker.shown(rows, len);
        let shown: Vec<Row> = range
            .filter_map(|rank| self.row(rank, facts, cols))
            .collect();
        let preview = preview.map(|size| detail(picked.as_ref(), facts, size));
        let query = match &self.editing {
            Some(Editing::Value { info, text }) => picker::drawn(&format!("{}={text}", info.name)),
            Some(Editing::Keys { bound, keys }) => {
                picker::drawn(&format!("keys for {}: {}", bound.does(), display(keys)))
            }
            _ if self.typing || !self.picker.query().is_empty() => {
                picker::drawn(self.picker.query())
            }
            _ => String::new(),
        };
        let count = match (&self.said, &self.editing) {
            (Some((said, _)), _) => said.clone(),
            (None, Some(Editing::Value { .. })) => "Enter sets it · Esc goes back".into(),
            (None, Some(Editing::Keys { .. })) => {
                "Enter saves · Backspace takes one back · Esc goes back".into()
            }
            _ => format!("{} / {}", self.ranking.len(), self.items.len()),
        };
        FinderView {
            title: "Settings",
            side: Some(side),
            place: daemon_elsewhere(facts),
            query,
            count,
            selected: self.picker.shown_selected(len),
            rows: shown,
            preview,
            empty: Some("No setting matches".into()),
        }
    }

    /// The sections, down the side, the picked row's picked out, in at
    /// most nine twentieths of `cols`.
    fn side(&self, picked: Option<&Entry>, rows: usize, cols: usize) -> SideView {
        let mut sections: Vec<Section> = self.entries.iter().map(Entry::section).collect();
        sections.dedup();
        let width = (sections.iter())
            .map(|section| section.title().chars().count())
            .max()
            .unwrap_or(0)
            .min(cols * 9 / 20);
        let here = picked.map(Entry::section);
        let shown: Vec<Row> = (sections.iter())
            .take(rows)
            .map(|section| {
                let title = section.title();
                let ink = if Some(*section) == here {
                    Ink::Bold
                } else {
                    Ink::Muted
                };
                fit(title, &vec![ink; title.chars().count()], width, false)
            })
            .collect();
        SideView {
            selected: here.and_then(|here| sections.iter().position(|&section| section == here)),
            rows: shown,
            cols: width,
        }
    }

    /// The row ranked `rank`, `cols` cells wide.
    fn row(&mut self, rank: usize, facts: &Facts, cols: usize) -> Option<Row> {
        let index = self.ranking.get(rank)?;
        let accents = self.ranking.matched_chars(self.items.get(index));
        let mut pieces: Vec<(String, Ink)> = Vec::new();
        // The characters of what's matched that a piece of the row shows,
        // from where it starts in the row's text.
        let matched_in = |piece: &str, from: usize| -> Vec<bool> {
            (0..piece.chars().count())
                .map(|at| {
                    accents
                        .binary_search(&u32::try_from(from + at).unwrap_or(u32::MAX))
                        .is_ok()
                })
                .collect()
        };
        let mut lit: Vec<bool> = Vec::new();
        let tag: Option<(String, Ink)> = match &self.entries[index] {
            Entry::Option(info) => {
                let changed = facts.value(info) != info.default_value();
                // The daemon's, whose names are longer, as wide as theirs,
                // with a cell less after, leaving room for `init.lua:N`.
                let width = match info.scope {
                    Scope::Daemon => {
                        (clackatoo_daemon::settings::options::ALL.iter())
                            .map(|info| info.name.len())
                            .max()
                            .unwrap_or(0)
                            + 1
                    }
                    Scope::Window | Scope::Startup(_) => self.name_cols + 2,
                };
                let name = format!("{:<width$}", info.name);
                lit.extend(matched_in(info.name, 0));
                lit.resize(name.chars().count(), false);
                pieces.push((name, if changed { Ink::Bold } else { Ink::Plain }));
                pieces.push((shown_value(info, &facts.value(info)), Ink::Plain));
                let differs = facts.get(info) != facts.configured_value(info);
                if let Some(by) = facts.locked(info) {
                    Some((short_place(&by), Ink::Muted))
                } else if facts.local(info).is_some() {
                    Some(("this window's own".into(), Ink::Muted))
                } else if !info.restart && differs {
                    Some(("not saved".into(), Ink::Caution))
                } else if info.restart && differs && info.scope == Scope::Daemon {
                    // Read only as the daemon starts: a change waits for it.
                    Some(("restart".into(), Ink::Caution))
                } else if info.restart && differs {
                    Some(("for windows opened after".into(), Ink::Muted))
                } else {
                    None
                }
            }
            Entry::Keys(bound) => {
                let keys = display(&bound.keys);
                let name = format!("{keys:<width$}", width = self.name_cols + 2);
                lit.extend(matched_in(&keys, 0));
                lit.resize(name.chars().count(), false);
                pieces.push((name, Ink::Plain));
                pieces.push((bound.does(), Ink::Plain));
                Some(match facts.bound_by(bound) {
                    Some(by) if by.config_loading() => (short_place(&by), Ink::Muted),
                    Some(by) if by.source == Source::Settings => {
                        ("settings.lua".into(), Ink::Muted)
                    }
                    _ => (KeyMode::letters(&bound.modes), Ink::Muted),
                })
            }
        };
        let mut text = String::new();
        let mut inks = Vec::new();
        for (piece, ink) in &pieces {
            for ch in piece.chars() {
                text.push(ch);
                let matched = inks.len() < lit.len() && lit[inks.len()];
                inks.push(if matched { Ink::Matched } else { *ink });
            }
        }
        if let Some((tag, ink)) = tag {
            let used = text.chars().count();
            // A cell at least between it and the value.
            let room = cols.saturating_sub(used + 1);
            let tag = crate::finder::view::cut(&tag, room, true);
            let pad = cols.saturating_sub(used + tag.chars().count());
            text.extend(std::iter::repeat_n(' ', pad));
            inks.extend(std::iter::repeat_n(Ink::Plain, pad));
            inks.extend(std::iter::repeat_n(ink, tag.chars().count()));
            text.push_str(&tag);
        }
        Some(fit(&text, &inks, cols, false))
    }
}

/// The font picker, while it's open: the families installed, what's typed
/// to find one, and the one picked. It lists monospace families, as a
/// terminal's font is, unless Tab lists them all.
#[derive(Debug)]
struct FontPicker {
    /// The families installed, each with whether it's monospace: none
    /// until the window has said.
    installed: Option<Vec<(String, bool)>>,
    /// Every family is listed, not only the monospace ones.
    all: bool,
    /// The families listed, as `items` has them after the system's font.
    families: Vec<String>,
    picker: Picker,
    items: Items,
    ranking: Ranking,
}

impl Default for FontPicker {
    fn default() -> Self {
        Self {
            installed: None,
            all: false,
            families: Vec::new(),
            picker: Picker::default(),
            items: Items::default(),
            ranking: Ranking::new(Case::default(), false),
        }
    }
}

impl FontPicker {
    /// Lists the families again, as `all` says, the one in use, `font`,
    /// among them whatever it is, and picks it.
    fn fill(&mut self, font: &str) {
        let installed = self.installed.as_deref().unwrap_or_default();
        self.families = (installed.iter())
            .filter(|(family, mono)| self.all || *mono || family == font)
            .map(|(family, _)| family.clone())
            .collect();
        self.items.clear();
        self.ranking.clear();
        self.items.push(SYSTEM_FONT);
        for family in &self.families {
            self.items.push(family);
        }
        self.ranking.add(&self.items);
        let at = (self.families.iter())
            .position(|family| family == font)
            .map_or(0, |at| at + 1);
        let rank = (0..self.ranking.len()).find(|&rank| self.ranking.get(rank) == Some(at));
        self.picker.set_selected(rank.unwrap_or(0));
    }

    /// The family picked: empty for the system's monospace font.
    fn picked(&mut self) -> Option<String> {
        let index = self.ranking.get(self.picker.selected())?;
        Some(match index {
            0 => String::new(),
            index => self.families.get(index - 1)?.clone(),
        })
    }

    /// The families shown in a list `rows` long and `cols` cells wide,
    /// `font` being the one in use.
    fn shown(
        families: &[String],
        picker: &mut Picker,
        items: &Items,
        ranking: &mut Ranking,
        font: &str,
        (rows, cols): (usize, usize),
    ) -> Vec<Row> {
        let len = ranking.len();
        (picker.shown(rows, len))
            .filter_map(|rank| {
                let index = ranking.get(rank)?;
                let text = items.get(index);
                let accents = ranking.matched_chars(text);
                // The first is the system's font, which is no family.
                let current = match index {
                    0 => font.is_empty(),
                    index => families.get(index - 1).is_some_and(|family| family == font),
                };
                let inks: Vec<Ink> = (0..text.chars().count())
                    .map(|at| {
                        if accents
                            .binary_search(&u32::try_from(at).unwrap_or(u32::MAX))
                            .is_ok()
                        {
                            Ink::Matched
                        } else if current {
                            Ink::Accent
                        } else {
                            Ink::Plain
                        }
                    })
                    .collect();
                Some(fit(text, &inks, cols, false))
            })
            .collect()
    }

    /// What Easy draws of it, below the button of row `row`, `font` being
    /// the one in use.
    fn easy_view(&mut self, font: &str, row: usize) -> view::FontsView {
        let rows = Self::shown(
            &self.families,
            &mut self.picker,
            &self.items,
            &mut self.ranking,
            font,
            (view::FONT_ROWS, 60),
        );
        view::FontsView {
            row,
            query: self.picker.query().to_owned(),
            count: match &self.installed {
                None => "looking for fonts…".into(),
                Some(_) => format!("{} / {}", self.ranking.len(), self.items.len()),
            },
            listing: if self.all {
                "every font"
            } else {
                "monospace fonts"
            },
            rows,
            selected: self.picker.shown_selected(self.ranking.len()),
        }
    }

    /// What to draw, `font` being the one in use, with `side` beside it,
    /// for a list `rows` rows tall and `cols` cells wide, and a preview so
    /// big, if there's room for one.
    fn view(
        &mut self,
        font: &str,
        side: SideView,
        (rows, cols): (usize, usize),
        preview: Option<(usize, usize)>,
    ) -> FinderView {
        let Self {
            installed,
            all,
            families,
            picker,
            items,
            ranking,
        } = self;
        let len = ranking.len();
        let shown = Self::shown(families, picker, items, ranking, font, (rows, cols));
        let waiting = installed.is_none();
        FinderView {
            title: "Font",
            side: Some(side),
            place: Some(
                if *all {
                    "every font"
                } else {
                    "monospace fonts"
                }
                .into(),
            ),
            query: picker::drawn(picker.query()),
            count: if waiting {
                "looking for fonts…".into()
            } else {
                format!("{} / {}", ranking.len(), items.len())
            },
            selected: picker.shown_selected(len),
            rows: shown,
            preview: preview.map(|size| {
                lines(
                    &[
                        vec![("Pick a font family for terminals and editors.", Ink::Plain)],
                        Vec::new(),
                        vec![
                            ("Enter", Ink::Key),
                            (" sets it · ", Ink::Muted),
                            ("Tab", Ink::Key),
                            (
                                if *all {
                                    " lists monospace ones · "
                                } else {
                                    " lists every font · "
                                },
                                Ink::Muted,
                            ),
                            ("Esc", Ink::Key),
                            (" goes back", Ink::Muted),
                        ],
                    ],
                    size,
                )
            }),
            empty: (!waiting).then(|| "No font matches".into()),
        }
    }
}

/// A line of the preview, in runs of ink.
type Said = Vec<(String, Ink)>;

/// A line saying `text` in `ink` after a label, as `Value     on`.
fn labelled(label: &str, text: String, ink: Ink) -> Said {
    vec![(format!("{label:<LABEL$}"), Ink::Muted), (text, ink)]
}

/// A line saying what keys do, as `Enter turns it on or off · r default`.
fn key_hints(pairs: &[(&str, &str)]) -> Said {
    let mut line = Vec::new();
    for (at, (key, does)) in pairs.iter().enumerate() {
        if at > 0 {
            line.push((" · ".to_owned(), Ink::Muted));
        }
        line.push(((*key).to_owned(), Ink::Key));
        line.push((format!(" {does}"), Ink::Muted));
    }
    line
}

/// What the preview says of the row picked: what it is, and where it's
/// set, as a preview `size` lines and cells big.
fn detail(picked: Option<&Entry>, facts: &Facts, size: (usize, usize)) -> PreviewView {
    let said = match picked {
        None => Vec::new(),
        Some(Entry::Option(info)) => option_detail(info, facts),
        Some(Entry::Keys(bound)) => keys_detail(bound, facts),
    };
    let said: Vec<Vec<(&str, Ink)>> = (said.iter())
        .map(|line| {
            line.iter()
                .map(|(text, ink)| (text.as_str(), *ink))
                .collect()
        })
        .collect();
    lines(&said, size)
}

/// What the preview says of `info`'s option.
#[allow(clippy::too_many_lines, reason = "a few lines for each thing said")]
fn option_detail(info: &Info, facts: &Facts) -> Vec<Said> {
    let mut title = vec![(info.name.to_owned(), Ink::Bold)];
    if !info.short.is_empty() {
        title.push((format!("  {}", info.short), Ink::Muted));
    }
    if info.scope == Scope::Daemon {
        title.push(("  the daemon's".into(), Ink::Muted));
    }
    let mut said = vec![
        title,
        vec![(info.description.to_owned(), Ink::Plain)],
        Vec::new(),
        labelled("Value", shown_value(info, &facts.value(info)), Ink::Plain),
        labelled(
            "Default",
            shown_value(info, &info.default_value()),
            Ink::Plain,
        ),
        labelled("Takes", takes(info), Ink::Plain),
    ];
    let shared = facts.get(info);
    let configured = facts.configured_value(info);
    let from = match facts.configured(info) {
        // Saved since the config loaded, at no line yet.
        Some((_, by)) if by.source == Source::Settings && by.at.is_none() => {
            format!("saved in {}", crate::options::persist::file(info))
        }
        Some((_, by)) => by.said(facts.dir_of(info)),
        None if configured != info.default_value() => "as the window opened: startup.toml".into(),
        None => "as it starts".into(),
    };
    said.push(labelled("From", from, Ink::Plain));
    if shared != configured {
        let now = format!(
            "configured {}, currently {}",
            shown_value(info, &configured),
            shown_value(info, &shared)
        );
        // One read as a window opens is as this one opened, and is for
        // windows opened after; one the daemon reads as it starts, as it
        // started.
        if info.restart {
            said.push(labelled("Now", now, Ink::Muted));
        } else {
            said.push(labelled("Now", now, Ink::Caution));
            said.push(labelled("", facts.said(&facts.set_by(info)), Ink::Muted));
        }
    }
    if let Some((value, by)) = facts.local(info) {
        let own = format!("this window's own: {}", shown_value(info, value));
        said.push(labelled("Here", own, Ink::Plain));
        said.push(labelled("", facts.said(by), Ink::Muted));
    }
    let mut file = crate::options::persist::file(info).to_owned();
    if let (Scope::Daemon, Some(machine)) =
        (info.scope, facts.daemon.and_then(|d| d.elsewhere.as_ref()))
    {
        file = format!("{file} on {machine}");
    }
    said.push(labelled("Saved in", file, Ink::Plain));
    let later = match info.scope {
        Scope::Startup(_) if info.restart => Some("takes effect for windows opened after"),
        Scope::Startup(_) => Some("takes effect for windows opened after, and this one now"),
        Scope::Daemon if info.restart => Some(RESTARTS),
        Scope::Daemon => Some("takes effect at once, for what the daemon starts from then on"),
        Scope::Window => None,
    };
    if let Some(later) = later {
        said.push(labelled("", later.into(), Ink::Muted));
    }
    if let (Scope::Daemon, Some(failure)) = (
        info.scope,
        facts.daemon.and_then(|daemon| daemon.failure.as_ref()),
    ) {
        said.push(Vec::new());
        let failed =
            format!("The daemon's config doesn't run, so it goes by what last did: {failure}");
        said.push(vec![(failed, Ink::Caution)]);
    }
    said.push(Vec::new());
    if let Some(by) = facts.locked(info) {
        let locked = format!(
            "Locked: {}, which outdoes what the menu saves",
            by.said(facts.dir_of(info))
        );
        said.push(vec![(locked, Ink::Caution)]);
        said.push(key_hints(&[("Enter", "opens it")]));
        return said;
    }
    let change = match info.kind {
        Kind::Bool => "turns it on or off",
        Kind::Choice(_) => "picks the next",
        Kind::Text if info.name == "font" => "picks a font",
        _ => "types a value",
    };
    let mut pairs = vec![("Enter", change)];
    if matches!(
        info.kind,
        Kind::Bool | Kind::Choice(_) | Kind::Number { .. } | Kind::Decimal { .. }
    ) {
        pairs.push(("h l", "step"));
    }
    pairs.extend([("r", "default"), ("u", "undo")]);
    if facts.local(info).is_some() {
        pairs.push(("x", "clears this window's"));
    }
    said.push(key_hints(&pairs));
    said
}

/// What the preview says of `bound`'s keys.
fn keys_detail(bound: &Bound, facts: &Facts) -> Vec<Said> {
    let spoken: Vec<String> = bound.keys.iter().map(Chord::spoken).collect();
    let by = facts.bound_by(bound).unwrap_or_else(|| bound.by.clone());
    let from = match by.source {
        Source::Settings => "saved in client/settings.lua".to_owned(),
        _ => facts.said(&by),
    };
    let mut said = vec![
        vec![
            (display(&bound.keys), Ink::Bold),
            (format!("  {}", spoken.join(" ")), Ink::Muted),
        ],
        vec![(bound.does(), Ink::Plain)],
        Vec::new(),
        labelled("Modes", modes_said(&bound.modes), Ink::Plain),
        labelled("From", from, Ink::Plain),
        Vec::new(),
    ];
    if by.config_loading() {
        let locked = format!(
            "Locked: {}, which outdoes what the menu saves",
            facts.said(&by)
        );
        said.push(vec![(locked, Ink::Caution)]);
        said.push(key_hints(&[("Enter", "opens it")]));
    } else if bound.typed().is_none() {
        said.push(vec![(
            "It runs a function of your config's: bind more keys to it in init.lua.".into(),
            Ink::Muted,
        )]);
    } else {
        let mut pairs = vec![("Enter", "records more keys for it"), ("u", "undo")];
        if by.source == Source::Settings {
            pairs.push(("d", "takes it out"));
        }
        said.push(key_hints(&pairs));
    }
    said
}
/// The bindings in `keys`, those bound alike in several modes, by the
/// same, as one, in the order of their keys.
fn bindings(keys: &Keys) -> Vec<Bound> {
    let mut found: Vec<Bound> = Vec::new();
    for mode in [
        KeyMode::Normal,
        KeyMode::Insert,
        KeyMode::Visual,
        KeyMode::Prompt,
    ] {
        for (bound_keys, binding, label) in keys.bound(mode) {
            let label = label.map(str::to_owned);
            let by = keys.bound_by(mode, bound_keys);
            let alike = found.iter_mut().find(|bound| {
                bound.keys == bound_keys
                    && bound.binding == *binding
                    && bound.label == label
                    && bound.by == by
            });
            match alike {
                Some(bound) => bound.modes.push(mode),
                None => found.push(Bound {
                    modes: vec![mode],
                    keys: bound_keys.to_vec(),
                    binding: binding.clone(),
                    label,
                    by,
                }),
            }
        }
    }
    found.sort_by(|a, b| (&a.keys, &a.modes).cmp(&(&b.keys, &b.modes)));
    found
}

/// What settings.lua binds `keys` to in each of `modes` it binds them
/// in, those alike as one, as it would save them again.
fn saved_bindings(keys: &Keys, modes: &[KeyMode], bound: &[Chord]) -> Vec<Mapping> {
    let mut found: Vec<Mapping> = Vec::new();
    for &mode in modes {
        if keys.bound_by(mode, bound).source != Source::Settings {
            continue;
        }
        let Some((Binding::Keys(to), label)) = keys.get(mode, bound) else {
            continue;
        };
        let desc = label.map(str::to_owned);
        match (found.iter_mut()).find(|saved| saved.to == *to && saved.desc == desc) {
            Some(saved) => saved.modes.push(mode),
            None => found.push(Mapping {
                modes: vec![mode],
                keys: bound.to_vec(),
                to: to.clone(),
                desc,
            }),
        }
    }
    found
}

/// `modes`, as the menu says them.
fn modes_said(modes: &[KeyMode]) -> String {
    let names: Vec<&str> = (modes.iter())
        .map(|mode| match mode {
            KeyMode::Normal => "normal",
            KeyMode::Insert => "insert",
            KeyMode::Visual => "visual",
            KeyMode::Prompt => "prompts",
        })
        .collect();
    names.join(", ")
}

/// `value`, as the menu shows it: on or off, a number, a word, or text.
pub(crate) fn shown_value(info: &Info, value: &Value) -> String {
    match value {
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
        Value::Number(0) if info.scope == Scope::Daemon => "0 (none)".into(),
        Value::Number(0) if matches!(info.kind, Kind::Number { or_zero: true, .. }) => {
            "0 (the window's choice)".into()
        }
        Value::Number(number) => number.to_string(),
        Value::Decimal(number) => number.to_string(),
        Value::List(numbers) if numbers.is_empty() => "none".into(),
        Value::List(numbers) => {
            let numbers: Vec<String> = numbers.iter().map(u32::to_string).collect();
            numbers.join(", ")
        }
        Value::Choice(word) => (*word).to_owned(),
        Value::Text(text) if text.is_empty() && info.name == "font" => SYSTEM_FONT.into(),
        Value::Text(text) if text.is_empty() => "none".into(),
        Value::Text(text) => format!("“{text}”"),
    }
}

/// `value` as it's typed after `=`, to start typing a new one from.
fn typed_value(value: &Value) -> String {
    match value {
        Value::Bool(on) => on.to_string(),
        Value::Number(number) => number.to_string(),
        Value::Decimal(number) => number.to_string(),
        Value::List(numbers) => {
            let numbers: Vec<String> = numbers.iter().map(u32::to_string).collect();
            numbers.join(",")
        }
        Value::Choice(word) => (*word).to_owned(),
        Value::Text(text) => text.clone(),
    }
}

/// What values `info`'s option takes, as the menu says it.
fn takes(info: &Info) -> String {
    match info.kind {
        Kind::Bool => "on or off".into(),
        Kind::Number {
            min,
            max,
            or_zero: true,
        } => {
            format!("0, the window's choice, or a whole number from {min} to {max}")
        }
        Kind::Number { min, max, .. } => format!("a whole number from {min} to {max}"),
        Kind::Decimal { min, max } => format!("a number from {min} to {max}"),
        Kind::List { min, max } => {
            format!("whole numbers from {min} to {max}, with commas between them")
        }
        Kind::Choice(words) => words.join(", "),
        Kind::Text if info.name == "font" => "a font family, as fc-list lists them".into(),
        Kind::Text => "text".into(),
    }
}

/// Where `by` set it, short, as a row's end says it: the file's name and
/// the line, `init.lua:12`.
fn short_place(by: &SetBy) -> String {
    match &by.at {
        Some(at) => {
            let file = Path::new(&*at.file).file_name().map_or_else(
                || at.file.to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            format!("{file}:{}", at.line)
        }
        None => "your config".into(),
    }
}

/// How wide a label before a value is, as `Value     on`.
const LABEL: usize = 10;

/// `said`, lines of runs of ink, as a preview `size` lines and cells big:
/// a line too long goes on over more, between words, under its value if
/// it has a label.
fn lines(said: &[Vec<(&str, Ink)>], (rows, cols): (usize, usize)) -> PreviewView {
    let cols = cols.max(1);
    let mut lines = Vec::new();
    for runs in said {
        let mut chars: Vec<(char, Ink)> = (runs.iter())
            .flat_map(|(piece, ink)| piece.chars().map(move |ch| (ch, *ink)))
            .collect();
        let labelled = runs
            .first()
            .is_some_and(|(label, ink)| *ink == Ink::Muted && label.chars().count() == LABEL);
        let indent = if labelled && cols > 2 * LABEL {
            LABEL
        } else {
            0
        };
        loop {
            let (row, rest) = if chars.len() <= cols {
                (std::mem::take(&mut chars), Vec::new())
            } else {
                // The last blank that fits, past the label, or else all
                // that fits.
                let at = (indent + 1..=cols)
                    .rev()
                    .find(|&at| chars[at].0 == ' ')
                    .unwrap_or(cols);
                let rest: Vec<(char, Ink)> = (chars[at..].iter())
                    .skip_while(|(ch, _)| *ch == ' ')
                    .copied()
                    .collect();
                chars.truncate(at);
                (std::mem::take(&mut chars), rest)
            };
            let (text, inks): (String, Vec<Ink>) = row.into_iter().unzip();
            lines.push(fit(&text, &inks, cols, false));
            if rest.is_empty() {
                break;
            }
            chars = std::iter::repeat_n((' ', Ink::Plain), indent)
                .chain(rest)
                .collect();
        }
    }
    lines.truncate(rows);
    PreviewView { lines, at: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{parse_keys, parse_modes};

    #[test]
    fn keys_alike_in_several_modes_are_one_row_only_if_one_thing_bound_them_all() {
        let mut keys = Keys::default();
        let alt_t = parse_keys("<A-t>").unwrap();
        let to = Binding::Keys(parse_keys("<C-b>c").unwrap());
        keys.bind(&parse_modes("niv").unwrap(), &alt_t, to, Some("new tab"));
        keys.set_by(&parse_modes("nv").unwrap(), &alt_t, &SetBy::SAVED);
        let rows: Vec<(String, Source)> = (bindings(&keys).into_iter())
            .filter(|bound| bound.keys == alt_t)
            .map(|bound| (KeyMode::letters(&bound.modes), bound.by.source))
            .collect();
        assert_eq!(
            rows,
            [
                ("nv".to_owned(), Source::Settings),
                ("i".to_owned(), Source::Default)
            ]
        );
    }

    #[test]
    fn the_font_picker_lists_monospace_families_and_tab_lists_them_all() {
        let mut fonts = FontPicker {
            installed: Some(vec![
                ("Mono A".to_owned(), true),
                ("Sans B".to_owned(), false),
                ("Serif C".to_owned(), false),
            ]),
            ..FontPicker::default()
        };
        fonts.fill("Serif C");
        // The one in use is listed whatever it is, and picked.
        assert_eq!(fonts.families, ["Mono A", "Serif C"]);
        assert_eq!(fonts.picked().as_deref(), Some("Serif C"));
        fonts.all = true;
        fonts.fill("");
        assert_eq!(fonts.families, ["Mono A", "Sans B", "Serif C"]);
        assert_eq!(fonts.picked().as_deref(), Some(""));
    }

    fn facts<'a>(store: &'a Store, keys: &'a Keys) -> Facts<'a> {
        Facts {
            store,
            window: 1,
            keys,
            dir: None,
            automating: false,
            daemon: None,
        }
    }

    #[test]
    fn keys_are_taken_out_only_if_settings_lua_bound_them_in_every_mode() {
        let store = Store::default();
        let mut keys = Keys::default();
        let alt_t = parse_keys("<A-t>").unwrap();
        let both = parse_modes("ni").unwrap();
        keys.bind(&both, &alt_t, Binding::Keys(parse_keys("x").unwrap()), None);
        keys.set_by(&both, &alt_t, &SetBy::SAVED);
        let mut menu = SettingsMenu::new(
            "<A-t>",
            Case::default(),
            Mode::Advanced,
            &facts(&store, &keys),
            0,
        );
        assert!(
            menu.picked()
                .is_some_and(|entry| matches!(entry, Entry::Keys(bound) if bound.keys == alt_t))
        );
        // Bound again in one mode since the rows were made: the row isn't
        // all settings.lua's now.
        keys.set_by(&[KeyMode::Insert], &alt_t, &SetBy::RUNTIME);
        let mut history = History::default();
        menu.stroke(
            Stroke::char('d'),
            false,
            &facts(&store, &keys),
            &mut history,
        );
        assert!(
            (menu.said.as_ref())
                .is_some_and(|(said, _)| said.contains("isn't saved in settings.lua")),
            "{:?}",
            menu.said
        );
        assert!(history.is_empty());
    }
}
