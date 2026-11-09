//! Original-image retention is separate from transcript snapshots.
use clackatoo_protocol::ImageId;

use super::{HostedChat, Item, ToolContent, handover, index_u32};
impl HostedChat {
    /// What's handed over of the chat's thumbnails' originals: which
    /// thumbnail has which handle, and which image each handle is in the
    /// store, whose bytes are handed over with it.
    pub(crate) fn hand_over_originals(&self) -> (handover::ImageOriginals, handover::ChatImages) {
        let mut references = Vec::new();
        for (turn, value) in self.node.turns.iter().enumerate() {
            for (index, image) in value.previews.iter().enumerate() {
                if image.original != 0 {
                    references.push((index_u32(turn), None, index_u32(index), image.original));
                }
            }
            for item in &value.items {
                if let Some(tool) = item.tool() {
                    for (index, image) in tool
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            ToolContent::Image(image) => Some(image),
                            _ => None,
                        })
                        .enumerate()
                    {
                        if image.original != 0 {
                            references.push((
                                index_u32(turn),
                                Some(tool.id.clone()),
                                index_u32(index),
                                image.original,
                            ));
                        }
                    }
                }
            }
        }
        self.originals.hand_over(self.id.0, references)
    }

    /// Takes back the thumbnails' handles handed over, and with `images`,
    /// from a build of version 47 or later, the images in the store each
    /// handle is: from an earlier build, the originals handed over are kept
    /// in the store again.
    pub(crate) fn resume_originals(
        &mut self,
        records: &mut [handover::ImageOriginals],
        images: Option<&[handover::ChatImages]>,
    ) {
        let Some(record) = records.iter_mut().find(|r| r.chat == self.id.0) else {
            return;
        };
        let handles = images.map(|images| {
            (images.iter())
                .find(|images| images.chat == self.id.0)
                .unwrap_or(&handover::ChatImages {
                    chat: self.id.0,
                    handles: Vec::new(),
                })
                .clone()
        });
        self.originals = crate::tool_images::Originals::resume(record, handles.as_ref());
        for (turn, tool, index, id) in &record.references {
            let Some(turn) = self.node.turns.get_mut(*turn as usize) else {
                continue;
            };
            let image = if let Some(name) = tool {
                (turn.items.iter_mut())
                    .filter_map(Item::tool_mut)
                    .find(|tool| &tool.id == name)
                    .and_then(|tool| {
                        (tool.content.iter_mut())
                            .filter_map(|c| match c {
                                ToolContent::Image(image) => Some(image),
                                _ => None,
                            })
                            .nth(*index as usize)
                    })
            } else {
                turn.previews.get_mut(*index as usize)
            };
            if let Some(image) = image {
                image.original = *id;
                image.image = self.originals.get(*id);
            }
        }
    }

    /// The store keeps the original of the thumbnails with handle
    /// `original` as `image`: they name it, for clients that show images
    /// through the store. False if none does.
    pub(crate) fn image_kept(&mut self, original: u64, image: ImageId) -> bool {
        if original == 0 || !self.originals.kept(original, image) {
            return false;
        }
        let mut changed = Vec::new();
        for (at, turn) in self.node.turns.iter_mut().enumerate() {
            let previews = (turn.previews.iter_mut()).chain(
                (turn.items.iter_mut())
                    .filter_map(Item::tool_mut)
                    .flat_map(|tool| tool.content.iter_mut())
                    .filter_map(|content| match content {
                        ToolContent::Image(image) => Some(image),
                        _ => None,
                    }),
            );
            let mut named = false;
            for preview in previews.filter(|preview| preview.original == original) {
                preview.image = Some(image);
                named = true;
            }
            if named {
                changed.push(at);
            }
        }
        for at in &changed {
            self.patch_turn(*at);
        }
        !changed.is_empty()
    }
}
