//! Mouse reporting: programs such as Neovim, htop and less ask the
//! terminal to tell them about clicks, drags and the wheel, and say how to
//! encode them. The emulator records what was asked (see
//! [`InputModes`](crate::InputModes)); clients turn their mouse events into
//! bytes for the program with [`encode`].

/// Which mouse events the program asked to be told about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseTracking {
    /// None: the mouse is the terminal's own, for selecting text.
    #[default]
    Off,
    /// Mode 9 (X10): button presses only, without modifiers.
    Presses,
    /// Mode 1000: presses and releases, and the wheel.
    Clicks,
    /// Mode 1002: clicks, plus motion while a button is held.
    Drags,
    /// Mode 1003: clicks, plus all motion.
    Motion,
}

impl MouseTracking {
    /// The DEC private mode that turns this on.
    #[must_use]
    pub fn mode(self) -> Option<u16> {
        match self {
            Self::Off => None,
            Self::Presses => Some(9),
            Self::Clicks => Some(1000),
            Self::Drags => Some(1002),
            Self::Motion => Some(1003),
        }
    }

    /// The tracking DEC private mode `mode` turns on, if it is one.
    #[must_use]
    pub fn from_mode(mode: u16) -> Option<Self> {
        [Self::Presses, Self::Clicks, Self::Drags, Self::Motion]
            .into_iter()
            .find(|tracking| tracking.mode() == Some(mode))
    }
}

/// How mouse events are written for the program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MouseEncoding {
    /// `ESC [ M` and three bytes, each 32 plus a value. Positions past
    /// column or line 223 can't be written.
    #[default]
    Default,
    /// Mode 1005: as the default, with the values as UTF-8 characters, up
    /// to 2015.
    Utf8,
    /// Mode 1006: `ESC [ <` and decimal numbers, with `M` for a press and
    /// `m` for a release. What modern programs ask for.
    Sgr,
    /// Mode 1015: `ESC [` and decimal numbers.
    Urxvt,
}

impl MouseEncoding {
    /// The DEC private mode that turns this on.
    #[must_use]
    pub fn mode(self) -> Option<u16> {
        match self {
            Self::Default => None,
            Self::Utf8 => Some(1005),
            Self::Sgr => Some(1006),
            Self::Urxvt => Some(1015),
        }
    }

    /// The encoding DEC private mode `mode` turns on, if it is one.
    #[must_use]
    pub fn from_mode(mode: u16) -> Option<Self> {
        [Self::Utf8, Self::Sgr, Self::Urxvt]
            .into_iter()
            .find(|encoding| encoding.mode() == Some(mode))
    }
}

/// A mouse button, or the wheel turning one way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// The left button.
    Left,
    /// The middle button.
    Middle,
    /// The right button.
    Right,
    /// The wheel, turned up (away from you).
    WheelUp,
    /// The wheel, turned down.
    WheelDown,
    /// The wheel, tilted left.
    WheelLeft,
    /// The wheel, tilted right.
    WheelRight,
}

impl MouseButton {
    fn is_wheel(self) -> bool {
        matches!(
            self,
            Self::WheelUp | Self::WheelDown | Self::WheelLeft | Self::WheelRight
        )
    }

    fn code(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
            Self::WheelLeft => 66,
            Self::WheelRight => 67,
        }
    }
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    /// A button went down, or the wheel turned.
    Press(MouseButton),
    /// A button came up.
    Release(MouseButton),
    /// The pointer moved to another cell, with this button held, if any.
    Motion(Option<MouseButton>),
}

/// A mouse event over a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "a modifier is held or not")]
pub struct MouseEvent {
    /// What happened.
    pub action: MouseAction,
    /// The cell's column, from 0 at the left.
    pub col: u16,
    /// The cell's line, from 0 at the top of the screen.
    pub line: u16,
    /// Shift was held. Clients usually keep Shift-clicks for themselves.
    pub shift: bool,
    /// Alt was held.
    pub alt: bool,
    /// Ctrl was held.
    pub ctrl: bool,
}

