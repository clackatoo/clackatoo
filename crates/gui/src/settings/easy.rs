//! The settings menu's Easy mode: pages of the options most people change,
//! each a row with a control, as [`crate::options::easy`] lists them.
//!
//! What a row shows is what's configured, what the menu saves: an option
//! changed since the config loaded, with `:set` or a key, says what it
//! is currently under its help. A row the user's own config sets as it
//! loads is locked, greyed, with the place it's set in, which a click or
//! Enter opens. Changes, undo and putting a row back as it starts go the
//! way Advanced's do, through the same history, so a change made in one
//! mode is undone in the other.
//!
//! The keys go to the pages down the side, or to the rows of the page
//! picked: `j` and `k` move, Tab goes from control to control and on to
//! the next page, `}` and `{` go a page at a time, `h` and `l` step a
//! value, and Enter or Space changes it. `/` searches both modes at once:
//! what Easy has, then what only Advanced has, which Enter switches to.

use super::find;
use super::view::{Card, ControlView, EasyView, Part, Preview, RowView, SectionView, SettingsHit};
use super::{
    Bound, Editing, Entry as AdvancedEntry, Facts, FontPicker, History, Mode, Outcome, Set,
    SettingsMenu, bindings, short_place, shown_value, typed_value,
};
use crate::desk::Binding;
use crate::finder::rank::{Items, Ranking};
use crate::keymap::{KeyMode, display};
use crate::options::Window as _;
use crate::options::easy::{self, Control, Entry, Page};
use crate::options::{Case, Decimal, Info, Kind, Value};
use crate::render::Ink;
use clackatoo_protocol::Color;

/// Where Easy is, and what its search goes by.
#[derive(Debug)]
pub(super) struct Easy {
    /// The page picked, by its place among [`Page::ALL`].
    page: usize,
    /// The keys go to the pages, not the rows.
    on_side: bool,
    /// The row the keys are on.
    focus: usize,
    /// The first part of the page shown, as the window last drew it, and
    /// whether to bring the row focused into view.
    top: usize,
    reveal: bool,
    /// The keys moved last, not the mouse: what they're on is shown.
    keys: bool,
    /// A slider being dragged: its row, and the value it's at.
    sliding: Option<Sliding>,
    /// Each entry's words, which a search matches.
    items: Items,
    ranking: Ranking,
}

/// A slider being dragged: its row and entry, kept from when the button
/// went down, whatever the keys do meanwhile, and the value it's at.
#[derive(Debug)]
struct Sliding {
    row: usize,
    entry: &'static Entry,
    value: Value,
}

/// A row Easy shows: an entry of its own, or, found by a search, one only
/// Advanced has, by its place among Advanced's rows.
#[derive(Clone, Copy, Debug)]
enum Shown {
    Entry(&'static Entry),
    Advanced(usize),
}

/// How many of what only Advanced has a search shows.
const ADVANCED_FOUND: usize = 8;

/// What the foot says until something's saved.
const SAVES: &str = "Changes save as you make them";

impl Easy {
    pub(super) fn new(case: Case) -> Self {
        let mut items = Items::default();
        for entry in easy::ENTRIES {
            items.push(&format!(
                "{} {} {} {} {}",
                entry.label,
                entry.help,
                entry.section,
                entry.page.title(),
                entry.control.options().join(" ")
            ));
        }
        let mut ranking = Ranking::new(case, false);
        ranking.add(&items);
        Self {
            page: 0,
            on_side: false,
            focus: 0,
            top: 0,
            reveal: false,
            keys: false,
            sliding: None,
            items,
            ranking,
        }
    }

    /// What's typed changed: the entries are matched against it again,
    /// and the first found is focused.
    pub(super) fn retyped(&mut self, query: &str) {
        self.ranking.set_query(query, &self.items);
        self.focus = 0;
        self.top = 0;
    }

    /// Typing stopped, keeping what's found: the keys go to its rows.
    pub(super) fn found(&mut self) {
        self.on_side = false;
        self.focus = 0;
        self.keyed();
    }

    /// The keys moved: what they're on is shown, and brought into view.
    fn keyed(&mut self) {
        self.keys = true;
        self.reveal = true;
    }

    /// The wheel turned `by` rows' worth.
    pub(super) fn scroll(&mut self, by: isize) -> Outcome {
        self.top = self.top.saturating_add_signed(by);
        self.reveal = false;
        Outcome::Changed
    }

    /// The page as the window last drew it: from its part `top`.
    pub(super) fn drawn(&mut self, top: usize) {
        self.top = top;
        self.reveal = false;
    }

    fn page(&self) -> Page {
        Page::ALL[self.page.min(Page::ALL.len() - 1)]
    }
}

impl SettingsMenu {
    /// Whether a search shows what it found in place of a page.
    fn searching(&self) -> bool {
        !self.picker.query().is_empty()
    }

