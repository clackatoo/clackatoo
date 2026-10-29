//! The chat's `@` menu: typing `@` at the start of a word in the message
//! offers the files of the chat's project, as the finder walks them (the
//! daemon's walk, off its loop, leaving out what git ignores), narrowed
//! fuzzily as the path is typed (CLACKA-193). It's made of what every
//! completion menu is made of (see [`crate::complete`]): the ranking, and
//! which is picked.
//!
//! | Key | Does |
//! |---|---|
//! | Down, Ctrl+N; Up, Ctrl+P | The next or previous file, round the ends |
//! | Tab, Enter | Puts `@path` in, from the chat's folder, quoted if it has a space, and a space after it |
//! | Esc | Closes the menu, until what's typed changes |
//!
//! The message goes as it's typed: the daemon finds the files it mentions
//! and sends them to the agent with it (see
//! [`clackatoo_protocol::mentions`]).

use clackatoo_protocol::{ChatPaths, Found, mention};
use winit::keyboard::{Key, NamedKey};

use super::paint::{PROMPT, Position};
use crate::complete::{Choice, Ranking};
use crate::fonts::Fonts;
use crate::input::Modifiers;
use crate::links::Target;
use crate::shortcuts::bindings::Hint;

/// The most files the menu shows at once; more are scrolled to.
pub(super) const HEIGHT: usize = 8;

/// The most files of a project the menu offers.
const MOST_FILES: usize = 200_000;

/// The menu: the project's files, what of them matches what's typed after
/// the `@`, and which is picked.
#[derive(Debug, Default)]
pub(crate) struct Mentions {
    /// The project's files, from its root, as the daemon walked them.
    files: Vec<String>,
    /// The project's root, as people write it, once the daemon has said.
    root: Option<String>,
    /// The files have been asked for since the menu opened.
    asked: bool,
    /// The ranking of the files, made again as more come.
    ranking: Option<Ranking>,
    /// How many files the ranking was made of.
    ranked: usize,
    choice: Choice,
    /// While the menu's open: where the mention being typed starts in the
    /// message, at its `@`, where it ends, and what's typed after the `@`.
    typed: Option<(usize, usize, String)>,
    /// The message as it was when Esc closed the menu, which stays closed
    /// until it changes.
    closed: Option<String>,
    /// The project's files are to be asked for, for the window to ask.
    wants_files: bool,
    /// A file a mention names, or a link, to open, as a click or `gf`
    /// asks, for the window to open.
    pub(super) opening: Option<Target>,
}

/// The mention being typed in `text` with the cursor at byte `cursor`:
/// where its `@` is, where it ends, and the path typed so far, without a
/// quote that opens it.
fn being_typed(text: &str, cursor: usize) -> Option<(usize, usize, &str)> {
    let before = text.get(..cursor)?;
    let at = before.rfind('@')?;
    let starts_word = before[..at]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    if !starts_word {
        return None;
    }
    let rest = &before[at + 1..];
    let path = match rest.strip_prefix('"') {
        Some(quoted) if !quoted.contains(['"', '\n']) => quoted,
        Some(_) => return None,
        None if rest.contains(char::is_whitespace) => return None,
        None => rest,
    };
    // A word the cursor is inside goes on to its end.
    let after = &text[cursor..];
    let end = cursor + after.find(char::is_whitespace).unwrap_or(after.len());
    Some((at, end, path))
}

/// `file`, a path from the project's root, from the folder `folder` is in
/// it, `inside`: as the agent, which works in the folder, reads it.
fn from_folder(file: &str, inside: &str) -> String {
    if inside.is_empty() {
        return file.to_owned();
    }
    let mut ups = inside.split('/').filter(|part| !part.is_empty()).count();
    let mut rest = file;
    let mut shared = inside.split('/').filter(|part| !part.is_empty());
    for part in shared.by_ref() {
        match rest
            .strip_prefix(part)
            .and_then(|after| after.strip_prefix('/'))
        {
            Some(after) => {
                rest = after;
                ups -= 1;
            }
            None => break,
        }
    }
    format!("{}{rest}", "../".repeat(ups))
}

impl Mentions {
    /// Takes in the message being typed, `text`, with the cursor at byte
    /// `cursor`: the menu opens if a mention is being typed there, and
    /// narrows to the files that match it. `typing` is whether the box
    /// takes keys now. Returns whether the project's files are to be asked
    /// for, as the menu opened without them.
    pub(crate) fn typed(&mut self, text: &str, cursor: usize, typing: bool) -> bool {
        if self.closed.as_deref().is_some_and(|closed| closed != text) {
            self.closed = None;
        }
        let found = being_typed(text, cursor).filter(|_| typing && self.closed.is_none());
        let Some((at, end, path)) = found else {
            if self.typed.take().is_some() {
                // Asked again next time, as files come and go.
                self.asked = false;
            }
            return false;
        };
        let ask = !self.asked;
        if ask {
            self.asked = true;
            // The root stays until the daemon says it again: it's the
            // chat's project's.
            self.files.clear();
            self.ranking = None;
        }
        self.typed = Some((at, end, path.to_owned()));
        self.rank();
        ask
    }

