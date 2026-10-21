//! Reading keys into a command, as Vim reads one, the same in every pane:
//! each pane's [`Dialect`] adds its own keys, and says which commands it
//! takes.
//!
//! A command is a count, then an operator and what it acts on (`d2w`,
//! `ci"`, `yy`), or a motion (`3j`, `f(`), or a key of its own (`x`, `p`,
//! `o`). A register can come before it, after `"`, with a count either
//! side (`"a3yy`, `2"bp`). The keys are read afresh as each arrives, until
//! they make a command or can't. A command the pane doesn't take is no
//! command, all its keys spent, as Vim spends them in a buffer it won't
//! change.

use std::fmt::Debug;

use super::Stroke;
use super::command::{
    Action, Command, Entry, Operator, Recase, Scroll, Target, ViewAt, is_mark, is_register,
    is_settable_mark,
};
use crate::motion::{Motion, count_with, multiplied};
use crate::objects::Object;

/// What a pane takes of the grammar, and its own keys. Each of its own
/// keys is asked about before the grammar reads it, so a pane can take a
/// key Vim uses; each hook answers `None` for a key that isn't its own,
/// and may read more keys from `keys`, as `gri` does.
pub(crate) trait Dialect {
    /// The commands of the pane's own.
    type Own: Copy + Debug + Eq;

    /// Whether `~` is an operator, as Vim's `tildeop` makes it.
    fn tildeop(&self) -> bool {
        false
    }

    /// The command of the pane's own that `stroke` starts, after any count
    /// and register, in `visual` mode or not.
    fn own(
        &self,
        _stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<Self::Own>> {
        None
    }

    /// The same, for the key after `g`.
    fn after_g(
        &self,
        _stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<Self::Own>> {
        None
    }

    /// The same, for the key after `]`, or `[` if `backward`: the grammar
    /// has no such keys of its own.
    fn after_bracket(
        &self,
        _backward: bool,
        _stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<Self::Own>> {
        None
    }

    /// Whether the pane takes `command`, read whole: one it doesn't is no
    /// command, and its keys are spent.
    fn takes(&self, _command: &Command<Self::Own>) -> bool {
        true
    }
}

/// What the keys typed so far make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Parsed<O> {
    /// More keys are needed.
    Incomplete,
    /// They make nothing: start again.
    Nothing,
    Command(Command<O>),
}

/// What keys read so far make of a part of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Read<T> {
    Yes(T),
    /// It needs another key.
    More,
    /// The keys aren't one.
    No,
}

impl<T> Read<T> {
    fn map<U>(self, make: impl FnOnce(T) -> U) -> Read<U> {
        match self {
            Self::Yes(read) => Read::Yes(make(read)),
            Self::More => Read::More,
            Self::No => Read::No,
        }
    }
}

/// Reads `keys` as a command in normal mode, or in `visual` mode, where
/// operators act on the selection and `i` and `a` start an object, as
/// `dialect` reads them.
pub(crate) fn parse<D: Dialect>(keys: &[Stroke], visual: bool, dialect: &D) -> Parsed<D::Own> {
    let mut reader = Reader {
        keys,
        at: 0,
        register: None,
        tildeop: dialect.tildeop(),
    };
    match reader.command(visual, dialect) {
        Parsed::Command(command) if !dialect.takes(&command) => Parsed::Nothing,
        parsed => parsed,
    }
}

/// The keys of a command typed so far in normal or visual mode.
#[derive(Debug, Default)]
pub(crate) struct Keys(Vec<Stroke>);

impl Keys {
    /// Takes `stroke`, and reads what the keys make, as `dialect` reads
    /// them: once they make a command, or nothing, they're forgotten.
    pub(crate) fn read<D: Dialect>(
        &mut self,
        stroke: Stroke,
        visual: bool,
        dialect: &D,
    ) -> Parsed<D::Own> {
        self.0.push(stroke);
        let parsed = parse(&self.0, visual, dialect);
        if parsed != Parsed::Incomplete {
            self.0.clear();
        }
        parsed
    }

    /// Whether a command is half typed, such as `d` waiting for its
    /// motion, which the next key is for.
    pub(crate) fn pending(&self) -> bool {
        !self.0.is_empty()
    }

