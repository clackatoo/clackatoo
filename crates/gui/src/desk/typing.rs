//! What's typed in the palette, and in the panes' searches: keys that edit
//! the line, as Vim's command line's do (see [`crate::cmdline`]), Tab
//! completing a command, Up and Down going back through what was typed
//! before, and Ctrl+R putting in a register.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{PaneId, Request};
use winit::keyboard::{Key, NamedKey};

use super::bindings::CHOICES;
use super::{Asked, Desk, Input, Purpose};
use crate::chrome::{PaletteChoice, Status};
use crate::cmdline::complete::{Completion, Found, Listed, Listing, Sources};
use crate::cmdline::recall::{self, Kind, Recalled, Save, Start};
use crate::cmdline::{Keyed, Line};
use crate::command;
use crate::input::KeyPress;
use crate::vim::{Stroke, prompt_strokes};

/// The lines typed before, and those this window added, for the file.
#[derive(Debug, Default)]
pub(super) struct Recalling {
    recalled: Recalled,
    /// Where they're kept, if anywhere.
    path: Option<PathBuf>,
    /// Lines run since they were last written.
    added: Vec<(Kind, String)>,
}

/// How long a folder may take to list for Tab before another may be
/// listed: one stuck on a network disk doesn't stop Tab for good.
const LISTING_PATIENCE: Duration = Duration::from_secs(5);

/// How long a folder may take to list for Tab before the palette says
/// it's being listed.
const LISTING_SAID: Duration = Duration::from_millis(100);

/// Folders listed for Tab, one at a time, off the window's thread.
#[derive(Debug, Default)]
pub(super) struct Listings {
    /// The one being listed, if any.
    pub(super) asked: Option<BeingListed>,
    /// The number the next is given.
    next: u64,
    /// Those for the window to list, oldest first.
    outbox: Vec<Listing>,
}

/// A folder being listed for Tab, and the command line as it was then,
/// and the pane it was typed from, which must be so still when the files
/// come for them to be offered: the same text typed from another pane
/// may name a file in another folder.
#[derive(Debug)]
pub(super) struct BeingListed {
    id: u64,
    pane: Option<PaneId>,
    text: String,
    caret: usize,
    /// When it was asked for.
    pub(super) at: Instant,
    /// Which way each Tab pressed since went, forward or, with Shift,
    /// back, the first first: they go through the files once they come,
    /// so Tab Tab is the second, and Shift+Tab the last.
    steps: Vec<bool>,
}

impl Desk {
    /// Starts with the lines typed before, as the window opens.
    pub(crate) fn start_recall(&mut self, start: Start) {
        self.recall.recalled = start.recalled;
        self.recall.path = start.path;
    }

    /// Takes the lines to add to the file of what was typed before, for the
    /// window to write, if there are any.
    pub(crate) fn take_recall_save(&mut self) -> Option<Save> {
        let path = self.recall.path.clone()?;
        if self.recall.added.is_empty() {
            return None;
        }
        Some(Save {
            path,
            added: std::mem::take(&mut self.recall.added),
        })
    }

    /// Takes the folders to list for Tab, for the window to list on
    /// threads, and hand back to [`listed`](Self::listed).
    pub(crate) fn take_listings(&mut self) -> Vec<Listing> {
        std::mem::take(&mut self.listings.outbox)
    }

    /// Takes in the files of a folder listed for Tab: offered if the
    /// command line is as it was when Tab asked for them. Returns whether
    /// the window must be redrawn.
    pub(crate) fn listed(&mut self, listed: Listed) -> bool {
        let Some(asked) = (self.listings.asked).take_if(|asked| asked.id == listed.id) else {
            return false;
        };
        let unchanged = self.input.as_ref().is_some_and(|input| {
            input.purpose == Purpose::Command
                && input.completing.is_none()
                && self.listing_for(input, &asked)
        });
        if !unchanged {
            self.suggest_completion();
            return false;
        }
        let completion = match listed.completion() {
            Ok(completion) => completion,
            Err(why) => {
                if !asked.steps.is_empty() {
                    self.say(why);
                }
                return true;
            }
        };
        let mut steps = asked.steps.into_iter();
        if let Some(forward) = steps.next() {
            self.completed(completion, forward);
        } else if let Some(input) = &mut self.input {
            input.suggesting = completion;
        }
        for forward in steps {
            self.step_completion(forward);
        }
        true
    }

    /// Whether `asked` was listed for the line as `input` has it now, typed
    /// from the pane focused now.
    fn listing_for(&self, input: &Input, asked: &BeingListed) -> bool {
        self.shown.map(|shown| shown.pane) == asked.pane
            && input.line.text() == asked.text
            && input.line.before_caret().len() == asked.caret
    }

