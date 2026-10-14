//! Chats' pages in clackatoo's browser, on the event loop: the agents'
//! calls to the browser's tools, the panes watching pages, and what the
//! browser's thread hands back (see [`crate::browser`]).
//!
//! A call goes to the browser's thread as a job, and waits among the
//! tools' calls (`Wait::Browser`) for its answer, so a call cancelled, or
//! one whose chat or connection goes, is let go as any other is.
//!
//! A window attaches to a chat's page for each pane of the tab it shows
//! that shows it, saying how big the pane is. While any window is
//! attached, the browser sends pictures of the page, at the largest size
//! asked for. Each window is sent only the latest picture it hasn't had,
//! once it has written out everything before: one falling behind skips
//! pictures, and never piles them up.

use std::collections::BTreeMap;
use std::sync::Arc;

use clackatoo_protocol::{
    BrowserFrame, BrowserStatus, BrowserUpdate, Content, Event, PaneTerminal, Response,
    ServerMessage, TerminalId, frame,
};
use mio::Token;
use serde_json::Value;

use super::{Daemon, Source};
use crate::browser::{Act, Answer, Browser, Job};

/// Chats' pages, as the event loop keeps them.
#[derive(Debug, Default)]
pub(super) struct Browsing {
    /// The browser's thread, once a chat has needed it.
    browser: Option<Browser>,
    /// The number the next call to the browser gets.
    next_call: u64,
    /// What each chat's page was last said to be.
    statuses: BTreeMap<TerminalId, BrowserStatus>,
    /// The windows attached to each chat's page, with how big a picture
    /// each wants.
    viewers: BTreeMap<TerminalId, Vec<(Token, [u32; 2])>>,
    /// Some window may have a picture it can be sent now.
    pub(super) frames_unsent: bool,
}

