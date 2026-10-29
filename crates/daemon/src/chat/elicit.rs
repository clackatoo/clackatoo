//! A chat's questions that aren't whether the agent may use a tool: forms
//! the agent asks the user to fill in, and links to open (ACP's
//! elicitation, read by [`crate::elicit`]), held with the chat's other
//! questions (see [`HostedChat::ask`](super::HostedChat::ask)), one shown at a time;
//! and the plan a question about leaving planning carries.
//!
//! Like the agent's other questions, one is answered by the first window
//! that answers it, and goes when the agent no longer waits: when the turn
//! is stopped (answered as cancelled), when the agent cancels its request
//! (`$/cancel_request`) or says a link is done with, or when its process
//! or session goes.

use agent_client_protocol_schema::v1::{PermissionOption, ToolCallUpdate, ToolKind as AcpKind};
use clackatoo_protocol::{Choice, ChoiceKind, FormAsk, FormReply, Question, code_span};
use serde_json::Value;

use super::{Action, Asker, HostedChat, MAX_QUESTIONS};
use crate::elicit::Asked;

/// The ID of the one choice a form question offers a window too old to
/// show forms: declining it.
pub(super) const DECLINE: &str = "decline";

/// The most characters of a plan kept.
const MOST_PLAN: usize = 64 * 1024;

/// The most characters of a form's message quoted in the title an older
/// window shows.
const MOST_QUOTED: usize = 200;

impl HostedChat {
    /// The agent asks the user what `asked` says, in its request
    /// `request`. Returns whether it was asked: one told again as the
    /// session loads, or past [`MAX_QUESTIONS`] waiting, isn't, and is to
    /// be declined.
    pub(crate) fn elicit(&mut self, request: Value, asked: Asked) -> bool {
        if self.told_again() || self.asking.len() >= MAX_QUESTIONS {
            return false;
        }
        let quoted: String = asked.form.message.chars().take(MOST_QUOTED).collect();
        let quoted = code_span(&quoted.replace(['\n', '\r', '\t'], " "));
        // Worded by the daemon, for a window older than forms, which shows
        // this and the one choice: what's asked is the agent's, quoted.
        let title = match &asked.form.ask {
            FormAsk::Url(_) => format!("open a link, {quoted}, which needs a newer clackatoo"),
            FormAsk::Fields { .. } => {
                format!("ask you {quoted}, which needs a newer clackatoo to answer")
            }
        };
        let question = Question {
            id: self.next_question,
            title,
            choices: vec![Choice {
                id: DECLINE.to_owned(),
                name: "Decline".to_owned(),
                kind: ChoiceKind::RejectOnce,
            }],
            own: false,
            // The question holds the form; nothing else does.
            form: Some(asked.form),
            plan: None,
            tool: None,
        };
        self.next_question += 1;
        let held = Asker::Elicit {
            request,
            url_id: asked.url_id,
        };
        self.asking.push_back((held, question));
        self.show_question();
        true
    }

    /// A window answered the form question numbered `question` with
    /// `reply`: if it's the one shown, and an acceptance fits the form,
    /// the agent hears it. Any other is ignored, as an answer sent twice
    /// or to a question gone is, so a stale or forged one changes nothing.
    pub(super) fn fill(&mut self, question: u64, reply: &FormReply, actions: &mut Vec<Action>) {
        let Some((Asker::Elicit { .. }, shown)) = self.asking.front() else {
            return;
        };
        let Some(form) = shown.form.as_ref().filter(|_| shown.id == question) else {
            return;
        };
        if let FormReply::Accept(answers) = reply
            && let Some(problem) = form.problem(answers)
        {
            log::info!(
                "chat {}: an answer to a form didn't fit it: {problem}",
                self.id
            );
            return;
        }
        let result = crate::elicit::result(form, reply);
        let Some((Asker::Elicit { request, .. }, _)) = self.asking.pop_front() else {
            return;
        };
        self.show_question();
        actions.push(Action::Elicited { request, result });
    }