    /// The rows Easy shows, under their headings: the page's, or what a
    /// search found.
    fn easy_sections(&mut self) -> Vec<(String, Vec<Shown>)> {
        // The daemon's, once it's said them.
        let daemon = self.daemon;
        let shown = |entry: &Entry| daemon || !entry.daemons();
        if !self.searching() {
            let mut sections: Vec<(String, Vec<Shown>)> = Vec::new();
            for entry in easy::on(self.easy.page()).filter(|entry| shown(entry)) {
                match sections.last_mut() {
                    Some((heading, rows)) if heading == entry.section => {
                        rows.push(Shown::Entry(entry));
                    }
                    _ => sections.push((entry.section.to_owned(), vec![Shown::Entry(entry)])),
                }
            }
            return sections;
        }
        let ranking = &mut self.easy.ranking;
        let found: Vec<Shown> = (0..ranking.len())
            .filter_map(|rank| ranking.get(rank))
            .map(|index| &easy::ENTRIES[index])
            .filter(|entry| shown(entry))
            .map(Shown::Entry)
            .collect();
        // What only Advanced has: options Easy doesn't show, and keys
        // other than those its Keys page does.
        let curated: Vec<Binding> = (easy::on(Page::Keys))
            .filter_map(|entry| match entry.control {
                Control::Keys(action) => Some(Binding::Window(action)),
                _ => None,
            })
            .collect();
        let advanced: Vec<Shown> = (0..self.ranking.len())
            .filter_map(|rank| self.ranking.get(rank))
            .filter(|&index| match &self.entries[index] {
                AdvancedEntry::Option(info) => !easy::shows(info.name),
                AdvancedEntry::Keys(bound) => !curated.contains(&bound.binding),
            })
            .take(ADVANCED_FOUND)
            .map(Shown::Advanced)
            .collect();
        let mut sections = Vec::new();
        if !found.is_empty() {
            sections.push(("In Easy".to_owned(), found));
        }
        if !advanced.is_empty() {
            sections.push(("Only in Advanced".to_owned(), advanced));
        }
        sections
    }

    /// Easy's rows, every section's, in order.
    fn easy_rows(&mut self) -> Vec<Shown> {
        (self.easy_sections().into_iter())
            .flat_map(|(_, rows)| rows)
            .collect()
    }

    /// The row the keys are on, if they're on a row.
    fn easy_focused(&mut self) -> Option<Shown> {
        if self.easy.on_side {
            return None;
        }
        self.easy_rows().get(self.easy.focus).copied()
    }

    /// Moves the keys `by` rows, or on the side, pages.
    pub(super) fn easy_go(&mut self, by: isize) -> Outcome {
        if self.easy.on_side {
            return self.easy_page(by);
        }
        let len = self.easy_rows().len();
        self.easy.focus = (self.easy.focus.saturating_add_signed(by)).min(len.saturating_sub(1));
        self.easy.keyed();
        Outcome::Changed
    }

    /// The row `rank`, or the last if there aren't so many.
    pub(super) fn easy_go_to(&mut self, rank: usize) -> Outcome {
        let len = self.easy_rows().len();
        self.easy.on_side = false;
        self.easy.focus = rank.min(len.saturating_sub(1));
        self.easy.keyed();
        Outcome::Changed
    }

    /// Tab: the next control, or from the last, the next page's first; or
    /// the one before, and from the first, the pages.
    pub(super) fn easy_tab(&mut self, forward: bool) -> Outcome {
        let len = self.easy_rows().len();
        match (self.easy.on_side, forward) {
            (true, true) => self.easy.on_side = false,
            (true, false) => return self.easy_page(-1),
            (false, true) if self.easy.focus + 1 >= len => {
                if self.searching() {
                    return Outcome::Nothing;
                }
                self.easy.page = (self.easy.page + 1) % Page::ALL.len();
                self.easy.focus = 0;
                self.easy.top = 0;
            }
            (false, true) => self.easy.focus += 1,
            (false, false) if self.easy.focus == 0 => self.easy.on_side = true,
            (false, false) => self.easy.focus -= 1,
        }
        self.easy.keyed();
        Outcome::Changed
    }

    /// The page `by` pages on, or back; a search found is put away.
    pub(super) fn easy_page(&mut self, by: isize) -> Outcome {
        self.easy.keys = true;
        let len = isize::try_from(Page::ALL.len()).unwrap_or(1);
        let at = isize::try_from(self.easy.page).unwrap_or(0);
        self.show_page(usize::try_from((at + by).rem_euclid(len)).unwrap_or(0))
    }

    /// Shows page `page`, by its place down the side.
    fn show_page(&mut self, page: usize) -> Outcome {
        if self.searching() {
            self.picker.clear();
            self.ranking.set_query("", &self.items);
            self.easy.retyped("");
        }
        self.typing = false;
        self.easy.page = page.min(Page::ALL.len() - 1);
        self.easy.focus = 0;
        self.easy.top = 0;
        Outcome::Changed
    }

    /// Switches to the other mode, keeping what's typed, and what's picked
    /// where the other mode has it too.
    pub(super) fn switch_mode(&mut self) -> Outcome {
        let mode = match self.mode {
            Mode::Easy => Mode::Advanced,
            Mode::Advanced => Mode::Easy,
        };
        self.go_to_mode(mode)
    }

    /// Switches to `mode`, if it's not the one shown.
    pub(super) fn go_to_mode(&mut self, mode: Mode) -> Outcome {
        if mode == self.mode {
            return Outcome::Nothing;
        }
        self.editing = None;
        self.easy.sliding = None;
        match mode {
            Mode::Advanced => {
                // The row focused, picked among Advanced's.
                let picked = match self.easy_focused() {
                    Some(Shown::Advanced(index)) => Some(index),
                    Some(Shown::Entry(entry)) => {
                        let names = entry.control.options();
                        (self.entries.iter()).position(|each| match each {
                            AdvancedEntry::Option(info) => names.first() == Some(&info.name),
                            AdvancedEntry::Keys(bound) => matches!(
                                entry.control,
                                Control::Keys(action) if bound.binding == Binding::Window(action)
                            ),
                        })
                    }
                    None => None,
                };
                self.mode = Mode::Advanced;
                if let Some(index) = picked
                    && let Some(rank) =
                        (0..self.ranking.len()).find(|&rank| self.ranking.get(rank) == Some(index))
                {
                    self.picker.set_selected(rank);
                }
            }
            Mode::Easy => {
                self.mode = Mode::Easy;
                // The option picked, on its page, if Easy has it and
                // nothing's searched.
                if !self.searching()
                    && let Some(AdvancedEntry::Option(info)) = self.picked()
                    && let Some(entry) = (easy::ENTRIES.iter())
                        .find(|entry| entry.control.options().contains(&info.name))
                {
                    self.easy.page = Page::ALL
                        .iter()
                        .position(|&page| page == entry.page)
                        .unwrap_or(0);
                    self.easy.focus = easy::on(entry.page)
                        .position(|each| std::ptr::eq(each, entry))
                        .unwrap_or(0);
                    self.easy.on_side = false;
                    self.easy.keyed();
                }
            }
        }
        Outcome::Mode(mode)
    }

