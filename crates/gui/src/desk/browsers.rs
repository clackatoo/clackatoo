//! The desk's browser panes: each shows a chat's page (see
//! [`crate::browser`]), attached only while it's in the tab shown, so
//! pictures of a page nobody sees are never sent.
//!
//! A chat's page is attached once, at the largest size its panes in the
//! tab want, in steps, so dragging a divider doesn't ask for new pictures
//! at every pixel. `:Browser` opens the focused chat's page in a new tab,
//! and `:|Browser` beside it. An agent's first page opens one by itself.

use std::collections::HashMap;
use std::sync::Arc;

use clackatoo_protocol::{
    BrowserUpdate, Content, Event, PaneId, PaneTerminal, Request, Side, Size, TerminalId,
};

use super::{Asked, Desk};
use crate::browser::{Ask, BrowserPane, STATUS_ROWS};
use crate::command::Place;
use crate::input::KeyPress;
use crate::layout::Tile;
use crate::render::Image;

/// The step sizes are rounded up to, in pixels.
const SIZE_STEP: u32 = 160;

/// The browser panes, and the pages attached.
#[derive(Debug, Default)]
pub(super) struct Browsers {
    panes: HashMap<PaneId, BrowserPane>,
    /// How big each pane's picture can be, in pixels, as its tile last
    /// was.
    sizes: HashMap<PaneId, [u32; 2]>,
    /// The pages attached, at the size asked for.
    attached: HashMap<TerminalId, [u32; 2]>,
}

impl Desk {
    /// The browser pane `pane` is, if it's one.
    pub(crate) fn browser(&self, pane: PaneId) -> Option<&BrowserPane> {
        self.browsers.panes.get(&pane)
    }

    /// The focused pane's browser, if it shows one.
    pub(super) fn focused_browser(&self) -> Option<&BrowserPane> {
        self.browsers.panes.get(&self.shown?.pane)
    }

    /// Makes a browser pane for every pane of the tab shown that shows a
    /// chat's page, and attaches to their pages at the size they want;
    /// forgets those gone, and detaches from pages no longer shown.
    pub(super) fn attach_shown_browsers(&mut self) {
        let shown: Vec<(PaneId, TerminalId)> = (self.shown_tab())
            .map(|tab| {
                (tab.layout.panes().iter())
                    .filter_map(|pane| Some((pane.id, pane.content.browser()?)))
                    .collect()
            })
            .unwrap_or_default();
        let browsers = &mut self.browsers;
        browsers
            .panes
            .retain(|pane, browser| shown.contains(&(*pane, browser.chat())));
        browsers
            .sizes
            .retain(|pane, _| shown.iter().any(|(shown, _)| shown == pane));
        for &(pane, chat) in &shown {
            browsers
                .panes
                .entry(pane)
                .or_insert_with(|| BrowserPane::new(chat));
        }
        if !self.connected {
            return;
        }
        // A pane is attached once it's been fitted to its tile, and its
        // size is known: pictures at a guess would be asked for again.
        let mut wanted: HashMap<TerminalId, [u32; 2]> = HashMap::new();
        for &(pane, chat) in &shown {
            let Some(&size) = browsers.sizes.get(&pane) else {
                continue;
            };
            let most = wanted.entry(chat).or_insert([0, 0]);
            *most = [most[0].max(size[0]), most[1].max(size[1])];
        }
        let gone: Vec<TerminalId> = (browsers.attached.keys())
            .filter(|chat| !wanted.contains_key(chat))
            .copied()
            .collect();
        for chat in gone {
            self.browsers.attached.remove(&chat);
            self.request(Request::DetachBrowser { chat }, Asked::Quiet);
        }
        for (chat, size) in wanted {
            if self.browsers.attached.get(&chat) == Some(&size) {
                continue;
            }
            self.browsers.attached.insert(chat, size);
            let [width, height] = size;
            let attach = Request::AttachBrowser {
                chat,
                width,
                height,
            };
            self.request(attach, Asked::Quiet);
        }
    }

