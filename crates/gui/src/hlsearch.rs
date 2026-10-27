//! Every match of the last search in view lit up, as Vim's `hlsearch`
//! lights them, in editors, in terminals' normal mode and in chats: from a
//! search, `n` or `N` until Esc or `:nohlsearch`, the match the cursor went
//! to brighter than the rest. With the option off, only that one's lit.
//!
//! What's lit is looked for only in what's drawn, no more than
//! [`MOST_LIT`] matches in a block of it (a chat's block of text, an
//! editor's or a terminal's line), and only when that, or the search,
//! changes: a frame that draws the same text in the same places takes what
//! was found last (see [`Memo`]). Each pane works out what it draws in its
//! own terms, and matches as its search does: editors and chats with Vim's
//! patterns ([`crate::editor::Pattern::matches_in`]), terminals as the
//! daemon's search does (`clackatoo_term::matches_in`), so what's lit is
//! what `n` goes to.

use std::cell::RefCell;

/// The most matches lit up in one block of what's drawn.
pub(crate) const MOST_LIT: usize = 512;

/// How far past what's drawn, and before it, the text is looked through
/// for matches drawn, in bytes: for what's around a match that it looks at
/// (`^`, `\<`, `$`), and for a match that starts in view and goes on past
/// it.
pub(crate) const AROUND: usize = 4 << 10;

/// `text` up to [`AROUND`] bytes past byte `end`, back to a character's
/// start, so a search for matches before `end` that finds none never goes
/// through the rest of a long text. Only a match ending near the cut could
/// differ from what the whole text has, as `$` matches there, and one of
/// those starting before `end` would be thousands of bytes long.
pub(crate) fn cut(text: &str, end: usize) -> &str {
    let mut cut = end.saturating_add(AROUND).min(text.len());
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

#[cfg(test)]
thread_local! {
    /// How many times what's lit has been looked for, for tests.
    pub(crate) static SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The matches lit, `M` each, last found in what `K` says: which search,
/// and what's drawn, where.
#[derive(Debug)]
pub(crate) struct Memo<K, M> {
    kept: RefCell<Option<(K, Vec<M>)>>,
}

impl<K, M> Default for Memo<K, M> {
    fn default() -> Self {
        Self {
            kept: RefCell::new(None),
        }
    }
}

impl<K: PartialEq, M: Clone> Memo<K, M> {
    /// The matches lit in what `key` says, as kept if they were last found
    /// in it, or else as `find` finds them now.
    pub(crate) fn get(&self, key: K, find: impl FnOnce() -> Vec<M>) -> Vec<M> {
        if let Some((kept, lit)) = &*self.kept.borrow()
            && *kept == key
        {
            return lit.clone();
        }
        #[cfg(test)]
        SCANS.with(|scans| scans.set(scans.get() + 1));
        let lit = find();
        *self.kept.borrow_mut() = Some((key, lit.clone()));
        lit
    }

    /// Forgets what was found, as nothing's lit any more.
    pub(crate) fn forget(&self) {
        self.kept.borrow_mut().take();
    }
}
