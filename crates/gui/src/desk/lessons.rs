//! The tutor's lessons in the window: `:Tutor` picks one in the finder,
//! `:Tutor NAME` opens it, and `:Tutor new` lists what's new (see
//! [`crate::tutor`] for lessons themselves).
//!
//! A lesson opens in a tab of its own: the lesson, as text, in an editor
//! on the left, and the practice pane on the right, an editor on a
//! scratch file or a terminal in the scratch directory. The window writes
//! the files first, then asks the daemon for the panes, as it would for
//! any others, so a lesson needs nothing of the daemon's own.
//!
//! After every key, and everything the daemon says, the step's check is
//! looked at. Once it holds, having been seen not to since the step
//! began, the step ticks off: its heading in the lesson is lit for a
//! moment, then stays green, and the next step is lit and scrolled to. A
//! step that changes the practice file's text does so as it starts. `]s`
//! skips a step, `[s` goes back, and `:q` in either pane ends the lesson,
//! closing both, throwing away what was typed in the practice file, and
//! removing the scratch directory, as closing the window does.
//!
//! The lesson pane reads as read-only: a change to it is put back, by
//! reading the file again.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    Block, BufferId, Content, NewTerminal, OpenFile, OpenFiles, PaneId, PaneTerminal, Request,
    Response, Side, TabId, Which,
};

use super::{Asked, Desk};
use crate::editor::Lit;
use crate::finder::{self, Finder, LocalItem};
use crate::input::{KeptPress, KeyPress};
use crate::keymap::KeyMode;
use crate::pane::{Mode, Selection};
use crate::render::HighlightKind;
use crate::tutor::check::{self, Facts, Focus, Kind};
use crate::tutor::news::NEWS;
use crate::tutor::{self, Io, Lesson, Rendered, Setup, Start, State};

/// How long a step's heading is lit as it ticks off.
const TICK: Duration = Duration::from_millis(900);

/// What the notice says when there's something new.
const UPDATED: &str = "clackatoo updated · :Tutor new";

/// What the notice says the first time clackatoo runs.
const WELCOME: &str = "Welcome to clackatoo · :Tutor to start";

/// What the tutor asked the daemon for, whose answer it waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LessonAsked {
    /// The practice pane, in a new tab.
    Practice,
    /// The lesson pane, beside it.
    Lesson,
    /// Something a step needs done before its check means anything.
    Setup,
    /// The lesson's file, read again to put back a change made to it.
    Heal,
    /// The practice terminal's last block, ending at or before the line:
    /// `again` if this is the block before a prompt with nothing run in it
    /// yet.
    Block { again: bool },
}

/// The tutor: what it remembers, and the lesson open, if there is one.
#[derive(Debug)]
pub(super) struct Tutor {
    /// Where lessons keep their files, or why there's nowhere.
    scratch: Result<PathBuf, String>,
    pub(super) state: State,
    state_path: Option<PathBuf>,
    pub(super) open: Option<Open>,
    /// Files to write and remove, for the window, oldest first.
    io: Vec<Io>,
    /// The scratch directories of windows no longer running, whose panes
    /// are closed as they're found.
    pub(super) leftovers: Vec<PathBuf>,
    /// Their panes, asked to close.
    closing: Vec<PaneId>,
    /// The notice for the bar, once connected.
    notice: Option<&'static str>,
    /// How many lessons have opened, which numbers them.
    opened: Serial,
    /// The lock on the scratch directory, held while the window runs.
    lock: Option<std::fs::File>,
    /// `]` or `[`, pressed in normal mode in a lesson's tab, waiting to see
    /// whether `s` follows: `true` for `]`, and the pane it was pressed in.
    pub(super) bracket: Option<(KeptPress, bool, PaneId)>,
    /// Whether a bracket held is going on to the pane, past the tutor.
    passing: bool,
}

/// Which lesson opened, of those this window has opened, so an answer
/// for one ended since isn't taken for the one open now.
pub(super) type Serial = u64;

impl Default for Tutor {
    fn default() -> Self {
        Self {
            scratch: Err("the tutor hasn't started".into()),
            state: State::default(),
            state_path: None,
            open: None,
            io: Vec::new(),
            leftovers: Vec::new(),
            closing: Vec::new(),
            notice: None,
            opened: 0,
            lock: None,
            bracket: None,
            passing: false,
        }
    }
}

/// A lesson open.
#[derive(Debug)]
// Separate facts about the lesson, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct Open {
    /// Which it is, of the lessons this window has opened.
    serial: Serial,
    pub(super) lesson: &'static Lesson,
    rendered: Rendered,
    /// The scratch directory for its files.
    dir: PathBuf,
    /// The lesson's own file, which its pane shows.
    lesson_file: PathBuf,
    /// Its tab, and panes, once the daemon has made them.
    tab: Option<TabId>,
    practice: Option<PaneId>,
    pane: Option<PaneId>,
    /// The buffers the panes showed as they opened.
    practice_buffer: Option<BufferId>,
    lesson_buffer: Option<BufferId>,
    /// The step being done, from 0: as many as there are once all are.
    pub(super) step: usize,
    /// Whether the step's check has been seen not to hold since it began,
    /// so holding now means it was done.
    armed: bool,
    /// The requests the step waits on before its check means anything.
    waiting: usize,
    /// The practice file's text to put in, once its buffer has come.
    text_due: Option<String>,
    /// The line the practice editor's cursor goes to, once it can.
    cursor_due: Option<u64>,
    /// Commands to type into the practice terminal, once there is one.
    runs: Vec<String>,
    /// Whether the lesson pane has been shown the step.
    placed: bool,
    /// Whether the lesson's file is being read again, a change made to it
    /// being put back.
    healing: bool,
    /// The step that last ticked off, lit until then.
    ticked: Option<(usize, Instant)>,
    /// How the practice terminal's last command went, as the daemon last
    /// said since the step began.
    exit: LastCommand,
    /// Whether the terminal has changed since the daemon was last asked
    /// about its last command, and whether it's being asked.
    exit_stale: bool,
    asking_exit: bool,
}

