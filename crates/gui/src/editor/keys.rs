//! What keys mean in an editor's normal and visual modes: a command, as
//! Vim reads one, from the keys typed so far.
//!
//! A command is a count, then an operator and what it acts on (`d2w`,
//! `ci"`, `yy`), or a motion (`3j`, `f(`), or a key of its own (`x`, `p`,
//! `o`). A register can come before it, after `"`, with a count either
//! side (`"a3yy`, `2"bp`). The keys are read afresh as each arrives, until
//! they make a command or can't.

use clackatoo_protocol::{Construct, Query};

use super::case::Recase;
use crate::keymap::{Base, Chord, Named};
use crate::motion::Motion;
use crate::objects::Object;

/// One key, as an editor reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stroke {
    /// A character, typed with Ctrl if `ctrl`.
    Char {
        ch: char,
        ctrl: bool,
    },
    Escape,
    Enter,
    Tab,
    /// Shift+Tab, which goes back a place in a snippet, and is Tab
    /// otherwise.
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

impl Stroke {
    pub(crate) fn char(ch: char) -> Self {
        Self::Char { ch, ctrl: false }
    }

    pub(crate) fn ctrl(ch: char) -> Self {
        Self::Char { ch, ctrl: true }
    }

    fn plain(self) -> Option<char> {
        match self {
            Self::Char { ch, ctrl: false } => Some(ch),
            _ => None,
        }
    }

    /// The keys `text` writes down, for tests: a character for itself,
    /// and a key that types none by its name in angle brackets, `<Esc>`,
    /// `<CR>` or `<C-r>`, with `<lt>` for `<`. A `<` that starts no name
    /// is itself. Line breaks are Enter, and the other control characters
    /// the keys that type them.
    #[cfg(test)]
    pub(crate) fn read(text: &str) -> Vec<Self> {
        let mut strokes = Vec::with_capacity(text.len());
        let mut rest = text;
        while let Some(ch) = rest.chars().next() {
            if ch == '<'
                && let Some(end) = rest[1..].find('>').map(|end| end + 1)
                && let Some(stroke) = Self::named(&rest[1..end])
            {
                strokes.push(stroke);
                rest = &rest[end + 1..];
                continue;
            }
            strokes.push(match ch {
                '\n' | '\r' => Self::Enter,
                '\t' => Self::Tab,
                '\u{1b}' => Self::Escape,
                '\u{8}' | '\u{7f}' => Self::Backspace,
                // Ctrl+A to Ctrl+Z.
                '\u{1}'..='\u{1a}' => Self::ctrl(char::from(ch as u8 + b'a' - 1)),
                _ => Self::char(ch),
            });
            rest = &rest[ch.len_utf8()..];
        }
        strokes
    }

    /// The key called `name`, between angle brackets, if it's one.
    #[cfg(test)]
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "Esc" => Self::Escape,
            "CR" => Self::Enter,
            "Tab" => Self::Tab,
            "S-Tab" => Self::BackTab,
            "BS" => Self::Backspace,
            "Del" => Self::Delete,
            "Left" => Self::Left,
            "Right" => Self::Right,
            "Up" => Self::Up,
            "Down" => Self::Down,
            "Home" => Self::Home,
            "End" => Self::End,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            "lt" => Self::char('<'),
            "Space" => Self::char(' '),
            "C-Space" => Self::ctrl(' '),
            _ => {
                let mut key = name.strip_prefix("C-")?.chars();
                let ch = key.next()?;
                if key.next().is_some() {
                    return None;
                }
                Self::ctrl(ch)
            }
        })
    }

    /// The key that types the stroke, as a macro records it: for what a
    /// completion menu put in, which is recorded in place of the key that
    /// took it.
    pub(crate) fn chord(self) -> Chord {
        let named = |named| Chord::with(Base::Named(named), false, false, false);
        match self {
            Self::Char { ch, ctrl } => Chord::with(Base::Char(ch), ctrl, false, false),
            Self::Escape => named(Named::Escape),
            Self::Enter => named(Named::Enter),
            Self::Tab => named(Named::Tab),
            Self::BackTab => Chord::with(Base::Named(Named::Tab), false, false, true),
            Self::Backspace => named(Named::Backspace),
            Self::Delete => named(Named::Delete),
            Self::Left => named(Named::Left),
            Self::Right => named(Named::Right),
            Self::Up => named(Named::Up),
            Self::Down => named(Named::Down),
            Self::Home => named(Named::Home),
            Self::End => named(Named::End),
            Self::PageUp => named(Named::PageUp),
            Self::PageDown => named(Named::PageDown),
        }
    }
}

/// Whether `ch` names a register, after `"`: `a` to `z`, or `A` to `Z` to
/// add to them, `0` to `9`, `"` for the unnamed, `-` for small deletes,
/// `_` for none, and `+` and `*` for the clipboard and the primary
/// selection.
pub(crate) fn is_register(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '"' | '-' | '_' | '+' | '*')
}

