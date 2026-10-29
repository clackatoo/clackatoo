//! A chat that goes on with a session its agent kept, as the start page
//! `:Agent` opens resumes one: one of clackatoo's chats', or one started
//! elsewhere, as in a terminal.
//!
//! The chat starts with the session's ID, so its first connect opens that
//! session again rather than making one. Where the agent can, it's asked
//! to load it (ACP's `session/load`), which tells the conversation so far
//! as updates before it answers: those are taken as turns, each message
//! the user sent starting one. Nothing in them acts now: no edit is taken
//! as the agent writing a file, no tool counts as running, and a question
//! or a request about a file that comes with them is turned down, since
//! it was all said long ago. Where the agent can't load it, the session
//! is resumed, and the chat says the turns before aren't shown.
//!
//! It loads until the session opens. A load that couldn't be made, as
//! with no one logged in, is made again with the next message. One the
//! agent stopped telling partway, as when it stopped, is resumed instead,
//! so no turn is told twice.
//!
//! A load the chat stopped waiting for, as at Stop, may still be telling:
//! what more it tells is passed over until the agent answers it, and the
//! session isn't asked for again until then, so nothing the agent goes
//! on telling of it can be taken for what it does now.
//!
//! A message sent while it loads waits until the turns told again are in,
//! and goes after them. Another sent meanwhile joins it. If the load ends
//! another way, the message shows as a turn of its own, saying how.

use std::path::PathBuf;

use agent_client_protocol_schema::v1::{ContentBlock, EmbeddedResourceResource, SessionUpdate};
use clackatoo_protocol::{
    ChatState, Outcome, ResumeSession, Turn, mention, mention_lines, mentions,
};

use super::{Action, HostedChat, Link, TITLE, Wanted, clean, clean_text, nested, now};

/// What Claude Code keeps, as a message from the user, where a turn was
/// stopped: at a tool's question, or otherwise. Codex's adapter tells no
/// such thing: a turn of Codex's that was stopped is told again as one
/// that ended.
const INTERRUPTED: [&str; 2] = [
    "[Request interrupted by user]",
    "[Request interrupted by user for tool use]",
];

/// Where a chat is in loading a session its agent kept.
#[derive(Debug, Default)]
pub(super) struct Told {
    /// Its session is to be loaded as it opens, telling the conversation
    /// so far.
    load: bool,
    /// What its agent says of its session is passed over: the rest of a
    /// load it stopped waiting for, which the agent may still be telling,
    /// until the agent answers it.
    skip: bool,
    /// Some of the conversation so far came as the session loaded.
    replayed: bool,
    /// A message sent while its session loaded, to go once it has, after
    /// the turns told again.
    held: Option<super::Prompt>,
}

/// What becomes of a message held while the session loaded, once the
/// load is over.
#[derive(Debug)]
pub(super) enum Held {
    /// It goes, once the session opens.
    Send,
    /// It shows as a turn of its own that ended so.
    End(Outcome),
}

impl HostedChat {
    /// It goes on with `resume`, a session its agent kept, rather than
    /// making a new one: its first connect loads it, where the agent can.
    pub(crate) fn go_on_with(&mut self, resume: ResumeSession) {
        self.session = Some(resume.session);
        self.told.load = true;
        let title = clean(&resume.title);
        if !title.is_empty() {
            self.title = title;
            if let Some((cut, _)) = self.title.char_indices().nth(TITLE) {
                self.title.truncate(cut);
                self.title.push('…');
            }
        }
    }

    /// It starts on `model`, as its first session opens, where its agent
    /// offers it.
    pub(crate) fn want_model(&mut self, model: String) {
        let wanted = self.wanted.get_or_insert_with(Wanted::default);
        wanted.model = Some(model);
    }

    /// It was made again as the daemon started, from the workspaces kept
    /// from the last run (see `server::restore`): it doesn't ask its agent
    /// for its session until it's shown, or sent a message, so starting
    /// the daemon starts no agent.
    pub(crate) fn wait_to_be_shown(&mut self) {
        self.link = Link::Closed;
    }

    /// It's being shown: if it was made again as the daemon started and
    /// hasn't asked for the session it goes on with yet, it does now, so
    /// the conversation so far is told as it's looked at. Returns whether
    /// it's to be connected.
    pub(crate) fn opens_when_shown(&mut self) -> bool {
        let waiting = self.link == Link::Closed
            && self.told.load
            && !self.told.skip
            && self.node.turns.is_empty();
        if waiting {
            self.link = Link::Opening;
            self.set_state(ChatState::Starting);
        }
        waiting
    }

