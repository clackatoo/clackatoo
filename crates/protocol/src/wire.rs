//! Versions on the wire: how a client and a daemon of different releases
//! still talk.
//!
//! Each connection runs at one protocol version, its *wire* version: the
//! older of the two sides', agreed in the `Hello`s (see the crate docs).
//! Everything sent on it is encoded as a build of that version would, and
//! read as one would write it. [`frame::encode`](crate::frame::encode) and
//! [`FrameDecoder::decode`](crate::frame::FrameDecoder::decode) take the
//! wire version for that, and hold it in a thread-local while they run,
//! for the serde helpers here to read.
//!
//! The messages are postcard: positional, with no field names and no
//! lengths inside a message, so a peer can skip nothing it doesn't know.
//! Changes are kept readable both ways by these rules:
//!
//! - **New messages, variants and fields go at the end.** Nothing is
//!   reordered, removed or reshaped: a reshape is a new variant, and the
//!   old one stays handled while any peer that sends it can connect.
//! - **A new field is gated** with [`before`] and [`since`], at a named
//!   gate below, so it's left out for an older peer and filled in with its
//!   `Default` from one:
//!
//!   ```ignore
//!   #[serde(
//!       skip_serializing_if = "wire::before::<{ wire::REVIEW_CHATS }, _>",
//!       deserialize_with = "wire::since::<{ wire::REVIEW_CHATS }, _, _>"
//!   )]
//!   pub chats: Vec<ReviewChat>,
//!   ```
//!
//!   The default must mean "an older peer", never something it didn't say:
//!   an `Option` with `None`, or a value the daemon fills in for the
//!   connection. `#[serde(default)]` does nothing under postcard.
//! - **A field that makes a request safer** isn't defaulted: the daemon
//!   refuses the request from older peers with `Error::Unsupported`, and a
//!   newer client asks [`daemon_has`] before sending it, as an older daemon
//!   would read past the field and do without it.
//! - **An answer is never refused** ([`Downgraded::Refused`]) to a peer
//!   that has the fallbacks: refusing a reply drops the client, which
//!   connects again into the same refusal, for ever. Give it an arm that
//!   turns what's newer into what it can read.
//! - **A new variant is never sent to a peer older than it.** Each variant
//!   of the enums the daemon pushes inside its state says when it arrived
//!   ([`Since`], which doesn't compile until it does), and what's pushed
//!   is turned, for an older peer, into what it can read, by [`Downgrade`]:
//!   a new pane kind into [`Content::Other`], a new error into
//!   [`Error::Other`]. A peer from before [`FALLBACKS`] has no `Other`:
//!   what's for it is dropped, or refused, never put in an `Other`. A new
//!   request is only sent once [`daemon_has`] says the daemon reads it.
//! - **The serde types of `clackatoo_term` and `clackatoo_text` are
//!   protocol types** too: a variant added to one needs a downgrade, or
//!   closes the window.
//! - **Nothing per cell or per line is gated** (`Line`, `Run`,
//!   `HistoryLine`, `Style`, `Color`, `Underline`, `Edit`'s ops): a change
//!   there closes the window, by raising the floors to the new version.
//! - **A change of meaning on the same bytes** is gated, the daemon
//!   keeping the old meaning for older peers, or closes the window.
//! - **Every new type, field or variant gets a sample** in
//!   [`corpus`](crate::corpus) that reaches it, and the last variant of
//!   every enum is sampled, so the release fixtures pin it.
//!
//! [`Content::Other`]: crate::Content::Other
//! [`Error::Other`]: crate::Error::Other

use std::cell::Cell;

use serde::{Deserialize, Deserializer};

use crate::{
    BufferUpdate, ChatPatch, ClientMessage, Content, Error, Event, FilesUpdate, Item, Node,
    PROTOCOL_VERSION, Patch, Response, ServerMessage, Setting, SettingKind, ToolContent, Turn,
    ViewUpdate, Workspaces,
};