impl Daemon {
    /// Hands the browser's thread `job`, starting it if it hasn't been.
    /// Returns whether there's a browser to hand it to.
    fn browser_job(&mut self, job: Job) -> bool {
        if self.browsing.browser.is_none() {
            let Some(config) = self.config.browser.clone() else {
                return false;
            };
            let waker = Arc::clone(&self.waker);
            let browser = Browser::start(config, move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for the browser failed: {err}");
                }
            });
            // Pages already watched are watched by the new thread.
            for (&chat, viewers) in &self.browsing.viewers {
                if let Some(size) = largest(viewers) {
                    browser.send(Job::Watch {
                        chat,
                        size: Some(size),
                    });
                }
            }
            self.browsing.browser = Some(browser);
        }
        if let Some(browser) = &self.browsing.browser {
            browser.send(job);
        }
        true
    }

    /// Carries out `act` for the agent of chat `chat`, answering the call
    /// `id` on the connection at `token` when the browser has. The first
    /// page a chat opens gets a pane beside it, unless one shows the
    /// chat's page already.
    pub(super) fn browse(&mut self, token: Token, chat: TerminalId, id: Value, act: Act) {
        if self.config.browser.is_none() {
            let refused = Err("this daemon runs no browser (CLACKATOO_BROWSER is off)".to_owned());
            self.answer_tool(token, &id, refused);
            return;
        }
        if matches!(act, Act::Navigate { .. })
            && !self.browser_shown(chat)
            && let Err(why) = self.place_beside(chat, PaneTerminal::Browser(chat))
        {
            log::info!("chat {chat}: its page gets no pane: {why}");
        }
        let call = self.browsing.next_call;
        self.browsing.next_call += 1;
        self.wait_for_browser(token, id, chat, call);
        self.browser_job(Job::Act { call, chat, act });
    }

    /// Whether a pane shows the chat's page.
    fn browser_shown(&self, chat: TerminalId) -> bool {
        (self.workspaces.get().list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .any(|pane| pane.content == Content::Browser(chat))
    }

    /// Closes the chat's page, as its Browser setting went off.
    pub(super) fn close_page(&mut self, chat: TerminalId) {
        if self.browsing.browser.is_some() {
            self.browser_job(Job::Close(chat));
        }
    }

    /// The chat has closed: so has its page, and the windows watching it
    /// are told.
    pub(super) fn chat_left_browser(&mut self, chat: TerminalId) {
        self.close_page(chat);
        self.browsing.statuses.remove(&chat);
        let viewers = self
            .browsing
            .viewers
            .get(&chat)
            .cloned()
            .unwrap_or_default();
        for (viewer, _) in viewers {
            let update = BrowserUpdate::Status(closed(chat));
            self.send_event(viewer, Event::Browser { chat, update });
        }
    }

    /// Answers [`Request::AttachBrowser`](clackatoo_protocol::Request::AttachBrowser).
    pub(super) fn attach_browser(
        &mut self,
        token: Token,
        chat: TerminalId,
        size: [u32; 2],
    ) -> Response {
        let viewers = self.browsing.viewers.entry(chat).or_default();
        viewers.retain(|&(viewer, _)| viewer != token);
        viewers.push((token, size));
        let status = match self.chat_ref(chat) {
            Some(_) => (self.browsing.statuses.get(&chat).cloned()).unwrap_or_default(),
            None => closed(chat),
        };
        self.send_event(
            token,
            Event::Browser {
                chat,
                update: BrowserUpdate::Status(status),
            },
        );
        self.watch_page(chat);
        Response::Done
    }

    /// Answers [`Request::DetachBrowser`](clackatoo_protocol::Request::DetachBrowser).
    pub(super) fn detach_browser(&mut self, token: Token, chat: TerminalId) -> Response {
        if let Some(viewers) = self.browsing.viewers.get_mut(&chat) {
            viewers.retain(|&(viewer, _)| viewer != token);
            if viewers.is_empty() {
                self.browsing.viewers.remove(&chat);
            }
        }
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
            client.browser_frames.remove(&chat);
        }
        self.watch_page(chat);
        Response::Done
    }

    /// Forgets the client at `token`, which has gone, as a viewer of
    /// pages.
    pub(super) fn forget_browser_viewer(&mut self, token: Token) {
        let mut changed = Vec::new();
        for (&chat, viewers) in &mut self.browsing.viewers {
            let before = viewers.len();
            viewers.retain(|&(viewer, _)| viewer != token);
            if viewers.len() != before {
                changed.push(chat);
            }
        }
        self.browsing
            .viewers
            .retain(|_, viewers| !viewers.is_empty());
        for chat in changed {
            self.watch_page(chat);
        }
    }

    /// Tells the browser how big pictures of the chat's page should be,
    /// or that nobody watches it.
    fn watch_page(&mut self, chat: TerminalId) {
        let size = self
            .browsing
            .viewers
            .get(&chat)
            .and_then(|viewers| largest(viewers));
        // A browser not started yet hears of it when it starts.
        if self.browsing.browser.is_some() {
            self.browser_job(Job::Watch { chat, size });
        }
    }

    /// Takes what the browser's thread has handed back: answers to calls,
    /// what pages are now, and pictures of them.
    pub(super) fn take_browser(&mut self) {
        let Some(browser) = &self.browsing.browser else {
            return;
        };
        let outbox = browser.take();
        for (call, answer) in outbox.answers {
            let Some((token, id)) = self.browser_answered(call) else {
                continue;
            };
            match answer {
                Ok(Answer::Text(text)) => self.answer_tool(token, &id, Ok(text)),
                Ok(Answer::Picture { text, png }) => self.answer_picture(token, &id, &text, &png),
                Err(why) => self.answer_tool(token, &id, Err(why)),
            }
        }
        for (chat, status) in outbox.statuses {
            // A closed chat's page says so, whatever the browser says last.
            if self.browsing.statuses.get(&chat) == Some(&status) || self.chat_ref(chat).is_none() {
                continue;
            }
            self.browsing.statuses.insert(chat, status.clone());
            let viewers = self
                .browsing
                .viewers
                .get(&chat)
                .cloned()
                .unwrap_or_default();
            for (viewer, _) in viewers {
                let update = BrowserUpdate::Status(status.clone());
                self.send_event(viewer, Event::Browser { chat, update });
            }
        }
        for (chat, jpeg) in outbox.frames {
            let jpeg = Arc::new(jpeg);
            for &(viewer, _) in self.browsing.viewers.get(&chat).into_iter().flatten() {
                if let Some(Source::Client(client)) = self.sources.get_mut(viewer.0) {
                    client.browser_frames.insert(chat, Arc::clone(&jpeg));
                    self.browsing.frames_unsent = true;
                }
            }
        }
    }

    /// Sends each window the pictures waiting for it, if it has written
    /// out everything before them.
    pub(super) fn send_browser_frames(&mut self) {
        if !std::mem::take(&mut self.browsing.frames_unsent) {
            return;
        }
        let mut sent = Vec::new();
        for (key, source) in &mut self.sources {
            let Source::Client(client) = source else {
                continue;
            };
            if client.browser_frames.is_empty() || client.finished || !client.outgoing.is_empty() {
                continue;
            }
            for (chat, jpeg) in std::mem::take(&mut client.browser_frames) {
                let update = BrowserUpdate::Frame(BrowserFrame {
                    jpeg: Arc::unwrap_or_clone(jpeg),
                });
                let event = ServerMessage::Event(Event::Browser { chat, update });
                match frame::encode(client.wire, &event) {
                    Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
                    Err(err) => log::error!("encoding a picture of a page failed: {err}"),
                }
            }
            sent.push(Token(key));
        }
        for token in sent {
            self.flush_client(token);
        }
    }

    /// Stops the browser and its thread, as the daemon stops or upgrades.
    pub(super) fn stop_browser(&mut self) {
        if let Some(mut browser) = self.browsing.browser.take() {
            browser.stop();
        }
    }
}

/// What a page is once its chat has closed.
fn closed(chat: TerminalId) -> BrowserStatus {
    BrowserStatus {
        failure: Some(format!("chat {chat} has closed")),
        ..BrowserStatus::default()
    }
}

/// The largest picture any of `viewers` wants, if there are any.
fn largest(viewers: &[(Token, [u32; 2])]) -> Option<[u32; 2]> {
    (viewers.iter().map(|&(_, size)| size)).reduce(|a, b| [a[0].max(b[0]), a[1].max(b[1])])
}