    /// The model it's on, by its value, as its agent last said.
    pub(crate) fn model(&self) -> Option<&str> {
        let setting = (self.node.settings.iter())
            .find(|setting| setting.kind == clackatoo_protocol::SettingKind::Model)?;
        Some(&setting.value)
    }

    /// Whether its session is to be loaded as it opens, telling the
    /// conversation so far: what its agent says of the session until it
    /// opens is that conversation, to [`replay`](Self::replay).
    pub(crate) fn loading(&self) -> bool {
        self.told.load
    }

    /// Whether what its agent says of its session is passed over, as the
    /// rest of a load it stopped waiting for: until the agent answers that
    /// load (see [`load_answered`](Self::load_answered)).
    pub(crate) fn skipping(&self) -> bool {
        self.told.skip
    }

    /// Whether what its agent says of its session was said long ago, and
    /// so mustn't act now: it's loading, or passing over what's left of a
    /// load.
    pub(crate) fn told_again(&self) -> bool {
        self.told.load || self.told.skip
    }

    /// Takes a piece of the conversation so far, as the agent tells it
    /// while its session loads. A message from the user starts a turn,
    /// ending the last; what the agent did before any goes in a turn
    /// with no message. Nothing a tool did is taken as written now.
    pub(crate) fn replay(&mut self, update: SessionUpdate) {
        self.told.replayed = true;
        match update {
            // What a subagent was told, as Claude Code's adapter tells it
            // again: the tool call that started it shows it already, and
            // it's no message of the user's.
            SessionUpdate::UserMessageChunk(chunk) if nested::told_a_subagent(&chunk) => {}
            SessionUpdate::UserMessageChunk(chunk) => {
                // A file the message mentioned comes back as what the
                // adapter made of it: a link, or as Claude's and Codex's
                // adapters keep them, the words they gave the model.
                let mentioned = match &chunk.content {
                    ContentBlock::Text(text) => match replayed_context(&text.text) {
                        Replayed::Words => None,
                        Replayed::Link(uri) => Some(uri),
                        Replayed::Text => return,
                    },
                    ContentBlock::ResourceLink(link) => Some(link.uri.as_str()),
                    ContentBlock::Resource(resource) => match &resource.resource {
                        EmbeddedResourceResource::TextResourceContents(text) => {
                            Some(text.uri.as_str())
                        }
                        EmbeddedResourceResource::BlobResourceContents(blob) => {
                            Some(blob.uri.as_str())
                        }
                        _ => return,
                    },
                    _ => None,
                };
                if let Some(uri) = mentioned {
                    let uri = uri.to_owned();
                    self.replay_mention(&uri);
                    return;
                }
                let text = match chunk.content {
                    ContentBlock::Text(text) => without_inline_images(clean_text(&text.text)),
                    ContentBlock::Image(_) => "[Image]".to_owned(),
                    _ => return,
                };
                // Claude Code keeps a stop as a message of the user's own:
                // the turn it ends was stopped, and no one wrote it.
                if INTERRUPTED.contains(&text.trim()) {
                    self.end_turn(Outcome::Stopped);
                    return;
                }
                // A message of several blocks comes a block at a time,
                // before the agent does anything with it.
                if let Some(index) = self.answering_turn()
                    && self.node.turns[index].items.is_empty()
                    && !self.node.turns[index].prompt.is_empty()
                {
                    let prompt = &mut self.node.turns[index].prompt;
                    prompt.push_str("\n\n");
                    prompt.push_str(&text);
                    self.patch_turn(index);
                    return;
                }
                self.end_turn(Outcome::Done);
                // As a message sent now would, it lets go of a plan all
                // done.
                self.forget_finished_plan();
                self.push_replayed(text);
            }
            update @ (SessionUpdate::AgentMessageChunk(_)
            | SessionUpdate::AgentThoughtChunk(_)
            | SessionUpdate::ToolCall(_)
            | SessionUpdate::ToolCallUpdate(_)) => {
                if !self.answering() {
                    self.push_replayed(String::new());
                }
                // Written long ago: never merged into buffers as the
                // agent's, nor kept for undo.
                let _ = self.update(update);
            }
            // Its title, settings, usage and plan, as at any time.
            update => {
                let _ = self.update(update);
            }
        }
    }