/// How the practice terminal's last command went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastCommand {
    /// The daemon hasn't said, since the step began.
    Unheard,
    /// It's running, or none has run, or the shell didn't say how it exited.
    Unfinished,
    /// It exited with this status.
    Exited(i32),
}

impl Open {
    fn step(&self) -> Option<&'static tutor::lesson::Step> {
        self.lesson.steps.get(self.step)
    }
}

impl Desk {
    /// Starts the tutor with what it remembers, and gives the notice of
    /// what's new if there's anything it hasn't noticed: everything, when
    /// it remembers nothing.
    pub(crate) fn start_tutor(&mut self, start: Start) {
        let tutor = &mut self.tutor;
        tutor.scratch = match (start.scratch, start.unavailable) {
            (Some(dir), _) => Ok(dir),
            (None, why) => Err(why.unwrap_or_else(|| "the tutor has nowhere to work".into())),
        };
        tutor.lock = start.lock;
        tutor.state_path = start.state_path;
        tutor.leftovers = start.leftovers;
        let keys: Vec<String> = NEWS.iter().map(tutor::news::News::key).collect();
        // Nothing remembered: a window of clackatoo's first run, which
        // has nothing to be new beside.
        let first_run = start.state.is_none();
        tutor.state = start.state.unwrap_or_default();
        if keys.iter().any(|key| !tutor.state.noticed.contains(key)) {
            tutor.notice = Some(if first_run { WELCOME } else { UPDATED });
            tutor.state.noticed.extend(keys);
            self.save_tutor();
        }
    }

    /// Takes the files the tutor would have written or removed, oldest
    /// first, for the window to, before it sends what's for the daemon.
    pub(crate) fn take_tutor_io(&mut self) -> Vec<Io> {
        std::mem::take(&mut self.tutor.io)
    }

    /// Gives the notice of what's new, if there's one to give, now there's
    /// a bar to give it in.
    pub(super) fn tutor_connected(&mut self) {
        if let Some(notice) = self.tutor.notice.take() {
            self.say(notice.into());
        }
    }

    /// Queues what the tutor remembers to be written.
    fn save_tutor(&mut self) {
        if let Some(path) = &self.tutor.state_path {
            self.tutor.io.push(Io::Save {
                path: path.clone(),
                state: self.tutor.state.clone(),
            });
        }
    }

    // --- Opening ------------------------------------------------------------

    /// `:Tutor`, `:Tutor NAME` and `:Tutor new`.
    pub(super) fn tutor(&mut self, topic: Option<&str>) {
        match topic {
            None => self.pick_lesson(),
            Some("new") => self.list_news(),
            Some(id) => self.open_lesson(id),
        }
    }

    /// The finder, on the lessons.
    fn pick_lesson(&mut self) {
        let state = &self.tutor.state;
        let items = (tutor::lessons().iter())
            .map(|lesson| {
                let progress = match state.steps.get(lesson.id) {
                    _ if state.done.contains(lesson.id) => " ✓".to_owned(),
                    Some(&step) => format!(" (step {} of {})", step + 1, lesson.steps.len()),
                    None => String::new(),
                };
                let name = format!("{}{progress}", lesson.title);
                LocalItem {
                    name: lesson.id.to_owned(),
                    text: format!("{name}  {}", lesson.summary),
                    name_len: name.len(),
                    preview: preview(lesson, state.steps.get(lesson.id).copied()),
                }
            })
            .collect();
        self.show_local_finder(finder::Kind::Lessons, items);
    }

    /// The finder, on what's new, the lines not listed before first, the
    /// newest first; each goes to its lesson. They're read from now on.
    fn list_news(&mut self) {
        let state = &self.tutor.state;
        let (unread, read): (Vec<_>, Vec<_>) =
            (NEWS.iter().rev()).partition(|news| !state.read.contains(&news.key()));
        let item = |news: &tutor::news::News, new: bool| {
            let name = if new {
                format!("new  {}", news.what)
            } else {
                news.what.to_owned()
            };
            let lesson = tutor::lesson(news.lesson);
            LocalItem {
                name: news.lesson.to_owned(),
                text: format!("{name}  {}", news.date),
                name_len: name.len(),
                preview: lesson.map_or_else(Vec::new, |lesson| {
                    preview(lesson, state.steps.get(lesson.id).copied())
                }),
            }
        };
        let items = (unread.iter().map(|news| item(news, true)))
            .chain(read.iter().map(|news| item(news, false)))
            .collect();
        self.show_local_finder(finder::Kind::News, items);
        let keys = NEWS.iter().map(tutor::news::News::key);
        self.tutor.state.read.extend(keys);
        self.save_tutor();
    }

