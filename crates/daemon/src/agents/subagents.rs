//! How far chats agents start may go: the limits on chats talking to each
//! other (see `server/tools/talk.rs`), which the user sets in the
//! `[subagents]` table of `agents.toml`:
//!
//! ```toml
//! [subagents]
//! working = 16       # chats agents started working at once, in all
//! default = 6        # a new chat's Subagents setting
//! open = 32          # open under one of the user's chats
//! started = 128      # started under one, since the user last wrote there
//! depth = 3          # how deep chats agents start go
//! hops = 6           # the longest chain of messages between agents
//! notice_turns = 100 # turns notices start, since the user last wrote
//! ```
//!
//! A value out of its range is refused, with a warning, and the default
//! used: never brought into range, so a typo doesn't quietly become a
//! limit no one chose.

use std::ops::RangeInclusive;

/// The limits on chats agents start, as `agents.toml` sets them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SubagentLimits {
    /// The most chats agents started that may work at once, under all of
    /// the user's chats together. Idle ones don't count.
    pub(crate) working: u8,
    /// The Subagents setting a chat of the user's starts with: how many
    /// chats agents started under it may work at once. 0 is off.
    pub(crate) default: u8,
    /// The most chats agents started, idle or working, open under any one
    /// of the user's chats.
    pub(crate) open: usize,
    /// The most chats agents start under one of the user's chats between
    /// two of the user's messages there.
    pub(crate) started: u32,
    /// How deep chats agents start go: the user's are 0 deep, the chats
    /// their agents start 1. A chat this deep starts none.
    pub(crate) depth: u8,
    /// The longest chain of messages, each sent in the turn the one
    /// before started.
    pub(crate) hops: u8,
    /// The most turns notices that chats finished start under one of the
    /// user's chats between two of the user's messages there.
    pub(crate) notice_turns: u32,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            working: 16,
            default: 6,
            open: 32,
            started: 128,
            depth: 3,
            hops: 6,
            notice_turns: 100,
        }
    }
}

/// What the Subagents setting offers, as well as the default: off, and
/// these, up to [`SubagentLimits::working`].
const OFFERED: [u8; 7] = [0, 2, 4, 6, 8, 12, 16];

/// The keys of the `[subagents]` table.
const KEYS: [&str; 7] = [
    "working",
    "default",
    "open",
    "started",
    "depth",
    "hops",
    "notice_turns",
];

/// The deepest chats may be set to go: as deep as a saved file's chain of
/// parents is followed (see `server/restore.rs`).
pub(crate) const DEEPEST: u8 = 8;

impl SubagentLimits {
    /// The limits the `[subagents]` table of `document` sets, each key it
    /// doesn't set, or sets out of range, at its default, and what's wrong
    /// in it, to log: the caller logs it only as it changes.
    pub(crate) fn from_document(document: &toml_edit::DocumentMut) -> (Self, Vec<String>) {
        let mut limits = Self::default();
        let wrong = std::cell::RefCell::new(Vec::new());
        let Some(table) = document.get("subagents") else {
            return (limits, Vec::new());
        };
        let Some(table) = table.as_table_like() else {
            return (limits, vec!["subagents is a table of limits".to_owned()]);
        };
        let read = |key: &str, range: RangeInclusive<i64>| -> Option<i64> {
            let item = table.get(key)?;
            match item.as_integer() {
                Some(value) if range.contains(&value) => Some(value),
                _ => {
                    wrong.borrow_mut().push(format!(
                        "subagents.{key} is a whole number from {} to {}: the default is used",
                        range.start(),
                        range.end()
                    ));
                    None
                }
            }
        };
        let small = |value: i64| u8::try_from(value).ok();
        let large = |value: i64| u32::try_from(value).ok();
        if let Some(value) = read("working", 1..=64).and_then(small) {
            limits.working = value;
        }
        // The default fits under the ceiling, whichever was set.
        limits.default = limits.default.min(limits.working);
        let working = i64::from(limits.working);
        if let Some(value) = read("default", 0..=working).and_then(small) {
            limits.default = value;
        }
        if let Some(value) = read("open", 1..=256).and_then(|value| usize::try_from(value).ok()) {
            limits.open = value;
        }
        if let Some(value) = read("started", 1..=10_000).and_then(large) {
            limits.started = value;
        }
        if let Some(value) = read("depth", 1..=i64::from(DEEPEST)).and_then(small) {
            limits.depth = value;
        }
        if let Some(value) = read("hops", 1..=64).and_then(small) {
            limits.hops = value;
        }
        if let Some(value) = read("notice_turns", 1..=10_000).and_then(large) {
            limits.notice_turns = value;
        }
        let mut wrong = wrong.into_inner();
        for (key, _) in table.iter() {
            if !KEYS.contains(&key) {
                wrong.push(format!("subagents.{key} isn't a limit clackatoo knows"));
            }
        }
        (limits, wrong)
    }

    /// The values the Subagents setting offers: off, the usual numbers up
    /// to [`working`](Self::working), and the default, in order.
    pub(crate) fn offered(&self) -> Vec<u8> {
        let mut offered: Vec<u8> = (OFFERED.iter())
            .copied()
            .filter(|&value| value <= self.working)
            .collect();
        if !offered.contains(&self.default) {
            offered.push(self.default);
            offered.sort_unstable();
        }
        offered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(text: &str) -> SubagentLimits {
        SubagentLimits::from_document(&text.parse().unwrap()).0
    }

    #[test]
    fn the_limits_are_read_and_those_out_of_range_left_at_their_defaults() {
        assert_eq!(limits(""), SubagentLimits::default());
        let set = limits(
            "[subagents]\nworking = 8\ndefault = 3\nopen = 10\nstarted = 20\ndepth = 4\n\
             hops = 2\nnotice_turns = 5\n",
        );
        let wanted = SubagentLimits {
            working: 8,
            default: 3,
            open: 10,
            started: 20,
            depth: 4,
            hops: 2,
            notice_turns: 5,
        };
        assert_eq!(set, wanted);
        // Never brought into range: the default instead.
        let wrong = limits(
            "[subagents]\nworking = 0\ndepth = 9\nhops = \"six\"\nopen = 1000\nstarted = -1\n",
        );
        assert_eq!(wrong, SubagentLimits::default());
        let document = "[subagents]\ndepth = 9\nwide = 1\n".parse().unwrap();
        let (_, said) = SubagentLimits::from_document(&document);
        assert_eq!(
            said,
            [
                "subagents.depth is a whole number from 1 to 8: the default is used",
                "subagents.wide isn't a limit clackatoo knows",
            ]
        );
        // The default can't be more than may work at once.
        let over = limits("[subagents]\nworking = 4\ndefault = 6\n");
        assert_eq!((over.working, over.default), (4, 4));
        let off = limits("[subagents]\ndefault = 0\n");
        assert_eq!(off.default, 0);
        assert_eq!(limits("subagents = 3\n"), SubagentLimits::default());
    }

    #[test]
    fn the_setting_offers_off_and_the_usual_numbers_up_to_the_ceiling() {
        assert_eq!(SubagentLimits::default().offered(), [0, 2, 4, 6, 8, 12, 16]);
        let few = limits("[subagents]\nworking = 5\ndefault = 3\n");
        assert_eq!(few.offered(), [0, 2, 3, 4]);
        let many = limits("[subagents]\nworking = 64\n");
        assert_eq!(many.offered(), [0, 2, 4, 6, 8, 12, 16]);
    }
}
