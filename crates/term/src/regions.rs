//! Regions of output a program marks as private, with clackatoo's own
//! escape sequence, OSC 3008 (CLACKA-242):
//!
//! ```text
//! OSC 3008 ; region ; start ; ID ; FLAGS ST
//! OSC 3008 ; region ; end ; ID ST
//! OSC 3008 ; forget ST
//! ```
//!
//! Every line printed on while a region is open gets the region's hints
//! (see [`crate::hints`]), and keeps them wherever its text goes:
//! scrolled, moved between rows by margins, rewrapped, into history.
//! `FLAGS` is a list of them, split by commas: `hide`, `nosave` and
//! `noagent`, or `secret` for all three and `private` for the last two.
//! Flags this terminal doesn't know are ignored, so the list can grow;
//! without one it knows, a region fails closed, `secret`.
//!
//! While a region kept from agents is open, a program's title, working
//! directory (OSC 0, 2 and 7), notifications and progress (OSC 9, 777
//! and 99) are ignored: they go where agents and the phone see them.
//!
//! Hints only ever add protection, so the rules are about closing:
//!
//! - **An end must carry its start's ID**, which whoever opens a region
//!   picks at random. Output inside the region can't close it, as it
//!   never sees the ID. Regions nest: an end closes the newest open one
//!   with its ID.
//! - **The shell's next prompt closes every region**, so a program can't
//!   leave one open over the user's later commands. Only a prompt the
//!   shell signs counts: clackatoo's own integration prints
//!   `OSC 3008 ; prompt ; clackatoo=SECRET` with each, with the secret
//!   only it and the terminal know (see [`crate::blocks`]). A program
//!   printing marks of its own, or a shell nested inside one, as over
//!   ssh, can't close a region early. So does the shell's next command,
//!   signed the same way.
//! - **Without shell integration**, a region closes after
//!   [`MOST_LINES`] lines.
//! - **A start is never dropped.** Past [`MOST_OPEN`] regions, or with an
//!   ID that isn't one, its hints go on until the prompt or the cap.
//! - **Nothing else closes one**: not a reset, nor the alternate screen.
//!
//! A line keeps the hints it got for as long as it's kept, whatever
//! closes the region later. A region closed early only leaves out what's
//! printed after.
//!
//! `forget` asks the terminal's owner to forget what it kept of the
//! terminal across restarts, and what it shows (see
//! [`Terminal::take_forget`]). The terminal never answers any of these.
//!
//! [`Terminal::take_forget`]: crate::Terminal::take_forget

use crate::hints::Hints;

/// The most regions open at once. A start past them adds its hints to
/// what's printed until the prompt or the cap.
pub const MOST_OPEN: usize = 16;

/// The number of clackatoo's escape sequence, `OSC 3008`, here alone so
/// moving it is a change of one line.
pub const OSC: &str = "3008";

/// The longest ID, in bytes.
pub const LONGEST_ID: usize = 64;

/// The most lines printed on while regions are open in a terminal without
/// shell integration, after which they close: there, no prompt will.
pub const MOST_LINES: u32 = 1000;

/// The most lines printed on while regions are open in any terminal,
/// after which they close: a shell that never prompts again, as one
/// `exec`ed into another, or nested, leaves them open no longer.
pub const MOST_LINES_EVER: u32 = 100_000;

/// One open region.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Region {
    id: Box<str>,
    hints: Hints,
}

/// A terminal's open regions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Regions {
    open: Vec<Region>,
    /// The hints of starts no end can close, which go on until the
    /// prompt or the cap.
    unending: Hints,
    /// Every open region's hints together: what a line printed on now
    /// gets. Empty while none is open, which printing looks at first.
    pub(crate) hinting: Hints,
    /// How many lines were printed on since regions were last all
    /// closed, each counted once: those numbered past every one before.
    lines: u32,
    /// The highest numbered line printed on since then.
    highest: Option<u64>,
    /// The line a program last asked for what's kept of the terminal
    /// before it to be forgotten from, since its owner last took it.
    pub(crate) forget: Option<u64>,
    /// The hints of kitty's virtual placements, by image ID and
    /// placement, of those made in a region, or whose placeholders were
    /// printed in one: their images are as private as the region, wherever
    /// their placeholders are or go (see [`crate::graphics`]).
    pub(crate) virtuals: std::collections::HashMap<(u32, u32), Hints>,
}

