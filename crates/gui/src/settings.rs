//! The settings menu `:Settings` opens: every option, and every key bound,
//! to look at and change, in sections down its side, as the docs list
//! them.
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
//! It's drawn as the finder is, over the panes (see
//! [`crate::finder::view`]), and is the window's alone, as the finder is:
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
/// them: the options as every window shares them, and this one's own, and
/// the keys bound.
pub(crate) struct Facts<'a> {
    pub(crate) store: &'a Store,
    pub(crate) window: u32,
    pub(crate) keys: &'a Keys,
    /// clackatoo's config directory, which places are said from.
    pub(crate) dir: Option<&'a Path>,
    /// The keys are an agent's, through native window access, which can't
    /// save keys (see `Desk::automating`).
    pub(crate) automating: bool,
}

impl Facts<'_> {
    /// `info`'s option as this window has it.
    fn shown(&self, info: &Info) -> Value {
        match self.store.local(self.window, info) {
            Some((value, _)) => value.clone(),
            None => self.store.get(info),
        }
    }

    /// `info`'s option as the menu shows and changes it: as this window
    /// has it, or for one read only as a window opens, as it's saved, for
    /// windows opened after.
    fn value(&self, info: &Info) -> Value {
        if info.restart {
            self.store.configured_value(info)
        } else {
            self.shown(info)
        }
    }

    /// Where the user's own config sets `info`'s option as it loads, if
    /// it does: then it's locked.
    fn locked(&self, info: &Info) -> Option<&SetBy> {
        let (_, by) = self.store.configured(info)?;
        by.config_loading().then_some(by)
    }

    /// What `info`'s option has saved in its file, as far as the window
    /// knows: what settings.lua set as the config loaded, or what's been
    /// saved since; and for startup.toml's, a value the process started
    /// with that isn't the default.
    fn saved(&self, info: &Info) -> Option<Value> {
        match self.store.configured(info) {
            Some((value, by)) if by.source == Source::Settings => Some(value.clone()),
            Some(_) => None,
            None => {
                let start = self.store.configured_value(info);
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

    /// `by`, as the menu says it.
    fn said(&self, by: &SetBy) -> String {
        by.said(self.dir)
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
    /// `info`'s option is to be set to `live` for every window, if it can
    /// be as the window runs, and `saved` in its file, or with none, taken
    /// out of it.
    Change {
        info: &'static Info,
        live: Value,
        saved: Option<Value>,
    },
    /// This window's own value of `info`'s option is to go.
    ClearLocal(&'static Info),
    /// Keys are to be bound, and saved, or with `bound` false, put back as
    /// the window starts them and taken out of settings.lua, each in turn.
    Keys(Vec<(Mapping, bool)>),
    /// The line at `place` is to be opened in an editor, and the menu
    /// closed.
    Open(Place),
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

/// A section of the menu, down its side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Section {
    Options(Category),
    Keys,
    /// The daemon's own settings, which come later (CLACKA-237 phase 4).
    Daemon,
}

impl Section {
    fn title(self) -> &'static str {
        match self {
            Self::Options(category) => category.title(),
            Self::Keys => "Keys",
            Self::Daemon => "Daemon",
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
    /// What the daemon's section says, for now.
    Coming,
}

impl Entry {
    fn section(&self) -> Section {
        match self {
            Self::Option(info) => Section::Options(info.category),
            Self::Keys(_) => Section::Keys,
            Self::Coming => Section::Daemon,
        }
    }
}

/// A change the menu made to a row, and what undoing it does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Undo {
    /// Sets the option to `live` again, and saves `saved`, or takes it
    /// out.
    Option {
        info: &'static Info,
        live: Value,
        saved: Option<Value>,
    },
    /// Binds keys again, or takes them out, as each says, in turn.
    Keys(Vec<(Mapping, bool)>),
}

/// The changes the menu has made, row by row, newest last, to undo: kept
/// by the window while the menu's closed, so it can be opened again to
/// undo them.
#[derive(Debug, Default)]
pub(crate) struct History(HashMap<String, Vec<Undo>>);

/// What's being changed in a row, while it is.
#[derive(Debug)]
enum Editing {
    /// A value being typed for an option, as `:set` takes it after `=`.
    Value { info: &'static Info, text: String },
    /// A font being picked from those installed: none until the window
    /// has said what they are.
    Font {
        families: Vec<String>,
        picker: Picker,
        items: Items,
        ranking: Ranking,
    },
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
}

/// Why an agent's keys don't change keys, as the window says too.
const AGENTS_SAVE_NO_KEYS: &str = "an agent's input can't save keys";

/// What the font picker lists for the system's monospace font, first.
const SYSTEM_FONT: &str = "the system's monospace font";

impl SettingsMenu {
    /// The menu, its rows made from `facts`, with `query` typed, matching
    /// case as `case` says; the bindings' changes counted `keys_seen`.
    pub(crate) fn new(query: &str, case: Case, facts: &Facts, keys_seen: u64) -> Self {
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
        };
        menu.fill(facts);
        if !query.is_empty() {
            let _ = menu.picker.paste(query);
            menu.retyped();
        }
        menu
    }

    /// Makes the rows again, as the bindings have changed since they were
    /// made, keeping the row picked where it can.
    pub(crate) fn follow_keys(&mut self, facts: &Facts, keys_seen: u64) {
        if keys_seen == self.keys_seen {
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
        self.items.push("daemon coming");
        self.entries.push(Entry::Coming);
        self.ranking.add(&self.items);
        self.picker.clamp(self.ranking.len());
    }

    /// The row ranked `rank`'s name in the history.
    fn name_at(&mut self, rank: usize) -> Option<String> {
        let index = self.ranking.get(rank)?;
        Some(match &self.entries[index] {
            Entry::Option(info) => info.name.to_owned(),
            Entry::Keys(bound) => bound.name(),
            Entry::Coming => "daemon".to_owned(),
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
                    Outcome::Changed
                };
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
            (Stroke::Right, _) | (_, Some('l')) =>
                Hint::Action("Next value"),
                self.step(1, facts, history);
            (Stroke::Left, _) | (_, Some('h')) =>
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
            Some(Editing::Font {
                picker,
                items,
                ranking,
                ..
            }) => {
                if picker.paste(text) == picker::Keyed::Typed {
                    ranking.set_query(picker.query(), items);
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
        // The row picked stays picked, among them all.
        let rank = keep.and_then(|name| {
            (0..self.ranking.len()).find(|&rank| self.name_at(rank).as_deref() == Some(&name))
        });
        self.picker.set_selected(rank.unwrap_or(0));
        Outcome::Changed
    }

    fn go(&mut self, by: isize) -> Outcome {
        let keyed = self.picker.go(by, self.ranking.len());
        keyed.outcome(|| Outcome::Changed)
    }

    fn go_to(&mut self, rank: usize) -> Outcome {
        let rank = rank.min(self.ranking.len().saturating_sub(1));
        self.picker.set_selected(rank);
        Outcome::Changed
    }

    /// Moves what's picked by `by` rows, as the mouse's wheel does.
    pub(crate) fn scroll(&mut self, by: isize) -> Outcome {
        match &mut self.editing {
            Some(Editing::Font {
                picker, ranking, ..
            }) => picker.go(by, ranking.len()).outcome(|| Outcome::Changed),
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
        if let Some(Editing::Font {
            picker, ranking, ..
        }) = &mut self.editing
        {
            if !picker.click(row, ranking.len()) {
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
            None | Some(Entry::Coming) => Outcome::Nothing,
            Some(Entry::Option(info)) => {
                if let Some(by) = facts.locked(info) {
                    return self.open_set_by(by.clone(), facts);
                }
                match info.kind {
                    Kind::Bool | Kind::Choice(_) => self.step(1, facts, history),
                    Kind::Text if info.name == "font" => {
                        self.editing = Some(Editing::Font {
                            families: Vec::new(),
                            picker: Picker::default(),
                            items: Items::default(),
                            ranking: Ranking::new(Case::default(), false),
                        });
                        Outcome::Fonts
                    }
                    _ => {
                        let text = typed_value(&facts.store.get(info));
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
                    return self.open_set_by(by, facts);
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

    /// Opens where `by` set it, if it says, or says it doesn't.
    fn open_set_by(&mut self, by: SetBy, facts: &Facts) -> Outcome {
        if let Some(place) = by.at {
            return Outcome::Open(place);
        }
        let said = format!("{}, at no line clackatoo knows", facts.said(&by));
        self.refuse(&said)
    }

    /// `o`: opens the line that set the row's value, if a line did.
    fn open(&mut self, facts: &Facts) -> Outcome {
        let by = match self.picked() {
            Some(Entry::Option(info)) => facts.store.set_by_in(facts.window, info),
            Some(Entry::Keys(bound)) => facts.keys.bound_by(bound.modes[0], &bound.keys),
            _ => return Outcome::Nothing,
        };
        if by.at.is_none() {
            return self.refuse("no line of config set it");
        }
        self.open_set_by(by, facts)
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
        if let Some(by) = facts.locked(info) {
            let said = format!(
                "{} sets {}, which outdoes what the menu saves: Enter opens it",
                by.place(facts.dir).unwrap_or_else(|| "your config".into()),
                info.name
            );
            return self.refuse(&said);
        }
        let undo = Undo::Option {
            info,
            live: facts.store.get(info),
            saved: facts.saved(info),
        };
        // One read only as a window opens is only saved, so it's as it was
        // if what's saved is.
        let unchanged = facts.saved(info) == saved
            && (info.restart
                || facts.store.get(info) == live
                    && facts.store.local(facts.window, info).is_none());
        if unchanged {
            return Outcome::Nothing;
        }
        history
            .0
            .entry(info.name.to_owned())
            .or_default()
            .push(undo);
        Outcome::Change { info, live, saved }
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
        let Some(undo) = history.0.get_mut(&name).and_then(Vec::pop) else {
            return self.refuse(&format!("the menu hasn't changed {name}"));
        };
        match undo {
            Undo::Option { info, live, saved } => {
                if let Some(by) = facts.locked(info) {
                    let said = format!(
                        "{} sets {} now, so there's nothing to undo",
                        by.place(facts.dir).unwrap_or_else(|| "your config".into()),
                        info.name
                    );
                    return self.refuse(&said);
                }
                Outcome::Change { info, live, saved }
            }
            Undo::Keys(changes) if facts.automating => {
                // Kept, for the user to undo.
                history.0.entry(name).or_default().push(Undo::Keys(changes));
                self.refuse(AGENTS_SAVE_NO_KEYS)
            }
            Undo::Keys(changes) => Outcome::Keys(changes),
        }
    }

    /// `x`: takes away this window's own value of the option picked.
    fn clear_local(&mut self, facts: &Facts) -> Outcome {
        let Some(Entry::Option(info)) = self.picked() else {
            return Outcome::Nothing;
        };
        if facts.store.local(facts.window, info).is_none() {
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
        let by = facts.keys.bound_by(bound.modes[0], &bound.keys);
        let (Binding::Keys(to), Source::Settings) = (&bound.binding, by.source) else {
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
        history
            .0
            .entry(bound.name())
            .or_default()
            .push(Undo::Keys(vec![(mapping.clone(), true)]));
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
        history
            .0
            .entry(bound.name())
            .or_default()
            .push(Undo::Keys(undo));
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
            Some(Editing::Font {
                families,
                picker,
                items,
                ranking,
            }) => match stroke {
                Stroke::Escape
                | Stroke::Char {
                    ch: 'c',
                    ctrl: true,
                } => {
                    self.editing = None;
                    Outcome::Changed
                }
                Stroke::Enter => {
                    let Some(index) = ranking.get(picker.selected()) else {
                        return Outcome::Nothing;
                    };
                    // The first is the system's font, which is no family.
                    let family = if index == 0 {
                        String::new()
                    } else {
                        families[index - 1].clone()
                    };
                    self.editing = None;
                    let info = crate::options::find("font").expect("an option");
                    let value = Value::Text(family);
                    self.change(info, value.clone(), Some(value), facts, history)
                }
                stroke => match picker.key(stroke, ranking.len()) {
                    Some(keyed) => keyed.outcome(|| {
                        ranking.set_query(picker.query(), items);
                        Outcome::Changed
                    }),
                    None => Outcome::Nothing,
                },
            },
            Some(Editing::Keys { .. }) | None => Outcome::Nothing,
        }
    }

    /// Whether it waits for the fonts installed, which [`fonts`](Self::fonts)
    /// gives it.
    pub(crate) fn wants_fonts(&self) -> bool {
        matches!(&self.editing, Some(Editing::Font { items, .. }) if items.len() == 0)
    }

    /// Takes in the font families installed, as `fc-list : family` lists
    /// them, for the font picker, the one in use, `font`, picked.
    pub(crate) fn fonts(&mut self, mut installed: Vec<String>, font: &str) {
        let Some(Editing::Font {
            families,
            picker,
            items,
            ranking,
        }) = &mut self.editing
        else {
            return;
        };
        installed.sort_by_key(|family| family.to_lowercase());
        installed.dedup();
        items.clear();
        ranking.clear();
        items.push(SYSTEM_FONT);
        for family in &installed {
            items.push(family);
        }
        ranking.add(items);
        let at = (installed.iter())
            .position(|family| family == font)
            .map_or(0, |at| at + 1);
        picker.set_selected(at);
        *families = installed;
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
        if let Some(Editing::Font {
            families,
            picker,
            items,
            ranking,
        }) = &mut self.editing
        {
            let font = FontPicker {
                families,
                picker,
                items,
                ranking,
            };
            return font.view(&facts.store.font, side, (rows, cols), preview);
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
            place: None,
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
        let mut tag: Option<(String, Ink)> = None;
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
        match &self.entries[index] {
            Entry::Option(info) => {
                let changed = facts.value(info) != info.default_value();
                let name = format!("{:<width$}", info.name, width = self.name_cols + 2);
                lit.extend(matched_in(info.name, 0));
                lit.resize(name.chars().count(), false);
                pieces.push((name, if changed { Ink::Bold } else { Ink::Plain }));
                pieces.push((shown_value(info, &facts.value(info)), Ink::Plain));
                tag = if let Some(by) = facts.locked(info) {
                    Some((short_place(by), Ink::Muted))
                } else if facts.store.local(facts.window, info).is_some() {
                    Some(("this window's own".into(), Ink::Muted))
                } else if !info.restart
                    && facts.store.get(info) != facts.store.configured_value(info)
                {
                    Some(("not saved".into(), Ink::Caution))
                } else if info.restart
                    && facts.store.get(info) != facts.store.configured_value(info)
                {
                    Some(("for windows opened after".into(), Ink::Muted))
                } else {
                    None
                };
            }
            Entry::Keys(bound) => {
                let keys = display(&bound.keys);
                let name = format!("{keys:<width$}", width = self.name_cols + 2);
                lit.extend(matched_in(&keys, 0));
                lit.resize(name.chars().count(), false);
                pieces.push((name, Ink::Plain));
                pieces.push((bound.does(), Ink::Plain));
                let by = facts.keys.bound_by(bound.modes[0], &bound.keys);
                tag = Some(match by.source {
                    Source::Config if by.loading => (short_place(&by), Ink::Muted),
                    Source::Settings => ("settings.lua".into(), Ink::Muted),
                    _ => (KeyMode::letters(&bound.modes), Ink::Muted),
                });
            }
            Entry::Coming => {
                pieces.push(("Coming: the daemon's own settings".into(), Ink::Muted));
            }
        }
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
            let room = cols.saturating_sub(used + 2);
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
/// to find one, and the one picked.
struct FontPicker<'a> {
    families: &'a [String],
    picker: &'a mut Picker,
    items: &'a Items,
    ranking: &'a mut Ranking,
}

impl FontPicker<'_> {
    /// What to draw, `font` being the one in use, with `side` beside it,
    /// for a list `rows` rows tall and `cols` cells wide, and a preview so
    /// big, if there's room for one.
    fn view(
        self,
        font: &str,
        side: SideView,
        (rows, cols): (usize, usize),
        preview: Option<(usize, usize)>,
    ) -> FinderView {
        let Self {
            families,
            picker,
            items,
            ranking,
        } = self;
        let len = ranking.len();
        let shown: Vec<Row> = (picker.shown(rows, len))
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
            .collect();
        let waiting = items.len() == 0;
        FinderView {
            title: "Font",
            side: Some(side),
            place: None,
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
        Some(Entry::Coming) => vec![
            vec![("The daemon's own settings".into(), Ink::Bold)],
            vec![(
                "Coming: agents, the web app and this machine, now in the daemon's TOML files."
                    .into(),
                Ink::Plain,
            )],
        ],
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
fn option_detail(info: &Info, facts: &Facts) -> Vec<Said> {
    let mut title = vec![(info.name.to_owned(), Ink::Bold)];
    if !info.short.is_empty() {
        title.push((format!("  {}", info.short), Ink::Muted));
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
    let shared = facts.store.get(info);
    let configured = facts.store.configured_value(info);
    let from = match facts.store.configured(info) {
        // Saved since the config loaded, at no line yet.
        Some((_, by)) if by.source == Source::Settings && by.at.is_none() => {
            format!("saved in {}", crate::options::persist::file(info))
        }
        Some((_, by)) => facts.said(by),
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
        // windows opened after.
        if info.restart {
            said.push(labelled("Now", now, Ink::Muted));
        } else {
            said.push(labelled("Now", now, Ink::Caution));
            said.push(labelled(
                "",
                facts.said(&facts.store.set_by(info)),
                Ink::Muted,
            ));
        }
    }
    if let Some((value, by)) = facts.store.local(facts.window, info) {
        let own = format!("this window's own: {}", shown_value(info, value));
        said.push(labelled("Here", own, Ink::Plain));
        said.push(labelled("", facts.said(by), Ink::Muted));
    }
    let file = crate::options::persist::file(info).to_owned();
    said.push(labelled("Saved in", file, Ink::Plain));
    let later = match info.scope {
        Scope::Startup(_) if info.restart => Some("takes effect for windows opened after"),
        Scope::Startup(_) => Some("takes effect for windows opened after, and this one now"),
        Scope::Window | Scope::Daemon => None,
    };
    if let Some(later) = later {
        said.push(labelled("", later.into(), Ink::Muted));
    }
    said.push(Vec::new());
    if let Some(by) = facts.locked(info) {
        let locked = format!(
            "Locked: {}, which outdoes what the menu saves",
            facts.said(by)
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
    if facts.store.local(facts.window, info).is_some() {
        pairs.push(("x", "clears this window's"));
    }
    said.push(key_hints(&pairs));
    said
}

/// What the preview says of `bound`'s keys.
fn keys_detail(bound: &Bound, facts: &Facts) -> Vec<Said> {
    let spoken: Vec<String> = bound.keys.iter().map(Chord::spoken).collect();
    let by = facts.keys.bound_by(bound.modes[0], &bound.keys);
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
fn shown_value(info: &Info, value: &Value) -> String {
    match value {
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
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
}