    /// Adds a turn told again, with `prompt` its message, still open for
    /// what the agent did with it.
    /// Takes in a file the message being told again mentioned, `uri`: as
    /// `@path` in its words, unless they mention it already, as the
    /// user's words do when clackatoo sent it.
    fn replay_mention(&mut self, uri: &str) {
        let Some((path, lines)) = file_of(uri) else {
            return;
        };
        let cwd = PathBuf::from(&self.cwd);
        let written = match path.strip_prefix(&cwd) {
            Ok(inside) if path.starts_with(&self.project) => inside.display().to_string(),
            _ => path.display().to_string(),
        };
        let open = (self.answering_turn()).filter(|&index| self.node.turns[index].items.is_empty());
        if let Some(index) = open {
            let prompt = &self.node.turns[index].prompt;
            let already = mentions(prompt).iter().any(|mentioned| {
                mentioned.lines == lines
                    && [cwd.as_path(), self.project.as_path()]
                        .iter()
                        .any(|from| crate::find::lexical(&from.join(mentioned.path)) == path)
            });
            if already {
                return;
            }
            let prompt = &mut self.node.turns[index].prompt;
            if !prompt.is_empty() {
                prompt.push(' ');
            }
            prompt.push_str(&mention(&written, lines));
            self.patch_turn(index);
            return;
        }
        self.end_turn(Outcome::Done);
        self.forget_finished_plan();
        self.push_replayed(mention(&written, lines));
    }

    fn push_replayed(&mut self, prompt: String) {
        self.push_turn(Turn {
            woke: None,
            previews: Vec::new(),
            prompt,
            sent: now(),
            items: Vec::new(),
            end: None,
            files: None,
            from: None,
            switch: None,
        });
    }

    /// Its session has opened again: told again as it opened if `loaded`,
    /// or else resumed, as its agent can't load one, which the chat says.
    /// The session has seen every turn it shows. A message sent while it
    /// loaded goes now.
    pub(crate) fn opened_again(&mut self, loaded: bool) {
        if !self.told.load {
            return;
        }
        self.finish_loading();
        if !loaded {
            self.set_notice(Some(format!(
                "{} can't tell clackatoo a conversation it kept, so the turns before aren't \
                 shown here. It remembers them, and goes on from where it was.",
                self.node.agent
            )));
        }
        self.release_held(Held::Send);
    }

    /// Its session couldn't be opened again, saying `why`: it starts a new
    /// one, which hears the turns shown, if any came, with its next
    /// message, and the chat says so. A message sent while it loaded goes
    /// now.
    pub(crate) fn load_failed(&mut self, why: &str) {
        if !self.told.load {
            return;
        }
        self.finish_loading();
        self.known = 0;
        self.set_notice(Some(format!(
            "{} couldn't open that conversation again ({why}), so this is a new one.",
            self.node.agent
        )));
        self.release_held(Held::Send);
    }

    /// The chat stops waiting for its load, which the agent may go on
    /// telling: what came of the conversation so far stays, what more
    /// comes is passed over until the agent answers it, and the session is
    /// resumed. The message held goes as `held` says.
    pub(super) fn stop_waiting_for_load(&mut self, held: Held) {
        if !self.told.load {
            return;
        }
        if self.link == Link::Opening {
            self.told.skip = true;
        }
        let nothing = !self.told.replayed;
        self.finish_loading();
        if nothing {
            self.set_notice(Some(format!(
                "{} hadn't told the conversation so far yet, so the turns before aren't shown \
                 here. It remembers them.",
                self.node.agent
            )));
        }
        self.release_held(held);
    }

    /// Its agent stopped, or is being started again, while its session
    /// loaded: nothing more of the load comes. A load nothing came of yet
    /// is made again when the session is next asked for; one told partway
    /// is resumed instead, so no turn is told twice. The message held goes
    /// as `held` says.
    pub(super) fn load_cut_off(&mut self, held: Held) {
        self.told.skip = false;
        if self.told.load && self.told.replayed {
            self.finish_loading();
        }
        self.release_held(held);
    }

    /// The agent answered a request for the session the chat stopped
    /// waiting for, so what it told for it has been told. Returns whether
    /// the chat now asks for its session, which it waited to do.
    pub(crate) fn load_answered(&mut self) -> bool {
        std::mem::take(&mut self.told.skip) && self.link == Link::Opening
    }