/// The bytes that tell the program about `event`, given how it asked to be
/// told, or `None` if it didn't ask about this event or its position can't
/// be written.
#[must_use]
pub fn encode(
    event: &MouseEvent,
    tracking: MouseTracking,
    encoding: MouseEncoding,
) -> Option<Vec<u8>> {
    let reported = match event.action {
        MouseAction::Press(button) => match tracking {
            MouseTracking::Off => false,
            MouseTracking::Presses => !button.is_wheel(),
            MouseTracking::Clicks | MouseTracking::Drags | MouseTracking::Motion => true,
        },
        // The wheel has no releases, and X10 mode reports presses only.
        MouseAction::Release(button) => {
            !button.is_wheel()
                && matches!(
                    tracking,
                    MouseTracking::Clicks | MouseTracking::Drags | MouseTracking::Motion
                )
        }
        MouseAction::Motion(held) => {
            tracking == MouseTracking::Motion
                || (tracking == MouseTracking::Drags && held.is_some())
        }
    };
    if !reported {
        return None;
    }

    let (mut code, release) = match event.action {
        MouseAction::Press(button) => (button.code(), false),
        MouseAction::Release(button) => (button.code(), true),
        // Motion is reported as the held button, or 3 for none, plus 32.
        MouseAction::Motion(held) => (held.map_or(3, MouseButton::code) + 32, false),
    };
    // Modes other than SGR can't say which button came up.
    if release && encoding != MouseEncoding::Sgr {
        code = 3;
    }
    // X10 mode reports no modifiers.
    if tracking != MouseTracking::Presses {
        code += u32::from(event.shift) * 4 + u32::from(event.alt) * 8 + u32::from(event.ctrl) * 16;
    }
    let (col, line) = (u32::from(event.col) + 1, u32::from(event.line) + 1);

    match encoding {
        MouseEncoding::Sgr => {
            let end = if release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{col};{line}{end}").into_bytes())
        }
        MouseEncoding::Urxvt => Some(format!("\x1b[{};{col};{line}M", code + 32).into_bytes()),
        MouseEncoding::Default => {
            let byte = |value: u32| u8::try_from(value + 32).ok();
            Some(vec![0x1b, b'[', b'M', byte(code)?, byte(col)?, byte(line)?])
        }
        MouseEncoding::Utf8 => {
            let mut bytes = b"\x1b[M".to_vec();
            for value in [code, col, line] {
                let ch = char::from_u32(value + 32).filter(|_| value + 32 < 2048)?;
                bytes.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            }
            Some(bytes)
        }
    }
}