    /// Takes in what the daemon found of the project's files.
    pub(crate) fn found(&mut self, found: Found) {
        match found {
            Found::Root { name, .. } => self.root = Some(name),
            Found::Files(files) => {
                let room = MOST_FILES.saturating_sub(self.files.len());
                self.files.extend(files.into_iter().take(room));
            }
            _ => {}
        }
    }

    /// Ranks the files against what's typed, making the ranking again if
    /// more have come.
    fn rank(&mut self) {
        let Some((.., path)) = &self.typed else {
            return;
        };
        if self.ranking.is_none() || self.ranked != self.files.len() {
            let mut ranking = Ranking::new(
                self.files
                    .iter()
                    .map(|file| (file.as_str(), file.as_str(), false)),
            );
            ranking.keep_given_order();
            self.ranking = Some(ranking);
            self.ranked = self.files.len();
        }
        let Some(ranking) = &mut self.ranking else {
            return;
        };
        let before = (ranking.shown().first().copied(), ranking.shown().len());
        ranking.narrow(path);
        if (ranking.shown().first().copied(), ranking.shown().len()) != before {
            self.choice.restart();
        }
    }

    /// Whether the menu is open, whether anything matches yet or not.
    #[cfg(test)]
    pub(crate) fn open(&self) -> bool {
        self.typed.is_some()
    }

    /// Whether the menu shows: it's open, and something matches.
    pub(crate) fn showing(&self) -> bool {
        self.typed.is_some() && self.ranking.as_ref().is_some_and(|r| !r.shown().is_empty())
    }

    /// Closes the menu, as Esc does, until the message, `text`, changes.
    pub(crate) fn close(&mut self, text: &str) {
        self.closed = Some(text.to_owned());
        self.typed = None;
        self.asked = false;
    }

    /// Moves the pick to the next file, or the one before if not `down`.
    pub(crate) fn step(&mut self, down: bool) {
        let count = self.ranking.as_ref().map_or(0, |r| r.shown().len());
        self.choice.step(down, count, HEIGHT);
    }

    /// The file picked, from the project's root, if the menu shows.
    pub(crate) fn picked(&self) -> Option<&str> {
        let ranking = self.ranking.as_ref().filter(|_| self.showing())?;
        let index = *ranking.shown().get(self.choice.selected())?;
        self.files.get(index as usize).map(String::as_str)
    }

    /// What picking the file picked puts in place of the mention being
    /// typed in a chat working in `folder`, as people write it: where it
    /// starts and ends in the message, and `@path`, from the folder, with a
    /// space after it.
    pub(crate) fn completion(&self, folder: Option<&str>) -> Option<(usize, usize, String)> {
        let file = self.picked()?;
        let (at, end, _) = self.typed.as_ref()?;
        // The folder within the project, if it's in it.
        let inside = match (folder, &self.root) {
            (Some(folder), Some(root)) => folder
                .strip_prefix(root.as_str())
                .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                .unwrap_or_default(),
            _ => "",
        };
        let path = from_folder(file, inside.trim_start_matches('/'));
        Some((*at, *end, format!("{} ", mention(&path, None))))
    }

    /// The files in view, at most `height`, each with whether it's picked;
    /// and how many match in all.
    pub(crate) fn in_view(&mut self, height: usize) -> (Vec<(String, bool)>, usize) {
        self.rank();
        let Some(ranking) = self.ranking.as_mut().filter(|_| self.typed.is_some()) else {
            return (Vec::new(), 0);
        };
        let count = ranking.shown().len();
        let view = self.choice.in_view(count, height);
        let picked = self.choice.selected();
        let rows = (view.clone().zip(&ranking.shown()[view]))
            .map(|(place, &index)| (self.files[index as usize].clone(), place == picked))
            .collect();
        (rows, count)
    }

    /// Picks the file `row` rows down those in view, as a click does.
    pub(crate) fn click(&mut self, row: usize) {
        let count = self.ranking.as_ref().map_or(0, |r| r.shown().len());
        let place = self.choice.top() + row;
        if place < count {
            self.choice.go_to(place, HEIGHT);
        }
    }
}