    fn show_local_finder(&mut self, kind: finder::Kind, items: Vec<LocalItem>) {
        self.close_finder();
        let case = crate::options::Case::of(&self.options);
        self.finder = Some(Finder::local(kind, items, case));
    }

    /// Opens the lesson `id` in a tab of its own, at the step it was left
    /// at, ending the one open, if any.
    pub(super) fn open_lesson(&mut self, id: &str) {
        let Some(lesson) = tutor::lesson(id) else {
            let names: Vec<&str> = tutor::lessons().iter().map(|lesson| lesson.id).collect();
            self.refuse(format!(
                "there's no lesson called {id}: the lessons are {}",
                names.join(", ")
            ));
            return;
        };
        let scratch = match &self.tutor.scratch {
            Ok(scratch) => scratch.clone(),
            Err(why) => return self.refuse(why.clone()),
        };
        let Some(shown) = self.shown else { return };
        if !self.connected {
            return self.refuse("the tutor needs the daemon, which is out of reach".into());
        }
        if self.tutor.open.is_some() {
            self.end_lesson();
        }
        let state = &self.tutor.state;
        let step = (state.steps.get(id).copied())
            .filter(|&step| step < lesson.steps.len() && !state.done.contains(id))
            .unwrap_or(0);
        let dir = scratch.join(id);
        let lesson_file = scratch.join(format!("{id}.txt"));
        let rendered = lesson.render();
        let io = &mut self.tutor.io;
        // Its folder is made afresh, whether or not the lesson has files to
        // write in it, as its practice terminal starts there.
        io.push(Io::Remove(dir.clone()));
        io.push(Io::MakeDir(dir.clone()));
        for (index, (name, text)) in lesson.files.iter().enumerate() {
            // The practice file as the step starts.
            let text = if index == 0 && !lesson.terminal() {
                lesson.practice_at(step).unwrap_or(text)
            } else {
                text
            };
            io.push(Io::Write {
                path: dir.join(name),
                text: text.to_owned(),
            });
        }
        io.push(Io::Write {
            path: lesson_file.clone(),
            text: rendered.text.clone(),
        });
        let practice = match lesson.practice_file() {
            Some((name, _)) => PaneTerminal::File(OpenFile {
                path: path_text(&dir.join(name)),
                cwd: None,
            }),
            None => PaneTerminal::New(NewTerminal {
                size: self.new_pane_size(None),
                // The daemon's own shell, which marks its commands.
                command: None,
                cwd: Some(path_text(&dir)),
                record_to: None,
            }),
        };
        self.tutor.opened += 1;
        self.tutor.open = Some(Open {
            serial: self.tutor.opened,
            lesson,
            rendered,
            dir,
            lesson_file,
            tab: None,
            practice: None,
            pane: None,
            practice_buffer: None,
            lesson_buffer: None,
            step,
            armed: false,
            waiting: 0,
            text_due: lesson.practice_at(step).map(str::to_owned),
            cursor_due: None,
            runs: Vec::new(),
            placed: false,
            healing: false,
            ticked: None,
            exit: LastCommand::Unheard,
            exit_stale: true,
            asking_exit: false,
        });
        self.request(
            Request::NewTab {
                workspace: shown.workspace,
                terminal: practice,
            },
            self.lesson_asked(LessonAsked::Practice),
        );
    }

    /// What a request the tutor makes for the lesson open is noted as.
    fn lesson_asked(&self, asked: LessonAsked) -> Asked {
        let serial = self.tutor.open.as_ref().map_or(0, |open| open.serial);
        Asked::Lesson(serial, asked)
    }

    /// Takes in the daemon's answer to what the tutor asked for the
    /// lesson numbered `serial`. A pane made for a lesson that's ended
    /// since is closed.
    pub(super) fn lesson_answered(
        &mut self,
        serial: Serial,
        asked: LessonAsked,
        response: Response,
    ) -> bool {
        let Some(open) = self
            .tutor
            .open
            .as_mut()
            .filter(|open| open.serial == serial)
        else {
            if let (LessonAsked::Practice | LessonAsked::Lesson, Response::PaneCreated(pane)) =
                (asked, response)
            {
                self.request(Request::ClosePane { pane }, Asked::Quiet);
            }
            return false;
        };
        match (asked, response) {
            (LessonAsked::Heal, _) => open.healing = false,
            (LessonAsked::Setup, response) => {
                open.waiting = open.waiting.saturating_sub(1);
                if let Response::Error(err) = response {
                    self.warn(err.to_string());
                }
            }
            (LessonAsked::Block { again }, Response::Block(block)) => {
                open.asking_exit = false;
                self.heard_block(block, again);
            }
            (LessonAsked::Block { .. }, _) => open.asking_exit = false,
            (_, Response::Error(err)) => {
                self.warn(format!("the lesson couldn't open: {err}"));
                self.end_lesson();
            }
            (LessonAsked::Practice, Response::PaneCreated(pane)) => {
                open.practice = Some(pane);
                self.practice_made(pane);
            }
            (LessonAsked::Lesson, Response::PaneCreated(pane)) => {
                open.pane = Some(pane);
                if let Some(practice) = open.practice {
                    self.show_pane(practice);
                }
                self.start_step(true);
            }
            _ => {}
        }
        true
    }