    /// Enter or Space: changes the row focused, as its control does, or
    /// opens where a locked one is set; on the side, goes into the page.
    pub(super) fn easy_act(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        if self.easy.on_side {
            self.easy.on_side = false;
            self.easy.focus = 0;
            self.easy.keyed();
            return Outcome::Changed;
        }
        match self.easy_focused() {
            None => Outcome::Nothing,
            Some(Shown::Advanced(index)) => self.go_to_advanced(index),
            Some(Shown::Entry(entry)) => self.activate(entry, None, facts, history),
        }
    }

    /// Switches to Advanced, with its row `index` picked.
    /// A jump, not a switch: the menu still opens in the mode it was
    /// switched to last.
    fn go_to_advanced(&mut self, index: usize) -> Outcome {
        self.typing = false;
        self.go_to_mode(Mode::Advanced);
        if let Some(rank) =
            (0..self.ranking.len()).find(|&rank| self.ranking.get(rank) == Some(index))
        {
            self.picker.set_selected(rank);
        }
        Outcome::Changed
    }

    /// Changes `entry`'s options as its control does when it's pressed,
    /// or at `part` of it if it was clicked there.
    fn activate(
        &mut self,
        entry: &'static Entry,
        part: Option<Part>,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        if let Some(by) = locked(entry, facts) {
            return self.open_set_by(first_info(entry), by, facts);
        }
        match (&entry.control, part) {
            (Control::Segmented { choices, .. }, Some(Part::Segment(at))) => {
                let choice = usize::from(at).min(choices.len() - 1);
                self.pick_choice(entry, choice, facts, history)
            }
            (Control::Stepper { .. }, Some(Part::Less)) => {
                self.easy_step_entry(entry, -1, false, facts, history)
            }
            (Control::Stepper { .. }, Some(Part::More)) => {
                self.easy_step_entry(entry, 1, false, facts, history)
            }
            (Control::Slider { .. }, Some(Part::Slider(at))) => {
                self.slide_to(self.easy.focus, entry, at);
                self.easy_slid(facts, history)
            }
            (Control::Cards(_), Some(Part::Card(at))) => {
                let agents = self.agent_cards(facts);
                let Some((agent, ..)) = agents.get(usize::from(at)) else {
                    return Outcome::Nothing;
                };
                let info = find("agent").expect("an option");
                let value = Value::Text(agent.clone());
                self.change(info, value.clone(), Some(value), facts, history)
            }
            (Control::Toggle(_) | Control::Segmented { .. } | Control::Cards(_), _) => {
                self.easy_step_entry(entry, 1, true, facts, history)
            }
            (Control::Font(_), _) => {
                self.editing = Some(Editing::Font(FontPicker::default()));
                Outcome::Fonts
            }
            (
                Control::Text(name)
                | Control::Slider { option: name, .. }
                | Control::Stepper { option: name, .. },
                _,
            ) => {
                let info = find(name).expect("an option");
                let text = typed_value(&facts.configured_value(info));
                self.editing = Some(Editing::Value { info, text });
                Outcome::Changed
            }
            (Control::Keys(action), _) => match key_bound(*action, facts) {
                Some(bound) if bound.typed().is_some() => {
                    self.editing = Some(Editing::Keys {
                        bound,
                        keys: Vec::new(),
                    });
                    Outcome::Changed
                }
                _ => self.refuse("nothing binds that now: bind it in Advanced"),
            },
        }
    }

    /// `l` or `h`: the next value of the row focused, or the one before; on
    /// the side, `l` goes into the page.
    pub(super) fn easy_step(&mut self, by: isize, facts: &Facts, history: &mut History) -> Outcome {
        if self.easy.on_side {
            if by > 0 {
                return self.easy_act(facts, history);
            }
            return Outcome::Nothing;
        }
        match self.easy_focused() {
            Some(Shown::Entry(entry)) => {
                if let Some(by) = locked(entry, facts) {
                    let place = match first_info(entry) {
                        Some(info) => facts.place(info, &by),
                        None => by.place(facts.dir).unwrap_or_else(|| "your config".into()),
                    };
                    let said = format!(
                        "{place} sets it, which outdoes what the menu saves: Enter opens it"
                    );
                    return self.refuse(&said);
                }
                self.easy_step_entry(entry, by, false, facts, history)
            }
            _ => Outcome::Nothing,
        }
    }