    /// When the palette is to say a folder's being listed for Tab, if one
    /// is and it hasn't said so yet: the window must be drawn again then.
    pub(super) fn listing_said(&self) -> Option<Instant> {
        let asked = self.listings.asked.as_ref()?;
        Some(asked.at + LISTING_SAID).filter(|&at| at > Instant::now())
    }

    /// Asks for the folder `listing` names to be listed for Tab, which
    /// went `forward` or back; or while one is being listed for this line
    /// already, notes the way, for when it comes.
    /// One listed for another pane is let go, as it's for another folder.
    fn ask_listing(&mut self, mut listing: Listing, forward: Option<bool>) {
        let Some(input) = &self.input else { return };
        let pane = self.shown.map(|shown| shown.pane);
        let same =
            (self.listings.asked.as_ref()).is_some_and(|asked| self.listing_for(input, asked));
        // Coalesce keystrokes before the window starts the worker. Once
        // started, let it finish before asking for the latest text.
        if !same && !self.listings.outbox.is_empty() {
            self.listings.outbox.clear();
            self.listings.asked = None;
        }
        if let Some(asked) = &mut self.listings.asked
            && asked.at.elapsed() < LISTING_PATIENCE
            && asked.pane == pane
        {
            if same {
                asked.steps.extend(forward);
            }
            return;
        }
        let id = self.listings.next;
        self.listings.next += 1;
        self.listings.asked = Some(BeingListed {
            id,
            pane,
            text: input.line.text().to_owned(),
            caret: input.line.before_caret().len(),
            at: Instant::now(),
            steps: forward.into_iter().collect(),
        });
        listing.id = id;
        self.listings.outbox.push(listing);
    }

    /// Remembers `line`, run as `kind`, for Up and Down, here and in the
    /// file, unless it's one not kept (see [`recall::kept`]).
    fn remember_typed(&mut self, kind: Kind, line: &str) {
        if !recall::kept(kind, line) {
            return;
        }
        self.recall.recalled.of_mut(kind).add(line);
        if self.recall.path.is_some() {
            self.recall.added.push((kind, line.to_owned()));
        }
    }

    /// Handles a key while the palette is open.
    pub(super) fn input_key(&mut self, press: &KeyPress<'_>) {
        let Some(input) = &mut self.input else {
            return;
        };
        if input.line.wants_register() {
            let text = self.register_text(press);
            if let Some(input) = &mut self.input {
                match text {
                    Some(text) => input.line.put(&text),
                    None => input.line.forget_register(),
                }
            }
            self.suggest_completion();
            return;
        }
        let action = input.key_action(press).0;
        if let InputKey::CompleteControl(forward) = action {
            self.complete(forward);
            return;
        }
        let named = match press.key {
            Key::Named(named) => Some(*named),
            _ => None,
        };
        let completing = input.completing.is_some();
        // Any key but Tab, and Up and Down going through the choices, ends
        // a completion.
        let goes_on = named == Some(NamedKey::Tab)
            || (completing && matches!(named, Some(NamedKey::ArrowUp | NamedKey::ArrowDown)));
        if !goes_on {
            input.completing = None;
        }
        match action {
            InputKey::Cancel => self.input = None,
            InputKey::Workspace(up) => {
                let count = (self.input.as_ref())
                    .map_or(0, |input| self.workspace_choices(input.line.text()).len());
                let Some(input) = &mut self.input else { return };
                input.selected = if up {
                    input.selected.saturating_sub(1)
                } else {
                    (input.selected + 1).min(count.saturating_sub(1))
                };
            }
            InputKey::Complete(forward) | InputKey::CompleteControl(forward) => {
                self.complete(forward);
            }
            InputKey::History(older) => {
                input
                    .line
                    .recall(self.recall.recalled.of(Kind::Command), older);
            }
            InputKey::Accept => self.enter_input(),
            InputKey::Edit | InputKey::Register => match input.line.key(press) {
                // Deleting past a command's colon closes the palette, as in
                // Vim.
                Keyed::Emptied if input.purpose == Purpose::Command => self.input = None,
                Keyed::Edited => input.selected = 0,
                Keyed::Emptied | Keyed::Other => {}
            },
        }
        if !goes_on {
            self.suggest_completion();
        }
    }