    /// The practice pane `pane` is made: the lesson goes beside it, and its
    /// tab is named for the lesson.
    fn practice_made(&mut self, pane: PaneId) {
        self.show_pane(pane);
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        let tab = (self.workspaces.find_pane(pane)).map(|(_, tab, _)| tab.id);
        open.tab = tab;
        let name = format!("Tutor: {}", open.lesson.title);
        let lesson = PaneTerminal::File(OpenFile {
            path: path_text(&open.lesson_file),
            cwd: None,
        });
        self.request(
            Request::Split {
                pane,
                side: Side::Left,
                terminal: lesson,
            },
            self.lesson_asked(LessonAsked::Lesson),
        );
        if let Some(tab) = tab {
            self.request(Request::RenameTab { tab, name }, Asked::Quiet);
        }
    }

    // --- Steps --------------------------------------------------------------

    /// Starts the step the lesson's at: its setup is done, and its check
    /// must be seen not to hold before it can. `opening` if the lesson is
    /// just opening, so the lesson's own setup is done first.
    fn start_step(&mut self, opening: bool) {
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        open.armed = false;
        open.placed = false;
        open.exit = LastCommand::Unheard;
        open.exit_stale = true;
        let lesson = open.lesson;
        let mut setup: Vec<Setup> = Vec::new();
        if opening {
            setup.extend(lesson.setup.iter().cloned());
        }
        if let Some(step) = open.step() {
            if !opening && let Some(text) = &step.practice {
                open.text_due = Some(text.clone());
            }
            setup.extend(step.setup.iter().cloned());
        }
        for setup in setup {
            self.set_up(setup);
        }
        // Where it's left, for the next window.
        let Some(open) = &self.tutor.open else { return };
        let (id, step) = (open.lesson.id, open.step);
        let state = &mut self.tutor.state;
        if step < lesson.steps.len() {
            state.steps.insert(id.to_owned(), step);
        } else {
            state.steps.remove(id);
            state.done.insert(id.to_owned());
            self.say(format!(
                "that's the end of {}: :q closes it, and :Tutor picks another",
                lesson.title
            ));
        }
        self.save_tutor();
    }

    fn set_up(&mut self, setup: Setup) {
        let has_explorer =
            (self.lesson_tab_panes().iter()).any(|pane| matches!(pane.content, Content::Files(_)));
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        match setup {
            Setup::Terminal => {}
            Setup::Run(command) => open.runs.push(command),
            Setup::Cursor(line) => open.cursor_due = Some(line - 1),
            Setup::Explorer => {
                let tab = open.tab;
                let dir = path_text(&open.dir);
                if let Some(tab) = tab.filter(|_| !has_explorer) {
                    open.waiting += 1;
                    let request = Request::SplitTab {
                        tab,
                        side: Side::Left,
                        terminal: PaneTerminal::Files(OpenFiles { dir: Some(dir) }),
                        share: Some(self.explorer_share()),
                    };
                    self.request(request, self.lesson_asked(LessonAsked::Setup));
                }
            }
        }
    }

    /// Whether the lesson's tab is shown.
    fn lesson_tab_shown(&self) -> bool {
        let tab = self.shown.map(|shown| shown.tab);
        (self.tutor.open.as_ref()).is_some_and(|open| open.tab.is_some() && open.tab == tab)
    }

    /// `]s` and `[s`, in normal mode in a lesson's tab: the lesson's next
    /// step, or the one before. `]` or `[` waits for the key after, and if
    /// that isn't `s`, both go on as if the tutor weren't there, so they
    /// do as they always do; anywhere else, they're never held. Returns
    /// `Some` if `press` was had, with whether the window must be redrawn.
    pub(super) fn lesson_bracket(&mut self, press: &KeyPress<'_>) -> Option<bool> {
        let plain = (press.text)
            .filter(|_| !press.modifiers.ctrl && !press.modifiers.alt)
            .and_then(|text| {
                let mut chars = text.chars();
                chars.next().filter(|_| chars.next().is_none())
            });
        let pane = self.shown.map(|shown| shown.pane);
        if let Some((held, forward, at)) = self.tutor.bracket.take() {
            if Some(at) != pane {
                // The focus moved meanwhile, as with a click: it's dropped.
                return None;
            }
            if plain == Some('s') {
                self.step_lesson(forward);
                return Some(true);
            }
            // Not `]s` after all: the bracket goes on, then this key.
            self.tutor.passing = true;
            self.key_with(&held.press(), false);
            self.tutor.passing = false;
            return None;
        }
        let forward = match plain {
            Some(']') => true,
            Some('[') => false,
            _ => return None,
        };
        let waits = !self.tutor.passing
            && self.lesson_tab_shown()
            && self.key_mode() == KeyMode::Normal
            && self.keymap.waiting().is_empty()
            && !self.pane_pending();
        let pane = pane.filter(|_| waits)?;
        self.tutor.bracket = Some((KeptPress::of(press), forward, pane));
        Some(false)
    }

