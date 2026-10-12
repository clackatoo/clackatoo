//! Registers: text kept to put back with `p`, as Vim's are, shared by
//! every editor of the window. `"` and a register's name before a command
//! says which it takes text from or keeps it in.
//!
//! | Register | Holds |
//! |---|---|
//! | `""`, the unnamed | What was last yanked or deleted, which `p` puts when no register's named |
//! | `"0` | The last yank with no register named |
//! | `"1` to `"9` | Deletes of a line or more, newest first, each pushing the others down |
//! | `"-` | The last delete within a line, with no register named |
//! | `"a` to `"z` | What's kept in them; `"A` to `"Z` add to them |
//! | `"_` | Nothing: what goes to it is thrown away, and the others keep theirs |
//! | `"+`, `"*` | The clipboard, and the primary selection |
//!
//! Macros are kept in registers too: `qa` records the keys typed into
//! `"a`, written down as Vim writes keys in mappings (see
//! [`Stroke::write`]), so `"ap` puts them in a file, and `@a` types
//! whatever text the register holds.
//!
//! The window can't wait for the clipboard while keys are typed, so `"+`
//! and `"*` hold what was last put there: by the window, or by anything
//! else, as a thread of the window's reads both each time it gets the
//! focus, whatever `clipboard` says. What the editors put there waits for
//! the window to send it on; what the thread reads is dropped if the
//! window put something there since it began.
//!
//! `clipboard` can make the unnamed register go along with the clipboard
//! or the primary selection, as Neovim's `unnamedplus` and `unnamed` do:
//! what's yanked or deleted goes there too, and `p` puts what's there.

use super::keys::Stroke;
use crate::options::Clipboard;
use crate::pane::Copy;

/// Text kept to put back with `p`: what was copied or deleted, by
/// characters, or whole lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Register {
    pub(crate) text: String,
    pub(crate) lines: bool,
}

impl Register {
    /// `text`, by characters.
    pub(crate) fn chars(text: String) -> Self {
        Self { text, lines: false }
    }

    /// Adds `more` to the end, as `"A` does: whole lines if either is,
    /// with a line break between.
    fn append(&mut self, more: Register) {
        if self.text.is_empty() {
            *self = more;
            return;
        }
        if self.lines || more.lines {
            if !self.text.ends_with('\n') {
                self.text.push('\n');
            }
            self.text.push_str(&more.text);
            if !self.text.ends_with('\n') {
                self.text.push('\n');
            }
            self.lines = true;
        } else {
            self.text.push_str(&more.text);
        }
    }
}

/// What put text in a register, which decides which others it goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wrote {
    /// `y`.
    Yank,
    /// `d`, `c`, `x` and their kin. `small` if it was within a line, which
    /// goes to `"-`; `history` if it goes to `"1`, as a line or more does,
    /// and a delete within a line over a long motion, as Vim's does.
    Delete { small: bool, history: bool },
}

/// The register each of `Copy`'s is, by its index in `Registers::system`.
fn system_index(copy: Copy) -> usize {
    match copy {
        Copy::Clipboard => 0,
        Copy::Primary => 1,
    }
}

/// Every register, and the macro being recorded.
#[derive(Debug)]
pub(crate) struct Registers {
    /// `""`.
    unnamed: Register,
    /// `"a` to `"z`.
    named: [Register; 26],
    /// `"0` to `"9`.
    numbered: [Register; 10],
    /// `"-`.
    small: Register,
    /// `"+` and `"*`, as last put there, here or elsewhere.
    system: [Register; 2],
    /// What's to go on the clipboard or the primary selection, for the
    /// window to send on: the last for each.
    to_system: Vec<(String, Copy)>,
    /// Whether the window has put something on the clipboard, or the
    /// primary selection, since it last asked what's there: the answer
    /// is older than that, and is dropped.
    put_since_asked: [bool; 2],
    /// Which the unnamed register goes along with, as `clipboard` says.
    sync: Clipboard,
    /// The register a macro is being recorded into, and the keys typed so
    /// far.
    recording: Option<(char, Vec<Stroke>)>,
    /// The register last typed with `@`, for `@@`.
    replayed: Option<char>,
}

