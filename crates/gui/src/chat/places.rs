//! Saved chat positions use absolute turns so loading older history does
//! not move them. Branch replacement invalidates positions conservatively.
use super::paint::{Position, TextKey};
use super::{ChatView, Input};
use crate::vim::place::ChatPlace;
impl ChatView {
    pub(crate) fn place(&self) -> Option<ChatPlace> {
        self.saved_place(self.cursor?)
    }
    pub(super) fn saved_place(&self, at: Position) -> Option<ChatPlace> {
        // Live activity cards have provisional indices; completed items do not.
        let turn = self.node.turns.get(at.text.turn)?;
        if at.text.piece >= super::paint::ITEMS + turn.items.len() {
            return None;
        }
        Some(ChatPlace {
            turn: u64::from(self.node.first_turn) + at.text.turn as u64,
            sent: self.node.turns.get(at.text.turn)?.sent,
            generation: self.place_generation,
            piece: at.text.piece,
            part: at.text.part,
            byte: at.byte,
        })
    }
    pub(crate) fn valid_place(&self, at: &ChatPlace) -> bool {
        let Some(turn) = at
            .turn
            .checked_sub(u64::from(self.node.first_turn))
            .and_then(|n| usize::try_from(n).ok())
        else {
            return false;
        };
        at.generation == self.place_generation
            && self
                .node
                .turns
                .get(turn)
                .is_some_and(|turn| turn.sent == at.sent)
    }
    pub(crate) fn show_place(&mut self, at: ChatPlace, exact: bool) -> bool {
        if !self.valid_place(&at) {
            return false;
        }
        self.queued.push(Input::Place { at, exact });
        true
    }
    pub(super) fn settle_place(
        &mut self,
        fonts: &mut crate::fonts::Fonts,
        at: ChatPlace,
        exact: bool,
    ) {
        if !self.valid_place(&at) {
            self.failed = true;
            return;
        }
        let turn =
            usize::try_from(at.turn - u64::from(self.node.first_turn)).expect("validated turn");
        self.reveal_piece(turn, at.piece);
        self.lay_out_keeping_view(fonts, turn);
        let key = TextKey {
            turn,
            piece: at.piece,
            part: at.part,
        };
        if !self.turn_texts(turn).contains(&key) {
            self.failed = true;
            return;
        }
        let Some(text) = self.text(key) else {
            self.failed = true;
            return;
        };
        let mut byte = super::paint::within(text.text, at.byte);
        if !exact {
            let start = text.text[..byte].rfind('\n').map_or(0, |n| n + 1);
            byte = start
                + text.text[start..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .map(char::len_utf8)
                    .sum::<usize>();
        }
        self.go(Position { text: key, byte });
        self.goal = None;
        self.follow_selection();
    }
    pub(crate) fn take_left_place(&mut self) -> Option<ChatPlace> {
        self.left_place.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::fixtures::{fonts, paint, read, talk};

    #[test]
    fn saved_places_survive_history_prepending_and_reject_branch_replacement() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), talk(&[("prompt", "alpha beta")]));
        chat.node.first_turn = 5;
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg}w");
        let saved = chat.place().unwrap();
        let older = talk(&[("older", "old")]).turns.remove(0);
        chat.node.turns.insert(0, older);
        chat.node.first_turn -= 1;
        chat.prepend_history(1);
        assert!(chat.valid_place(&saved));
        chat.settle_place(&mut fonts, saved, true);
        assert_eq!(chat.place(), Some(saved));
        chat.cut_from(1);
        assert!(!chat.valid_place(&saved));
    }

    #[test]
    fn exact_and_line_marks_restore_utf8_text_after_layout_changes() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(
            String::new(),
            talk(&[("prompt", "```\n  中文 alpha\n  beta\n```")]),
        );
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg}^w");
        let saved = chat.place().unwrap();
        read(&mut chat, "G");
        chat.restyled(0);
        chat.settle_place(&mut fonts, saved, true);
        assert_eq!(chat.place(), Some(saved));
        chat.settle_place(&mut fonts, saved, false);
        assert_eq!(chat.place().unwrap().byte, 2);
    }
}
