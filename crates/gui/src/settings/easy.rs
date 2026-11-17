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
//! what Easy has, then what only Advanced has, which Enter switches to,
//! then the servers, specialists, skills and rules of the pages that list
//! those, by name.
//!
//! Some pages list things rather than settings: MCP servers, specialists
//! and permission rules (see [`super::form`]) and skills. Each says under
//! its title what it lists and for where, has one button by its title,
//! which `a` presses too, and reads its list as it's shown, after
//! anything done on it, and when a lost connection is back. A click
//! anywhere acts only on a control: on the rest of a row, it takes the
//! keys there.

use super::find;
use super::form::{self, Catalogue, Did, Keyed, Listing, Notice};
use super::view::{
    ButtonView, Card, ControlView, EasyView, FieldView, Foot, FootButton, Part, Preview, RowView,
    Scrolling, SectionView, SettingsHit,
};
use super::{
    Bound, Catalogued, ChoicePicker, Editing, Entry as AdvancedEntry, Facts, FontPicker, History,
    Mode, Outcome, Set, SettingsMenu, bindings, short_place, shown_value, typed_value,
};
use crate::desk::Binding;
use crate::finder::rank::{Items, Ranking};
use crate::keymap::{KeyMode, display};
use crate::options::Window as _;
use crate::options::easy::{self, Control, Entry, Page};
use crate::options::{Case, Decimal, Info, Kind, Value};
use crate::render::Ink;
use crate::vim::Stroke;
use clackatoo_protocol::Color;

/// Where Easy is, and what its search goes by.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools, reason = "each is so or not")]
pub(super) struct Easy {
    /// The page picked, by its place among [`Page::ALL`].
    page: usize,
    /// The keys go to the pages, not the rows.
    on_side: bool,
    /// The row the keys are on.
    focus: usize,
    /// How far down the page is to be scrolled, in pixels, and as far
    /// as it can be, as the window last laid it out; whether to bring the
    /// row focused into view; and whether to go there at once, as a new
    /// page or search starts at its top.
    scroll: f32,
    most: f32,
    reveal: bool,
    jump: bool,
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

/// A row Easy shows.
#[derive(Clone, Copy, Debug)]
enum Shown {
    /// An entry of its own.
    Entry(&'static Entry),
    /// Found by a search, an option or keys only Advanced has, by its
    /// place among Advanced's rows.
    Advanced(usize),
    /// A skill, by its place among those listed.
    Skill(usize),
    /// A row of the page's catalogue, by its place among the
    /// catalogue's.
    Row(usize),
    /// Found by a search, a thing of a catalogue's, by its place in the
    /// catalogue's list.
    Found(Catalogued, usize),
}

/// Rows Easy shows under a heading, and the sentence under the heading,
/// if it has one.
struct Section {
    heading: String,
    note: Option<String>,
    rows: Vec<Shown>,
}

impl Section {
    fn new(heading: &str, rows: Vec<Shown>) -> Self {
        Self {
            heading: heading.to_owned(),
            note: None,
            rows,
        }
    }
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
            scroll: 0.0,
            most: 0.0,
            reveal: false,
            jump: false,
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
        self.start_at_top();
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

    /// The page starts again at its top, at once.
    fn start_at_top(&mut self) {
        self.scroll = 0.0;
        self.jump = true;
    }

    /// The wheel turned `by` pixels' worth, down for more than none: where
    /// the page is to be scrolled to now.
    pub(super) fn wheel(&mut self, by: f32) -> f32 {
        self.scroll = (self.scroll + by).clamp(0.0, self.most);
        self.reveal = false;
        self.scroll
    }

    pub(super) fn page(&self) -> Page {
        Page::ALL[self.page.min(Page::ALL.len() - 1)]
    }
}

impl SettingsMenu {
    /// The catalogue whose page is shown, if one's is.
    pub(super) fn catalogue_shown(&self) -> Option<Catalogued> {
        if self.mode != Mode::Easy || self.searching() {
            return None;
        }
        match self.easy.page() {
            Page::Mcp => Some(Catalogued::Servers),
            Page::Specialists => Some(Catalogued::Specialists),
            Page::Permissions => Some(Catalogued::Rules),
            _ => None,
        }
    }

    /// The catalogue whose page is shown, to look at.
    fn catalogue(&self) -> Option<&dyn Catalogue> {
        Some(match self.catalogue_shown()? {
            Catalogued::Servers => &self.mcp,
            Catalogued::Specialists => &self.specialists,
            Catalogued::Rules => &self.permissions,
        })
    }

    /// The catalogue whose page is shown, to change, and where the keys
    /// are on it.
    fn catalogue_mut(&mut self) -> Option<(&mut dyn Catalogue, &mut Easy)> {
        let catalogue: &mut dyn Catalogue = match self.catalogue_shown()? {
            Catalogued::Servers => &mut self.mcp,
            Catalogued::Specialists => &mut self.specialists,
            Catalogued::Rules => &mut self.permissions,
        };
        Some((catalogue, &mut self.easy))
    }

    /// What's said round the form the page shows, if it shows one.
    fn form_shown(&self) -> Option<form::FormView> {
        self.catalogue()?.form()
    }

    /// Whether the page shows a form with changes that aren't saved.
    pub(super) fn form_changed(&self) -> bool {
        self.form_shown().is_some_and(|form| form.changed)
    }

    /// Whether the page shown is a form, rather than a list or settings.
    pub(super) fn form_open(&self) -> bool {
        self.form_shown().is_some()
    }

    /// How many rows the page has before its catalogue's: the settings a
    /// list shares its page with.
    fn before_catalogue(&self) -> usize {
        match self.catalogue_shown() {
            Some(Catalogued::Rules) if !self.form_open() => self.permission_entries().len(),
            _ => 0,
        }
    }

    /// The row the keys are on, by its place among the page's
    /// catalogue's.
    pub(super) fn easy_focus(&self) -> usize {
        self.easy.focus.saturating_sub(self.before_catalogue())
    }

    /// The page shows something else of its catalogue's, a form or its
    /// list again: the keys go to its row `row`, from its top.
    pub(super) fn easy_went(&mut self, row: usize) {
        self.easy.focus = self.before_catalogue() + row;
        self.easy.start_at_top();
        self.easy.on_side = false;
        self.easy.keyed();
    }