    /// The agent no longer waits for its request `request`: the question
    /// it asked with it goes, unanswered. Returns whether one did, which
    /// the agent is to be told was cancelled.
    pub(crate) fn withdraw_request(&mut self, request: &Value) -> bool {
        let before = self.asking.len();
        self.asking.retain(|(asker, _)| match asker {
            Asker::Agent(asked) | Asker::Elicit { request: asked, .. } => asked != request,
            _ => true,
        });
        let withdrawn = self.asking.len() != before;
        if withdrawn {
            self.show_question();
        }
        withdrawn
    }

    /// The agent says the link it asked the user to open, which it calls
    /// `id`, is done with: the question goes. Returns the request it was
    /// asked in, if one still waited, to answer: as cancelled, as the user never opened it.
    pub(crate) fn link_done(&mut self, id: &str) -> Option<Value> {
        let at = (self.asking.iter()).position(|(asker, _)| {
            matches!(asker, Asker::Elicit { url_id: Some(url_id), .. } if url_id == id)
        })?;
        let (Asker::Elicit { request, .. }, _) = self.asking.remove(at)? else {
            return None;
        };
        self.show_question();
        Some(request)
    }

    /// Says the agent asked something too big to show, which was
    /// declined: once, until the user next writes, however many come.
    pub(crate) fn too_big(&mut self) {
        if !std::mem::replace(&mut self.told_too_big, true) {
            let agent = self.agent_title().to_owned();
            self.notify(format!(
                "{agent} asked you something too big to show, so it was declined."
            ));
        }
    }

    /// Answers every form the agent asked as cancelled, as stopping does.
    pub(super) fn cancel_forms(&mut self, actions: &mut Vec<Action>) {
        let before = self.asking.len();
        self.asking.retain(|(asker, _)| {
            let Asker::Elicit { request, .. } = asker else {
                return true;
            };
            actions.push(Action::Elicited {
                request: request.clone(),
                result: serde_json::json!({ "action": "cancel" }),
            });
            false
        });
        if self.asking.len() != before {
            self.show_question();
        }
    }

    /// The chat is moving to another agent: what the agent it leaves
    /// asked the user goes, its requests kept to answer as cancelled (see
    /// [`take_left_forms`](Self::take_left_forms)).
    pub(super) fn forms_left(&mut self) {
        let mut left = Vec::new();
        self.asking.retain(|(asker, _)| match asker {
            Asker::Elicit { request, .. } => {
                left.push(request.clone());
                false
            }
            _ => true,
        });
        if !left.is_empty() {
            self.left_forms.extend(left);
            self.show_question();
        }
    }

    /// The requests of the forms the agent the chat moved from still
    /// waits on, to answer as cancelled.
    pub(crate) fn take_left_forms(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.left_forms)
    }

    /// The agent's process or session has gone: what it asked the user
    /// goes with it, as nothing waits for the answer.
    pub(super) fn elicitations_lost(&mut self) {
        let before = self.asking.len();
        (self.asking).retain(|(asker, _)| !matches!(asker, Asker::Elicit { .. }));
        if self.asking.len() != before {
            self.show_question();
        }
    }
}

/// The plan a question about `tool` asks to go ahead with, if it's one
/// that leaves planning: a tool that switches mode (Codex's plan review),
/// or offered the options that leave Claude Code's plan mode (whose
/// questions to a client that asks as AIR does say no kind), with the
/// plan in its input, as both adapters put it.
pub(super) fn plan(tool: &ToolCallUpdate, options: &[PermissionOption]) -> Option<String> {
    let leaves = tool.fields.kind == Some(AcpKind::SwitchMode)
        || (options.iter()).any(|option| {
            let id = &option.option_id.0;
            id.starts_with("exit-plan-") || **id == *"implement_plan"
        });
    if !leaves {
        return None;
    }
    let plan = tool.fields.raw_input.as_ref()?.get("plan")?.as_str()?;
    let plan: String = (plan.trim().chars())
        .filter(|&ch| matches!(ch, '\n' | '\t') || !ch.is_control())
        .take(MOST_PLAN)
        .collect();
    // Shown as agents' words to other agents are: nothing in it hidden.
    let plan = crate::server::unhidden(&plan);
    (!plan.is_empty()).then_some(plan)
}
