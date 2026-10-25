//! In-memory photo drafts belong to their original chat, including while a
//! picker decodes or a submission waits for acknowledgment.

use super::State;
use clackatoo_protocol::{ChatImage, TerminalId};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Attachment {
    pub id: u64,
    pub image: Arc<ChatImage>,
}

impl State {
    /// Adds a prepared image to this chat's local draft.
    ///
    /// # Errors
    /// Refuses invalid images or per-message/page memory limits.
    pub fn add_image(&mut self, chat: TerminalId, image: ChatImage) -> Result<(), String> {
        if !image.valid() {
            return Err("This image is too large. Choose a smaller image.".into());
        }
        let current = self.images.entry(chat).or_default();
        if current.len() >= ChatImage::MAX_COUNT {
            return Err("Attach up to four photos per message.".into());
        }
        if current.iter().map(|a| a.image.data.len()).sum::<usize>() + image.data.len()
            > ChatImage::MAX_DATA
        {
            return Err(
                "These photos exceed the message size limit. Remove one or choose smaller photos."
                    .into(),
            );
        }
        if self
            .images
            .values()
            .flatten()
            .map(|a| a.image.data.len())
            .sum::<usize>()
            + image.data.len()
            > 4 * ChatImage::MAX_DATA
        {
            return Err(
                "Photo drafts are full. Send or remove photos in another chat first.".into(),
            );
        }
        self.next_image += 1;
        self.images.entry(chat).or_default().push(Attachment {
            id: self.next_image,
            image: Arc::new(image),
        });
        Ok(())
    }

    pub fn remove_image(&mut self, chat: TerminalId, id: u64) {
        if let Some(images) = self.images.get_mut(&chat) {
            images.retain(|a| a.id != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clackatoo_protocol::{ChatAction, ClientMessage, Request, Response, ServerMessage};

    fn image() -> ChatImage {
        ChatImage {
            data: "png-data".into(),
            width: 2,
            height: 3,
            preview: None,
        }
    }

    fn state() -> State {
        let mut state = State::default();
        state.connected();
        state.open_chat(TerminalId(4));
        state.asked.clear();
        state.take_outbox();
        state
    }

    #[test]
    fn image_only_submit_clears_only_the_photos_that_were_accepted() {
        let mut s = state();
        let chat = TerminalId(4);
        s.add_image(chat, image()).unwrap();
        s.submit(false);
        let out = s.take_outbox();
        assert!(matches!(&out[0], ClientMessage::Request(Request::Chat {
            action: ChatAction::Submit {text,images,..},..
        }) if text.is_empty() && images == &[image()]));
        s.add_image(chat, image()).unwrap();
        let new = s.images[&chat][1].id;
        s.receive(ServerMessage::Response(Response::Done));
        assert_eq!(
            s.images[&chat].iter().map(|a| a.id).collect::<Vec<_>>(),
            [new]
        );
    }

    #[test]
    fn rejected_and_unconfirmed_photos_survive_without_replay() {
        let mut s = state();
        let chat = TerminalId(4);
        s.add_image(chat, image()).unwrap();
        s.submit(true);
        s.take_outbox();
        s.receive(ServerMessage::Response(Response::Error(
            clackatoo_protocol::Error::NoSuchTerminal(chat),
        )));
        assert_eq!(s.images[&chat].len(), 1);
        s.submit(false);
        s.take_outbox();
        s.lost(1);
        s.connected();
        assert_eq!(s.images[&chat].len(), 1);
        assert!(s.take_outbox().iter().all(|m| !matches!(
            m,
            ClientMessage::Request(Request::Chat {
                action: ChatAction::Submit { .. },
                ..
            })
        )));
    }

    #[test]
    fn decoding_cannot_send_early_or_attach_to_a_different_chat() {
        let mut s = state();
        let original = TerminalId(4);
        s.drafts.insert(original, "with photos".into());
        s.loading_images.insert(original);
        s.submit(false);
        assert!(s.take_outbox().is_empty());
        s.open_chat(TerminalId(8));
        s.add_image(original, image()).unwrap();
        assert!(!s.images.contains_key(&TerminalId(8)));
        assert_eq!(s.images[&original].len(), 1);
    }

    #[test]
    fn photo_count_size_and_removal_are_bounded() {
        let mut s = state();
        let chat = TerminalId(4);
        for _ in 0..ChatImage::MAX_COUNT {
            s.add_image(chat, image()).unwrap();
        }
        assert!(s.add_image(chat, image()).is_err());
        let id = s.images[&chat][0].id;
        s.remove_image(chat, id);
        assert_eq!(s.images[&chat].len(), 3);
        let mut large = image();
        large.data = "x".repeat(ChatImage::MAX_DATA);
        assert!(s.add_image(chat, large).is_err());
        assert_eq!(s.images[&chat].len(), 3);
    }
}