    /// The row the keys are on is now the catalogue's row `row`, as its
    /// list was read again: they stay on it, and nothing scrolls.
    pub(super) fn easy_keep_focus(&mut self, row: usize) {
        self.easy.focus = self.before_catalogue() + row;
    }

    /// Shows `catalogue`'s page, with nothing searched for.
    pub(super) fn show_catalogue(&mut self, catalogue: Catalogued) {
        let page = match catalogue {
            Catalogued::Servers => Page::Mcp,
            Catalogued::Specialists => Page::Specialists,
            Catalogued::Rules => Page::Permissions,
        };
        self.mode = Mode::Easy;
        let at = Page::ALL.iter().position(|each| *each == page).unwrap_or(0);
        self.show_page(at);
        self.easy.on_side = false;
        self.easy.keyed();
    }

    /// Carries out what pressing or stepping a row of the page's
    /// catalogue did.
    fn did(&mut self, did: Did) -> Outcome {
        match did {
            Did::Nothing => Outcome::Nothing,
            Did::Changed => Outcome::Changed,
            Did::Went(row) => {
                self.easy_went(row);
                Outcome::Changed
            }
            Did::Outcome(outcome) => outcome,
            Did::Pick(pick) => {
                self.editing = Some(Editing::Choice(ChoicePicker::new(pick, self.easy_focus())));
                Outcome::Changed
            }
        }
    }

    /// Enter on row `at` of the page's catalogue, or a click on `part` of
    /// its control.
    fn catalogue_press(&mut self, at: usize, part: Option<Part>) -> Outcome {
        let Some((catalogue, _)) = self.catalogue_mut() else {
            return Outcome::Nothing;
        };
        // What's been sent to be saved is what's saved: nothing in the
        // form changes until the daemon answers.
        if catalogue.form().is_some_and(|form| form.saving) {
            return Outcome::Nothing;
        }
        // A click in a box of text puts the caret there.
        let did = match part {
            Some(Part::Caret(caret)) => {
                form::begin(catalogue, at, usize::try_from(caret).ok());
                Did::Changed
            }
            part => {
                form::finish(catalogue);
                catalogue.press(at, part)
            }
        };
        self.did(did)
    }

    /// A key while a form is shown: every key is the form's.
    pub(super) fn form_stroke(&mut self, stroke: Stroke) -> Outcome {
        let Some((catalogue, easy)) = self.catalogue_mut() else {
            return Outcome::Nothing;
        };
        // What's been sent to be saved is what's saved: until the daemon
        // answers, the keys move about the form, and cancel it, no more.
        let moves = matches!(
            stroke,
            Stroke::Escape | Stroke::Tab | Stroke::BackTab | Stroke::Up | Stroke::Down
        );
        if catalogue.form().is_some_and(|form| form.saving) && !moves {
            return Outcome::Nothing;
        }
        match form::key(catalogue, &mut easy.focus, stroke) {
            Keyed::Nothing => Outcome::Nothing,
            Keyed::Changed => {
                // What's typed stays in view.
                easy.reveal = true;
                Outcome::Changed
            }
            Keyed::Moved => {
                easy.keyed();
                Outcome::Changed
            }
            Keyed::Save => {
                let did = catalogue.save();
                self.did(did)
            }
            Keyed::Cancel => self.cancel_form(),
            Keyed::Did(did) => self.did(did),
        }
    }

    /// Puts the form shown away, unsaved, for its list: one with changes
    /// asks first, and goes if it's asked again.
    fn cancel_form(&mut self) -> Outcome {
        if self.may_leave()
            && let Some((catalogue, _)) = self.catalogue_mut()
            && catalogue.form().is_some()
        {
            let row = catalogue.cancel();
            self.easy_went(row);
        }
        Outcome::Changed
    }

    /// Whether the menu may close, or show something else, as it's asked
    /// to: a form with changes isn't thrown away without asking, so the
    /// first time it's not, and the foot asks; asked again straight
    /// after, it is.
    pub(super) fn may_leave(&mut self) -> bool {
        if self.form_changed() && !self.confirming {
            self.discarding = true;
            return false;
        }
        true
    }

    /// Puts away the form shown, if one is, as the page is left: the
    /// user has agreed to, if it had changes.
    fn leave_form(&mut self) {
        if let Some((catalogue, _)) = self.catalogue_mut()
            && catalogue.form().is_some()
        {
            catalogue.cancel();
        }
    }

    /// The cog, or anything else outside the menu that closes it: whether
    /// it may close now (see [`SettingsMenu::may_leave`]).
    pub(crate) fn closing(&mut self) -> bool {
        self.confirming = std::mem::take(&mut self.discarding);
        self.said = None;
        self.may_leave()
    }

    /// Types `text` into the form shown. Returns whether it did.
    pub(super) fn form_paste(&mut self, text: &str) -> Outcome {
        let Some((catalogue, easy)) = self.catalogue_mut() else {
            return Outcome::Nothing;
        };
        if catalogue.form().is_some_and(|form| form.saving) {
            return Outcome::Nothing;
        }
        match form::paste(catalogue, easy.focus, text) {
            form::Pasted::Nowhere => Outcome::Nothing,
            form::Pasted::Typed => {
                easy.reveal = true;
                Outcome::Changed
            }
            form::Pasted::TooLong => {
                self.refuse("that's more than this box holds: none of it was typed")
            }
        }
    }

    /// The daemon listed the shared skills, or said why it can't.
    pub(crate) fn skills_came(&mut self, result: Result<Vec<clackatoo_protocol::Skill>, String>) {
        self.skills_feed.came(None);
        match result {
            Ok(skills) => {
                self.skills = skills;
                self.skills_listing = Listing::Ready;
            }
            Err(why) => self.skills_listing = Listing::Unavailable(why),
        }
    }

    /// Opens the file of the skill at `at`, if it has one, in an editor.
    fn open_skill(&self, at: usize) -> Outcome {
        (self.skills.get(at))
            .filter(|skill| is_skill_file(skill))
            .map_or(Outcome::Nothing, |skill| {
                Outcome::OpenFile(super::Place {
                    file: skill.path.clone().into(),
                    line: 1,
                })
            })
    }

