//! Original-image retention is separate from transcript snapshots.
use super::{HostedChat, Item, ToolContent, handover, index_u32};
impl HostedChat {
    pub(crate) fn hand_over_originals(&self) -> handover::ImageOriginals {
        let mut references = Vec::new();
        for (turn, value) in self.node.turns.iter().enumerate() {
            for (index, image) in value.previews.iter().enumerate() {
                if image.original != 0 {
                    references.push((index_u32(turn), None, index_u32(index), image.original));
                }
            }
            for item in &value.items {
                if let Item::Tool(tool) = item {
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

    pub(crate) fn resume_originals(&mut self, records: &mut [handover::ImageOriginals]) {
        let Some(record) = records.iter_mut().find(|r| r.chat == self.id.0) else {
            return;
        };
        self.originals = crate::tool_images::Originals::resume(record);
        for (turn, tool, index, id) in &record.references {
            let Some(turn) = self.node.turns.get_mut(*turn as usize) else {
                continue;
            };
            let image = if let Some(name) = tool {
                turn.items.iter_mut().find_map(|item| match item {
                    Item::Tool(tool) if &tool.id == name => tool
                        .content
                        .iter_mut()
                        .filter_map(|c| match c {
                            ToolContent::Image(image) => Some(image),
                            _ => None,
                        })
                        .nth(*index as usize),
                    _ => None,
                })
            } else {
                turn.previews.get_mut(*index as usize)
            };
            if let Some(image) = image {
                image.original = *id;
            }
        }
    }
}
