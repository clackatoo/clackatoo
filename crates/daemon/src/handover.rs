//! What one build of the daemon hands the next when it upgrades in place
//! (see [`upgrade`](crate::upgrade)): its terminals, chats, workspaces and
//! listening socket.
//!
//! The state is a file format, written by one build and read by a newer
//! one, so the types here are frozen, like [`clackatoo_term::Snapshot`]:
//! they don't follow the protocol's types, which change with every protocol
//! version. A change that needs the format to change makes a new version
//! alongside this one, and newer builds keep reading this one.
//!
//! File descriptors travel separately (they can't be written down), and
//! the state refers to each by its index in [`Fds`].
//!
//! Versions:
//! 1. The first.
//! 2. Each terminal's shell blocks ([`clackatoo_term::BlocksSnapshot`]).
//! 3. Chats with agents, as the screens they were written into.
//! 4. Chats as their turns.
//! 5. Chats' settings, and the values the user chose for them.
//! 6. What sort of tool each of a chat's tools is, and what it shows: a
//!    command's output, a change to a file.
//! 7. Where in files each tool works: their paths, and lines.
//! 8. Buffers: files being edited, with their history, and panes showing
//!    them.
//! 9. Each terminal's links ([`clackatoo_term::LinksSnapshot`]).
//! 10. File explorers, and panes showing them.
//! 11. Which agent each chat talks to.
//! 12. The edit log: what agents changed in files, by chat and turn, so
//!     that turns can still be undone.
//! 13. Where each chat stands among the chats agents talk in, and panes
//!     showing boards.
//! 14. The last change each window sent each buffer, so a window that lost
//!     its connection as the daemon upgraded knows which arrived.
//! 15. What agents changed and the user hasn't reviewed: each agent hunk,
//!     open or not, the comments waiting to go to chats, and reviews, and
//!     panes showing them.
//! 16. Chats that move between agents: the sessions each had with other
//!     agents, where it moved, how much of it each session has seen, and
//!     the agent's plan.
//! 17. Panes showing a chat's page in clackatoo's browser. The browser
//!     itself isn't handed over: it stops, and starts again with the next
//!     page an agent opens.
//! 18. Panes showing a folder in an editor, as its listing. The folder is
//!     an explorer's root, handed over with the other explorers.
//! 19. The lock on the recovery files, so no other daemon takes them
//!     while the next build starts.
//! 20. Each terminal's shell's secret (see `crate::shell`), the file it's
//!     read from if the shell hasn't yet, and which block's command the
//!     shell's integration marked with it.
//! 21. Which machine the daemon is, by its ID, and its connections to the
//!     daemons on other machines, which the next build makes again.
//! 22. Panes showing what's on another machine, by that machine and the
//!     number this daemon gave the pane there, and the workspaces this
//!     daemon lent another, holding a pane of its own for that daemon to
//!     show.
//! 23. Whether this daemon put such a pane in that daemon's layout itself,
//!     as that daemon can't reach this one, and what the machines this
//!     daemon's borrowed panes are on are called.
//! 24. Which connections the daemon doesn't pass on to the daemons
//!     connected to it, and the machine each was last connected to; the
//!     machine's identity key; and the keys it knows other machines by.
//! 25. Background tabs: the tabs each workspace keeps running out of its
//!     tab bar, and the failure of each no window has said was seen.
//! 26. The terminals the user marked to keep running, and the command
//!     each keeps running.
//! 27. What panes made again as the daemon last started afresh still have
//!     to show for it: the command each terminal was running, still
//!     offered to run again, and where each editor was. Before, an upgrade
//!     lost every offer still open.
//! 28. Bounded image thumbnails retained in chat turns.
//! 29. Bounded thumbnails returned by tools, kept alongside their tool IDs.
//! 30. Retained original image data and references from prompt/tool thumbnails.
//! 31. Pending user messages, restored paused after an upgrade.
//! 32. Explicit continuations for turns interrupted by a planned upgrade.
//! 33. Managed agent activities, as a versioned journal.
//! 34. What buffers' undo steps keep, as Vim's undo header does: where the
//!     cursor was and the last selection, for each step; which steps
//!     change nothing; and whether older steps were let go.
//! 35. Workspaces lent as background tabs of the daemons they're lent to.
//! 36. Why messages chats sent others, which waited their turns, were
//!     dropped.
//! 37. Where chats stand among the others (CLACKA-187): what each runs in
//!     the background, the turns agents took by themselves, which answers
//!     each chat has had from each other, the counts agents' work is held
//!     to, and the waits for chats the upgrade cut off. Before, all of it
//!     was lost.
//! 38. The agents' processes that go on through the upgrade (CLACKA-188):
//!     each adapter's pipes and pidfd, the requests it hasn't answered,
//!     what it said it can do, and what's half read of it; each of its
//!     chats' open turn, link and questions; the connections its tools are
//!     served on, and the calls on them still waiting. Before, every agent
//!     was stopped, and all it ran with it.
//! 39. Which items of chats' turns an agent's own subagents did, and the
//!     tool call that started each (CLACKA-195). Before, a subagent's work
//!     came back as the agent's own.
//! 45. Version 44's state, said by a build that asks before a folder's
//!     language servers run (CLACKA-224): one handing over an older
//!     version ran them unasked, so the next build trusts the folders it
//!     ran them for.
//! 46. What programs asked of each terminal beyond its modes
//!     (CLACKA-234): the kitty keyboard protocol's flags, the titles
//!     pushed, and how far a task has got. Before, an upgrade dropped
//!     them, and keys went in xterm's encoding until a program asked
//!     again.
//! 47. What a tab's layout keeps beside its splits (CLACKA-240): the
//!     tabs zoomed to one of their panes, and the panes and tabs the user
//!     closed, to open again. Before, an upgrade unzoomed every tab and
//!     forgot what was closed.
//! 48. What notices from clackatoo said of the chats that finished
//!     (CLACKA-244), with the turns they started and the messages waiting
//!     to. Before, an upgrade showed each such turn as the words its agent
//!     was told, rather than the chats' cards.
//! 49. The image layer's store (CLACKA-239): each image's owners and
//!     what it is, its bytes in a file of its own in the runtime
//!     directory; which image each chat's thumbnail handles name, whose
//!     originals the chat kept itself before; and picture panes, with
//!     their files. Before, chats' originals came in the handover itself.
//! 50. What's kept of each terminal's history across a restart beyond its
//!     lines (CLACKA-242): whether it's kept at all, the blocks kept
//!     though it isn't, where `:forget` left off, the lines' hints of how
//!     private they are, and where history kept from before the last
//!     restart ends. Before, all of it was kept, no line had hints, and
//!     none came from before a restart.
//! 51. What the user marks a terminal with beyond keeping it (CLACKA-238):
//!     that its notifications reach the phone. Before, there was nothing
//!     else to mark.
//! 52. The ports forwarded to other machines through the daemon's
//!     connections to them, with their listening sockets (CLACKA-118).
//!     Before, they were forgotten, and refused connections until asked
//!     for again.
//! 53. Terminals' images (CLACKA-239, part 2): the pictures their
//!     programs showed, on their lines and in their history, the images
//!     kitty's commands keep by ID, and the virtual placements kitty's
//!     placeholders show, the images' bytes in the store's files. Before,
//!     terminals showed none.

pub(crate) mod relay;

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::OwnedFd;

use clackatoo_term::{AskedSnapshot, BlocksSnapshot, LinksSnapshot, Snapshot};
use serde::{Deserialize, Serialize};

/// The start of every handover, to tell it from anything else.
const MAGIC: &[u8] = b"clackatoo handover\n";

/// The version written.
pub(crate) const VERSION: u32 = 53;

/// The first version written by a build that asks before a folder's
/// language servers run (CLACKA-224): a build handing over an older one
/// ran them unasked.
pub(crate) const ASKS_TRUST: u32 = 45;

/// The versions this build reads.
pub(crate) const READABLE: &[u32] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
    51, 52, VERSION,
];

/// Everything one build hands the next.
#[derive(Debug)]
pub(crate) struct Handover {
    pub(crate) state: State,
    pub(crate) fds: Fds,
    /// The version the state was written in, which says what the build
    /// handing it over did.
    pub(crate) version: u32,
}

/// The file descriptors a handover's state refers to, by index.
#[derive(Debug, Default)]
pub(crate) struct Fds(Vec<Option<OwnedFd>>);

impl Fds {
    /// Adds `fd`, returning the index the state refers to it by.
    pub(crate) fn add(&mut self, fd: OwnedFd) -> u32 {
        self.0.push(Some(fd));
        u32::try_from(self.0.len() - 1).expect("far fewer than 4 billion fds")
    }

    /// Takes the fd at `index`. Each can be taken once.
    pub(crate) fn take(&mut self, index: u32) -> Result<OwnedFd, HandoverError> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.0.get_mut(index))
            .and_then(Option::take)
            .ok_or(HandoverError::Damaged("an fd is missing or used twice"))
    }

    /// Every fd, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &OwnedFd> {
        self.0.iter().flatten()
    }

    pub(crate) fn from_received(fds: Vec<OwnedFd>) -> Self {
        Self(fds.into_iter().map(Some).collect())
    }
}

/// The daemon's state, version 32.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct State {
    /// The listening socket.
    pub(crate) listener: u32,
    /// The runtime directory's lock, set by whoever holds it.
    pub(crate) lock: Option<u32>,
    /// The ID the next terminal gets.
    pub(crate) next_terminal: u32,
    pub(crate) terminals: Vec<Terminal>,
    /// Chats with agents, which are terminals too: their IDs are
    /// terminals' IDs.
    pub(crate) chats: Vec<Chat>,
    /// Closed terminals' programs that haven't exited yet, and agents that
    /// were stopped.
    pub(crate) orphans: Vec<Process>,
    pub(crate) workspaces: Workspaces,
    /// The ID the next buffer gets.
    pub(crate) next_buffer: u32,
    /// Files being edited.
    pub(crate) buffers: Vec<Buffer>,
    /// The ID the next file explorer gets.
    pub(crate) next_files: u32,
    /// File explorers.
    pub(crate) explorers: Vec<Files>,
    /// What agents changed in files.
    pub(crate) edits: EditLog,
    /// The last change each window sent each buffer. Since version 14.
    pub(crate) applied: Vec<Applied>,
    /// The daemon's own number, chosen at random when it first started:
    /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
    /// Since version 14; 0 before, for the build reading it to choose one.
    pub(crate) daemon: u64,
    /// What agents changed that the user hasn't reviewed, and reviews.
    pub(crate) reviews: Reviews,
    /// The lock on the recovery files, if the daemon keeps them (see
    /// `crate::recovery`). Since version 19.
    pub(crate) recovery_lock: Option<u32>,
    /// The secrets of the terminals' shells that have one. Since version
    /// 20; before, no shell had one.
    pub(crate) shells: Vec<Shell>,
    /// Which machine the daemon is: its ID (see
    /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
    /// `None` before, for the build reading it to take the one its state
    /// directory keeps.
    pub(crate) machine: Option<[u8; 16]>,
    /// The daemon's connections to the daemons on other machines. Since
    /// version 21; before, there were none.
    pub(crate) connections: Vec<Connection>,
    /// The workspaces lent to the daemons on other machines. Since version
    /// 22; before, there were none.
    pub(crate) lent: Vec<Lent>,
    /// The machines they're lent to. Since version 22.
    pub(crate) borrowers: Vec<Borrower>,
    /// What the machines the panes borrowed are on are called. Since
    /// version 23; before, none were known.
    pub(crate) lenders: Vec<Lender>,
    /// Which of the connections aren't passed on, and the machine each
    /// was last connected to, by host. Since version 24; before, every
    /// one was passed on, and none's machine was known.
    pub(crate) sharing: Vec<Sharing>,
    /// The seed of the machine's identity key (see `crate::machine`).
    /// Since version 24; before, the next build reads it from the
    /// `machine` file, or chooses one.
    pub(crate) key: Option<[u8; 32]>,
    /// The identity keys of other machines the daemon knows. Since version
    /// 24; before, none were known.
    pub(crate) keys: Vec<Known>,
    /// The workspaces' background tabs. Since version 25; before, there
    /// were none.
    pub(crate) background: Vec<Background>,
    /// The terminals the user marked to keep running. Since version 26;
    /// before, none was. How running each again was going isn't handed
    /// over: it starts again.
    pub(crate) kept: Vec<Kept>,
    /// Each terminal's blocks whose commands its shell's integration said,
    /// with its secret, so the command a shell ran last is still known to
    /// be its own: [`Shell::trusted`] says only the latest block's. Since
    /// version 26; before, only that one was.
    pub(crate) trusted: Vec<Trusted>,
    /// The program each terminal was started as, so one its shell has
    /// become another program in, as with `exec`, is still known to have
    /// no shell to type into. Since version 26; before, none was known.
    pub(crate) exes: Vec<Exe>,
    /// What panes made again as the daemon last started afresh still have
    /// to show for it (see
    /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
    /// Since version 27: `None` from a build before, which couldn't hand
    /// them over, as distinct from one that had none to.
    pub(crate) restored: Option<Vec<Restored>>,
    /// Small image previews retained by chat and turn, since version 28.
    pub(crate) chat_previews: Vec<ChatPreviews>,
    /// Thumbnails attached to tools, since version 29.
    pub(crate) tool_previews: Vec<ToolPreviews>,
    /// Compressed originals and thumbnail handles, since version 30.
    pub(crate) image_originals: Vec<ImageOriginals>,
    /// Pending drafts, resumed paused unless continuing a turn, since version 31.
    pub(crate) chat_queues: Vec<ChatQueue>,
    /// Only work interrupted by this upgrade, since version 32.
    pub(crate) chat_continuations: Vec<ChatContinuation>,
    /// Versioned managed-operation records, since version 33.
    pub(crate) activities: Vec<u8>,
    /// What the buffers' undo steps keep, since version 34; before, they
    /// kept nothing, and none changed nothing.
    pub(crate) undo_states: Vec<BufferUndo>,
    /// The workspaces lent as background tabs of the daemons they're lent
    /// to, and the failure of each not seen there yet. Since version 35;
    /// before, none was.
    pub(crate) lent_background: Vec<LentBackground>,
    /// Messages chats sent others that waited their turns and were
    /// dropped as they came, so each sender is still told why. Since
    /// version 36; before, none was.
    pub(crate) undelivered: Vec<Undelivered>,
    /// Where chats stand among the others: see [`Talk`]. Since version
    /// 37; `None` before, from a build that couldn't hand it over.
    pub(crate) talk: Option<Talk>,
    /// The agents' processes that go on through the upgrade, and what
    /// goes on with them: see [`Live`]. Since version 38; `None` before,
    /// and from a build that stopped its agents, as it was told to, or as
    /// the build it upgraded to couldn't take them.
    pub(crate) live: Option<Live>,
    /// The items of chats' turns that an agent's own subagents did: see
    /// [`NestedItem`]. Since version 39; before, none was.
    pub(crate) nested: Vec<NestedItem>,
    /// The messages being typed in chats' boxes and not sent yet. Since
    /// version 40; before, none was kept.
    pub(crate) drafts: Vec<ChatDraft>,
    /// The numbers that stay with chats' turns, of the chats whose turns
    /// aren't each numbered by their index: see [`ChatUids`]. Since
    /// version 41; before, every turn was.
    pub(crate) uids: Vec<ChatUids>,
    /// The branches of chats' conversations, of the chats that have been
    /// cut back: see [`ChatBranches`]. Since version 41.
    pub(crate) branches: Vec<ChatBranches>,
    /// Where chats' agent sessions can be forked: see [`ChatForks`].
    /// Since version 41; before, none was known.
    pub(crate) forks: Vec<ChatForks>,
    /// Command ownership and permanent selection protection, since 42.
    /// None from older builds: selection history is unknown, so retain all.
    pub(crate) agent_terminals: Option<Vec<AgentCommand>>,
    /// Federated views and edit receipts, since version 43.
    pub(crate) federation: Option<relay::Federation>,
    /// What the secrets given chats' MCP servers are checked against, and
    /// which went with which session, since version 44. None from older
    /// builds, which gave none.
    pub(crate) mcp: Option<Mcp>,
    /// What programs asked of the terminals that asked anything beyond
    /// their modes: see [`Asked`]. Since version 46; before, nothing was
    /// kept.
    pub(crate) asked: Vec<Asked>,
    /// What tabs' layouts keep beside their splits: see [`LayoutExtras`].
    /// Since version 47; `None` before.
    pub(crate) layout: Option<LayoutExtras>,
    /// What notices from clackatoo said of the chats that finished, of the
    /// chats that have any: see [`ChatNotices`]. Since version 48; before,
    /// none was kept.
    pub(crate) notices: Vec<ChatNotices>,
    /// The image layer's store (see [`Images`]), whose images' bytes are
    /// in files of their own. Since version 49; `None` before, from a
    /// build whose chats kept their images themselves.
    pub(crate) images: Option<Images>,
    /// What's kept of the terminals' history across a restart beyond
    /// their lines, of the terminals with anything to say: see
    /// [`History`]. Since version 50; before, every terminal's was kept,
    /// no line had hints, and none came from before a restart.
    pub(crate) histories: Vec<History>,
    /// What the user marked the terminals with, and what else of them
    /// isn't in a [`Terminal`]: see [`Marks`]. Since version 51; before,
    /// nothing was marked.
    pub(crate) marks: Vec<Marks>,
    /// The ports forwarded to other machines through the daemon's
    /// connections to them, listening still: see [`Forwarded`]. Since
    /// version 52; before, none were kept.
    pub(crate) forwards: Vec<Forwarded>,
    /// The connections, by host, whose ssh straight there was found to ask
    /// a question as a client asked for it, which aren't tried in the
    /// background (CLACKA-118). Since version 52; before, none.
    pub(crate) direct_asks: Vec<String>,
    /// The images of the terminals that show or keep any: see
    /// [`Graphics`]. Since version 53; before, none did.
    pub(crate) graphics: Vec<Graphics>,
}

/// The images terminal `terminal` shows and keeps, whose bytes are the
/// store's (see [`Images`]). Since version 53.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Graphics {
    pub(crate) terminal: u32,
    pub(crate) graphics: clackatoo_term::GraphicsSnapshot,
}

/// A port forwarded to another machine through the daemon's connection
/// to it, as `:portforward` in one of that machine's workspaces asked,
/// whose sockets go on listening through the upgrade, so a browser's
/// connections made meanwhile go through once the connection is made
/// again. Connections it had open are reset. Since version 52.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Forwarded {
    /// The connection's host.
    pub(crate) host: String,
    /// The port here.
    pub(crate) local: u16,
    /// The machine there, as the other machine finds it.
    pub(crate) to: String,
    /// The port there.
    pub(crate) port: u16,
    /// Its listening sockets, by their fds.
    pub(crate) listeners: Vec<u32>,
}

/// What terminal `terminal` was marked with, and the rules it lays its
/// cells out by. Since version 51, for every terminal; a terminal handed
/// over by an older build has no marks, and lays its cells out as before
/// ([`Widths::LEGACY`](clackatoo_term::Widths::LEGACY)), as its rows were.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Marks {
    pub(crate) terminal: u32,
    /// Its notifications reach the phone too.
    pub(crate) phone: bool,
    /// Variation selector 16 widens an emoji in it.
    pub(crate) vs16: bool,
    /// Its rows are rebuilt by grapheme clusters, and it can print by
    /// them (mode 2027).
    pub(crate) graphemes: bool,
    /// The left and right margins its program set, as the first column
    /// and the one past the last, if they narrow it (see
    /// `Terminal::margins`).
    pub(crate) margins: Option<(u16, u16)>,
}

impl Marks {
    /// The rules the terminal lays its cells out by.
    pub(crate) fn widths(&self) -> clackatoo_term::Widths {
        clackatoo_term::Widths {
            vs16: self.vs16,
            graphemes: self.graphemes,
        }
    }
}

/// What's kept of terminal `terminal`'s history across a restart beyond
/// its lines (see `server/history.rs`). Since version 50.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct History {
    pub(crate) terminal: u32,
    /// Its history isn't kept, but for the blocks the user kept, which
    /// are marked on their lines, with `hints`: the user marked it so.
    pub(crate) off: bool,
    /// The first line kept: those before it were forgotten.
    pub(crate) from: u64,
    /// How its lines were numbered as `from` was set: its history's
    /// generation and its width.
    pub(crate) numbered: (u64, u64),
    /// The lines with hints of how private they are, by number, with the
    /// hints' bits (see `clackatoo_term::Hints`).
    pub(crate) hints: Vec<(u64, u8)>,
    /// The line that says where history kept from before a restart ends,
    /// and when the restart was, in seconds since 1970.
    pub(crate) restarted: Option<(u64, i64)>,
}

/// The image layer's store (see `crate::images`). Since version 49.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Images {
    /// The folder the images' bytes are in, a file each, named by the
    /// image's ID in hex, as bytes: private to the user, in the runtime
    /// directory, and taken away once read.
    pub(crate) dir: Vec<u8>,
    /// The images kept.
    pub(crate) kept: Vec<KeptImage>,
    /// The images let go to keep within the limits, and the limit each
    /// was let go for, oldest first, so what showed them still says so.
    pub(crate) dropped: Vec<([u8; 16], u64)>,
    /// Which image each chat's thumbnail handles name.
    pub(crate) chats: Vec<ChatImages>,
    /// The picture panes.
    pub(crate) pictures: Vec<PictureShown>,
}

/// An image the store keeps (see [`Images`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct KeptImage {
    /// Its ID: the first 16 bytes of its bytes' SHA-256.
    pub(crate) id: [u8; 16],
    /// How its bytes are encoded: 0 PNG, 1 JPEG, 2 GIF, 3 WebP.
    pub(crate) format: u8,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// How many bytes it is.
    pub(crate) bytes: u64,
    /// What holds it.
    pub(crate) owners: Vec<ImageOwner>,
}

/// What holds an image (see `clackatoo_protocol::ImageFrom`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ImageOwner {
    /// A terminal, or a chat, by its ID.
    Terminal(u32),
    /// A picture pane, by its picture's ID.
    Picture(u32),
    /// A buffer, by its ID.
    Buffer(u32),
}