/// The bytes that tell a program asking for focus events (mode 1004) that
/// the terminal gained or lost focus.
#[must_use]
pub fn focus(focused: bool) -> &'static [u8] {
    if focused { b"\x1b[I" } else { b"\x1b[O" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(action: MouseAction, col: u16, line: u16) -> MouseEvent {
        MouseEvent {
            action,
            col,
            line,
            shift: false,
            alt: false,
            ctrl: false,
        }
    }

    fn sgr(event: &MouseEvent, tracking: MouseTracking) -> Option<String> {
        encode(event, tracking, MouseEncoding::Sgr).map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn sgr_says_which_button_and_whether_it_went_down_or_up() {
        let press = event(MouseAction::Press(MouseButton::Left), 4, 2);
        let release = event(MouseAction::Release(MouseButton::Right), 0, 0);
        assert_eq!(sgr(&press, MouseTracking::Clicks).unwrap(), "\x1b[<0;5;3M");
        assert_eq!(
            sgr(&release, MouseTracking::Clicks).unwrap(),
            "\x1b[<2;1;1m"
        );
        let wheel = event(MouseAction::Press(MouseButton::WheelDown), 9, 9);
        assert_eq!(
            sgr(&wheel, MouseTracking::Clicks).unwrap(),
            "\x1b[<65;10;10M"
        );
    }

    #[test]
    fn modifiers_add_to_the_button() {
        let mut click = event(MouseAction::Press(MouseButton::Middle), 0, 0);
        click.alt = true;
        click.ctrl = true;
        assert_eq!(sgr(&click, MouseTracking::Clicks).unwrap(), "\x1b[<25;1;1M");
        // X10 mode never reports them.
        assert_eq!(sgr(&click, MouseTracking::Presses).unwrap(), "\x1b[<1;1;1M");
    }

    #[test]
    fn each_tracking_mode_reports_its_own_events() {
        let release = event(MouseAction::Release(MouseButton::Left), 0, 0);
        let drag = event(MouseAction::Motion(Some(MouseButton::Left)), 1, 0);
        let hover = event(MouseAction::Motion(None), 1, 0);
        let wheel_up = event(MouseAction::Release(MouseButton::WheelUp), 0, 0);
        let reports = |tracking| {
            [release, drag, hover, wheel_up].map(|event| sgr(&event, tracking).is_some())
        };
        assert_eq!(reports(MouseTracking::Off), [false; 4]);
        assert_eq!(reports(MouseTracking::Presses), [false; 4]);
        assert_eq!(reports(MouseTracking::Clicks), [true, false, false, false]);
        assert_eq!(reports(MouseTracking::Drags), [true, true, false, false]);
        assert_eq!(reports(MouseTracking::Motion), [true, true, true, false]);
        assert_eq!(sgr(&drag, MouseTracking::Drags).unwrap(), "\x1b[<32;2;1M");
        assert_eq!(sgr(&hover, MouseTracking::Motion).unwrap(), "\x1b[<35;2;1M");
    }

    #[test]
    fn the_old_encodings_add_32_and_cant_say_which_button_came_up() {
        let press = event(MouseAction::Press(MouseButton::Left), 4, 2);
        let release = event(MouseAction::Release(MouseButton::Left), 4, 2);
        let default = |event| encode(&event, MouseTracking::Clicks, MouseEncoding::Default);
        assert_eq!(default(press).unwrap(), b"\x1b[M\x20\x25\x23");
        assert_eq!(default(release).unwrap(), b"\x1b[M\x23\x25\x23");
        let urxvt = encode(&release, MouseTracking::Clicks, MouseEncoding::Urxvt).unwrap();
        assert_eq!(urxvt, b"\x1b[35;5;3M");
    }

    #[test]
    fn positions_too_far_for_an_encoding_are_dropped() {
        let far = event(MouseAction::Press(MouseButton::Left), 300, 0);
        assert!(encode(&far, MouseTracking::Clicks, MouseEncoding::Default).is_none());
        let utf8 = encode(&far, MouseTracking::Clicks, MouseEncoding::Utf8).unwrap();
        assert_eq!(utf8, "\x1b[M\u{20}\u{14d}\u{21}".as_bytes());
        let very_far = event(MouseAction::Press(MouseButton::Left), 3000, 0);
        assert!(encode(&very_far, MouseTracking::Clicks, MouseEncoding::Utf8).is_none());
        assert!(sgr(&very_far, MouseTracking::Clicks).is_some());
    }

    #[test]
    fn modes_round_trip_through_their_numbers() {
        for tracking in [
            MouseTracking::Presses,
            MouseTracking::Clicks,
            MouseTracking::Drags,
            MouseTracking::Motion,
        ] {
            assert_eq!(
                MouseTracking::from_mode(tracking.mode().unwrap()),
                Some(tracking)
            );
        }
        for encoding in [
            MouseEncoding::Utf8,
            MouseEncoding::Sgr,
            MouseEncoding::Urxvt,
        ] {
            assert_eq!(
                MouseEncoding::from_mode(encoding.mode().unwrap()),
                Some(encoding)
            );
        }
    }
}