/// What a terminal's regions are, to be handed to the next build in an
/// upgrade (see [`Terminal::regions`](crate::Terminal::regions)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RegionsSnapshot {
    /// Each open region's ID and hints' bits, oldest first.
    pub open: Vec<(String, u8)>,
    /// The bits of the hints no end can close.
    pub unending: u8,
    /// The lines printed on since regions were last all closed.
    pub lines: u32,
    /// The hints' bits of kitty's virtual placements made or shown in a
    /// region, by image ID and placement.
    pub virtuals: Vec<(u32, u32, u8)>,
}

impl Regions {
    /// Takes in `OSC 3008 ; params…`.
    pub(crate) fn osc(&mut self, params: &[&[u8]]) {
        match params {
            [b"region", b"start", id, flags @ ..] => {
                let hints: Hints = flags.iter().map(|flags| parse_flags(flags)).collect();
                // Without a flag it knows, a region fails closed: secret.
                let hints = if hints.is_empty() {
                    Hints::HIDE | Hints::NOSAVE | Hints::NOAGENT
                } else {
                    hints
                };
                self.start(id, hints);
            }
            [b"region", b"end", id] => self.end(id),
            _ => log::trace!("unhandled OSC 3008"),
        }
    }

    fn start(&mut self, id: &[u8], hints: Hints) {
        let id = std::str::from_utf8(id).ok().filter(|id| is_id(id));
        match id {
            Some(id) if self.open.len() < MOST_OPEN => self.open.push(Region {
                id: id.into(),
                hints,
            }),
            _ => self.unending |= hints,
        }
        self.hinting |= hints;
    }

    fn end(&mut self, id: &[u8]) {
        let Some(at) = (self.open.iter()).rposition(|region| region.id.as_bytes() == id) else {
            return;
        };
        self.open.remove(at);
        self.hinting = (self.open.iter()).fold(self.unending, |hints, region| hints | region.hints);
        if self.is_closed() {
            self.close_all();
        }
    }

    /// Whether no region is open.
    pub(crate) fn is_closed(&self) -> bool {
        self.open.is_empty() && self.unending.is_empty()
    }

    /// Closes every region, as the shell's prompt does.
    pub(crate) fn close_all(&mut self) {
        *self = Self {
            forget: self.forget,
            virtuals: std::mem::take(&mut self.virtuals),
            ..Self::default()
        };
    }

    /// Adds `hints` to the virtual placement `key`'s.
    pub(crate) fn mark_virtual(&mut self, key: (u32, u32), hints: Hints) {
        if !hints.is_empty() {
            *self.virtuals.entry(key).or_default() |= hints;
        }
    }

    /// The hints of the virtual placement `key`.
    pub(crate) fn virtual_hints(&self, key: (u32, u32)) -> Hints {
        self.virtuals.get(&key).copied().unwrap_or_default()
    }

    /// Counts line `line` as printed on, once however often, as it's
    /// past every line printed on before: going back and forth between
    /// lines counts nothing. Says how many lines that makes.
    pub(crate) fn printed_on(&mut self, line: u64) -> u32 {
        if self.highest.is_none_or(|highest| line > highest) {
            self.highest = Some(line);
            self.lines = self.lines.saturating_add(1);
        }
        self.lines
    }

    /// Whether a region that keeps what's printed from agents is open, as
    /// one's title, directory and notifications are then.
    pub(crate) fn keeps_from_agents(&self) -> bool {
        self.hinting.contains(Hints::NOAGENT)
    }

