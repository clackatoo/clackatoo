//! Questions about the pieces of code in buffers, in the event loop: where
//! the functions, classes and arguments around a place are, for text
//! objects such as `af`, and where the next ones start, for motions such
//! as `]f`.
//!
//! They come as [`ClientMessage::Query`](clackatoo_protocol::ClientMessage::Query)s,
//! as questions for language servers do, but only the thread parsing
//! buffers has their trees, so they go to it, in order with the buffer's
//! edits, and its answers come back through the loop's waker, as
//! highlights do.

use std::collections::HashMap;

use clackatoo_protocol::{Answer, BufferId, BufferUpdate, Query};
use clackatoo_syntax::{Answered, Ask};
use clackatoo_text::Bias;
use mio::Token;

use super::Daemon;
use super::buffers::key;

/// The questions on their way to the thread parsing buffers, by the
/// number it answers them with.
#[derive(Debug, Default)]
pub(super) struct PieceQuestions {
    asked: HashMap<u64, Asked>,
    next_ticket: u64,
}

/// A question a client asked.
#[derive(Debug)]
struct Asked {
    token: Token,
    buffer: BufferId,
    id: u64,
}

impl Daemon {
    /// Answers `query`, which is for the grammar of buffer `buffer`, about
    /// byte `at` of its text at `version`, for the client at `token`,
    /// which numbered it `id`. It's always answered, if only to say why
    /// it can't be, as the window holds keys typed until it is.
    pub(super) fn serve_pieces(
        &mut self,
        token: Token,
        buffer: BufferId,
        (id, version, at): (u64, u64, usize),
        query: Query,
    ) {
        let failed = |why: &str| Answer::Failed(why.to_owned());
        let Some(slot) = self.buffers.get(&buffer) else {
            return self.tell_answer(token, buffer, id, failed("the file isn't open"));
        };
        if !slot.viewers.contains(&token) {
            log::debug!("a query in buffer {buffer} from a client not following it");
            return self.tell_answer(token, buffer, id, failed("the file isn't followed"));
        }
        let Some(syntax) = self.syntax.as_ref().filter(|_| slot.parsed) else {
            // No grammar: no pieces.
            return self.tell_answer(token, buffer, id, Answer::Nothing);
        };
        if syntax.stopped() {
            let why = failed("the thread reading grammars has stopped");
            return self.tell_answer(token, buffer, id, why);
        }
        let ask = match query {
            Query::Pieces(construct) => Ask::Around(construct),
            Query::Starts {
                construct,
                backward,
                count,
            } => Ask::Starts {
                construct,
                backward,
                count: count as usize,
            },
            other => {
                log::error!("{other:?} isn't for a grammar");
                return self.tell_answer(token, buffer, id, failed("that isn't for a grammar"));
            }
        };
        // The place, moved along with edits made since the client's text,
        // such as an agent's.
        let Some(edits) = slot.buffer.edits_since(version) else {
            return self.tell_answer(token, buffer, id, failed("the file has changed since"));
        };
        let at = edits.fold(at, |at, edit| edit.map(at, Bias::Before));
        let questions = &mut self.piece_questions;
        let ticket = questions.next_ticket;
        questions.next_ticket += 1;
        syntax.ask(key(buffer), ticket, at, ask);
        questions.asked.insert(ticket, Asked { token, buffer, id });
    }

    /// Forgets the questions the client at `token` asked about `buffer`,
    /// or any buffer, as it's stopped following it, or gone.
    pub(super) fn forget_piece_questions(&mut self, token: Token, buffer: Option<BufferId>) {
        (self.piece_questions.asked).retain(|_, asked| {
            asked.token != token || buffer.is_some_and(|buffer| buffer != asked.buffer)
        });
    }

    /// Sends the client that asked the answer the thread parsing buffers
    /// gave, with its pieces moved along with edits made since the text it
    /// was about.
    pub(super) fn answered_pieces(&mut self, answered: Answered) {
        let Some(Asked { token, buffer, id }) = self.piece_questions.asked.remove(&answered.ticket)
        else {
            return;
        };
        let Some(slot) = self.buffers.get(&buffer) else {
            let failed = Answer::Failed("the file isn't open".into());
            return self.tell_answer(token, buffer, id, failed);
        };
        let answer = match (answered.pieces, slot.buffer.edits_since(answered.version)) {
            (None, _) => Answer::Failed("its grammar couldn't be loaded".into()),
            (Some(mut pieces), Some(edits)) => {
                for edit in edits {
                    for piece in &mut pieces {
                        piece.map(edit);
                    }
                }
                Answer::Pieces(pieces)
            }
            (Some(_), None) => Answer::Failed("the file has changed since".into()),
        };
        self.tell_answer(token, buffer, id, answer);
    }

    /// Answers every question still waiting, as the thread parsing buffers
    /// has stopped, and won't.
    pub(super) fn fail_piece_questions(&mut self) {
        let asked: Vec<Asked> = self
            .piece_questions
            .asked
            .drain()
            .map(|(_, asked)| asked)
            .collect();
        for Asked { token, buffer, id } in asked {
            let failed = Answer::Failed("the thread reading grammars has stopped".into());
            self.tell_answer(token, buffer, id, failed);
        }
    }

    /// Sends the client at `token` `answer` to its question numbered `id`
    /// about `buffer`, at the buffer's version, if it's open.
    fn tell_answer(&mut self, token: Token, buffer: BufferId, id: u64, answer: Answer) {
        let version = (self.buffers.get(&buffer)).map_or(0, |slot| slot.buffer.version());
        let update = BufferUpdate::Answer {
            id,
            version,
            answer,
        };
        self.tell(token, buffer, update);
    }
}