impl super::ChatView {
    /// Takes in the message being typed, as the `@` menu reads it, once it
    /// may have changed: asking for the project's files if it opens.
    pub(crate) fn refresh_mentions(&mut self) {
        let typing = !self.reading
            && self.node.question.is_none()
            && self.menu.is_none()
            && self.handoff_sent.is_none()
            && !self.viewing_modal()
            && !self.slash.showing()
            && !self.recalling();
        let cursor = self.editor.raw_selection().focus().index();
        if self.mentions.typed(self.editor.raw_text(), cursor, typing) {
            self.mentions.wants_files = true;
        }
    }

    /// Takes what the daemon found of the chat's project's files, for the
    /// `@` menu.
    pub(crate) fn found_files(&mut self, found: Found) {
        self.mentions.found(found);
    }

    /// The file a message mentions at `spot`, in the conversation, if
    /// one does: a click opens it.
    pub(super) fn mention_at_spot(&self, spot: Position) -> Option<Target> {
        if spot.text.piece != PROMPT {
            return None;
        }
        let text = self.text(spot.text)?;
        let found = crate::links::mention_at(text.text, spot.byte)?;
        clackatoo_protocol::mentions(text.text)
            .iter()
            .any(|mention| mention.range == found.bytes)
            .then_some(found.target)
    }

    /// The file a message mentions at `spot`, or the link there, if
    /// there's one: `gf` opens it.
    pub(super) fn link_at_spot(&self, spot: Position) -> Option<crate::links::Found> {
        let text = self.text(spot.text)?;
        crate::links::mention_at(text.text, spot.byte)
    }

    /// Puts `text`, a mention, at the end of the message being typed, as
    /// `:Ask` attaches an editor's selection.
    pub(crate) fn append(&mut self, text: String) {
        self.queued.push(super::Input::Append(text));
    }

    /// Puts `text` at the end of the message being typed, after a space if
    /// the message doesn't end in one.
    pub(super) fn settle_append(&mut self, fonts: &mut Fonts, text: String) {
        let message = self.editor.raw_text();
        let space = !message.is_empty() && !message.ends_with(char::is_whitespace);
        let text = if space { format!(" {text}") } else { text };
        self.drive(fonts, |driver| {
            driver.move_to_text_end();
            driver.insert_or_replace_selection(&text);
        });
        self.refresh_slash();
    }

    /// Where it works, made absolute, if the daemon says.
    pub(crate) fn paths(&self) -> Option<&ChatPaths> {
        self.node.paths.as_ref()
    }

    /// The folder its agent works in, as people write it.
    pub(crate) fn folder(&self) -> Option<&str> {
        self.folder.as_deref()
    }

    /// The link `gf` or a click asked to open, for the window to open.
    pub(crate) fn take_opening(&mut self) -> Option<Target> {
        self.mentions.opening.take()
    }

    /// Whether the project's files are to be asked of the daemon, for the
    /// `@` menu, since this was last asked.
    pub(crate) fn take_wants_files(&mut self) -> bool {
        std::mem::take(&mut self.mentions.wants_files)
    }

    crate::shortcuts::bindings::described_keys! {
        pub(super) fn mention_key / describe_mention_key(&mut self, key: &Key, modifiers: Modifiers) -> bool;
        execute(fonts: &mut Fonts);
        prepare {
            let (shift, ctrl) = (modifiers.shift, modifiers.ctrl);
            let letter = match key {
                Key::Character(text) if ctrl => text.chars().next().map(|ch| ch.to_ascii_lowercase()),
                _ => None,
            };
        }
        match (key, letter) => {
            _ if !self.mentions.showing() =>
                Hint::None,
                false;
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('n')) if !shift =>
                Hint::Action("Next file"),
                {
                    self.mentions.step(true);
                    true
                };
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('p')) if !shift =>
                Hint::Action("Previous file"),
                {
                    self.mentions.step(false);
                    true
                };
            (Key::Named(NamedKey::Tab | NamedKey::Enter), _) if !shift && !ctrl =>
                Hint::Action("Mention the file"),
                self.complete_mention(fonts);
            (Key::Named(NamedKey::Escape), _) =>
                Hint::Action("Close the file menu"),
                {
                    self.mentions.close(self.editor.raw_text());
                    true
                };
            _ =>
                Hint::None,
                false;
        }
        result
    }

    /// Puts the file picked in the box, as `@path`, in place of the
    /// mention being typed. Returns whether one was.
    pub(super) fn complete_mention(&mut self, fonts: &mut Fonts) -> bool {
        let Some((at, end, with)) = self.mentions.completion(self.folder.as_deref()) else {
            return false;
        };
        self.drive(fonts, |driver| {
            driver.select_byte_range(at, end);
            driver.insert_or_replace_selection(&with);
        });
        self.refresh_mentions();
        true
    }
}