    pub(crate) fn snapshot(&self) -> Option<RegionsSnapshot> {
        let mut virtuals: Vec<(u32, u32, u8)> = (self.virtuals.iter())
            .map(|(&(id, placement), hints)| (id, placement, hints.bits()))
            .collect();
        virtuals.sort_unstable();
        (!self.is_closed() || !virtuals.is_empty()).then(|| RegionsSnapshot {
            virtuals,
            open: (self.open.iter())
                .map(|region| (region.id.to_string(), region.hints.bits()))
                .collect(),
            unending: self.unending.bits(),
            lines: self.lines,
        })
    }

    /// The regions `snapshot` says, checked as a program's would be.
    pub(crate) fn restore(&mut self, snapshot: &RegionsSnapshot) {
        self.close_all();
        self.virtuals.clear();
        // Only what a program could set: never the user's own mark.
        let hints = |bits| Hints::from_bits_truncate(bits) - Hints::KEPT;
        for (id, bits) in &snapshot.open {
            self.start(id.as_bytes(), hints(*bits));
        }
        self.unending |= hints(snapshot.unending);
        self.hinting |= self.unending;
        self.lines = snapshot.lines;
        self.virtuals = (snapshot.virtuals.iter())
            .map(|&(id, placement, bits)| ((id, placement), hints(bits)))
            .filter(|(_, hints)| !hints.is_empty())
            .collect();
    }
}

/// The hints a list of flags names, split by commas. Flags not known are
/// left out.
fn parse_flags(flags: &[u8]) -> Hints {
    (flags.split(|&byte| byte == b','))
        .map(|flag| match flag {
            b"hide" => Hints::HIDE,
            b"nosave" => Hints::NOSAVE,
            b"noagent" => Hints::NOAGENT,
            b"secret" => Hints::HIDE | Hints::NOSAVE | Hints::NOAGENT,
            b"private" => Hints::NOSAVE | Hints::NOAGENT,
            _ => Hints::empty(),
        })
        .collect()
}