/// Which image in the store each of chat `chat`'s thumbnail handles
/// names (see `tool_images::Originals`). Since version 49.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ChatImages {
    pub(crate) chat: u32,
    pub(crate) handles: Vec<(u64, [u8; 16])>,
}

/// A picture pane's picture: its file, and the image it showed, if it was
/// read. Since version 49.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PictureShown {
    pub(crate) id: u32,
    /// The file, as bytes.
    pub(crate) path: Vec<u8>,
    pub(crate) image: Option<[u8; 16]>,
}

/// What the notices of the chat `chat` said of the chats that finished
/// (CLACKA-244), which neither its turns nor its messages keep: for each
/// turn a notice started, by its index and when it was sent, and for
/// each notice still waiting its turn, by its place among the chat's
/// messages, the chats it says finished, each as the JSON of the
/// protocol's `FinishedChat`: one a build can't read is let go, rather
/// than the whole handover. Since version 48.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ChatNotices {
    pub(crate) chat: u32,
    pub(crate) turns: Vec<(u32, u64, Vec<String>)>,
    pub(crate) inbox: Vec<(u32, Vec<String>)>,
}

/// What tabs' layouts keep beside their splits (CLACKA-240). Since
/// version 47.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct LayoutExtras {
    /// The tabs zoomed, each by its number and its pane's.
    pub(crate) zoomed: Vec<(u32, u32)>,
    /// The panes and tabs the user closed, to open again, the newest last,
    /// each as the JSON of `saved::Closed`: one a build can't read is let
    /// go, rather than the whole handover.
    pub(crate) closed: Vec<String>,
}

/// What the program in terminal `terminal` asked of it beyond its modes
/// (see [`AskedSnapshot`]). Since version 46.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Asked {
    pub(crate) terminal: u32,
    pub(crate) asked: AskedSnapshot,
}

/// What the secrets given chats' MCP servers are checked against, and
/// which went with which session (CLACKA-215).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Mcp {
    /// The run's own part of the key the secrets are signed with: new
    /// each time the daemon starts afresh, so none given before is good
    /// after, and handed on through upgrades, so all are.
    pub(crate) key: [u8; 32],
    /// The secrets each chat's sessions' servers were given, the newest
    /// last: by the chat's terminal's number, the session, once it opened,
    /// and the secret.
    pub(crate) sessions: Vec<(u32, Option<String>, String)>,
}

/// The branches of the conversation of the chat `chat`, by its terminal's
/// number (see `chat/branches.rs`): every segment of its tree, by its
/// place, `None` for one let go; the leaf of the branch shown; where a new
/// branch not yet written to starts; how many branches were let go where;
/// and how the session of a branch being made is to be had. Since version
/// 41.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatBranches {
    pub(crate) chat: u32,
    pub(crate) segs: Vec<Option<BranchSegment>>,
    pub(crate) current: u64,
    pub(crate) pending: Option<u64>,
    /// The segment they came off, where, and how many.
    pub(crate) dropped: Vec<(u64, u64, u16)>,
    pub(crate) plan: Option<ForkPlanRecord>,
    /// The sessions of branches let go, still kept off the start page.
    pub(crate) gone: Vec<String>,
    /// The uids of turns off the branch shown whose files the rewind or
    /// switch that left them undid, to redo as they're shown again.
    pub(crate) undone: Vec<u32>,
    /// The chats whose cards were on branches let go, by their terminals'
    /// numbers: their finishing isn't told to the branch shown.
    pub(crate) lost_cards: Vec<u32>,
}

/// A segment of a chat's conversation: the one it goes on from, the index
/// of its first turn, how many it has, and, off the branch shown, its
/// turns, or what's kept of them once let go; with the session that knows
/// the branch ending there, if it's a leaf.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BranchSegment {
    pub(crate) parent: Option<u64>,
    pub(crate) start: u64,
    pub(crate) len: u64,
    pub(crate) held: Option<HeldTurns>,
    pub(crate) stub: Option<StubTurns>,
    pub(crate) session: Option<BranchSessionRecord>,
    pub(crate) created: u64,
    pub(crate) used: u64,
    /// Its session was made from a digest of the turns before.
    pub(crate) summary: bool,
}

/// The turns of a segment off the branch shown, each with its uid, and the
/// subagent cards on them, each as the protocol's JSON: one this build
/// can't read is left out.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct HeldTurns {
    pub(crate) uids: Vec<u32>,
    pub(crate) turns: Vec<ChatTurn>,
    pub(crate) cards: Vec<String>,
}

/// What's kept of the turns of a segment off the branch shown once
/// they're let go: the first line of the first one's message, their uids
/// (none if they're to be numbered anew as they come back), and the
/// subagent cards on them, as [`HeldTurns`] has them.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StubTurns {
    pub(crate) first: String,
    /// Each turn's mark (`chat/branches.rs`), by which its turns told
    /// again are known.
    pub(crate) marks: Vec<u64>,
    pub(crate) uids: Vec<u32>,
    pub(crate) cards: Vec<String>,
}

/// The agent session that knows a branch.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BranchSessionRecord {
    pub(crate) agent: String,
    pub(crate) session: Option<String>,
    pub(crate) known: u64,
    pub(crate) chosen: Vec<(String, String)>,
}

/// How a branch's session being made is to be had: forked from `from` at
/// the agent's message `message`, knowing the first `known` turns; `sent`
/// if the agent was asked to fork and can.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ForkPlanRecord {
    pub(crate) from: String,
    pub(crate) message: Option<String>,
    pub(crate) known: u64,
    pub(crate) sent: bool,
}

/// Where the agent session of the chat `chat` can be forked: by turn uid,
/// the session and the agent's message. Since version 41.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatForks {
    pub(crate) chat: u32,
    pub(crate) points: Vec<(u32, String, String)>,
}

/// The uids of the turns of the chat `chat`, by its terminal's number,
/// one for each of its turns handed over, in order, and the uid its next
/// turn gets (see `chat/uids.rs`). Only for a chat whose turns aren't
/// each numbered by their index, as after its conversation was cut back;
/// records handed over keyed by turn, as the edit log and the reviews'
/// hunks are, name turns by these. Since version 41.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatUids {
    pub(crate) chat: u32,
    pub(crate) next: u32,
    pub(crate) uids: Vec<u32>,
}

/// The message being typed in the box of the chat `chat`, by its
/// terminal's number, and not sent yet (see [`State::drafts`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatDraft {
    pub(crate) chat: u32,
    pub(crate) text: String,
}

/// An item of a chat's turn that a subagent the agent started itself did,
/// as Claude Code's Task tool starts one, and the tool call that started
/// it: [`ChatItem`] holds nothing nested. Since version 39.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NestedItem {
    /// The chat, by its terminal's number.
    pub(crate) chat: u32,
    pub(crate) turn: u32,
    pub(crate) item: u32,
    /// The ID of the tool call that started the subagent.
    pub(crate) parent: String,
}

/// Where chats stand among the others, beyond what [`Chat`] keeps: since
/// version 37. Each chat is by its terminal's number.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Talk {
    /// What each chat's agent runs in the background.
    pub(crate) tasks: Vec<ChatTasks>,
    /// The turns agents took by themselves, with no message.
    pub(crate) woke: Vec<WokeTurn>,
    /// How many of each chat's turns another has had the answers to.
    pub(crate) heard: Vec<Heard>,
    /// How many chats agents have started under each of the user's chats.
    pub(crate) starts: Vec<Starts>,
    /// How many turns notices that chats finished have started under each
    /// of the user's chats, since which of the user's turns there.
    pub(crate) notice_turns: Vec<NoticeTurns>,
    /// The chats holding such a notice for the user, with the last turn
    /// the user started there as they began to.
    pub(crate) held: Vec<Held>,
    /// The waits for chats the upgrade cut off, by the chat that waited.
    pub(crate) waits: Vec<CutWait>,
    /// The chats whose user had asked them to stop as the upgrade came:
    /// they aren't told what the upgrade stopped.
    pub(crate) stopped: Vec<u32>,
}

/// What the chat `chat`'s agent runs in the background, in the session
/// `session` (see [`Talk::tasks`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatTasks {
    pub(crate) chat: u32,
    pub(crate) session: Option<String>,
    pub(crate) tasks: Vec<ChatTask>,
}

/// One of the things an agent runs in the background, as the chat shows
/// it (see `clackatoo_protocol::AgentTask`), its kind and state by name.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatTask {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) title: String,
    pub(crate) command: Option<String>,
    pub(crate) state: String,
    pub(crate) summary: Option<String>,
    pub(crate) started: u64,
    pub(crate) ended: Option<u64>,
    pub(crate) can_stop: bool,
}

/// Turn `turn` of the chat `chat`, which its agent took by itself, saying
/// `why` if it said (see [`Talk::woke`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WokeTurn {
    pub(crate) chat: u32,
    pub(crate) turn: u64,
    pub(crate) why: String,
}

/// The chat `by` has had the answers to the chat `chat`'s first `turns`
/// turns (see [`Talk::heard`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Heard {
    pub(crate) chat: u32,
    pub(crate) by: u32,
    pub(crate) turns: u64,
}

/// `count` chats have been started under the user's chat `root` (see
/// [`Talk::starts`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Starts {
    pub(crate) root: u32,
    pub(crate) count: u32,
}

/// Notices have started `count` turns under the user's chat `root` since
/// the user's turn `since` there, by its uid (see [`Talk::notice_turns`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NoticeTurns {
    pub(crate) root: u32,
    pub(crate) since: Option<u64>,
    pub(crate) count: u32,
}

/// The chat `chat` holds a notice for the user, since the user's turn
/// `since` there, by its uid (see [`Talk::held`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Held {
    pub(crate) chat: u32,
    pub(crate) since: Option<u64>,
}

/// The chat `waiter` was waiting for the chats `chats`, all of them or
/// with `first` any one, as the upgrade cut its wait off (see
/// [`Talk::waits`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CutWait {
    pub(crate) waiter: u32,
    pub(crate) chats: Vec<u32>,
    pub(crate) first: bool,
}

/// The agents' processes that go on through an upgrade, rather than
/// stop, and what goes on with them: since version 38. Each chat and
/// terminal is by its number, and each fd by its index in [`Fds`]; every
/// deadline is the milliseconds left of it, as an `Instant` means nothing
/// to another process; and every JSON-RPC ID and set of tool arguments is
/// JSON text, as postcard can't keep JSON's values themselves.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Live {
    pub(crate) agents: Vec<LiveAgent>,
    /// The number the next agent started gets, so none's is used again.
    pub(crate) next_agent: u32,
    /// The chats of those agents.
    pub(crate) chats: Vec<LiveChat>,
    /// The connections those chats' tools are served on.
    pub(crate) tools: Vec<LiveConnection>,
    /// The calls on those connections still waiting.
    pub(crate) waiting: Vec<LiveWait>,
    /// The number the next call waiting for the user gets.
    pub(crate) next_call: u64,
    /// The chats asking their agent for a session, until it answers.
    pub(crate) opening: Vec<LiveOpening>,
    /// The idle chats in the background, and when each lets its session
    /// go.
    pub(crate) parking: Vec<LiveDue>,
    /// The chats whose agent took a turn by itself, and when that turn is
    /// taken to have ended, the agent being quiet till then.
    pub(crate) woken: Vec<LiveDue>,
    /// The terminals agents ran commands in, for the waits for them.
    pub(crate) commanded: Vec<LiveCommanded>,
    /// Each agent's settings, by its name, as its sessions last gave them.
    pub(crate) settings: Vec<(String, Vec<ChatSetting>)>,
    /// How many tokens each agent's context holds, by its name.
    pub(crate) context: Vec<(String, u64)>,
    /// Each agent's account's limits, by its name: how much of each is
    /// used, in percent, and when it starts again, in milliseconds since
    /// the Unix epoch, if it was said.
    pub(crate) limits: Vec<LiveLimits>,
}

/// An agent's account's limits, as it last said them (see [`Live`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveLimits {
    pub(crate) agent: String,
    pub(crate) five_hour: Option<(u8, Option<u64>)>,
    pub(crate) weekly: Option<(u8, Option<u64>)>,
}

/// An agent's process, going on through the upgrade (see [`Live`]).
#[derive(Debug, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent capabilities the agent negotiated"
)]
pub(crate) struct LiveAgent {
    /// The daemon's number for it.
    pub(crate) id: u32,
    /// Its name, which chats find it by.
    pub(crate) name: String,
    /// What it was started with, each word's bytes.
    pub(crate) command: Vec<Vec<u8>>,
    pub(crate) process: Process,
    pub(crate) stdin: u32,
    pub(crate) stdout: u32,
    pub(crate) stderr: u32,
    /// Messages to it not yet written, framed.
    pub(crate) outgoing: Vec<u8>,
    /// What's been read of a message from it not yet whole.
    pub(crate) incoming: Vec<u8>,
    /// What's been read of a line of its stderr not yet ended.
    pub(crate) errors: Vec<u8>,
    /// The last lines of its stderr.
    pub(crate) last_errors: Vec<String>,
    pub(crate) login_required: bool,
    /// The ID its next request gets.
    pub(crate) next_id: u64,
    /// The requests sent it that it hasn't answered, by ID.
    pub(crate) pending: Vec<(u64, LivePending)>,
    /// How long each of those has waited, in milliseconds, by ID, so one
    /// left unanswered through several upgrades is still seen to be.
    pub(crate) waited: Vec<(u64, u64)>,
    /// What it said it can do, as it answered `initialize`: kept, never
    /// worked out again, as it's this process that said it.
    pub(crate) can_close: bool,
    pub(crate) can_load: bool,
    pub(crate) can_image: bool,
    pub(crate) can_steer: bool,
    pub(crate) can_list: Option<bool>,
    /// Whether it was asked what it runs in the background.
    pub(crate) tasks: bool,
    /// The client capabilities clackatoo declared to it as it started, as
    /// JSON: what this connection negotiated, whatever the next build
    /// would declare.
    pub(crate) client: String,
}

/// What a request sent an agent was for, waiting for its answer (see
/// `agent::Pending`). One a client waited for, which can't wait through
/// an upgrade, goes as `Ignored`, its answer dropped.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum LivePending {
    Initialize,
    Session {
        terminal: u32,
        resumed: Option<String>,
        load: bool,
    },
    Prompt(u32),
    Steer(u32, u64),
    Setting(u32),
    Probe,
    Abandoned(u32, Option<String>),
    Closing(u32),
    StopTask(u32, String),
    Ignored,
}

/// A chat whose agent goes on through the upgrade: what it needs beside
/// [`Chat`] to go on as it was (see [`Live`]). Its open turn, if it has
/// one, goes on: [`Chat`] keeps it unended, its tools running.
#[derive(Debug, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts of the chat"
)]
pub(crate) struct LiveChat {
    pub(crate) chat: u32,
    pub(crate) link: LiveLink,
    /// A message waiting for its session to open.
    pub(crate) queued: Option<ResumePrompt>,
    pub(crate) stop_requested: bool,
    /// Whether it says its agent is slow to open its session.
    pub(crate) slow: bool,
    /// How many turns the session will have seen once the message sent
    /// with a digest is answered.
    pub(crate) heard: Option<u64>,
    /// The task standing for the turn its agent took by itself.
    pub(crate) woke_by_task: Option<String>,
    pub(crate) woken_stopping: bool,
    /// How many milliseconds more what the agent says with no turn open
    /// opens none.
    pub(crate) wake_held: Option<u64>,
    /// The tool calls naming clackatoo's own MCP server.
    pub(crate) own_tools: Vec<String>,
    /// What each tool of the open turn last said it changed, by its ID.
    pub(crate) diffs: Vec<(String, Vec<LiveDiff>)>,
    /// The questions waiting for answers, the first shown.
    pub(crate) asking: Vec<LiveQuestion>,
    /// The number its next question gets.
    pub(crate) next_question: u64,
    /// A steering message being delivered: its number, turn (by its uid,
    /// see [`ChatUids`]) and item.
    pub(crate) native: Option<(u64, u64, u64)>,
    /// The queued message a stop is to start again with.
    pub(crate) restart: Option<u64>,
    /// Whether its queue of messages is paused.
    pub(crate) queue_paused: bool,
}

/// Whether a chat's session is open in its agent (see `chat::Link`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LiveLink {
    Closed,
    Opening,
    Open,
    Closing,
}

/// A change a tool said it made to a file.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveDiff {
    pub(crate) path: String,
    pub(crate) old: Option<String>,
    pub(crate) new: String,
}

/// A question waiting in a chat, and who asked it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveQuestion {
    pub(crate) asker: LiveAsker,
    pub(crate) id: u64,
    pub(crate) title: String,
    /// The answers offered: each one's ID, its name, and what it means.
    pub(crate) choices: Vec<(String, String, LiveChoice)>,
    pub(crate) own: bool,
    /// The plan it asks to go ahead with, in Markdown, if it asks to leave
    /// planning.
    pub(crate) plan: Option<String>,
}

/// What an answer means (see `clackatoo_protocol::ChoiceKind`).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum LiveChoice {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    /// One whose meaning the build didn't know, shown as the agent named it.
    Other,
}

/// Who asked a question (see `chat::Asker`).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum LiveAsker {
    /// The agent, in the request with this ID, as JSON.
    Agent(String),
    /// A call to clackatoo's tools, by its number.
    Tool(u64),
    Install,
    Suggest,
    Slow,
}

/// A connection a chat's tools are served on, going on through the
/// upgrade, its socket and all (see [`Live`]).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveConnection {
    pub(crate) stream: u32,
    pub(crate) chat: u32,
    pub(crate) listed: bool,
    /// What's been read of a message not yet whole.
    pub(crate) incoming: Vec<u8>,
    /// Messages not yet written.
    pub(crate) outgoing: Vec<u8>,
    pub(crate) browser: bool,
}

/// A call to a chat's tools still waiting, on the connection
/// `connection`, by its index in [`Live::tools`].
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveWait {
    pub(crate) connection: u32,
    /// The call's JSON-RPC ID, as JSON.
    pub(crate) id: String,
    pub(crate) chat: u32,
    pub(crate) what: LiveWaiting,
}

/// What a call waits for (see `server::tools::Wait`). One waiting on the
/// user or the boards keeps the call as it came, its tool's name and
/// arguments, to be read again as it was.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum LiveWaiting {
    Permission {
        call: u64,
        tool: String,
        arguments: String,
    },
    Finish {
        terminal: u32,
        lines: u64,
        left: u64,
    },
    Turn {
        chat: u32,
        left: u64,
    },
    Chats {
        chats: Vec<u32>,
        first: bool,
        left: u64,
    },
    Boards {
        tool: String,
        arguments: String,
        left: u64,
    },
}

/// A chat asking its agent for a session: the milliseconds it has waited
/// since asking, and has left before it's told the agent is slow.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveOpening {
    pub(crate) chat: u32,
    pub(crate) waited: u64,
    pub(crate) left: u64,
    pub(crate) fetching: bool,
}

/// Something of a chat due in `left` milliseconds.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveDue {
    pub(crate) chat: u32,
    pub(crate) left: u64,
}

/// A terminal an agent ran a command in (see `server::tools::Commanded`).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LiveCommanded {
    pub(crate) terminal: u32,
    pub(crate) owner: Option<u32>,
    pub(crate) name: Option<String>,
    /// A command still waiting for the shell's first prompt: what's to be
    /// typed, the milliseconds it waits still, and whether the shell marks
    /// its prompts.
    pub(crate) typing: Option<(String, u64, bool)>,
    pub(crate) failed: Option<String>,
    pub(crate) expect: Option<u32>,
    pub(crate) expect_first: bool,
    pub(crate) renumbered: u64,
}

/// Why the last message the chat `from` sent the chat `to` was dropped
/// as its turn came: see [`State::undelivered`]. Since version 36.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Undelivered {
    pub(crate) from: u32,
    pub(crate) to: u32,
    pub(crate) why: String,
}

/// A workspace lent as a background tab of the daemon it's lent to: see
/// [`State::lent_background`]. Since version 35.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LentBackground {
    /// The workspace, by its ID.
    pub(crate) workspace: u32,
    /// The exit status of its failure not seen yet, if one is.
    pub(crate) failed: Option<i32>,
}

/// What a buffer's undo steps keep (see [`clackatoo_text::StepState`]):
/// for each author, in the order its history lists them. Since version 34.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BufferUndo {
    pub(crate) buffer: u32,
    pub(crate) authors: Vec<AuthorUndo>,
}

/// What an author's undo and redo steps keep, and which change nothing,
/// as many of each as [`AuthorSteps`] has, and whether older steps were
/// let go. Since version 34.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct AuthorUndo {
    pub(crate) undo: Vec<Option<UndoState>>,
    pub(crate) redo: Vec<Option<UndoState>>,
    pub(crate) forgot: bool,
    pub(crate) undo_empty: Vec<bool>,
    pub(crate) redo_empty: Vec<bool>,
}

/// What one undo step keeps. Since version 34.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct UndoState {
    /// The cursor: a byte on its line, and its column in bytes.
    pub(crate) cursor: (u64, u32),
    pub(crate) taken: bool,
    pub(crate) visual: Option<UndoVisual>,
}

/// The last selection an undo step keeps. Since version 34.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct UndoVisual {
    pub(crate) start: (u64, u32),
    pub(crate) end: (u64, u32),
    /// 0 by characters, 1 by lines, 2 a block.
    pub(crate) kind: u8,
    pub(crate) backward: bool,
    pub(crate) to_end: bool,
}

/// Frozen originals, with handles attached by turn/tool/image position.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ImageOriginals {
    pub(crate) chat: u32,
    pub(crate) next: u64,
    pub(crate) images: Vec<(u64, String)>,
    pub(crate) references: Vec<(u32, Option<String>, u32, u64)>,
}

/// Frozen tool thumbnails, addressed by their chat, turn and tool ID.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ToolPreviews {
    pub(crate) chat: u32,
    pub(crate) turn: u32,
    pub(crate) tool: String,
    pub(crate) images: Vec<ChatPreview>,
}