/// `path`, a file on the daemon's machine, made absolute, as a chat
/// working where `paths` say mentions it: from its folder, going up out
/// of it as far as it must within its project, or else whole. Whole too
/// when where it works isn't known, as from a daemon older than protocol
/// 119.
pub(crate) fn from_chat_paths(path: &str, paths: Option<&ChatPaths>) -> String {
    let under = |root: &str| {
        let root = root.trim_end_matches('/');
        (path.strip_prefix(root))
            .and_then(|rest| rest.strip_prefix('/'))
            .map(str::to_owned)
    };
    let Some(paths) = paths else {
        return path.to_owned();
    };
    if let Some(inside) = under(&paths.cwd) {
        return inside;
    }
    match (
        under(&paths.project),
        paths.cwd.strip_prefix(paths.project.trim_end_matches('/')),
    ) {
        (Some(file), Some(folder)) => from_folder(&file, folder.trim_matches('/')),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> Found {
        Found::Files(names.iter().map(|name| (*name).to_owned()).collect())
    }

    #[test]
    fn an_at_starting_a_word_opens_it_and_asks_for_the_files_once() {
        let mut menu = Mentions::default();
        assert!(menu.typed("look at @", 9, true), "asks");
        assert!(menu.open() && !menu.showing(), "none yet");
        menu.found(Found::Root {
            path: "/home/j/p".into(),
            name: "~/p".into(),
        });
        menu.found(files(&["src/main.rs", "src/lib.rs", "README.md"]));
        assert!(!menu.typed("look at @li", 11, true), "asked already");
        assert_eq!(menu.picked(), Some("src/lib.rs"));
        // Not inside a word, nor once a space ends it, nor in normal mode.
        assert!(!menu.typed("mail me@x", 9, true) && !menu.open());
        assert!(!menu.typed("@lib then", 9, true) && !menu.open());
        assert!(!menu.typed("@li", 3, false) && !menu.open());
        // Opened again, it asks again.
        assert!(menu.typed("@", 1, true));
    }

    #[test]
    fn picking_writes_the_path_from_the_chats_folder_quoted_if_it_must_be() {
        let mut menu = Mentions::default();
        menu.typed("see @ma please", 5, true);
        menu.found(Found::Root {
            path: "/home/j/p".into(),
            name: "~/p".into(),
        });
        menu.found(files(&["src/main.rs", "docs/my notes.md"]));
        menu.typed("see @ma please", 7, true);
        assert_eq!(
            menu.completion(Some("~/p")),
            Some((4, 7, "@src/main.rs ".into()))
        );
        assert_eq!(
            menu.completion(Some("~/p/src")),
            Some((4, 7, "@main.rs ".into()))
        );
        assert_eq!(
            menu.completion(Some("~/p/docs")),
            Some((4, 7, "@../src/main.rs ".into()))
        );
        menu.typed("see @\"my n", 10, true);
        assert_eq!(
            menu.completion(Some("~/p")),
            Some((4, 10, "@\"docs/my notes.md\" ".into()))
        );
    }

    #[test]
    fn esc_closes_it_until_the_message_changes_and_picking_goes_round() {
        let mut menu = Mentions::default();
        menu.typed("@", 1, true);
        menu.found(files(&["a1", "a2", "a3"]));
        menu.typed("@a", 2, true);
        menu.step(false);
        assert_eq!(menu.picked(), Some("a3"));
        menu.click(1);
        assert_eq!(menu.picked(), Some("a2"));
        menu.close("@a");
        assert!(!menu.typed("@a", 2, true) && !menu.showing());
        assert!(menu.typed("@a1", 3, true));
    }

    #[test]
    fn a_file_is_mentioned_from_the_chats_folder_as_far_as_its_project_goes() {
        let paths = ChatPaths {
            cwd: "/w/p/src".into(),
            project: "/w/p".into(),
        };
        let paths = Some(&paths);
        assert_eq!(from_chat_paths("/w/p/src/a.rs", paths), "a.rs");
        assert_eq!(from_chat_paths("/w/p/docs/b.md", paths), "../docs/b.md");
        assert_eq!(from_chat_paths("/w/q/c.rs", paths), "/w/q/c.rs");
        assert_eq!(from_chat_paths("/w/p/src/a.rs", None), "/w/p/src/a.rs");
    }

    #[test]
    fn paths_from_a_folder_go_up_as_far_as_they_must() {
        assert_eq!(from_folder("a/b/c.rs", ""), "a/b/c.rs");
        assert_eq!(from_folder("a/b/c.rs", "a"), "b/c.rs");
        assert_eq!(from_folder("a/b/c.rs", "a/x/y"), "../../b/c.rs");
        assert_eq!(from_folder("top.rs", "a/b"), "../../top.rs");
    }
}