impl Default for Registers {
    fn default() -> Self {
        Self {
            unnamed: Register::default(),
            named: Default::default(),
            numbered: Default::default(),
            small: Register::default(),
            system: Default::default(),
            to_system: Vec::new(),
            put_since_asked: [false; 2],
            sync: crate::options::Options::default().clipboard,
            recording: None,
            replayed: None,
        }
    }
}

impl Registers {
    /// Takes in `clipboard`, as the options have it.
    pub(crate) fn set_sync(&mut self, sync: Clipboard) {
        self.sync = sync;
    }

    /// The register the unnamed goes along with, if any.
    fn synced(&self) -> Option<Copy> {
        match self.sync {
            Clipboard::UnnamedPlus => Some(Copy::Clipboard),
            Clipboard::Unnamed => Some(Copy::Primary),
            Clipboard::Off => None,
        }
    }

    /// What the register `name` holds, or the unnamed's if none is named,
    /// `None` if it's empty. `"_` is always empty.
    pub(crate) fn get(&self, name: Option<char>) -> Option<&Register> {
        let register = match name {
            None | Some('"') => match self.synced() {
                Some(copy) if !self.system[system_index(copy)].text.is_empty() => {
                    &self.system[system_index(copy)]
                }
                _ => &self.unnamed,
            },
            Some(name @ 'a'..='z') => &self.named[letter(name)],
            Some(name @ 'A'..='Z') => &self.named[letter(name.to_ascii_lowercase())],
            Some(name @ '0'..='9') => &self.numbered[name as usize - '0' as usize],
            Some('-') => &self.small,
            Some('+') => &self.system[0],
            Some('*') => &self.system[1],
            Some(_) => return None,
        };
        (!register.text.is_empty()).then_some(register)
    }

    /// Keeps `register`, which `wrote` made, in the register `name`, or as
    /// Vim keeps it if none is named, and in the unnamed, unless it's
    /// `"_`.
    pub(crate) fn wrote(&mut self, name: Option<char>, register: Register, wrote: Wrote) {
        let name = name.filter(|&name| name != '"');
        // Nothing taken, as by `D` on an empty line, keeps nothing, as in
        // Vim, so it doesn't empty the registers or the clipboard.
        if name == Some('_') || register.text.is_empty() {
            return;
        }
        if let Wrote::Delete { small, history } = wrote {
            if history {
                self.numbered[1..].rotate_right(1);
                self.numbered[1] = register.clone();
            }
            if small && name.is_none() {
                self.small = register.clone();
            }
        }
        if let Some(name) = name {
            self.unnamed = self.set(name, register);
            return;
        }
        if wrote == Wrote::Yank {
            self.numbered[0] = register.clone();
        }
        if let Some(copy) = self.synced() {
            self.send(copy, &register);
        }
        self.unnamed = register;
    }

    /// Puts `register` in the one called `name`, or adds it to the end for
    /// `A` to `Z`. Returns what that register then holds.
    fn set(&mut self, name: char, register: Register) -> Register {
        let kept = match name {
            'a'..='z' => &mut self.named[letter(name)],
            'A'..='Z' => {
                let kept = &mut self.named[letter(name.to_ascii_lowercase())];
                kept.append(register);
                return kept.clone();
            }
            '0'..='9' => &mut self.numbered[name as usize - '0' as usize],
            '-' => &mut self.small,
            '+' | '*' => {
                let copy = if name == '+' {
                    Copy::Clipboard
                } else {
                    Copy::Primary
                };
                self.send(copy, &register);
                return register;
            }
            _ => return register,
        };
        *kept = register.clone();
        register
    }

