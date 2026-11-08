//! The pane the desk addresses, resolved once before reading its status or
//! moving focus or routing input. Folder listings can have both an editor and an explorer:
//! the editor is the view, but leaving it also ends the explorer's search.
//!
//! Read paths borrow the pane and its separately owned buffer directly.
//! Focus changes use a short-lived handle, never kept across updates. The
//! closed enum dispatch allocates nothing and covers every pane kind.

use std::ops::Range;

use clackatoo_protocol::{PaneId, TerminalId};

use super::{Desk, editor_mode_name, mode_name};
use crate::board::BoardPane;
use crate::browser::BrowserPane;
use crate::chat::ChatPane;
use crate::chrome::ModeName;
use crate::editor::{BufferCopy, Editor};
use crate::explorer::Explorer;
use crate::other::OtherPane;
use crate::pane::Pane;
use crate::review::Review;
use crate::vim::Mode;

#[derive(Clone, Copy, Debug)]
pub(super) enum PaneView<'a> {
    Editor(PaneId, &'a Editor, &'a BufferCopy),
    Explorer(PaneId, &'a Explorer),
    Board(PaneId, &'a BoardPane),
    Review(PaneId, &'a Review),
    Browser(&'a BrowserPane),
    Chat(TerminalId, &'a ChatPane),
    Terminal(TerminalId, &'a Pane),
    Other(&'a OtherPane),
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Focus {
    Editor(PaneId),
    Explorer(PaneId),
    Board(PaneId),
    Review(PaneId),
    Browser,
    Chat(TerminalId),
    Terminal(TerminalId),
    Other,
}

/// A pane's contribution to the common status bar. Window messages and
/// macro recording are added by the desk, equally for every pane kind.
#[derive(Debug, Default)]
pub(super) struct Bar {
    pub(super) label: Option<String>,
    pub(super) said: Option<String>,
    pub(super) prompting: bool,
    pub(super) selected: Option<Range<usize>>,
}

impl Desk {
    pub(super) fn focused_view(&self) -> Option<PaneView<'_>> {
        self.pane_view(self.shown?.pane)
    }

    pub(super) fn pane_view(&self, pane: PaneId) -> Option<PaneView<'_>> {
        // Editors, including a folder's editable listing, are resolved
        // without walking the workspace tree. Retain these references:
        // rendering the bar must not look them up again for each field.
        if let Some(editor) = self.editors.panes.get(&pane)
            && let Some(copy) = self.editors.buffers.get(&editor.buffer())
        {
            return Some(PaneView::Editor(pane, editor, copy));
        }
        if let Some(view) = self.explorer(pane) {
            return Some(PaneView::Explorer(pane, view));
        }
        if let Some(view) = self.board(pane) {
            return Some(PaneView::Board(pane, view));
        }
        if let Some(view) = self.review(pane) {
            return Some(PaneView::Review(pane, view));
        }
        if let Some(view) = self.browser(pane) {
            return Some(PaneView::Browser(view));
        }
        if let Some(view) = self.other(pane) {
            return Some(PaneView::Other(view));
        }
        let id = self.workspaces.find_pane(pane)?.2.content.terminal()?;
        if let Some(chat) = self.chats.get(&id) {
            Some(PaneView::Chat(id, chat))
        } else {
            self.panes.get(&id).map(|view| PaneView::Terminal(id, view))
        }
    }
}

impl PaneView<'_> {
    pub(super) fn focus(self) -> Focus {
        match self {
            Self::Editor(id, ..) => Focus::Editor(id),
            Self::Explorer(id, _) => Focus::Explorer(id),
            Self::Board(id, _) => Focus::Board(id),
            Self::Review(id, _) => Focus::Review(id),
            Self::Chat(id, _) => Focus::Chat(id),
            Self::Terminal(id, _) => Focus::Terminal(id),
            Self::Browser(..) => Focus::Browser,
            Self::Other(..) => Focus::Other,
        }
    }

    pub(super) fn shown_mode(self) -> ModeName {
        match self {
            Self::Editor(_, editor, _) => editor_mode_name(editor),
            Self::Chat(_, chat) => mode_name(chat.shown_mode()),
            Self::Terminal(_, terminal) => mode_name(terminal.shown_mode()),
            Self::Explorer(..)
            | Self::Board(..)
            | Self::Review(..)
            | Self::Browser(..)
            | Self::Other(..) => ModeName::Normal,
        }
    }

    /// Editing mode, distinct from a mouse selection shown in the bar.
    /// Read-only panes leave the window's remembered mode alone.
    pub(super) fn mode(self, desk: &Desk) -> Option<Mode> {
        match self {
            Self::Editor(pane, editor, _) => {
                (!desk.has_no_insert_mode(pane)).then_some(editor.mode)
            }
            Self::Chat(_, chat) => Some(chat.mode()),
            Self::Terminal(_, terminal) => Some(terminal.mode),
            Self::Explorer(..)
            | Self::Board(..)
            | Self::Review(..)
            | Self::Browser(..)
            | Self::Other(..) => None,
        }
    }

    pub(super) fn bar(self) -> Bar {
        let mut bar = Bar::default();
        bar.label = match self {
            Self::Editor(_, editor, copy) => {
                bar.said = editor.said().map(str::to_owned);
                bar.prompting = editor.prompting();
                bar.selected = editor.label_selected();
                Some(editor.label(copy))
            }
            Self::Explorer(_, explorer) => {
                bar.prompting = explorer.prompting();
                Some(explorer.label())
            }
            Self::Board(_, board) => {
                bar.prompting = board.prompting();
                Some(board.label())
            }
            Self::Review(_, review) => {
                bar.prompting = review.prompting();
                Some(review.label())
            }
            Self::Browser(browser) => Some(browser.label()),
            Self::Other(other) => Some(other.title().to_owned()),
            Self::Chat(_, chat) => {
                let prompt = chat.prompt_label();
                bar.prompting = prompt.is_some();
                prompt.or_else(|| chat.label())
            }
            Self::Terminal(_, terminal) => {
                bar.prompting = terminal.prompting();
                terminal.label()
            }
        };
        bar
    }
}

impl Focus {
    pub(super) fn leave(self, desk: &mut Desk) {
        match self {
            Focus::Editor(pane) => {
                desk.close_editor_panel(pane);
                desk.editor_leaves(pane);
                // A folder's tree can still own a search or pending keys.
                if let Some(explorer) = desk.explorers.panes.get_mut(&pane) {
                    explorer.leave();
                }
            }
            Focus::Explorer(pane) => desk
                .explorers
                .panes
                .get_mut(&pane)
                .expect("resolved explorer")
                .leave(),
            Focus::Board(pane) => desk
                .boards
                .panes
                .get_mut(&pane)
                .expect("resolved board")
                .leave(),
            Focus::Review(pane) => desk
                .reviews
                .panes
                .get_mut(&pane)
                .expect("resolved review")
                .leave(),
            Focus::Chat(id) => desk.chats.get_mut(&id).expect("resolved chat").leave(),
            Focus::Terminal(id) => desk.panes.get_mut(&id).expect("resolved terminal").leave(),
            Focus::Browser | Focus::Other => {}
        }
    }

    /// Arrives in the requested mode. True means a chat held the focus
    /// in normal mode to prevent stray keys from answering its question.
    pub(super) fn arrive(self, desk: &mut Desk, insert: bool) -> bool {
        match self {
            Focus::Editor(pane) => desk.editor_to_mode(pane, insert),
            Focus::Chat(id) => {
                let chat = desk.chats.get_mut(&id).expect("resolved chat");
                chat.arrived();
                let held = insert && chat.asking();
                if insert && !held {
                    chat.enter_insert_mode();
                } else {
                    chat.enter_normal_mode();
                }
                return held;
            }
            Focus::Terminal(id) => {
                let terminal = desk.panes.get_mut(&id).expect("resolved terminal");
                if insert {
                    terminal.enter_insert_mode();
                } else {
                    terminal.enter_normal_mode();
                }
            }
            Focus::Explorer(_)
            | Focus::Board(_)
            | Focus::Review(_)
            | Focus::Browser
            | Focus::Other => {}
        }
        false
    }
}

impl Focus {
    /// Display-only panes can take focus without requesting clipboard data.
    pub(super) fn takes_paste(self) -> bool {
        !matches!(self, Self::Browser | Self::Other)
    }

    pub(super) fn paste(self, desk: &mut Desk, text: &str) {
        match self {
            Self::Editor(_) => {
                desk.paste_into_editor(text);
            }
            Self::Explorer(id) => {
                desk.explorers
                    .panes
                    .get_mut(&id)
                    .expect("resolved explorer")
                    .paste(text);
            }
            Self::Board(id) => {
                desk.boards
                    .panes
                    .get_mut(&id)
                    .expect("resolved board")
                    .paste(text);
            }
            Self::Review(id) => {
                desk.reviews
                    .panes
                    .get_mut(&id)
                    .expect("resolved review")
                    .paste(text);
            }
            Self::Chat(id) => {
                let chat = desk.chats.get_mut(&id).expect("resolved chat");
                match chat.search_line() {
                    Some(line) => line.put(text),
                    None => chat.paste(text),
                }
            }
            Self::Terminal(id) => {
                let pane = desk.panes.get_mut(&id).expect("resolved terminal");
                if !pane.paste_into_search(text) {
                    pane.paste(text);
                }
            }
            Self::Browser | Self::Other => {}
        }
    }

    pub(super) fn wheel(
        self,
        desk: &mut Desk,
        point: [f32; 2],
        lines: i64,
        modifiers: crate::input::Modifiers,
    ) -> bool {
        match self {
            Self::Editor(id) => desk.editor_wheel(id, lines),
            // List panes count downwards; terminal wheel input counts upwards.
            Self::Explorer(id) => desk
                .explorers
                .panes
                .get_mut(&id)
                .expect("resolved explorer")
                .scroll(lines.saturating_neg()),
            Self::Board(id) => desk
                .boards
                .panes
                .get_mut(&id)
                .expect("resolved board")
                .scroll(lines.saturating_neg()),
            Self::Review(id) => desk
                .reviews
                .panes
                .get_mut(&id)
                .expect("resolved review")
                .scroll(lines.saturating_neg()),
            Self::Chat(id) => {
                let pixel = desk
                    .cell_at(point, None)
                    .filter(|(at, ..)| *at == id)
                    .map_or([-1.0; 2], |(_, _, pixel)| pixel);
                desk.chats
                    .get_mut(&id)
                    .expect("resolved chat")
                    .wheel(lines, pixel, modifiers)
            }
            Self::Terminal(id) => {
                let cell = desk
                    .cell_at(point, None)
                    .filter(|(at, ..)| *at == id)
                    .map_or([0, 0], |(_, cell, _)| cell);
                desk.panes
                    .get_mut(&id)
                    .expect("resolved terminal")
                    .wheel(lines, cell, modifiers)
            }
            Self::Browser | Self::Other => false,
        }
    }

    pub(super) fn click(
        self,
        desk: &mut Desk,
        point: [f32; 2],
        cell: [i64; 2],
        clicks: u8,
        modifiers: crate::input::Modifiers,
    ) -> bool {
        match self {
            Self::Editor(id) => {
                desk.rendered_click(point) || desk.editor_press(id, cell, clicks, modifiers.alt)
            }
            Self::Explorer(id) => {
                desk.acting_from_list(|desk| desk.explorer_click(id, point, cell, clicks))
            }
            Self::Board(id) => desk.acting_from_list(|desk| desk.board_click(id, cell, clicks)),
            Self::Review(id) => desk.acting_from_list(|desk| desk.review_click(id, cell, clicks)),
            Self::Chat(_) | Self::Terminal(_) | Self::Browser | Self::Other => false,
        }
    }
}

impl<'a> PaneView<'a> {
    pub(super) fn key_state(self) -> Option<&'a dyn crate::vim::KeyState> {
        Some(match self {
            Self::Editor(_, pane, _) => pane,
            Self::Explorer(_, pane) => pane,
            Self::Board(_, pane) => pane,
            Self::Review(_, pane) => pane,
            Self::Chat(_, pane) => pane,
            Self::Terminal(_, pane) => pane,
            Self::Browser(_) | Self::Other(_) => return None,
        })
    }

    pub(super) fn shortcut_context(self) -> crate::shortcuts::Context {
        match self {
            Self::Editor(_, pane, _) => pane.shortcut_context(),
            Self::Explorer(_, pane) => pane.shortcut_context(),
            Self::Board(_, pane) => pane.shortcut_context(),
            Self::Review(_, pane) => pane.shortcut_context(),
            Self::Chat(_, pane) => pane.shortcut_context(),
            Self::Terminal(_, pane) => pane.shortcut_context(),
            Self::Browser(pane) => pane.shortcut_context(),
            Self::Other(_) => crate::shortcuts::Context::new("Window", &[]),
        }
    }

    pub(super) fn text_target(self) -> Option<super::TextTarget> {
        use super::TextTarget;
        let (prompting, typing) = match self {
            Self::Editor(_, pane, _) => (pane.prompting(), pane.mode == Mode::Insert),
            Self::Explorer(_, pane) => (pane.prompting(), false),
            Self::Board(_, pane) => (pane.prompting(), false),
            Self::Review(_, pane) => (pane.prompting(), false),
            Self::Chat(_, pane) => (pane.prompting(), pane.takes_text()),
            Self::Terminal(_, pane) => (pane.prompting(), pane.mode == Mode::Insert),
            Self::Browser(_) | Self::Other(_) => return None,
        };
        if prompting {
            Some(TextTarget::Bar)
        } else {
            typing.then_some(TextTarget::Pane)
        }
    }
}

impl Focus {
    pub(super) fn key_state_mut(self, desk: &mut Desk) -> Option<&mut dyn crate::vim::KeyState> {
        Some(match self {
            Self::Editor(id) => desk.editors.panes.get_mut(&id)?,
            Self::Explorer(id) => desk.explorers.panes.get_mut(&id)?,
            Self::Board(id) => desk.boards.panes.get_mut(&id)?,
            Self::Review(id) => desk.reviews.panes.get_mut(&id)?,
            Self::Chat(id) => desk.chats.get_mut(&id)?,
            Self::Terminal(id) => desk.panes.get_mut(&id)?,
            Self::Browser | Self::Other => return None,
        })
    }
}