/// The oldest client a daemon of this build talks to.
///
/// Moved only when a release is made, and never past the previous
/// release's protocol.
pub const MIN_CLIENT: u32 = 72;

/// The oldest daemon a client of this build talks to.
///
/// Moved only when a release is made, and never past the previous
/// release's protocol. **Never below [`FALLBACKS`]:** a daemon older than
/// that refuses a client by answering `Hello` with its own version and
/// hanging up, which a client that took that version would read as
/// agreement. It would then find the connection closed, and a remote
/// window would connect again into the same refusal for ever, never
/// saying the daemon needs updating.
pub const MIN_DAEMON: u32 = 73;

const _: () = assert!(MIN_DAEMON >= FALLBACKS, "see MIN_DAEMON");
const _: () = assert!(
    FALLBACKS > crate::corpus::FIRST,
    "the corpus began at the last release without the fallbacks"
);
const _: () = assert!(MIN_CLIENT <= PROTOCOL_VERSION && MIN_DAEMON <= PROTOCOL_VERSION);

/// Declares the gates: the protocol versions things arrived in, by name,
/// and [`GATES`], the list of them.
macro_rules! gates {
    ($($(#[$doc:meta])* $name:ident = $version:literal,)*) => {
        $($(#[$doc])* pub const $name: u32 = $version;)*

        /// Every gate, by name.
        pub const GATES: &[(&str, u32)] = &[$((stringify!($name), $name),)*];
    };
}

gates! {
    /// The fallback variants a daemon turns what's newer into
    /// (`Content::Other`, `Node::Other`, `Item::Other`,
    /// `ToolContent::Other`, `Error::Unsupported`, `Error::Other`), and
    /// reading past frames that can't be read rather than hanging up.
    /// Renumbered with the protocol version it lands in, if that changes
    /// on a rebase, and `MIN_DAEMON` with it: never below it.
    FALLBACKS = 73,
}

/// The oldest protocol version any peer of this build can speak: older
/// gates and fixtures are of no more use.
pub const OLDEST: u32 = if MIN_CLIENT < MIN_DAEMON {
    MIN_CLIENT
} else {
    MIN_DAEMON
};

thread_local! {
    /// The wire version of what's being encoded or decoded on this thread.
    static WIRE: Cell<u32> = const { Cell::new(PROTOCOL_VERSION) };
}

/// The wire version of what's being encoded or decoded on this thread, or
/// this build's own outside of that.
#[must_use]
pub fn current() -> u32 {
    WIRE.get()
}

/// Runs `f` with [`current`] at `wire`, putting it back after, even if `f`
/// panics.
pub(crate) fn speaking<R>(wire: u32, f: impl FnOnce() -> R) -> R {
    /// Puts the version back when dropped.
    struct Restore(u32);
    impl Drop for Restore {
        fn drop(&mut self) {
            WIRE.set(self.0);
        }
    }
    let _restore = Restore(WIRE.replace(wire));
    f()
}

/// For `skip_serializing_if`: whether what's encoded is for a peer older
/// than version `V`, which hasn't the field.
#[must_use]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
pub fn before<const V: u32, T: ?Sized>(_field: &T) -> bool {
    current() < V
}

/// For `deserialize_with`: reads the field if what's read is from a peer
/// of version `V` or later, and otherwise, reading nothing, takes its
/// default.
///
/// # Errors
///
/// If the field is there and can't be read.
pub fn since<'de, const V: u32, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    if current() >= V {
        T::deserialize(deserializer)
    } else {
        Ok(T::default())
    }
}

/// The wire version a daemon of this build runs a connection at, for a
/// client that said it speaks `client`, or `None` if it's too old.
#[must_use]
pub fn for_client(client: u32) -> Option<u32> {
    (client >= MIN_CLIENT).then(|| client.min(PROTOCOL_VERSION))
}

/// Whether a client of this build can run a connection at `reply`, the
/// version a daemon answered its `Hello` with. A daemon newer than
/// [`MIN_CLIENT`] allows answers with its own, newer version when it
/// refuses the client; a daemon older than [`MIN_DAEMON`] can't be talked
/// to.
#[must_use]
pub fn takes_reply(reply: u32) -> bool {
    (MIN_DAEMON..=PROTOCOL_VERSION).contains(&reply)
}

/// Whether a connection at `wire` has what arrived at `gate`: whether
/// the daemon on the other end reads a request new then, say, before a
/// window offers what needs it.
#[must_use]
pub fn daemon_has(wire: u32, gate: u32) -> bool {
    wire >= gate
}

/// Older than any peer of this build can be: what every one has.
pub const EARLIER: u32 = 0;

/// The protocol version a variant arrived in, of the enums whose variants
/// are pushed to a client inside the daemon's state, where a peer older
/// than the variant can't be sent it: [`EARLIER`] for what every peer has.
///
/// Each implementation matches every variant, so a variant added to one
/// of these enums doesn't compile until it says when it arrived; and then
/// [`newest`] finds it, and [`Downgrade`] keeps it from older peers.
pub trait Since {
    /// The protocol version it arrived in.
    fn since(&self) -> u32;
}

impl Since for Content {
    fn since(&self) -> u32 {
        match self {
            Self::Terminal(_)
            | Self::Buffer(_)
            | Self::Files(_)
            | Self::Board(_)
            | Self::Review(_)
            | Self::Browser(_)
            | Self::Folder(_) => EARLIER,
            Self::Other { .. } => FALLBACKS,
        }
    }
}

impl Since for Node {
    fn since(&self) -> u32 {
        match self {
            Self::Grid(_) | Self::Chat(_) => EARLIER,
            Self::Other => FALLBACKS,
        }
    }
}

impl Since for Item {
    fn since(&self) -> u32 {
        match self {
            Self::Words(_) | Self::Thought(_) | Self::Tool(_) => EARLIER,
            Self::Other(_) => FALLBACKS,
        }
    }
}

impl Since for ToolContent {
    fn since(&self) -> u32 {
        match self {
            Self::Text(_) | Self::Diff(_) | Self::Output(_) => EARLIER,
            Self::Other(_) => FALLBACKS,
        }
    }
}

impl Since for ChatPatch {
    fn since(&self) -> u32 {
        match self {
            Self::Turn { .. }
            | Self::Item { .. }
            | Self::Append { .. }
            | Self::Output { .. }
            | Self::State(_)
            | Self::Question(_)
            | Self::Notice(_)
            | Self::Settings(_)
            | Self::Usage(_)
            | Self::Files { .. }
            | Self::Agent(_)
            | Self::Providers(_)
            | Self::Subagents(_) => EARLIER,
        }
    }
}

impl Since for BufferUpdate {
    fn since(&self) -> u32 {
        match self {
            Self::Snapshot(_)
            | Self::Resumed { .. }
            | Self::Acked { .. }
            | Self::Edited { .. }
            | Self::NoChange(_)
            | Self::Modified(_)
            | Self::Highlights { .. }
            | Self::Diagnostics { .. }
            | Self::Offer { .. }
            | Self::Answer { .. }
            | Self::Hunks { .. }
            | Self::Features(_)
            | Self::Moved(_)
            | Self::Notice(_)
            | Self::ChangedOnDisk
            | Self::Agents { .. } => EARLIER,
        }
    }
}

impl Since for FilesUpdate {
    fn since(&self) -> u32 {
        match self {
            Self::Root { .. }
            | Self::Listed { .. }
            | Self::Unlisted { .. }
            | Self::Git(_)
            | Self::NoGit(_) => EARLIER,
        }
    }
}

impl Since for SettingKind {
    fn since(&self) -> u32 {
        match self {
            Self::Mode | Self::Model | Self::Effort | Self::Tools | Self::Other | Self::Browser => {
                EARLIER
            }
        }
    }
}

impl Since for Event {
    fn since(&self) -> u32 {
        match self {
            Self::View { .. }
            | Self::Workspaces(_)
            | Self::Buffer { .. }
            | Self::Installed { .. }
            | Self::Found { .. }
            | Self::Clipboard { .. }
            | Self::ClipboardAsked { .. }
            | Self::Files { .. }
            | Self::FileOp { .. }
            | Self::Show { .. }
            | Self::Board { .. }
            | Self::Review { .. }
            | Self::Browser { .. }
            | Self::Chats(_)
            | Self::Pairing(_)
            | Self::Sessions { .. } => EARLIER,
        }
    }
}

impl Since for Error {
    fn since(&self) -> u32 {
        match self {
            Self::HelloRequired
            | Self::NoSuchTerminal(_)
            | Self::TerminalExited(_)
            | Self::InputBacklogFull(_)
            | Self::SpawnFailed(_)
            | Self::RecordingFailed { .. }
            | Self::NoSuchPane(_)
            | Self::NoSuchWorkspace(_)
            | Self::NoSuchTab(_)
            | Self::NoSuchSplit(_)
            | Self::EmptyShare
            | Self::IsAChat(_)
            | Self::NotAChat(_)
            | Self::TerminalInPane(_)
            | Self::NameTaken(_)
            | Self::BadName { .. }
            | Self::CantCarry(_)
            | Self::NoSuchBuffer(_)
            | Self::OpenFailed { .. }
            | Self::SaveFailed { .. }
            | Self::ChangedOnDisk(_)
            | Self::NotAFile(_)
            | Self::NoSuchFiles(_)
            | Self::NoTools(_)
            | Self::NoSuchBoard(_)
            | Self::Boards(_)
            | Self::NoSuchReview(_)
            | Self::Review(_)
            | Self::NoSuchDevice(_)
            | Self::Pairing(_)
            | Self::SessionOpen(_) => EARLIER,
            Self::Unsupported | Self::Other(_) => FALLBACKS,
        }
    }
}

/// The newest protocol version any variant in `message` arrived in, of
/// those [`Since`] tags, and so the oldest peer that can be sent it as it
/// is.
#[must_use]
pub fn newest(message: &ServerMessage) -> u32 {
    match message {
        ServerMessage::Hello { .. } | ServerMessage::NotUpgrading { .. } => EARLIER,
        ServerMessage::Response(response) => match response {
            Response::Error(err) => err.since(),
            Response::Workspaces(workspaces) => newest_in_workspaces(workspaces),
            Response::View(view) => newest_in_node(&view.root),
            _ => EARLIER,
        },
        ServerMessage::Event(event) => {
            let inside = match event {
                Event::View { update, .. } => match update {
                    ViewUpdate::Snapshot(view) => newest_in_node(&view.root),
                    ViewUpdate::Patch(patches) => (patches.iter())
                        .map(|patch| match patch {
                            Patch::Chat(patch) => newest_in_patch(patch),
                            Patch::Title(_) | Patch::Cwd(_) | Patch::Grid { .. } => EARLIER,
                        })
                        .max()
                        .unwrap_or(EARLIER),
                },
                Event::Workspaces(workspaces) => newest_in_workspaces(workspaces),
                Event::Buffer { update, .. } => update.since(),
                Event::Files { update, .. } => update.since(),
                _ => EARLIER,
            };
            event.since().max(inside)
        }
    }
}

fn newest_in_workspaces(workspaces: &Workspaces) -> u32 {
    (workspaces.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .map(|pane| pane.content.since())
        .max()
        .unwrap_or(EARLIER)
}

fn newest_in_node(node: &Node) -> u32 {
    match node {
        Node::Chat(chat) => (chat.turns.iter())
            .map(newest_in_turn)
            .chain([newest_in_settings(&chat.settings)])
            .max()
            .unwrap_or(EARLIER),
        Node::Grid(_) | Node::Other => node.since(),
    }
}

fn newest_in_settings(settings: &[Setting]) -> u32 {
    (settings.iter())
        .map(|setting| setting.kind.since())
        .max()
        .unwrap_or(EARLIER)
}

fn newest_in_turn(turn: &Turn) -> u32 {
    turn.items
        .iter()
        .map(newest_in_item)
        .max()
        .unwrap_or(EARLIER)
}

fn newest_in_item(item: &Item) -> u32 {
    match item {
        Item::Tool(tool) => (tool.content.iter())
            .map(Since::since)
            .max()
            .unwrap_or(EARLIER),
        Item::Words(_) | Item::Thought(_) | Item::Other(_) => item.since(),
    }
}

fn newest_in_patch(patch: &ChatPatch) -> u32 {
    let inside = match patch {
        ChatPatch::Turn { turn, .. } => newest_in_turn(turn),
        ChatPatch::Item { value, .. } => newest_in_item(value),
        ChatPatch::Settings(settings) => newest_in_settings(settings),
        _ => EARLIER,
    };
    patch.since().max(inside)
}

/// What a message becomes for a peer older than this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Downgraded<T> {
    /// It can read the message as it is.
    AsIs,
    /// It reads this in its place.
    To(T),
    /// It's sent nothing: what it would say is only for a newer peer.
    Dropped,
    /// It can't be sent anything in its place, and it must be: an answer
    /// to a request. Encoding it fails, which drops the client, and it
    /// connects again into the same refusal: only for a peer from before
    /// the fallbacks, which never asks for what's newer.
    Refused,
}

/// What a peer older than this build can read in place of a message that
/// may hold something newer than it.
///
/// [`frame::encode`](crate::frame::encode) asks, for a connection at a
/// version older than [`PROTOCOL_VERSION`], before encoding: so what's
/// sent to a peer of this build's own version never pays for it.
pub trait Downgrade: Sized {
    /// The message as a peer speaking `wire` can read it, never holding a
    /// variant newer than that.
    fn downgrade(&self, wire: u32) -> Downgraded<Self> {
        let _ = wire;
        Downgraded::AsIs
    }
}

/// What clients send needs no downgrading: a window asks [`daemon_has`]
/// before sending anything newer.
impl Downgrade for ClientMessage {}

impl Downgrade for ServerMessage {
    /// A peer from before the fallbacks has nothing to read in place of
    /// what's newer: an event holding it is dropped, and an answer refused.
    /// A later peer gets the fallbacks: as the daemon's state grows a
    /// variant, its arm goes here, for `wire` older than the variant. A
    /// pane of a new kind becomes `Content::Other`, naming it; a new node,
    /// chat item or tool content becomes its `Other`; a new chat patch
    /// becomes a fresh snapshot of the chat; something only informative,
    /// such as a new buffer update, is dropped; a new error becomes
    /// `Error::Other(err.to_string())`. None is newer than the fallbacks
    /// yet.
    fn downgrade(&self, wire: u32) -> Downgraded<Self> {
        if newest(self) <= wire {
            return Downgraded::AsIs;
        }
        match self {
            Self::Response(_) => Downgraded::Refused,
            _ if wire < FALLBACKS => Downgraded::Dropped,
            // What's newer than the fallbacks has no arm yet, so nothing is.
            _ => Downgraded::Refused,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::*;
    use crate::frame::{self, FrameDecoder};

    /// The test messages gained what they gained in this build's version,
    /// as a change made in it does: a peer a version older hasn't it.
    const NEW: u32 = PROTOCOL_VERSION;
    const OLD: u32 = PROTOCOL_VERSION - 1;

    /// A message as this build has it.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Before {
        items: Vec<Thing>,
        shape: OldShape,
        last: u8,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Thing {
        name: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum OldShape {
        Round,
        Other,
    }

    /// The same message a version on: with a field gated at it, at its
    /// end and in a struct inside a list, and a variant new then.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct After {
        items: Vec<NewThing>,
        shape: Shape,
        last: u8,
        #[serde(
            skip_serializing_if = "before::<{ self::NEW }, _>",
            deserialize_with = "since::<{ self::NEW }, _, _>"
        )]
        note: Option<String>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct NewThing {
        name: String,
        #[serde(
            skip_serializing_if = "before::<NEW, _>",
            deserialize_with = "since::<NEW, _, _>"
        )]
        size: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Shape {
        Round,
        Other,
        /// New at `NEW`: older peers get `Other`.
        Square,
    }

    impl Downgrade for After {
        fn downgrade(&self, wire: u32) -> Downgraded<Self> {
            if wire < NEW && self.shape == Shape::Square {
                Downgraded::To(Self {
                    shape: Shape::Other,
                    ..self.clone()
                })
            } else {
                Downgraded::AsIs
            }
        }
    }

    impl Downgrade for Before {}

    fn after(shape: Shape) -> After {
        After {
            items: vec![
                NewThing {
                    name: "a".into(),
                    size: 1,
                },
                NewThing {
                    name: "b".into(),
                    size: 300,
                },
            ],
            shape,
            last: 9,
            note: Some("new".into()),
        }
    }

    fn old(shape: OldShape) -> Before {
        Before {
            items: vec![Thing { name: "a".into() }, Thing { name: "b".into() }],
            shape,
            last: 9,
        }
    }

    fn decode<T: serde::de::DeserializeOwned>(wire: u32, bytes: &[u8]) -> T {
        let mut decoder = FrameDecoder::new();
        decoder.push(bytes);
        decoder.decode(wire).unwrap().unwrap()
    }

    #[test]
    fn gated_fields_are_left_out_for_an_older_peer_byte_for_byte() {
        let sent = frame::encode(OLD, &after(Shape::Round)).unwrap();
        let old_bytes = frame::encode(OLD, &old(OldShape::Round)).unwrap();
        assert_eq!(sent, old_bytes);
        assert_eq!(decode::<Before>(OLD, &sent), old(OldShape::Round));
    }

    #[test]
    fn gated_fields_from_an_older_peer_take_their_defaults() {
        let sent = frame::encode(OLD, &old(OldShape::Round)).unwrap();
        let read: After = decode(OLD, &sent);
        let mut expected = after(Shape::Round);
        expected.note = None;
        expected.items[0].size = 0;
        expected.items[1].size = 0;
        assert_eq!(read, expected);
    }

    #[test]
    fn gated_fields_go_both_ways_between_peers_that_have_them() {
        let sent = frame::encode(NEW, &after(Shape::Square)).unwrap();
        let older = frame::encode(OLD, &after(Shape::Square)).unwrap();
        assert_ne!(sent, older);
        assert_eq!(decode::<After>(NEW, &sent), after(Shape::Square));
    }

    #[test]
    fn a_variant_newer_than_the_peer_is_downgraded_before_it_goes() {
        let sent = frame::encode(OLD, &after(Shape::Square)).unwrap();
        let old_bytes = frame::encode(OLD, &old(OldShape::Other)).unwrap();
        assert_eq!(sent, old_bytes);
        // A peer that has the variant gets it.
        let sent = frame::encode(NEW, &after(Shape::Square)).unwrap();
        assert_eq!(decode::<After>(NEW, &sent).shape, Shape::Square);
    }

    #[test]
    fn the_version_is_put_back_after_encoding_and_decoding() {
        assert_eq!(current(), PROTOCOL_VERSION);
        let bytes = frame::encode(MIN_CLIENT, &after(Shape::Round)).unwrap();
        assert_eq!(current(), PROTOCOL_VERSION);
        let _: Before = decode(MIN_CLIENT, &bytes);
        assert_eq!(current(), PROTOCOL_VERSION);
        let caught = std::panic::catch_unwind(|| speaking(1, || panic!("in the middle")));
        assert!(caught.is_err());
        assert_eq!(current(), PROTOCOL_VERSION);
    }

    /// The messages a newer daemon may send a peer that has the fallbacks
    /// nothing in place of, as they're only informative: each added here
    /// on purpose, beside the downgrade arm that drops it.
    fn droppable(message: &ServerMessage) -> bool {
        let _ = message;
        false
    }

    #[test]
    fn nothing_newer_than_a_peer_reaches_it() {
        use crate::corpus::{Sample, corpus};

        let mut withheld = 0;
        for wire in OLDEST..PROTOCOL_VERSION {
            for sample in corpus(PROTOCOL_VERSION) {
                let Sample::Server(message) = sample else {
                    continue;
                };
                let newer = newest(&message) > wire;
                match message.downgrade(wire) {
                    Downgraded::AsIs => assert!(!newer, "{message:?} goes to {wire} as it is"),
                    // Before the fallbacks there's no Other to read.
                    Downgraded::To(older) if wire < FALLBACKS => {
                        panic!("{message:?} became {older:?} for {wire}");
                    }
                    Downgraded::To(older) => {
                        assert!(newer, "{message:?} is changed for nothing");
                        assert!(
                            newest(&older) <= wire,
                            "{older:?} is still newer than {wire}"
                        );
                    }
                    Downgraded::Dropped | Downgraded::Refused if wire < FALLBACKS => {
                        assert!(newer, "{message:?} is withheld from {wire} for nothing");
                        withheld += 1;
                    }
                    // From the fallbacks on, an answer is never refused,
                    // and only what's listed is dropped.
                    Downgraded::Dropped => assert!(
                        newer && droppable(&message),
                        "{message:?} is dropped for {wire}: give it a downgrade arm, or list it \
                         as droppable"
                    ),
                    Downgraded::Refused => panic!(
                        "{message:?} is refused to {wire}, which drops it: give it a downgrade arm"
                    ),
                }
                // And what's encoded for it, read back, is as old as it is.
                match frame::encode(wire, &message) {
                    Ok(bytes) if bytes.is_empty() => assert!(newer),
                    Ok(bytes) => {
                        let sent: ServerMessage = decode(wire, &bytes);
                        assert!(newest(&sent) <= wire, "{sent:?}");
                    }
                    Err(err) => {
                        assert!(newer && matches!(err, crate::frame::FrameError::TooNew(_)));
                    }
                }
            }
        }
        assert!(withheld >= 5, "the corpus samples the fallbacks");
    }

    #[test]
    fn each_fixture_lists_the_gates_its_release_had() {
        // A branch that also took a version, merged as if it hadn't, would
        // have its gates at a version a release already had without them.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("the fixtures are committed") {
            let path = entry.unwrap().path();
            let Some(version) = (path.file_name().and_then(|name| name.to_str()))
                .and_then(|name| name.strip_prefix("protocol-v")?.strip_suffix(".bin"))
                .and_then(|version| version.parse::<u32>().ok())
            else {
                continue;
            };
            let listed = std::fs::read_to_string(path.with_extension("gates"))
                .unwrap_or_else(|_| panic!("protocol-v{version}.gates is missing"));
            for &(name, gate) in GATES.iter().filter(|&&(_, gate)| gate <= version) {
                assert!(
                    listed.lines().any(|line| line == format!("{name} {gate}")),
                    "{name} is at {gate}, but protocol {version}'s release didn't have it: \
                     renumber it, and what it gates, past {version}"
                );
            }
            checked += 1;
        }
        assert!(checked > 0);
    }

    #[test]
    fn every_variant_says_when_it_arrived() {
        assert_eq!(Node::Other.since(), FALLBACKS);
        assert_eq!(Error::Unsupported.since(), FALLBACKS);
        assert_eq!(Error::SessionOpen(crate::TerminalId(1)).since(), EARLIER);
        assert_eq!(Item::Other(String::new()).since(), FALLBACKS);
        assert_eq!(ToolContent::Other(String::new()).since(), FALLBACKS);
        let other = Content::Other {
            kind: crate::PaneKind::new("x"),
        };
        assert_eq!(other.since(), FALLBACKS);
        assert_eq!(BufferUpdate::ChangedOnDisk.since(), EARLIER);
        assert_eq!(FilesUpdate::NoGit(String::new()).since(), EARLIER);
        assert_eq!(SettingKind::Browser.since(), EARLIER);
    }

    #[test]
    fn a_daemon_runs_a_connection_at_the_older_version_or_refuses() {
        assert_eq!(for_client(PROTOCOL_VERSION), Some(PROTOCOL_VERSION));
        assert_eq!(for_client(MIN_CLIENT), Some(MIN_CLIENT));
        assert_eq!(for_client(MIN_CLIENT - 1), None);
        // A newer client runs at this build's version.
        assert_eq!(for_client(PROTOCOL_VERSION + 5), Some(PROTOCOL_VERSION));
        // Asking with a version past every daemon's gets its own back.
        assert_eq!(for_client(u32::MAX), Some(PROTOCOL_VERSION));
    }

    #[test]
    fn a_client_takes_only_a_reply_it_can_talk_at() {
        assert!(takes_reply(PROTOCOL_VERSION));
        assert!(takes_reply(MIN_DAEMON));
        // A newer daemon's own version: it refused this client.
        assert!(!takes_reply(PROTOCOL_VERSION + 1));
        // A daemon older than the fallbacks: its own version, and so
        // perhaps a refusal, is never taken for agreement.
        for reply in 0..FALLBACKS {
            assert!(!takes_reply(reply), "{reply}");
        }
    }

    #[test]
    fn features_are_offered_only_by_daemons_that_have_them() {
        assert!(daemon_has(PROTOCOL_VERSION, FALLBACKS));
        assert!(daemon_has(FALLBACKS, FALLBACKS));
        assert!(!daemon_has(FALLBACKS - 1, FALLBACKS));
    }

    #[test]
    fn the_fallbacks_are_this_builds_version_until_a_release_has_them() {
        // A daemon from before them refuses a client by answering with its
        // own version, which a client takes for agreement if FALLBACKS is
        // below the version they came in: so until a release with them is
        // made, writing its fixture, they're this build's own version, as
        // a rebase onto other protocol changes must keep them.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let released = dir.join(crate::corpus::fixture_name(FALLBACKS)).exists();
        assert!(
            released || FALLBACKS == PROTOCOL_VERSION,
            "FALLBACKS is {FALLBACKS}, but no release has had them yet: it must be \
             PROTOCOL_VERSION ({PROTOCOL_VERSION}), and MIN_DAEMON with it"
        );
    }

    #[test]
    fn no_gate_is_newer_than_the_protocol() {
        for &(name, version) in GATES {
            assert!(version <= PROTOCOL_VERSION, "{name} is {version}");
        }
    }

    #[test]
    fn gates_and_fixtures_every_peer_has_outgrown_are_deleted() {
        // Every peer this build talks to is OLDEST or newer: a gate at or
        // below it is always open, and a fixture below it pins bytes no
        // peer reads. `scripts/release` moves the floors; this says what
        // to delete then.
        // Bar FALLBACKS: the Other variants are for good. Its checks are
        // only always true then, and can go or stay.
        let gates: Vec<&str> = (GATES.iter())
            .filter(|&&(name, version)| version <= OLDEST && name != "FALLBACKS")
            .map(|&(name, _)| name)
            .collect();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let fixtures: Vec<String> = std::fs::read_dir(dir)
            .expect("the fixtures are committed")
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let version: u32 = (name.strip_prefix("protocol-v")?)
                    .strip_suffix(".bin")?
                    .parse()
                    .ok()?;
                (version < OLDEST).then_some(name)
            })
            .collect();
        assert!(
            gates.is_empty() && fixtures.is_empty(),
            "every peer is protocol {OLDEST} or newer now: delete the gates {gates:?} \
             (and what they gate), and the fixtures {fixtures:?}"
        );
    }
}