/// Frozen thumbnail data, independent of the live protocol.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatPreview {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// Preview images by chat and turn. Old handovers have none.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatPreviews {
    pub(crate) chat: u32,
    pub(crate) turns: Vec<(u32, Vec<ChatPreview>)>,
}

/// What a pane made again as the daemon started still has to show for
/// it (see [`clackatoo_protocol::Restored`]). Since version 27.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Restored {
    /// Its pane, by its ID.
    pub(crate) pane: u32,
    /// The command its terminal was running, offered to run again.
    pub(crate) ran: Option<String>,
    /// Where its editor's cursor and view were: the cursor's line and
    /// column, and the line at the top of the view.
    pub(crate) place: Option<(u64, u64, u64)>,
}

/// A terminal the user marked to keep running (see
/// [`clackatoo_protocol::Kept`]). Since version 26.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Kept {
    /// Its pane, by its ID.
    pub(crate) pane: u32,
    /// The command it keeps running.
    pub(crate) command: String,
    /// It's run again as it ends, too.
    pub(crate) always: bool,
    /// Running it again was given up.
    pub(crate) given_up: bool,
    /// Running it again as it ends is paused.
    pub(crate) paused: bool,
    /// How running it again was going, if it was kept running always.
    pub(crate) restart: Option<Restarting>,
}

/// How running a terminal's command again was going (see [`Kept`]), so the
/// pause, the blocks the daemon typed it as, and its waits outlast an
/// upgrade. Since version 26.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Restarting {
    /// The block running as it was typed into, pausing it.
    pub(crate) paused_during: Option<u32>,
    /// The block the daemon typed the command as, and how many times the
    /// terminal had numbered its blocks from 0 again then.
    pub(crate) ours: Option<(u32, u64)>,
    /// The block last seen running the command, as the user ran it.
    pub(crate) running: Option<u32>,
    /// The block whose end was taken for the command's.
    pub(crate) handled: Option<u32>,
    /// How many times in a row it ended soon after it started.
    pub(crate) quick: u32,
    /// How long it waits next time, in milliseconds.
    pub(crate) wait_ms: u64,
    /// How long until it runs again, in milliseconds, if it's to.
    pub(crate) due_in_ms: Option<u64>,
    /// The terminal whose shell ended, which a new shell is to take the
    /// place of.
    pub(crate) ended: Option<u32>,
    /// How long ago the daemon last ran it, in milliseconds, if it did.
    pub(crate) started_ms_ago: Option<u64>,
}

/// The program a terminal was started as, by its device and inode (see
/// [`State::exes`]). Since version 26.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Exe {
    /// The terminal, by its ID.
    pub(crate) terminal: u32,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

/// The blocks of a terminal's whose commands its shell's integration said,
/// with the terminal's secret (see [`State::trusted`]). Since version 26.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Trusted {
    /// The terminal, by its ID.
    pub(crate) terminal: u32,
    /// The blocks, by their numbers, oldest first.
    pub(crate) blocks: Vec<u32>,
}

/// A tab a workspace keeps running out of its tab bar (see
/// [`clackatoo_protocol::Background`]). How it's getting on, and what
/// it's called, are made again from its terminals. Since version 25.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Background {
    /// Its workspace, by its ID.
    pub(crate) workspace: u32,
    pub(crate) tab: Tab,
    /// The exit status of a failure of its no window has said was seen.
    pub(crate) failed: Option<i32>,
}

/// The identity key of another machine, as the daemon knows it. Since
/// version 24.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Known {
    /// The machine's ID.
    pub(crate) machine: [u8; 16],
    /// Its key.
    pub(crate) key: [u8; 32],
    /// Met straight, with ssh, rather than vouched for by a machine
    /// passing it on.
    pub(crate) met: bool,
}

/// Whether a connection is passed on, and the machine it was last
/// connected to. Since version 24.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Sharing {
    /// The connection's host.
    pub(crate) host: String,
    /// The daemon doesn't pass it on, as asked.
    pub(crate) unshared: bool,
    /// The machine's ID.
    pub(crate) machine: Option<[u8; 16]>,
}

/// What a machine this daemon's borrowed panes are on is called. Since
/// version 23.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Lender {
    /// Its ID.
    pub(crate) machine: [u8; 16],
    /// What it says it's called.
    pub(crate) name: String,
}

/// A machine whose daemon this one lends workspaces to. Since version 22.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Borrower {
    /// Its ID.
    pub(crate) machine: [u8; 16],
    /// What it says it's called.
    pub(crate) name: String,
    /// When its daemon was last connected, in seconds since 1970.
    pub(crate) last_seen: u64,
}

/// A workspace lent to the daemon on another machine, holding a pane for
/// it to show (see [`clackatoo_protocol::Lent`]). Since version 22.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Lent {
    /// The workspace, by its ID.
    pub(crate) workspace: u32,
    /// The machine it's lent to, by its ID.
    pub(crate) to: [u8; 16],
    /// The number that machine knows the pane by.
    pub(crate) key: u64,
    /// This daemon put it in that machine's layout itself. Since version
    /// 23; before, none was.
    pub(crate) placed: bool,
}

/// A connection to the daemon on another machine. Since version 21.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Connection {
    /// The machine, as ssh names it.
    pub(crate) host: String,
    /// The shell command that runs clackatoo there.
    pub(crate) command: String,
}

/// What agents changed in files, hunk by hunk, whether the user has
/// reviewed it, the comments waiting to go to chats, and reviews. Since
/// version 15.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Reviews {
    /// The number the next hunk gets.
    pub(crate) next_hunk: u64,
    /// The agent hunks of each open buffer, by its ID.
    pub(crate) buffers: Vec<(u32, Vec<AgentHunk>)>,
    /// The agent hunks of files not open.
    pub(crate) closed: Vec<ClosedHunks>,
    /// What the agent made each hunk's lines into, by its number.
    pub(crate) made: Vec<(u64, String)>,
    /// The number the next comment gets.
    pub(crate) next_comment: u64,
    pub(crate) comments: Vec<Comment>,
    /// The ID the next review gets.
    pub(crate) next_review: u32,
    pub(crate) list: Vec<Review>,
    /// The chat whose agent last changed a file.
    pub(crate) last_chat: Option<u32>,
}

/// Lines an agent changed: bytes `start` to `end` of the text, which were
/// `base`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentHunk {
    pub(crate) id: u64,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) base: String,
    pub(crate) chat: u32,
    pub(crate) turn: u32,
    pub(crate) reviewed: bool,
}

/// The agent hunks of a file not open, which fit its text as it was when
/// its stamp was `stamp`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ClosedHunks {
    /// The file's path, as bytes.
    pub(crate) path: Vec<u8>,
    pub(crate) stamp: Option<DiskStamp>,
    pub(crate) hunks: Vec<AgentHunk>,
}

/// A comment waiting to go to chat `chat`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Comment {
    pub(crate) chat: u32,
    pub(crate) id: u64,
    /// The file's path, as bytes.
    pub(crate) path: Vec<u8>,
    /// The line, from 1, as it was when it was made.
    pub(crate) line: u32,
    /// The buffer it's in, and where, while that's open.
    pub(crate) at: Option<(u32, u64)>,
    pub(crate) text: String,
}

/// A review: what it lists, of which chat, from which directory.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Review {
    pub(crate) id: u32,
    pub(crate) chat: Option<u32>,
    pub(crate) scope: ReviewScope,
    /// The project's directory, as bytes.
    pub(crate) root: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum ReviewScope {
    Turn(u32),
    Chat,
    Base,
}

/// The number of the last change window `window` sent buffer `buffer`
/// that the daemon dealt with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Applied {
    pub(crate) buffer: u32,
    pub(crate) window: u64,
    pub(crate) seq: u64,
    /// The version it made, if it made one.
    pub(crate) landed: Option<u64>,
}

/// What agents changed in files, by chat and turn. Since version 12.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct EditLog {
    pub(crate) chats: Vec<ChatEdits>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatEdits {
    pub(crate) chat: u32,
    /// Turns dropped to keep to the bound, and how many files each changed.
    pub(crate) dropped: Vec<(u64, u32)>,
    pub(crate) turns: Vec<TurnEdits>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TurnEdits {
    pub(crate) turn: u64,
    pub(crate) files: Vec<FileEdits>,
}

/// What a turn changed in one file.
#[derive(Debug, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four facts about a file, each yes or no, and each free of the others"
)]
pub(crate) struct FileEdits {
    /// The file's path, as bytes.
    pub(crate) path: Vec<u8>,
    /// The tool call that last changed it.
    pub(crate) tool: Option<String>,
    /// Whether the file was there before the changes, and after.
    pub(crate) existed: bool,
    pub(crate) exists: bool,
    /// Whether what the turn did to it is known.
    pub(crate) known: bool,
    /// Whether the turn's changes to it are undone.
    pub(crate) undone: bool,
    pub(crate) changes: Vec<FileChange>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum FileChange {
    /// The edit that made `version` of buffer `buffer`, keeping `bytes`.
    Buffer {
        buffer: u32,
        version: u64,
        bytes: u64,
    },
    /// A change to the file not open: `edit` made `after`.
    Text { after: String, edit: Vec<EditOp> },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Terminal {
    pub(crate) id: u32,
    /// The program, unless it has been reaped.
    pub(crate) process: Option<Process>,
    /// The program's pid, kept after it is reaped.
    pub(crate) pid: u32,
    /// The PTY's master end, unless it has closed.
    pub(crate) pty: Option<u32>,
    pub(crate) status: Status,
    /// Input the PTY hasn't accepted yet.
    pub(crate) input: Vec<u8>,
    pub(crate) screen: Snapshot,
    pub(crate) blocks: BlocksSnapshot,
    pub(crate) links: LinksSnapshot,
}

/// A terminal's shell's secret, which its integration adds to its marks
/// (see `crate::shell`). Since version 20.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Shell {
    pub(crate) terminal: u32,
    pub(crate) secret: String,
    /// The file the shell reads it from, if it's still there, for the next
    /// build to remove once the shell has read it.
    pub(crate) file: Option<Vec<u8>>,
    /// The latest block, if the integration marked its command with the
    /// secret: the terminal's blocks don't say.
    pub(crate) trusted: Option<u32>,
}

/// A chat with an agent. The agent isn't handed over: the next build
/// opens the session again when it's next needed.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Chat {
    pub(crate) id: u32,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) title: String,
    /// The directory the agent works in.
    pub(crate) cwd: String,
    /// The agent's session, if it had made one.
    pub(crate) session: Option<String>,
    /// Why the agent couldn't start, if it couldn't.
    pub(crate) failure: Option<String>,
    /// The turns. A turn the upgrade cut off is handed over ended.
    pub(crate) turns: Vec<ChatTurn>,
    /// The agent's settings, as it last gave them.
    pub(crate) settings: Vec<ChatSetting>,
    /// The values the user gave settings, by their IDs.
    pub(crate) chosen: Vec<(String, String)>,
    /// The agent, by its name: `claude`, `codex`…
    pub(crate) agent: String,
    /// Where it stands among the chats agents talk in. Since version 13.
    pub(crate) relations: ChatRelations,
    /// How many of the turns the session has seen. Since version 16.
    pub(crate) known: u64,
    /// The sessions it had with other agents. Since version 16.
    pub(crate) parked: Vec<ParkedSession>,
    /// The turns where it moved to another agent. Since version 16.
    pub(crate) switches: Vec<ChatSwitch>,
    /// About the most tokens a digest for the next agent takes. Since
    /// version 16.
    pub(crate) budget: u32,
    /// The agent's plan, as it last gave it, and that agent, as it's
    /// shown. Since version 16.
    pub(crate) plan: Vec<PlanStep>,
    pub(crate) plan_by: Option<String>,
}

impl Chat {
    /// What a chat from before version 16 had of moving between agents:
    /// nothing, its session having seen all of its `turns`.
    fn of_one_agent(turns: &[ChatTurn]) -> (u64, u32) {
        let known = u64::try_from(turns.len()).unwrap_or(u64::MAX);
        let budget = u32::try_from(crate::handoff::BUDGET).unwrap_or(u32::MAX);
        (known, budget)
    }
}

/// A session a chat had with another agent. Since version 16.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ParkedSession {
    /// The agent, by its name.
    pub(crate) agent: String,
    /// The agent, as it's shown.
    pub(crate) title: String,
    pub(crate) session: Option<String>,
    /// How many of the chat's turns it saw.
    pub(crate) known: u64,
    pub(crate) chosen: Vec<(String, String)>,
    pub(crate) settings: Vec<ChatSetting>,
}

/// A turn of a chat that's where it moved to another agent. Since
/// version 16.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatSwitch {
    /// The turn's index.
    pub(crate) turn: u64,
    /// The agents it moved from and to, as they're shown.
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) model: Option<String>,
    /// It went back to a session it had before.
    pub(crate) resumed: bool,
    /// The settings that didn't carry over.
    pub(crate) notes: Vec<String>,
}

/// One step of an agent's plan. Since version 16.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PlanStep {
    pub(crate) content: String,
    pub(crate) state: StepState,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum StepState {
    ToDo,
    Doing,
    Done,
}

/// Where a chat stands among the chats agents talk in: which chat's agent
/// started it, and which messages other agents sent it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ChatRelations {
    /// The chat whose agent started it, if an agent did.
    pub(crate) parent: Option<u32>,
    /// The chat the user started that the chats starting it go back to.
    pub(crate) root: Option<u32>,
    /// How many chats started by agents lead down to it.
    pub(crate) depth: u8,
    /// How many messages between agents led to its last turn.
    pub(crate) hops: u8,
    /// The user's turn its last turn came of: the chat, and the turn's
    /// index, if a message started it.
    pub(crate) cause: Option<(u32, u32)>,
    /// The turns other agents sent, by index, and who sent each.
    pub(crate) senders: Vec<(u32, ChatSender)>,
    /// Messages from other agents waiting for its turn to end.
    pub(crate) inbox: Vec<ChatMessage>,
}

/// The chat whose agent sent a message, as it was then.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatSender {
    pub(crate) chat: u32,
    pub(crate) agent: String,
    pub(crate) title: String,
}