    /// Enter in the palette: does what was typed.
    fn enter_input(&mut self) {
        let Some(Input {
            purpose,
            mut line,
            selected,
            ..
        }) = self.input.take()
        else {
            return;
        };
        let text = line.take();
        match purpose {
            Purpose::Command => {
                self.remember_typed(Kind::Command, &text);
                match command::parse(&text) {
                    Ok(command) => self.run(command),
                    Err(message) => self.refuse(message),
                }
            }
            Purpose::RenameWorkspace(workspace) => self.request(
                Request::RenameWorkspace {
                    workspace,
                    name: text,
                },
                Asked::Command,
            ),
            Purpose::RenameTab(tab) => {
                self.request(Request::RenameTab { tab, name: text }, Asked::Command);
            }
            Purpose::Workspace => self.pick_workspace(&text, selected),
            Purpose::Comment { chat, place } => self.send_comment(chat, place, text),
        }
    }

    /// Refresh live choices without changing the user's text. Disk lookups
    /// stay off the UI thread; host files are only read on explicit Tab.
    pub(super) fn suggest_completion(&mut self) {
        let Some(input) = &mut self.input else { return };
        input.suggesting = None;
        if input.purpose != Purpose::Command
            || input.completing.is_some()
            || input.line.wants_register()
        {
            return;
        }
        let input = self.input.as_ref().unwrap();
        match Completion::of(input.line.before_caret(), &self.completion_sources(false)) {
            Ok(Found::Done(completion)) => {
                self.input.as_mut().unwrap().suggesting = completion;
            }
            Ok(Found::List(listing)) => self.ask_listing(listing, None),
            Err(_) => {} // Automatic suggestions never interrupt typing with errors.
        }
    }

    /// Tab, or if not `forward`, Shift+Tab, on the command line: the next
    /// choice for the word before the caret, or the one before.
    fn complete(&mut self, forward: bool) {
        let Some(input) = &mut self.input else { return };
        if input.completing.is_none() {
            // Host Tab also includes SSH config and recent connections.
            let host = input
                .line
                .text()
                .split_whitespace()
                .next()
                .is_some_and(|word| command::takes(word) == command::Takes::Host);
            if !host && let Some(completion) = input.suggesting.take() {
                self.completed(Some(completion), forward);
                return;
            }
            let input = self.input.as_ref().unwrap();
            match Completion::of(input.line.before_caret(), &self.completion_sources(true)) {
                Err(why) => self.say(why),
                Ok(Found::List(listing)) => self.ask_listing(listing, Some(forward)),
                Ok(Found::Done(completion)) => self.completed(completion, forward),
            }
            return;
        }
        self.step_completion(forward);
    }

    /// The next choice Tab could complete the word with, or if not
    /// `forward`, the one before, while they're listed.
    fn step_completion(&mut self, forward: bool) {
        let Some(input) = &mut self.input else { return };
        if let Some(completion) = &mut input.completing {
            let start = completion.start;
            input.line.replace_before(start, completion.step(forward));
        }
    }

    /// Puts in what Tab found for the word before the caret, if anything:
    /// one choice at once, and that's that; or of more, the first, or the
    /// last if not `forward`, with them listed.
    fn completed(&mut self, completion: Option<Completion>, forward: bool) {
        let (Some(input), Some(mut completion)) = (&mut self.input, completion) else {
            return;
        };
        input.suggesting = None;
        let start = completion.start;
        if let [only] = &completion.choices[..] {
            input.line.replace_before(start, only);
            return;
        }
        input.line.replace_before(start, completion.step(forward));
        input.completing = Some(completion);
    }

    /// What Tab completes with: the workspaces, the focused pane's folder,
    /// which paths are from, and, after a command that takes one, the
    /// machines it may name.
    fn completion_sources(&self, explicit: bool) -> Sources {
        let first = (self.input.as_ref()).and_then(|input| {
            input
                .line
                .text()
                .split_whitespace()
                .next()
                .map(str::to_owned)
        });
        let takes_host = (first.as_deref())
            .is_some_and(|word| crate::command::takes(word) == crate::command::Takes::Host);
        // A pane on another machine than the window's has its files there,
        // as does one made on another with `@`.
        let own = self.workspaces.machine.as_ref().map(|own| own.id);
        let named = (first.as_deref())
            .and_then(|word| word.split_once('@'))
            .and_then(|(_, host)| self.machine_named(host).ok());
        let remote = self.remote
            || match named {
                Some(machine) => Some(machine) != own,
                None => self.shown_elsewhere().is_some(),
            };
        Sources {
            workspaces: (self.workspaces.list.iter())
                .map(|workspace| self.shown_name(workspace))
                .collect(),
            hosts: if takes_host && explicit {
                self.known_hosts()
            } else if takes_host {
                self.connections
                    .iter()
                    .map(|connection| connection.host.clone())
                    .collect()
            } else {
                Vec::new()
            },
            machines: self.machine_names(),
            folder: (!remote).then(|| self.focused_folder()).flatten(),
            remote,
        }
    }