    /// The count typed, if that's all of a command that's been typed.
    pub(crate) fn counting(&self) -> Option<usize> {
        let mut reader = Reader {
            keys: &self.0,
            at: 0,
            register: None,
            tildeop: false,
        };
        let count = reader.count();
        count.filter(|_| reader.at == self.0.len())
    }

    /// The keys typed so far, as `g` waiting for its second.
    pub(crate) fn so_far(&self) -> &[Stroke] {
        &self.0
    }

    /// The keys typed so far, as the bar shows them while a command is half
    /// typed, as Vim's `showcmd` does: `y3`, `"a`, `^W`.
    pub(crate) fn shown(&self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.iter().map(|stroke| stroke.shown()).collect())
    }

    /// Keeps `stroke` as typed, unread: for a view that reads its keys its
    /// own way, as the editor's rendered Markdown does.
    pub(crate) fn push(&mut self, stroke: Stroke) {
        self.0.push(stroke);
    }

    /// The keys typed so far, forgotten here.
    pub(crate) fn take(&mut self) -> Vec<Stroke> {
        std::mem::take(&mut self.0)
    }

    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
}

/// Where the keys are read up to, and what they've named so far.
pub(crate) struct Reader<'a> {
    keys: &'a [Stroke],
    at: usize,
    /// The register named before the command, if one was.
    register: Option<char>,
    /// `~` is an operator.
    tildeop: bool,
}

