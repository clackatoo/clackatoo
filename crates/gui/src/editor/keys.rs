//! What keys mean in an editor's normal and visual modes: a command, as
//! Vim reads one, from the keys typed so far.
//!
//! A command is a count, then an operator and what it acts on (`d2w`,
//! `ci"`, `yy`), or a motion (`3j`, `f(`), or a key of its own (`x`, `p`,
//! `o`). The keys are read afresh as each arrives, until they make a
//! command or can't.

use clackatoo_protocol::Query;

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

/// A command, and how many times to do it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Command {
    pub(crate) count: Option<usize>,
    pub(crate) action: Action,
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
    /// `o` in visual mode: to the other end of the selection.
    OtherEnd,
    /// `i` or `a` and an object, in visual mode: select it.
    Select {
        object: Object,
        inner: bool,
    },
}

impl Action {
    /// Whether it changes the text, and so is a change for `.` to make
    /// again.
    pub(crate) fn changes(self) -> bool {
        matches!(
            self,
            Self::Operate(
                Operator::Delete | Operator::Change | Operator::Indent | Operator::Dedent,
                _
            ) | Self::DeleteChars { .. }
                | Self::DeleteToEnd { .. }
                | Self::Substitute
                | Self::Put { .. }
                | Self::Join
                | Self::Replace(_)
                | Self::Insert(_)
        )
    }

    /// Whether it's a jump, which the jump list records, as Vim's are:
    /// `gg`, `G`, `n` and `N`, and `]d` and `[d`. A search typed after `/`
    /// is too, once Enter finds it, as is going where a language server
    /// says, once it answers.
    pub(crate) fn jumps(self) -> bool {
        matches!(
            self,
            Self::Move(Motion::First | Motion::Last)
                | Self::GoToLine
                | Self::SearchAgain { .. }
                | Self::Diagnostic { .. }
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

/// Reads `keys` as a normal-mode command, or in `visual` mode, where
/// operators act on the selection and `i` and `a` start an object.
pub(crate) fn parse(keys: &[Stroke], visual: bool) -> Parsed {
    let mut reader = Reader { keys, at: 0 };
    let first = reader.count();
    let command = |count, action| Parsed::Command(Command { count, action });
    let Some(stroke) = reader.next() else {
        return Parsed::Incomplete;
    };
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

struct Reader<'a> {
    keys: &'a [Stroke],
    at: usize,
}

impl Reader<'_> {
    /// The action of the plain key `ch`, which may read keys after it, or
    /// what the keys make if not an action: an operator reads its target.
    fn key(&mut self, ch: char, first: Option<usize>, visual: bool) -> Result<Action, Parsed> {
        let command = |count, action| Parsed::Command(Command { count, action });
        Ok(match ch {
            'd' | 'c' | 'y' | '>' | '<' => {
                let operator = match ch {
                    'd' => Operator::Delete,
                    'c' => Operator::Change,
                    'y' => Operator::Yank,
                    '>' => Operator::Indent,
                    _ => Operator::Dedent,
                };
                if visual {
                    return Err(command(first, Action::Operate(operator, Target::Selection)));
                }
                return Err(self.target(first, operator, ch));
            }
            'x' if visual => Action::Operate(Operator::Delete, Target::Selection),
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
            bracket @ (']' | '[') => match self.next() {
                None => return Err(Parsed::Incomplete),
                Some(next) => match next.plain() {
                    Some('d') => Action::Diagnostic {
                        backward: bracket == '[',
                    },
                    _ => return Err(Parsed::Nothing),
                },
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
                'g' => {
                    // What a language server knows: `gd`, `gD`, `gy`, `gri`,
                    // `grr` and `grn`, as Neovim's are.
                    let ask = |query| Read::Yes(Action::Ask(query));
                    let Some(next) = self.next() else {
                        return Some(Read::More);
                    };
                    return Some(match next.plain() {
                        Some('g') => Read::Yes(Action::Move(Motion::First)),
                        Some('d') => ask(Query::Definition),
                        Some('D') => ask(Query::Declaration),
                        Some('y') => ask(Query::TypeDefinition),
                        Some('r') => match self.next() {
                            None => Read::More,
                            Some(last) => match last.plain() {
                                Some('i') => ask(Query::Implementation),
                                Some('r') => ask(Query::References),
                                Some('n') => Read::Yes(Action::Rename),
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
        let second = self.count();
        let count = match (first, second) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1).saturating_mul(b.unwrap_or(1))),
        };
        let command = |target| {
            Parsed::Command(Command {
                count,
                action: Action::Operate(operator, target),
            })
        };
        let Some(stroke) = self.next() else {
            return Parsed::Incomplete;
        };
        if stroke == Stroke::char(key) {
            return command(Target::Lines);
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
        Parsed::Command(Command { count, action })
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
    }
}