/// Whether `ch` names a mark that can be set, after `m`: `a` to `z` in
/// the file, `A` to `Z` anywhere, and `'` or `` ` `` for the place before
/// the last jump.
fn is_settable_mark(ch: char) -> bool {
    ch.is_ascii_alphabetic() || matches!(ch, '\'' | '`')
}

/// Whether `ch` names a mark to go to, after `'` or `` ` ``: one that can
/// be set, or `.` for the last change.
fn is_mark(ch: char) -> bool {
    is_settable_mark(ch) || ch == '.'
}

/// What an operator does to the text it's given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operator {
    /// `d`: delete it, keeping it to put back.
    Delete,
    /// `c`: delete it and type in its place.
    Change,
    /// `y`: copy it.
    Yank,
    /// `>`: indent its lines, whole, by `shiftwidth`.
    Indent,
    /// `<`: take as much off their indent.
    Dedent,
    /// `g~`, `gu`, `gU` and `g?`: change its case.
    Recase(Recase),
    /// `=`: indent its lines as their language's grammar says.
    Reindent,
}

/// What an operator acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// What the motion moves over, `count` times.
    Motion(Motion),
    /// A text object around the cursor.
    Object { object: Object, inner: bool },
    /// Whole lines, from the cursor's: `dd`, `cc` and `yy`.
    Lines,
    /// What's selected, in visual mode.
    Selection,
    /// To a mark: from line to line after `'`, or exactly, as an exclusive
    /// motion, after `` ` ``.
    Mark { mark: char, exact: bool },
}

/// Where insert mode starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// `i`: before the cursor.
    Before,
    /// `a`: after it.
    After,
    /// `I`: before the line's first character that isn't blank.
    LineStart,
    /// `A`: at the line's end.
    LineEnd,
    /// `o`: on a new line below.
    Below,
    /// `O`: on a new line above.
    Above,
}

/// Where to put the view, by the cursor's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewAt {
    /// `zt`: the cursor's line at the top.
    Top,
    /// `zz`: in the middle.
    Middle,
    /// `zb`: at the bottom.
    Bottom,
}

/// A command, how many times to do it, and the register it takes text
/// from or keeps it in, if one's named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) count: Option<usize>,
    pub(crate) action: Action,
    pub(crate) register: Option<char>,
}

impl Command {
    /// The count, or 1.
    pub(crate) fn times(self) -> usize {
        self.count.unwrap_or(1)
    }
}

/// What a command does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Move(Motion),
    /// `G` with a count: to that line.
    GoToLine,
    Operate(Operator, Target),
    /// `x`, or `X` if `before`.
    DeleteChars {
        before: bool,
    },
    /// `D`, or `C` if `change`.
    DeleteToEnd {
        change: bool,
    },
    /// `s`.
    Substitute,
    /// `p`, or `P` if `before`.
    Put {
        before: bool,
    },
    /// `J`.
    Join,
    /// `r` and the character.
    Replace(char),
    Insert(Entry),
    Undo,
    Redo,
    /// `.`.
    Repeat,
    /// `v`, or `V` if `lines`.
    Visual {
        lines: bool,
    },
    /// `/`, or `?` if `backward`.
    Search {
        backward: bool,
    },
    /// `n`, or `N` if `reverse`.
    SearchAgain {
        reverse: bool,
    },
    /// `*`, or `#` if `backward`: the word under the cursor, as a whole
    /// word.
    SearchWord {
        backward: bool,
    },
    /// `;`, or `,` if `reverse`.
    FindAgain {
        reverse: bool,
    },
    /// Ctrl+D and Ctrl+U: half a page, down or up if negative.
    HalfPage(i8),
    /// Ctrl+F and Page Down, Page Up: a page.
    Page(i8),
    View(ViewAt),
    /// `]d`, or `[d` if `backward`: to the next diagnostic.
    Diagnostic {
        backward: bool,
    },
    /// `]c`, or `[c` if `backward`: to the next hunk, a change since the
    /// file's text in the index.
    Hunk {
        backward: bool,
    },
    /// `]f`, or `[f` if `backward`: to the start of the next function, as
    /// the grammar finds them; `]C` and `[C` to the next class.
    Piece {
        construct: Construct,
        backward: bool,
    },
    /// `gd` and the like: ask the language server, and go where it says.
    Ask(Query),
    /// `K`: ask the language server what's under the cursor, and show
    /// what it says, with what's wrong there, in a panel by the cursor.
    Hover,
    /// Ctrl+W `d`: show what's wrong at the cursor, or on its line, in
    /// full, in the same panel.
    ShowDiagnostics,
    /// `grn`: rename what's under the cursor, everywhere, asking in the
    /// bar for the new name.
    Rename,
    /// `gra`: ask what the language server could do to the text at the
    /// cursor, or selected, such as fix what's wrong there, and show it in
    /// a menu to pick from.
    CodeActions,
    /// `gq` in visual mode: format what's selected, as the language server
    /// does.
    FormatSelection,
    /// `o` in visual mode: to the other end of the selection.
    OtherEnd,
    /// `i` or `a` and an object, in visual mode: select it.
    Select {
        object: Object,
        inner: bool,
    },
    /// `q` and a register: record the keys typed into it, until `q`.
    Record(char),
    /// `@` and a register: type the keys it holds, as if typed again.
    /// `@@` types the last register typed so again.
    Replay(char),
    /// `m` and a mark: set it at the cursor.
    SetMark(char),
    /// `` ` `` and a mark: go to it, or with `'`, to its line's first
    /// character that isn't blank. `''` and ``` `` ``` go back to before
    /// the last jump.
    Mark {
        mark: char,
        exact: bool,
    },
    /// `~`, with `tildeop` off: turn over the case of the character under
    /// the cursor, and as many after it as the count, moving on past them.
    Tilde,
    /// `&`: the last `:s` again on the cursor's line, without its flags;
    /// or `g&`, on every line, with them, and the last search's pattern.
    RepeatSubstitute {
        all: bool,
    },
}