    /// `]s`, or `[s` if not `forward`: the next step, or the one before,
    /// done or not.
    fn step_lesson(&mut self, forward: bool) {
        let Some(open) = self.tutor.open.as_mut() else {
            return;
        };
        let last = open.lesson.steps.len();
        let step = if forward {
            (open.step + 1).min(last)
        } else {
            open.step.saturating_sub(1)
        };
        if step == open.step {
            let why = if forward {
                "that was the last step"
            } else {
                "that's the first step"
            };
            self.refuse(why.into());
            return;
        }
        open.step = step;
        self.start_step(false);
    }

    /// The step's check holds: it ticks off, and the next starts.
    fn tick(&mut self) {
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        open.ticked = Some((open.step, Instant::now() + TICK));
        open.step += 1;
        self.start_step(false);
    }

    // --- As things happen ---------------------------------------------------

    /// The terminal `terminal` has changed: if it's the practice
    /// terminal, its last command may have finished.
    pub(super) fn lesson_view_changed(&mut self, terminal: clackatoo_protocol::TerminalId) {
        let practice = self.practice_terminal();
        if let Some(open) = &mut self.tutor.open
            && practice == Some(terminal)
        {
            open.exit_stale = true;
        }
    }

    /// Looks at the lesson open after anything that may change what its
    /// step checks: sees to its panes and setup, and ticks off the step if
    /// it's done. Does nothing when no lesson's open.
    pub(super) fn settle_lesson(&mut self) {
        let Some(open) = &self.tutor.open else {
            return;
        };
        let gone = |pane: Option<PaneId>| {
            pane.is_some_and(|pane| self.workspaces.find_pane(pane).is_none())
        };
        if gone(open.practice) || gone(open.pane) {
            return self.end_lesson();
        }
        let (Some(practice), Some(pane)) = (open.practice, open.pane) else {
            return;
        };
        self.note_buffers(practice, pane);
        self.fence_explorers();
        self.show_step_in_lesson(pane);
        self.put_practice_text();
        self.run_commands();
        let Some(open) = &self.tutor.open else { return };
        let Some(step) = open.step() else { return };
        if open.waiting > 0 || open.text_due.is_some() || open.healing {
            return;
        }
        if step.checks.iter().any(check::Check::wants_exit) {
            self.ask_exit();
            // Until the daemon says how the last command went, the check
            // means nothing: a command that ended before the step began
            // mustn't look like one run for it.
            if self
                .tutor
                .open
                .as_ref()
                .is_some_and(|open| open.exit == LastCommand::Unheard)
            {
                return;
            }
        }
        if step.checks.is_empty() {
            return;
        }
        let holds = {
            let facts = DeskFacts { desk: self };
            step.checks.iter().all(|check| check.holds(&facts))
        };
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        if !holds {
            open.armed = true;
        } else if open.armed {
            self.tick();
            // The next step's check, as it starts: if it holds already, it
            // must be seen not to before it can tick off.
            self.settle_lesson();
        }
    }

    /// Keeps the explorers of the lesson's folder in it, and the editors
    /// showing it, so `-` stops there, and nothing outside it is touched
    /// from them.
    fn fence_explorers(&mut self) {
        let Some(open) = &self.tutor.open else { return };
        let dir = path_text(&open.dir);
        for pane in self.lesson_tab_panes() {
            if matches!(pane.content, Content::Files(_) | Content::Folder(_)) {
                self.fence_explorer(pane.id, &dir);
            }
        }
    }

    /// Notes the buffers the lesson's panes show, once they do.
    fn note_buffers(&mut self, practice: PaneId, pane: PaneId) {
        let buffer = |pane: PaneId| {
            let (_, _, pane) = self.workspaces.find_pane(pane)?;
            pane.content.buffer()
        };
        let (practice, pane) = (buffer(practice), buffer(pane));
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        open.practice_buffer = open.practice_buffer.or(practice);
        open.lesson_buffer = open.lesson_buffer.or(pane);
    }

    /// Draws the lesson pane as a lesson, lit for the steps done and the
    /// step being done, scrolled to it once its text has come; and puts
    /// back a change made to its text.
    fn show_step_in_lesson(&mut self, pane: PaneId) {
        let Some(open) = &self.tutor.open else { return };
        let Some(buffer) = open.lesson_buffer else {
            return;
        };
        let Some(copy) = self.buffer_copy(buffer).filter(|copy| copy.loaded()) else {
            return;
        };
        let changed = copy.text() != open.rendered.text.as_str();
        if changed && !open.healing {
            // Written again first, in case it was written over with `:w`.
            let (path, text) = (open.lesson_file.clone(), open.rendered.text.clone());
            self.tutor.io.push(Io::Write { path, text });
            if let Some(open) = &mut self.tutor.open {
                open.healing = true;
            }
            self.say("the lesson can't be changed: practise in the pane beside it".into());
            self.request(
                Request::Discard { buffer },
                self.lesson_asked(LessonAsked::Heal),
            );
            return;
        }
        if changed {
            return;
        }
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        let rendered = &open.rendered;
        let line = |line: usize| u64::try_from(line).unwrap_or(u64::MAX);
        let heading = |range: &std::ops::Range<usize>| line(range.start)..line(range.start) + 1;
        let mut lit: Vec<Lit> = (rendered.steps.iter().take(open.step))
            .map(|range| Lit {
                lines: heading(range),
                kind: HighlightKind::LessonDone,
                until: None,
            })
            .collect();
        let current = rendered.steps.get(open.step).unwrap_or(&rendered.end);
        let top = line(current.start);
        lit.push(Lit {
            lines: top..line(current.end),
            kind: HighlightKind::LessonStep,
            until: None,
        });
        if let Some((step, until)) = open.ticked
            && let Some(range) = rendered.steps.get(step)
        {
            lit.push(Lit {
                lines: heading(range),
                kind: HighlightKind::Copied,
                until: Some(until),
            });
        }
        let place = !std::mem::replace(&mut open.placed, true);
        // Shown as prose from the first, once.
        let options = (self.editor(pane))
            .is_some_and(|editor| !editor.shows_lesson())
            .then(|| self.options.clone());
        self.with_editor(pane, |editor, copy| {
            if let Some(options) = options {
                editor.show_lesson(&options, copy);
            }
            editor.light(lit);
            if place {
                editor.go_to_top(top, copy);
            }
        });
    }