    /// Ends the turn told again last, if one is open: the session has seen
    /// every turn.
    fn finish_loading(&mut self) {
        self.told.load = false;
        self.end_turn(Outcome::Done);
        self.known = self.node.turns.len();
    }

    /// Holds `text`, a message sent while the session is to load, as the
    /// turn it starts must come after those told again, joining any held
    /// already; and asks for the session, if it isn't asked for. Returns
    /// whether it did, adding to `actions` what the server must do.
    pub(super) fn hold_while_loading(
        &mut self,
        text: &super::Prompt,
        actions: &mut Vec<Action>,
    ) -> bool {
        if !self.told.load {
            return false;
        }
        match &mut self.told.held {
            Some(held) => {
                if held.images.len() + text.images.len() > clackatoo_protocol::ChatImage::MAX_COUNT
                    || held
                        .images
                        .iter()
                        .chain(&text.images)
                        .map(|image| image.data.len())
                        .sum::<usize>()
                        > clackatoo_protocol::ChatImage::MAX_DATA
                {
                    self.set_notice(Some(
                        "The message waiting for this session already has too many images.".into(),
                    ));
                    return true;
                }
                held.text.push_str("\n\n");
                held.text.push_str(&text.text);
                held.images.extend(text.images.clone());
            }
            None => self.told.held = Some(text.to_owned()),
        }
        self.set_notice(Some(format!(
            "Your message goes once {} has told the conversation so far.",
            self.node.agent
        )));
        self.set_state(ChatState::Starting);
        if self.link == Link::Closed {
            self.link = Link::Opening;
            actions.push(Action::Connect);
        }
        true
    }

    /// Whether a message waits for the session to load.
    pub(super) fn holding(&self) -> bool {
        self.told.held.is_some()
    }

    /// Does with the message held while the session loaded, if there is
    /// one, as `held` says.
    pub(super) fn release_held(&mut self, held: Held) {
        let Some(text) = self.told.held.take() else {
            return;
        };
        match held {
            Held::Send => {
                let mut actions = Vec::new();
                self.send_prompt(text, &mut actions);
                // The session is being asked for whenever a message is
                // released to go.
                debug_assert!(actions.is_empty(), "nothing goes before the session opens");
            }
            Held::End(outcome) => {
                self.set_notice(None);
                self.push_turn(Turn {
                    woke: None,
                    previews: Vec::new(),
                    prompt: text.shown(),
                    sent: now(),
                    items: Vec::new(),
                    end: None,
                    files: None,
                    from: None,
                    switch: None,
                });
                // As it was, images and all, to send again.
                self.remember_prompt(self.node.turns.len() - 1, &text);
                self.end_turn(outcome);
            }
        }
    }

    /// The chat is handed over to the next build mid-load: the turns that
    /// came are kept as heard, as the next build resumes the session, and
    /// a message held shows as cut off.
    pub(super) fn hand_over_loading(&mut self) {
        if self.told.load {
            self.finish_loading();
        }
        self.release_held(Held::End(Outcome::Failed(
            "cut off by an upgrade".to_owned(),
        )));
    }

    /// Preserve a user message waiting behind history replay as an unsent
    /// turn. Replayed historical work alone is never continued.
    pub(super) fn keep_held_for_upgrade(&mut self) {
        if self.told.load {
            self.finish_loading();
        }
        if let Some(prompt) = self.told.held.take() {
            self.link = Link::Opening;
            self.begin(prompt.shown(), prompt, None, &mut Vec::new());
        }
    }
}

/// Some adapters replay image inputs as standalone Markdown links containing
/// their data URI rather than ACP image blocks. Keep the same label we use for
/// replayed image blocks; binary payloads must never enter the visible history.
/// Also used when accepting history handed over by an older daemon.
pub(super) fn without_inline_images(text: String) -> String {
    const PREFIX: &str = "[@image](data:image/";
    if !text.contains(PREFIX) {
        return text;
    }
    let mut shown = String::with_capacity(text.len().min(4096));
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let inline = body
            .strip_prefix(PREFIX)
            .and_then(|uri| uri.strip_suffix(')'))
            .and_then(|uri| uri.split_once(";base64,"))
            .is_some_and(|(mime, data)| {
                !mime.is_empty()
                    && !data.is_empty()
                    && data.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
                    })
            });
        if inline {
            shown.push_str("[Image]");
            if line.ends_with('\n') {
                shown.push('\n');
            }
        } else {
            shown.push_str(line);
        }
    }
    shown
}

