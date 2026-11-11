//! Retained context is recovered only against the exact provider-replayed prompt.
use super::{HostedChat, TurnUid};
use clackatoo_protocol::ContextSnapshot;

impl HostedChat {
    pub(crate) fn restore_context_record(
        &mut self,
        index: usize,
        uid: TurnUid,
        original: &str,
        text: String,
        context: Vec<ContextSnapshot>,
    ) {
        if self.turn_uid(index) != Some(uid)
            || self
                .node
                .turns
                .get(index)
                .is_none_or(|t| t.prompt != original)
        {
            return;
        }
        self.node.turns[index].prompt = text;
        self.node.turns[index].context = context;
        self.patch_turn(index);
        self.node.prompts = clackatoo_protocol::recent_prompts(&self.node.turns);
    }
}