    /// Puts the practice file's text due in, once its buffer has come, and
    /// its cursor where the step says.
    fn put_practice_text(&mut self) {
        let Some(open) = &self.tutor.open else { return };
        let Some(buffer) = open.practice_buffer else {
            return;
        };
        if self.buffer_copy(buffer).is_none_or(|copy| !copy.loaded()) {
            return;
        }
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        let (text, line, practice) = (open.text_due.take(), open.cursor_due.take(), open.practice);
        if let Some(text) = text {
            self.replace_text(buffer, &text);
        }
        if let (Some(line), Some(practice)) = (line, practice) {
            self.with_editor(practice, |editor, copy| editor.go_to(line, 0, copy));
        }
    }

    /// Types the commands due into the practice terminal, once it's there.
    fn run_commands(&mut self) {
        let Some(terminal) = self.practice_terminal() else {
            return;
        };
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        if open.runs.is_empty() || !self.panes.contains_key(&terminal) {
            return;
        }
        let runs = std::mem::take(&mut open.runs);
        if let Some(pane) = self.panes.get_mut(&terminal) {
            for command in runs {
                pane.send_bytes(format!("{command}\r").into_bytes());
            }
        }
    }

    /// The terminal the practice pane shows, if it's a terminal.
    fn practice_terminal(&self) -> Option<clackatoo_protocol::TerminalId> {
        let practice = self.tutor.open.as_ref()?.practice?;
        let (_, _, pane) = self.workspaces.find_pane(practice)?;
        pane.content.terminal()
    }

    /// Asks the daemon about the practice terminal's last command, if it's
    /// changed since last asked.
    fn ask_exit(&mut self) {
        let Some(terminal) = self.practice_terminal() else {
            return;
        };
        let end = self
            .panes
            .get(&terminal)
            .and_then(crate::pane::Pane::document_end);
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        let Some(end) = end.filter(|_| open.exit_stale && !open.asking_exit) else {
            return;
        };
        open.exit_stale = false;
        open.asking_exit = true;
        self.request(
            Request::Block {
                terminal,
                line: end,
                which: Which::Before,
            },
            self.lesson_asked(LessonAsked::Block { again: false }),
        );
    }

    /// Takes in the practice terminal's last block, as asked: the block
    /// before it, if it's a prompt with nothing run in it yet.
    fn heard_block(&mut self, block: Option<Block>, again: bool) {
        let Some(terminal) = self.practice_terminal() else {
            return;
        };
        let Some(open) = &mut self.tutor.open else {
            return;
        };
        let Some(block) = block else {
            open.exit = LastCommand::Unfinished;
            return;
        };
        let nothing_run = block.command.is_none() && block.output.is_none() && block.exit.is_none();
        if nothing_run && !again && block.first > 0 {
            open.asking_exit = true;
            self.request(
                Request::Block {
                    terminal,
                    line: block.first,
                    which: Which::Before,
                },
                self.lesson_asked(LessonAsked::Block { again: true }),
            );
            return;
        }
        open.exit = match block.exit {
            Some(status) if !block.running => LastCommand::Exited(status),
            _ => LastCommand::Unfinished,
        };
    }

    // --- Ending -------------------------------------------------------------

    /// Whether `pane` is the lesson pane or the practice pane, which `:q`
    /// ends the lesson from.
    pub(super) fn in_lesson_pane(&self, pane: PaneId) -> bool {
        (self.tutor.open.as_ref())
            .is_some_and(|open| open.practice == Some(pane) || open.pane == Some(pane))
    }

    /// Whether `pane` shows a lesson, and so takes no file opened from a
    /// link or an explorer.
    pub(super) fn is_lesson_pane(&self, pane: PaneId) -> bool {
        (self.tutor.open.as_ref()).is_some_and(|open| open.pane == Some(pane))
    }