    /// The folder the focused pane is in: a terminal's shell's, the
    /// folder of an editor's file, or an explorer's root, an editor's
    /// folder's too, whose listing is an editor's. None for a chat, whose
    /// folder the window has only as it's shown, nor for a buffer with no
    /// file yet.
    pub(super) fn focused_folder(&self) -> Option<PathBuf> {
        if let Some(explorer) = self.focused_explorer() {
            return explorer.root().map(PathBuf::from);
        }
        if let Some((_, copy)) = self.focused_editor() {
            return Path::new(copy.path()).parent().map(Path::to_owned);
        }
        let terminal = self.focused_terminal()?;
        self.panes.get(&terminal)?.cwd().map(PathBuf::from)
    }

    /// What Ctrl+R and then `press` put in: what the register it names
    /// holds; with Ctrl+W, the word under the focused editor's cursor; with
    /// `:`, the last command; with `/`, the last search.
    fn register_text(&self, press: &KeyPress<'_>) -> Option<String> {
        use crate::cmdline::controls::Register;
        match Register::read(press).0 {
            Register::Word => {
                let (editor, copy) = self.focused_editor()?;
                editor.word_under_cursor(copy)
            }
            Register::Command => self.recall.recalled.commands.last().map(str::to_owned),
            Register::Search => self.recall.recalled.searches.last().map(str::to_owned),
            Register::Named(name) => self
                .shared()
                .registers
                .get(Some(name))
                .map(|register| register.text.clone()),
            Register::None => None,
        }
    }

    /// A key for a search or a name being typed in the focused pane, before
    /// the pane has it: Up and Down go back through the searches typed
    /// before, Enter remembers the search, and a register's name after
    /// Ctrl+R puts in what it holds. Returns whether the window must be
    /// redrawn, or `None` if the pane is to have the key.
    pub(super) fn prompt_line_key(&mut self, press: &KeyPress<'_>) -> Option<bool> {
        let (wants_register, kind) = {
            let (line, kind) = self.focused_prompt_line()?;
            (line.wants_register(), kind)
        };
        if wants_register {
            let put = self.register_text(press);
            let (line, _) = self.focused_prompt_line()?;
            match put {
                Some(put) => line.put(&put),
                None => line.forget_register(),
            }
            return Some(true);
        }
        let kind = kind?;
        match press.key {
            key if crate::cmdline::controls::history(key).is_some() => {
                // Lent to the line, which is the pane's, for a moment.
                let recalled = std::mem::take(&mut self.recall.recalled);
                let older = crate::cmdline::controls::history(key).expect("history key");
                let moved = (self.focused_prompt_line())
                    .map(|(line, _)| line.recall(recalled.of(kind), older));
                self.recall.recalled = recalled;
                moved
            }
            Key::Named(NamedKey::Enter) => {
                let text = self.focused_prompt_line()?.0.text().to_owned();
                self.remember_typed(kind, &text);
                None
            }
            _ => None,
        }
    }

    /// The search or name being typed in the focused pane, if one is, and
    /// which lines typed before it goes back through.
    fn focused_prompt_line(&mut self) -> Option<(&mut Line, Option<Kind>)> {
        let shown = self.shown?;
        if self.focused_editor().is_some() {
            return self.editor_prompt_line(shown.pane);
        }
        if self.focused_list_mut().is_some() {
            let line = self.focused_list_mut()?.search_mut().line()?;
            return Some((line, Some(Kind::Search)));
        }
        let terminal = self.focused_terminal()?;
        let line = match self.chats.get_mut(&terminal) {
            Some(chat) => chat.search_line()?,
            None => self.panes.get_mut(&terminal)?.search_line()?,
        };
        Some((line, Some(Kind::Search)))
    }