impl Reader<'_> {
    /// The next key, read.
    pub(crate) fn next(&mut self) -> Option<Stroke> {
        let stroke = self.keys.get(self.at).copied();
        self.at += 1;
        stroke
    }

    /// The next key, not read yet.
    fn peek(&self) -> Option<Stroke> {
        self.keys.get(self.at).copied()
    }

    /// The plain key after this one, if it's one `wanted` accepts, made
    /// into what `make` makes of it.
    pub(crate) fn then<T>(
        &mut self,
        wanted: impl Fn(char) -> bool,
        make: impl FnOnce(char) -> T,
    ) -> Read<T> {
        match self.next() {
            None => Read::More,
            Some(next) => match next.plain().filter(|&ch| wanted(ch)) {
                Some(ch) => Read::Yes(make(ch)),
                None => Read::No,
            },
        }
    }

    /// What the keys make, read from the start.
    fn command<D: Dialect>(&mut self, visual: bool, dialect: &D) -> Parsed<D::Own> {
        let first = self.count();
        // A register, and a count after it, which multiplies the first.
        let first = if self.peek() == Some(Stroke::char('"')) {
            self.at += 1;
            match self.next().map(Stroke::plain) {
                None => return Parsed::Incomplete,
                Some(Some(name)) if is_register(name) => self.register = Some(name),
                Some(_) => return Parsed::Nothing,
            }
            multiplied(first, self.count())
        } else {
            first
        };
        let Some(stroke) = self.next() else {
            return Parsed::Incomplete;
        };
        if let Some(own) = dialect.own(stroke, self, visual) {
            return self.made(first, own.map(Action::Own));
        }
        if stroke == Stroke::char('g') {
            return self.after_g(first, visual, dialect);
        }
        if let Some(motion) = self.motion(stroke) {
            return self.made(first, motion.map(Action::Move));
        }
        let action = match stroke {
            Stroke::Char {
                ch: 'd',
                ctrl: true,
            } => Action::Scroll(Scroll::HalfPage { down: true }),
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => Action::Scroll(Scroll::HalfPage { down: false }),
            Stroke::Char {
                ch: 'e',
                ctrl: true,
            } => Action::Scroll(Scroll::Line { down: true }),
            Stroke::Char {
                ch: 'y',
                ctrl: true,
            } => Action::Scroll(Scroll::Line { down: false }),
            Stroke::Char {
                ch: 'f',
                ctrl: true,
            }
            | Stroke::PageDown => Action::Scroll(Scroll::Page { down: true }),
            Stroke::PageUp => Action::Scroll(Scroll::Page { down: false }),
            Stroke::Char {
                ch: 'r',
                ctrl: true,
            } => Action::Redo,
            Stroke::Char {
                ch: 'v',
                ctrl: true,
            } => Action::BlockVisual,
            // Del deletes what's selected, as `x` does.
            Stroke::Delete if visual => Action::Operate(Operator::Delete, Target::Selection),
            Stroke::Delete => Action::DeleteChars { before: false },
            Stroke::Char { ctrl: false, ch } => return self.key(ch, first, visual, dialect),
            _ => return Parsed::Nothing,
        };
        self.made(first, Read::Yes(action))
    }

    /// The command `read` makes, done `count` times.
    fn made<O>(&self, count: Option<usize>, read: Read<Action<O>>) -> Parsed<O> {
        match read {
            Read::Yes(action) => Parsed::Command(Command {
                count,
                action,
                register: self.register,
            }),
            Read::More => Parsed::Incomplete,
            Read::No => Parsed::Nothing,
        }
    }

    /// What `g` and the key after it make, with `first` typed before them.
    fn after_g<D: Dialect>(
        &mut self,
        first: Option<usize>,
        visual: bool,
        dialect: &D,
    ) -> Parsed<D::Own> {
        let Some(next) = self.next() else {
            return Parsed::Incomplete;
        };
        if let Some(own) = dialect.after_g(next, self, visual) {
            return self.made(first, own.map(Action::Own));
        }
        // `g~`, `gu`, `gU` and `g?`, which change case.
        let recase = match next.plain() {
            Some('~') => Some(Recase::Toggle),
            Some('u') => Some(Recase::Lower),
            Some('U') => Some(Recase::Upper),
            Some('?') => Some(Recase::Rot13),
            _ => None,
        };
        if let (Some(recase), Some(key)) = (recase, next.plain()) {
            let operator = Operator::Recase(recase);
            if visual {
                return self.made(
                    first,
                    Read::Yes(Action::Operate(operator, Target::Selection)),
                );
            }
            return self.target(first, operator, key);
        }
        if next.plain() == Some('v') {
            return self.made(first, Read::Yes(Action::Reselect));
        }
        self.made(first, after_g_motion(next).map(Action::Move))
    }

    /// The action of the plain key `ch`, which may read keys after it, or
    /// what the keys make if not an action: an operator reads its target.
    fn key<D: Dialect>(
        &mut self,
        ch: char,
        first: Option<usize>,
        visual: bool,
        dialect: &D,
    ) -> Parsed<D::Own> {
        let action = match ch {
            'd' | 'c' | 'y' | '>' | '<' | '=' => {
                let operator = match ch {
                    'd' => Operator::Delete,
                    'c' => Operator::Change,
                    'y' => Operator::Yank,
                    '>' => Operator::Indent,
                    '=' => Operator::Reindent,
                    _ => Operator::Dedent,
                };
                if !visual {
                    return self.target(first, operator, ch);
                }
                Action::Operate(operator, Target::Selection)
            }
            'x' if visual => Action::Operate(Operator::Delete, Target::Selection),
            '~' if visual => Action::Operate(Operator::Recase(Recase::Toggle), Target::Selection),
            'u' if visual => Action::Operate(Operator::Recase(Recase::Lower), Target::Selection),
            'U' if visual => Action::Operate(Operator::Recase(Recase::Upper), Target::Selection),
            '~' if self.tildeop => {
                return self.target(first, Operator::Recase(Recase::Toggle), '~');
            }
            '~' => Action::Tilde,
            's' if visual => Action::Operate(Operator::Change, Target::Selection),
            'i' | 'a' if visual => {
                let select = self.object().map(|object| Action::Select {
                    object,
                    inner: ch == 'i',
                });
                return self.made(first, select);
            }
            'o' if visual => Action::OtherEnd,
            'O' if visual => Action::OtherCorner,
            'm' => {
                let mark = self.then(is_settable_mark, Action::SetMark);
                return self.made(first, mark);
            }
            '\'' | '`' => {
                let mark = self.then(is_mark, |mark| Action::Mark {
                    mark,
                    exact: ch == '`',
                });
                return self.made(first, mark);
            }
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
            'r' => {
                let replace = self.then(|_| true, Action::Replace);
                return self.made(first, replace);
            }
            'i' => Action::Insert(Entry::Before),
            'a' => Action::Insert(Entry::After),
            'I' => Action::Insert(Entry::LineStart),
            'A' => Action::Insert(Entry::LineEnd),
            'o' => Action::Insert(Entry::Below),
            'O' => Action::Insert(Entry::Above),
            'u' => Action::Undo,
            '.' => Action::Repeat,
            'v' => Action::Visual { lines: false },
            'V' => Action::Visual { lines: true },
            '/' => Action::Search { backward: false },
            '?' => Action::Search { backward: true },
            'n' => Action::SearchAgain { reverse: false },
            'N' => Action::SearchAgain { reverse: true },
            '*' => Action::SearchWord { backward: false },
            '#' => Action::SearchWord { backward: true },
            ';' => Action::FindAgain { reverse: false },
            ',' => Action::FindAgain { reverse: true },
            'z' => {
                let at = |key| match key {
                    't' => ViewAt::Top,
                    'z' => ViewAt::Middle,
                    _ => ViewAt::Bottom,
                };
                let view = self.then(|key| matches!(key, 't' | 'z' | 'b'), at);
                return self.made(first, view.map(Action::View));
            }
            ']' | '[' => {
                let Some(next) = self.next() else {
                    return Parsed::Incomplete;
                };
                return match dialect.after_bracket(ch == '[', next, self, visual) {
                    Some(own) => self.made(first, own.map(Action::Own)),
                    None => Parsed::Nothing,
                };
            }
            _ => return Parsed::Nothing,
        };
        self.made(first, Read::Yes(action))
    }

    /// The object the next key names, after `i` or `a`.
    fn object(&mut self) -> Read<Object> {
        match self.next() {
            None => Read::More,
            Some(next) => next
                .plain()
                .and_then(Object::of_key)
                .map_or(Read::No, Read::Yes),
        }
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
            count = Some(count_with(count, digit));
            self.at += 1;
        }
        count
    }

    /// The motion `stroke` starts, if it starts one.
    fn motion(&mut self, stroke: Stroke) -> Option<Read<Motion>> {
        let motion = match stroke {
            Stroke::Left | Stroke::Backspace => Motion::Left,
            Stroke::Right => Motion::Right,
            Stroke::Up => Motion::Up(1),
            Stroke::Down | Stroke::Enter => Motion::Down(1),
            Stroke::Home => Motion::LineStart,
            Stroke::End => Motion::LineEnd,
            Stroke::WordBack => Motion::WordBack,
            Stroke::WordForward => Motion::WordStart,
            Stroke::TextStart => Motion::First,
            Stroke::TextEnd => Motion::Last,
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
                'H' => Motion::ViewTop,
                'M' => Motion::ViewMiddle,
                'L' => Motion::ViewBottom,
                '%' => Motion::Match,
                '}' => Motion::Paragraph { forward: true },
                '{' => Motion::Paragraph { forward: false },
                'g' => {
                    return Some(match self.next() {
                        None => Read::More,
                        Some(next) => after_g_motion(next),
                    });
                }
                'f' | 't' | 'F' | 'T' => {
                    return Some(self.then(
                        |_| true,
                        |target| Motion::Find {
                            ch: target,
                            forward: ch.is_lowercase(),
                            till: ch.eq_ignore_ascii_case(&'t'),
                        },
                    ));
                }
                _ => return None,
            },
            _ => return None,
        };
        Some(Read::Yes(motion))
    }

    /// What an operator typed as `key` acts on: a motion or an object, or
    /// `key` again for lines.
    fn target<O>(&mut self, first: Option<usize>, operator: Operator, key: char) -> Parsed<O> {
        let count = multiplied(first, self.count());
        let operate = |target| Read::Yes(Action::Operate(operator, target));
        let Some(stroke) = self.next() else {
            return Parsed::Incomplete;
        };
        if stroke == Stroke::char(key) {
            return self.made(count, operate(Target::Lines));
        }
        // `g~g~` as `g~~`, and their kin.
        if matches!(operator, Operator::Recase(_))
            && stroke == Stroke::char('g')
            && self.peek() == Some(Stroke::char(key))
        {
            self.at += 1;
            return self.made(count, operate(Target::Lines));
        }
        if let Some(kind @ ('\'' | '`')) = stroke.plain() {
            let mark = self.then(is_mark, |mark| Target::Mark {
                mark,
                exact: kind == '`',
            });
            return self.made(count, mark.map(|mark| Action::Operate(operator, mark)));
        }
        if let Some(kind @ ('i' | 'a')) = stroke.plain() {
            let target = self.object().map(|object| Target::Object {
                object,
                inner: kind == 'i',
            });
            return self.made(
                count,
                target.map(|target| Action::Operate(operator, target)),
            );
        }
        let target = match self.motion(stroke) {
            Some(motion) => motion.map(Target::Motion),
            None => Read::No,
        };
        self.made(
            count,
            target.map(|target| Action::Operate(operator, target)),
        )
    }
}

