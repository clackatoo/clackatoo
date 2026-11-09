//! The mouse pointer's shape a program asks for over its terminal, with
//! kitty's `OSC 22` (<https://sw.kovidgoyal.net/kitty/pointer-shapes/>),
//! as a link or a draggable divider in it would.
//!
//! - `OSC 22 ; <name> ST` or `OSC 22 ; =<name> ST` sets the shape, in
//!   place of the one on top of the stack; an empty name sets the default.
//! - `OSC 22 ; ><name>[,<name>…] ST` pushes the first known of the names
//!   given, falling back as listed, and `OSC 22 ; < ST` pops it.
//! - `OSC 22 ; ?<name>[,<name>…] ST` asks whether each is known, answered
//!   `OSC 22 ; 1,0,… ST`; `__current__`, `__default__` and `__grabbed__`
//!   are answered with a known shape's name.
//!
//! Names are CSS's, and the X11 ones kitty reads besides. A name that
//! isn't known changes nothing, and what's answered is only ever made of
//! known names and digits, never what the program sent. Each screen has a
//! stack of its own, at most [`DEPTH`] deep, as kitty keeps them.

/// The pointer shapes a program can ask for: CSS's names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(missing_docs, reason = "each is named as CSS names it")]
pub enum Pointer {
    Default,
    None,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    EResize,
    NResize,
    NeResize,
    NwResize,
    SResize,
    SeResize,
    SwResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ColResize,
    RowResize,
    AllScroll,
    ZoomIn,
    ZoomOut,
}

/// Each shape and its CSS name.
const NAMES: [(Pointer, &str); 35] = [
    (Pointer::Default, "default"),
    (Pointer::None, "none"),
    (Pointer::ContextMenu, "context-menu"),
    (Pointer::Help, "help"),
    (Pointer::Pointer, "pointer"),
    (Pointer::Progress, "progress"),
    (Pointer::Wait, "wait"),
    (Pointer::Cell, "cell"),
    (Pointer::Crosshair, "crosshair"),
    (Pointer::Text, "text"),
    (Pointer::VerticalText, "vertical-text"),
    (Pointer::Alias, "alias"),
    (Pointer::Copy, "copy"),
    (Pointer::Move, "move"),
    (Pointer::NoDrop, "no-drop"),
    (Pointer::NotAllowed, "not-allowed"),
    (Pointer::Grab, "grab"),
    (Pointer::Grabbing, "grabbing"),
    (Pointer::EResize, "e-resize"),
    (Pointer::NResize, "n-resize"),
    (Pointer::NeResize, "ne-resize"),
    (Pointer::NwResize, "nw-resize"),
    (Pointer::SResize, "s-resize"),
    (Pointer::SeResize, "se-resize"),
    (Pointer::SwResize, "sw-resize"),
    (Pointer::WResize, "w-resize"),
    (Pointer::EwResize, "ew-resize"),
    (Pointer::NsResize, "ns-resize"),
    (Pointer::NeswResize, "nesw-resize"),
    (Pointer::NwseResize, "nwse-resize"),
    (Pointer::ColResize, "col-resize"),
    (Pointer::RowResize, "row-resize"),
    (Pointer::AllScroll, "all-scroll"),
    (Pointer::ZoomIn, "zoom-in"),
    (Pointer::ZoomOut, "zoom-out"),
];

/// The X11 cursor-font names kitty reads too, and the shapes they are.
const X11: [(&str, Pointer); 19] = [
    ("left_ptr", Pointer::Default),
    ("arrow", Pointer::Default),
    ("xterm", Pointer::Text),
    ("ibeam", Pointer::Text),
    ("hand", Pointer::Pointer),
    ("hand1", Pointer::Pointer),
    ("hand2", Pointer::Pointer),
    ("question_arrow", Pointer::Help),
    ("watch", Pointer::Wait),
    ("left_ptr_watch", Pointer::Progress),
    ("tcross", Pointer::Crosshair),
    ("cross", Pointer::Crosshair),
    ("fleur", Pointer::Move),
    ("right_side", Pointer::EResize),
    ("top_side", Pointer::NResize),
    ("bottom_side", Pointer::SResize),
    ("left_side", Pointer::WResize),
    ("sb_h_double_arrow", Pointer::EwResize),
    ("sb_v_double_arrow", Pointer::NsResize),
];

/// How many shapes a screen's stack keeps. Pushing past it drops the
/// oldest.
pub(crate) const DEPTH: usize = 16;