    /// Puts `register` on the clipboard, or the primary selection, once the
    /// window sends it on.
    fn send(&mut self, copy: Copy, register: &Register) {
        self.system[system_index(copy)] = register.clone();
        self.put_since_asked[system_index(copy)] = true;
        self.to_system.retain(|&(_, to)| to != copy);
        self.to_system.push((register.text.clone(), copy));
    }

    /// The window put `text` on the clipboard, or the primary selection,
    /// itself, as copying in a terminal does: `"+` or `"*` hold it.
    pub(crate) fn copied(&mut self, copy: Copy, text: String) {
        self.put_since_asked[system_index(copy)] = true;
        self.take_in(copy, text);
    }

    /// The window is asking what's on the clipboard and the primary
    /// selection, to hear in [`heard_system`](Self::heard_system).
    pub(crate) fn asking_system(&mut self) {
        self.put_since_asked = [false; 2];
    }

    /// Takes what's to go on the clipboard or the primary selection, for
    /// the window to put there.
    pub(crate) fn take_to_system(&mut self) -> Vec<(String, Copy)> {
        std::mem::take(&mut self.to_system)
    }

    /// The clipboard, or the primary selection, holds `text`, as the
    /// window has heard: it's `"+` or `"*`, whole lines if it ends with a
    /// line break, unless it's what the editors put there, whose kind is
    /// known.
    /// But if the window has put something there since it asked, what it
    /// heard is older, and dropped.
    pub(crate) fn heard_system(&mut self, copy: Copy, text: String) {
        if !self.put_since_asked[system_index(copy)] {
            self.take_in(copy, text);
        }
    }

    /// `"+` or `"*` holds `text`, as it's on the clipboard, or the primary
    /// selection, keeping its kind if it's what was there.
    fn take_in(&mut self, copy: Copy, text: String) {
        let register = &mut self.system[system_index(copy)];
        if register.text != text {
            *register = Register {
                lines: text.ends_with('\n'),
                text,
            };
        }
    }

    // --- Macros -------------------------------------------------------------

    /// The register a macro is being recorded into, if one is.
    pub(crate) fn recording(&self) -> Option<char> {
        self.recording.as_ref().map(|(name, _)| *name)
    }

    /// Starts recording the keys typed into the register `name`.
    pub(crate) fn record(&mut self, name: char) {
        self.recording = Some((name, Vec::new()));
    }

    /// Notes `stroke`, typed, if a macro is being recorded.
    pub(crate) fn typed(&mut self, stroke: Stroke) {
        if let Some((_, strokes)) = &mut self.recording {
            strokes.push(stroke);
        }
    }

    /// Stops recording, keeping the keys typed in the register.
    pub(crate) fn stop_recording(&mut self) {
        let Some((name, strokes)) = self.recording.take() else {
            return;
        };
        let text = Stroke::write(&strokes);
        self.set(name, Register { text, lines: false });
    }

    /// The keys the register `name` holds, to type as a macro: `@` is the
    /// last register typed so. Says why not if there are none.
    pub(crate) fn macro_keys(&mut self, name: char) -> Result<Vec<Stroke>, String> {
        let name = match name {
            '@' => self.replayed.ok_or("no macro has been typed yet")?,
            name => name,
        };
        let register = self
            .get(Some(name))
            .ok_or(format!("register {name} is empty"))?;
        let strokes = Stroke::read(&register.text);
        self.replayed = Some(name);
        Ok(strokes)
    }

    /// The registers that hold something, a line for each as `:registers`
    /// lists them, as Neovim does: `l` for lines or `c` for characters, the
    /// name, and the start of the text, with line breaks and other control
    /// characters as `^J` and the like. Unnamed first, then numbered,
    /// named, small, clipboard and primary.
    pub(crate) fn listing(&self) -> Vec<String> {
        let names = std::iter::once('"')
            .chain('0'..='9')
            .chain('a'..='z')
            .chain(['-', '+', '*']);
        names
            .filter_map(|name| {
                let register = self.get(Some(name))?;
                let kind = if register.lines { 'l' } else { 'c' };
                Some(format!("{kind}  \"{name}   {}", shown(&register.text)))
            })
            .collect()
    }
}