    /// Steps `entry`'s value `by` on: its switch, its next choice or card,
    /// going round if `round`, or its number by its step.
    fn easy_step_entry(
        &mut self,
        entry: &'static Entry,
        by: isize,
        round: bool,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        match &entry.control {
            Control::Toggle(name) => {
                let info = find(name).expect("an option");
                let Value::Bool(on) = facts.configured_value(info) else {
                    return Outcome::Nothing;
                };
                let next = Value::Bool(!on);
                self.change(info, next.clone(), Some(next), facts, history)
            }
            Control::Segmented { choices, .. } => {
                let len = choices.len();
                let near = choice_of(entry, |info| facts.configured_value(info), true);
                let next = near.map_or(0, |at| stepped(at, by, len, round));
                self.pick_choice(entry, next, facts, history)
            }
            Control::Cards(_) => {
                let agents = self.agent_cards(facts);
                let len = agents.len();
                if len == 0 {
                    return Outcome::Nothing;
                }
                let at = agents.iter().position(|(.., picked)| *picked);
                let next = at.map_or(0, |at| stepped(at, by, len, round));
                let info = find("agent").expect("an option");
                let value = Value::Text(agents[next].0.clone());
                self.change(info, value.clone(), Some(value), facts, history)
            }
            Control::Slider { option, step, .. } => {
                // From the real value, within the option's own range: the
                // slider only shows one past its ends at the end.
                let info = find(option).expect("an option");
                let (low, high) = match info.kind {
                    Kind::Decimal { min, max } | Kind::Number { min, max, .. } => {
                        (min.saturating_mul(100), max.saturating_mul(100))
                    }
                    _ => return Outcome::Nothing,
                };
                let now = hundredths(&facts.configured_value(info));
                let next = if by > 0 {
                    now.saturating_add(*step).min(high)
                } else {
                    now.saturating_sub(*step).max(low)
                };
                let value = of_hundredths(info, next);
                self.change(info, value.clone(), Some(value), facts, history)
            }
            Control::Stepper { option, step, .. } => {
                let info = find(option).expect("an option");
                let Value::Number(now) = facts.configured_value(info) else {
                    return Outcome::Nothing;
                };
                let Kind::Number { min, max, .. } = info.kind else {
                    return Outcome::Nothing;
                };
                let next = if by > 0 {
                    now.saturating_add(*step).min(max)
                } else {
                    now.saturating_sub(*step).max(min)
                };
                self.change(
                    info,
                    Value::Number(next),
                    Some(Value::Number(next)),
                    facts,
                    history,
                )
            }
            Control::Font(_) | Control::Text(_) | Control::Keys(_) => Outcome::Nothing,
        }
    }

    /// Sets `entry`'s options to its choice `at`, together.
    fn pick_choice(
        &mut self,
        entry: &'static Entry,
        at: usize,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        let Control::Segmented { options, choices } = &entry.control else {
            return Outcome::Nothing;
        };
        let Some(choice) = choices.get(at) else {
            return Outcome::Nothing;
        };
        let sets: Vec<Set> = (options.iter().zip(choice.values))
            .filter_map(|(name, word)| {
                let info = find(name).ok()?;
                let value = choice_value(info, word)?;
                Some(Set {
                    info,
                    live: value.clone(),
                    saved: Some(value),
                })
            })
            .collect();
        self.change_all(sets, facts, history)
    }

    /// `r`: puts the row focused back as it starts.
    pub(super) fn easy_reset(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        let Some(Shown::Entry(entry)) = self.easy_focused() else {
            return Outcome::Nothing;
        };
        if let Control::Keys(action) = entry.control {
            return match key_bound(action, facts) {
                Some(_) => self.refuse("keys are taken out in Advanced, with d"),
                None => Outcome::Nothing,
            };
        }
        let sets = (entry.control.options().iter())
            .filter_map(|name| find(name).ok())
            .map(|info| Set {
                info,
                live: info.default_value(),
                saved: None,
            })
            .collect();
        self.change_all(sets, facts, history)
    }

    /// `u`: undoes the last change to the row focused.
    pub(super) fn easy_undo(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        let Some(Shown::Entry(entry)) = self.easy_focused() else {
            return Outcome::Nothing;
        };
        // The newest change to any of its options, made here or in
        // Advanced.
        let names: Vec<String> = match entry.control {
            Control::Keys(action) => match key_bound(action, facts) {
                Some(bound) => vec![bound.name()],
                None => return Outcome::Nothing,
            },
            _ => (entry.control.options().iter())
                .map(|&name| name.to_owned())
                .collect(),
        };
        self.undo_named(&names, facts, history)
    }

    /// `o`: opens the line that set the row focused, if a line did.
    pub(super) fn easy_open(&mut self, facts: &Facts) -> Outcome {
        let Some(Shown::Entry(entry)) = self.easy_focused() else {
            return Outcome::Nothing;
        };
        // Where it's locked first, as Enter and its place open.
        let by = locked(entry, facts).or_else(|| {
            (entry.control.options().iter())
                .filter_map(|name| find(name).ok())
                .map(|info| facts.set_by_in(info))
                .find(|by| by.at.is_some())
        });
        match by {
            Some(by) => self.open_set_by(first_info(entry), by, facts),
            None => self.refuse("no line of config set it"),
        }
    }

    /// A slider is dragged to `at` thousandths along: its value is shown,
    /// and the sample drawn at it, but nothing's set until it's let go.
    fn slide_to(&mut self, row: usize, entry: &'static Entry, at: u16) {
        let Control::Slider {
            option,
            min,
            max,
            step,
            ..
        } = entry.control
        else {
            return;
        };
        let Ok(info) = find(option) else { return };
        let span = f64::from(max - min);
        let along = f64::from(at.min(1000)) / 1000.0;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (along * span / f64::from(step)).round() as u32;
        let value = of_hundredths(info, (min + steps * step).min(max));
        self.easy.sliding = Some(Sliding { row, entry, value });
    }

    /// The slider dragged is let go: its value is set and saved.
    pub(crate) fn easy_slid(&mut self, facts: &Facts, history: &mut History) -> Outcome {
        let Some(Sliding { entry, value, .. }) = self.easy.sliding.take() else {
            return Outcome::Nothing;
        };
        let Some(info) = entry
            .control
            .options()
            .first()
            .and_then(|name| find(name).ok())
        else {
            return Outcome::Changed;
        };
        match self.change(info, value.clone(), Some(value), facts, history) {
            Outcome::Nothing => Outcome::Changed,
            outcome => outcome,
        }
    }