    /// Sizes the browser pane `pane` to its tile, attaching again if its
    /// picture can be a step bigger or smaller.
    pub(super) fn fit_browser(&mut self, pane: PaneId, tile: Tile) {
        if let Some(browser) = self.browsers.panes.get_mut(&pane)
            && let Ok(size) = Size::new(tile.cols, tile.rows)
        {
            browser.resize(size);
        }
        let words = f32::from(STATUS_ROWS) / f32::from(tile.rows.max(1));
        let height = tile.rect.height * (1.0 - words).max(0.0);
        let size = [stepped(tile.rect.width), stepped(height)];
        if self.browsers.sizes.insert(pane, size) != Some(size) {
            self.attach_shown_browsers();
        }
    }

    /// Lays out the words of the browser panes in the tab shown.
    pub(crate) fn settle_browsers(&mut self) {
        for browser in self.browsers.panes.values_mut() {
            browser.settle();
        }
    }

    /// Forgets what was attached, as a new connection does.
    pub(super) fn browsers_reconnected(&mut self) {
        self.browsers.attached.clear();
    }

    /// Takes in what the daemon says about a chat's page. Returns whether
    /// the window must be redrawn.
    pub(crate) fn browser_event(&mut self, event: Event) -> bool {
        let Event::Browser { chat, update } = event else {
            return false;
        };
        let BrowserUpdate::Status(status) = update else {
            // Pictures come decoded, through `browser_picture`.
            return false;
        };
        let mut shown = false;
        for browser in self.browsers.panes.values_mut() {
            if browser.chat() == chat {
                browser.set_status(status.clone());
                shown = true;
            }
        }
        shown
    }

    /// The latest picture of `chat`'s page is `image`. Returns whether the
    /// window must be redrawn.
    pub(crate) fn browser_picture(&mut self, chat: TerminalId, image: &Arc<Image>) -> bool {
        let mut shown = false;
        for browser in self.browsers.panes.values_mut() {
            if browser.chat() == chat {
                browser.set_image(Arc::clone(image));
                shown = true;
            }
        }
        shown
    }

    /// Hands `press` to the focused browser pane, then does what it asks.
    /// Returns whether the window must be redrawn.
    pub(super) fn browser_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let Some(browser) = self.browsers.panes.get_mut(&pane) else {
            return false;
        };
        let mut redraw = false;
        for stroke in crate::vim::plain_strokes(press, false) {
            redraw |= browser.stroke(stroke);
        }
        for ask in browser.take_asks() {
            match ask {
                Ask::Close => self.request(Request::ClosePane { pane }, Asked::Command),
            }
        }
        redraw
    }

    /// `:Browser`: the focused chat's page, where `place` says.
    pub(super) fn open_browser(&mut self, place: Place) {
        let Some(shown) = self.shown else { return };
        let chat =
            (self.workspaces.find_pane(shown.pane)).and_then(|(_, _, pane)| match pane.content {
                Content::Terminal(terminal)
                    if self
                        .panes
                        .get(&terminal)
                        .is_some_and(crate::pane::Pane::is_chat) =>
                {
                    Some(terminal)
                }
                Content::Browser(chat) => Some(chat),
                _ => None,
            });
        let Some(chat) = chat else {
            self.refuse("Browser shows a chat's page: focus the chat first".to_owned());
            return;
        };
        let terminal = PaneTerminal::Browser(chat);
        let request = match place {
            Place::Tab => Request::NewTab {
                workspace: shown.workspace,
                terminal,
                on: None,
            },
            Place::Beside(side) => Request::Split {
                pane: shown.pane,
                side,
                terminal,
                on: None,
            },
            Place::Here => Request::Split {
                pane: shown.pane,
                side: Side::Right,
                terminal,
                on: None,
            },
        };
        self.request(request, Asked::Pane);
    }
}

/// `pixels`, rounded up to the next step.
fn stepped(pixels: f32) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pixels = pixels.max(1.0).ceil() as u32;
    pixels.div_ceil(SIZE_STEP) * SIZE_STEP
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_go_up_in_steps() {
        assert_eq!(stepped(0.0), SIZE_STEP);
        assert_eq!(stepped(161.0), 2 * SIZE_STEP);
        assert_eq!(stepped(320.0), 2 * SIZE_STEP);
    }
}