impl Pointer {
    /// The shape `name` names, CSS's or X11's, if it's one.
    fn named(name: &[u8]) -> Option<Self> {
        (NAMES.iter())
            .find(|(_, known)| known.as_bytes() == name)
            .map(|&(pointer, _)| pointer)
            .or_else(|| {
                (X11.iter())
                    .find(|(known, _)| known.as_bytes() == name)
                    .map(|&(_, pointer)| pointer)
            })
    }

    /// Its CSS name.
    #[must_use]
    pub fn name(self) -> &'static str {
        (NAMES.iter())
            .find(|&&(pointer, _)| pointer == self)
            .map_or("default", |&(_, name)| name)
    }
}

/// One screen's stack of shapes: the last is shown, and with none, the
/// window's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stack(Vec<Pointer>);

impl Stack {
    /// The shape shown, if a program set one.
    pub(crate) fn shown(&self) -> Option<Pointer> {
        self.0.last().copied()
    }

    /// `OSC 22 ; payload`: what it asks, done; the answer to a question,
    /// if it was one.
    pub(crate) fn osc(&mut self, payload: &[u8]) -> Option<String> {
        match payload {
            [b'<', ..] => {
                self.0.pop();
            }
            [b'>', names @ ..] => {
                if let Some(pointer) = names.split(|&byte| byte == b',').find_map(Pointer::named) {
                    if self.0.len() == DEPTH {
                        self.0.remove(0);
                    }
                    self.0.push(pointer);
                }
            }
            [b'?', names @ ..] => return Some(self.answer(names)),
            [b'=', name @ ..] | name => self.set(name),
        }
        None
    }

    /// Sets the shape on top, or with an empty name, the default.
    fn set(&mut self, name: &[u8]) {
        let pointer = if name.is_empty() {
            Some(Pointer::Default)
        } else {
            Pointer::named(name)
        };
        if let Some(pointer) = pointer {
            match self.0.last_mut() {
                Some(top) => *top = pointer,
                None => self.0.push(pointer),
            }
        }
    }

    /// The answer to `OSC 22 ; ? names`: for each, `1` if it's known, or
    /// `0`; for kitty's `__current__`, `__default__` and `__grabbed__`, a
    /// shape's name. At most four times [`DEPTH`] names are answered.
    fn answer(&self, names: &[u8]) -> String {
        let answers: Vec<&str> = names
            .split(|&byte| byte == b',')
            .take(4 * DEPTH)
            .map(|name| match name {
                b"__current__" => self.shown().unwrap_or(Pointer::Default).name(),
                b"__default__" | b"__grabbed__" => Pointer::Default.name(),
                name if Pointer::named(name).is_some() => "1",
                _ => "0",
            })
            .collect();
        answers.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_set_pushed_and_popped() {
        let mut stack = Stack::default();
        assert_eq!(stack.shown(), None);
        stack.osc(b"pointer");
        assert_eq!(stack.shown(), Some(Pointer::Pointer));
        stack.osc(b"=text");
        assert_eq!(stack.0, [Pointer::Text], "set in place of the top");
        stack.osc(b">nonsense,hand2,wait");
        assert_eq!(stack.shown(), Some(Pointer::Pointer), "the first known");
        stack.osc(b"<");
        assert_eq!(stack.shown(), Some(Pointer::Text));
        stack.osc(b"bogus");
        assert_eq!(stack.shown(), Some(Pointer::Text), "unknown, nothing");
        stack.osc(b"");
        assert_eq!(stack.shown(), Some(Pointer::Default));
        stack.osc(b"<");
        stack.osc(b"<");
        assert_eq!(stack.shown(), None, "popping past the end is harmless");
        for _ in 0..DEPTH + 5 {
            stack.osc(b">wait");
        }
        assert_eq!(stack.0.len(), DEPTH);
    }

    #[test]
    fn questions_are_answered_with_known_names_and_digits_alone() {
        let mut stack = Stack::default();
        stack.osc(b"crosshair");
        assert_eq!(
            stack.osc(b"?pointer,\x15rm -rf,__current__,left_ptr"),
            Some("1,0,crosshair,1".to_owned())
        );
        let many = vec![&b"x"[..]; 1000].join(&b',');
        let mut asked = b"?".to_vec();
        asked.extend(many);
        let answer = stack.osc(&asked).unwrap();
        assert_eq!(answer.split(',').count(), 4 * DEPTH);
    }

    #[test]
    fn every_shape_has_a_name_of_its_own() {
        for (pointer, name) in NAMES {
            assert_eq!(pointer.name(), name);
            assert_eq!(Pointer::named(name.as_bytes()), Some(pointer));
        }
    }
}