    /// The mouse, with its button held, moved to `at` thousandths along
    /// the slider being dragged.
    pub(crate) fn slide(&mut self, at: u16) -> Outcome {
        let Some(Sliding { row, entry, .. }) = self.easy.sliding else {
            return Outcome::Nothing;
        };
        self.slide_to(row, entry, at);
        Outcome::Changed
    }

    /// Whether a slider is being dragged.
    pub(crate) fn sliding(&self) -> bool {
        self.easy.sliding.is_some()
    }

    /// A click on `hit`, in either mode. One on a slider starts dragging
    /// it, until [`SettingsMenu::easy_slid`].
    pub(crate) fn pointer(
        &mut self,
        hit: SettingsHit,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        self.said = None;
        self.easy.keys = false;
        match hit {
            SettingsHit::Panel => Outcome::Nothing,
            SettingsHit::Backdrop | SettingsHit::Close => Outcome::Close,
            SettingsHit::Mode(mode) => self.go_to_mode(mode),
            SettingsHit::Advanced => self.go_to_mode(Mode::Advanced),
            SettingsHit::Search => {
                self.editing = None;
                self.typing = true;
                Outcome::Changed
            }
            SettingsHit::Page(page) => {
                self.editing = None;
                self.easy.on_side = true;
                self.show_page(usize::from(page))
            }
            SettingsHit::Font(row) => {
                let Some(Editing::Font(fonts)) = &mut self.editing else {
                    return Outcome::Nothing;
                };
                if !fonts.picker.click(usize::from(row), fonts.ranking.len()) {
                    return Outcome::Nothing;
                }
                self.edit_stroke(crate::vim::Stroke::Enter, facts, history)
            }
            SettingsHit::Row(row) | SettingsHit::Lock(row) | SettingsHit::Control(row, _) => {
                // A click elsewhere puts away what's being typed or picked.
                if matches!(self.editing, Some(Editing::Font(_) | Editing::Value { .. })) {
                    self.editing = None;
                }
                self.typing = false;
                self.easy.on_side = false;
                self.easy.focus = usize::from(row);
                let Some(shown) = self.easy_rows().get(usize::from(row)).copied() else {
                    return Outcome::Changed;
                };
                match (shown, hit) {
                    (Shown::Advanced(index), SettingsHit::Control(..)) => {
                        self.go_to_advanced(index)
                    }
                    (Shown::Entry(entry), SettingsHit::Lock(_)) => match locked(entry, facts) {
                        Some(by) => self.open_set_by(first_info(entry), by, facts),
                        None => Outcome::Changed,
                    },
                    (Shown::Entry(entry), SettingsHit::Control(_, Part::Slider(at))) => {
                        if locked(entry, facts).is_some() {
                            return Outcome::Changed;
                        }
                        self.slide_to(usize::from(row), entry, at);
                        Outcome::Changed
                    }
                    (Shown::Entry(entry), SettingsHit::Control(_, part)) => {
                        let outcome = self.activate(entry, Some(part), facts, history);
                        if outcome == Outcome::Nothing {
                            Outcome::Changed
                        } else {
                            outcome
                        }
                    }
                    _ => Outcome::Changed,
                }
            }
        }
    }

    /// The agents to show as cards: the daemon's, or until it's said,
    /// those clackatoo knows; and the one configured, if it's none of
    /// them. Each with what it's shown as, whether it's installed, and
    /// whether it's the one picked.
    fn agent_cards(&self, facts: &Facts) -> Vec<(String, String, bool, bool)> {
        let info = find("agent").expect("an option");
        let configured = match facts.configured_value(info) {
            Value::Text(agent) if !agent.is_empty() => agent,
            _ => "claude".to_owned(),
        };
        let mut agents: Vec<(String, String, bool)> = if self.agents.is_empty() {
            [
                ("claude", "Claude Code"),
                ("codex", "Codex"),
                ("gemini", "Gemini"),
            ]
            .map(|(agent, title)| (agent.to_owned(), title.to_owned(), true))
            .to_vec()
        } else {
            self.agents.clone()
        };
        if !agents.iter().any(|(agent, ..)| *agent == configured) {
            agents.push((configured.clone(), configured.clone(), true));
        }
        (agents.into_iter())
            .map(|(agent, title, installed)| {
                let picked = agent == configured;
                (agent, title, installed, picked)
            })
            .collect()
    }

    /// Takes in the agents the daemon knows: each one's name, title and
    /// whether it's installed.
    pub(crate) fn set_agents(&mut self, agents: Vec<(String, String, bool)>) {
        self.agents = agents;
    }

    /// Says what was saved, as the bar says it, or why not, along Easy's
    /// foot, as a sentence.
    pub(crate) fn saved(&mut self, said: &str, warning: bool) {
        let mut chars = said.chars();
        let said = (chars.next())
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        self.said_saved = Some((said, warning));
    }

    /// The page as the window last drew it, from its part `top`. Returns
    /// whether that changes what it shows.
    pub(crate) fn easy_drawn(&mut self, top: usize) -> bool {
        let changed = self.easy.top != top || self.easy.reveal;
        self.easy.drawn(top);
        changed
    }

