//! The daemon routes only to a window which explicitly granted this chat access.
//! Grants are ephemeral and bound to a live connection, never to a reusable ID.
use std::time::{Duration, Instant};

use clackatoo_protocol::{Event, NativeAccess, NativeAction, NativeReply, TerminalId};
use mio::Token;
use serde_json::Value;

use super::{Daemon, Source, Wait, Waiting};
use crate::server::Link;

impl Daemon {
    pub(in crate::server) fn native_grant(
        &mut self,
        window: Token,
        chat: TerminalId,
        access: NativeAccess,
    ) {
        let valid = matches!(self.sources.get(window.0), Some(Source::Client(c))
            if !matches!(c.link, Link::Web(_)) && c.views.contains_key(&chat) && c.wire >= clackatoo_protocol::wire::HARNESS);
        if !valid || self.chat_ref(chat).is_none() {
            self.send_event(window, Event::NativeGrantAck { chat, access: NativeAccess::Off, reason: "Access not granted: select a local chat in this window (for a remote chat, connect the window directly with --remote).".into() });
            return;
        }
        // Revocation can only remove this window's grant, not another window's.
        if access == NativeAccess::Off {
            if self
                .tools
                .native
                .get(&chat)
                .is_some_and(|(token, _)| *token == window)
            {
                self.tools.native.remove(&chat);
                self.refuse_native_waits(window, Some(chat));
            }
        } else {
            let previous: Vec<_> = self
                .tools
                .native
                .iter()
                .filter(|(_, (w, _))| *w == window)
                .map(|(chat, _)| *chat)
                .collect();
            for old_chat in previous {
                self.tools.native.remove(&old_chat);
                self.refuse_native_waits(window, Some(old_chat));
            }
            if let Some((old, _)) = self.tools.native.insert(chat, (window, access)) {
                self.refuse_native_waits(old, Some(chat));
                if old != window {
                    self.send_event(
                        old,
                        Event::NativeGrantAck {
                            chat,
                            access: NativeAccess::Off,
                            reason: "Access moved to another window.".into(),
                        },
                    );
                }
            }
        }
        self.send_event(
            window,
            Event::NativeGrantAck {
                chat,
                access,
                reason: format!("Agent window access: {access:?}. Revoke with :agentaccess off."),
            },
        );
    }

    pub(in crate::server) fn native_disconnected(&mut self, window: Token) {
        self.tools.native.retain(|_, (token, _)| *token != window);
        self.refuse_native_waits(window, None);
    }

    fn refuse_native_waits(&mut self, window: Token, chat: Option<TerminalId>) {
        let (revoked, kept) = std::mem::take(&mut self.tools.waiting)
            .into_iter()
            .partition(|w| {
                chat.is_none_or(|chat| w.chat == chat)
                    && matches!(w.what, Wait::Native { window: target, .. } if target == window)
            });
        self.tools.waiting = kept;
        for w in revoked {
            self.answer_tool(w.token, &w.id, Err("Window access was revoked or disconnected. A pending action's outcome is unknown; do not retry it blindly.".into()));
        }
    }

    pub(super) fn native_call(
        &mut self,
        token: Token,
        chat: TerminalId,
        id: Value,
        action: NativeAction,
    ) {
        let Some(&(window, access)) = self.tools.native.get(&chat) else {
            self.answer_tool(token, &id, Err("Native window access is Off. The user must select a window and run :agentaccess inspect or :agentaccess control there. The agent cannot grant itself access.".into()));
            return;
        };
        let inspecting = action == NativeAction::Snapshot;
        if !inspecting
            && (access != NativeAccess::Control
                || self.chat_ref(chat).is_none_or(|c| c.limits().no_commands()))
        {
            self.answer_tool(
                token,
                &id,
                Err("This action requires Control access and a chat allowed to act.".into()),
            );
            return;
        }
        if self
            .tools
            .waiting
            .iter()
            .any(|w| matches!(w.what, Wait::Native { window: target, .. } if target == window))
        {
            self.answer_tool(
                token,
                &id,
                Err("This window already has a request in flight; wait for its result.".into()),
            );
            return;
        }
        let call = self.tools.next_call;
        self.tools.next_call += 1;
        self.tools.waiting.push(Waiting {
            token,
            id,
            chat,
            what: Wait::Native {
                call,
                window,
                due: Instant::now() + Duration::from_secs(15),
            },
        });
        self.send_event(
            window,
            Event::NativeRequest {
                id: call,
                chat,
                action,
            },
        );
    }

    pub(in crate::server) fn native_reply(
        &mut self,
        window: Token,
        id: u64,
        result: Result<NativeReply, String>,
    ) {
        let Some(at) = self.tools.waiting.iter().position(|w| matches!(w.what, Wait::Native { call, window: target, .. } if call == id && target == window)) else { return; };
        let waiting = self.tools.waiting.remove(at);
        if self
            .chat_ref(waiting.chat)
            .is_none_or(|chat| chat.tools() == clackatoo_protocol::AgentTools::Off)
            || self
                .tools
                .native
                .get(&waiting.chat)
                .is_none_or(|(target, _)| *target != window)
        {
            self.answer_tool(
                waiting.token,
                &waiting.id,
                Err("Window access was revoked.".into()),
            );
            return;
        }
        match result {
            Ok(reply)
                if reply.description.len() <= 64 * 1024
                    && reply
                        .png
                        .as_ref()
                        .is_none_or(|p| p.len() <= 8 * 1024 * 1024) =>
            {
                let text = format!("revision={}\n{}", reply.revision, reply.description);
                if let Some(png) = reply.png {
                    self.answer_picture(waiting.token, &waiting.id, &text, &png);
                } else {
                    self.answer_tool(waiting.token, &waiting.id, Ok(text));
                }
            }
            Ok(_) => self.answer_tool(
                waiting.token,
                &waiting.id,
                Err("Window response exceeded its size limit.".into()),
            ),
            Err(why) => self.answer_tool(
                waiting.token,
                &waiting.id,
                Err(why.chars().take(4096).collect()),
            ),
        }
    }
}