/// A message another chat's agent sent, waiting its turn.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatMessage {
    pub(crate) from: ChatSender,
    pub(crate) text: String,
    pub(crate) hops: u8,
    pub(crate) cause: Option<(u32, u32)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatSetting {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: SettingKind,
    pub(crate) value: String,
    pub(crate) choices: Vec<ChatChoice>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum SettingKind {
    Mode,
    Model,
    Effort,
    Other,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatChoice {
    pub(crate) value: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatTurn {
    pub(crate) prompt: String,
    /// Milliseconds since the Unix epoch.
    pub(crate) sent: u64,
    pub(crate) items: Vec<ChatItem>,
    pub(crate) end: Option<ChatEnd>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChatItem {
    Words(String),
    Thought(String),
    Tool {
        id: String,
        title: String,
        status: ToolStatus,
        kind: ToolKind,
        content: Vec<ToolContent>,
        locations: Vec<ToolLocation>,
    },
}

/// A file a tool works in, and the line, if it says.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ToolLocation {
    pub(crate) path: String,
    pub(crate) line: Option<u32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum ToolStatus {
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ToolContent {
    /// Markdown.
    Text(String),
    /// A change to the file at `path`: `old` is `None` for a new file.
    Diff {
        path: String,
        old: Option<String>,
        new: String,
    },
    /// A command's output, as plain text. Since version 11.
    Output(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatEnd {
    /// Milliseconds since the Unix epoch.
    pub(crate) at: u64,
    pub(crate) outcome: ChatOutcome,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChatOutcome {
    Done,
    Stopped,
    Failed(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Process {
    pub(crate) pid: i32,
    pub(crate) pidfd: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum Status {
    Running,
    Exited(i32),
    Killed(i32),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Workspaces {
    pub(crate) list: Vec<Workspace>,
    pub(crate) active: Option<u32>,
    pub(crate) next_workspace: u32,
    pub(crate) next_tab: u32,
    pub(crate) next_pane: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Tab {
    pub(crate) id: u32,
    pub(crate) layout: Layout,
    pub(crate) focused: u32,
    pub(crate) name: Option<String>,
}

/// Every version reads this, so new kinds of pane only ever go on the end.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Layout {
    /// A pane showing a terminal.
    Pane { id: u32, terminal: u32 },
    /// Panes side by side, left to right or top to bottom, each with its
    /// share of the space.
    Split {
        left_to_right: bool,
        parts: Vec<Layout>,
        shares: Vec<u32>,
    },
    /// A pane showing a buffer. Since version 8.
    Buffer { id: u32, buffer: u32 },
    /// A pane showing a file explorer. Since version 10.
    Files { id: u32, files: u32 },
    /// A pane showing a board, by the board's ID, which is kept on disk
    /// with it. Since version 13.
    Board { id: u32, board: u32 },
    /// A pane showing a review. Since version 15.
    Review { id: u32, review: u32 },
    /// A pane showing a chat's page in the browser, by the chat's ID.
    /// Since version 17.
    Browser { id: u32, chat: u32 },
    /// A pane showing a folder in an editor, as its listing, by the
    /// explorer whose root it is. Since version 18.
    Folder { id: u32, files: u32 },
    /// A pane showing what's on another machine: a pane its daemon lent
    /// this one, by that machine's ID and the number this daemon gave the
    /// pane, and the kind of pane it is, such as `terminal`. Since version
    /// 22.
    Borrowed {
        id: u32,
        machine: [u8; 16],
        key: u64,
        kind: String,
    },
    /// A pane showing an image file, by its picture's ID (see
    /// [`Images::pictures`]). Since version 49.
    Picture { id: u32, picture: u32 },
}

/// A file explorer: the directory at its root. What's expanded in it is
/// each window's own, and isn't handed over.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Files {
    pub(crate) id: u32,
    /// The directory, as bytes.
    pub(crate) root: Vec<u8>,
}

/// A file being edited: its text, its history, and the file as it was
/// when last read or written.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Buffer {
    pub(crate) id: u32,
    /// The file's path, as bytes; none for a buffer with no file yet.
    /// Nothing reads a buffer's path as it's resumed, so an older build
    /// taking one over has a buffer it can't write, not a failed handover.
    pub(crate) path: Vec<u8>,
    /// The directory paths opened beside it are from, as bytes.
    pub(crate) dir: Vec<u8>,
    pub(crate) text: String,
    /// The text as last read or written, if it's changed since.
    pub(crate) saved: Option<String>,
    /// The file as last read or written, if it existed.
    pub(crate) disk: Option<DiskStamp>,
    pub(crate) history: BufferHistory,
}

/// What tells one version of a file from another without reading it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct DiskStamp {
    pub(crate) seconds: i64,
    pub(crate) nanoseconds: i64,
    pub(crate) len: u64,
    pub(crate) inode: u64,
    pub(crate) device: u64,
}

/// A buffer's edits, and each author's undo and redo steps: see
/// [`clackatoo_text::History`].
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BufferHistory {
    pub(crate) first: u64,
    pub(crate) edits: Vec<(Vec<EditOp>, Author)>,
    pub(crate) steps: Vec<AuthorSteps>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum EditOp {
    Retain(u64),
    Insert(String),
    Delete(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum Author {
    User,
    Agent(u32),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AuthorSteps {
    pub(crate) author: Author,
    pub(crate) version: u64,
    pub(crate) undo: Vec<Vec<EditOp>>,
    pub(crate) redo: Vec<Vec<EditOp>>,
}

/// A handover couldn't be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HandoverError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("it isn't a handover from clackatoo")]
    NotAHandover,
    #[error("it is version {0}, newer than this build reads")]
    Version(u32),
    #[error("it can't be decoded: {0}")]
    Decode(#[from] postcard::Error),
    #[error("it is damaged: {0}")]
    Damaged(&'static str),
    #[error(transparent)]
    Restore(#[from] clackatoo_term::RestoreError),
}

impl State {
    /// Writes the state to `file`, from its start.
    pub(crate) fn write(&self, file: &mut File) -> io::Result<()> {
        let body = postcard::to_allocvec(self).map_err(io::Error::other)?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        file.write_all(&body)?;
        file.rewind()
    }

    /// Reads a state [`write`](Self::write) wrote, from `file`'s start.
    #[cfg(test)]
    pub(crate) fn read(file: &mut File) -> Result<Self, HandoverError> {
        Self::read_versioned(file).map(|(state, _)| state)
    }

    /// As [`read`](Self::read), with the version it was written in.
    pub(crate) fn read_versioned(file: &mut File) -> Result<(Self, u32), HandoverError> {
        file.rewind()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Self::from_bytes(&bytes)
    }

    /// As [`read_versioned`](Self::read_versioned), from the file's bytes.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<(Self, u32), HandoverError> {
        let rest = bytes
            .strip_prefix(MAGIC)
            .ok_or(HandoverError::NotAHandover)?;
        let (version, body) = rest
            .split_first_chunk::<4>()
            .ok_or(HandoverError::NotAHandover)?;
        let version = u32::from_le_bytes(*version);
        let state = match version {
            1 => Ok(v7::State::from(postcard::from_bytes::<v1::State>(body)?).into()),
            2 => Ok(v7::State::from(postcard::from_bytes::<v2::State>(body)?).into()),
            3 => Ok(v7::State::try_from(postcard::from_bytes::<v3::State>(body)?)?.into()),
            4 => Ok(v7::State::from(postcard::from_bytes::<v4::State>(body)?).into()),
            5 => Ok(v7::State::from(postcard::from_bytes::<v5::State>(body)?).into()),
            6 => Ok(v7::State::from(postcard::from_bytes::<v6::State>(body)?).into()),
            7 => Ok(postcard::from_bytes::<v7::State>(body)?.into()),
            8 => Ok(postcard::from_bytes::<v8::State>(body)?.into()),
            9 => Ok(postcard::from_bytes::<v9::State>(body)?.into()),
            10 => Ok(postcard::from_bytes::<v10::State>(body)?.into()),
            11 => Ok(postcard::from_bytes::<v11::State>(body)?.into()),
            12 => Ok(postcard::from_bytes::<v12::State>(body)?.into()),
            13 => Ok(postcard::from_bytes::<v13::State>(body)?.into()),
            14 => Ok(postcard::from_bytes::<v14::State>(body)?.into()),
            15 => Ok(postcard::from_bytes::<v15::State>(body)?.into()),
            // Versions 16 and 17 are version 18 without browser panes,
            // then without folders' panes, which go on the end of
            // `Layout`, so they read as it does.
            16..=18 => Ok(postcard::from_bytes::<v18::State>(body)?.into()),
            19 => Ok(postcard::from_bytes::<v19::State>(body)?.into()),
            20 => Ok(postcard::from_bytes::<v20::State>(body)?.into()),
            // Version 21 is version 22 without lent workspaces; borrowed
            // panes go on the end of `Layout`, so they read as it does.
            21 => Ok(postcard::from_bytes::<v21::State>(body)?.into()),
            22 => Ok(postcard::from_bytes::<v22::State>(body)?.into()),
            23 => Ok(postcard::from_bytes::<v23::State>(body)?.into()),
            24 => Ok(postcard::from_bytes::<v24::State>(body)?.into()),
            25 => Ok(postcard::from_bytes::<v25::State>(body)?.into()),
            26 => Ok(postcard::from_bytes::<v26::State>(body)?.into()),
            27 => Ok(postcard::from_bytes::<v27::State>(body)?.into()),
            28 => Ok(postcard::from_bytes::<v28::State>(body)?.into()),
            29 => Ok(postcard::from_bytes::<v29::State>(body)?.into()),
            30 => Ok(postcard::from_bytes::<v30::State>(body)?.into()),
            31 => Ok(postcard::from_bytes::<v31::State>(body)?.into()),
            32 => Ok(postcard::from_bytes::<v32::State>(body)?.into()),
            33 => Ok(postcard::from_bytes::<v33::State>(body)?.into()),
            34 => Ok(postcard::from_bytes::<v34::State>(body)?.into()),
            35 => Ok(postcard::from_bytes::<v35::State>(body)?.into()),
            36 => Ok(postcard::from_bytes::<v36::State>(body)?.into()),
            37 => Ok(postcard::from_bytes::<v37::State>(body)?.into()),
            38 => Ok(postcard::from_bytes::<v38::State>(body)?.into()),
            39 => Ok(postcard::from_bytes::<v39::State>(body)?.into()),
            40 => Ok(postcard::from_bytes::<v40::State>(body)?.into()),
            41 => Ok(postcard::from_bytes::<v41::State>(body)?.into()),
            42 => Ok(postcard::from_bytes::<v42::State>(body)?.into()),
            43 => Ok(postcard::from_bytes::<v43::State>(body)?.into()),
            // Version 45 is 44's shape: it says only that the build
            // handing over asked before a folder's servers ran.
            44 | 45 => Ok(postcard::from_bytes::<v45::State>(body)?.into()),
            46 => Ok(postcard::from_bytes::<v46::State>(body)?.into()),
            47 => Ok(postcard::from_bytes::<v47::State>(body)?.into()),
            48 => Ok(postcard::from_bytes::<v48::State>(body)?.into()),
            49 => Ok(postcard::from_bytes::<v49::State>(body)?.into()),
            50 => Ok(postcard::from_bytes::<v50::State>(body)?.into()),
            51 => Ok(postcard::from_bytes::<v51::State>(body)?.into()),
            52 => Ok(postcard::from_bytes::<v52::State>(body)?.into()),
            VERSION => Ok(postcard::from_bytes(body)?),
            other => Err(HandoverError::Version(other)),
        };
        state.map(|state| (state, version))
    }
}

/// Version 31, before automatic continuation after upgrades.
mod v31 {
    use super::{
        Applied, Background, Borrower, Buffer, Chat, ChatPreviews, ChatQueue, Connection, EditLog,
        Exe, Files, ImageOriginals, Kept, Known, Lender, Lent, Process, Restored, Reviews, Sharing,
        Shell, Terminal, ToolPreviews, Trusted, Workspaces,
    };
    use serde::Deserialize;
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
        background: Vec<Background>,
        kept: Vec<Kept>,
        trusted: Vec<Trusted>,
        exes: Vec<Exe>,
        restored: Option<Vec<Restored>>,
        chat_previews: Vec<ChatPreviews>,
        tool_previews: Vec<ToolPreviews>,
        image_originals: Vec<ImageOriginals>,
        chat_queues: Vec<ChatQueue>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 30, before pending user messages.
mod v30 {
    use super::{
        Applied, Background, Borrower, Buffer, Chat, ChatPreviews, Connection, EditLog, Exe, Files,
        ImageOriginals, Kept, Known, Lender, Lent, Process, Restored, Reviews, Sharing, Shell,
        Terminal, ToolPreviews, Trusted, Workspaces,
    };
    use serde::Deserialize;
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
        background: Vec<Background>,
        kept: Vec<Kept>,
        trusted: Vec<Trusted>,
        exes: Vec<Exe>,
        restored: Option<Vec<Restored>>,
        chat_previews: Vec<ChatPreviews>,
        tool_previews: Vec<ToolPreviews>,
        image_originals: Vec<ImageOriginals>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 29, before retained original images.
mod v29 {
    use super::{
        Applied, Background, Borrower, Buffer, Chat, ChatPreviews, Connection, EditLog, Exe, Files,
        Kept, Known, Lender, Lent, Process, Restored, Reviews, Sharing, Shell, Terminal,
        ToolPreviews, Trusted, Workspaces,
    };
    use serde::Deserialize;
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
        background: Vec<Background>,
        kept: Vec<Kept>,
        trusted: Vec<Trusted>,
        exes: Vec<Exe>,
        restored: Option<Vec<Restored>>,
        chat_previews: Vec<ChatPreviews>,
        tool_previews: Vec<ToolPreviews>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 28, before image tool results.
mod v28 {
    use super::{
        Applied, Background, Borrower, Buffer, Chat, ChatPreviews, Connection, EditLog, Exe, Files,
        Kept, Known, Lender, Lent, Process, Restored, Reviews, Sharing, Shell, Terminal, Trusted,
        Workspaces,
    };
    use serde::Deserialize;
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
        background: Vec<Background>,
        kept: Vec<Kept>,
        trusted: Vec<Trusted>,
        exes: Vec<Exe>,
        restored: Option<Vec<Restored>>,
        chat_previews: Vec<ChatPreviews>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 27, before retained chat previews.
mod v27 {
    use super::{
        Applied, Background, Borrower, Buffer, Chat, Connection, EditLog, Exe, Files, Kept, Known,
        Lender, Lent, Process, Restored, Reviews, Sharing, Shell, Terminal, Trusted, Workspaces,
    };
    use serde::Deserialize;
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
        background: Vec<Background>,
        kept: Vec<Kept>,
        trusted: Vec<Trusted>,
        exes: Vec<Exe>,
        restored: Option<Vec<Restored>>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 26, as the builds that wrote it wrote it: what panes made again
/// still had to show wasn't handed over.
mod v26 {
    use serde::Deserialize;

    use super::{
        Applied, Background, Borrower, Buffer, Chat, Connection, EditLog, Exe, Files, Kept, Known,
        Lender, Lent, Process, Reviews, Sharing, Shell, Terminal, Trusted, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
        background: Vec<Background>,
        kept: Vec<Kept>,
        trusted: Vec<Trusted>,
        exes: Vec<Exe>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 25, as the builds that wrote it wrote it: no terminal was kept
/// running.
mod v25 {
    use serde::Deserialize;

    use super::{
        Applied, Background, Borrower, Buffer, Chat, Connection, EditLog, Files, Known, Lender,
        Lent, Process, Reviews, Sharing, Shell, Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
        background: Vec<Background>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 24, as the builds that wrote it wrote it: no background tabs.
mod v24 {
    use serde::Deserialize;

    use super::{
        Applied, Borrower, Buffer, Chat, Connection, EditLog, Files, Known, Lender, Lent, Process,
        Reviews, Sharing, Shell, Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
        sharing: Vec<Sharing>,
        key: Option<[u8; 32]>,
        keys: Vec<Known>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 23, as the builds that wrote it wrote it: every connection was
/// passed on, and none's machine was kept.
mod v23 {
    use serde::Deserialize;

    use super::{
        Applied, Borrower, Buffer, Chat, Connection, EditLog, Files, Lender, Lent, Process,
        Reviews, Shell, Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
        lenders: Vec<Lender>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 22, as the builds that wrote it wrote it: no lent workspace
/// was placed, and no machine's name was kept for the panes borrowed.
mod v22 {
    use serde::Deserialize;

    use super::{
        Applied, Borrower, Buffer, Chat, Connection, EditLog, Files, Process, Reviews, Shell,
        Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
        lent: Vec<Lent>,
        borrowers: Vec<Borrower>,
    }

    #[derive(Debug, Deserialize)]
    struct Lent {
        workspace: u32,
        to: [u8; 16],
        key: u64,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: (state.lent.into_iter())
                    .map(|lent| super::Lent {
                        workspace: lent.workspace,
                        to: lent.to,
                        key: lent.key,
                        placed: false,
                    })
                    .collect(),
                borrowers: state.borrowers,
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 21, as the builds that wrote it wrote it: no lent workspaces.
mod v21 {
    use serde::Deserialize;

    use super::{
        Applied, Buffer, Chat, Connection, EditLog, Files, Process, Reviews, Shell, Terminal,
        Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 20, as the builds that wrote it wrote it: no machine, and no
/// connections.
mod v20 {
    use serde::Deserialize;

    use super::{
        Applied, Buffer, Chat, EditLog, Files, Process, Reviews, Shell, Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 19, as the builds that wrote it wrote it: no shells' secrets.
mod v19 {
    use serde::Deserialize;

    use super::{Applied, Buffer, Chat, EditLog, Files, Process, Reviews, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 18, as the builds that wrote it wrote it: no lock on the
/// recovery files.
mod v18 {
    use serde::Deserialize;

    use super::{Applied, Buffer, Chat, EditLog, Files, Process, Reviews, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 1, as the builds that wrote it wrote it. The types that later
/// versions left as they were are shared: copy one here before changing it.
mod v1 {
    use clackatoo_term::Snapshot;
    use serde::Deserialize;

    use super::{Process, Status, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    struct Terminal {
        id: u32,
        process: Option<Process>,
        pid: u32,
        pty: Option<u32>,
        status: Status,
        input: Vec<u8>,
        screen: Snapshot,
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state
                    .terminals
                    .into_iter()
                    .map(|terminal| super::v8::Terminal {
                        id: terminal.id,
                        process: terminal.process,
                        pid: terminal.pid,
                        pty: terminal.pty,
                        status: terminal.status,
                        input: terminal.input,
                        screen: terminal.screen,
                        blocks: clackatoo_term::BlocksSnapshot::default(),
                    })
                    .collect(),
                chats: Vec::new(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 2, as the builds that wrote it wrote it, sharing the types
/// later versions left as they were.
mod v2 {
    use serde::Deserialize;

    use super::v8::Terminal;
    use super::{Process, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: Vec::new(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 3, as the builds that wrote it wrote it: chats as the screens
/// they were written into, which become one turn of their text.
mod v3 {
    use clackatoo_term::{BlocksSnapshot, Position, Snapshot};
    use serde::Deserialize;

    use super::{
        ChatEnd, ChatItem, ChatOutcome, ChatTurn, HandoverError, Process, Workspaces, v8::Terminal,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) answering: bool,
        pub(super) failure: Option<String>,
        pub(super) input: String,
        pub(super) screen: Snapshot,
        pub(super) blocks: BlocksSnapshot,
    }

    impl TryFrom<State> for super::v7::State {
        type Error = HandoverError;

        fn try_from(state: State) -> Result<Self, HandoverError> {
            let chats = (state.chats.into_iter())
                .map(super::v10::Chat::try_from)
                .collect::<Result<_, _>>()?;
            Ok(Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
            })
        }
    }

    impl TryFrom<Chat> for super::v10::Chat {
        type Error = HandoverError;

        /// The chat's text, as written into its screen and history, in
        /// one turn of its own, as a block of text.
        fn try_from(chat: Chat) -> Result<Self, HandoverError> {
            let screen = clackatoo_term::Terminal::restore(
                &chat.screen,
                &clackatoo_term::Config::default(),
            )?;
            let size = screen.size();
            let text = screen.text(
                Position { line: 0, col: 0 },
                Position {
                    line: u64::MAX,
                    col: usize::MAX,
                },
                true,
            );
            let text = text.trim_end();
            let turns = if text.is_empty() {
                Vec::new()
            } else {
                vec![ChatTurn {
                    prompt: String::new(),
                    sent: 0,
                    items: vec![ChatItem::Words(format!("```text\n{text}\n```"))],
                    end: Some(ChatEnd {
                        at: 0,
                        outcome: ChatOutcome::Done,
                    }),
                }]
            };
            let _ = (chat.answering, chat.input, chat.blocks);
            Ok(Self {
                id: chat.id,
                cols: size.cols(),
                rows: size.rows(),
                title: screen.title().to_owned(),
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns,
                settings: Vec::new(),
                chosen: Vec::new(),
            })
        }
    }
}

/// Version 4, as the builds that wrote it wrote it: chats without their
/// settings, which they get again from the agent.
mod v4 {
    use serde::Deserialize;

    use super::v5::ChatTurn;
    use super::v8::Terminal;
    use super::{Process, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    struct Chat {
        id: u32,
        cols: u16,
        rows: u16,
        title: String,
        cwd: String,
        session: Option<String>,
        failure: Option<String>,
        turns: Vec<ChatTurn>,
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            super::v5::State {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: (state.chats.into_iter())
                    .map(|chat| super::v5::Chat {
                        id: chat.id,
                        cols: chat.cols,
                        rows: chat.rows,
                        title: chat.title,
                        cwd: chat.cwd,
                        session: chat.session,
                        failure: chat.failure,
                        turns: chat.turns,
                        settings: Vec::new(),
                        chosen: Vec::new(),
                    })
                    .collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
            .into()
        }
    }
}

/// Version 5, as the builds that wrote it wrote it: tools without their
/// kinds or what they show, which come through as tools of no kind,
/// showing nothing.
mod v5 {
    use serde::Deserialize;

    use super::v8::Terminal;
    use super::{ChatEnd, ChatSetting, Process, ToolStatus, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        pub(super) listener: u32,
        pub(super) lock: Option<u32>,
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        pub(super) chats: Vec<Chat>,
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cols: u16,
        pub(super) rows: u16,
        pub(super) title: String,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) failure: Option<String>,
        pub(super) turns: Vec<ChatTurn>,
        pub(super) settings: Vec<ChatSetting>,
        pub(super) chosen: Vec<(String, String)>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct ChatTurn {
        prompt: String,
        sent: u64,
        items: Vec<ChatItem>,
        end: Option<ChatEnd>,
    }

    #[derive(Debug, Deserialize)]
    enum ChatItem {
        Words(String),
        Thought(String),
        Tool {
            id: String,
            title: String,
            status: ToolStatus,
        },
    }

    impl From<ChatTurn> for super::v6::ChatTurn {
        fn from(turn: ChatTurn) -> Self {
            Self {
                prompt: turn.prompt,
                sent: turn.sent,
                items: (turn.items.into_iter())
                    .map(|item| match item {
                        ChatItem::Words(words) => super::v6::ChatItem::Words(words),
                        ChatItem::Thought(words) => super::v6::ChatItem::Thought(words),
                        ChatItem::Tool { id, title, status } => super::v6::ChatItem::Tool {
                            id,
                            title,
                            status,
                            kind: super::ToolKind::Other,
                            content: Vec::new(),
                        },
                    })
                    .collect(),
                end: turn.end,
            }
        }
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            super::v6::State {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: (state.chats.into_iter())
                    .map(|chat| super::v6::Chat {
                        id: chat.id,
                        cols: chat.cols,
                        rows: chat.rows,
                        title: chat.title,
                        cwd: chat.cwd,
                        session: chat.session,
                        failure: chat.failure,
                        turns: chat.turns.into_iter().map(Into::into).collect(),
                        settings: chat.settings,
                        chosen: chat.chosen,
                    })
                    .collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
            .into()
        }
    }
}

/// Version 6, as the builds that wrote it wrote it: tools without where
/// they work, which come through as working nowhere in particular.
mod v6 {
    use serde::Deserialize;

    use super::{
        ChatEnd, ChatSetting, Process, ToolContent, ToolKind, ToolStatus, Workspaces, v8::Terminal,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        pub(super) listener: u32,
        pub(super) lock: Option<u32>,
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        pub(super) chats: Vec<Chat>,
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cols: u16,
        pub(super) rows: u16,
        pub(super) title: String,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) failure: Option<String>,
        pub(super) turns: Vec<ChatTurn>,
        pub(super) settings: Vec<ChatSetting>,
        pub(super) chosen: Vec<(String, String)>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct ChatTurn {
        pub(super) prompt: String,
        pub(super) sent: u64,
        pub(super) items: Vec<ChatItem>,
        pub(super) end: Option<ChatEnd>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) enum ChatItem {
        Words(String),
        Thought(String),
        Tool {
            id: String,
            title: String,
            status: ToolStatus,
            kind: ToolKind,
            content: Vec<ToolContent>,
        },
    }

    impl From<ChatTurn> for super::ChatTurn {
        fn from(turn: ChatTurn) -> Self {
            Self {
                prompt: turn.prompt,
                sent: turn.sent,
                items: (turn.items.into_iter())
                    .map(|item| match item {
                        ChatItem::Words(words) => super::ChatItem::Words(words),
                        ChatItem::Thought(words) => super::ChatItem::Thought(words),
                        ChatItem::Tool {
                            id,
                            title,
                            status,
                            kind,
                            content,
                        } => super::ChatItem::Tool {
                            id,
                            title,
                            status,
                            kind,
                            content,
                            locations: Vec::new(),
                        },
                    })
                    .collect(),
                end: turn.end,
            }
        }
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: (state.chats.into_iter())
                    .map(|chat| super::v10::Chat {
                        id: chat.id,
                        cols: chat.cols,
                        rows: chat.rows,
                        title: chat.title,
                        cwd: chat.cwd,
                        session: chat.session,
                        failure: chat.failure,
                        turns: chat.turns.into_iter().map(Into::into).collect(),
                        settings: chat.settings,
                        chosen: chat.chosen,
                    })
                    .collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 7, as the builds that wrote it wrote it: no buffers. The older
/// versions are read into this.
mod v7 {
    use serde::Deserialize;

    use super::v8::Terminal;
    use super::v10::Chat;
    use super::{Process, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        pub(super) listener: u32,
        pub(super) lock: Option<u32>,
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        pub(super) chats: Vec<Chat>,
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals.into_iter().map(Into::into).collect(),
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: 1,
                buffers: Vec::new(),
                next_files: 1,
                explorers: Vec::new(),
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 8, as the builds that wrote it wrote it: terminals without
/// their links. Older versions share its terminals.
mod v8 {
    use clackatoo_term::{BlocksSnapshot, LinksSnapshot, Snapshot};
    use serde::Deserialize;

    use super::v10::Chat;
    use super::{Buffer, Process, Status, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Terminal {
        pub(super) id: u32,
        pub(super) process: Option<Process>,
        pub(super) pid: u32,
        pub(super) pty: Option<u32>,
        pub(super) status: Status,
        pub(super) input: Vec<u8>,
        pub(super) screen: Snapshot,
        pub(super) blocks: BlocksSnapshot,
    }

    impl From<Terminal> for super::Terminal {
        fn from(terminal: Terminal) -> Self {
            Self {
                id: terminal.id,
                process: terminal.process,
                pid: terminal.pid,
                pty: terminal.pty,
                status: terminal.status,
                input: terminal.input,
                screen: terminal.screen,
                blocks: terminal.blocks,
                links: LinksSnapshot::default(),
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals.into_iter().map(Into::into).collect(),
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: 1,
                explorers: Vec::new(),
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 9, as the builds that wrote it wrote it: no file explorers.
mod v9 {
    use serde::Deserialize;

    use super::v10::Chat;
    use super::{Buffer, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: 1,
                explorers: Vec::new(),
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 11, as the builds that wrote it wrote it: no edit log.
mod v11 {
    use serde::Deserialize;

    use super::v12::Chat;
    use super::{Buffer, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 15, as the builds that wrote it wrote it: chats with one agent
/// each. Versions 13 and 14 share its chats.
mod v15 {
    use serde::Deserialize;

    use super::{
        Applied, Buffer, ChatRelations, ChatSetting, ChatTurn, EditLog, Files, Process, Reviews,
        Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        id: u32,
        cols: u16,
        rows: u16,
        title: String,
        cwd: String,
        session: Option<String>,
        failure: Option<String>,
        turns: Vec<ChatTurn>,
        settings: Vec<ChatSetting>,
        chosen: Vec<(String, String)>,
        agent: String,
        relations: ChatRelations,
    }

    impl From<Chat> for super::Chat {
        fn from(chat: Chat) -> Self {
            let (known, budget) = Self::of_one_agent(&chat.turns);
            Self {
                id: chat.id,
                cols: chat.cols,
                rows: chat.rows,
                title: chat.title,
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns: chat.turns,
                settings: chat.settings,
                chosen: chat.chosen,
                agent: chat.agent,
                relations: chat.relations,
                known,
                parked: Vec::new(),
                switches: Vec::new(),
                budget,
                plan: Vec::new(),
                plan_by: None,
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 14, as the builds that wrote it wrote it: no reviews.
mod v14 {
    use serde::Deserialize;

    use super::v15::Chat;
    use super::{Applied, Buffer, EditLog, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 13, as the builds that wrote it wrote it: no windows' changes.
mod v13 {
    use serde::Deserialize;

    use super::v15::Chat;
    use super::{Buffer, EditLog, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 12, as the builds that wrote it wrote it: chats with nothing
/// of other chats, and no panes showing boards. Version 11 shares its
/// chats.
mod v12 {
    use serde::Deserialize;

    use super::{Buffer, ChatSetting, ChatTurn, EditLog, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        id: u32,
        cols: u16,
        rows: u16,
        title: String,
        cwd: String,
        session: Option<String>,
        failure: Option<String>,
        turns: Vec<ChatTurn>,
        settings: Vec<ChatSetting>,
        chosen: Vec<(String, String)>,
        agent: String,
    }

    impl From<Chat> for super::Chat {
        fn from(chat: Chat) -> Self {
            let (known, budget) = Self::of_one_agent(&chat.turns);
            Self {
                id: chat.id,
                cols: chat.cols,
                rows: chat.rows,
                title: chat.title,
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns: chat.turns,
                settings: chat.settings,
                chosen: chat.chosen,
                agent: chat.agent,
                relations: super::ChatRelations::default(),
                known,
                parked: Vec::new(),
                switches: Vec::new(),
                budget,
                plan: Vec::new(),
                plan_by: None,
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 10, as the builds that wrote it wrote it: chats without their
/// agent, as there was only ever Claude Code. Older versions share its
/// chats.
mod v10 {
    use serde::Deserialize;

    use super::{Buffer, ChatSetting, ChatTurn, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cols: u16,
        pub(super) rows: u16,
        pub(super) title: String,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) failure: Option<String>,
        pub(super) turns: Vec<ChatTurn>,
        pub(super) settings: Vec<ChatSetting>,
        pub(super) chosen: Vec<(String, String)>,
    }

    impl From<Chat> for super::Chat {
        fn from(chat: Chat) -> Self {
            let (known, budget) = Self::of_one_agent(&chat.turns);
            Self {
                id: chat.id,
                cols: chat.cols,
                rows: chat.rows,
                title: chat.title,
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns: chat.turns,
                settings: chat.settings,
                chosen: chat.chosen,
                agent: crate::agents::DEFAULT.to_owned(),
                relations: super::ChatRelations::default(),
                known,
                parked: Vec::new(),
                switches: Vec::new(),
                budget,
                plan: Vec::new(),
                plan_by: None,
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
                lenders: Vec::new(),
                sharing: Vec::new(),
                key: None,
                keys: Vec::new(),
                background: Vec::new(),
                kept: Vec::new(),
                trusted: Vec::new(),
                exes: Vec::new(),
                restored: None,
                chat_previews: Vec::new(),
                tool_previews: Vec::new(),
                image_originals: Vec::new(),
                chat_queues: Vec::new(),
                chat_continuations: Vec::new(),
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image layer's store, as handed over: an image held by a chat
    /// and a picture pane, one let go, which image a chat's thumbnail
    /// handle names, and the picture pane.
    fn images() -> Images {
        Images {
            dir: b"/run/user/1000/clackatoo/images".to_vec(),
            kept: vec![KeptImage {
                id: [2; 16],
                format: 3,
                width: 640,
                height: 480,
                bytes: 4096,
                owners: vec![
                    ImageOwner::Terminal(2),
                    ImageOwner::Picture(1),
                    ImageOwner::Buffer(3),
                ],
            }],
            dropped: vec![([3; 16], 64 << 20)],
            chats: vec![ChatImages {
                chat: 2,
                handles: vec![(1, [2; 16])],
            }],
            pictures: vec![PictureShown {
                id: 1,
                path: b"/home/me/plan.png".to_vec(),
                image: Some([2; 16]),
            }],
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one state, whole, each part of it filled"
    )]
    fn state() -> State {
        State {
            graphics: vec![Graphics {
                terminal: 1,
                graphics: graphics(),
            }],
            direct_asks: vec!["server".into()],
            forwards: vec![Forwarded {
                host: "desk".into(),
                local: 3000,
                to: "localhost".into(),
                port: 8080,
                listeners: vec![4, 5],
            }],
            marks: vec![Marks {
                terminal: 1,
                phone: true,
                vs16: true,
                graphemes: true,
                margins: Some((2, 7)),
            }],
            images: Some(images()),
            histories: vec![History {
                terminal: 1,
                off: true,
                from: 3,
                numbered: (4, 80),
                hints: vec![(5, 6)],
                restarted: Some((2, 1_794_000_000)),
            }],
            notices: vec![ChatNotices {
                chat: 2,
                turns: vec![(1, 1_792_000_000_000, vec![FINISHED.into()])],
                inbox: vec![(0, vec![FINISHED.into()])],
            }],
            asked: vec![Asked {
                terminal: 1,
                asked: asked(),
            }],
            layout: Some(LayoutExtras {
                zoomed: vec![(1, 2)],
                closed: vec![
                    r#"{"pane":{"workspace":1,"tab":1,"place":{"panes":[2],"side":"right","share":500},"pane":{"terminal":{"cwd":"/src","ran":"make"}}}}"#.into(),
                ],
            }),
            mcp: Some(Mcp {
                key: [5; 32],
                sessions: vec![
                    (3, Some("s1".into()), "ab.cd".into()),
                    (3, None, "ef.01".into()),
                ],
            }),
            federation: Some(relay::Federation {
                window: 71,
                changes: 42,
                next_board: 100,
                active: Some(1),
                typed_parked: vec![([9; 16], 5)],
                peers: vec![relay::Peer {
                    host: "desk".into(),
                    command: "clackatoo".into(),
                    asked: true,
                    unshared: false,
                    expected: Some([9; 16]),
                    wire: Some(125),
                    view: include_bytes!("../tests/fixtures/federation-view-v1.bin").to_vec(),
                    buffers: vec![relay::Buffer {
                        local: 5,
                        applied: vec![(71, 40, Some(11))],
                        pending: Some((42, 71, 41)),
                    }],
                }],
            }),
            agent_terminals: Some(vec![AgentCommand {
                command: LiveCommanded {
                    terminal: 2,
                    owner: Some(3),
                    name: Some("tests".into()),
                    typing: None,
                    failed: None,
                    expect: Some(4),
                    expect_first: false,
                    renumbered: 0,
                },
                retained: true,
                close_left: None,
            }]),
            listener: 0,
            lock: Some(1),
            next_terminal: 3,
            terminals: Vec::new(),
            chats: vec![Chat {
                id: 2,
                cols: 40,
                rows: 10,
                title: "fixing the build".to_owned(),
                cwd: "/src".to_owned(),
                session: Some("s".to_owned()),
                failure: None,
                turns: vec![ChatTurn {
                    prompt: "why?".to_owned(),
                    sent: 1,
                    items: vec![
                        ChatItem::Words("Because".to_owned()),
                        ChatItem::Tool {
                            id: "t".to_owned(),
                            title: "Read".to_owned(),
                            status: ToolStatus::Done,
                            kind: ToolKind::Edit,
                            content: vec![
                                ToolContent::Text("for".to_owned()),
                                ToolContent::Output("ran\n".to_owned()),
                                ToolContent::Diff {
                                    path: "/src/a".to_owned(),
                                    old: None,
                                    new: "b".to_owned(),
                                },
                            ],
                            locations: vec![ToolLocation {
                                path: "/src/a".to_owned(),
                                line: Some(4),
                            }],
                        },
                    ],
                    end: Some(ChatEnd {
                        at: 2,
                        outcome: ChatOutcome::Failed("no".to_owned()),
                    }),
                }],
                settings: vec![ChatSetting {
                    id: "mode".to_owned(),
                    name: "Mode".to_owned(),
                    kind: SettingKind::Mode,
                    value: "auto".to_owned(),
                    choices: vec![ChatChoice {
                        value: "auto".to_owned(),
                        name: "Auto".to_owned(),
                        description: None,
                    }],
                }],
                chosen: vec![("model".to_owned(), "haiku".to_owned())],
                agent: "codex".to_owned(),
                relations: ChatRelations {
                    parent: Some(5),
                    root: Some(4),
                    depth: 2,
                    hops: 1,
                    cause: Some((4, 7)),
                    senders: vec![(0, sender())],
                    inbox: vec![ChatMessage {
                        from: sender(),
                        text: "next".to_owned(),
                        hops: 2,
                        cause: Some((4, 7)),
                    }],
                },
                known: 1,
                parked: vec![ParkedSession {
                    agent: "claude".to_owned(),
                    title: "Claude Code".to_owned(),
                    session: Some("c".to_owned()),
                    known: 1,
                    chosen: vec![("mode".to_owned(), "plan".to_owned())],
                    settings: Vec::new(),
                }],
                switches: vec![ChatSwitch {
                    turn: 1,
                    from: "Claude Code".to_owned(),
                    to: "Codex".to_owned(),
                    model: Some("6 Luna".to_owned()),
                    resumed: false,
                    notes: vec!["Mode Plan doesn't carry over".to_owned()],
                }],
                budget: 9000,
                plan: vec![PlanStep {
                    content: "test it".to_owned(),
                    state: StepState::Doing,
                }],
                plan_by: Some("Codex".to_owned()),
            }],
            orphans: vec![Process { pid: 42, pidfd: 2 }],
            workspaces: Workspaces {
                list: vec![
                    Workspace {
                        id: 1,
                        name: "clackatoo".to_owned(),
                        tabs: vec![Tab {
                            id: 1,
                            layout: Layout::Split {
                                left_to_right: true,
                                parts: vec![
                                    Layout::Pane { id: 1, terminal: 1 },
                                    Layout::Pane { id: 2, terminal: 2 },
                                    Layout::Buffer { id: 3, buffer: 1 },
                                    Layout::Files { id: 4, files: 1 },
                                    Layout::Board { id: 5, board: 3 },
                                    Layout::Review { id: 6, review: 1 },
                                    Layout::Browser { id: 7, chat: 2 },
                                    Layout::Folder { id: 8, files: 2 },
                                    Layout::Borrowed {
                                        id: 9,
                                        machine: [0xd5; 16],
                                        key: 0x0123_4567_89ab_cdef,
                                        kind: "terminal".to_owned(),
                                    },
                                ],
                                shares: vec![2, 1, 1, 1, 1, 1, 1, 1, 1],
                            },
                            focused: 2,
                            name: None,
                        }],
                        active_tab: 1,
                    },
                    Workspace {
                        id: 2,
                        name: String::new(),
                        tabs: vec![Tab {
                            id: 2,
                            layout: Layout::Buffer { id: 10, buffer: 1 },
                            focused: 10,
                            name: None,
                        }],
                        active_tab: 2,
                    },
                ],
                active: Some(1),
                next_workspace: 3,
                next_tab: 3,
                next_pane: 11,
            },
            next_buffer: 2,
            buffers: vec![buffer()],
            next_files: 3,
            explorers: vec![
                Files {
                    id: 1,
                    root: b"/src".to_vec(),
                },
                Files {
                    id: 2,
                    root: b"/src/sub".to_vec(),
                },
            ],
            edits: edits(),
            applied: vec![Applied {
                buffer: 1,
                window: 0xfeed,
                seq: 7,
                landed: Some(3),
            }],
            daemon: 0xd43,
            reviews: reviews(),
            recovery_lock: Some(3),
            shells: vec![Shell {
                terminal: 1,
                secret: "0123456789abcdef0123456789abcdef".to_owned(),
                file: Some(b"/run/clackatoo/shell/secret.1".to_vec()),
                trusted: Some(4),
            }],
            machine: Some([0xa7; 16]),
            connections: vec![Connection {
                host: "jack@laptop:2222".to_owned(),
                command: "~/.cargo/bin/clackatoo".to_owned(),
            }],
            lent: vec![Lent {
                workspace: 2,
                to: [0xa1; 16],
                key: 7,
                placed: true,
            }],
            borrowers: vec![Borrower {
                machine: [0xa1; 16],
                name: "jack-ubuntu-laptop".to_owned(),
                last_seen: 1_792_000_000,
            }],
            lenders: vec![Lender {
                machine: [0xd5; 16],
                name: "jack-ubuntu-server".to_owned(),
            }],
            sharing: vec![Sharing {
                host: "jack@laptop:2222".to_owned(),
                unshared: true,
                machine: Some([0xa1; 16]),
            }],
            key: Some([0x4b; 32]),
            keys: vec![Known {
                machine: [0xa1; 16],
                key: [0x5c; 32],
                met: true,
            }],
            background: vec![Background {
                workspace: 1,
                tab: Tab {
                    id: 3,
                    layout: Layout::Split {
                        left_to_right: false,
                        parts: vec![
                            Layout::Pane {
                                id: 11,
                                terminal: 3,
                            },
                            Layout::Pane {
                                id: 12,
                                terminal: 4,
                            },
                        ],
                        shares: vec![1, 1],
                    },
                    focused: 12,
                    name: Some("server".to_owned()),
                },
                failed: Some(137),
            }],
            kept: vec![Kept {
                pane: 12,
                command: "trail serve".to_owned(),
                always: true,
                given_up: false,
                paused: true,
                restart: Some(Restarting {
                    paused_during: Some(5),
                    ours: Some((4, 1)),
                    running: Some(5),
                    handled: Some(3),
                    quick: 2,
                    wait_ms: 4000,
                    due_in_ms: None,
                    ended: None,
                    started_ms_ago: Some(1500),
                }),
            }],
            trusted: vec![Trusted {
                terminal: 4,
                blocks: vec![2, 5],
            }],
            exes: vec![Exe {
                terminal: 4,
                dev: 0x803,
                ino: 1_234_567,
            }],
            activities: b"[1,[]]".to_vec(),
            lent_background: vec![LentBackground {
                workspace: 4,
                failed: Some(2),
            }],
            undelivered: vec![Undelivered {
                from: 2,
                to: 3,
                why: "chat 3 has moved".to_owned(),
            }],
            talk: Some(Talk {
                tasks: vec![ChatTasks {
                    chat: 2,
                    session: Some("s2".to_owned()),
                    tasks: vec![ChatTask {
                        id: "t1".to_owned(),
                        kind: "shell".to_owned(),
                        title: "Serve the docs".to_owned(),
                        command: Some("npm run dev".to_owned()),
                        state: "running".to_owned(),
                        summary: None,
                        started: 5,
                        ended: None,
                        can_stop: true,
                    }],
                }],
                woke: vec![WokeTurn {
                    chat: 2,
                    turn: 1,
                    why: "the gate ended".to_owned(),
                }],
                heard: vec![Heard {
                    chat: 2,
                    by: 3,
                    turns: 1,
                }],
                starts: vec![Starts { root: 3, count: 4 }],
                notice_turns: vec![NoticeTurns {
                    root: 3,
                    since: Some(0),
                    count: 100,
                }],
                held: vec![Held {
                    chat: 3,
                    since: None,
                }],
                waits: vec![CutWait {
                    waiter: 3,
                    chats: vec![2],
                    first: true,
                }],
                stopped: vec![4],
            }),
            live: Some(live()),
            nested: vec![NestedItem {
                chat: 2,
                turn: 0,
                item: 1,
                parent: "toolu_task".to_owned(),
            }],
            drafts: vec![ChatDraft {
                chat: 2,
                text: "and then the docs".to_owned(),
            }],
            uids: vec![ChatUids {
                chat: 2,
                next: 9,
                uids: vec![0, 7],
            }],
            branches: vec![ChatBranches {
                chat: 2,
                segs: vec![
                    Some(BranchSegment {
                        parent: None,
                        start: 0,
                        len: 1,
                        held: None,
                        stub: None,
                        session: None,
                        created: 1,
                        used: 1,
                        summary: false,
                    }),
                    Some(BranchSegment {
                        parent: Some(0),
                        start: 1,
                        len: 1,
                        held: Some(HeldTurns {
                            uids: vec![3],
                            turns: vec![],
                            cards: vec![],
                        }),
                        stub: None,
                        session: Some(BranchSessionRecord {
                            agent: "claude".to_owned(),
                            session: Some("s-left".to_owned()),
                            known: 2,
                            chosen: vec![],
                        }),
                        created: 2,
                        used: 3,
                        summary: false,
                    }),
                    None,
                    Some(BranchSegment {
                        parent: Some(0),
                        start: 1,
                        len: 2,
                        held: None,
                        stub: Some(StubTurns {
                            first: "try the other way".to_owned(),
                            marks: vec![7, 8],
                            uids: vec![4, 5],
                            cards: vec![],
                        }),
                        session: Some(BranchSessionRecord {
                            agent: "claude".to_owned(),
                            session: Some("s-stub".to_owned()),
                            known: 3,
                            chosen: vec![],
                        }),
                        created: 4,
                        used: 4,
                        summary: false,
                    }),
                ],
                current: 0,
                pending: Some(1),
                dropped: vec![(0, 1, 2)],
                plan: Some(ForkPlanRecord {
                    from: "s-left".to_owned(),
                    message: Some("msg_01".to_owned()),
                    known: 1,
                    sent: true,
                }),
                gone: vec!["s-gone".to_owned()],
                undone: vec![3],
                lost_cards: vec![5],
            }],
            forks: vec![ChatForks {
                chat: 2,
                points: vec![(0, "s-left".to_owned(), "msg_01".to_owned())],
            }],
            undo_states: vec![BufferUndo {
                buffer: 1,
                authors: vec![AuthorUndo {
                    undo: vec![
                        Some(UndoState {
                            cursor: (4, 2),
                            taken: true,
                            visual: Some(UndoVisual {
                                start: (4, 0),
                                end: (9, 3),
                                kind: 1,
                                backward: false,
                                to_end: false,
                            }),
                        }),
                        None,
                    ],
                    redo: Vec::new(),
                    forgot: true,
                    undo_empty: vec![false, true],
                    redo_empty: Vec::new(),
                }],
            }],
            chat_continuations: vec![ChatContinuation {
                chat: 2,
                turn: 0,
                pending: Some(ResumePrompt {
                    role: None,
                    text: "not sent yet".into(),
                    images: Vec::new(),
                }),
                queue_paused: false,
            }],
            chat_queues: vec![ChatQueue {
                chat: 2,
                next: 9,
                messages: vec![QueuedChatMessage {
                    id: 9,
                    text: "next".into(),
                    images: Vec::new(),
                    target: Some(QueuedTarget {
                        agent: "claude".into(),
                        model: Some("sonnet".into()),
                        budget: 4096,
                    }),
                }],
            }],
            image_originals: vec![ImageOriginals {
                chat: 2,
                next: 7,
                images: vec![(7, "cG5n".into())],
                references: vec![(0, Some("image".into()), 0, 7)],
            }],
            tool_previews: vec![ToolPreviews {
                chat: 2,
                turn: 0,
                tool: "image".into(),
                images: vec![ChatPreview {
                    width: 1,
                    height: 1,
                    rgba: vec![20, 40, 60, 255],
                }],
            }],
            chat_previews: vec![ChatPreviews {
                chat: 2,
                turns: vec![(
                    0,
                    vec![ChatPreview {
                        width: 1,
                        height: 1,
                        rgba: vec![10, 20, 30, 255],
                    }],
                )],
            }],
            restored: Some(vec![
                Restored {
                    pane: 11,
                    ran: Some("claude --resume".to_owned()),
                    place: None,
                },
                Restored {
                    pane: 2,
                    ran: None,
                    place: Some((40, 8, 30)),
                },
            ]),
        }
    }

    fn reviews() -> Reviews {
        let hunk = |id, reviewed| AgentHunk {
            id,
            start: 0,
            end: 4,
            base: "old\n".to_owned(),
            chat: 2,
            turn: 0,
            reviewed,
        };
        Reviews {
            next_hunk: 3,
            buffers: vec![(1, vec![hunk(1, false)])],
            closed: vec![ClosedHunks {
                path: b"/src/b".to_vec(),
                stamp: Some(DiskStamp {
                    seconds: 6,
                    nanoseconds: 7,
                    len: 8,
                    inode: 9,
                    device: 10,
                }),
                hunks: vec![hunk(2, true)],
            }],
            made: vec![(1, "new\n".to_owned()), (2, "made\n".to_owned())],
            next_comment: 1,
            comments: vec![Comment {
                chat: 2,
                id: 0,
                path: b"/src/a".to_vec(),
                line: 1,
                at: Some((1, 0)),
                text: "why?".to_owned(),
            }],
            next_review: 2,
            list: vec![Review {
                id: 1,
                chat: Some(2),
                scope: ReviewScope::Turn(0),
                root: b"/src".to_vec(),
            }],
            last_chat: Some(2),
        }
    }

    fn edits() -> EditLog {
        EditLog {
            chats: vec![ChatEdits {
                chat: 2,
                dropped: vec![(0, 3)],
                turns: vec![TurnEdits {
                    turn: 1,
                    files: vec![
                        FileEdits {
                            path: b"/src/a".to_vec(),
                            tool: Some("t1".to_owned()),
                            existed: true,
                            exists: true,
                            known: true,
                            undone: false,
                            changes: vec![FileChange::Buffer {
                                buffer: 1,
                                version: 1,
                                bytes: 6,
                            }],
                        },
                        FileEdits {
                            path: b"/src/b".to_vec(),
                            tool: None,
                            existed: false,
                            exists: true,
                            known: true,
                            undone: true,
                            changes: vec![FileChange::Text {
                                after: "made\n".to_owned(),
                                edit: vec![EditOp::Insert("made\n".to_owned())],
                            }],
                        },
                    ],
                }],
            }],
        }
    }

    fn buffer() -> Buffer {
        Buffer {
            id: 1,
            path: b"/src/a".to_vec(),
            dir: b"/src".to_vec(),
            text: "new".to_owned(),
            saved: Some("old".to_owned()),
            disk: Some(DiskStamp {
                seconds: 1,
                nanoseconds: 2,
                len: 3,
                inode: 4,
                device: 5,
            }),
            history: BufferHistory {
                first: 0,
                edits: vec![(
                    vec![
                        EditOp::Delete("old".to_owned()),
                        EditOp::Insert("new".to_owned()),
                    ],
                    Author::User,
                )],
                steps: vec![AuthorSteps {
                    author: Author::User,
                    version: 1,
                    undo: vec![vec![
                        EditOp::Insert("old".to_owned()),
                        EditOp::Delete("new".to_owned()),
                    ]],
                    redo: Vec::new(),
                }],
            },
        }
    }

    #[test]
    fn a_state_reads_back_as_written() {
        let mut file = tempfile::tempfile().unwrap();
        state().write(&mut file).unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(format!("{read:?}"), format!("{:?}", state()));
    }

    fn sender() -> ChatSender {
        ChatSender {
            chat: 5,
            agent: "Claude Code".to_owned(),
            title: "parent".to_owned(),
        }
    }

    /// Written by a build that wrote version 13. It must always be read.
    #[test]
    fn version_13_is_read_with_each_chats_relations_and_panes_showing_boards() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v13.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let relations = &state.chats[0].relations;
        assert_eq!(
            (
                relations.parent,
                relations.root,
                relations.depth,
                relations.hops
            ),
            (Some(5), Some(4), 2, 1)
        );
        assert_eq!(relations.cause, Some((4, 7)));
        assert_eq!(relations.senders[0].1.title, "parent");
        assert_eq!(relations.inbox[0].text, "next");
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[4], Layout::Board { id: 5, board: 3 }));
        assert_eq!(state.edits.chats[0].chat, 2);
    }

    /// Written by a build that wrote version 15. It must always be read.
    #[test]
    fn version_15_is_read_with_the_reviews() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v15.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let reviews = &state.reviews;
        assert_eq!(reviews.next_hunk, 3);
        let (buffer, hunks) = &reviews.buffers[0];
        assert_eq!((*buffer, hunks[0].id, hunks[0].reviewed), (1, 1, false));
        assert_eq!(hunks[0].base, "old\n");
        let closed = &reviews.closed[0];
        assert_eq!(closed.path, b"/src/b");
        assert!(closed.hunks[0].reviewed);
        assert_eq!(reviews.made[1], (2, "made\n".to_owned()));
        assert_eq!(reviews.comments[0].text, "why?");
        assert!(matches!(
            reviews.list[0],
            Review {
                id: 1,
                chat: Some(2),
                scope: ReviewScope::Turn(0),
                ..
            }
        ));
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[5], Layout::Review { id: 6, review: 1 }));
        assert_eq!(state.edits.chats[0].chat, 2);
    }

    /// What goes on through an upgrade with an agent, each part filled.
    #[allow(clippy::too_many_lines, reason = "each part of it filled")]
    fn live() -> Live {
        Live {
            agents: vec![LiveAgent {
                id: 3,
                name: "claude".to_owned(),
                command: vec![b"/bin/claude-agent-acp".to_vec()],
                process: Process { pid: 77, pidfd: 9 },
                stdin: 10,
                stdout: 11,
                stderr: 12,
                outgoing: b"{\"id\":5}\n".to_vec(),
                incoming: b"{\"jsonrpc\":\"2.0\",\"me".to_vec(),
                errors: b"half a li".to_vec(),
                last_errors: vec!["warning".to_owned()],
                login_required: false,
                next_id: 6,
                pending: vec![
                    (4, LivePending::Prompt(2)),
                    (
                        5,
                        LivePending::Session {
                            terminal: 4,
                            resumed: Some("s4".to_owned()),
                            load: false,
                        },
                    ),
                ],
                waited: vec![(4, 60_000), (5, 150_000)],
                can_close: true,
                can_load: true,
                can_image: true,
                can_steer: true,
                can_list: Some(true),
                tasks: true,
                client: "{\"terminal\":false}".to_owned(),
            }],
            next_agent: 4,
            chats: vec![LiveChat {
                chat: 2,
                link: LiveLink::Open,
                queued: None,
                stop_requested: false,
                slow: false,
                heard: Some(1),
                woke_by_task: None,
                woken_stopping: false,
                wake_held: Some(300),
                own_tools: vec!["toolu_1".to_owned()],
                diffs: vec![(
                    "t".to_owned(),
                    vec![LiveDiff {
                        path: "/src/a".to_owned(),
                        old: None,
                        new: "b".to_owned(),
                    }],
                )],
                asking: vec![LiveQuestion {
                    asker: LiveAsker::Agent("\"p1\"".to_owned()),
                    id: 7,
                    title: "Write notes.txt".to_owned(),
                    choices: vec![(
                        "allow".to_owned(),
                        "Allow".to_owned(),
                        LiveChoice::AllowOnce,
                    )],
                    own: false,
                    plan: Some("1. Fix it".to_owned()),
                }],
                next_question: 8,
                native: Some((1, 0, 2)),
                restart: None,
                queue_paused: false,
            }],
            tools: vec![LiveConnection {
                stream: 13,
                chat: 2,
                listed: true,
                incoming: b"{\"jsonrpc\"".to_vec(),
                outgoing: Vec::new(),
                browser: false,
            }],
            waiting: vec![LiveWait {
                connection: 0,
                id: "3".to_owned(),
                chat: 2,
                what: LiveWaiting::Chats {
                    chats: vec![4],
                    first: true,
                    left: 1000,
                },
            }],
            next_call: 9,
            opening: vec![LiveOpening {
                chat: 4,
                waited: 20,
                left: 29_980,
                fetching: false,
            }],
            parking: vec![LiveDue {
                chat: 5,
                left: 60_000,
            }],
            woken: Vec::new(),
            commanded: vec![LiveCommanded {
                terminal: 6,
                owner: Some(2),
                name: Some("deploy".to_owned()),
                typing: None,
                failed: None,
                expect: Some(3),
                expect_first: false,
                renumbered: 0,
            }],
            settings: vec![("claude".to_owned(), Vec::new())],
            context: vec![("claude".to_owned(), 200_000)],
            limits: vec![LiveLimits {
                agent: "claude".to_owned(),
                five_hour: Some((42, Some(1_760_018_400_000))),
                weekly: None,
            }],
        }
    }

    #[test]
    fn version_41_keeps_the_numbers_that_stay_with_chats_turns() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v41.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.uids.len(), 1);
        assert_eq!(
            (read.uids[0].chat, read.uids[0].next, &read.uids[0].uids[..]),
            (2, 9, &[0, 7][..])
        );
        let branches = &read.branches[0];
        assert_eq!((branches.segs.len(), branches.pending), (4, Some(1)));
        let stub = branches.segs[3].as_ref().unwrap().stub.as_ref().unwrap();
        assert_eq!(
            (stub.first.as_str(), &stub.uids[..]),
            ("try the other way", &[4, 5][..])
        );
        let left = branches.segs[1].as_ref().unwrap();
        assert_eq!(left.held.as_ref().unwrap().uids, [3]);
        assert_eq!(
            left.session.as_ref().unwrap().session.as_deref(),
            Some("s-left")
        );
        assert!(branches.plan.as_ref().unwrap().sent);
        assert_eq!(
            (&branches.undone[..], &branches.lost_cards[..]),
            (&[3][..], &[5][..])
        );
        assert_eq!(read.forks[0].points[0].2, "msg_01");
        assert_eq!(read.drafts.len(), 1);
    }

    #[test]
    fn version_40_keeps_what_was_typed_in_chats_boxes() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v40.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.drafts.len(), 1);
        assert_eq!(
            (read.drafts[0].chat, read.drafts[0].text.as_str()),
            (2, "and then the docs")
        );
        assert_eq!(read.nested.len(), 1);
    }

    #[test]
    fn version_39_keeps_which_items_subagents_did() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v39.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let [nested] = &read.nested[..] else {
            panic!("{:?}", read.nested);
        };
        assert_eq!((nested.chat, nested.turn, nested.item), (2, 0, 1));
        assert_eq!(nested.parent, "toolu_task");
        assert!(read.talk.is_some() && read.live.is_some());
    }

    #[test]
    fn version_38_keeps_the_agents_that_go_on() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v38.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let live = read.live.expect("kept");
        let agent = &live.agents[0];
        assert_eq!((agent.id, agent.name.as_str()), (3, "claude"));
        assert_eq!((agent.process.pid, agent.stdout), (77, 11));
        assert_eq!(agent.incoming, b"{\"jsonrpc\":\"2.0\",\"me");
        assert!(matches!(agent.pending[0], (4, LivePending::Prompt(2))));
        assert_eq!((agent.can_list, agent.next_id), (Some(true), 6));
        assert_eq!(agent.waited, [(4, 60_000), (5, 150_000)]);
        let chat = &live.chats[0];
        assert_eq!((chat.chat, chat.link), (2, LiveLink::Open));
        assert!(matches!(&chat.asking[0].asker, LiveAsker::Agent(id) if id == "\"p1\""));
        assert_eq!(live.tools[0].stream, 13);
        assert!(matches!(
            live.waiting[0].what,
            LiveWaiting::Chats { first: true, .. }
        ));
        assert_eq!((live.next_agent, live.next_call), (4, 9));
        assert_eq!(live.commanded[0].expect, Some(3));
        assert_eq!(agent.client, "{\"terminal\":false}");
        assert_eq!(chat.asking[0].plan.as_deref(), Some("1. Fix it"));
        assert_eq!(live.context, [("claude".to_owned(), 200_000)]);
        assert_eq!(
            live.limits[0].five_hour,
            Some((42, Some(1_760_018_400_000)))
        );
        assert!(read.talk.is_some());
    }

    #[test]
    fn version_37_keeps_where_chats_stand_among_the_others() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v37.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let talk = read.talk.expect("kept");
        assert_eq!(talk.tasks[0].tasks[0].title, "Serve the docs");
        assert_eq!(talk.tasks[0].session.as_deref(), Some("s2"));
        assert_eq!(
            (talk.woke[0].turn, talk.woke[0].why.as_str()),
            (1, "the gate ended")
        );
        assert_eq!(
            (talk.heard[0].chat, talk.heard[0].by, talk.heard[0].turns),
            (2, 3, 1)
        );
        assert_eq!((talk.starts[0].root, talk.starts[0].count), (3, 4));
        assert_eq!(talk.notice_turns[0].count, 100);
        assert_eq!(talk.held[0].chat, 3);
        assert!(talk.waits[0].first);
        assert_eq!(talk.stopped, [4]);
        assert_eq!(
            (talk.waits[0].waiter, talk.waits[0].chats.as_slice()),
            (3, &[2][..])
        );
        assert_eq!(read.undelivered.len(), 1);
        // Its agents all stopped.
        assert!(read.live.is_none());
    }

    #[test]
    fn version_36_keeps_why_messages_were_dropped() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v36.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let [dropped] = &read.undelivered[..] else {
            panic!("{:?}", read.undelivered);
        };
        assert_eq!((dropped.from, dropped.to), (2, 3));
        assert_eq!(dropped.why, "chat 3 has moved");
        assert_eq!(read.lent_background.len(), 1);
        // Where chats stood among the others it couldn't hand over.
        assert!(read.talk.is_none());
    }

    #[test]
    fn version_35_keeps_the_workspaces_lent_as_background_tabs() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v35.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let [lent] = &read.lent_background[..] else {
            panic!("{:?}", read.lent_background);
        };
        assert_eq!((lent.workspace, lent.failed), (4, Some(2)));
        assert!(!read.undo_states.is_empty());
    }

    #[test]
    fn version_34_keeps_what_undo_steps_keep() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v34.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let undo = &read.undo_states[0];
        assert_eq!(undo.buffer, 1);
        let author = &undo.authors[0];
        assert!(author.forgot);
        let state = author.undo[0].unwrap();
        assert_eq!((state.cursor, state.taken), ((4, 2), true));
        let visual = state.visual.unwrap();
        assert_eq!((visual.start, visual.end, visual.kind), ((4, 0), (9, 3), 1));
        assert!(author.undo[1].is_none());
        assert_eq!(author.undo_empty, [false, true]);
        assert!(author.redo.is_empty());
    }

    #[test]
    fn version_33_keeps_the_operation_journal() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v33.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.activities, b"[1,[]]");
        assert!(!read.chat_continuations.is_empty());
        assert!(read.undo_states.is_empty(), "steps kept nothing");
    }

    #[test]
    fn version_32_keeps_explicit_upgrade_continuations() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v32.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let continuation = &read.chat_continuations[0];
        assert_eq!((continuation.chat, continuation.turn), (2, 0));
        assert_eq!(continuation.pending.as_ref().unwrap().text, "not sent yet");
        assert!(!continuation.queue_paused);
    }

    #[test]
    fn version_31_keeps_pending_messages_and_provider_choices() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v31.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert!(read.chat_continuations.is_empty());
        let queue = &read.chat_queues[0];
        assert_eq!((queue.chat, queue.next), (2, 9));
        assert_eq!(queue.messages[0].text, "next");
        assert_eq!(queue.messages[0].target.as_ref().unwrap().budget, 4096);
    }

    #[test]
    fn version_30_keeps_originals_and_their_thumbnail_references() {
        assert_eq!(READABLE, (1..=VERSION).collect::<Vec<_>>());
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v30.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let originals = &read.image_originals[0];
        assert_eq!((originals.chat, originals.next), (2, 7));
        assert_eq!(originals.images, vec![(7, "cG5n".into())]);
        assert_eq!(originals.references, vec![(0, Some("image".into()), 0, 7)]);
    }

    #[test]
    fn version_29_keeps_tool_images_and_reads_older_previews() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v29.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.tool_previews[0].chat, 2);
        assert_eq!(read.tool_previews[0].tool, "image");
        assert_eq!(read.tool_previews[0].images[0].rgba, [20, 40, 60, 255]);
        assert_eq!(read.chat_previews[0].turns[0].1[0].rgba, [10, 20, 30, 255]);
    }

    #[test]
    fn version_28_keeps_preview_pixels_without_changing_older_handovers() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v28.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert!(read.tool_previews.is_empty());
        assert_eq!(read.chat_previews[0].chat, 2);
        assert_eq!(read.chat_previews[0].turns[0].1[0].rgba, [10, 20, 30, 255]);
        let mut old = tempfile::tempfile().unwrap();
        old.write_all(include_bytes!("../tests/fixtures/handover-v27.bin"))
            .unwrap();
        assert!(State::read(&mut old).unwrap().chat_previews.is_empty());
    }

    #[test]
    fn version_27_is_read_with_what_panes_made_again_still_have_to_show() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v27.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let Some([offer, place]) = state.restored.as_deref() else {
            panic!("{:?}", state.restored);
        };
        assert_eq!(
            (offer.pane, offer.ran.as_deref(), offer.place),
            (11, Some("claude --resume"), None)
        );
        assert_eq!(
            (place.pane, place.ran.as_deref(), place.place),
            (2, None, Some((40, 8, 30)))
        );
        assert_eq!(state.kept[0].command, "trail serve");
    }

    #[test]
    fn version_26_is_read_with_the_terminals_kept_running() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v26.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let kept = &state.kept[0];
        assert_eq!(
            (kept.pane, kept.command.as_str(), kept.always, kept.given_up),
            (12, "trail serve", true, false)
        );
        assert!(kept.paused);
        let restart = kept.restart.as_ref().unwrap();
        assert_eq!(
            (
                restart.paused_during,
                restart.ours,
                restart.quick,
                restart.wait_ms
            ),
            (Some(5), Some((4, 1)), 2, 4000)
        );
        let exe = &state.exes[0];
        assert_eq!((exe.terminal, exe.dev, exe.ino), (4, 0x803, 1_234_567));
        let trusted = &state.trusted[0];
        assert_eq!(
            (trusted.terminal, trusted.blocks.as_slice()),
            (4, &[2, 5][..])
        );
        assert_eq!(state.background[0].tab.id, 3);
        // What panes made again had to show wasn't handed over.
        assert!(state.restored.is_none());
    }

    #[test]
    fn version_25_is_read_with_background_tabs() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v25.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let background = &state.background[0];
        assert_eq!(
            (background.workspace, background.tab.id, background.failed),
            (1, 3, Some(137))
        );
        let Layout::Split { parts, .. } = &background.tab.layout else {
            panic!("not split");
        };
        assert!(matches!(
            parts[1],
            Layout::Pane {
                id: 12,
                terminal: 4
            }
        ));
        assert_eq!(background.tab.focused, 12);
        // No terminal was kept running.
        assert!(state.kept.is_empty());
    }

    #[test]
    fn version_24_is_read_with_connections_not_passed_on_and_their_machines() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v24.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let sharing = &state.sharing[0];
        assert_eq!(
            (sharing.host.as_str(), sharing.unshared, sharing.machine),
            ("jack@laptop:2222", true, Some([0xa1; 16]))
        );
        assert_eq!(state.key, Some([0x4b; 32]));
        let known = &state.keys[0];
        assert_eq!(
            (known.machine, known.key, known.met),
            ([0xa1; 16], [0x5c; 32], true)
        );
        // There were no background tabs.
        assert!(state.background.is_empty());
    }

    #[test]
    fn version_23_is_read_with_placed_workspaces_and_lenders_names() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v23.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let lent = &state.lent[0];
        assert_eq!(
            (lent.workspace, lent.to, lent.key, lent.placed),
            (2, [0xa1; 16], 7, true)
        );
        let lender = &state.lenders[0];
        assert_eq!(
            (lender.machine, lender.name.as_str()),
            ([0xd5; 16], "jack-ubuntu-server")
        );
        // Every connection was passed on, and none's machine kept, nor any
        // key.
        assert!(state.sharing.is_empty() && state.key.is_none() && state.keys.is_empty());
    }

    #[test]
    fn version_22_is_read_with_borrowed_panes_and_lent_workspaces() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v22.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(
            &parts[8],
            Layout::Borrowed { id: 9, machine: [0xd5, ..], key: 0x0123_4567_89ab_cdef, kind }
                if kind == "terminal"
        ));
        let lent = &state.lent[0];
        assert_eq!((lent.workspace, lent.to, lent.key), (2, [0xa1; 16], 7));
        assert!(!lent.placed && state.lenders.is_empty());
        let borrower = &state.borrowers[0];
        assert_eq!(
            (borrower.machine, borrower.name.as_str(), borrower.last_seen),
            ([0xa1; 16], "jack-ubuntu-laptop", 1_792_000_000)
        );
    }

    #[test]
    fn version_21_is_read_with_the_machine_and_its_connections() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v21.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!(state.machine, Some([0xa7; 16]));
        let connection = &state.connections[0];
        assert_eq!(
            (connection.host.as_str(), connection.command.as_str()),
            ("jack@laptop:2222", "~/.cargo/bin/clackatoo")
        );
        assert_eq!(state.shells[0].trusted, Some(4));
        assert!(state.lent.is_empty());
    }

    #[test]
    fn version_20_is_read_with_the_shells_secrets() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v20.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let shell = &state.shells[0];
        assert_eq!(
            (shell.terminal, shell.secret.as_str(), shell.trusted),
            (1, "0123456789abcdef0123456789abcdef", Some(4))
        );
        assert_eq!(
            shell.file.as_deref(),
            Some(&b"/run/clackatoo/shell/secret.1"[..])
        );
        assert_eq!(state.recovery_lock, Some(3));
        assert!(state.machine.is_none() && state.connections.is_empty());
    }

    #[test]
    fn version_19_is_read_with_the_lock_on_the_recovery_files_and_no_secrets() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v19.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!(state.recovery_lock, Some(3));
        assert_eq!(state.daemon, 0xd43);
        assert!(state.shells.is_empty());
    }

    #[test]
    fn version_18_is_read_with_panes_showing_folders() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v18.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[7], Layout::Folder { id: 8, files: 2 }));
        assert_eq!(state.workspaces.next_pane, 9);
        assert_eq!(state.explorers[1].root, b"/src/sub");
    }

    #[test]
    fn version_17_is_read_with_panes_showing_chats_pages() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v17.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[6], Layout::Browser { id: 7, chat: 2 }));
        assert_eq!(state.workspaces.next_pane, 8);
    }

    #[test]
    fn version_16_is_read_with_chats_that_moved_between_agents() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v16.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.chats[0];
        assert_eq!(
            (chat.agent.as_str(), chat.known, chat.budget),
            ("codex", 1, 9000)
        );
        let parked = &chat.parked[0];
        assert_eq!(
            (
                parked.agent.as_str(),
                parked.session.as_deref(),
                parked.known
            ),
            ("claude", Some("c"), 1)
        );
        assert_eq!(parked.chosen, [("mode".to_owned(), "plan".to_owned())]);
        let switch = &chat.switches[0];
        assert_eq!(
            (switch.turn, switch.to.as_str(), switch.model.as_deref()),
            (1, "Codex", Some("6 Luna"))
        );
        assert_eq!(switch.notes, ["Mode Plan doesn't carry over"]);
        assert!(matches!(chat.plan[0].state, StepState::Doing));
        assert_eq!(chat.plan_by.as_deref(), Some("Codex"));
        assert_eq!(chat.relations.inbox[0].text, "next");
    }

    /// Before version 16, a chat's session had seen every turn, and it had
    /// no other.
    #[test]
    fn version_15_comes_with_each_session_having_seen_every_turn() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v15.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.chats[0];
        assert_eq!(chat.known, 1);
        assert!(chat.parked.is_empty() && chat.switches.is_empty());
        assert_eq!(chat.relations.parent, Some(5));
    }

    #[test]
    fn version_14_comes_with_no_reviews() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v14.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert!(state.reviews.buffers.is_empty() && state.reviews.list.is_empty());
    }

    /// Version 12 comes with chats that know nothing of other chats.
    #[test]
    fn version_12_comes_with_no_relations() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v12.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert!(
            state
                .chats
                .iter()
                .all(|chat| chat.relations.parent.is_none() && chat.relations.inbox.is_empty())
        );
    }

    /// What a program that asked for kitty's keyboard flags, pushed its
    /// title and said how far it had got asked of a terminal.
    fn asked() -> AskedSnapshot {
        let mut terminal = clackatoo_term::Terminal::new(
            clackatoo_term::Size::new(10, 2).unwrap(),
            &clackatoo_term::Config::default(),
        );
        terminal.advance(b"\x1b]2;shell\x07\x1b[22t\x1b[>5u\x1b]9;4;1;40\x07");
        terminal.asked_snapshot()
    }

    /// A terminal's images: one of kitty's, kept by ID 7 and shown.
    fn graphics() -> clackatoo_term::GraphicsSnapshot {
        let size = clackatoo_term::Size::new(20, 5).unwrap();
        let mut terminal = clackatoo_term::Terminal::new(size, &clackatoo_term::Config::default());
        terminal.advance_until_waiting(b"\x1b_Ga=T,f=100,i=7,q=2;AAAA\x1b\\");
        let token = terminal.take_image_requests()[0].token;
        let kept = clackatoo_term::ImageKept {
            key: [7; 16],
            width: 20,
            height: 40,
        };
        terminal.image_kept(token, Ok(kept));
        terminal.graphics_snapshot()
    }

    /// A chat a notice says finished, as the JSON a handover keeps it as.
    const FINISHED: &str = r#"{"chat":2,"agent":"Claude Code","profile":"explore","asked":"build it","started":1792000000000,"ended":1792000060000,"outcome":"Done","answer":"built it"}"#;

    /// Writes the fixture for the current version: run once, with
    /// `--ignored`, when a version is made, and never again.
    #[test]
    #[ignore = "writes tests/fixtures/handover-v53.bin"]
    fn write_the_current_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/handover-v53.bin"
        );
        let mut file = File::create(path).unwrap();
        state().write(&mut file).unwrap();
    }

    #[test]
    fn version_53_keeps_the_terminals_images() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v53.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.graphics.len(), 1);
        assert_eq!(read.graphics[0].terminal, 1);
        assert_eq!(read.graphics[0].graphics, graphics());
        assert_eq!(read.forwards.len(), 1);
        // Version 52 had none.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v52.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.graphics.is_empty());
        assert_eq!(old.forwards.len(), 1);
    }

    #[test]
    fn version_52_keeps_the_ports_forwarded_to_other_machines() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v52.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let [forwarded] = &read.forwards[..] else {
            panic!("{:?}", read.forwards);
        };
        assert_eq!(
            (
                forwarded.host.as_str(),
                forwarded.local,
                forwarded.to.as_str()
            ),
            ("desk", 3000, "localhost")
        );
        assert_eq!(
            (forwarded.port, &forwarded.listeners[..]),
            (8080, &[4, 5][..])
        );
        assert_eq!(read.direct_asks, ["server"]);
        assert_eq!(read.marks.len(), 1);
        // Version 51 had none.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v51.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.forwards.is_empty() && old.direct_asks.is_empty());
        assert_eq!(old.marks.len(), 1);
    }

    #[test]
    fn version_51_keeps_the_terminals_marks() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v51.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.marks.len(), 1);
        assert_eq!((read.marks[0].terminal, read.marks[0].phone), (1, true));
        assert!(read.marks[0].vs16 && read.marks[0].graphemes);
        assert_eq!(read.marks[0].margins, Some((2, 7)));
        assert!(read.layout.is_some());
        // Version 50 had none.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v50.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.marks.is_empty());
        assert!(old.layout.is_some());
    }

    #[test]
    fn version_50_keeps_what_is_kept_of_terminals_history() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v50.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.histories, state().histories);
        // Version 49 had nothing of it.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v49.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert!(read.histories.is_empty());
        assert!(read.images.is_some());
    }

    #[test]
    fn version_49_keeps_the_image_layers_store() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v49.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let store = read.images.expect("the store");
        let expected = images();
        assert_eq!(store.dir, expected.dir);
        assert_eq!(store.kept.len(), 1);
        let kept = &store.kept[0];
        assert_eq!(
            (kept.id, kept.format, kept.width, kept.height, kept.bytes),
            ([2; 16], 3, 640, 480, 4096)
        );
        assert_eq!(kept.owners, expected.kept[0].owners);
        assert_eq!(store.dropped, expected.dropped);
        assert_eq!(store.chats[0].handles, vec![(1, [2; 16])]);
        assert_eq!(store.pictures[0].path, b"/home/me/plan.png");
        assert_eq!(store.pictures[0].image, Some([2; 16]));
        // Version 48 had none: its chats kept their images themselves.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v48.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.images.is_none());
        assert_eq!(old.notices.len(), 1);
    }

    #[test]
    fn version_48_keeps_what_notices_said_of_the_chats_that_finished() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v48.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let notices = &read.notices[0];
        assert_eq!(notices.chat, 2);
        assert_eq!(notices.turns[0].0, 1);
        assert_eq!(notices.inbox[0].0, 0);
        let finished: clackatoo_protocol::FinishedChat =
            serde_json::from_str(&notices.turns[0].2[0]).unwrap();
        assert_eq!(finished.chat, clackatoo_protocol::TerminalId(2));
        assert_eq!(finished.outcome, Some(clackatoo_protocol::Outcome::Done));
        assert_eq!(finished.answer, "built it");
        // Version 47 had none, and keeps what it had.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v47.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.notices.is_empty());
        assert!(old.layout.is_some());
    }

    #[test]
    fn version_47_keeps_tabs_zoomed_and_what_was_closed() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v47.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        let layout = read.layout.expect("version 47 has them");
        assert_eq!(layout.zoomed, [(1, 2)]);
        let closed: crate::saved::Closed = serde_json::from_str(&layout.closed[0]).unwrap();
        assert!(matches!(closed, crate::saved::Closed::Pane { tab: 1, .. }));
        // Version 46 had neither, and keeps what it had.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v46.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.layout.is_none());
        assert_eq!(old.asked.len(), 1);
    }

    #[test]
    fn version_46_keeps_what_programs_asked_of_terminals() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v46.bin"))
            .unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(read.asked.len(), 1);
        assert_eq!(read.asked[0].terminal, 1);
        assert_eq!(read.asked[0].asked, asked());
        let mut terminal = clackatoo_term::Terminal::new(
            clackatoo_term::Size::new(10, 2).unwrap(),
            &clackatoo_term::Config::default(),
        );
        terminal.restore_asked(&read.asked[0].asked);
        assert_eq!(terminal.keyboard(), 5);
        assert_eq!(
            terminal.progress(),
            Some(clackatoo_term::TaskProgress::Running(40))
        );
        // The title pushed comes back when it's popped.
        terminal.advance(b"\x1b[23t");
        assert_eq!(terminal.title(), "shell");
        // Version 45 had nothing of it.
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v45.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.asked.is_empty());
        assert!(old.mcp.is_some());
    }

    #[test]
    fn version_45_is_read_as_44_and_says_it_asked_before_servers_ran() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v45.bin"))
            .unwrap();
        let (state, version) = State::read_versioned(&mut file).unwrap();
        assert!(version >= ASKS_TRUST);
        assert_eq!(state.mcp.unwrap().key, [5; 32]);
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v44.bin"))
            .unwrap();
        let (_, version) = State::read_versioned(&mut file).unwrap();
        assert!(version < ASKS_TRUST, "a build that ran servers unasked");
    }

    #[test]
    fn version_44_keeps_the_mcp_servers_secrets() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v44.bin"))
            .unwrap();
        let mcp = State::read(&mut file).unwrap().mcp.unwrap();
        assert_eq!(mcp.key, [5; 32]);
        assert_eq!(mcp.sessions[0], (3, Some("s1".into()), "ab.cd".into()));
    }

    #[test]
    fn version_43_gave_no_mcp_server_secrets() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v43.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.mcp.is_none());
        assert!(old.federation.is_some());
    }

    #[test]
    fn version_43_keeps_federation_delivery_receipts() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v43.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let relay = state.federation.unwrap();
        assert_eq!((relay.window, relay.changes), (71, 42));
        assert_eq!(relay.peers[0].buffers[0].pending, Some((42, 71, 41)));
        assert_eq!(relay.peers[0].buffers[0].applied, vec![(71, 40, Some(11))]);
    }

    #[test]
    fn version_42_has_no_federation_checkpoint() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v42.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.federation.is_none());
        assert!(old.agent_terminals.is_some());
    }

    #[test]
    fn terminal_retention_is_carried_and_old_selection_history_is_unknown() {
        let mut file = tempfile::tempfile().unwrap();
        state().write(&mut file).unwrap();
        let read = State::read(&mut file).unwrap();
        let commands = read.agent_terminals.unwrap();
        assert!(commands[0].retained);
        assert_eq!(commands[0].command.owner, Some(3));
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v41.bin"))
            .unwrap();
        let old = State::read(&mut file).unwrap();
        assert!(old.agent_terminals.is_none());
    }

    #[test]
    fn other_files_and_versions_are_refused() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"not a handover").unwrap();
        assert!(matches!(
            State::read(&mut file),
            Err(HandoverError::NotAHandover)
        ));

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(MAGIC).unwrap();
        file.write_all(&(VERSION + 1).to_le_bytes()).unwrap();
        assert!(matches!(
            State::read(&mut file),
            Err(HandoverError::Version(version)) if version == VERSION + 1
        ));
    }

    /// Written by a build that wrote version 1. It must always be read.
    #[test]
    fn version_1_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v1.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (8, 99));
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.input.as_slice()), (7, &b"ls\r"[..]));
        assert_eq!(terminal.blocks, BlocksSnapshot::default());
        let screen =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap()
                .screen_text();
        assert_eq!(screen, "two\nthree\nfour");
        assert!(matches!(
            state.workspaces.list[0].tabs[0].layout,
            Layout::Split { ref shares, .. } if *shares == [2, 1]
        ));
    }

    /// Written by a build that wrote version 2. It must always be read.
    #[test]
    fn version_2_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v2.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (6, 98));
        assert!(state.chats.is_empty());
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.pid), (5, 77));
        let mut emulator =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap();
        emulator.restore_blocks(&terminal.blocks);
        let block = emulator.block(0, clackatoo_term::Which::At).unwrap();
        assert_eq!(block.command.as_deref(), Some("ls"));
    }

    /// Written by a build that wrote version 3. It must always be read.
    #[test]
    fn version_3_is_read_with_each_chat_as_its_text() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v3.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (4, 97));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.cols, chat.rows), (3, 30, 6));
        assert_eq!(
            (chat.cwd.as_str(), chat.session.as_deref()),
            ("/src/project", Some("s1"))
        );
        let [ChatTurn { prompt, items, .. }] = chat.turns.as_slice() else {
            panic!("one turn: {:?}", chat.turns);
        };
        assert!(prompt.is_empty());
        let [ChatItem::Words(text)] = items.as_slice() else {
            panic!("{items:?}");
        };
        assert_eq!(text, "```text\n› hi\n\necho: hi\n```");
    }

    /// Written by a build that wrote version 4. It must always be read.
    #[test]
    fn version_4_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v4.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (5, 96));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (4, Some("s4")));
        assert_eq!(chat.turns[0].prompt, "why?");
        assert!(chat.settings.is_empty() && chat.chosen.is_empty());
    }

    /// Written by a build that wrote version 5. It must always be read.
    #[test]
    fn version_5_is_read_with_tools_of_no_kind() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v5.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (6, 95));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (5, Some("s5")));
        assert_eq!(chat.chosen, [("model".to_owned(), "haiku".to_owned())]);
        let [
            _,
            ChatItem::Tool {
                title,
                kind,
                content,
                ..
            },
        ] = chat.turns[0].items.as_slice()
        else {
            panic!("{:?}", chat.turns[0].items);
        };
        assert_eq!(title, "Read");
        assert!(matches!(kind, ToolKind::Other) && content.is_empty());
    }

    /// Written by a build that wrote version 6. It must always be read.
    #[test]
    fn version_6_is_read_with_tools_working_nowhere() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v6.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (7, 94));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (6, Some("s6")));
        let [
            _,
            ChatItem::Tool {
                kind,
                content,
                locations,
                ..
            },
        ] = chat.turns[0].items.as_slice()
        else {
            panic!("{:?}", chat.turns[0].items);
        };
        assert!(matches!(kind, ToolKind::Edit) && content.len() == 2);
        assert!(locations.is_empty());
    }

    /// Written by a build that wrote version 7. It must always be read.
    #[test]
    fn version_7_is_read_with_no_buffers() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v7.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (8, 93));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (7, Some("s7")));
        let ChatItem::Tool { locations, .. } = &chat.turns[0].items[1] else {
            panic!("{:?}", chat.turns[0].items);
        };
        assert_eq!(locations[0].line, Some(4));
        assert!(state.buffers.is_empty());
        assert_eq!(state.next_buffer, 1);
    }

    /// Written by a build that wrote version 8. It must always be read.
    #[test]
    fn version_8_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v8.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (9, 94));
        assert_eq!(state.chats[0].session.as_deref(), Some("s8"));
        // From before there were other agents.
        assert_eq!(state.chats[0].agent, "claude");
        assert_eq!(
            (state.next_buffer, state.buffers[0].text.as_str()),
            (2, "new")
        );
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.pid), (1, 88));
        let mut emulator =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap();
        emulator.restore_blocks(&terminal.blocks);
        assert_eq!(emulator.screen_text(), "two\nthree\nfour");
        assert_eq!(emulator.primary().scrollback_len(), 1);
        assert_eq!(emulator.cwd(), Some("/v8"));
    }

    /// Written by a build that wrote version 9. It must always be read.
    #[test]
    fn version_9_is_read_with_each_terminals_links() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v9.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (10, 95));
        assert_eq!((state.next_files, state.explorers.len()), (1, 0));
        let terminal = &state.terminals[0];
        let emulator = clackatoo_term::Terminal::restore_with_links(
            &terminal.screen,
            &terminal.links,
            &clackatoo_term::Config::default(),
        )
        .unwrap();
        assert_eq!(emulator.screen_text(), "two\nthree\nfour");
        let uri = |row: &clackatoo_term::Row| {
            let id = row.style(row.cells()[0]).link?;
            Some(emulator.link(id)?.uri().to_owned())
        };
        let history = emulator.primary().scrollback().next().unwrap();
        assert_eq!(uri(&history).as_deref(), Some("https://a.example/"));
        let three = emulator.grid().row(1);
        assert_eq!(uri(three).as_deref(), Some("file:///v9/three"));
        assert_eq!(uri(emulator.grid().row(2)), None);
    }

    /// Written by a build that wrote version 10. It must always be read.
    #[test]
    fn version_10_is_read_with_its_explorers() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v10.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (11, 96));
        assert_eq!(state.next_files, 2);
        let explorer = &state.explorers[0];
        assert_eq!((explorer.id, explorer.root.as_slice()), (1, &b"/v10"[..]));
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[3], Layout::Files { id: 4, files: 1 }));
        assert_eq!(state.buffers[0].text, "new");
    }

    /// Written by a build that wrote version 11. It must always be read.
    #[test]
    fn version_11_is_read_with_each_chats_agent() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v11.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.chats[0];
        assert_eq!(
            (chat.agent.as_str(), chat.session.as_deref()),
            ("codex", Some("s"))
        );
        let ChatItem::Tool { content, .. } = &chat.turns[0].items[1] else {
            panic!("not a tool");
        };
        assert!(matches!(&content[1], ToolContent::Output(text) if text == "ran\n"));
        assert_eq!(
            (state.next_files, state.buffers[0].text.as_str()),
            (2, "new")
        );
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[3], Layout::Files { id: 4, files: 1 }));
    }

    /// Written by a build that wrote version 12. It must always be read.
    #[test]
    fn version_12_is_read_with_the_edit_log() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v12.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.edits.chats[0];
        assert_eq!((chat.chat, chat.dropped.as_slice()), (2, &[(0, 3)][..]));
        let files = &chat.turns[0].files;
        assert_eq!(chat.turns[0].turn, 1);
        assert!(matches!(
            files[0].changes[..],
            [FileChange::Buffer {
                buffer: 1,
                version: 1,
                bytes: 6
            }]
        ));
        assert_eq!(files[0].tool.as_deref(), Some("t1"));
        let FileChange::Text { after, edit } = &files[1].changes[0] else {
            panic!("not a change to a file not open");
        };
        assert_eq!(after, "made\n");
        assert!(matches!(&edit[..], [EditOp::Insert(made)] if made == "made\n"));
        assert!(!files[1].existed && files[1].exists && files[1].undone);
        assert_eq!(state.buffers[0].text, "new");
    }

    /// Versions before 12 come with an empty edit log.
    #[test]
    fn version_11_comes_with_no_edits() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v11.bin"))
            .unwrap();
        assert!(State::read(&mut file).unwrap().edits.chats.is_empty());
    }

    /// Written by a build that wrote version 14. It must always be read.
    #[test]
    fn version_14_is_read_with_each_windows_last_change() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v14.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!(
            state.applied,
            [Applied {
                buffer: 1,
                window: 0xfeed,
                seq: 7,
                landed: Some(3)
            }]
        );
        assert_eq!(state.daemon, 0xd43);
        assert_eq!(state.buffers[0].text, "new");
    }

    /// Versions before 14 come knowing no window's changes.
    #[test]
    fn version_13_comes_with_no_windows_changes() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v13.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert!(state.applied.is_empty());
        assert_eq!(state.daemon, 0, "none: the build reading it chooses one");
    }

    #[test]
    fn each_fd_is_taken_once() {
        let mut fds = Fds::default();
        let index = fds.add(tempfile::tempfile().unwrap().into());
        assert!(fds.take(index).is_ok());
        assert!(fds.take(index).is_err());
        assert!(fds.take(index + 1).is_err());
    }
}