    /// The Skills page's rows: the skills there are to use, each with a
    /// switch, then what was found but can't be used, with why.
    fn skill_sections(&self) -> Vec<Section> {
        let off = |skill: &clackatoo_protocol::Skill| self.skills_off.contains(&skill.name);
        let usable = |skill: &clackatoo_protocol::Skill| skill.problem.is_none() || off(skill);
        let rows = |wanted: bool| -> Vec<Shown> {
            (self.skills.iter().enumerate())
                .filter(|(_, skill)| usable(skill) == wanted)
                .map(|(at, _)| Shown::Skill(at))
                .collect()
        };
        let unused = rows(false);
        let mut sections = vec![Section::new("", rows(true))];
        if !unused.is_empty() {
            sections.push(Section {
                heading: "Not loaded".into(),
                note: Some(
                    "Found, but not offered to agents, each with why. A project's skills, \
                     skillsets and specialists load once you trust its folder, with :trust."
                        .into(),
                ),
                rows: unused,
            });
        }
        sections
    }

    /// Whether the skill at `at` is switched on, if it has a switch: one
    /// that could be used, on a daemon that keeps a list of those
    /// switched off.
    fn skill_switch(&self, at: usize, facts: &Facts) -> Option<bool> {
        let skill = self.skills.get(at)?;
        let info = find(SKILLS_OFF).ok()?;
        if !facts.daemon.is_some_and(|daemon| daemon.knows(info)) {
            return None;
        }
        let off = self.skills_off.contains(&skill.name);
        (skill.problem.is_none() || off).then_some(!off)
    }

    /// Enter on the skill at `at`, or a click on its switch: its switch
    /// goes over, if it has one; else its file opens.
    fn skill_pressed(&mut self, at: usize, facts: &Facts, history: &mut History) -> Outcome {
        if self.skill_switch(at, facts).is_some() {
            self.skill_switched(at, facts, history)
        } else {
            self.open_skill(at)
        }
    }

    /// Switches the skill at `at` off, or on again, by its name in the
    /// daemon's list of those switched off, saved as its other options
    /// are.
    fn skill_switched(&mut self, at: usize, facts: &Facts, history: &mut History) -> Outcome {
        let Some(name) = self.skills.get(at).map(|skill| skill.name.clone()) else {
            return Outcome::Nothing;
        };
        let Ok(info) = find(SKILLS_OFF) else {
            return Outcome::Nothing;
        };
        let mut off = self.skills_off.clone();
        match off.iter().position(|each| *each == name) {
            Some(listed) => {
                off.remove(listed);
            }
            None => off.push(name),
        }
        let value = Value::Text(off.join(", "));
        self.change(info, value.clone(), Some(value), facts, history)
    }

    /// Takes in which skills the daemon's option says are switched off.
    fn follow_skills_off(&mut self, facts: &Facts) {
        self.skills_off = match find(SKILLS_OFF).map(|info| facts.configured_value(info)) {
            Ok(Value::Text(names)) => names
                .split([',', ' '])
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
    }

    /// Goes to the page of `catalogue` and opens the thing at `at` in its
    /// list, as a search that found it does.
    fn open_found(&mut self, catalogue: Catalogued, at: usize) -> Outcome {
        let server = match catalogue {
            Catalogued::Servers => self.mcp.servers().get(at).map(|server| server.name.clone()),
            Catalogued::Specialists | Catalogued::Rules => None,
        };
        self.show_catalogue(catalogue);
        let went = match (catalogue, server) {
            (Catalogued::Servers, Some(name)) => self.mcp.open_named(&name),
            (Catalogued::Specialists, _) => self.specialists.open_listed(at),
            (Catalogued::Rules, _) => self.permissions.open_at(at),
            (Catalogued::Servers, None) => None,
        };
        if let Some(row) = went {
            self.easy_went(row);
        }
        Outcome::Changed
    }

    /// Takes in the daemon's rules as `facts` have them. The keys stay on
    /// the rule they're on, wherever a change made elsewhere puts it, so
    /// Enter never opens another.
    pub(super) fn follow_rules(&mut self, facts: &Facts) {
        let listed = self.catalogue_shown() == Some(Catalogued::Rules) && !self.form_open();
        let on = (listed && self.easy.focus >= self.before_catalogue())
            .then(|| self.permissions.rule_at(self.easy_focus()))
            .flatten();
        self.permissions.follow(facts);
        if let Some(row) = on.and_then(|rule| self.permissions.row_of(&rule)) {
            self.easy_keep_focus(row);
        }
    }

    /// The settings the Permissions page shows over its rules.
    fn permission_entries(&self) -> Vec<&'static Entry> {
        easy::on(Page::Permissions)
            .filter(|entry| self.daemon || !entry.daemons())
            .collect()
    }

    /// Whether a search shows what it found in place of a page.
    pub(super) fn searching(&self) -> bool {
        !self.picker.query().is_empty()
    }

    /// The rows Easy shows, under their headings: the page's, or what a
    /// search found.
    fn easy_sections(&mut self) -> Vec<Section> {
        // The daemon's, once it's said them.
        let daemon = self.daemon;
        let shown = |entry: &Entry| daemon || !entry.daemons();
        if self.searching() {
            return self.found_sections();
        }
        let page = self.easy.page();
        if let Some(catalogue) = self.catalogue() {
            // A list's rows, after any settings it shares its page with;
            // a form's alone.
            let mut sections = Vec::new();
            let mut at = 0;
            for (heading, note, rows) in catalogue.sections() {
                sections.push(Section {
                    heading: heading.to_owned(),
                    note,
                    rows: (at..at + rows).map(Shown::Row).collect(),
                });
                at += rows;
            }
            if page == Page::Permissions && !self.form_open() {
                let entries = self.permission_entries();
                let rows = entries.into_iter().map(Shown::Entry).collect();
                sections.insert(0, Section::new("New chats", rows));
            }
            return sections;
        }
        if page == Page::Skills {
            return self.skill_sections();
        }
        let mut sections: Vec<Section> = Vec::new();
        for entry in easy::on(page).filter(|entry| shown(entry)) {
            match sections.last_mut() {
                Some(section) if section.heading == entry.section => {
                    section.rows.push(Shown::Entry(entry));
                }
                _ => sections.push(Section::new(entry.section, vec![Shown::Entry(entry)])),
            }
        }
        sections
    }

