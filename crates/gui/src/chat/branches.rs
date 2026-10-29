//! Rewinding a chat's conversation, editing an earlier message and
//! switching between the conversation's branches: the window's side
//! (CLACKA-198). The daemon keeps the branches, inside the one chat (see
//! [`Divergence`]); the window asks for a rewind or a switch, asks the
//! user first what's to become of the files the turns taken out changed
//! and of the agent's background tasks, and puts an edited message in
//! the box.
//!
//! | Keys / commands | Does |
//! |---|---|
//! | `dG` on a turn | Rewinds to before it |
//! | `cc`, `S` on a message of yours | Rewinds to before it, its message in the box to send again |
//! | `[b` `]b` | The branch before or after, where the branches part at the turn under the cursor, or else the nearest above it |
//! | `:rewind[!] [N]`, `/rewind` | Rewinds to before turn N, or the last; `!` undoes the files without asking |
//! | `:branch N`, `:branches` | Branch N where they part, or the finder over every branch |
//!
//! A cut or a switch restarts the chat's journal, so the chat comes whole
//! again: what the window keeps by turn index, from folds to the cursor,
//! is kept for the turns before the first that changed and let go of
//! from there (see [`ChatView::cut_from`]).

use std::fmt::Write as _;

use clackatoo_protocol::{ChatAction, Divergence, ForkFiles, Response, TurnFilesState};

use super::{ChatView, Input};
use crate::options::RewindFiles;
use crate::vim::Stroke;
use crate::vim::confirm::Confirm;

/// A rewind or a switch to another branch, as it's to be asked of the
/// daemon once the user has said what's to become of the files and
/// tasks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Move {
    what: What,
    files: ForkFiles,
    stop_tasks: bool,
}

/// Which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum What {
    /// To before turn `turn`, by its index among those held, with its
    /// message in the box if `edit`.
    Rewind { turn: usize, edit: bool },
    /// To branch `to`, from 1, of those parting at turn `turn`, by its
    /// index among those held (one past the last for a new branch not
    /// written to yet).
    Switch { turn: usize, to: u16 },
}

/// The question a rewind or a switch asks first, while it's asked.
pub(super) type Asked = Confirm<Move>;

/// How many of `count` there are, as "1 turn" or "3 turns".
fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// The first line of `words`, a message, cut short for the bar.
fn quoted(words: &str) -> String {
    const LONGEST: usize = 24;
    let line = words.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= LONGEST {
        line.to_owned()
    } else {
        let cut: String = line.chars().take(LONGEST - 1).collect();
        format!("{}…", cut.trim_end())
    }
}

/// `ms` milliseconds, roughly, as how long ago a branch was shown:
/// "just now", "5 minutes", "3 hours", "2 days".
fn ago(ms: u64) -> String {
    let minutes = ms / 60_000;
    match minutes {
        0 => "moments".to_owned(),
        1..60 => counted(usize::try_from(minutes).unwrap_or(0), "minute", "minutes"),
        60..1440 => counted(usize::try_from(minutes / 60).unwrap_or(0), "hour", "hours"),
        _ => counted(usize::try_from(minutes / 1440).unwrap_or(0), "day", "days"),
    }
}

impl ChatView {
    /// Takes what a rewind does to files, as the `rewindfiles` option
    /// says.
    pub(crate) fn set_rewind_files(&mut self, files: RewindFiles) {
        self.rewind_files = files;
    }

    /// The divergence at turn `turn`, by its index among those held, or
    /// at one past the last for a new branch not written to yet.
    pub(super) fn divergence_at(&self, turn: usize) -> Option<&Divergence> {
        let at = self.absolute(turn)?;
        self.node.branches.iter().find(|place| place.at == at)
    }

    /// Turn `turn`, by its index among those held, as the daemon numbers
    /// turns: from the conversation's start.
    fn absolute(&self, turn: usize) -> Option<u32> {
        u32::try_from(turn).ok()?.checked_add(self.node.first_turn)
    }

    /// What the bar calls turn `turn`, by its index among those held:
    /// "turn 4", as people count them, or "the end" for one past the last.
    fn turn_named(&self, turn: usize) -> String {
        if turn >= self.node.turns.len() {
            return "the end".to_owned();
        }
        let number = clackatoo_protocol::turn_number(&self.node.turns, turn)
            .map_or(turn + 1, |number| number + self.node.first_message as usize);
        format!("turn {number}")
    }