impl Action {
    /// Whether it changes the text, and so is a change for `.` to make
    /// again.
    pub(crate) fn changes(self) -> bool {
        matches!(
            self,
            Self::Operate(
                Operator::Delete
                    | Operator::Change
                    | Operator::Indent
                    | Operator::Dedent
                    | Operator::Recase(_)
                    | Operator::Reindent,
                _
            ) | Self::DeleteChars { .. }
                | Self::Tilde
                | Self::DeleteToEnd { .. }
                | Self::Substitute
                | Self::Put { .. }
                | Self::Join
                | Self::Replace(_)
                | Self::Insert(_)
        )
    }

    /// Whether it's a jump, which the jump list records, as Vim's are:
    /// `gg`, `G`, `n` and `N`, `]d` and `[d`, `]c` and `[c`, `]f` and the
    /// like, and going to a mark. A search typed after `/` is too, once
    /// Enter finds it, as is going where a language server says, once it
    /// answers.
    pub(crate) fn jumps(self) -> bool {
        matches!(
            self,
            Self::Move(Motion::First | Motion::Last | Motion::Match | Motion::Paragraph { .. })
                | Self::GoToLine
                | Self::SearchAgain { .. }
                | Self::SearchWord { .. }
                | Self::Diagnostic { .. }
                | Self::Hunk { .. }
                | Self::Piece { .. }
                | Self::Mark { .. }
        )
    }
}

/// What the keys typed so far make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// More keys are needed.
    Incomplete,
    /// They make nothing: start again.
    Nothing,
    Command(Command),
}

/// Reads `keys` as a normal-mode command, or in `visual` mode, with
/// `tildeop` off.
#[cfg(test)]
pub(crate) fn parse(keys: &[Stroke], visual: bool) -> Parsed {
    parse_with(keys, visual, false)
}

/// Reads `keys` as a normal-mode command, or in `visual` mode, where
/// operators act on the selection and `i` and `a` start an object; `~`
/// is an operator if `tildeop`.
pub(crate) fn parse_with(keys: &[Stroke], visual: bool, tildeop: bool) -> Parsed {
    let mut reader = Reader {
        keys,
        at: 0,
        register: None,
        tildeop,
    };
    let first = reader.count();
    // A register, and a count after it, which multiplies the first.
    let first = if reader.peek() == Some(Stroke::char('"')) {
        reader.at += 1;
        match reader.next().map(Stroke::plain) {
            None => return Parsed::Incomplete,
            Some(Some(name)) if is_register(name) => reader.register = Some(name),
            Some(_) => return Parsed::Nothing,
        }
        times(first, reader.count())
    } else {
        first
    };
    let register = reader.register;
    let command = |count, action| {
        Parsed::Command(Command {
            count,
            action,
            register,
        })
    };
    let Some(stroke) = reader.next() else {
        return Parsed::Incomplete;
    };
    if stroke == Stroke::char('g') {
        // `gq` on a selection formats it, as its language server does.
        if visual && reader.peek().and_then(Stroke::plain) == Some('q') {
            return command(first, Action::FormatSelection);
        }
        // `g~`, `gu`, `gU` and `g?`, which change case, and `g&`.
        let recase = match reader.peek().and_then(Stroke::plain) {
            Some('~') => Some(Recase::Toggle),
            Some('u') => Some(Recase::Lower),
            Some('U') => Some(Recase::Upper),
            Some('?') => Some(Recase::Rot13),
            Some('&') => return command(first, Action::RepeatSubstitute { all: true }),
            _ => None,
        };
        if let Some(recase) = recase {
            let key = reader.next().and_then(Stroke::plain).unwrap_or('~');
            let operator = Operator::Recase(recase);
            if visual {
                return command(first, Action::Operate(operator, Target::Selection));
            }
            return reader.target(first, operator, key);
        }
    }
    if let Some(motion) = reader.motion(stroke) {
        return match motion {
            Read::More => Parsed::Incomplete,
            Read::No => Parsed::Nothing,
            Read::Yes(Action::Move(Motion::Last)) if first.is_some() => {
                command(first, Action::GoToLine)
            }
            Read::Yes(action) => command(first, action),
        };
    }
    let action = match stroke {
        Stroke::Char {
            ch: 'd',
            ctrl: true,
        } => Action::HalfPage(1),
        Stroke::Char {
            ch: 'u',
            ctrl: true,
        } => Action::HalfPage(-1),
        Stroke::Char {
            ch: 'f',
            ctrl: true,
        }
        | Stroke::PageDown => Action::Page(1),
        Stroke::PageUp => Action::Page(-1),
        Stroke::Char {
            ch: 'r',
            ctrl: true,
        } => Action::Redo,
        // Ctrl+W `d`, or Ctrl+W Ctrl+D, as Neovim's.
        Stroke::Char {
            ch: 'w',
            ctrl: true,
        } => match reader.next() {
            None => return Parsed::Incomplete,
            Some(Stroke::Char { ch: 'd', .. }) => Action::ShowDiagnostics,
            Some(_) => return Parsed::Nothing,
        },
        Stroke::Delete => Action::DeleteChars { before: false },
        Stroke::Char { ctrl: false, ch } => match reader.key(ch, first, visual) {
            Ok(action) => action,
            Err(parsed) => return parsed,
        },
        _ => return Parsed::Nothing,
    };
    command(first, action)
}

