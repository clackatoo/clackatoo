//! A provider choice belongs to the draft until its first prompt is sent.
use clackatoo_protocol::{ChatAction, ChatImage, ProviderModels, Response, SettingKind};

use super::{ChatView, Menu, Settled, picker};

#[derive(Clone, Debug)]
pub(super) struct Pending {
    pub agent: String,
    pub model: Option<String>,
    pub title: String,
    pub label: String,
}

#[derive(Debug)]
pub(super) struct Draft {
    pub(super) text: String,
    pub(super) images: Vec<ChatImage>,
}

impl ChatView {
    pub(super) fn choose(&mut self, setting: String, value: String, settled: &mut Settled) {
        if self.handoff_sent.is_some() {
            return;
        }
        if let Some(agent) = picker::switch_to(&setting) {
            let Some(provider) = self.node.providers.iter().find(|p| p.agent == agent) else {
                return;
            };
            let label = match &provider.models {
                ProviderModels::Known(models) => models
                    .iter()
                    .find(|model| model.value == value)
                    .map(|model| model.name.clone()),
                _ => None,
            }
            .unwrap_or_else(|| provider.title.clone());
            self.handoff = Some(Pending {
                agent: agent.to_owned(),
                model: (!value.is_empty()).then_some(value),
                title: provider.title.clone(),
                label,
            });
            self.follow = true;
        } else {
            if self
                .setting(&setting)
                .is_some_and(|s| s.kind == SettingKind::Model)
            {
                self.handoff = None;
            }
            settled.actions.push(ChatAction::Set { setting, value });
        }
    }

    pub(super) fn choice_rows(&self, menu: &Menu) -> Vec<picker::Row> {
        let mut rows = picker::rows(&self.offer(), menu);
        if let Some(pending) = &self.handoff {
            for row in &mut rows {
                if let picker::Row::Choice(choice) = row
                    && (picker::switch_to(&choice.setting).is_some()
                        || self
                            .setting(&choice.setting)
                            .is_some_and(|s| s.kind == SettingKind::Model))
                {
                    choice.current = picker::switch_to(&choice.setting) == Some(&pending.agent)
                        && pending.model.as_deref().unwrap_or("") == choice.value;
                }
            }
            // The new provider's reasoning choices are not known until it opens.
            rows.retain(|row| !matches!(row, picker::Row::Slider(_)));
        }
        rows
    }

    pub(super) fn send_handoff(&mut self, text: String, settled: &mut Settled) {
        if !self.handoff_supported {
            self.say("Update the daemon to send a pending provider handoff.".into());
            return;
        }
        let Some(pending) = &self.handoff else { return };
        let images = std::mem::take(&mut self.images);
        settled.actions.push(ChatAction::SendTo {
            agent: pending.agent.clone(),
            model: pending.model.clone(),
            budget: 0,
            text,
            images: images.clone(),
        });
        self.handoff_sent = Some(Draft {
            text: self.editor.raw_text().to_owned(),
            images,
        });
        self.editor.set_text("");
        self.follow = true;
        self.picked = None;
    }

    pub(crate) fn handoff_answer(&mut self, response: Response) {
        match response {
            Response::Done => {
                self.handoff_sent = None;
                self.handoff = None;
            }
            Response::Error(error) => self.handoff_failed(&error.to_string()),
            _ => self.handoff_failed(
                "The message was not confirmed. Check the history before retrying.",
            ),
        }
    }

    pub(super) fn handoff_failed(&mut self, why: &str) {
        let Some(draft) = self.handoff_sent.take() else {
            return;
        };
        let newer = self.editor.raw_text();
        let text = if newer.is_empty() {
            draft.text
        } else {
            format!("{}\n{newer}", draft.text)
        };
        self.editor.set_text(&text);
        let mut images = draft.images;
        images.append(&mut self.images);
        self.images = images;
        self.say(why.to_owned());
    }
}
