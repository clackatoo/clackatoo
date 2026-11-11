//! Context uses the review's already-rendered hunks and never reads source here.
use super::Daemon;
use clackatoo_protocol::{ContextCandidate, ContextKind, ContextRef, ReviewHunk, TerminalId};
use mio::Token;

fn identity(path: &str, hunk: &ReviewHunk) -> u64 {
    ContextRef::hunk_id(path, hunk.line, &hunk.base, &hunk.text)
}

impl Daemon {
    pub(in crate::server) fn context_review_candidates(
        &self,
        token: Token,
        destination: TerminalId,
    ) -> Vec<ContextCandidate> {
        let Some(own) = self.chat_ref(destination) else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for review in self
            .reviews
            .list
            .values()
            .filter(|r| {
                !r.stale
                    && r.root == own.project()
                    && r.chat.is_none_or(|chat| self.may_see_terminal(token, chat))
            })
            .take(32)
        {
            for file in &review.context {
                for hunk in &file.hunks {
                    items.push(ContextCandidate {
                        kind: ContextKind::Change,
                        label: format!("{}:{}", file.shown, hunk.line),
                        detail: format!("{:?} · before / after", review.scope),
                        source: ContextRef::Change {
                            chat: review.chat.unwrap_or(destination),
                            path: file.path.clone(),
                            hunk: identity(&file.path, hunk),
                        },
                    });
                    if items.len() >= 500 {
                        return items;
                    }
                }
            }
        }
        items
    }
    pub(in crate::server) fn captured_review_hunk(
        &self,
        token: Token,
        destination: TerminalId,
        owner: TerminalId,
        path: &str,
        id: u64,
    ) -> Result<(String, String), String> {
        let root = self
            .chat_ref(destination)
            .ok_or("Destination chat closed.")?
            .project();
        for review in self
            .reviews
            .list
            .values()
            .filter(|r| {
                !r.stale
                    && r.root == root
                    && r.chat.unwrap_or(destination) == owner
                    && self.may_see_terminal(token, owner)
            })
            .take(32)
        {
            for file in review.context.iter().filter(|f| f.path == path) {
                if let Some(hunk) = file.hunks.iter().find(|h| identity(path, h) == id) {
                    return Ok((
                        format!("Change · {}:{}", file.shown, hunk.line),
                        format!(
                            "Comparison: {:?}\nFile: {path}:{}\n\nBefore:\n{}\nAfter:\n{}",
                            review.scope, hunk.line, hunk.base, hunk.text
                        ),
                    ));
                }
            }
        }
        Err(
            "That review hunk changed or is unavailable. Refresh the review and select it again."
                .into(),
        )
    }
}