    /// What a search found, under a heading for each kind of thing.
    fn found_sections(&mut self) -> Vec<Section> {
        let daemon = self.daemon;
        let shown = |entry: &Entry| daemon || !entry.daemons();
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
        // The things the catalogues list, by name and what's said of them:
        // every word typed is somewhere in it.
        let query = self.picker.query().to_lowercase();
        let matches = |text: String| {
            let text = text.to_lowercase();
            query.split_whitespace().all(|word| text.contains(word))
        };
        let servers = (self.mcp.servers().iter().enumerate())
            .filter(|(_, server)| matches(format!("mcp server {} {}", server.name, server.source)))
            .map(|(at, _)| Shown::Found(Catalogued::Servers, at))
            .collect();
        let specialists = (self.specialists.files().iter().enumerate())
            .filter(|(_, file)| {
                let definition = &file.definition;
                matches(format!(
                    "specialist {} {}",
                    definition.name, definition.settings
                ))
            })
            .map(|(at, _)| Shown::Found(Catalogued::Specialists, at))
            .collect();
        let skills = (self.skills.iter().enumerate())
            .filter(|(_, skill)| {
                matches(format!(
                    "skills {} {} {} {}",
                    skill.name, skill.description, skill.source, skill.path
                ))
            })
            .map(|(at, _)| Shown::Skill(at))
            .collect();
        let rules = (self.permissions.sentences().into_iter().enumerate())
            .filter(|(_, sentence)| matches(format!("permission rule {sentence}")))
            .map(|(at, _)| Shown::Found(Catalogued::Rules, at))
            .collect();
        let sections = vec![
            Section::new("In Easy", found),
            Section::new("Only in Advanced", advanced),
            Section::new("MCP servers", servers),
            Section::new("Specialists", specialists),
            Section::new("Skills", skills),
            Section::new("Permission rules", rules),
        ];
        (sections.into_iter())
            .filter(|section| !section.rows.is_empty())
            .collect()
    }

    /// Easy's rows, every section's, in order.
    fn easy_rows(&mut self) -> Vec<Shown> {
        (self.easy_sections().into_iter())
            .flat_map(|section| section.rows)
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
                self.easy.start_at_top();
                self.read_page();
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
        self.leave_form();
        self.easy.page = page.min(Page::ALL.len() - 1);
        self.easy.focus = 0;
        self.easy.start_at_top();
        self.read_page();
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
                    None | Some(Shown::Skill(_) | Shown::Row(_) | Shown::Found(..)) => None,
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
            Some(Shown::Advanced(index)) => self.go_to_advanced(index),
            Some(Shown::Entry(entry)) => self.activate(entry, None, facts, history),
            Some(Shown::Skill(at)) => self.skill_pressed(at, facts, history),
            Some(Shown::Row(at)) => self.catalogue_press(at, None),
            Some(Shown::Found(catalogue, at)) => self.open_found(catalogue, at),
            None => Outcome::Nothing,
        }
    }