/// How much of a register's text `:registers` shows, in characters.
const SHOWN: usize = 100;

/// The start of `text`, on one line, with control characters as `^` and a
/// letter, as Vim shows them: a line break as `^J`.
fn shown(text: &str) -> String {
    let mut shown = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index == SHOWN {
            shown.push('…');
            break;
        }
        match u8::try_from(ch) {
            Ok(code @ 0..=0x1f) => {
                shown.push('^');
                shown.push(char::from(code + b'@'));
            }
            Ok(0x7f) => shown.push_str("^?"),
            _ => shown.push(ch),
        }
    }
    shown
}

/// Where the register for the letter `name`, `a` to `z`, is kept.
fn letter(name: char) -> usize {
    name as usize - 'a' as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Register {
        Register {
            text: text.into(),
            lines: false,
        }
    }

    fn lines(text: &str) -> Register {
        Register {
            text: text.into(),
            lines: true,
        }
    }

    fn text_of(registers: &Registers, name: char) -> Option<&str> {
        registers
            .get(Some(name))
            .map(|register| register.text.as_str())
    }

    fn registers() -> Registers {
        let mut registers = Registers::default();
        registers.set_sync(Clipboard::Off);
        registers
    }

    #[test]
    fn capitals_add_to_their_registers_as_lines_if_either_is() {
        let mut registers = registers();
        registers.wrote(Some('a'), chars("one"), Wrote::Yank);
        registers.wrote(Some('A'), chars(" two"), Wrote::Yank);
        assert_eq!(text_of(&registers, 'a'), Some("one two"));
        registers.wrote(Some('A'), lines("three\n"), Wrote::Yank);
        assert_eq!(registers.get(Some('a')), Some(&lines("one two\nthree\n")));
        assert_eq!(registers.get(None), Some(&lines("one two\nthree\n")));
        // To an empty register, adding is putting.
        registers.wrote(Some('B'), chars("b"), Wrote::Yank);
        assert_eq!(registers.get(Some('b')), Some(&chars("b")));
        assert_eq!(text_of(&registers, '0'), None, "named, so not 0");
    }

    #[test]
    fn deletes_push_down_the_numbered_and_small_ones_go_to_minus() {
        let mut registers = registers();
        let delete = Wrote::Delete {
            small: false,
            history: true,
        };
        for line in ["1\n", "2\n", "3\n"] {
            registers.wrote(None, lines(line), delete);
        }
        assert_eq!(text_of(&registers, '1'), Some("3\n"));
        assert_eq!(text_of(&registers, '3'), Some("1\n"));
        let small = Wrote::Delete {
            small: true,
            history: false,
        };
        registers.wrote(None, chars("x"), small);
        assert_eq!(text_of(&registers, '-'), Some("x"));
        assert_eq!(text_of(&registers, '1'), Some("3\n"), "not pushed down");
        assert_eq!(text_of(&registers, '"'), Some("x"));
        // Named, a line still pushes them down, but a small one isn't `-`.
        registers.wrote(Some('q'), lines("4\n"), delete);
        assert_eq!(text_of(&registers, '1'), Some("4\n"));
        registers.wrote(Some('q'), chars("y"), small);
        assert_eq!(text_of(&registers, '-'), Some("x"));
        // Ten deep, the oldest falls off the end.
        for line in 0..10 {
            registers.wrote(None, lines(&format!("{line}\n")), delete);
        }
        assert_eq!(text_of(&registers, '9'), Some("1\n"));
    }

    #[test]
    fn the_black_hole_keeps_nothing_and_changes_nothing() {
        let mut registers = registers();
        registers.wrote(None, chars("kept"), Wrote::Yank);
        let delete = Wrote::Delete {
            small: false,
            history: true,
        };
        registers.wrote(Some('_'), lines("gone\n"), delete);
        assert_eq!(text_of(&registers, '"'), Some("kept"));
        assert_eq!(text_of(&registers, '1'), None);
        assert_eq!(text_of(&registers, '_'), None);
    }

    #[test]
    fn the_unnamed_goes_along_with_the_clipboard_if_clipboard_says() {
        let mut registers = registers();
        registers.wrote(None, chars("mine"), Wrote::Yank);
        assert!(registers.take_to_system().is_empty());
        registers.wrote(Some('+'), lines("plus\n"), Wrote::Yank);
        assert_eq!(
            registers.take_to_system(),
            [("plus\n".to_owned(), Copy::Clipboard)]
        );
        registers.set_sync(Clipboard::UnnamedPlus);
        registers.wrote(None, chars("both"), Wrote::Yank);
        assert_eq!(
            registers.take_to_system(),
            [("both".to_owned(), Copy::Clipboard)]
        );
        // Copied elsewhere: `p` puts it, and it's whole lines if it ends so.
        registers.asking_system();
        registers.heard_system(Copy::Clipboard, "from a browser\n".into());
        assert_eq!(registers.get(None), Some(&lines("from a browser\n")));
        // Heard back as it was put there, it keeps its kind.
        registers.wrote(None, chars("x\n"), Wrote::Yank);
        registers.asking_system();
        registers.heard_system(Copy::Clipboard, "x\n".into());
        assert_eq!(registers.get(None), Some(&chars("x\n")));
        assert_eq!(text_of(&registers, '*'), None);
        // Asked, then put there before the answer came: the answer's older.
        registers.asking_system();
        registers.wrote(None, chars("newer"), Wrote::Yank);
        registers.heard_system(Copy::Clipboard, "older".into());
        assert_eq!(registers.get(None), Some(&chars("newer")));
        // As when a terminal copies.
        registers.asking_system();
        registers.copied(Copy::Primary, "selected".into());
        registers.heard_system(Copy::Primary, "older".into());
        assert_eq!(text_of(&registers, '*'), Some("selected"));
    }

    #[test]
    fn nothing_taken_keeps_nothing() {
        let mut registers = registers();
        registers.set_sync(Clipboard::UnnamedPlus);
        registers.wrote(None, chars("kept"), Wrote::Yank);
        registers.take_to_system();
        let empty = Wrote::Delete {
            small: true,
            history: false,
        };
        registers.wrote(None, chars(""), empty);
        assert_eq!(text_of(&registers, '"'), Some("kept"));
        assert_eq!(text_of(&registers, '-'), None);
        assert!(registers.take_to_system().is_empty());
    }

    #[test]
    fn a_macro_is_kept_as_text() {
        let mut registers = registers();
        registers.record('m');
        for stroke in Stroke::read("ihi<Esc>") {
            registers.typed(stroke);
        }
        registers.stop_recording();
        assert_eq!(registers.recording(), None);
        assert_eq!(text_of(&registers, 'm'), Some("ihi<Esc>"));
        assert_eq!(registers.macro_keys('m'), Ok(Stroke::read("ihi<Esc>")));
        assert_eq!(registers.macro_keys('@'), Ok(Stroke::read("ihi<Esc>")));
        assert!(registers.macro_keys('z').is_err());
    }

    #[test]
    fn registers_are_listed_by_kind_and_name_on_a_line_each() {
        let mut registers = registers();
        registers.wrote(Some('a'), lines("one\n\ttwo\n"), Wrote::Yank);
        registers.wrote(None, chars("x"), Wrote::Yank);
        assert_eq!(
            registers.listing(),
            ["c  \"\"   x", "c  \"0   x", "l  \"a   one^J^Itwo^J"]
        );
    }
}