    /// What Easy draws.
    #[allow(clippy::too_many_lines, reason = "the page, then each row")]
    pub(crate) fn easy_view(&mut self, facts: &Facts) -> EasyView {
        let sections = self.easy_sections();
        let searching = self.searching();
        let page = self.easy.page();
        let found = sections.iter().map(|(_, rows)| rows.len()).sum::<usize>();
        let (title, subtitle) = if searching {
            let query = self.picker.query();
            let subtitle = match found {
                0 => format!("Nothing matches “{query}”."),
                1 => format!("One setting matches “{query}”."),
                found => format!("{found} settings match “{query}”."),
            };
            ("Search".to_owned(), subtitle)
        } else {
            (page.title().to_owned(), page.subtitle().to_owned())
        };
        self.easy.focus = self.easy.focus.min(found.saturating_sub(1));
        // Every key bound, once, if a row shows keys.
        let keys_shown = (sections.iter().flat_map(|(_, rows)| rows)).any(|shown| {
            matches!(shown, Shown::Entry(entry) if matches!(entry.control, Control::Keys(_)))
        });
        let all = if keys_shown {
            bindings(facts.keys)
        } else {
            Vec::new()
        };
        // Whether a row shows the daemon's settings.
        let daemons = (sections.iter().flat_map(|(_, rows)| rows))
            .any(|shown| matches!(shown, Shown::Entry(entry) if entry.daemons()));
        let mut drawn = Vec::new();
        for (heading, rows) in sections {
            let mut views = Vec::new();
            for shown in rows {
                views.push(self.row_view(shown, searching, facts, &all));
            }
            drawn.push(SectionView {
                heading,
                rows: views,
            });
        }
        let fonts = match &mut self.editing {
            Some(Editing::Font(fonts)) => Some(fonts.easy_view(&facts.store.font, self.easy.focus)),
            _ => None,
        };
        // What's being typed or recorded says how, unless something more
        // was said.
        let editing = match &self.editing {
            Some(Editing::Value { .. }) => Some("Enter sets it · Esc goes back"),
            Some(Editing::Keys { .. }) => Some(
                "Press the keys, then Enter to save · Backspace takes one back · Esc goes back",
            ),
            _ => None,
        };
        // Why the daemon's config doesn't run, while it doesn't, on a page
        // of the daemon's settings.
        let failure = (facts.daemon)
            .and_then(|daemon| daemon.failure.as_ref())
            .filter(|_| daemons)
            .map(|failure| {
                format!("The daemon's config doesn't run, so it goes by what last did: {failure}")
            });
        let (footer, warning) = match (&self.said, editing, &self.said_saved, failure) {
            (Some((said, warning)), ..) | (None, None, Some((said, warning)), _) => {
                (said.clone(), *warning)
            }
            (None, Some(how), ..) => (how.to_owned(), false),
            (None, None, None, Some(failure)) => (failure, true),
            (None, None, None, None) => (SAVES.into(), false),
        };
        EasyView {
            machine: super::daemon_elsewhere(facts).filter(|_| daemons),
            query: self.picker.query().to_owned(),
            typing: self.typing,
            page: (!searching).then_some(self.easy.page),
            on_side: self.easy.on_side && self.easy.keys,
            title,
            subtitle,
            preview: (!searching && page.previews()).then(|| self.preview(facts)),
            sections: drawn,
            note: (!searching)
                .then(|| page.note().map(|note| (note, page == Page::Keys)))
                .flatten(),
            focus: (self.easy.keys && !self.easy.on_side && found > 0).then_some(self.easy.focus),
            top: self.easy.top,
            reveal: self.easy.reveal,
            footer,
            warning,
            fonts,
            sliding: self.easy.sliding.as_ref().map(|sliding| sliding.row),
        }
    }