/// Whether a motion was read.
enum Read<T> {
    Yes(T),
    /// It needs another key.
    More,
    /// The keys aren't one.
    No,
}

/// Two counts, one multiplying the other, as in `2d3w`.
fn times(first: Option<usize>, second: Option<usize>) -> Option<usize> {
    match (first, second) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
    }
}

struct Reader<'a> {
    keys: &'a [Stroke],
    at: usize,
    /// The register named before the command, if one was.
    register: Option<char>,
    /// `~` is an operator.
    tildeop: bool,
}

impl Reader<'_> {
    /// The plain key after this one, if it's one `wanted` accepts, made
    /// into an action by `make`.
    fn then(
        &mut self,
        wanted: impl Fn(char) -> bool,
        make: impl FnOnce(char) -> Action,
    ) -> Result<Action, Parsed> {
        match self.next() {
            None => Err(Parsed::Incomplete),
            Some(next) => match next.plain().filter(|&ch| wanted(ch)) {
                Some(ch) => Ok(make(ch)),
                None => Err(Parsed::Nothing),
            },
        }
    }

    /// What `q`, `@`, `m`, `'` or `` ` ``, `ch`, does with the register or
    /// mark named after it.
    fn macro_or_mark(&mut self, ch: char) -> Result<Action, Parsed> {
        match ch {
            'q' => self.then(|name| name.is_ascii_alphanumeric(), Action::Record),
            '@' => self.then(
                |name| (is_register(name) && name != '_') || name == '@',
                Action::Replay,
            ),
            'm' => self.then(is_settable_mark, Action::SetMark),
            _ => self.then(is_mark, |mark| Action::Mark {
                mark,
                exact: ch == '`',
            }),
        }
    }

    /// The action of the plain key `ch`, which may read keys after it, or
    /// what the keys make if not an action: an operator reads its target.
    fn key(&mut self, ch: char, first: Option<usize>, visual: bool) -> Result<Action, Parsed> {
        let register = self.register;
        let command = |count, action| {
            Parsed::Command(Command {
                count,
                action,
                register,
            })
        };
        Ok(match ch {
            'd' | 'c' | 'y' | '>' | '<' | '=' => {
                let operator = match ch {
                    'd' => Operator::Delete,
                    'c' => Operator::Change,
                    'y' => Operator::Yank,
                    '>' => Operator::Indent,
                    '=' => Operator::Reindent,
                    _ => Operator::Dedent,
                };
                if visual {
                    return Err(command(first, Action::Operate(operator, Target::Selection)));
                }
                return Err(self.target(first, operator, ch));
            }
            'x' if visual => Action::Operate(Operator::Delete, Target::Selection),
            '~' if visual => Action::Operate(Operator::Recase(Recase::Toggle), Target::Selection),
            'u' if visual => Action::Operate(Operator::Recase(Recase::Lower), Target::Selection),
            'U' if visual => Action::Operate(Operator::Recase(Recase::Upper), Target::Selection),
            '~' if self.tildeop => {
                return Err(self.target(first, Operator::Recase(Recase::Toggle), '~'));
            }
            '~' => Action::Tilde,
            '&' => Action::RepeatSubstitute { all: false },
            's' if visual => Action::Operate(Operator::Change, Target::Selection),
            'i' | 'a' if visual => {
                let Some(next) = self.next() else {
                    return Err(Parsed::Incomplete);
                };
                match next.plain().and_then(Object::of_key) {
                    Some(object) => Action::Select {
                        object,
                        inner: ch == 'i',
                    },
                    None => return Err(Parsed::Nothing),
                }
            }
            'o' if visual => Action::OtherEnd,
            'q' | '@' | 'm' | '\'' | '`' => self.macro_or_mark(ch)?,
            'x' => Action::DeleteChars { before: false },
            'X' => Action::DeleteChars { before: true },
            'D' => Action::DeleteToEnd { change: false },
            'C' => Action::DeleteToEnd { change: true },
            's' => Action::Substitute,
            'S' => Action::Operate(Operator::Change, Target::Lines),
            'Y' => Action::Operate(Operator::Yank, Target::Lines),
            'p' => Action::Put { before: false },
            'P' => Action::Put { before: true },
            'J' => Action::Join,
            'r' => match self.next() {
                None => return Err(Parsed::Incomplete),
                Some(next) => match next.plain() {
                    Some(ch) => Action::Replace(ch),
                    None => return Err(Parsed::Nothing),
                },
            },
            'i' => Action::Insert(Entry::Before),
            'a' => Action::Insert(Entry::After),
            'I' => Action::Insert(Entry::LineStart),
            'A' => Action::Insert(Entry::LineEnd),
            'o' => Action::Insert(Entry::Below),
            'O' => Action::Insert(Entry::Above),
            'u' => Action::Undo,
            'K' => Action::Hover,
            '.' => Action::Repeat,
            'v' => Action::Visual { lines: false },
            'V' => Action::Visual { lines: true },
            '/' => Action::Search { backward: false },
            '?' => Action::Search { backward: true },
            'n' => Action::SearchAgain { reverse: false },
            '*' => Action::SearchWord { backward: false },
            '#' => Action::SearchWord { backward: true },
            'N' => Action::SearchAgain { reverse: true },
            ';' => Action::FindAgain { reverse: false },
            ',' => Action::FindAgain { reverse: true },
            'z' => match self.next() {
                None => return Err(Parsed::Incomplete),
                Some(next) => match next.plain() {
                    Some('t') => Action::View(ViewAt::Top),
                    Some('z') => Action::View(ViewAt::Middle),
                    Some('b') => Action::View(ViewAt::Bottom),
                    _ => return Err(Parsed::Nothing),
                },
            },
            bracket @ (']' | '[') => self.bracket(bracket == '[')?,
            _ => return Err(Parsed::Nothing),
        })
    }

    /// What `]`, or `[` if `backward`, goes to by the key after it: the
    /// next diagnostic, hunk, function or class, or the one before.
    fn bracket(&mut self, backward: bool) -> Result<Action, Parsed> {
        let Some(next) = self.next() else {
            return Err(Parsed::Incomplete);
        };
        Ok(match next.plain() {
            Some('d') => Action::Diagnostic { backward },
            Some('c') => Action::Hunk { backward },
            Some(key @ ('f' | 'C')) => Action::Piece {
                construct: if key == 'f' {
                    Construct::Function
                } else {
                    Construct::Class
                },
                backward,
            },
            _ => return Err(Parsed::Nothing),
        })
    }

    fn next(&mut self) -> Option<Stroke> {
        let stroke = self.keys.get(self.at).copied();
        self.at += 1;
        stroke
    }

    fn peek(&self) -> Option<Stroke> {
        self.keys.get(self.at).copied()
    }

    /// A count, if one is typed.
    fn count(&mut self) -> Option<usize> {
        let mut count = None::<usize>;
        while let Some(Stroke::Char { ch, ctrl: false }) = self.peek() {
            let Some(digit) = ch.to_digit(10) else {
                break;
            };
            // A 0 that doesn't follow a digit is a motion.
            if digit == 0 && count.is_none() {
                break;
            }
            count = Some(
                count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit as usize),
            );
            self.at += 1;
        }
        count
    }

    /// The motion `stroke` starts, if it starts one.
    fn motion(&mut self, stroke: Stroke) -> Option<Read<Action>> {
        let motion = match stroke {
            Stroke::Left | Stroke::Backspace => Motion::Left,
            Stroke::Right => Motion::Right,
            Stroke::Up => Motion::Up(1),
            Stroke::Down | Stroke::Enter => Motion::Down(1),
            Stroke::Home => Motion::LineStart,
            Stroke::End => Motion::LineEnd,
            Stroke::Char { ch, ctrl: false } => match ch {
                'h' => Motion::Left,
                'l' | ' ' => Motion::Right,
                'k' => Motion::Up(1),
                'j' => Motion::Down(1),
                '0' => Motion::LineStart,
                '^' => Motion::FirstNonBlank,
                '$' => Motion::LineEnd,
                'w' => Motion::WordStart,
                'b' => Motion::WordBack,
                'e' => Motion::WordEnd,
                'W' => Motion::BigWordStart,
                'B' => Motion::BigWordBack,
                'E' => Motion::BigWordEnd,
                'G' => Motion::Last,
                '%' => Motion::Match,
                '}' => Motion::Paragraph { forward: true },
                '{' => Motion::Paragraph { forward: false },
                'g' => {
                    // What a language server knows: `gd`, `gD`, `gy`, `gri`,
                    // `grr`, `grn` and `gra`, as Neovim's are.
                    let ask = |query| Read::Yes(Action::Ask(query));
                    let Some(next) = self.next() else {
                        return Some(Read::More);
                    };
                    return Some(match next.plain() {
                        Some('g') => Read::Yes(Action::Move(Motion::First)),
                        // By screen rows, where lines wrap.
                        Some('j') => Read::Yes(Action::Move(Motion::RowDown(1))),
                        Some('k') => Read::Yes(Action::Move(Motion::RowUp(1))),
                        Some('0') => Read::Yes(Action::Move(Motion::RowStart)),
                        Some('$') => Read::Yes(Action::Move(Motion::RowEnd)),
                        Some('d') => ask(Query::Definition),
                        Some('D') => ask(Query::Declaration),
                        Some('y') => ask(Query::TypeDefinition),
                        Some('r') => match self.next() {
                            None => Read::More,
                            Some(last) => match last.plain() {
                                Some('i') => ask(Query::Implementation),
                                Some('r') => ask(Query::References),
                                Some('n') => Read::Yes(Action::Rename),
                                Some('a') => Read::Yes(Action::CodeActions),
                                _ => Read::No,
                            },
                        },
                        _ => Read::No,
                    });
                }
                'f' | 't' | 'F' | 'T' => {
                    return Some(match self.next() {
                        None => Read::More,
                        Some(next) => match next.plain() {
                            Some(target) => Read::Yes(Action::Move(Motion::Find {
                                ch: target,
                                forward: ch.is_lowercase(),
                                till: ch.eq_ignore_ascii_case(&'t'),
                            })),
                            None => Read::No,
                        },
                    });
                }
                _ => return None,
            },
            _ => return None,
        };
        Some(Read::Yes(Action::Move(motion)))
    }

    /// What an operator typed as `key` acts on: a motion or an object, or
    /// `key` again for lines.
    fn target(&mut self, first: Option<usize>, operator: Operator, key: char) -> Parsed {
        let count = times(first, self.count());
        let register = self.register;
        let command = |target| {
            Parsed::Command(Command {
                count,
                action: Action::Operate(operator, target),
                register,
            })
        };
        let Some(stroke) = self.next() else {
            return Parsed::Incomplete;
        };
        if stroke == Stroke::char(key) {
            return command(Target::Lines);
        }
        // `g~g~` as `g~~`, and their kin.
        if matches!(operator, Operator::Recase(_))
            && stroke == Stroke::char('g')
            && self.peek() == Some(Stroke::char(key))
        {
            self.at += 1;
            return command(Target::Lines);
        }
        if let Some(kind @ ('\'' | '`')) = stroke.plain() {
            return match self.then(is_mark, |mark| Action::Mark {
                mark,
                exact: kind == '`',
            }) {
                Ok(Action::Mark { mark, exact }) => command(Target::Mark { mark, exact }),
                Ok(_) => Parsed::Nothing,
                Err(parsed) => parsed,
            };
        }

        if let Some(Stroke::Char {
            ch: kind @ ('i' | 'a'),
            ctrl: false,
        }) = Some(stroke)
        {
            return match self.next() {
                None => Parsed::Incomplete,
                Some(next) => match next.plain().and_then(Object::of_key) {
                    Some(object) => command(Target::Object {
                        object,
                        inner: kind == 'i',
                    }),
                    None => Parsed::Nothing,
                },
            };
        }
        match self.motion(stroke) {
            Some(Read::Yes(Action::Move(motion))) => command(Target::Motion(motion)),
            Some(Read::More) => Parsed::Incomplete,
            _ => Parsed::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<Stroke> {
        text.chars().map(Stroke::char).collect()
    }

    fn read(text: &str) -> Parsed {
        parse(&keys(text), false)
    }

    fn command(count: Option<usize>, action: Action) -> Parsed {
        Parsed::Command(Command {
            count,
            action,
            register: None,
        })
    }

    fn with_register(register: char, count: Option<usize>, action: Action) -> Parsed {
        Parsed::Command(Command {
            count,
            action,
            register: Some(register),
        })
    }

    #[test]
    fn motions_take_counts() {
        assert_eq!(read("j"), command(None, Action::Move(Motion::Down(1))));
        assert_eq!(
            read("12j"),
            command(Some(12), Action::Move(Motion::Down(1)))
        );
        assert_eq!(read("1"), Parsed::Incomplete);
        assert_eq!(read("0"), command(None, Action::Move(Motion::LineStart)));
        assert_eq!(read("10G"), command(Some(10), Action::GoToLine));
        assert_eq!(read("G"), command(None, Action::Move(Motion::Last)));
        assert_eq!(read("g"), Parsed::Incomplete);
        assert_eq!(read("gg"), command(None, Action::Move(Motion::First)));
        assert_eq!(read("gq"), Parsed::Nothing);
        assert_eq!(
            read("3gj"),
            command(Some(3), Action::Move(Motion::RowDown(1)))
        );
        assert_eq!(read("g$"), command(None, Action::Move(Motion::RowEnd)));
        assert_eq!(
            read("dgk"),
            command(
                None,
                Action::Operate(Operator::Delete, Target::Motion(Motion::RowUp(1)))
            )
        );
        let find = Motion::Find {
            ch: '(',
            forward: false,
            till: true,
        };
        assert_eq!(read("T("), command(None, Action::Move(find)));
    }

    #[test]
    fn operators_take_motions_objects_and_themselves() {
        let delete = |target| Action::Operate(Operator::Delete, target);
        assert_eq!(read("d"), Parsed::Incomplete);
        assert_eq!(
            read("dw"),
            command(None, delete(Target::Motion(Motion::WordStart)))
        );
        assert_eq!(
            read("2d3w"),
            command(Some(6), delete(Target::Motion(Motion::WordStart)))
        );
        assert_eq!(read("dd"), command(None, delete(Target::Lines)));
        assert_eq!(read("3dd"), command(Some(3), delete(Target::Lines)));
        assert_eq!(read("di"), Parsed::Incomplete);
        let object = Target::Object {
            object: Object::Quote('"'),
            inner: true,
        };
        assert_eq!(
            read("ci\""),
            command(None, Action::Operate(Operator::Change, object))
        );
        assert_eq!(
            read("dgg"),
            command(None, delete(Target::Motion(Motion::First)))
        );
        assert_eq!(read("dz"), Parsed::Nothing);
        assert_eq!(read("dc"), Parsed::Nothing);
        assert_eq!(read("df"), Parsed::Incomplete);
        let indent = |target| Action::Operate(Operator::Indent, target);
        assert_eq!(read("3>>"), command(Some(3), indent(Target::Lines)));
        assert_eq!(
            read(">j"),
            command(None, indent(Target::Motion(Motion::Down(1))))
        );
        assert_eq!(
            read("<<"),
            command(None, Action::Operate(Operator::Dedent, Target::Lines))
        );
        assert_eq!(read("><"), Parsed::Nothing);
        let reindent = |target| Action::Operate(Operator::Reindent, target);
        assert_eq!(read("2=="), command(Some(2), reindent(Target::Lines)));
        assert_eq!(
            read("=G"),
            command(None, reindent(Target::Motion(Motion::Last)))
        );
        assert_eq!(read("=>"), Parsed::Nothing);
    }

    #[test]
    fn keys_of_their_own() {
        assert_eq!(
            read("3x"),
            command(Some(3), Action::DeleteChars { before: false })
        );
        assert_eq!(read("r"), Parsed::Incomplete);
        assert_eq!(read("rx"), command(None, Action::Replace('x')));
        assert_eq!(read("zz"), command(None, Action::View(ViewAt::Middle)));
        assert_eq!(
            parse(&[Stroke::ctrl('r')], false),
            command(None, Action::Redo)
        );
        assert_eq!(read("Q"), Parsed::Nothing);
    }

    #[test]
    fn visual_mode_operates_on_the_selection() {
        let visual = |text: &str| parse(&keys(text), true);
        assert_eq!(
            visual("d"),
            command(None, Action::Operate(Operator::Delete, Target::Selection))
        );
        assert_eq!(visual("o"), command(None, Action::OtherEnd));
        assert_eq!(
            visual("2<"),
            command(
                Some(2),
                Action::Operate(Operator::Dedent, Target::Selection)
            )
        );
        assert_eq!(visual("i"), Parsed::Incomplete);
        let word = Action::Select {
            object: Object::Word,
            inner: false,
        };
        assert_eq!(visual("aw"), command(None, word));
        assert_eq!(
            visual("\"+y"),
            with_register(
                '+',
                None,
                Action::Operate(Operator::Yank, Target::Selection)
            )
        );
    }

    #[test]
    fn registers_come_before_commands_with_counts_either_side() {
        let lines = |operator| Action::Operate(operator, Target::Lines);
        assert_eq!(read("\""), Parsed::Incomplete);
        assert_eq!(read("\"a"), Parsed::Incomplete);
        assert_eq!(read("\"!"), Parsed::Nothing);
        assert_eq!(
            read("\"ayy"),
            with_register('a', None, lines(Operator::Yank))
        );
        assert_eq!(
            read("2\"A3dd"),
            with_register('A', Some(6), lines(Operator::Delete))
        );
        assert_eq!(
            read("\"_x"),
            with_register('_', None, Action::DeleteChars { before: false })
        );
        assert_eq!(
            read("\"+2P"),
            with_register('+', Some(2), Action::Put { before: true })
        );
    }

    #[test]
    fn macros_and_marks() {
        assert_eq!(read("q"), Parsed::Incomplete);
        assert_eq!(read("qa"), command(None, Action::Record('a')));
        assert_eq!(read("q-"), Parsed::Nothing);
        assert_eq!(read("3@a"), command(Some(3), Action::Replay('a')));
        assert_eq!(read("@@"), command(None, Action::Replay('@')));
        assert_eq!(read("@_"), Parsed::Nothing);
        assert_eq!(read("mA"), command(None, Action::SetMark('A')));
        assert_eq!(read("m."), Parsed::Nothing);
        let to = |mark, exact| Action::Mark { mark, exact };
        assert_eq!(read("'a"), command(None, to('a', false)));
        assert_eq!(read("`."), command(None, to('.', true)));
        assert_eq!(read("''"), command(None, to('\'', false)));
        assert_eq!(read("'"), Parsed::Incomplete);
        let target = Target::Mark {
            mark: 'b',
            exact: true,
        };
        assert_eq!(
            read("\"ad`b"),
            with_register('a', None, Action::Operate(Operator::Delete, target))
        );
        assert_eq!(read("y'"), Parsed::Incomplete);
    }

    #[test]
    fn strokes_are_the_keys_that_type_them_as_vim_writes_them() {
        let strokes = [
            Stroke::char('i'),
            Stroke::char('<'),
            Stroke::char(' '),
            Stroke::Enter,
            Stroke::ctrl('w'),
            Stroke::ctrl(' '),
            Stroke::Left,
            Stroke::Escape,
        ];
        let chords: Vec<Chord> = strokes.iter().map(|stroke| stroke.chord()).collect();
        let written = crate::keymap::write_macro(&chords);
        assert_eq!(written, "i<lt> <CR><C-w><C-Space><Left><Esc>");
        assert_eq!(Stroke::read(&written), strokes);
    }

    #[test]
    fn pieces_of_code_are_objects_and_brackets_go_to_them() {
        let function = Target::Object {
            object: Object::Code(Construct::Function),
            inner: false,
        };
        assert_eq!(
            read("daf"),
            command(None, Action::Operate(Operator::Delete, function))
        );
        let argument = Action::Select {
            object: Object::Code(Construct::Argument),
            inner: true,
        };
        assert_eq!(parse(&keys("ia"), true), command(None, argument));
        let piece = |construct, backward| Action::Piece {
            construct,
            backward,
        };
        assert_eq!(
            read("2]f"),
            command(Some(2), piece(Construct::Function, false))
        );
        assert_eq!(read("[C"), command(None, piece(Construct::Class, true)));
        assert_eq!(read("]c"), command(None, Action::Hunk { backward: false }));
        assert!(piece(Construct::Function, true).jumps());
    }

    #[test]
    fn case_operators_tilde_and_ampersand() {
        let recase = |recase, target| Action::Operate(Operator::Recase(recase), target);
        let lines = |recase| Action::Operate(Operator::Recase(recase), Target::Lines);
        assert_eq!(read("g"), Parsed::Incomplete);
        assert_eq!(read("gU"), Parsed::Incomplete);
        assert_eq!(read("gUg"), Parsed::Incomplete);
        assert_eq!(
            read("gUiw"),
            command(
                None,
                recase(
                    Recase::Upper,
                    Target::Object {
                        object: Object::Word,
                        inner: true
                    }
                )
            )
        );
        assert_eq!(read("g~~"), command(None, lines(Recase::Toggle)));
        assert_eq!(read("3guu"), command(Some(3), lines(Recase::Lower)));
        assert_eq!(read("gUgU"), command(None, lines(Recase::Upper)));
        assert_eq!(read("g?g?"), command(None, lines(Recase::Rot13)));
        assert_eq!(
            read("gUgg"),
            command(None, recase(Recase::Upper, Target::Motion(Motion::First)))
        );
        assert_eq!(read("3~"), command(Some(3), Action::Tilde));
        assert!(Action::Tilde.changes() && lines(Recase::Lower).changes());
        assert_eq!(
            read("&"),
            command(None, Action::RepeatSubstitute { all: false })
        );
        assert_eq!(
            read("g&"),
            command(None, Action::RepeatSubstitute { all: true })
        );
        assert!(!Action::RepeatSubstitute { all: false }.changes());
        // In visual mode, `~`, `u` and `U` act on the selection.
        let visual = |text: &str| parse(&keys(text), true);
        assert_eq!(
            visual("u"),
            command(None, recase(Recase::Lower, Target::Selection))
        );
        assert_eq!(
            visual("U"),
            command(None, recase(Recase::Upper, Target::Selection))
        );
        assert_eq!(
            visual("~"),
            command(None, recase(Recase::Toggle, Target::Selection))
        );
        assert_eq!(
            visual("g?"),
            command(None, recase(Recase::Rot13, Target::Selection))
        );
        // With `tildeop`, `~` takes a motion.
        assert_eq!(
            parse_with(&keys("~w"), false, true),
            command(
                None,
                recase(Recase::Toggle, Target::Motion(Motion::WordStart))
            )
        );
        assert_eq!(
            parse_with(&keys("~~"), false, true),
            command(None, lines(Recase::Toggle))
        );
    }
}