    /// `a`: the page's one button, which adds to its list.
    pub(super) fn easy_add(&mut self) -> Outcome {
        let Some((catalogue, _)) = self.catalogue_mut() else {
            return Outcome::Nothing;
        };
        let did = catalogue.add();
        self.did(did)
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
            Some(Shown::Row(at)) => {
                let Some((catalogue, _)) = self.catalogue_mut() else {
                    return Outcome::Nothing;
                };
                let did = catalogue.step(at, by);
                self.did(did)
            }
            Some(Shown::Skill(at)) if self.skill_switch(at, facts).is_some() => {
                self.skill_switched(at, facts, history)
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
        let entry = match self.easy_focused() {
            Some(Shown::Entry(entry)) => entry,
            // A skill's file, which says what it does.
            Some(Shown::Skill(at)) => return self.open_skill(at),
            _ => return Outcome::Nothing,
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
        self.confirming = std::mem::take(&mut self.discarding);
        // What closes the menu, or shows something else, never throws a
        // form's changes away without asking.
        let leaves = matches!(
            hit,
            SettingsHit::Page(_)
                | SettingsHit::Mode(_)
                | SettingsHit::Advanced
                | SettingsHit::Search
                | SettingsHit::Backdrop
                | SettingsHit::Close
        );
        if leaves && !self.may_leave() {
            return Outcome::Changed;
        }
        self.easy.keys = false;
        // A click anywhere but on a control takes back the first press of
        // a button that asks for two, and puts away a list of choices.
        if !matches!(hit, SettingsHit::Control(..) | SettingsHit::Font(_)) {
            if let Some((catalogue, _)) = self.catalogue_mut() {
                catalogue.disarm();
            }
            if matches!(self.editing, Some(Editing::Choice(_))) {
                self.editing = None;
            }
        }
        match hit {
            SettingsHit::Panel => Outcome::Nothing,
            SettingsHit::Backdrop | SettingsHit::Close => Outcome::Close,
            SettingsHit::Mode(mode) => {
                self.leave_form();
                self.go_to_mode(mode)
            }
            SettingsHit::Advanced => {
                self.leave_form();
                self.go_to_mode(Mode::Advanced)
            }
            SettingsHit::Search => {
                self.leave_form();
                self.editing = None;
                self.typing = true;
                Outcome::Changed
            }
            SettingsHit::Page(page) => {
                self.editing = None;
                self.easy.on_side = true;
                self.show_page(usize::from(page))
            }
            SettingsHit::Primary => self.easy_add(),
            SettingsHit::Back | SettingsHit::Foot(Foot::Cancel) => self.cancel_form(),
            SettingsHit::Foot(Foot::Save) => {
                let Some((catalogue, _)) = self.catalogue_mut() else {
                    return Outcome::Nothing;
                };
                form::finish(catalogue);
                let did = catalogue.save();
                self.did(did)
            }
            SettingsHit::Font(row) if matches!(self.editing, Some(Editing::Choice(_))) => {
                let Some(Editing::Choice(choices)) = &mut self.editing else {
                    return Outcome::Nothing;
                };
                if !choices.click(usize::from(row)) {
                    return Outcome::Nothing;
                }
                self.edit_stroke(Stroke::Enter, facts, history)
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
                self.row_clicked(row, hit, facts, history)
            }
        }
    }

    /// A click on row `row`, as `hit` says where: on its control, the
    /// place it's locked at, or the rest of it.
    fn row_clicked(
        &mut self,
        row: u16,
        hit: SettingsHit,
        facts: &Facts,
        history: &mut History,
    ) -> Outcome {
        // A click elsewhere puts away what's being typed or picked.
        let put_away = matches!(
            self.editing,
            Some(Editing::Font(_) | Editing::Choice(_) | Editing::Value { .. })
        );
        if put_away {
            self.editing = None;
        }
        self.typing = false;
        self.easy.on_side = false;
        self.easy.focus = usize::from(row);
        let Some(shown) = self.easy_rows().get(usize::from(row)).copied() else {
            return Outcome::Changed;
        };
        // Only a control acts: a click on the rest of a row takes
        // the keys there, and finishes what's typed elsewhere.
        let part = match hit {
            SettingsHit::Control(_, part) => Some(part),
            _ => None,
        };
        match (shown, part) {
            (Shown::Row(_), None) if matches!(hit, SettingsHit::Lock(_)) => {
                self.open_rules_lock(facts)
            }
            (Shown::Row(_), None) => {
                if let Some((catalogue, _)) = self.catalogue_mut() {
                    form::finish(catalogue);
                }
                Outcome::Changed
            }
            (Shown::Row(at), Some(part)) => match self.catalogue_press(at, Some(part)) {
                Outcome::Nothing => Outcome::Changed,
                outcome => outcome,
            },
            // Its switch; or its button, which opens its file.
            (Shown::Skill(at), Some(Part::Whole)) => self.skill_pressed(at, facts, history),
            (Shown::Skill(at), Some(_)) => self.open_skill(at),
            (Shown::Found(catalogue, at), Some(_)) => self.open_found(catalogue, at),
            (Shown::Advanced(index), Some(_)) => self.go_to_advanced(index),
            (Shown::Entry(entry), None) if matches!(hit, SettingsHit::Lock(_)) => {
                match locked(entry, facts) {
                    Some(by) => self.open_set_by(first_info(entry), by, facts),
                    None => Outcome::Changed,
                }
            }
            (Shown::Entry(entry), Some(Part::Slider(at))) => {
                if locked(entry, facts).is_some() {
                    return Outcome::Changed;
                }
                self.slide_to(usize::from(row), entry, at);
                Outcome::Changed
            }
            (Shown::Entry(entry), Some(part)) => {
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

    /// Opens the line of the daemon's config that sets the permission
    /// rules, as a click on a locked rule's place does.
    fn open_rules_lock(&mut self, facts: &Facts) -> Outcome {
        let info = find("permissions.rules").ok();
        match self.permissions.locked_by().cloned() {
            Some(by) => self.open_set_by(info, by, facts),
            None => Outcome::Changed,
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

    /// Takes in the agents the daemon knows, each with its models, as far
    /// as it knows them: for the cards, and for a specialist's form.
    pub(crate) fn set_agents(&mut self, agents: &[clackatoo_protocol::Provider]) {
        use clackatoo_protocol::ProviderModels;
        self.agents_feed.came(None);
        self.agents = (agents.iter())
            .map(|provider| {
                let installed = !matches!(provider.models, ProviderModels::NotInstalled);
                (provider.agent.clone(), provider.title.clone(), installed)
            })
            .collect();
        self.specialists.agents = (agents.iter())
            .map(|provider| super::specialists::AgentChoice {
                name: provider.agent.clone(),
                title: provider.title.clone(),
                models: match &provider.models {
                    ProviderModels::Known(models) => (models.iter())
                        .map(|model| (model.value.clone(), model.name.clone()))
                        .collect(),
                    _ => Vec::new(),
                },
            })
            .collect();
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

    /// The page as the window laid it out, `scrolling` so: where it's to
    /// be scrolled to now, as far as it goes, and the row focused brought
    /// into view if the keys moved; and whether to go there at once.
    pub(crate) fn easy_drawn(&mut self, scrolling: Scrolling) -> (f32, bool) {
        let easy = &mut self.easy;
        easy.most = scrolling.most();
        easy.scroll = if easy.reveal {
            scrolling.revealing(easy.scroll)
        } else {
            easy.scroll.clamp(0.0, easy.most)
        };
        easy.reveal = false;
        (easy.scroll, std::mem::take(&mut easy.jump))
    }

    /// Whether the wheel scrolls Easy's page, by pixels, rather than
    /// moving what's picked, as in Advanced and the font picker.
    pub(crate) fn easy_page_scrolls(&self) -> bool {
        self.mode == Mode::Easy
            && !matches!(self.editing, Some(Editing::Font(_) | Editing::Choice(_)))
    }

    /// The wheel turned over Easy's page `by` pixels' worth, down for
    /// more than none: where it's to be scrolled to now.
    pub(crate) fn easy_wheel(&mut self, by: f32) -> f32 {
        self.easy.wheel(by)
    }

    /// What Easy draws.
    #[allow(clippy::too_many_lines, reason = "the page, then each row")]
    pub(crate) fn easy_view(&mut self, facts: &Facts) -> EasyView {
        self.follow_rules(facts);
        self.follow_skills_off(facts);
        self.mcp.machine = facts.daemon.and_then(|daemon| daemon.elsewhere.clone());
        self.know_names();
        let sections = self.easy_sections();
        let searching = self.searching();
        let page = self.easy.page();
        let found = (sections.iter())
            .map(|section| section.rows.len())
            .sum::<usize>();
        self.easy.focus = self.easy.focus.min(found.saturating_sub(1));
        // Every key bound, once, if a row shows keys.
        let keys_shown = (sections.iter().flat_map(|section| &section.rows)).any(|shown| {
            matches!(shown, Shown::Entry(entry) if matches!(entry.control, Control::Keys(_)))
        });
        let all = if keys_shown {
            bindings(facts.keys)
        } else {
            Vec::new()
        };
        // Whether the page shows what's the daemon's: then its header
        // says whose, if it's another machine's.
        let lists = matches!(
            page,
            Page::Specialists | Page::Permissions | Page::Mcp | Page::Skills
        );
        let daemons = lists
            || (sections.iter().flat_map(|section| &section.rows))
                .any(|shown| matches!(shown, Shown::Entry(entry) if entry.daemons()));
        // A catalogue's rows are drawn together, once.
        let catalogued = self.catalogue().map(Catalogue::drawn).unwrap_or_default();
        let mut drawn = Vec::new();
        for section in sections {
            let mut views = Vec::new();
            for shown in section.rows {
                views.push(match shown {
                    Shown::Row(at) => match catalogued.get(at) {
                        Some(row) => row.clone(),
                        None => RowView::new("", "", ControlView::None),
                    },
                    shown => self.row_view(shown, searching, facts, &all),
                });
            }
            drawn.push(SectionView {
                heading: section.heading,
                note: section.note,
                rows: views,
            });
        }
        let fonts = match &mut self.editing {
            Some(Editing::Font(fonts)) => Some(fonts.easy_view(&facts.store.font, self.easy.focus)),
            Some(Editing::Choice(choices)) => Some(choices.view(self.easy.focus)),
            _ => None,
        };
        let form = self.form_shown();
        let words = self.page_words(facts, searching, found);
        let (footer, warning, ticked) = self.foot_said(facts, daemons);
        // What Enter does to the row the keys are on, for the foot to say.
        let on = match self.easy_focused() {
            None => On::Nothing,
            Some(Shown::Entry(_)) => On::Setting,
            Some(Shown::Row(at)) => match catalogued.get(at) {
                Some(row) if row.control != ControlView::None => On::Opens,
                _ => On::Nothing,
            },
            Some(Shown::Skill(at)) => match self.skill_switch(at, facts) {
                Some(_) => On::Switch,
                None if self.skills.get(at).is_some_and(is_skill_file) => On::Opens,
                None => On::Nothing,
            },
            Some(Shown::Advanced(_) | Shown::Found(..)) => On::Opens,
        };
        let hints = self.hints(searching, form.as_ref(), lists, on);
        let buttons = match &form {
            Some(form) => vec![
                FootButton {
                    label: if form.changed { "Cancel" } else { "Back" }.into(),
                    does: Foot::Cancel,
                    main: false,
                },
                FootButton {
                    label: form.save.clone(),
                    does: Foot::Save,
                    main: true,
                },
            ],
            None => Vec::new(),
        };
        EasyView {
            machine: super::daemon_elsewhere(facts).filter(|_| daemons),
            query: self.picker.query().to_owned(),
            typing: self.typing,
            page: (!searching).then_some(self.easy.page),
            on_side: self.easy.on_side && self.easy.keys,
            title: words.title,
            within: words.within,
            subtitle: words.subtitle,
            primary: words.primary,
            banner: words.banner,
            empty: words.empty,
            preview: (!searching && page.previews()).then(|| self.preview(facts)),
            sections: drawn,
            note: (!searching && form.is_none())
                .then(|| page.note().map(|note| (note, page == Page::Keys)))
                .flatten(),
            focus: (self.easy.keys && !self.easy.on_side && found > 0).then_some(self.easy.focus),
            scroll: self.easy.scroll,
            footer,
            warning,
            ticked,
            hints,
            buttons,
            fonts,
            sliding: self.easy.sliding.as_ref().map(|sliding| sliding.row),
        }
    }

    /// Tells the specialists' form the skills and servers there are, to
    /// say which of those it names aren't among them.
    fn know_names(&mut self) {
        let skills = self.skills.iter().map(|skill| skill.name.clone()).collect();
        let servers = (self.mcp.servers().iter())
            .map(|server| server.name.clone())
            .collect();
        self.specialists.known.insert("skills", skills);
        self.specialists.known.insert("mcp_servers", servers);
    }

    /// What the page shown is for, and for where, as its lists' pages say
    /// under their titles: `~/notes on desk`, or with nothing known of
    /// where, `this project`.
    fn whereabouts(&self, facts: &Facts) -> String {
        let dir = self.dir.as_deref().unwrap_or("this project");
        match facts.daemon.and_then(|daemon| daemon.elsewhere.as_ref()) {
            Some(machine) => format!("{dir} on {machine}"),
            None => dir.to_owned(),
        }
    }

    /// What the page says round its rows: its title and the line under
    /// it, its one button, what's gone wrong, and why it lists nothing.
    fn page_words(&self, facts: &Facts, searching: bool, found: usize) -> PageWords {
        let page = self.easy.page();
        let mut words = PageWords {
            title: page.title().to_owned(),
            subtitle: page.subtitle().to_owned(),
            ..PageWords::default()
        };
        if searching {
            let query = self.picker.query();
            words.title = "Search".into();
            words.subtitle = match found {
                0 => format!("Nothing matches “{query}”."),
                1 => format!("One match for “{query}”."),
                found => format!("{found} matches for “{query}”."),
            };
            return words;
        }
        let place = self.whereabouts(facts);
        if self.offline && page.group() == "Agents" {
            words.banner = Some(
                "The connection to the daemon is lost. What's here may be out of date, and is \
                 read again once it's back."
                    .into(),
            );
        }
        // A list says why it has nothing to show, in place of its rows.
        let listing = |listing: &Listing, reading: &str| match listing {
            Listing::Reading => Some(reading.to_owned()),
            Listing::Unavailable(why) => Some(why.clone()),
            Listing::Ready => None,
        };
        let notice = match page {
            Page::Mcp => {
                words.subtitle = format!("Servers chats can use, for {place}.");
                words.empty = listing(self.mcp.listing(), "Reading the servers…");
                if words.empty.is_none() {
                    words.primary = Some("Add a server".into());
                    words.banner = words
                        .banner
                        .or_else(|| self.mcp.problems().map(str::to_owned));
                }
                self.mcp.notice()
            }
            Page::Specialists => {
                words.subtitle = format!("Roles a chat can hand work to, for {place}.");
                words.empty = listing(self.specialists.listing(), "Reading the specialists…");
                if words.empty.is_none() {
                    words.primary = Some("New specialist".into());
                }
                self.specialists.notice()
            }
            Page::Skills => {
                words.subtitle = format!("What agents know how to do, for {place}.");
                words.empty = listing(&self.skills_listing, "Reading the skills…").or_else(|| {
                    self.skills.is_empty().then(|| {
                        "No skills yet. Add one as .agents/skills/NAME/SKILL.md in the \
                         project, or under ~/.agents for every project."
                            .to_owned()
                    })
                });
                None
            }
            Page::Permissions => {
                let machine = match facts.daemon.and_then(|daemon| daemon.elsewhere.as_ref()) {
                    Some(machine) => format!("on {machine}"),
                    None => "on this machine".to_owned(),
                };
                words.subtitle = format!(
                    "Who answers when an agent asks to do something, for every chat {machine}."
                );
                if !self.permissions.ready() {
                    words.empty = Some(if self.offline {
                        "Not connected to the daemon: its rules show once it's back.".into()
                    } else {
                        "The daemon hasn't said its settings, so its rules can't be shown. It \
                         may be older than this window."
                            .into()
                    });
                }
                if self.permissions.can_add() {
                    words.primary = Some("New rule".into());
                }
                words.banner = words.banner.or_else(|| self.permissions.unreadable());
                self.permissions.notice()
            }
            _ => None,
        };
        // What went wrong is said in full, however long it is.
        if let Some(Notice::Failed(why)) = notice {
            words.banner = Some(why.clone());
        }
        if let Some(form) = self.form_shown() {
            words.within = Some(page.title().to_owned());
            words.title = form.title;
            words.subtitle = form.subtitle;
            words.primary = None;
            words.empty = None;
        }
        words
    }

    /// What the foot says, whether it's a warning, and whether it's
    /// ticked, as something saved is: what was just said, a form's
    /// question, how the page's last action went, how to type what's
    /// being typed, what was saved last, or why the daemon's config
    /// doesn't run.
    fn foot_said(&self, facts: &Facts, daemons: bool) -> (String, bool, bool) {
        if let Some((said, warning)) = &self.said {
            return (said.clone(), *warning, false);
        }
        if self.discarding {
            let asks = "Discard the changes? Esc again discards them · Ctrl+S saves them";
            return (asks.into(), true, false);
        }
        let notice = match self.catalogue_shown() {
            Some(Catalogued::Servers) => self.mcp.notice(),
            Some(Catalogued::Specialists) => self.specialists.notice(),
            Some(Catalogued::Rules) => self.permissions.notice(),
            None => None,
        };
        match notice {
            Some(Notice::Working(said)) => return (said.clone(), false, false),
            Some(Notice::Done(said)) => return (said.clone(), false, true),
            Some(Notice::Confirm(said)) => return (said.clone(), true, false),
            // Said in full over the rows.
            Some(Notice::Failed(_)) => return ("It didn't work: see above.".into(), true, false),
            Some(Notice::Marked) => {
                let said = "It wasn't saved: see what's marked.";
                return (said.into(), true, false);
            }
            None => {}
        }
        match &self.editing {
            Some(Editing::Value { .. }) => {
                return ("Enter sets it · Esc goes back".into(), false, false);
            }
            Some(Editing::Keys { .. }) => {
                let how =
                    "Press the keys, then Enter to save · Backspace takes one back · Esc goes back";
                return (how.into(), false, false);
            }
            _ => {}
        }
        if let Some((said, warning)) = &self.said_saved {
            return (said.clone(), *warning, !*warning);
        }
        // Why the daemon's config doesn't run, while it doesn't, on a page
        // of the daemon's settings.
        let failure = (facts.daemon)
            .and_then(|daemon| daemon.failure.as_ref())
            .filter(|_| daemons);
        if let Some(failure) = failure {
            let said =
                format!("The daemon's config doesn't run, so it goes by what last did: {failure}");
            return (said, true, false);
        }
        // A page of settings saves each as it's changed; a list has
        // nothing to say until something's done.
        let lists = self.catalogue_shown().is_some() || self.easy.page() == Page::Skills;
        if lists && !self.searching() {
            (String::new(), false, false)
        } else {
            (SAVES.into(), false, true)
        }
    }

    /// The keys the foot names, as the page is now.
    fn hints(
        &self,
        searching: bool,
        form: Option<&form::FormView>,
        lists: bool,
        on: On,
    ) -> Vec<(&'static str, &'static str)> {
        let typing_in_form =
            (self.catalogue()).is_some_and(|catalogue| catalogue.typing().is_some());
        match &self.editing {
            Some(Editing::Font(_)) => {
                return vec![("Enter", "pick"), ("Tab", "every font"), ("Esc", "back")];
            }
            Some(Editing::Choice(_)) => return vec![("Enter", "pick"), ("Esc", "back")],
            Some(Editing::Value { .. } | Editing::Keys { .. }) => {
                return vec![("Enter", "save"), ("Esc", "back")];
            }
            None => {}
        }
        if self.typing {
            return vec![("Enter", "keep"), ("Esc", "clear")];
        }
        // What Enter, and the letters, do to the row the keys are on.
        let row: &[(&str, &str)] = match on {
            On::Nothing => &[],
            On::Opens => &[("Enter", "open")],
            On::Switch => &[("Enter", "on or off"), ("o", "open its file")],
            On::Setting => &[("u", "undo"), ("r", "default")],
        };
        if searching {
            return (row.iter().copied())
                .chain([("m", "Advanced"), ("Esc", "clear")])
                .collect();
        }
        // The question the foot asks says its own keys.
        if self.discarding {
            return Vec::new();
        }
        if typing_in_form {
            return vec![
                ("Enter", "next field"),
                ("Ctrl+S", "save"),
                ("Esc", "done typing"),
            ];
        }
        if let Some(form) = form {
            let esc = if form.changed { "cancel" } else { "back" };
            return vec![("Tab", "next field"), ("Ctrl+S", "save"), ("Esc", esc)];
        }
        let adds = match self.easy.page() {
            Page::Mcp if *self.mcp.listing() == Listing::Ready => Some(("a", "add a server")),
            Page::Specialists if *self.specialists.listing() == Listing::Ready => {
                Some(("a", "new specialist"))
            }
            Page::Permissions if self.permissions.can_add() => Some(("a", "new rule")),
            _ => None,
        };
        if lists {
            return (adds.into_iter())
                .chain(row.iter().copied())
                .chain([("/", "search"), ("Esc", "close")])
                .collect();
        }
        vec![
            ("u", "undo"),
            ("r", "default"),
            ("m", "Advanced"),
            ("Esc", "close"),
        ]
    }

    /// A row as it's drawn, with `all` the keys bound, if it shows keys.
    #[allow(clippy::too_many_lines, reason = "each kind of control")]
    fn row_view(&mut self, shown: Shown, searching: bool, facts: &Facts, all: &[Bound]) -> RowView {
        let entry = match shown {
            // Drawn with its catalogue's others (see `easy_view`).
            Shown::Row(_) => return RowView::new("", "", ControlView::None),
            Shown::Found(catalogue, at) => return self.found_row(catalogue, at),
            Shown::Skill(at) => return self.skill_row(at, searching, facts),
            Shown::Entry(entry) => entry,
            Shown::Advanced(at) => {
                let (label, help) = match &self.entries[at] {
                    AdvancedEntry::Option(info) => {
                        (info.name.to_owned(), info.description.to_owned())
                    }
                    AdvancedEntry::Keys(bound) => (display(&bound.keys), bound.does()),
                };
                return RowView::new(label, help, ControlView::Advanced);
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
                Some(Editing::Value { info, text }) if info.name == *name => typed_field(text),
                _ => ControlView::Field(FieldView {
                    text: typed_value(&configured(name)),
                    hint: "none",
                    ..FieldView::default()
                }),
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
            ) if info.name == *option => typed_field(text),
            _ => control,
        };
        RowView {
            label: entry.label.to_owned(),
            help,
            notes,
            lock: lock.as_ref().map(short_place),
            status: None,
            control,
        }
    }

    /// A skill's row: what it's for, where it's from, who starts it or
    /// why it can't be used, its file, and its switch.
    fn skill_row(&self, at: usize, searching: bool, facts: &Facts) -> RowView {
        let Some(skill) = self.skills.get(at) else {
            return RowView::new("", "", ControlView::None);
        };
        let name = &skill.name;
        let switch = self.skill_switch(at, facts);
        let (starts, ink) = match (switch, &skill.problem) {
            (Some(false), _) => (
                "Switched off: agents aren't offered it, and its command is gone.".to_owned(),
                Ink::Caution,
            ),
            (_, Some(problem)) => (format!("It can't be used: {problem}"), Ink::Warning),
            (_, None) => match (skill.implicit, skill.user_invocable) {
                (false, false) => (
                    "Its file says neither you nor agents start it.".to_owned(),
                    Ink::Muted,
                ),
                (false, true) => (format!("Only you start it, with /{name}."), Ink::Muted),
                (true, false) => (
                    "Only agents start it, when it fits what they're doing.".to_owned(),
                    Ink::Muted,
                ),
                (true, true) => (
                    format!("You start it with /{name}; agents use it when it fits."),
                    Ink::Muted,
                ),
            },
        };
        let help = if searching {
            format!("Skills · {}", skill.description)
        } else {
            skill.description.clone()
        };
        // What isn't a skill's file is a specialist's or a skillset's
        // folder, which there's nothing to open of.
        let file = is_skill_file(skill);
        let open = ButtonView::plain("Open its file");
        let control = match (switch, file) {
            (Some(on), true) => ControlView::Switched { on, button: open },
            (Some(on), false) => ControlView::Toggle(on),
            (None, true) => ControlView::Button(open),
            (None, false) => ControlView::None,
        };
        let label = if file {
            format!("/{name}")
        } else {
            name.clone()
        };
        RowView::new(label, help, control)
            .with_status(&skill.source, Ink::Muted)
            .noted(starts, ink)
            .noted(form::tilde(&skill.path, &self.home), Ink::Muted)
    }

    /// The row of something a search found in a catalogue's list: its
    /// name, the page it's on, and a button that opens it there.
    fn found_row(&self, catalogue: Catalogued, at: usize) -> RowView {
        let open = ControlView::Button(ButtonView::plain("Open"));
        match catalogue {
            Catalogued::Servers => match self.mcp.servers().get(at) {
                Some(server) => RowView::new(server.name.as_str(), "MCP Servers", open),
                None => RowView::new("", "", ControlView::None),
            },
            Catalogued::Specialists => match self.specialists.files().get(at) {
                Some(file) => RowView::new(file.definition.name.as_str(), "Specialists", open),
                None => RowView::new("", "", ControlView::None),
            },
            Catalogued::Rules => match self.permissions.sentences().get(at) {
                Some(sentence) => RowView::new(sentence.as_str(), "Permissions", open),
                None => RowView::new("", "", ControlView::None),
            },
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

/// The daemon's option that lists the skills switched off, by name.
const SKILLS_OFF: &str = "skills.disabled";

/// Whether what `skill` says is at its path is a skill's file, which can
/// be opened, rather than the folder of a specialist or a skillset that
/// wasn't loaded, which the daemon lists with the skills.
fn is_skill_file(skill: &clackatoo_protocol::Skill) -> bool {
    std::path::Path::new(&skill.path).extension().is_some()
}

/// What the row the keys are on is, as far as the keys the foot names go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum On {
    /// No row, or one that only says something.
    Nothing,
    /// One Enter opens, or presses the button of.
    Opens,
    /// A skill, which Enter switches off or on, and `o` opens the file of.
    Switch,
    /// A setting, which `u` undoes and `r` puts back.
    Setting,
}

/// What a page says round its rows.
#[derive(Default)]
struct PageWords {
    title: String,
    /// The page a form is of, if the title is a form's.
    within: Option<String>,
    subtitle: String,
    /// What the page's one button says.
    primary: Option<String>,
    /// What's gone wrong, said in full.
    banner: Option<String>,
    /// Why the page lists nothing.
    empty: Option<String>,
}

/// A box of text holding `text`, an option's value being typed, the caret
/// at its end.
fn typed_field(text: &str) -> ControlView {
    ControlView::Field(FieldView {
        text: text.to_owned(),
        caret: Some(text.len()),
        ..FieldView::default()
    })
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