    /// A row as it's drawn, with `all` the keys bound, if it shows keys.
    #[allow(clippy::too_many_lines, reason = "each kind of control")]
    fn row_view(&mut self, shown: Shown, searching: bool, facts: &Facts, all: &[Bound]) -> RowView {
        let entry = match shown {
            Shown::Entry(entry) => entry,
            Shown::Advanced(at) => {
                let (label, help) = match &self.entries[at] {
                    AdvancedEntry::Option(info) => {
                        (info.name.to_owned(), info.description.to_owned())
                    }
                    AdvancedEntry::Keys(bound) => (display(&bound.keys), bound.does()),
                };
                return RowView {
                    label,
                    help,
                    notes: Vec::new(),
                    lock: None,
                    control: ControlView::Advanced,
                };
            }
        };
        let infos: Vec<&'static Info> = (entry.control.options().iter())
            .filter_map(|name| find(name).ok())
            .collect();
        let lock = locked(entry, facts);
        let mut help = if searching {
            format!("{} · {}", entry.page.title(), entry.section)
        } else {
            entry.help.to_owned()
        };
        if let Some(by) = &lock
            && !searching
        {
            let file = by.at.as_ref().map_or_else(
                || "config".to_owned(),
                |at| {
                    std::path::Path::new(&*at.file).file_name().map_or_else(
                        || "config".into(),
                        |name| name.to_string_lossy().into_owned(),
                    )
                },
            );
            help = format!("Your {file} sets this, and it wins over what's saved here.");
        }
        let mut notes = Vec::new();
        // What it is now, if that's not what's configured.
        let differs = infos
            .iter()
            .any(|info| !info.restart && facts.get(info) != facts.configured_value(info));
        if differs && lock.is_none() {
            let now = match &entry.control {
                Control::Segmented { choices, .. } => {
                    let at = choice_of(entry, |info| facts.get(info), false);
                    (at.and_then(|at| choices.get(at))).map_or_else(
                        || {
                            (infos.iter())
                                .map(|info| crate::options::show_value(info, &facts.get(info)))
                                .collect::<Vec<_>>()
                                .join(" ")
                        },
                        |choice| choice.label.to_owned(),
                    )
                }
                _ => infos.first().map_or_else(String::new, |info| {
                    easy_value(entry, info, &facts.get(info))
                }),
            };
            notes.push((
                format!("Currently {now}, set since the config loaded."),
                Ink::Caution,
            ));
        }
        for info in &infos {
            if let Some((value, _)) = facts.local(info) {
                notes.push((
                    format!("This window's own: {}.", shown_value(info, value)),
                    Ink::Muted,
                ));
            }
        }
        // One the daemon reads only as it starts says so, lit while a
        // change waits for that.
        let restarts = (infos.iter())
            .filter(|info| info.scope == crate::options::Scope::Daemon && info.restart);
        let (mut restart, mut waits) = (false, false);
        for info in restarts {
            restart = true;
            waits |= facts.get(info) != facts.configured_value(info);
        }
        if restart && lock.is_none() {
            let mut chars = super::RESTARTS.chars();
            let said: String = (chars.next().map(|first| first.to_ascii_uppercase()))
                .into_iter()
                .chain(chars)
                .collect();
            let ink = if waits { Ink::Caution } else { Ink::Muted };
            notes.push((format!("{said}."), ink));
        }
        let configured =
            |name: &str| find(name).map_or(Value::Bool(false), |info| facts.configured_value(info));
        let control = match &entry.control {
            Control::Toggle(name) => ControlView::Toggle(configured(name) == Value::Bool(true)),
            Control::Segmented { choices, .. } => ControlView::Segmented {
                labels: choices.iter().map(|choice| choice.label).collect(),
                picked: picked_choice(entry, facts),
            },
            Control::Slider {
                option, min, max, ..
            } => {
                let value = match &self.easy.sliding {
                    Some(sliding) if std::ptr::eq(sliding.entry, entry) => sliding.value.clone(),
                    _ => configured(option),
                };
                let span = f64::from(max - min).max(1.0);
                let along = f64::from(hundredths(&value).clamp(*min, *max) - min) / span;
                #[allow(clippy::cast_possible_truncation)]
                let at = along as f32;
                let info = find(option).expect("an option");
                ControlView::Slider {
                    at,
                    value: easy_value(entry, info, &value),
                }
            }
            Control::Stepper { option, .. } => {
                let info = find(option).expect("an option");
                ControlView::Stepper(easy_value(entry, info, &configured(option)))
            }
            Control::Cards(_) => ControlView::Cards(
                (self.agent_cards(facts).into_iter())
                    .map(|(agent, title, installed, picked)| Card {
                        title,
                        command: agent,
                        note: (!installed).then(|| ("Not installed".to_owned(), Ink::Caution)),
                        picked,
                    })
                    .collect(),
            ),
            Control::Font(name) => {
                let info = find(name).expect("an option");
                ControlView::Font(shown_value(info, &configured(name)))
            }
            Control::Text(name) => match &self.editing {
                Some(Editing::Value { info, text }) if info.name == *name => ControlView::Field {
                    text: text.clone(),
                    editing: true,
                },
                _ => ControlView::Field {
                    text: typed_value(&configured(name)),
                    editing: false,
                },
            },
            Control::Keys(action) => {
                // Keys bound to the action, and keys bound to type those,
                // as the menu saves them.
                let normal = |bound: &&Bound| bound.modes.contains(&KeyMode::Normal);
                let own: Vec<&Bound> = (all.iter())
                    .filter(normal)
                    .filter(|bound| bound.binding == Binding::Window(*action))
                    .collect();
                let keys: Vec<String> = (all.iter())
                    .filter(normal)
                    .filter(|bound| match &bound.binding {
                        Binding::Window(each) => each == action,
                        Binding::Keys(to) => own.iter().any(|own| own.keys == *to),
                        Binding::Lua { .. } => false,
                    })
                    .map(|bound| display(&bound.keys))
                    .collect();
                let recording = match &self.editing {
                    Some(Editing::Keys { bound, keys: typed })
                        if bound.binding == Binding::Window(*action) =>
                    {
                        Some(if typed.is_empty() {
                            "Press keys…".to_owned()
                        } else {
                            display(typed)
                        })
                    }
                    _ => None,
                };
                ControlView::Keys { keys, recording }
            }
        };
        // A value being typed shows in its row.
        let control = match (&self.editing, &entry.control) {
            (
                Some(Editing::Value { info, text }),
                Control::Slider { option, .. } | Control::Stepper { option, .. },
            ) if info.name == *option => ControlView::Field {
                text: text.clone(),
                editing: true,
            },
            _ => control,
        };
        RowView {
            label: entry.label.to_owned(),
            help,
            notes,
            lock: lock.as_ref().map(short_place),
            control,
        }
    }