    /// Ends the lesson open: closes its panes, throwing away what was typed
    /// in the practice file, and removes its scratch files.
    pub(super) fn end_lesson(&mut self) {
        let tab_panes = self.lesson_tab_panes();
        let Some(open) = self.tutor.open.take() else {
            return;
        };
        // Only what shows its scratch files goes: its own panes, a file a
        // link opened there, an explorer of its directory, the practice
        // terminal. A pane gone on to a file of yours, as after `:e`,
        // stays as it is, unsaved changes and all.
        let in_scratch = |path: &str| {
            let path = Path::new(path);
            path.starts_with(&open.dir) || path == open.lesson_file
        };
        for pane in &tab_panes {
            let (close, discard) = match pane.content {
                Content::Buffer(buffer) => {
                    let scratch = match self.buffer_copy(buffer).filter(|copy| copy.loaded()) {
                        Some(copy) => in_scratch(copy.path()),
                        // Not come yet: the lesson's own, as they opened.
                        None => [open.practice_buffer, open.lesson_buffer].contains(&Some(buffer)),
                    };
                    // What was typed in a scratch file is thrown away, as
                    // `:q!` throws it away, so no buffer is kept, unseen, on
                    // a file that's gone.
                    (scratch, scratch && Some(pane.id) != open.pane)
                }
                Content::Files(_) | Content::Folder(_) => {
                    let root = self.explorer(pane.id).and_then(|explorer| explorer.root());
                    (root.is_some_and(in_scratch), false)
                }
                Content::Terminal(_) => (Some(pane.id) == open.practice, false),
                Content::Board(_)
                | Content::Review(_)
                | Content::Browser(_)
                | Content::Other { .. } => (false, false),
            };
            if discard && let Content::Buffer(buffer) = pane.content {
                self.request(Request::Discard { buffer }, Asked::Quiet);
            }
            if close {
                self.request(Request::ClosePane { pane: pane.id }, Asked::Quiet);
            }
        }
        self.tutor.io.push(Io::Remove(open.dir));
        self.tutor.io.push(Io::Remove(open.lesson_file));
    }

    /// The window is closing: the lesson open ends, and the scratch
    /// directory goes.
    pub(crate) fn tutor_exiting(&mut self) {
        self.end_lesson();
        if let Ok(scratch) = &self.tutor.scratch {
            self.tutor.io.push(Io::Remove(scratch.clone()));
        }
    }

    /// Closes the panes, in the workspace shown, of lessons of windows no
    /// longer running: their files are gone. Looked at as the workspaces
    /// change and as files' text comes, which is when a buffer's path is
    /// first known, and forgotten once nothing shown is theirs.
    pub(super) fn close_leftovers(&mut self) {
        if self.tutor.leftovers.is_empty() {
            return;
        }
        let leftovers = &self.tutor.leftovers;
        let closing = &self.tutor.closing;
        let candidates = self.lesson_candidates();
        let mut unknown = false;
        let mut left: Vec<(PaneId, BufferId)> = Vec::new();
        for (pane, buffer) in candidates {
            match self.buffer_copy(buffer).filter(|copy| copy.loaded()) {
                None => unknown = true,
                Some(copy) => {
                    let theirs =
                        (leftovers.iter()).any(|dir| Path::new(copy.path()).starts_with(dir));
                    if theirs && !closing.contains(&pane) {
                        left.push((pane, buffer));
                    }
                }
            }
        }
        if left.is_empty() && !unknown {
            self.tutor.leftovers.clear();
            self.tutor.closing.clear();
            return;
        }
        for (pane, buffer) in left {
            if self
                .buffer_copy(buffer)
                .is_some_and(crate::editor::BufferCopy::modified)
            {
                self.request(Request::Discard { buffer }, Asked::Quiet);
            }
            self.request(Request::ClosePane { pane }, Asked::Quiet);
            self.tutor.closing.push(pane);
        }
    }

    /// The panes showing buffers in the workspace shown.
    fn lesson_candidates(&self) -> Vec<(PaneId, BufferId)> {
        let Some(workspace) = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
        else {
            return Vec::new();
        };
        (workspace.tabs.iter())
            .flat_map(|tab| tab.layout.panes())
            .filter_map(|pane| Some((pane.id, pane.content.buffer()?)))
            .collect()
    }

    /// The panes of the lesson's tab.
    fn lesson_tab_panes(&self) -> Vec<clackatoo_protocol::Pane> {
        let Some(tab) = self.tutor.open.as_ref().and_then(|open| open.tab) else {
            return Vec::new();
        };
        (self.workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .find(|candidate| candidate.id == tab)
            .map(|tab| tab.layout.panes())
            .unwrap_or_default()
    }
}

/// The path as text, as the daemon takes it.
fn path_text(path: &Path) -> String {
    path.display().to_string()
}

/// What the finder shows beside a lesson: its title, what it teaches, and
/// its steps, those done before `at` ticked.
fn preview(lesson: &Lesson, at: Option<usize>) -> Vec<String> {
    let mut lines = vec![
        lesson.title.clone(),
        String::new(),
        lesson.summary.clone(),
        String::new(),
    ];
    for (index, step) in lesson.steps.iter().enumerate() {
        let tick = if at.is_some_and(|at| index < at) {
            "✓"
        } else {
            " "
        };
        lines.push(format!("{tick} {}. {}", index + 1, step.title));
    }
    lines
}

/// The window's state, as a lesson's checks see it.
struct DeskFacts<'d> {
    desk: &'d Desk,
}

impl DeskFacts<'_> {
    fn open(&self) -> Option<&Open> {
        self.desk.tutor.open.as_ref()
    }

    fn kind(&self, pane: &clackatoo_protocol::Pane) -> Kind {
        match pane.content {
            Content::Terminal(terminal)
                if self
                    .desk
                    .panes
                    .get(&terminal)
                    .is_some_and(crate::pane::Pane::is_chat) =>
            {
                Kind::Chat
            }
            Content::Terminal(_) => Kind::Terminal,
            Content::Buffer(_) => Kind::Editor,
            Content::Files(_) => Kind::Explorer,
            Content::Folder(_) => Kind::Folder,
            Content::Board(_) => Kind::Board,
            Content::Review(_) => Kind::Review,
            Content::Browser(_) => Kind::Browser,
            Content::Other { .. } => Kind::Other,
        }
    }
}

