//! How far chats agents start may go: the limits on chats talking to each
//! other (see `server/tools/talk.rs`), which the user sets in the
//! daemon's config, as `daemon.subagents` (see [`crate::settings`]):
//!
//! ```lua
//! daemon.subagents.working = 16       -- chats agents started working at once, in all
//! daemon.subagents.default = 6        -- a new chat's Subagents setting
//! daemon.subagents.open = 32          -- open under one of the user's chats
//! daemon.subagents.started = 128      -- started under one, since the user last wrote there
//! daemon.subagents.depth = 3          -- how deep chats agents start go
//! daemon.subagents.hops = 6           -- the longest chain of messages between agents
//! daemon.subagents.notice_turns = 100 -- turns notices start, since the user last wrote
//! ```
//!
//! A value out of its range is refused, and the config with it: never
//! brought into range, so a typo doesn't quietly become a limit no one
//! chose.

/// The limits on chats agents start, as the daemon's config sets them.
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

/// The deepest chats may be set to go: as deep as a saved file's chain of
/// parents is followed (see `server/restore.rs`).
pub(crate) const DEEPEST: u8 = 8;

impl SubagentLimits {
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
        crate::settings::Settings::from_lua(text)
            .unwrap()
            .agents()
            .subagents
    }

    #[test]
    fn the_limits_are_read_and_those_out_of_range_refused() {
        assert_eq!(limits(""), SubagentLimits::default());
        let set = limits(
            "daemon.subagents.working = 8\ndaemon.subagents.default = 3\n\
             daemon.subagents.open = 10\ndaemon.subagents.started = 20\n\
             daemon.subagents.depth = 4\ndaemon.subagents.hops = 2\n\
             daemon.subagents.notice_turns = 5\n",
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
        // Never brought into range: refused, saying where.
        for wrong in [
            "daemon.subagents.working = 0",
            "daemon.subagents.depth = 9",
            "daemon.subagents.hops = \"six\"",
            "daemon.subagents.open = 1000",
            "daemon.subagents.started = -1",
            "daemon.subagents.wide = 1",
        ] {
            let failure = crate::settings::Settings::from_lua(wrong).unwrap_err();
            assert_eq!(failure.at.map(|at| at.line), Some(1), "{wrong}");
        }
        let failure =
            crate::settings::Settings::from_lua("daemon.subagents.depth = 9").unwrap_err();
        assert_eq!(
            failure.said,
            "daemon/init.lua:1: daemon.subagents.depth is a whole number from 1 to 8"
        );
        // The default can't be more than may work at once.
        let over = crate::settings::Settings::from_lua(
            "daemon.subagents.working = 4\ndaemon.subagents.default = 6",
        )
        .unwrap_err();
        assert!(over.said.starts_with("daemon/init.lua:2: "), "{over:?}");
        let off = limits("daemon.subagents.default = 0\n");
        assert_eq!(off.default, 0);
    }

    #[test]
    fn the_setting_offers_off_and_the_usual_numbers_up_to_the_ceiling() {
        assert_eq!(SubagentLimits::default().offered(), [0, 2, 4, 6, 8, 12, 16]);
        let few = limits("daemon.subagents.working = 5\ndaemon.subagents.default = 3\n");
        assert_eq!(few.offered(), [0, 2, 3, 4]);
        let many = limits("daemon.subagents.working = 64\n");
        assert_eq!(many.offered(), [0, 2, 4, 6, 8, 12, 16]);
    }
}