/// Frozen queue payloads; image bytes never enter shared snapshots.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatQueue {
    pub chat: u32,
    pub next: u64,
    pub messages: Vec<QueuedChatMessage>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct QueuedChatMessage {
    pub id: u64,
    pub text: String,
    pub images: Vec<QueuedImage>,
    pub target: Option<QueuedTarget>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct QueuedImage {
    pub data: String,
    pub width: u32,
    pub height: u32,
    pub preview: Option<ChatPreview>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct QueuedTarget {
    pub agent: String,
    pub model: Option<String>,
    pub budget: u32,
}

/// A turn the upgrade interrupted, never inferred from a historical error.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatContinuation {
    pub chat: u32,
    pub turn: u64,
    /// A user prompt still waiting to be sent; otherwise continue the session.
    pub pending: Option<ResumePrompt>,
    pub queue_paused: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ResumePrompt {
    pub role: Option<String>,
    pub text: String,
    pub images: Vec<QueuedImage>,
}

/// Version 32, before durable managed activities. Keep this shape frozen.
mod v32 {
    use super::{
        Applied, Background, Borrower, Buffer, Chat, ChatContinuation, ChatPreviews, ChatQueue,
        Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, Lender, Lent,
        Process, Restored, Reviews, Sharing, Shell, Terminal, ToolPreviews, Trusted, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: Vec::new(),
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 33, before undo steps kept anything. Keep this shape frozen.
mod v33 {
    use super::{
        Applied, Background, Borrower, Buffer, Chat, ChatContinuation, ChatPreviews, ChatQueue,
        Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, Lender, Lent,
        Process, Restored, Reviews, Sharing, Shell, Terminal, ToolPreviews, Trusted, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: Vec::new(),
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 34, before workspaces lent as background tabs.
mod v34 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatContinuation, ChatPreviews,
        ChatQueue, Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known,
        Lender, Lent, Process, Restored, Reviews, Sharing, Shell, Terminal, ToolPreviews, Trusted,
        Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34.
        undo_states: Vec<BufferUndo>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: Vec::new(),
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 35, before dropped messages' reasons were handed over. Keep
/// this shape frozen.
mod v35 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatContinuation, ChatPreviews,
        ChatQueue, Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known,
        Lender, Lent, LentBackground, Process, Restored, Reviews, Sharing, Shell, Terminal,
        ToolPreviews, Trusted, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34.
        undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs, since version 35.
        lent_background: Vec<LentBackground>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: state.lent_background,
                undelivered: Vec::new(),
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 36, before what chats ran in the background, which answers
/// each had, and where agents talking to each other stood were handed
/// over. Keep this shape frozen.
mod v36 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatContinuation, ChatPreviews,
        ChatQueue, Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known,
        Lender, Lent, LentBackground, Process, Restored, Reviews, Sharing, Shell, Terminal,
        ToolPreviews, Trusted, Undelivered, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34.
        undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs, since version 35.
        lent_background: Vec<LentBackground>,
        /// Why waiting messages were dropped, since version 36.
        undelivered: Vec<Undelivered>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: state.lent_background,
                undelivered: state.undelivered,
                talk: None,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 37, before agents' processes went on through upgrades. Keep
/// this shape frozen. It's version 36, before what chats ran in the background, which answers
/// each had, and where agents talking to each other stood were handed
/// over. Keep this shape frozen.
mod v37 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatContinuation, ChatPreviews,
        ChatQueue, Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known,
        Lender, Lent, LentBackground, Process, Restored, Reviews, Sharing, Shell, Talk, Terminal,
        ToolPreviews, Trusted, Undelivered, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34.
        undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs, since version 35.
        lent_background: Vec<LentBackground>,
        /// Why waiting messages were dropped, since version 36.
        undelivered: Vec<Undelivered>,
        /// Where chats stand among the others, since version 37.
        talk: Option<Talk>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: state.lent_background,
                undelivered: state.undelivered,
                talk: state.talk,
                live: None,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 38, before which items of chats' turns an agent's own
/// subagents did were handed over. Keep this shape frozen.
mod v38 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatContinuation, ChatPreviews,
        ChatQueue, Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known,
        Lender, Lent, LentBackground, Live, Process, Restored, Reviews, Sharing, Shell, Talk,
        Terminal, ToolPreviews, Trusted, Undelivered, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34.
        undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs, since version 35.
        lent_background: Vec<LentBackground>,
        /// Why waiting messages were dropped, since version 36.
        undelivered: Vec<Undelivered>,
        /// Where chats stand among the others, since version 37.
        talk: Option<Talk>,
        /// The agents that go on, since version 38.
        live: Option<Live>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: state.lent_background,
                undelivered: state.undelivered,
                talk: state.talk,
                live: state.live,
                nested: Vec::new(),
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

mod v39 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatContinuation, ChatPreviews,
        ChatQueue, Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known,
        Lender, Lent, LentBackground, Live, NestedItem, Process, Restored, Reviews, Sharing, Shell,
        Talk, Terminal, ToolPreviews, Trusted, Undelivered, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34.
        undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs, since version 35.
        lent_background: Vec<LentBackground>,
        /// Why waiting messages were dropped, since version 36.
        undelivered: Vec<Undelivered>,
        /// Where chats stand among the others, since version 37.
        talk: Option<Talk>,
        /// The agents that go on, since version 38.
        live: Option<Live>,
        /// The items agents' own subagents did, since version 39.
        nested: Vec<NestedItem>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: state.lent_background,
                undelivered: state.undelivered,
                talk: state.talk,
                live: state.live,
                nested: state.nested,
                drafts: Vec::new(),
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

mod v40 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatContinuation, ChatDraft,
        ChatPreviews, ChatQueue, Connection, Deserialize, EditLog, Exe, Files, ImageOriginals,
        Kept, Known, Lender, Lent, LentBackground, Live, NestedItem, Process, Restored, Reviews,
        Sharing, Shell, Talk, Terminal, ToolPreviews, Trusted, Undelivered, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34.
        undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs, since version 35.
        lent_background: Vec<LentBackground>,
        /// Why waiting messages were dropped, since version 36.
        undelivered: Vec<Undelivered>,
        /// Where chats stand among the others, since version 37.
        talk: Option<Talk>,
        /// The agents that go on, since version 38.
        live: Option<Live>,
        /// The items agents' own subagents did, since version 39.
        nested: Vec<NestedItem>,
        /// What was typed in chats' boxes, since version 40.
        drafts: Vec<ChatDraft>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: state.lent_background,
                undelivered: state.undelivered,
                talk: state.talk,
                live: state.live,
                nested: state.nested,
                drafts: state.drafts,
                uids: Vec::new(),
                branches: Vec::new(),
                forks: Vec::new(),
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Agent-created terminal lifecycle, including ownership even when its agent stopped.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentCommand {
    pub(crate) command: LiveCommanded,
    pub(crate) retained: bool,
    /// Remaining grace period in milliseconds. New work resets it.
    pub(crate) close_left: Option<u64>,
}

/// The exact version 41 handover, before terminal retention.
mod v41 {
    use super::{
        Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches, ChatContinuation,
        ChatDraft, ChatForks, ChatPreviews, ChatQueue, ChatUids, Connection, Deserialize, EditLog,
        Exe, Files, ImageOriginals, Kept, Known, Lender, Lent, LentBackground, Live, NestedItem,
        Process, Restored, Reviews, Sharing, Shell, Talk, Terminal, ToolPreviews, Trusted,
        Undelivered, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        lock: Option<u32>,
        /// The ID the next terminal gets.
        next_terminal: u32,
        terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        orphans: Vec<Process>,
        workspaces: Workspaces,
        /// The ID the next buffer gets.
        next_buffer: u32,
        /// Files being edited.
        buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        next_files: u32,
        /// File explorers.
        explorers: Vec<Files>,
        /// What agents changed in files.
        edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        forks: Vec<ChatForks>,
    }
    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: state.lent,
                borrowers: state.borrowers,
                lenders: state.lenders,
                sharing: state.sharing,
                key: state.key,
                keys: state.keys,
                background: state.background,
                kept: state.kept,
                trusted: state.trusted,
                exes: state.exes,
                restored: state.restored,
                chat_previews: state.chat_previews,
                tool_previews: state.tool_previews,
                image_originals: state.image_originals,
                chat_queues: state.chat_queues,
                chat_continuations: state.chat_continuations,
                activities: state.activities,
                undo_states: state.undo_states,
                lent_background: state.lent_background,
                undelivered: state.undelivered,
                talk: state.talk,
                live: state.live,
                nested: state.nested,
                drafts: state.drafts,
                uids: state.uids,
                branches: state.branches,
                forks: state.forks,
                agent_terminals: None,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 42, before federated views and edit receipts survived upgrades.
mod v42 {
    use super::{
        AgentCommand, Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatPreviews, ChatQueue, ChatUids, Connection,
        Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, Lender, Lent,
        LentBackground, Live, NestedItem, Process, Restored, Reviews, Sharing, Shell, Talk,
        Terminal, ToolPreviews, Trusted, Undelivered, Workspaces,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: None,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 43, before the secrets given chats' MCP servers were kept.
mod v43 {
    use super::{
        AgentCommand, Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatPreviews, ChatQueue, ChatUids, Connection,
        Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, Lender, Lent,
        LentBackground, Live, NestedItem, Process, Restored, Reviews, Sharing, Shell, Talk,
        Terminal, ToolPreviews, Trusted, Undelivered, Workspaces, relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: None,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Versions 44 and 45, before what programs asked of terminals was kept.
mod v45 {
    use super::{
        AgentCommand, Applied, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatPreviews, ChatQueue, ChatUids, Connection,
        Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, Lender, Lent,
        LentBackground, Live, Mcp, NestedItem, Process, Restored, Reviews, Sharing, Shell, Talk,
        Terminal, ToolPreviews, Trusted, Undelivered, Workspaces, relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against,
        /// since version 44.
        pub(super) mcp: Option<Mcp>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: Vec::new(),
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 46, before tabs' layouts kept what they keep beside their
/// splits.
mod v46 {
    use super::{
        AgentCommand, Applied, Asked, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatPreviews, ChatQueue, ChatUids, Connection,
        Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, Lender, Lent,
        LentBackground, Live, Mcp, NestedItem, Process, Restored, Reviews, Sharing, Shell, Talk,
        Terminal, ToolPreviews, Trusted, Undelivered, Workspaces, relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against,
        /// since version 44.
        pub(super) mcp: Option<Mcp>,
        /// What programs asked of the terminals that asked anything beyond
        /// their modes, since version 46.
        pub(super) asked: Vec<Asked>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: s.asked,
                layout: None,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

mod v47 {
    use super::{
        AgentCommand, Applied, Asked, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatPreviews, ChatQueue, ChatUids, Connection,
        Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, LayoutExtras, Lender, Lent,
        LentBackground, Live, Mcp, NestedItem, Process, Restored, Reviews, Sharing, Shell, Talk,
        Terminal, ToolPreviews, Trusted, Undelivered, Workspaces, relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against,
        /// since version 44.
        pub(super) mcp: Option<Mcp>,
        /// What programs asked of the terminals that asked anything beyond
        /// their modes, since version 46.
        pub(super) asked: Vec<Asked>,
        /// What tabs' layouts keep beside their splits, since version 47.
        pub(super) layout: Option<LayoutExtras>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: s.asked,
                layout: s.layout,
                notices: Vec::new(),
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// Version 48, before the image layer's store was handed over.
mod v48 {
    use super::{
        AgentCommand, Applied, Asked, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatNotices, ChatPreviews, ChatQueue, ChatUids,
        Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Kept, Known, LayoutExtras,
        Lender, Lent, LentBackground, Live, Mcp, NestedItem, Process, Restored, Reviews, Sharing,
        Shell, Talk, Terminal, ToolPreviews, Trusted, Undelivered, Workspaces, relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against,
        /// since version 44.
        pub(super) mcp: Option<Mcp>,
        /// What programs asked of the terminals that asked anything beyond
        /// their modes, since version 46.
        pub(super) asked: Vec<Asked>,
        /// What tabs' layouts keep beside their splits, since version 47.
        pub(super) layout: Option<LayoutExtras>,
        /// What notices said of the chats that finished, since version 48.
        pub(super) notices: Vec<ChatNotices>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: s.asked,
                layout: s.layout,
                notices: s.notices,
                images: None,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// The exact version 49 handover, before what's kept of terminals'
/// history across a restart.
mod v49 {
    use super::{
        AgentCommand, Applied, Asked, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatNotices, ChatPreviews, ChatQueue, ChatUids,
        Connection, Deserialize, EditLog, Exe, Files, ImageOriginals, Images, Kept, Known,
        LayoutExtras, Lender, Lent, LentBackground, Live, Mcp, NestedItem, Process, Restored,
        Reviews, Sharing, Shell, Talk, Terminal, ToolPreviews, Trusted, Undelivered, Workspaces,
        relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against, and
        /// which went with which session, since version 44. None from older
        /// builds, which gave none.
        pub(super) mcp: Option<Mcp>,
        /// What programs asked of the terminals that asked anything beyond
        /// their modes: see [`Asked`]. Since version 46; before, nothing was
        /// kept.
        pub(super) asked: Vec<Asked>,
        /// What tabs' layouts keep beside their splits: see [`LayoutExtras`].
        /// Since version 47; `None` before.
        pub(super) layout: Option<LayoutExtras>,
        /// What notices from clackatoo said of the chats that finished, of the
        /// chats that have any: see [`ChatNotices`]. Since version 48; before,
        /// none was kept.
        pub(super) notices: Vec<ChatNotices>,
        /// The image layer's store (see [`Images`]), whose images' bytes are
        /// in files of their own. Since version 49; `None` before, from a
        /// build whose chats kept their images themselves.
        pub(super) images: Option<Images>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: s.asked,
                layout: s.layout,
                notices: s.notices,
                images: s.images,
                histories: Vec::new(),
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// The exact version 50 handover, before the terminals' marks.
mod v50 {
    use super::{
        AgentCommand, Applied, Asked, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatNotices, ChatPreviews, ChatQueue, ChatUids,
        Connection, Deserialize, EditLog, Exe, Files, History, ImageOriginals, Images, Kept, Known,
        LayoutExtras, Lender, Lent, LentBackground, Live, Mcp, NestedItem, Process, Restored,
        Reviews, Sharing, Shell, Talk, Terminal, ToolPreviews, Trusted, Undelivered, Workspaces,
        relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against, and
        /// which went with which session, since version 44. None from older
        /// builds, which gave none.
        pub(super) mcp: Option<Mcp>,
        /// What programs asked of the terminals that asked anything beyond
        /// their modes: see [`Asked`]. Since version 46; before, nothing was
        /// kept.
        pub(super) asked: Vec<Asked>,
        /// What tabs' layouts keep beside their splits: see [`LayoutExtras`].
        /// Since version 47; `None` before.
        pub(super) layout: Option<LayoutExtras>,
        /// What notices from clackatoo said of the chats that finished, of the
        /// chats that have any: see [`ChatNotices`]. Since version 48; before,
        /// none was kept.
        pub(super) notices: Vec<ChatNotices>,
        /// The image layer's store (see [`Images`]), whose images' bytes are
        /// in files of their own. Since version 49; `None` before, from a
        /// build whose chats kept their images themselves.
        pub(super) images: Option<Images>,
        /// What's kept of the terminals' history across a restart beyond
        /// their lines, since version 50.
        pub(super) histories: Vec<History>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: s.asked,
                layout: s.layout,
                notices: s.notices,
                images: s.images,
                histories: s.histories,
                marks: Vec::new(),
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

/// The exact version 51 handover, before the ports forwarded to other
/// machines.
mod v51 {
    use super::{
        AgentCommand, Applied, Asked, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatNotices, ChatPreviews, ChatQueue, ChatUids,
        Connection, Deserialize, EditLog, Exe, Files, History, ImageOriginals, Images, Kept, Known,
        LayoutExtras, Lender, Lent, LentBackground, Live, Marks, Mcp, NestedItem, Process,
        Restored, Reviews, Sharing, Shell, Talk, Terminal, ToolPreviews, Trusted, Undelivered,
        Workspaces, relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against, and
        /// which went with which session, since version 44. None from older
        /// builds, which gave none.
        pub(super) mcp: Option<Mcp>,
        /// What programs asked of the terminals that asked anything beyond
        /// their modes: see [`Asked`]. Since version 46; before, nothing was
        /// kept.
        pub(super) asked: Vec<Asked>,
        /// What tabs' layouts keep beside their splits: see [`LayoutExtras`].
        /// Since version 47; `None` before.
        pub(super) layout: Option<LayoutExtras>,
        /// What notices from clackatoo said of the chats that finished, of the
        /// chats that have any: see [`ChatNotices`]. Since version 48; before,
        /// none was kept.
        pub(super) notices: Vec<ChatNotices>,
        /// The image layer's store (see [`Images`]), whose images' bytes are
        /// in files of their own. Since version 49; `None` before, from a
        /// build whose chats kept their images themselves.
        pub(super) images: Option<Images>,
        /// What's kept of the terminals' history across a restart beyond
        /// their lines, since version 50.
        pub(super) histories: Vec<History>,
        /// What the user marked the terminals with, since version 51.
        pub(super) marks: Vec<Marks>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: s.asked,
                layout: s.layout,
                notices: s.notices,
                images: s.images,
                histories: s.histories,
                marks: s.marks,
                forwards: Vec::new(),
                direct_asks: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }
}

mod v52 {
    use super::{
        AgentCommand, Applied, Asked, Background, Borrower, Buffer, BufferUndo, Chat, ChatBranches,
        ChatContinuation, ChatDraft, ChatForks, ChatNotices, ChatPreviews, ChatQueue, ChatUids,
        Connection, Deserialize, EditLog, Exe, Files, Forwarded, History, ImageOriginals, Images,
        Kept, Known, LayoutExtras, Lender, Lent, LentBackground, Live, Marks, Mcp, NestedItem,
        Process, Restored, Reviews, Sharing, Shell, Talk, Terminal, ToolPreviews, Trusted,
        Undelivered, Workspaces, relay,
    };
    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        /// The listening socket.
        pub(super) listener: u32,
        /// The runtime directory's lock, set by whoever holds it.
        pub(super) lock: Option<u32>,
        /// The ID the next terminal gets.
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        /// Chats with agents, which are terminals too: their IDs are
        /// terminals' IDs.
        pub(super) chats: Vec<Chat>,
        /// Closed terminals' programs that haven't exited yet, and agents that
        /// were stopped.
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
        /// The ID the next buffer gets.
        pub(super) next_buffer: u32,
        /// Files being edited.
        pub(super) buffers: Vec<Buffer>,
        /// The ID the next file explorer gets.
        pub(super) next_files: u32,
        /// File explorers.
        pub(super) explorers: Vec<Files>,
        /// What agents changed in files.
        pub(super) edits: EditLog,
        /// The last change each window sent each buffer. Since version 14.
        pub(super) applied: Vec<Applied>,
        /// The daemon's own number, chosen at random when it first started:
        /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
        /// Since version 14; 0 before, for the build reading it to choose one.
        pub(super) daemon: u64,
        /// What agents changed that the user hasn't reviewed, and reviews.
        pub(super) reviews: Reviews,
        /// The lock on the recovery files, if the daemon keeps them (see
        /// `crate::recovery`). Since version 19.
        pub(super) recovery_lock: Option<u32>,
        /// The secrets of the terminals' shells that have one. Since version
        /// 20; before, no shell had one.
        pub(super) shells: Vec<Shell>,
        /// Which machine the daemon is: its ID (see
        /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
        /// `None` before, for the build reading it to take the one its state
        /// directory keeps.
        pub(super) machine: Option<[u8; 16]>,
        /// The daemon's connections to the daemons on other machines. Since
        /// version 21; before, there were none.
        pub(super) connections: Vec<Connection>,
        /// The workspaces lent to the daemons on other machines. Since version
        /// 22; before, there were none.
        pub(super) lent: Vec<Lent>,
        /// The machines they're lent to. Since version 22.
        pub(super) borrowers: Vec<Borrower>,
        /// What the machines the panes borrowed are on are called. Since
        /// version 23; before, none were known.
        pub(super) lenders: Vec<Lender>,
        /// Which of the connections aren't passed on, and the machine each
        /// was last connected to, by host. Since version 24; before, every
        /// one was passed on, and none's machine was known.
        pub(super) sharing: Vec<Sharing>,
        /// The seed of the machine's identity key (see `crate::machine`).
        /// Since version 24; before, the next build reads it from the
        /// `machine` file, or chooses one.
        pub(super) key: Option<[u8; 32]>,
        /// The identity keys of other machines the daemon knows. Since version
        /// 24; before, none were known.
        pub(super) keys: Vec<Known>,
        /// The workspaces' background tabs. Since version 25; before, there
        /// were none.
        pub(super) background: Vec<Background>,
        /// The terminals the user marked to keep running. Since version 26;
        /// before, none was. How running each again was going isn't handed
        /// over: it starts again.
        pub(super) kept: Vec<Kept>,
        /// Each terminal's blocks whose commands its shell's integration said,
        /// with its secret, so the command a shell ran last is still known to
        /// be its own: [`Shell::trusted`] says only the latest block's. Since
        /// version 26; before, only that one was.
        pub(super) trusted: Vec<Trusted>,
        /// The program each terminal was started as, so one its shell has
        /// become another program in, as with `exec`, is still known to have
        /// no shell to type into. Since version 26; before, none was known.
        pub(super) exes: Vec<Exe>,
        /// What panes made again as the daemon last started afresh still have
        /// to show for it (see
        /// [`Workspaces::restored`](clackatoo_protocol::Workspaces::restored)).
        /// Since version 27: `None` from a build before, which couldn't hand
        /// them over, as distinct from one that had none to.
        pub(super) restored: Option<Vec<Restored>>,
        /// Small image previews retained by chat and turn, since version 28.
        pub(super) chat_previews: Vec<ChatPreviews>,
        /// Thumbnails attached to tools, since version 29.
        pub(super) tool_previews: Vec<ToolPreviews>,
        /// Compressed originals and thumbnail handles, since version 30.
        pub(super) image_originals: Vec<ImageOriginals>,
        /// Pending drafts, resumed paused unless continuing a turn, since version 31.
        pub(super) chat_queues: Vec<ChatQueue>,
        /// Only work interrupted by this upgrade, since version 32.
        pub(super) chat_continuations: Vec<ChatContinuation>,
        /// Versioned managed-operation records, since version 33.
        pub(super) activities: Vec<u8>,
        /// What the buffers' undo steps keep, since version 34; before, they
        /// kept nothing, and none changed nothing.
        pub(super) undo_states: Vec<BufferUndo>,
        /// The workspaces lent as background tabs of the daemons they're lent
        /// to, and the failure of each not seen there yet. Since version 35;
        /// before, none was.
        pub(super) lent_background: Vec<LentBackground>,
        /// Messages chats sent others that waited their turns and were
        /// dropped as they came, so each sender is still told why. Since
        /// version 36; before, none was.
        pub(super) undelivered: Vec<Undelivered>,
        /// Where chats stand among the others: see [`Talk`]. Since version
        /// 37; `None` before, from a build that couldn't hand it over.
        pub(super) talk: Option<Talk>,
        /// The agents' processes that go on through the upgrade, and what
        /// goes on with them: see [`Live`]. Since version 38; `None` before,
        /// and from a build that stopped its agents, as it was told to, or as
        /// the build it upgraded to couldn't take them.
        pub(super) live: Option<Live>,
        /// The items of chats' turns that an agent's own subagents did: see
        /// [`NestedItem`]. Since version 39; before, none was.
        pub(super) nested: Vec<NestedItem>,
        /// The messages being typed in chats' boxes and not sent yet. Since
        /// version 40; before, none was kept.
        pub(super) drafts: Vec<ChatDraft>,
        /// The numbers that stay with chats' turns, of the chats whose turns
        /// aren't each numbered by their index: see [`ChatUids`]. Since
        /// version 41; before, every turn was.
        pub(super) uids: Vec<ChatUids>,
        /// The branches of chats' conversations, of the chats that have been
        /// cut back: see [`ChatBranches`]. Since version 41.
        pub(super) branches: Vec<ChatBranches>,
        /// Where chats' agent sessions can be forked: see [`ChatForks`].
        /// Since version 41; before, none was known.
        pub(super) forks: Vec<ChatForks>,
        /// Command ownership and permanent selection protection, since 42.
        /// None from older builds: selection history is unknown, so retain all.
        pub(super) agent_terminals: Option<Vec<AgentCommand>>,
        /// Federated views and edit receipts, since version 43.
        pub(super) federation: Option<relay::Federation>,
        /// What the secrets given chats' MCP servers are checked against, and
        /// which went with which session, since version 44. None from older
        /// builds, which gave none.
        pub(super) mcp: Option<Mcp>,
        /// What programs asked of the terminals that asked anything beyond
        /// their modes: see [`Asked`]. Since version 46; before, nothing was
        /// kept.
        pub(super) asked: Vec<Asked>,
        /// What tabs' layouts keep beside their splits: see [`LayoutExtras`].
        /// Since version 47; `None` before.
        pub(super) layout: Option<LayoutExtras>,
        /// What notices from clackatoo said of the chats that finished, of the
        /// chats that have any: see [`ChatNotices`]. Since version 48; before,
        /// none was kept.
        pub(super) notices: Vec<ChatNotices>,
        /// The image layer's store (see [`Images`]), whose images' bytes are
        /// in files of their own. Since version 49; `None` before, from a
        /// build whose chats kept their images themselves.
        pub(super) images: Option<Images>,
        /// What's kept of the terminals' history across a restart beyond
        /// their lines, since version 50.
        pub(super) histories: Vec<History>,
        /// What the user marked the terminals with, since version 51.
        pub(super) marks: Vec<Marks>,
        /// The ports forwarded to other machines, since version 52.
        pub(super) forwards: Vec<Forwarded>,
        /// The connections whose ssh straight there asks, since version 52.
        pub(super) direct_asks: Vec<String>,
    }
    impl From<State> for super::State {
        fn from(s: State) -> Self {
            Self {
                listener: s.listener,
                lock: s.lock,
                next_terminal: s.next_terminal,
                terminals: s.terminals,
                chats: s.chats,
                orphans: s.orphans,
                workspaces: s.workspaces,
                next_buffer: s.next_buffer,
                buffers: s.buffers,
                next_files: s.next_files,
                explorers: s.explorers,
                edits: s.edits,
                applied: s.applied,
                daemon: s.daemon,
                reviews: s.reviews,
                recovery_lock: s.recovery_lock,
                shells: s.shells,
                machine: s.machine,
                connections: s.connections,
                lent: s.lent,
                borrowers: s.borrowers,
                lenders: s.lenders,
                sharing: s.sharing,
                key: s.key,
                keys: s.keys,
                background: s.background,
                kept: s.kept,
                trusted: s.trusted,
                exes: s.exes,
                restored: s.restored,
                chat_previews: s.chat_previews,
                tool_previews: s.tool_previews,
                image_originals: s.image_originals,
                chat_queues: s.chat_queues,
                chat_continuations: s.chat_continuations,
                activities: s.activities,
                undo_states: s.undo_states,
                lent_background: s.lent_background,
                undelivered: s.undelivered,
                talk: s.talk,
                live: s.live,
                nested: s.nested,
                drafts: s.drafts,
                uids: s.uids,
                branches: s.branches,
                forks: s.forks,
                agent_terminals: s.agent_terminals,
                federation: s.federation,
                mcp: s.mcp,
                asked: s.asked,
                layout: s.layout,
                notices: s.notices,
                images: s.images,
                histories: s.histories,
                marks: s.marks,
                forwards: s.forwards,
                direct_asks: s.direct_asks,
                graphics: Vec::new(),
            }
        }
    }
}