/// Whether `id` is one: 1 to [`LONGEST_ID`] letters, digits, `-`, `_` or
/// `.`.
fn is_id(id: &str) -> bool {
    (1..=LONGEST_ID).contains(&id.len())
        && (id.bytes()).all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

impl crate::Terminal {
    /// Whether a region is open (see [`crate::regions`]).
    #[must_use]
    pub fn has_regions(&self) -> bool {
        !self.emulator_regions().is_closed()
    }

    /// Whether a program asked, since the last call, for what's kept of
    /// the terminal across restarts to be forgotten (`OSC 3008 ;
    /// forget`), and if so the line, numbered as in [`crate::document`],
    /// before which it's all forgotten: the cursor's, as it asked, or
    /// behind a full-screen program the line after the main screen.
    pub fn take_forget(&mut self) -> Option<u64> {
        self.emulator_regions_mut().forget.take()
    }

    /// The open regions, if any, for the next build in an upgrade.
    #[must_use]
    pub fn regions(&self) -> Option<RegionsSnapshot> {
        self.emulator_regions().snapshot()
    }

    /// Opens the regions `regions` says were open, as
    /// [`regions`](Self::regions) gave them.
    pub fn restore_regions(&mut self, regions: &RegionsSnapshot) {
        self.emulator_regions_mut().restore(regions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Position, Size, Terminal};

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn terminal() -> Terminal {
        Terminal::new(Size::new(20, 5).unwrap(), &Config::default())
    }

    /// Every line's text with its hints, in history then on screen.
    fn lines(t: &Terminal) -> Vec<(String, Hints)> {
        let (first, last) = t.document_bounds();
        let hinted = t.hints();
        (first..=last)
            .map(|line| {
                let row = t.document_line(line).unwrap();
                let end = t.primary().scrollback_end();
                let hints = if line < end {
                    (hinted.iter())
                        .find(|&&(at, _)| at == line)
                        .map_or(Hints::empty(), |&(_, hints)| hints)
                } else {
                    row.hints()
                };
                (row.text(), hints)
            })
            .filter(|(text, _)| !text.is_empty())
            .collect()
    }

    fn hints_of(t: &Terminal, text: &str) -> Hints {
        (lines(t).into_iter())
            .find(|(line, _)| line == text)
            .map_or_else(
                || panic!("no line {text:?}: {:?}", lines(t)),
                |(_, hints)| hints,
            )
    }

    #[test]
    fn lines_printed_on_in_a_region_get_its_hints() {
        let mut t = terminal();
        t.advance(b"before\r\n\x1b]3008;region;start;a1;hide,nosave\x1b\\token\r\n");
        t.advance(b"more\x1b]3008;region;end;a1\x07\r\nafter");
        assert_eq!(hints_of(&t, "before"), Hints::empty());
        assert_eq!(hints_of(&t, "token"), Hints::HIDE | Hints::NOSAVE);
        assert_eq!(hints_of(&t, "more"), Hints::HIDE | Hints::NOSAVE);
        assert_eq!(hints_of(&t, "after"), Hints::empty());
        assert!(!t.has_regions());
    }

    #[test]
    fn flags_name_hints_presets_and_nothing_else() {
        let all = Hints::HIDE | Hints::NOSAVE | Hints::NOAGENT;
        for (flags, hints) in [
            ("", all),
            (";", all),
            (";secret", all),
            (";private", Hints::NOSAVE | Hints::NOAGENT),
            (";noagent", Hints::NOAGENT),
            (";hide,shiny,nosave", Hints::HIDE | Hints::NOSAVE),
            (";hide;nosave", Hints::HIDE | Hints::NOSAVE),
            // None it knows fails closed.
            (";keep,kept", all),
        ] {
            let mut t = terminal();
            t.advance(format!("\x1b]3008;region;start;id{flags}\x07x").as_bytes());
            assert_eq!(hints_of(&t, "x"), hints, "{flags:?}");
        }
    }

    #[test]
    fn only_the_matching_id_ends_a_region_and_regions_nest() {
        let mut t = terminal();
        t.advance(b"\x1b]3008;region;start;outer;noagent\x07");
        t.advance(b"\x1b]3008;region;start;inner;hide\x07a\r\n");
        // An end with another ID, as output inside could print, does
        // nothing.
        t.advance(b"\x1b]3008;region;end;guess\x07\x1b]3008;region;end;OUTER\x07b\r\n");
        t.advance(b"\x1b]3008;region;end;inner\x07c\r\n");
        t.advance(b"\x1b]3008;region;end;outer\x07d");
        assert_eq!(hints_of(&t, "a"), Hints::NOAGENT | Hints::HIDE);
        assert_eq!(hints_of(&t, "b"), Hints::NOAGENT | Hints::HIDE);
        assert_eq!(hints_of(&t, "c"), Hints::NOAGENT);
        assert_eq!(hints_of(&t, "d"), Hints::empty());
    }

    /// The shell's signed prompt closes regions; no prompt a program
    /// prints does, as a shell nested in it would, whenever it's read:
    /// before the shell's own, or after it, with the shell in front.
    #[test]
    fn only_the_shells_signed_prompt_closes_regions() {
        let mut t = terminal();
        t.expect_secret(Some(SECRET));
        let signed = format!("\x1b]133;C;clackatoo={SECRET}\x07");
        t.advance(format!("\x1b]133;A\x07$ \x1b]133;B\x07ssh\r\n{signed}").as_bytes());
        t.advance(b"\x1b]3008;region;start;r;private\x07one\r\n");
        // A shell nested inside, as over ssh, marks its command's end and
        // its prompt, and forges ours.
        t.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]3008;prompt\x07");
        t.advance(b"\x1b]3008;prompt;clackatoo=0123456789abcdef0123456789abcdee\x07");
        t.advance(b"remote$ two\r\n");
        assert!(t.has_regions());
        assert_eq!(hints_of(&t, "remote$ two"), Hints::NOSAVE | Hints::NOAGENT);
        // The shell's own.
        let prompt = format!("\x1b]3008;prompt;clackatoo={SECRET}\x07");
        t.advance(format!("\x1b]133;D;0\x07{prompt}\x1b]133;A\x07$ three\r\n").as_bytes());
        assert!(!t.has_regions());
        assert_eq!(hints_of(&t, "$ three"), Hints::empty());
        // What a program forges after the shell's prompt, as one in the
        // background might, closes nothing either.
        t.advance(b"\x1b]3008;region;start;s;noagent\x07\x1b]133;D;0\x07\x1b]133;A\x07four");
        assert!(t.has_regions());
        assert_eq!(hints_of(&t, "four"), Hints::NOAGENT);
    }

    #[test]
    fn the_shells_signed_command_closes_regions_left_open() {
        let mut t = terminal();
        t.expect_secret(Some(SECRET));
        t.advance(b"\x1b]3008;region;start;r;noagent\x07one\r\n");
        t.advance(format!("\x1b]133;C;clackatoo={SECRET}\x07two").as_bytes());
        assert!(!t.has_regions());
        assert_eq!(hints_of(&t, "two"), Hints::empty());
    }

    #[test]
    fn without_shell_integration_regions_close_after_a_cap() {
        let mut t = terminal();
        t.advance(b"\x1b]3008;region;start;r;noagent\x07");
        for line in 0..MOST_LINES {
            t.advance(format!("{line}\r\n").as_bytes());
        }
        assert!(t.has_regions());
        t.advance(b"last\r\nafter");
        assert!(!t.has_regions());
        assert_eq!(hints_of(&t, "after"), Hints::empty());
        // With it, only the prompt closes them.
        let mut t = terminal();
        t.expect_secret(Some(SECRET));
        t.advance(b"\x1b]3008;region;start;r;noagent\x07");
        for line in 0..=MOST_LINES {
            t.advance(format!("{line}\r\n").as_bytes());
        }
        assert!(t.has_regions());
    }

    /// Even with shell integration, a region closes after a high cap: a
    /// shell that never prompts again, as one replaced by `exec`, can't
    /// leave one open for ever.
    #[test]
    fn every_region_closes_after_a_high_cap() {
        let mut t = terminal();
        t.expect_secret(Some(SECRET));
        t.advance(b"\x1b]3008;region;start;r;noagent\x07");
        let lines = "x\r\n".repeat(usize::try_from(MOST_LINES_EVER).unwrap() - 1);
        t.advance(lines.as_bytes());
        assert!(t.has_regions());
        t.advance(b"x\r\ny\r\nafter");
        assert!(!t.has_regions());
        assert_eq!(hints_of(&t, "after"), Hints::empty());
    }

    /// REP never repeats a region's last character after it, however it
    /// closed.
    #[test]
    fn rep_never_repeats_a_secret_after_its_region() {
        let rep = |closing: &str| {
            let mut t = terminal();
            t.expect_secret(Some(SECRET));
            t.advance(b"\x1b]3008;region;start;r;noagent\x07Z\r\n");
            t.advance(closing.as_bytes());
            t.advance(b"\x1b[3b");
            !t.screen_text().contains("ZZ")
        };
        assert!(rep("\x1b]3008;region;end;r\x07"));
        assert!(rep(&format!("\x1b]133;C;clackatoo={SECRET}\x07")));
        assert!(rep(&format!("\x1b]3008;prompt;clackatoo={SECRET}\x07")));
        // And the cap, without integration.
        let mut t = terminal();
        t.advance(b"\x1b]3008;region;start;r;noagent\x07");
        let lines = "Z\r\n".repeat(usize::try_from(MOST_LINES).unwrap() + 1);
        t.advance(lines.as_bytes());
        assert!(!t.has_regions());
        t.advance(b"\x1b[3b");
        let (first, last) = t.document_bounds();
        let read = t.text_for_agents(
            Position {
                line: first,
                col: 0,
            },
            Position { line: last, col: 0 },
            true,
        );
        assert!(!read.contains('Z'), "{read:?}");
    }

    /// A mark joining a character printed before a region, printed in
    /// it, gives its line the region's hints.
    #[test]
    fn a_mark_printed_in_a_region_hints_its_line() {
        let mut t = terminal();
        t.advance("e\x1b]3008;region;start;r;noagent\x07\u{301}".as_bytes());
        assert_eq!(hints_of(&t, "e\u{301}"), Hints::NOAGENT);
    }

    #[test]
    fn a_start_is_never_dropped() {
        let mut t = terminal();
        for n in 0..MOST_OPEN {
            t.advance(format!("\x1b]3008;region;start;r{n};noagent\x07").as_bytes());
        }
        // Past the most, and with an ID that isn't one.
        t.advance(b"\x1b]3008;region;start;extra;hide\x07");
        t.advance(b"\x1b]3008;region;start;no way;nosave\x07");
        for n in 0..MOST_OPEN {
            t.advance(format!("\x1b]3008;region;end;r{n}\x07").as_bytes());
        }
        t.advance(b"\x1b]3008;region;end;extra\x07x");
        assert_eq!(hints_of(&t, "x"), Hints::HIDE | Hints::NOSAVE);
        t.expect_secret(Some(SECRET));
        t.advance(format!("\x1b]3008;prompt;clackatoo={SECRET}\x07y").as_bytes());
        assert!(!t.has_regions());
    }

    /// Lines are counted once each, however often a program goes back
    /// and forth between them.
    #[test]
    fn the_cap_counts_lines_not_moves_between_them() {
        let mut t = terminal();
        t.advance(b"\x1b]3008;region;start;r;noagent\x07");
        for _ in 0..MOST_LINES {
            t.advance(b"\x1b[1Ha\x1b[2Hb");
        }
        assert!(t.has_regions());
    }

    /// Inside a region kept from agents, a program can't name its tab or
    /// directory, nor notify anyone.
    #[test]
    fn a_region_kept_from_agents_names_nothing_and_notifies_no_one() {
        let mut t = terminal();
        t.advance(b"\x1b]2;before\x07\x1b]3008;region;start;r;noagent\x07");
        t.advance(b"\x1b]2;hunter2\x07\x1b]7;file:///hunter2\x07\x1b]9;hunter2\x07");
        assert_eq!(t.title(), "before");
        assert_eq!(t.cwd(), None);
        assert!(t.take_notice().is_none());
        t.advance(b"\x1b]3008;region;end;r\x07\x1b]2;after\x07");
        assert_eq!(t.title(), "after");
    }

    #[test]
    fn nothing_but_an_end_prompt_or_cap_closes_a_region() {
        let mut t = terminal();
        t.advance(b"\x1b]3008;region;start;r;noagent\x07");
        // A full reset, and the alternate screen and back.
        t.advance(b"\x1bc\x1b[?1049hfull\x1b[?1049l");
        t.advance(b"x");
        assert!(t.has_regions());
        assert_eq!(hints_of(&t, "x"), Hints::NOAGENT);
    }

    #[test]
    fn regions_come_back_from_a_snapshot_as_a_program_could_set_them() {
        let mut t = terminal();
        t.advance(b"\x1b]3008;region;start;r;hide\x07\x1b]3008;region;start;s;nosave\x07");
        let mut snapshot = t.regions().unwrap();
        snapshot.open.push(("u".into(), Hints::KEPT.bits()));
        let mut new = terminal();
        new.restore_regions(&snapshot);
        new.advance(b"\x1b]3008;region;end;s\x07x");
        assert_eq!(hints_of(&new, "x"), Hints::HIDE);
        new.advance(b"\x1b]3008;region;end;r\x07\x1b]3008;region;end;u\x07");
        assert!(!new.has_regions());
        assert_eq!(terminal().regions(), None);
    }

    #[test]
    fn forget_is_taken_once_and_nothing_is_answered() {
        let mut t = terminal();
        t.advance(b"\x1b]3008;forget\x07\x1b]3008;region;start;r\x07\x1b]3008;region;end;r\x07");
        assert_eq!(t.take_forget(), Some(0));
        assert_eq!(t.take_forget(), None);
        t.advance(b"a\r\nb\r\n\x1b]3008;forget\x07c\r\n\x1b[?1049h\x1b]3008;forget\x07");
        assert_eq!(t.take_forget(), Some(5), "the main screen's 5 rows");
        t.advance(b"\x1b[?1049l\x1b]3008;forget\x07");
        assert_eq!(t.take_forget(), Some(3));
        assert!(t.take_replies().is_empty());
    }

    /// Bits of what a program might print inside a region: OSC 3008's own
    /// ends and starts with other IDs, prompts it forges, resets, and the
    /// alternate screen.
    fn inside() -> impl proptest::strategy::Strategy<Value = Vec<u8>> {
        use proptest::prelude::*;
        let piece = prop_oneof![
            Just(&b"text"[..]),
            Just(&b"\r\n"[..]),
            Just(&b"\x1b]3008;region;end;guess\x07"[..]),
            Just(&b"\x1b]3008;region;end;\x07"[..]),
            Just(&b"\x1b]3008;region;end;Secret0\x07"[..]),
            Just(&b"\x1b]3008;region;start;other;hide\x07"[..]),
            Just(&b"\x1b]3008;region;end;other\x07"[..]),
            Just(&b"\x1b]3008;forget\x1b\\"[..]),
            Just(&b"\x1b]133;D;0\x07\x1b]133;A\x07"[..]),
            Just(&b"\x1b]133;C;clackatoo=guess\x07"[..]),
            Just(&b"\x1bc"[..]),
            Just(&b"\x1b[?1049h"[..]),
            Just(&b"\x1b[?1049l"[..]),
            Just(&b"\x1b]3008;"[..]),
            Just(&b"\x1b]3008;prompt;clackatoo=guess\x07"[..]),
            Just(&b"\x07"[..]),
        ];
        proptest::collection::vec(piece, 0..40).prop_map(|pieces| pieces.concat())
    }

    proptest::proptest! {
        /// Nothing printed inside a region, which never sees its ID, can
        /// close it, whatever it prints and however it's cut; and the
        /// terminal never answers.
        #[test]
        fn only_its_own_end_closes_a_region(inside in inside(), cut in 0usize..64) {
            let mut t = terminal();
            t.expect_secret(Some(SECRET));
            t.advance(b"\x1b]3008;region;start;secret0;noagent\x07first");
            let cut = cut.min(inside.len());
            t.advance(&inside[..cut]);
            t.advance(&inside[cut..]);
            proptest::prop_assert!(t.has_regions());
            proptest::prop_assert!(t.take_replies().is_empty());
            // The terminal may be left in a string: end it, then print.
            t.advance(b"\x07\r\nafter");
            let grid = t.grid();
            let after = (0..grid.height())
                .map(|row| grid.row(row))
                .find(|row| row.text() == "after");
            proptest::prop_assert!(after.is_some_and(|row| row.hints().contains(Hints::NOAGENT)));
            t.advance(b"\x1b]3008;region;end;secret0\x07");
            // Unless another region it opened is still open.
            let others_only = t.regions().is_none_or(|open| {
                open.open.iter().all(|(id, _)| id == "other")
            });
            proptest::prop_assert!(others_only);
        }
    }

    /// What a program might print after a region, to move its text
    /// about: scrolling, inserting and deleting lines and characters,
    /// within margins and a scroll region or not, repeating the last
    /// character, the alternate screen.
    fn moves() -> impl proptest::strategy::Strategy<Value = Vec<u8>> {
        use proptest::prelude::*;
        let piece = prop_oneof![
            Just(&b"\x1b[?69h\x1b[2;7s"[..]),
            Just(&b"\x1b[?69h\x1b[1;3s"[..]),
            Just(&b"\x1b[?69l"[..]),
            Just(&b"\x1b[2;5r"[..]),
            Just(&b"\x1b[r"[..]),
            Just(&b"\x1b[S"[..]),
            Just(&b"\x1b[2T"[..]),
            Just(&b"\x1b[L"[..]),
            Just(&b"\x1b[2M"[..]),
            Just(&b"\x1b[3@"[..]),
            Just(&b"\x1b[2P"[..]),
            Just(&b"\x1b[H"[..]),
            Just(&b"\x1b[3;2H"[..]),
            Just(&b"\x1b[6;4H"[..]),
            Just(&b"\x1bD"[..]),
            Just(&b"\x1bM"[..]),
            Just(&b"\x1b[3b"[..]),
            Just(&b"\x1b[K"[..]),
            Just(&b"\x1b[1J"[..]),
            Just(&b"\x1b[?1049h"[..]),
            Just(&b"\x1b[?1049l"[..]),
            Just(&b"\r\n"[..]),
            Just(&b"ab"[..]),
        ];
        proptest::collection::vec(piece, 0..30).prop_map(|pieces| pieces.concat())
    }

    proptest::proptest! {
        /// Text printed in a region keeps its hints wherever later output
        /// moves it: no row without them ever holds any of it. Its `Z`s
        /// never reach what agents read.
        #[test]
        fn a_regions_text_never_lands_on_a_row_without_its_hints(
            moves in moves(),
            cut in 0usize..64,
            cols in 3u16..14,
            rows in 2u16..8,
        ) {
            let mut t = Terminal::new(Size::new(10, 6).unwrap(), &Config::default());
            t.advance(b"pre\r\n\x1b]3008;region;start;r;noagent\x07ZZZZZZZZZZZZ\r\nZZ");
            t.advance(b"\x1b]3008;region;end;r\x07\r\npost");
            let cut = cut.min(moves.len());
            t.advance(&moves[..cut]);
            t.resize(Size::new(cols, rows).unwrap());
            t.advance(&moves[cut..]);
            let (first, last) = t.document_bounds();
            let from = Position { line: first, col: 0 };
            let to = Position { line: last, col: 0 };
            let read = t.text_for_agents(from, to, true);
            proptest::prop_assert!(!read.contains('Z'), "{read:?}");
        }
    }

    #[test]
    fn hidden_lines_are_masked_in_copies_and_skipped_by_search() {
        let mut t = terminal();
        t.advance(b"one pw\r\n\x1b]3008;region;start;r;hide\x07pw\r\npw2\x1b]3008;region;end;r\x07\r\nlast pw");
        let at = |line, col| Position { line, col };
        let (first, last) = (t.document_bounds().0, 3);
        assert_eq!(
            t.text_masked(
                at(first, 0),
                at(last, 20),
                false,
                Hints::HIDE,
                &|_, _, _| { false }
            ),
            format!("one pw\n{}\nlast pw", crate::hints::MASK)
        );
        assert_eq!(
            t.text_masked(
                at(first, 0),
                at(last, 20),
                false,
                Hints::HIDE,
                &|line, _, _| { (1..3).contains(&line) }
            ),
            "one pw\npw\npw2\nlast pw"
        );
        assert_eq!(
            t.block_text_masked(at(first, 0), at(last, 1), Hints::HIDE, &|_, _, _| false),
            format!("on\n{0}\n{0}\nla", crate::hints::MASK)
        );
        assert_eq!(t.hidden_run(2), Some(1..3));
        assert_eq!(t.hidden_run(0), None);
        let found: Vec<u64> = (0..4)
            .filter_map(|n| {
                let none = |_: u64, _: &crate::Row, _: Hints| false;
                t.search_nth_masked("pw", at(0, 0), false, false, n + 1, Hints::HIDE, &none)
            })
            .map(|(found, _)| found.start.line)
            .collect();
        assert!(
            found.iter().all(|&line| line == 0 || line == 3),
            "{found:?}"
        );
        assert!(found.contains(&3));
    }
}