    /// The palette, as the bar shows it: what's typed, after what it's
    /// for, and under it, the workspaces it could be, or what Tab could
    /// complete the word with.
    pub(super) fn typing_status(&self, input: &Input) -> Status {
        let (label, prefix) = input.purpose.label();
        let choices = match (
            &input.purpose,
            input.completing.as_ref().or(input.suggesting.as_ref()),
        ) {
            (Purpose::Workspace, _) => {
                let mut choices: Vec<(String, bool)> = self
                    .workspace_choices(input.line.text())
                    .into_iter()
                    .enumerate()
                    .map(|(index, (_, name))| (name, index == input.selected))
                    .collect();
                let name = input.line.text().trim();
                if choices.is_empty() && !name.is_empty() {
                    choices.push((format!("New workspace “{name}”"), true));
                }
                choices.into_iter().map(Into::into).collect()
            }
            // As many as fit, from the one picked.
            (_, Some(completion)) => {
                let top =
                    (completion.picked).map_or(0, |picked| (picked + 1).saturating_sub(CHOICES));
                (completion
                    .choices
                    .iter()
                    .enumerate()
                    .skip(top)
                    .take(CHOICES))
                .map(|(index, choice)| PaletteChoice {
                    text: choice.clone(),
                    description: crate::cmdline::complete::description(
                        input.line.before_caret(),
                        choice,
                    ),
                    picked: Some(index) == completion.picked,
                })
                .collect()
            }
            // A folder slow to list says so.
            (Purpose::Command, None)
                if (self.listings.asked.as_ref()).is_some_and(|asked| {
                    (LISTING_SAID..LISTING_PATIENCE).contains(&asked.at.elapsed())
                        && self.listing_for(input, asked)
                }) =>
            {
                vec![("listing…".to_owned(), false).into()]
            }
            _ => Vec::new(),
        };
        let selected = (input.line.selection())
            .map(|selected| selected.start + prefix.len()..selected.end + prefix.len());
        Status::Typing {
            label,
            text: format!("{prefix}{}", input.line.shown()),
            choices,
            selected,
            under: None,
        }
    }
}

#[derive(Clone, Copy)]
enum InputKey {
    Register,
    Cancel,
    Workspace(bool),
    Complete(bool),
    CompleteControl(bool),
    History(bool),
    Accept,
    Edit,
}

impl Input {
    fn key_action(&self, press: &KeyPress<'_>) -> (InputKey, crate::shortcuts::bindings::Hint) {
        use crate::shortcuts::bindings::Hint;
        if self.line.wants_register() {
            return (InputKey::Register, Hint::Text("Insert register contents"));
        }
        if self.purpose == Purpose::Command
            && let [
                Stroke::Char {
                    ch: ch @ ('n' | 'p'),
                    ctrl: true,
                },
            ] = prompt_strokes(press).as_slice()
        {
            return (
                InputKey::CompleteControl(*ch == 'n'),
                Hint::Action(if *ch == 'n' {
                    "Next suggestion"
                } else {
                    "Previous suggestion"
                }),
            );
        }
        let named = match press.key {
            Key::Named(named) => Some(*named),
            _ => None,
        };
        match named {
            Some(NamedKey::Escape) => (InputKey::Cancel, Hint::Action("Cancel prompt")),
            Some(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Tab)
                if self.purpose == Purpose::Workspace =>
            {
                let up = named == Some(NamedKey::ArrowUp)
                    || (named == Some(NamedKey::Tab) && press.modifiers.shift);
                (
                    InputKey::Workspace(up),
                    Hint::Action(if up {
                        "Previous workspace"
                    } else {
                        "Next workspace"
                    }),
                )
            }
            Some(NamedKey::Tab) if self.purpose == Purpose::Command => (
                InputKey::Complete(!press.modifiers.shift),
                Hint::Action(if press.modifiers.shift {
                    "Complete backward"
                } else {
                    "Complete forward"
                }),
            ),
            Some(NamedKey::ArrowDown | NamedKey::ArrowUp) if self.completing.is_some() => (
                InputKey::CompleteControl(named == Some(NamedKey::ArrowDown)),
                Hint::Action(if named == Some(NamedKey::ArrowUp) {
                    "Previous suggestion"
                } else {
                    "Next suggestion"
                }),
            ),
            Some(NamedKey::ArrowDown | NamedKey::ArrowUp) if self.purpose == Purpose::Command => (
                InputKey::History(named == Some(NamedKey::ArrowUp)),
                Hint::Action(if named == Some(NamedKey::ArrowUp) {
                    "Previous command in history"
                } else {
                    "Next command in history"
                }),
            ),
            Some(NamedKey::Enter) => (InputKey::Accept, Hint::Action("Accept the text")),
            _ => (
                InputKey::Edit,
                prompt_strokes(press)
                    .first()
                    .map_or(Hint::None, |stroke| self.line.describe_stroke(*stroke)),
            ),
        }
    }

    pub(super) fn shortcut_context(&self) -> crate::shortcuts::Context {
        let title = self
            .purpose
            .label()
            .0
            .unwrap_or_else(|| "Command prompt".into());
        if self.line.wants_register() {
            return crate::cmdline::controls::register_help(&title);
        }
        crate::shortcuts::bindings::from_presses(&title, |press| self.key_action(press).1)
    }
}
