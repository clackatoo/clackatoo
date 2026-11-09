//! The kitty keyboard protocol's flags
//! (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>).
//!
//! xterm's encoding of keys can't tell some apart: Esc from the start of
//! a sequence, Ctrl+I from Tab, Shift+Enter from Enter. A program asks for
//! a richer encoding a flag at a time, and pushes what it asked (`CSI >
//! flags u`) so that it can pop it again as it ends (`CSI < count u`),
//! leaving what the program that started it asked. It can also change
//! what's on top (`CSI = flags ; mode u`), and ask what that is (`CSI ?
//! u`). Each screen has a stack of its own, so a full-screen program on
//! the alternate screen leaves the shell's flags alone.
//!
//! The emulator only keeps what was asked: windows encode the keys (see
//! [`Terminal::keyboard`](crate::Terminal::keyboard)).

/// Esc, and keys with Ctrl or Alt, are sent as `CSI u` sequences that
/// can't be mistaken for anything else.
pub const DISAMBIGUATE: u8 = 1;

/// Key repeats and releases are sent too.
pub const EVENT_TYPES: u8 = 2;

/// Keys sent as `CSI u` sequences also say the key Shift made, and the
/// key in the standard (US) layout.
pub const ALTERNATE_KEYS: u8 = 4;

/// Every key is sent as an escape sequence, even one that types text.
pub const ALL_KEYS: u8 = 8;

/// A key sent as an escape sequence also says the text it types.
pub const TEXT: u8 = 16;

/// The flags clackatoo's windows act on. A program asking for others gets
/// these alone, and sees so when it asks: windows don't send key repeats
/// and releases yet, so [`EVENT_TYPES`] is never set.
pub const SUPPORTED: u8 = DISAMBIGUATE | ALTERNATE_KEYS | ALL_KEYS | TEXT;

/// How many sets of flags a screen's stack keeps. Pushing past it drops
/// the oldest, as kitty does.
pub(crate) const DEPTH: usize = 16;

/// The flags a sequence's parameter asks for, of those there are.
pub(crate) fn flags(param: u16) -> u8 {
    u8::try_from(param & 0x1F).expect("five bits fit a byte")
}

/// One screen's stack of flags.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stack {
    /// The flags with nothing pushed, which `CSI = u` sets then.
    base: u8,
    /// What was pushed, oldest first. The last is in effect.
    pushed: Vec<u8>,
}

impl Stack {
    /// A stack as [`parts`](Self::parts) gave it, of flags clackatoo
    /// acts on alone, and only as many as are kept.
    pub(crate) fn from_parts(base: u8, pushed: &[u8]) -> Self {
        let kept = &pushed[pushed.len().saturating_sub(DEPTH)..];
        Self {
            base: base & SUPPORTED,
            pushed: kept.iter().map(|&flags| flags & SUPPORTED).collect(),
        }
    }

    /// The flags with nothing pushed, and what was pushed, oldest first.
    pub(crate) fn parts(&self) -> (u8, &[u8]) {
        (self.base, &self.pushed)
    }

    /// The flags in effect.
    pub(crate) fn flags(&self) -> u8 {
        self.pushed.last().copied().unwrap_or(self.base)
    }

    /// `CSI = flags ; mode u`: the flags in effect become `flags` (mode
    /// 1), gain them (2) or lose them (3).
    pub(crate) fn set(&mut self, flags: u8, mode: u16) {
        let current = self.pushed.last_mut().unwrap_or(&mut self.base);
        let flags = flags & SUPPORTED;
        *current = match mode {
            1 => flags,
            2 => *current | flags,
            3 => *current & !flags,
            _ => return,
        };
    }

    /// `CSI > flags u`: `flags` take effect, until popped.
    pub(crate) fn push(&mut self, flags: u8) {
        if self.pushed.len() == DEPTH {
            self.pushed.remove(0);
        }
        self.pushed.push(flags & SUPPORTED);
    }

    /// `CSI < count u`: the last `count` pushed are popped. Popping all
    /// that were turns every flag off, as the protocol says.
    pub(crate) fn pop(&mut self, count: usize) {
        if count >= self.pushed.len() {
            self.pushed.clear();
            self.base = 0;
        } else {
            self.pushed.truncate(self.pushed.len() - count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_pushed_takes_effect_until_popped() {
        let mut stack = Stack::default();
        assert_eq!(stack.flags(), 0);
        stack.push(DISAMBIGUATE);
        stack.push(DISAMBIGUATE | ALL_KEYS);
        assert_eq!(stack.flags(), DISAMBIGUATE | ALL_KEYS);
        stack.pop(1);
        assert_eq!(stack.flags(), DISAMBIGUATE);
        stack.pop(1);
        assert_eq!(stack.flags(), 0);
    }

    #[test]
    fn setting_changes_only_the_top() {
        let mut stack = Stack::default();
        stack.set(DISAMBIGUATE, 1);
        assert_eq!(stack.flags(), DISAMBIGUATE);
        stack.push(ALL_KEYS);
        stack.set(TEXT, 2);
        assert_eq!(stack.flags(), ALL_KEYS | TEXT);
        stack.set(ALL_KEYS, 3);
        assert_eq!(stack.flags(), TEXT);
        stack.set(ALL_KEYS, 9);
        assert_eq!(stack.flags(), TEXT, "an unknown mode changes nothing");
        stack.pop(1);
        assert_eq!(stack.flags(), 0, "popping everything turns all off");
    }

    #[test]
    fn popping_more_than_was_pushed_turns_everything_off() {
        let mut stack = Stack::default();
        stack.set(DISAMBIGUATE, 1);
        stack.push(ALL_KEYS);
        stack.pop(5);
        assert_eq!(stack.flags(), 0);
    }

    #[test]
    fn only_flags_windows_act_on_are_kept() {
        let mut stack = Stack::default();
        stack.push(flags(0xFFFF));
        assert_eq!(stack.flags(), SUPPORTED);
        assert_eq!(stack.flags() & EVENT_TYPES, 0);
        stack.set(EVENT_TYPES | TEXT, 1);
        assert_eq!(stack.flags(), TEXT);
    }

    #[test]
    fn the_stack_keeps_the_newest() {
        let mut stack = Stack::default();
        for _ in 0..DEPTH {
            stack.push(DISAMBIGUATE);
        }
        stack.push(TEXT);
        assert_eq!(stack.parts().1.len(), DEPTH);
        stack.pop(1);
        assert_eq!(stack.flags(), DISAMBIGUATE);
        // What's put back is kept to the same bounds.
        let pushed = vec![0xFF; DEPTH + 3];
        let stack = Stack::from_parts(0xFF, &pushed);
        assert_eq!(stack.parts(), (SUPPORTED, &[SUPPORTED; DEPTH][..]));
    }
}