/// The motion `g` and `stroke` make: `gg`, and `gj`, `gk`, `g0` and `g$`
/// by screen rows, where lines wrap.
fn after_g_motion(stroke: Stroke) -> Read<Motion> {
    match stroke.plain() {
        Some('g') => Read::Yes(Motion::First),
        Some('j') => Read::Yes(Motion::RowDown(1)),
        Some('k') => Read::Yes(Motion::RowUp(1)),
        Some('0') => Read::Yes(Motion::RowStart),
        Some('$') => Read::Yes(Motion::RowEnd),
        _ => Read::No,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::Construct;

    use super::*;

    /// Vim's keys and no more, every command taken; `~` an operator if
    /// `tildeop`.
    struct Plain {
        tildeop: bool,
    }

    impl Dialect for Plain {
        type Own = ();

        fn tildeop(&self) -> bool {
            self.tildeop
        }
    }

    fn keys(text: &str) -> Vec<Stroke> {
        text.chars().map(Stroke::char).collect()
    }

    fn read(text: &str) -> Parsed<()> {
        parse(&keys(text), false, &Plain { tildeop: false })
    }

    fn visual(text: &str) -> Parsed<()> {
        parse(&keys(text), true, &Plain { tildeop: false })
    }

    fn command(count: Option<usize>, action: Action<()>) -> Parsed<()> {
        Parsed::Command(Command {
            count,
            action,
            register: None,
        })
    }

    fn with_register(register: char, count: Option<usize>, action: Action<()>) -> Parsed<()> {
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
        assert_eq!(read("10G"), command(Some(10), Action::Move(Motion::Last)));
        assert_eq!(read("10gg"), command(Some(10), Action::Move(Motion::First)));
        // A count too big is kept to the most there is.
        let most = Some(crate::motion::MOST_COUNT);
        assert_eq!(
            read("999999999999999999999999j"),
            command(most, Action::Move(Motion::Down(1)))
        );
        assert_eq!(
            read("99999999d99999999w"),
            command(
                most,
                Action::Operate(Operator::Delete, Target::Motion(Motion::WordStart))
            )
        );
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
        assert_eq!(read("zx"), Parsed::Nothing);
        assert_eq!(
            parse(&[Stroke::ctrl('r')], false, &Plain { tildeop: false }),
            command(None, Action::Redo)
        );
        assert_eq!(
            parse(&[Stroke::PageUp], false, &Plain { tildeop: false }),
            command(None, Action::Scroll(Scroll::Page { down: false }))
        );
        assert_eq!(
            parse(&[Stroke::ctrl('e')], false, &Plain { tildeop: false }),
            command(None, Action::Scroll(Scroll::Line { down: true }))
        );
        assert_eq!(
            parse(&[Stroke::ctrl('y')], false, &Plain { tildeop: false }),
            command(None, Action::Scroll(Scroll::Line { down: false }))
        );
        assert_eq!(read("Q"), Parsed::Nothing);
    }

    #[test]
    fn visual_mode_operates_on_the_selection() {
        assert_eq!(
            visual("d"),
            command(None, Action::Operate(Operator::Delete, Target::Selection))
        );
        assert_eq!(visual("o"), command(None, Action::OtherEnd));
        assert_eq!(
            parse(&[Stroke::Delete], true, &Plain { tildeop: false }),
            command(None, Action::Operate(Operator::Delete, Target::Selection))
        );
        // `gv` in either mode, but not as what an operator acts on.
        assert_eq!(visual("gv"), command(None, Action::Reselect));
        assert_eq!(read("gv"), command(None, Action::Reselect));
        assert_eq!(read("dgv"), Parsed::Nothing);
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
    fn marks() {
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
    fn pieces_of_code_are_objects() {
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
        assert_eq!(visual("ia"), command(None, argument));
    }

    #[test]
    fn case_operators_and_tilde() {
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
        // In visual mode, `~`, `u` and `U` act on the selection.
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
        let tildeop = |text: &str| parse(&keys(text), false, &Plain { tildeop: true });
        assert_eq!(
            tildeop("~w"),
            command(
                None,
                recase(Recase::Toggle, Target::Motion(Motion::WordStart))
            )
        );
        assert_eq!(tildeop("~~"), command(None, lines(Recase::Toggle)));
    }

    /// A pane with keys of its own, that takes no operator but `y`.
    struct Picky;

    impl Dialect for Picky {
        type Own = char;

        fn own(&self, stroke: Stroke, _keys: &mut Reader<'_>, _visual: bool) -> Option<Read<char>> {
            // Its own `w`, before Vim's.
            (stroke == Stroke::char('w')).then_some(Read::Yes('w'))
        }

        fn after_g(
            &self,
            stroke: Stroke,
            keys: &mut Reader<'_>,
            _visual: bool,
        ) -> Option<Read<char>> {
            // `gx`, and `gzz`, which reads on.
            match stroke.plain()? {
                'x' => Some(Read::Yes('x')),
                'z' => Some(keys.then(|key| key == 'z', |key| key)),
                _ => None,
            }
        }

        fn after_bracket(
            &self,
            backward: bool,
            stroke: Stroke,
            _keys: &mut Reader<'_>,
            _visual: bool,
        ) -> Option<Read<char>> {
            let again = if backward { '[' } else { ']' };
            (stroke.plain()? == again).then_some(Read::Yes(again))
        }

        fn takes(&self, command: &Command<char>) -> bool {
            !matches!(command.action, Action::Operate(operator, _) if operator != Operator::Yank)
        }
    }

    #[test]
    fn a_pane_adds_keys_of_its_own_and_refuses_whole_commands() {
        let read = |text: &str| parse(&keys(text), false, &Picky);
        let made = |count, action| {
            Parsed::Command(Command {
                count,
                action,
                register: None,
            })
        };
        assert_eq!(read("2w"), made(Some(2), Action::Own('w')));
        assert_eq!(read("gx"), made(None, Action::Own('x')));
        assert_eq!(read("gz"), Parsed::Incomplete);
        assert_eq!(read("gzz"), made(None, Action::Own('z')));
        assert_eq!(read("gzx"), Parsed::Nothing);
        // The grammar's own `g` keys are still there.
        assert_eq!(read("gg"), made(None, Action::Move(Motion::First)));
        assert_eq!(read("]]"), made(None, Action::Own(']')));
        assert_eq!(read("[x"), Parsed::Nothing);
        // `d` waits for what it acts on, and then the whole command is
        // refused, its keys spent.
        assert_eq!(read("d"), Parsed::Incomplete);
        assert_eq!(read("dj"), Parsed::Nothing);
        let yank = Action::Operate(Operator::Yank, Target::Motion(Motion::Down(1)));
        assert_eq!(read("yj"), made(None, yank));
    }

    #[test]
    fn keys_are_forgotten_once_they_make_a_command_or_nothing() {
        let mut keys = Keys::default();
        let plain = Plain { tildeop: false };
        let read = |keys: &mut Keys, ch| keys.read(Stroke::char(ch), false, &plain);
        assert_eq!(read(&mut keys, '2'), Parsed::Incomplete);
        assert_eq!(keys.counting(), Some(2));
        assert_eq!(read(&mut keys, '0'), Parsed::Incomplete);
        assert_eq!(keys.counting(), Some(20));
        assert_eq!(read(&mut keys, 'd'), Parsed::Incomplete);
        assert!(keys.pending());
        assert_eq!(keys.counting(), None, "more than a count");
        assert_eq!(
            keys.so_far(),
            [Stroke::char('2'), Stroke::char('0'), Stroke::char('d')]
        );
        assert_eq!(keys.shown().as_deref(), Some("20d"));
        assert!(matches!(read(&mut keys, 'j'), Parsed::Command(_)));
        assert!(!keys.pending());
        assert_eq!(keys.shown(), None);
        let ctrl_w = keys.read(Stroke::ctrl('w'), false, &plain);
        assert_eq!(ctrl_w, Parsed::Nothing);
        keys.read(Stroke::char('"'), false, &plain);
        assert_eq!(keys.shown().as_deref(), Some("\""));
        keys.clear();
        read(&mut keys, 'd');
        assert_eq!(read(&mut keys, 'z'), Parsed::Nothing);
        assert!(!keys.pending());
    }
}