/// What a block of words the user's side of a session told again is, as
/// the adapters write the files a message mentioned (see
/// `server/mentions.rs`).
#[derive(Debug, PartialEq, Eq)]
enum Replayed<'a> {
    /// The user's own words.
    Words,
    /// A link to the file at this URI, `[@name](file:///…)`: Claude's for
    /// a link or the text sent, Codex's for a link, and Codex's for the
    /// text sent, with the text after it.
    Link(&'a str),
    /// The text of a file sent, as Claude's adapter put it after the
    /// message: `<context ref="file:///…">…</context>`.
    Text,
}

/// What the block of words `text` is, of [`Replayed`].
fn replayed_context(text: &str) -> Replayed<'_> {
    if let Some(uri) = link(text) {
        return Replayed::Link(uri);
    }
    if let Some((first, rest)) = text.split_once('\n')
        && let Some(uri) = link(first)
        && context(rest, Some(uri))
    {
        return Replayed::Link(uri);
    }
    if context(text.strip_prefix('\n').unwrap_or(text), None) {
        return Replayed::Text;
    }
    Replayed::Words
}

/// The file URI `line` links to, if it's all an adapter's link to a
/// file, `[@name](file:///…)`.
fn link(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("[@")?;
    let (name, rest) = rest.split_once("](")?;
    let uri = rest.strip_suffix(')')?;
    let plain = !name.contains(['\n', ']']) && !uri.contains(char::is_whitespace);
    (plain && uri.starts_with("file://")).then_some(uri)
}

/// Whether `text` is all a file's text as an adapter gives it the model,
/// `<context ref="file:///…">…</context>`, and of `uri` if it's given.
fn context(text: &str, uri: Option<&str>) -> bool {
    let Some(rest) = text.strip_prefix("<context ref=\"") else {
        return false;
    };
    let Some((reference, _)) = rest.split_once('"') else {
        return false;
    };
    reference.starts_with("file://")
        && uri.is_none_or(|uri| uri == reference)
        && rest.ends_with("</context>")
}

/// The file a `file://` URI names, and the lines its fragment says,
/// `#L12-L20`.
fn file_of(uri: &str) -> Option<(PathBuf, Option<(u32, u32)>)> {
    let url = url::Url::parse(uri).ok()?;
    let path = url.to_file_path().ok()?;
    let lines = (url.fragment()).and_then(|fragment| mention_lines(&format!("#{fragment}")));
    Some((path, lines))
}

#[cfg(test)]
mod tests {
    use super::{Replayed, replayed_context, without_inline_images};

    #[test]
    fn the_adapters_words_for_files_sent_are_told_apart_from_the_users() {
        let uri = "file:///p/a.rs";
        assert_eq!(
            replayed_context("[@a.rs](file:///p/a.rs)"),
            Replayed::Link(uri)
        );
        assert_eq!(
            replayed_context(
                "[@a.rs](file:///p/a.rs)\n<context ref=\"file:///p/a.rs\">\nx\n</context>"
            ),
            Replayed::Link(uri)
        );
        assert_eq!(
            replayed_context("\n<context ref=\"file:///p/a.rs\">\nx\n</context>"),
            Replayed::Text
        );
        for words in [
            "[@a.rs](https://example.com)",
            "[@a.rs](file:///p/a.rs) and more",
            "[@a.rs](file:///p/a.rs)\n<context ref=\"file:///p/b.rs\">\nx\n</context>",
            "<context ref=\"file:///p/a.rs\">unclosed",
            "plain words",
        ] {
            assert_eq!(replayed_context(words), Replayed::Words, "{words}");
        }
    }

    #[test]
    fn inline_image_labels_preserve_surrounding_text_and_ordinary_links() {
        for text in [
            "Text with accents: café\n\n",
            "[@image](https://example.com/picture.png)",
            "Use [@image](data:image/png;base64,AAAA) in this example.",
            "[@image](data:image/png;base64,not base64)",
            "[@image](data:image/png;base64,)",
        ] {
            assert_eq!(without_inline_images(text.into()), text);
        }
        assert_eq!(
            without_inline_images("Before\n[@image](data:image/jpeg;base64,a+/=)\nAfter".into()),
            "Before\n[Image]\nAfter"
        );
    }
}