    /// Appearance's sample, as the options say it's drawn: line numbers,
    /// the cursor's line lit, and the size a slider is being dragged to.
    fn preview(&self, facts: &Facts) -> Preview {
        let get =
            |name: &str| find(name).map_or(Value::Bool(false), |info| facts.configured_value(info));
        let number = get("number") == Value::Bool(true);
        let relative = get("relativenumber") == Value::Bool(true);
        let width = match get("numberwidth") {
            Value::Number(width) => usize::try_from(width).unwrap_or(4).saturating_sub(1).max(2),
            _ => 3,
        };
        // Lines 41 and 42, the cursor on 42, as Vim numbers them.
        let numbers: [Option<String>; 2] = match (number, relative) {
            (false, false) => [None, None],
            (true, false) => [
                Some(format!("{:>width$} ", 41)),
                Some(format!("{:>width$} ", 42)),
            ],
            (false, true) => [
                Some(format!("{:>width$} ", 1)),
                Some(format!("{:>width$} ", 0)),
            ],
            (true, true) => [
                Some(format!("{:>width$} ", 1)),
                Some(format!("{:<width$} ", 42)),
            ],
        };
        let colour = |index| Ink::Color(Color::Indexed(index));
        let mut lines = vec![
            vec![
                ("~/projects/clackatoo".to_owned(), colour(4)),
                (" $ ".to_owned(), Ink::Muted),
                ("git log --oneline -1".to_owned(), Ink::Plain),
            ],
            vec![
                ("13b1220b".to_owned(), colour(3)),
                (
                    " fix(daemon): kept blocks ride on their rows".to_owned(),
                    Ink::Plain,
                ),
            ],
        ];
        let code = [
            vec![
                ("fn".to_owned(), colour(5)),
                (" ".to_owned(), Ink::Plain),
                ("main".to_owned(), colour(4)),
                ("() {".to_owned(), Ink::Plain),
            ],
            vec![
                ("    ".to_owned(), Ink::Plain),
                ("let".to_owned(), colour(5)),
                (" greeting = ".to_owned(), Ink::Plain),
                ("\"hello\"".to_owned(), colour(2)),
                (";".to_owned(), Ink::Plain),
            ],
        ];
        let mut cursor = (3, 0);
        for (at, (number, mut runs)) in numbers.into_iter().zip(code).enumerate() {
            if let Some(number) = number {
                runs.insert(0, (number, Ink::Muted));
            }
            if at == 1 {
                cursor.1 = runs
                    .iter()
                    .map(|(text, _)| crate::render::mono_width(text))
                    .sum();
            }
            lines.push(runs);
        }
        // The size being dragged to, against the size it's drawn at now.
        let scale = match &self.easy.sliding {
            Some(Sliding {
                entry,
                value: Value::Decimal(size),
                ..
            }) if entry.control.options() == ["fontsize"] => {
                let now = match facts.shown(find("fontsize").expect("an option")) {
                    Value::Decimal(now) => now.as_f32(),
                    _ => size.as_f32(),
                };
                size.as_f32() / now.max(1.0)
            }
            _ => 1.0,
        };
        Preview {
            lines,
            cursorline: (get("cursorline") == Value::Bool(true)).then_some(3),
            cursor,
            scale,
        }
    }
}

/// Where the user's own config sets one of `entry`'s options as it loads,
/// if it does: then the row is locked.
fn locked(entry: &Entry, facts: &Facts) -> Option<crate::options::SetBy> {
    (entry.control.options().iter())
        .filter_map(|name| find(name).ok())
        .find_map(|info| facts.locked(info))
}

/// The first of `entry`'s options, which says whose config its places are
/// in: none for keys.
fn first_info(entry: &Entry) -> Option<&'static Info> {
    (entry.control.options().first()).and_then(|name| find(name).ok())
}

/// `at`, a step `by` on among `len`: going round from one end to the other
/// if `round`, or else stopping there.
fn stepped(at: usize, by: isize, len: usize, round: bool) -> usize {
    match (at.checked_add_signed(by), round) {
        (Some(next), _) if next < len => next,
        // Before the first, or past the last.
        (None, true) | (Some(_), false) => len - 1,
        (Some(_), true) | (None, false) => 0,
    }
}

/// Which of `entry`'s choices its options are configured as, if any.
fn picked_choice(entry: &Entry, facts: &Facts) -> Option<usize> {
    choice_of(entry, |info| facts.configured_value(info), false)
}

/// Which of `entry`'s choices its options, as `value` gives them, are, or
/// do the same as; or with `near`, are nearest to, though no choice.
fn choice_of(entry: &Entry, value: impl Fn(&Info) -> Value, near: bool) -> Option<usize> {
    let Control::Segmented { options, choices } = &entry.control else {
        return None;
    };
    let now: Vec<Option<Value>> = (options.iter())
        .map(|name| find(name).ok().map(&value))
        .collect();
    let is = |values: &[&str]| {
        (options.iter().zip(values).zip(&now)).all(|((name, word), now)| {
            find(name).is_ok_and(|info| choice_value(info, word) == *now)
        })
    };
    choices.iter().position(|choice| {
        is(choice.values)
            || choice.also.iter().any(|values| is(values))
            || near && choice.near.iter().any(|values| is(values))
    })
}

/// The value a choice's `word` gives `info`'s option: `true` or `false`
/// for one that's on or off, or a word as `:set` takes it.
fn choice_value(info: &Info, word: &str) -> Option<Value> {
    match (info.kind, word) {
        (Kind::Bool, "true") => Some(Value::Bool(true)),
        (Kind::Bool, "false") => Some(Value::Bool(false)),
        (Kind::Bool, _) => None,
        _ => info.parse(word).ok(),
    }
}

/// The binding of `action` in normal mode, as the keys section lists it.
fn key_bound(action: crate::desk::Action, facts: &Facts) -> Option<Bound> {
    (bindings(facts.keys).into_iter()).find(|bound| {
        bound.binding == Binding::Window(action) && bound.modes.contains(&KeyMode::Normal)
    })
}

/// A number as hundredths, as a slider counts.
fn hundredths(value: &Value) -> u32 {
    match value {
        Value::Decimal(number) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let hundredths = (number.as_f64() * 100.0).round() as u32;
            hundredths
        }
        Value::Number(number) => number.saturating_mul(100),
        _ => 0,
    }
}

/// `hundredths` as a value of `info`'s option.
fn of_hundredths(info: &Info, hundredths: u32) -> Value {
    match info.kind {
        Kind::Decimal { .. } => Value::Decimal(
            Decimal::exact(f64::from(hundredths) / 100.0).unwrap_or(Decimal::whole(0)),
        ),
        _ => Value::Number(hundredths / 100),
    }
}

/// `value` of `info`'s option as `entry`'s row shows it, with its unit.
fn easy_value(entry: &Entry, info: &Info, value: &Value) -> String {
    let unit = match entry.control {
        Control::Slider { unit, .. } | Control::Stepper { unit, .. } => unit,
        _ => "",
    };
    let shown = match value {
        Value::Bool(true) => "on".to_owned(),
        Value::Bool(false) => "off".to_owned(),
        value => shown_value(info, value),
    };
    if unit.is_empty() {
        shown
    } else {
        format!("{shown} {unit}")
    }
}