    /// Why a rewind or a switch can't be asked for now, if it can't: the
    /// daemon can't, or the agent is at work or asking something.
    fn branching_refused(&self) -> Option<&'static str> {
        if !self.branches_supported {
            Some("rewinding a conversation needs a newer clackatoo daemon: update it")
        } else if self.offline {
            Some("the daemon is out of reach")
        } else if self.working() || self.node.asking() {
            Some("wait for the agent to finish, or stop it, first")
        } else {
            None
        }
    }

    /// The turn the keys rewind from or edit: the cursor's, if it's in
    /// view, as `u` picks its turn; or else the last.
    pub(super) fn turn_under_cursor(&self) -> Option<usize> {
        let here = (self.cursor.map(|cursor| cursor.text.turn))
            .filter(|&turn| (self.shown.iter()).any(|shown| shown.key.turn == turn));
        here.or_else(|| self.node.turns.len().checked_sub(1))
    }

    /// `dG`, `cc` or `S`: rewinds to before the turn under the cursor,
    /// with its message in the box if `edit`.
    pub(super) fn rewind_key(&mut self, edit: bool) {
        if let Some(turn) = self.turn_under_cursor() {
            self.rewind(turn, edit, false);
        } else {
            self.say("there's no turn to rewind to before".to_owned());
            self.failed = true;
        }
    }

    /// A cut the chat doesn't take was typed, as `dd` or `cw`: the bar
    /// says what it takes.
    pub(super) fn cut_refused(&mut self) {
        self.say(
            "a conversation can only be cut from a turn to its end: dG, or cc to edit your message"
                .to_owned(),
        );
    }

    /// `:rewind N` or `/rewind`: rewinds to before turn `number`, as
    /// people count them, from 1, or the last; with `force`, undoing the
    /// files the turns cut changed without asking.
    pub(crate) fn rewind_numbered(
        &mut self,
        number: Option<usize>,
        force: bool,
    ) -> Result<(), String> {
        let turn = match number {
            Some(number) => {
                let local = number
                    .checked_sub(self.node.first_message as usize)
                    .filter(|&local| local > 0)
                    .ok_or("Scroll up to load that turn before rewinding to it")?;
                clackatoo_protocol::turn_numbered(&self.node.turns, local)
                    .ok_or(format!("there's no turn {number}"))?
            }
            None => (self.node.turns.len().checked_sub(1))
                .ok_or("there's no turn to rewind to before")?,
        };
        self.rewind(turn, false, force);
        Ok(())
    }

    /// Rewinds to before turn `turn`, by its index among those held, as
    /// [`ChatAction::Rewind`] does, with its message in the box if
    /// `edit`, asking first what's to become of the files and tasks,
    /// unless `force`, which undoes the files without asking.
    pub(super) fn rewind(&mut self, turn: usize, edit: bool, force: bool) {
        if let Some(why) = self.branching_refused() {
            self.say(why.to_owned());
            self.failed = true;
            return;
        }
        let Some(shown) = self.node.turns.get(turn) else {
            self.say(format!("there's no {}", self.turn_named(turn)));
            self.failed = true;
            return;
        };
        if edit && shown.user_prompt().is_none() {
            self.say("only a message of yours can be edited: dG rewinds to before any turn".into());
            self.failed = true;
            return;
        }
        // The turn's named, and quoted: one key does this, and may fall
        // back to the last turn.
        let mut named = self.turn_named(turn);
        if let Some(words) = shown.user_prompt() {
            let _ = write!(named, " (\"{}\")", quoted(words));
        }
        let doing = if edit {
            format!("Edit {named}")
        } else {
            format!("Rewind to before {named}")
        };
        let undoable = self.undoable_from(turn);
        let files = if force {
            Ok(ForkFiles::UndoForce)
        } else {
            match (self.rewind_files, undoable) {
                (_, 0) | (RewindFiles::Keep, _) => Ok(ForkFiles::Keep),
                (RewindFiles::Undo, _) => Ok(ForkFiles::Undo),
                (RewindFiles::Ask, _) => Err(format!(
                    "undo the files {} changed",
                    counted(undoable, "turn", "turns")
                )),
            }
        };
        let what = What::Rewind { turn, edit };
        self.ask_move(what, &doing, files, !force);
    }

    /// How many turns from `turn` on, by its index among those held,
    /// have changes to files made, that a rewind could undo.
    fn undoable_from(&self, turn: usize) -> usize {
        (self.node.turns.get(turn..).unwrap_or_default().iter())
            .filter(|turn| {
                (turn.files).is_some_and(|files| {
                    matches!(files.state, TurnFilesState::Made | TurnFilesState::Partly)
                })
            })
            .count()
    }

    /// Asks for `what`, `doing` as the bar says it, once the user has
    /// said what's to become of the files, if `files` is the question to
    /// ask about them rather than the answer, and of the agent's
    /// background tasks, if any run: leaving the branch stops them. With
    /// `confirm`, it's asked even if nothing else is.
    fn ask_move(
        &mut self,
        what: What,
        doing: &str,
        files: Result<ForkFiles, String>,
        confirm: bool,
    ) {
        let tasks = (self.node.tasks.iter()).filter(|task| task.going()).count();
        let stop_tasks = tasks > 0;
        let tasks_said = stop_tasks.then(|| {
            format!(
                ", stopping {}",
                counted(tasks, "background task", "background tasks")
            )
        });
        let moved = |files| Move {
            what,
            files,
            stop_tasks,
        };
        let asked = match (files, tasks_said) {
            (Ok(files), None) if !confirm => return self.send_move(moved(files)),
            (Ok(files), tasks) => Confirm::new(format!(
                "{doing}{}? (y/n)",
                tasks.unwrap_or_default()
            ))
            .choice('y', "Go on", moved(files)),
            (Err(question), tasks) => Confirm::new(format!(
                "{doing}{}: {question}? (y/n)",
                tasks.unwrap_or_default(),
            ))
            .choice('y', "Undo the files", moved(ForkFiles::Undo))
            .choice('n', "Keep the files", moved(ForkFiles::Keep)),
        };
        self.branch_asked = Some(asked);
    }

    /// Whether a rewind or a switch asks something, in the bar.
    pub(crate) fn branch_asking(&self) -> Option<&str> {
        self.branch_asked.as_ref().map(Confirm::question)
    }

    /// Takes `stroke` as the answer to what the rewind or switch asks:
    /// one of its keys goes on, and any other stays where the chat is.
    pub(crate) fn answer_branch(&mut self, stroke: Stroke) {
        let Some(asked) = self.branch_asked.take() else {
            return;
        };
        match asked.answer(stroke) {
            Some(moved) => self.send_move(moved),
            None => {
                self.say("nothing was rewound or switched".to_owned());
            }
        }
    }

    /// What Ctrl+? says the keys do while a rewind or switch asks.
    pub(crate) fn branch_shortcuts(&self) -> Option<crate::shortcuts::Context> {
        let asked = self.branch_asked.as_ref()?;
        Some(crate::shortcuts::bindings::list(
            "Rewind or switch branch",
            None,
            |stroke, _| asked.hint(stroke),
        ))
    }

    /// Asks the daemon for `moved`; an edit tells the daemon what's typed
    /// first, for the message to go before it, and keeps the message to
    /// put in this box once it's done (see
    /// [`branch_answered`](Self::branch_answered)).
    fn send_move(&mut self, moved: Move) {
        let Move {
            what,
            files,
            stop_tasks,
        } = moved;
        let action = match what {
            What::Rewind { turn, edit } => {
                if edit {
                    let shown = (self.node.turns.get(turn)).map_or("", |turn| turn.prompt.as_str());
                    let (message, images) = clackatoo_protocol::without_image_labels(shown);
                    if let Some(typed) = self.take_draft(true) {
                        self.queued
                            .push(Input::Act(ChatAction::Draft { text: typed }));
                    }
                    self.memory.editing = Some(message);
                    if images {
                        self.say("its images aren't carried into the box: paste them again".into());
                    }
                }
                ChatAction::Rewind {
                    at: u32::try_from(turn).unwrap_or(u32::MAX),
                    files,
                    edit,
                    stop_tasks,
                }
            }
            What::Switch { turn, to } => ChatAction::SwitchBranch {
                at: u32::try_from(turn).unwrap_or(u32::MAX),
                to,
                files,
                stop_tasks,
            },
        };
        self.queued.push(Input::Act(action));
    }

    /// Takes the daemon's answer to a rewind or a switch: a refusal is
    /// said; an edit done puts the message before what's typed in this
    /// box, telling that as the draft at once. Returns whether the box
    /// took a message, for the pane to go to insert mode to send it.
    pub(crate) fn branch_answered(&mut self, response: Response) -> bool {
        let editing = self.memory.editing.take();
        if let Response::Error(error) = response {
            self.say(error.to_string());
            return false;
        }
        let Some(message) = editing else {
            return false;
        };
        let text = super::prompts::joined(&message, self.editor.raw_text());
        self.put_taken_back(&text);
        self.tell_draft_at_once();
        true
    }

    /// Whether an edit waits for the daemon, so the chat coming whole
    /// again mustn't fill the box from the daemon's draft meanwhile: the
    /// answer puts the message in.
    pub(super) fn editing(&self) -> bool {
        self.memory.editing.is_some()
    }

    /// The divergence `[b`, `]b` and `:branch` switch at: the one at the
    /// turn under the cursor; or a new branch not written to yet, with
    /// the cursor on the last turn or nowhere; or else the nearest above.
    fn divergence_here(&self) -> Option<(usize, &Divergence)> {
        let turns = self.node.turns.len();
        let local =
            |place: &Divergence| (place.at.checked_sub(self.node.first_turn)).map(|at| at as usize);
        let cursor = self.cursor.map(|cursor| cursor.text.turn);
        let places =
            || (self.node.branches.iter()).filter_map(|place| Some((local(place)?, place)));
        if let Some(turn) = cursor
            && let Some(found) = places().find(|&(at, _)| at == turn)
        {
            return Some(found);
        }
        if (cursor.is_none() || cursor == turns.checked_sub(1))
            && let Some(found) = places().find(|&(at, _)| at == turns)
        {
            return Some(found);
        }
        let below = cursor.unwrap_or(turns);
        places()
            .filter(|&(at, _)| at <= below)
            .max_by_key(|&(at, _)| at)
    }

    /// `]b`, or `[b` if `back`, `count` times: the branch after or before
    /// the one shown where they part (see [`divergence_here`]), stopping
    /// at the first and the last.
    ///
    /// [`divergence_here`]: Self::divergence_here
    pub(super) fn branch_step(&mut self, back: bool, count: Option<usize>) {
        let Some((turn, place)) = self.divergence_here() else {
            self.say("the conversation has no other branches here".to_owned());
            self.failed = true;
            return;
        };
        let step = u16::try_from(count.unwrap_or(1)).unwrap_or(u16::MAX);
        let to = if back {
            place.current.saturating_sub(step).max(1)
        } else {
            place.current.saturating_add(step).min(place.count)
        };
        if to == place.current {
            let end = if back { "first" } else { "last" };
            self.say(format!("this is the {end} branch here"));
            self.failed = true;
            return;
        }
        self.switch_branch(turn, to);
    }

    /// `:branch N`: branch `to`, from 1, where the branches part at the
    /// turn under the cursor, or else the nearest above it.
    pub(crate) fn branch_numbered(&mut self, to: u16) -> Result<(), String> {
        let (turn, _) = self
            .divergence_here()
            .ok_or("the conversation has no other branches here")?;
        self.switch_branch(turn, to);
        Ok(())
    }

    /// Switches to branch `to`, from 1, of those parting at turn `turn`,
    /// by its index among those held, as [`ChatAction::SwitchBranch`]
    /// does, asking first what's to become of the files and tasks.
    ///
    /// The window knows what's still made in the files of the branch shown,
    /// not of the one switched to: with nothing here to undo, it asks for
    /// the files to be undone all the same, which redoes only what leaving
    /// the other undid, putting the files back as that branch had them.
    pub(super) fn switch_branch(&mut self, turn: usize, to: u16) {
        if let Some(why) = self.branching_refused() {
            self.say(why.to_owned());
            self.failed = true;
            return;
        }
        let Some(place) = self.divergence_at(turn) else {
            self.say("the conversation has no other branches there".to_owned());
            self.failed = true;
            return;
        };
        let (current, count) = (place.current, place.count);
        if to == current {
            self.say(format!("branch {to} is the one shown"));
            return;
        }
        if to == 0 || to > count {
            self.say(format!(
                "there {} at {}",
                if count == 1 {
                    "is 1 branch".to_owned()
                } else {
                    format!("are {count} branches")
                },
                self.turn_named(turn),
            ));
            self.failed = true;
            return;
        }
        let doing = format!(
            "Switch to branch {to} of {count} at {}",
            self.turn_named(turn)
        );
        let undoable = self.undoable_from(turn);
        let files = match (self.rewind_files, undoable) {
            (RewindFiles::Keep, _) => Ok(ForkFiles::Keep),
            (RewindFiles::Undo, _) => Ok(ForkFiles::Undo),
            (RewindFiles::Ask, 0) => Err("redo its files".to_owned()),
            (RewindFiles::Ask, _) => Err(format!(
                "undo the files {} here changed, and redo its",
                counted(undoable, "turn", "turns")
            )),
        };
        let what = What::Switch { turn, to };
        self.ask_move(what, &doing, files, false);
    }

    /// Every branch of the conversation where it parts from others, for
    /// the finder (`:branches`): each named by where it parts and which
    /// it is there, as [`switch_picked`](Self::switch_picked) takes it,
    /// with its first message, how many turns it has from there and when
    /// it was last shown.
    pub(crate) fn branch_items(&self) -> Vec<crate::finder::LocalItem> {
        let now = self.now();
        let mut items = Vec::new();
        for place in &self.node.branches {
            let Some(turn) = (place.at.checked_sub(self.node.first_turn)).map(|at| at as usize)
            else {
                continue;
            };
            let named = self.turn_named(turn);
            for (index, branch) in place.branches.iter().enumerate() {
                let number = u16::try_from(index + 1).unwrap_or(u16::MAX);
                let head = format!("At {named} · {number}/{} ", place.count);
                let first = if branch.first.is_empty() {
                    "(a new branch, not written to)".to_owned()
                } else {
                    branch.first.clone()
                };
                let mut said = vec![counted(branch.turns as usize, "turn", "turns")];
                if number == place.current {
                    said.push("shown".to_owned());
                } else if branch.used > 0 {
                    said.push(format!("{} ago", ago(now.saturating_sub(branch.used))));
                }
                if branch.summary {
                    said.push("from a summary".to_owned());
                }
                let text = format!("{head}{first} · {}", said.join(" · "));
                items.push(crate::finder::LocalItem {
                    name: format!("{}:{number}", place.at),
                    name_len: head.len(),
                    preview: vec![branch.first.clone()],
                    text,
                    alert: None,
                });
            }
        }
        items
    }

    /// Shows the branch `picked` names, as
    /// [`branch_items`](Self::branch_items) names it.
    pub(crate) fn switch_picked(&mut self, picked: &str) -> Result<(), String> {
        let (at, to) = (picked.split_once(':'))
            .and_then(|(at, to)| Some((at.parse::<u32>().ok()?, to.parse::<u16>().ok()?)))
            .ok_or("no such branch")?;
        let turn = (at.checked_sub(self.node.first_turn))
            .ok_or("scroll up to load where that branch parts first")?;
        self.switch_branch(turn as usize, to);
        Ok(())
    }

    /// Older turns, `count` of them, have come before those held: what a
    /// rewind or a switch asks is about a turn numbered that many on.
    pub(super) fn branches_prepended(&mut self, count: usize) {
        if let Some(asked) = &mut self.branch_asked {
            asked.each_choice(|moved| match &mut moved.what {
                What::Rewind { turn, .. } | What::Switch { turn, .. } => *turn += count,
            });
        }
    }

    /// The chat came whole again with its turns from `turn` on, by index
    /// among those held, not those it held before, as after a rewind or a
    /// switch to another branch: what's kept by turn index from there on
    /// is let go of, and the view follows the conversation's end.
    pub(super) fn cut_from(&mut self, turn: usize) {
        self.cache.cut(turn);
        self.folds.retain(|&at, _| at < turn);
        self.open_agents.retain(|&at| at < turn);
        self.table_scrolls.retain(|&(at, _, _), _| at < turn);
        self.search_cut(turn);
        if self.cursor.is_some_and(|cursor| cursor.text.turn >= turn) {
            self.cursor = None;
            self.goal = None;
        }
        if (self.picked.as_ref())
            .is_some_and(|picked| picked.anchor.text.turn >= turn || picked.head.text.turn >= turn)
        {
            self.picked = None;
        }
        for key in [&mut self.hovered_code, &mut self.copied_code] {
            if key.is_some_and(|key| key.turn >= turn) {
                *key = None;
            }
        }
        for at in [&mut self.hovered_turn, &mut self.copied_answer] {
            if at.is_some_and(|at| at >= turn) {
                *at = None;
            }
        }
        self.shown.retain(|shown| shown.key.turn < turn);
        self.hits.retain(|(hit, _)| match hit {
            super::Hit::Card(at, _)
            | super::Hit::Undo(at)
            | super::Hit::Redo(at)
            | super::Hit::Fold(at)
            | super::Hit::Agents(at)
            | super::Hit::CopyAnswer(at) => *at < turn,
            super::Hit::CopyCode(key) => key.turn < turn,
            super::Hit::Branch { .. } => false,
            _ => true,
        });
        let past = |at: u32| at as usize >= turn;
        // An edit whose rewind is dropped puts nothing in the box.
        let edit_dropped = (self.queued.iter()).any(|input| {
            matches!(input, Input::Act(ChatAction::Rewind { at, edit: true, .. }) if past(*at))
        });
        if edit_dropped {
            self.memory.editing = None;
        }
        self.queued.retain(|input| match input {
            Input::Yank { start, end, .. } => start.text.turn < turn && end.text.turn < turn,
            Input::Search { from, .. } => from.text.turn < turn,
            Input::Act(
                ChatAction::Undo { turn: at, .. }
                | ChatAction::Redo { turn: at, .. }
                | ChatAction::Rewind { at, .. }
                | ChatAction::SwitchBranch { at, .. },
            ) => !past(*at),
            _ => true,
        });
        self.branch_asked = None;
        self.follow = true;
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        AgentTask, AgentTaskKind, AgentTaskState, BranchSummary, ChatAction, ChatNode, ForkFiles,
        Response, TurnFiles, TurnFilesState,
    };

    use crate::chat::ChatView;
    use crate::chat::fixtures::{fonts, read, talk};
    use crate::options::RewindFiles;
    use crate::vim::Stroke;

    fn node(prompts: &[&str]) -> ChatNode {
        let pairs: Vec<(&str, &str)> = prompts.iter().map(|prompt| (*prompt, "ok")).collect();
        talk(&pairs)
    }

    fn chat_of(node: ChatNode) -> ChatView {
        let mut chat = ChatView::new(String::new(), node);
        chat.branches_supported = true;
        chat
    }

    fn made(node: &mut ChatNode, index: usize) {
        node.turns[index].files = Some(TurnFiles {
            count: 1,
            state: TurnFilesState::Made,
        });
    }

    fn actions(chat: &mut ChatView) -> Vec<ChatAction> {
        (std::mem::take(&mut chat.queued).into_iter())
            .filter_map(|input| match input {
                super::Input::Act(action) => Some(action),
                _ => None,
            })
            .collect()
    }

    fn rewind(at: u32, files: ForkFiles, edit: bool, stop_tasks: bool) -> ChatAction {
        ChatAction::Rewind {
            at,
            files,
            edit,
            stop_tasks,
        }
    }

    fn switch(at: u32, to: u16, files: ForkFiles) -> ChatAction {
        ChatAction::SwitchBranch {
            at,
            to,
            files,
            stop_tasks: false,
        }
    }

    fn divergence(at: u32, current: u16, count: u16) -> clackatoo_protocol::Divergence {
        clackatoo_protocol::Divergence {
            at,
            current,
            count,
            dropped: 0,
            branches: vec![BranchSummary::default(); usize::from(count)],
        }
    }

    fn snapshot(node: ChatNode) -> clackatoo_protocol::ViewUpdate {
        clackatoo_protocol::ViewUpdate::Snapshot(clackatoo_protocol::View {
            title: String::new(),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::new(node)),
        })
    }

    #[test]
    fn a_rewind_always_names_the_turn_and_asks_before_it_goes() {
        let mut chat = chat_of(node(&[
            "one",
            "two, a long message that goes on and on",
            "three",
        ]));
        chat.rewind(1, false, false);
        assert_eq!(
            chat.branch_asking(),
            Some("Rewind to before turn 2 (\"two, a long message tha…\")? (y/n)")
        );
        chat.answer_branch(Stroke::char('n'));
        assert!(actions(&mut chat).is_empty());
        chat.rewind(1, false, false);
        chat.answer_branch(Stroke::char('y'));
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::Keep, false, false)]
        );
        // `:rewind!` asks nothing.
        chat.rewind(1, false, true);
        assert_eq!(chat.branch_asking(), None);
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::UndoForce, false, false)]
        );
    }

    #[test]
    fn a_rewind_asks_about_the_files_and_each_answer_goes_on_or_stays() {
        let mut shown = node(&["one", "two", "three"]);
        made(&mut shown, 2);
        let mut chat = chat_of(shown);
        chat.rewind(1, false, false);
        assert_eq!(
            chat.branch_asking(),
            Some("Rewind to before turn 2 (\"two\"): undo the files 1 turn changed? (y/n)")
        );
        chat.answer_branch(Stroke::char('y'));
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::Undo, false, false)]
        );
        chat.rewind(1, false, false);
        chat.answer_branch(Stroke::char('n'));
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::Keep, false, false)]
        );
        chat.rewind(1, false, false);
        chat.answer_branch(Stroke::Escape);
        assert!(actions(&mut chat).is_empty());
        assert_eq!(chat.branch_asking(), None);
        // The option answers about the files; the turn is still asked about.
        chat.rewind_files = RewindFiles::Undo;
        chat.rewind(1, false, false);
        chat.answer_branch(Stroke::char('y'));
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::Undo, false, false)]
        );
        chat.rewind_files = RewindFiles::Keep;
        chat.rewind(1, false, false);
        chat.answer_branch(Stroke::char('y'));
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::Keep, false, false)]
        );
    }

    #[test]
    fn background_tasks_running_are_asked_about_and_stopped() {
        let mut shown = node(&["one", "two"]);
        shown.tasks.push(AgentTask {
            id: "t".into(),
            kind: AgentTaskKind::Shell,
            title: "dev server".into(),
            command: None,
            state: AgentTaskState::Running,
            summary: None,
            started: 0,
            ended: None,
            can_stop: true,
            output: None,
        });
        let mut chat = chat_of(shown);
        chat.rewind(1, false, false);
        assert_eq!(
            chat.branch_asking(),
            Some("Rewind to before turn 2 (\"two\"), stopping 1 background task? (y/n)")
        );
        chat.answer_branch(Stroke::char('n'));
        assert!(actions(&mut chat).is_empty());
        chat.rewind(1, false, false);
        chat.answer_branch(Stroke::char('y'));
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::Keep, false, true)]
        );
    }

    #[test]
    fn an_edit_puts_the_message_before_whats_typed_once_done() {
        let mut chat = chat_of(node(&["one", "fix the bug\n[Image]"]));
        chat.editor.set_text("and more");
        chat.rewind(1, true, false);
        assert_eq!(
            chat.branch_asking(),
            Some("Edit turn 2 (\"fix the bug\")? (y/n)")
        );
        chat.answer_branch(Stroke::char('y'));
        let sent = actions(&mut chat);
        assert_eq!(sent.last(), Some(&rewind(1, ForkFiles::Keep, true, false)));
        assert!(chat.editing());
        assert!(chat.branch_answered(Response::Done));
        assert_eq!(chat.editor.raw_text(), "fix the bug\nand more");
        assert!(!chat.editing());
        // Not a message of the user's: refused, asking nothing.
        let mut chat = chat_of(node(&["one"]));
        chat.node.turns[0].woke = Some(String::new());
        chat.rewind(0, true, false);
        assert_eq!(chat.branch_asking(), None);
        assert!(actions(&mut chat).is_empty());
    }

    #[test]
    fn an_older_daemon_or_a_busy_agent_refuses_at_once() {
        let mut chat = chat_of(node(&["one", "two"]));
        chat.branches_supported = false;
        chat.rewind(1, false, false);
        assert_eq!(chat.branch_asking(), None);
        assert!(chat.take_failed());
    }

    #[test]
    fn brackets_step_through_the_branches_where_they_part_with_counts_and_stop_at_the_ends() {
        let mut shown = node(&["one", "two", "three"]);
        shown.branches = vec![divergence(1, 2, 3)];
        let mut chat = chat_of(shown);
        chat.rewind_files = RewindFiles::Undo;
        read(&mut chat, "[b");
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Undo)]);
        read(&mut chat, "5]b");
        assert_eq!(actions(&mut chat), [switch(1, 3, ForkFiles::Undo)]);
        read(&mut chat, "9[b");
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Undo)]);
        chat.node.branches = vec![divergence(1, 3, 3)];
        read(&mut chat, "]b");
        assert!(actions(&mut chat).is_empty());
        assert!(chat.take_failed());
        assert!(chat.branch_numbered(1).is_ok());
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Undo)]);
    }

    #[test]
    fn a_switch_asks_about_the_files_even_with_nothing_here_to_undo() {
        let mut shown = node(&["one", "two"]);
        shown.branches = vec![divergence(1, 2, 2)];
        let mut chat = chat_of(shown);
        chat.branch_step(true, None);
        assert_eq!(
            chat.branch_asking(),
            Some("Switch to branch 1 of 2 at turn 2: redo its files? (y/n)")
        );
        chat.answer_branch(Stroke::char('y'));
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Undo)]);
        chat.branch_step(true, None);
        chat.answer_branch(Stroke::char('n'));
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Keep)]);
        made(&mut chat.node, 1);
        chat.branch_step(true, None);
        assert!(
            chat.branch_asking()
                .unwrap()
                .contains("undo the files 1 turn here changed, and redo its")
        );
        chat.rewind_files = RewindFiles::Keep;
        chat.branch_asked = None;
        chat.branch_step(true, None);
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Keep)]);
    }

    #[test]
    fn a_new_branch_not_written_to_is_switched_from_with_the_cursor_at_the_end() {
        let mut shown = node(&["one"]);
        shown.branches = vec![divergence(1, 2, 2)];
        let mut chat = chat_of(shown);
        chat.rewind_files = RewindFiles::Undo;
        chat.branch_step(true, None);
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Undo)]);
    }

    #[test]
    fn rewinding_by_number_counts_as_people_do() {
        let mut chat = chat_of(node(&["one", "two", "three"]));
        assert!(chat.rewind_numbered(Some(2), true).is_ok());
        assert_eq!(
            actions(&mut chat),
            [rewind(1, ForkFiles::UndoForce, false, false)]
        );
        assert!(chat.rewind_numbered(None, true).is_ok());
        assert_eq!(
            actions(&mut chat),
            [rewind(2, ForkFiles::UndoForce, false, false)]
        );
        assert!(chat.rewind_numbered(Some(9), false).is_err());
    }

    #[test]
    fn the_keys_rewind_the_turn_in_view_or_else_the_last() {
        let mut fonts = fonts();
        let mut chat = chat_of(node(&["one", "two", "three"]));
        chat.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        read(&mut chat, "gg");
        read(&mut chat, "dG");
        assert!(chat.branch_asking().unwrap().contains("turn 1 (\"one\")"));
        chat.branch_asked = None;
        // The cursor's turn out of view: the last.
        chat.shown.clear();
        read(&mut chat, "dG");
        assert!(chat.branch_asking().unwrap().contains("turn 3 (\"three\")"));
    }

    #[test]
    fn older_turns_coming_while_asked_keep_the_question_on_its_turn() {
        let mut chat = chat_of(node(&["one", "two"]));
        chat.rewind(1, false, false);
        chat.queued
            .push(super::Input::Act(rewind(0, ForkFiles::Keep, false, false)));
        chat.prepend_history(3);
        assert_eq!(
            actions(&mut chat),
            [rewind(3, ForkFiles::Keep, false, false)]
        );
        chat.answer_branch(Stroke::char('y'));
        assert_eq!(
            actions(&mut chat),
            [rewind(4, ForkFiles::Keep, false, false)]
        );
    }

    #[test]
    fn the_switcher_is_drawn_where_branches_part_and_its_arrows_switch() {
        let mut fonts = fonts();
        let mut shown = node(&["one", "two", "three"]);
        shown.branches = vec![divergence(1, 2, 3)];
        let mut chat = chat_of(shown);
        chat.rewind_files = RewindFiles::Undo;
        chat.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        let arrow = |chat: &ChatView, to| {
            let (_, [x, y, width, height]) = (chat.hits.iter())
                .find(|(hit, _)| *hit == super::super::Hit::Branch { at: 1, to })
                .expect("the arrow is drawn");
            [x + width / 2.0, y + height / 2.0]
        };
        // Over turn 2, below turn 1's message.
        let back = arrow(&chat, 1);
        let turn = (chat.shown.iter())
            .find(|shown| shown.key.turn == 1)
            .unwrap();
        assert!(back[1] < turn.at[1], "over the turn's message");
        assert_eq!(chat.pointer(back), super::super::Pointer::Hand);
        chat.press(back, 1, false);
        assert_eq!(
            chat.settle(&mut fonts).actions,
            [switch(1, 1, ForkFiles::Undo)]
        );
        arrow(&chat, 3);
    }

    #[test]
    fn a_new_branch_not_written_to_has_a_row_after_the_last_turn_or_alone() {
        let mut fonts = fonts();
        let mut shown = node(&["one"]);
        shown.branches = vec![divergence(1, 2, 2)];
        let mut chat = chat_of(shown);
        chat.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        let row =
            (chat.hits.iter()).find(|(hit, _)| *hit == super::super::Hit::Branch { at: 1, to: 1 });
        assert!(row.is_some(), "the new branch's row, after the last turn");
        let mut empty = node(&[]);
        empty.branches = vec![divergence(0, 2, 2)];
        let mut chat = chat_of(empty);
        chat.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        let row =
            (chat.hits.iter()).find(|(hit, _)| *hit == super::super::Hit::Branch { at: 0, to: 1 });
        assert!(row.is_some(), "the new branch's row, in an empty chat");
    }

    #[test]
    fn a_rewind_lets_go_of_what_was_kept_of_the_turns_cut_and_a_reconnect_keeps_it() {
        let mut fonts = fonts();
        let shown = node(&["one", "two", "three"]);
        let mut chat = chat_of(shown.clone());
        chat.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        chat.folds.insert(0, true);
        chat.folds.insert(2, true);
        chat.open_agents.insert(2);
        chat.table_scrolls.insert((0, 2, 0), 10.0);
        chat.table_scrolls.insert((2, 2, 0), 10.0);
        // The same turns again, as after a reconnect: everything kept.
        chat.apply(&snapshot(shown.clone()));
        assert_eq!(chat.folds.len(), 2);
        assert_eq!(chat.table_scrolls.len(), 2);
        // Turn 2 on is another branch now: what's kept of it goes.
        let mut cut = node(&["one", "TWO"]);
        cut.turns[1].sent = 99_000;
        cut.branches = vec![divergence(1, 2, 2)];
        chat.apply(&snapshot(cut));
        assert_eq!(chat.folds.keys().copied().collect::<Vec<_>>(), [0]);
        assert!(chat.open_agents.is_empty());
        assert_eq!(
            chat.table_scrolls.keys().copied().collect::<Vec<_>>(),
            [(0, 2, 0)]
        );
        chat.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        assert!(
            chat.hits
                .iter()
                .any(|(hit, _)| matches!(hit, super::super::Hit::Branch { .. }))
        );
        // Cut back to fewer turns.
        chat.folds.insert(1, false);
        chat.apply(&snapshot(node(&["one"])));
        assert_eq!(chat.folds.keys().copied().collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn a_cut_lets_go_of_the_search_selection_and_queue_past_it() {
        use crate::chat::fixtures::paint;
        let mut fonts = fonts();
        let mut chat = chat_of(talk(&[("a cat", "ok"), ("b", "a cat too")]));
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "G");
        read(&mut chat, "/cat");
        crate::chat::fixtures::read_named(&mut chat, winit::keyboard::NamedKey::Enter);
        chat.settle(&mut fonts);
        read(&mut chat, "v");
        assert!(
            chat.search
                .found()
                .is_some_and(|found| found.text.turn == 1)
        );
        assert!(chat.picked.is_some());
        chat.queued.push(super::Input::Act(ChatAction::Undo {
            turn: 1,
            force: false,
        }));
        chat.queued.push(super::Input::Act(ChatAction::Undo {
            turn: 0,
            force: false,
        }));
        chat.queued
            .push(super::Input::Act(rewind(1, ForkFiles::Keep, true, false)));
        chat.memory.editing = Some("b".into());
        chat.cut_from(1);
        assert_eq!(chat.search.found(), None);
        assert!(chat.picked.is_none());
        assert!(chat.cursor.is_none());
        assert_eq!(
            actions(&mut chat),
            [ChatAction::Undo {
                turn: 0,
                force: false
            }]
        );
        assert!(
            !chat.editing(),
            "the edit's rewind went, and the box takes nothing"
        );
    }

    #[test]
    fn finder_rows_name_each_branch_and_pick_it() {
        let mut shown = node(&["one", "two"]);
        let mut place = divergence(1, 2, 2);
        place.branches[0].first = "two, the first time".into();
        place.branches[0].turns = 3;
        place.branches[1].first = "two".into();
        place.branches[1].turns = 1;
        place.branches[1].summary = true;
        shown.branches = vec![place];
        let mut chat = chat_of(shown);
        chat.rewind_files = RewindFiles::Undo;
        let items = chat.branch_items();
        assert_eq!(items.len(), 2);
        assert!(
            items[0]
                .text
                .starts_with("At turn 2 · 1/2 two, the first time · 3 turns")
        );
        assert!(items[1].text.ends_with("1 turn · shown · from a summary"));
        assert!(chat.switch_picked(&items[0].name).is_ok());
        assert_eq!(actions(&mut chat), [switch(1, 1, ForkFiles::Undo)]);
    }

    #[test]
    fn dg_rewinds_cc_edits_and_other_cuts_say_why_not() {
        let mut chat = chat_of(node(&["one", "two"]));
        for (keys, edit) in [("dG", false), ("cc", true), ("S", true)] {
            read(&mut chat, keys);
            chat.answer_branch(Stroke::char('y'));
            assert_eq!(
                actions(&mut chat).last(),
                Some(&rewind(1, ForkFiles::Keep, edit, false)),
                "{keys}"
            );
            chat.memory.editing = None;
        }
        assert!(!chat.take_failed());
        read(&mut chat, "dd");
        assert!(actions(&mut chat).is_empty());
        assert!(chat.take_failed());
        read(&mut chat, "]b");
        assert!(chat.take_failed(), "no branches to step through");
    }

    #[test]
    fn ctrl_question_mark_lists_the_keys_from_the_dialect() {
        let rows = crate::vim::shortcuts::context(
            "Chat",
            &crate::vim::Keys::default(),
            crate::vim::Mode::Normal,
            &crate::chat::read::ChatKeys,
        )
        .rows;
        let has = |key: &str| rows.iter().any(|(shown, _)| shown == key);
        assert!(
            has("dG") && has("cc") && has("S") && has("[b") && has("]b"),
            "{rows:?}"
        );
        assert!(!has("dd") && !has("dw"));
    }
}