impl Facts for DeskFacts<'_> {
    fn practice_text(&self) -> Option<String> {
        let buffer = self.open()?.practice_buffer?;
        let copy = self.desk.buffer_copy(buffer).filter(|copy| copy.loaded())?;
        Some(copy.text().to_string())
    }

    fn practice_saved(&self) -> Option<bool> {
        let buffer = self.open()?.practice_buffer?;
        let copy = self.desk.buffer_copy(buffer).filter(|copy| copy.loaded())?;
        Some(!copy.modified())
    }

    fn cursor(&self) -> Option<(usize, String, usize)> {
        let (editor, copy) = self.desk.focused_editor()?;
        let text = copy.text();
        let byte = editor.cursor_byte(copy);
        let line = text.byte_to_line(byte);
        let start = text.line_to_byte(line);
        let line_text = text.line(line).to_string();
        Some((
            line,
            line_text.trim_end_matches('\n').to_owned(),
            byte - start,
        ))
    }

    fn register(&self, name: char) -> Option<String> {
        let register = self.desk.registers().get(Some(name))?;
        Some(register.text.clone())
    }

    fn mode(&self) -> Option<check::Mode> {
        let mode = if let Some((editor, _)) = self.desk.focused_editor() {
            editor.mode
        } else if self.desk.focused_explorer().is_some() {
            Mode::Normal
        } else {
            self.desk.panes.get(&self.desk.focused_terminal()?)?.mode
        };
        Some(match mode {
            Mode::Normal => check::Mode::Normal,
            Mode::Insert => check::Mode::Insert,
            Mode::Visual(Selection::Characters) => check::Mode::Visual,
            Mode::Visual(Selection::Lines) => check::Mode::VisualLine,
        })
    }

    fn panes(&self) -> Vec<Kind> {
        let panes = self.desk.lesson_tab_panes();
        panes.iter().map(|pane| self.kind(pane)).collect()
    }

    fn focused(&self) -> Option<(Kind, Option<Focus>)> {
        let shown = self.desk.shown?;
        let (_, _, pane) = self.desk.workspaces.find_pane(shown.pane)?;
        let open = self.open()?;
        let role = if open.practice == Some(pane.id) {
            Some(Focus::Practice)
        } else if open.pane == Some(pane.id) {
            Some(Focus::Lesson)
        } else {
            None
        };
        Some((self.kind(&pane), role))
    }

    fn lesson_shown(&self) -> bool {
        let tab = self.desk.shown.map(|shown| shown.tab);
        self.open()
            .is_some_and(|open| open.tab.is_some() && open.tab == tab)
    }

    fn workspace(&self) -> Option<String> {
        let shown = self.desk.shown?;
        Some(self.desk.workspaces.get(shown.workspace)?.name.clone())
    }

    fn last_exit(&self) -> Option<i32> {
        match self.open()?.exit {
            LastCommand::Exited(status) => Some(status),
            LastCommand::Unheard | LastCommand::Unfinished => None,
        }
    }

    fn option(&self, name: &str) -> String {
        crate::options::find(name)
            .map_or_else(|_| String::new(), |info| self.desk.options.show(info))
    }

    fn finder(&self) -> Option<&'static str> {
        if self.desk.start.is_some() {
            return Some("agent");
        }
        Some(match self.desk.finder.as_ref()?.kind() {
            finder::Kind::Files => "files",
            finder::Kind::Grep => "grep",
            finder::Kind::Buffers => "buffers",
            finder::Kind::Diagnostics => "diagnostics",
            finder::Kind::Marks => "marks",
            finder::Kind::Places => "places",
            finder::Kind::Boards => "boards",
            finder::Kind::Lessons => "lessons",
            finder::Kind::News => "news",
            finder::Kind::Symbols => "symbols",
            finder::Kind::ProjectSymbols => "projectsymbols",
        })
    }

    fn listed(&self, path: &str) -> bool {
        (self.desk.lesson_tab_panes().iter())
            .filter_map(|pane| self.desk.explorer(pane.id))
            .any(|explorer| explorer.lists(path))
    }

    fn rooted(&self, path: &str) -> bool {
        let Some(open) = self.open() else {
            return false;
        };
        let wanted = match path {
            "." => open.dir.clone(),
            path => open.dir.join(path),
        };
        (self.desk.lesson_tab_panes().iter())
            .filter_map(|pane| self.desk.explorer(pane.id))
            .filter_map(|explorer| explorer.root())
            .any(|root| Path::new(root) == wanted)
    }

    fn file(&self) -> Option<String> {
        let (_, copy) = self.desk.focused_editor()?;
        // A folder's listing is no file.
        (copy.loaded() && !copy.is_scratch()).then(|| copy.name().to_owned())
    }

    fn recording(&self) -> Option<char> {
        self.desk.registers().recording()
    }

    fn messages_shown(&self) -> bool {
        self.desk.messages_listed().is_some()
    }
}
