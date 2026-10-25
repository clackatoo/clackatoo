//! Executable key matches and their help are declared together. The macro
//! emits an ordinary match for dispatch and a read-only match for discovery;
//! discovery never evaluates action bodies. Guards must be read-only.

macro_rules! described_keys {
    (
        $vis:vis fn $run:ident / $describe:ident
        (&mut $this:ident $(, $arg:ident: $ty:ty)* $(,)?) -> $out:ty;
        $(execute($($extra:ident: $extra_ty:ty),*);)?
        $(prepare { $($setup:tt)* })?
        match $input:expr => {
            $($pattern:pat $(if $guard:expr)? => $help:expr, $body:expr;)*
        }
        result
    ) => {
        $vis fn $run(&mut $this, $($arg: $ty,)* $($($extra: $extra_ty),*)?) -> $out {
            $($($setup)*)?
            match $input {
                $($pattern $(if $guard)? => $body,)*
            }

        }

        #[allow(unused_variables, reason = "bindings capture operands used only by execution")]
        $vis fn $describe(&$this, $($arg: $ty),*) -> $crate::shortcuts::bindings::Hint {
            $($($setup)*)?
            match $input {
                $($pattern $(if $guard)? => $help,)*
            }
        }
    };
    (
        $vis:vis fn $run:ident / $describe:ident
        (&mut $this:ident $(, $arg:ident: $ty:ty)* $(,)?) -> $out:ty;
        $(execute($($extra:ident: $extra_ty:ty),*);)?
        $(prepare { $($setup:tt)* })?
        match $input:expr => {
            $($pattern:pat $(if $guard:expr)? => $help:expr, $body:expr;)*
        }
        then $tail:expr
    ) => {
        $vis fn $run(&mut $this, $($arg: $ty,)* $($($extra: $extra_ty),*)?) -> $out {
            $($($setup)*)?
            match $input {
                $($pattern $(if $guard)? => $body,)*
            }
            $tail
        }

        #[allow(unused_variables, reason = "bindings capture operands used only by execution")]
        $vis fn $describe(&$this, $($arg: $ty),*) -> $crate::shortcuts::bindings::Hint {
            $($($setup)*)?
            match $input {
                $($pattern $(if $guard)? => $help,)*
            }
        }
    };
}
pub(crate) use described_keys;

use super::Context;
use crate::vim::Stroke;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hint {
    Action(&'static str),
    Prefix,
    Count,
    Text(&'static str),
    None,
}

/// The input alphabet, not a binding catalog. Which keys are accepted and
/// what they mean comes entirely from each executable match.
pub(crate) fn strokes() -> impl Iterator<Item = Stroke> {
    use Stroke::{
        BackTab, Backspace, CtrlEnd, CtrlHome, CtrlLeft, CtrlRight, Delete, Down, End, Enter,
        Escape, Home, Left, PageDown, PageUp, Right, ShiftDown, ShiftLeft, ShiftRight, ShiftUp,
        Tab, Up,
    };
    (' '..='~')
        .map(Stroke::char)
        .chain(('a'..='z').map(Stroke::ctrl))
        .chain([
            Escape, Enter, Tab, BackTab, Backspace, Delete, Left, Right, Up, Down, Home, End,
            PageUp, PageDown, ShiftLeft, ShiftRight, CtrlLeft, CtrlRight, CtrlHome, CtrlEnd,
            ShiftUp, ShiftDown,
        ])
}

pub(crate) fn label(stroke: Stroke) -> String {
    super::sequence(&[stroke.chord()])
}

/// Enumerate list commands through the same match which executes them. A
/// prefix is expanded only when the definition marks it as one.
pub(crate) fn list(
    title: &str,
    pending: Option<char>,
    describe: impl Fn(Stroke, Option<char>) -> Hint,
) -> Context {
    let mut context = Context::new(
        pending.map_or_else(|| title.to_owned(), |key| format!("{title} · after {key}")),
        &[],
    );
    for stroke in strokes() {
        match describe(stroke, pending) {
            Hint::Prefix if pending.is_none() => {
                for next in strokes().filter(|stroke| stroke.plain().is_some()) {
                    add(
                        &mut context,
                        format!("{}{}", label(stroke), label(next)),
                        describe(next, stroke.plain()),
                    );
                }
            }
            hint => add(&mut context, label(stroke), hint),
        }
    }
    context
}

pub(crate) fn add(context: &mut Context, key: String, hint: Hint) {
    let (key, description) = match hint {
        Hint::Action(description) => (key, description),
        Hint::Count => (
            "1–9, then digits".into(),
            "Repeat the following command that many times",
        ),
        Hint::Text(description) => ("Type".into(), description),
        Hint::None | Hint::Prefix => return,
    };
    if let Some((keys, _)) = context
        .rows
        .iter_mut()
        .find(|(_, desc)| desc == description)
    {
        if !keys.split(" / ").any(|old| old == key) {
            keys.push_str(" / ");
            keys.push_str(&key);
        }
    } else {
        context.rows.push((key, description.into()));
    }
}

pub(crate) fn presses() -> impl Iterator<Item = crate::keymap::Chord> {
    use crate::keymap::{Base, Chord, Named};
    let named = |key, ctrl, shift| Chord::with(Base::Named(key), ctrl, false, shift);
    strokes().map(Stroke::chord).chain([
        named(Named::Enter, false, true),
        named(Named::Enter, true, false),
        named(Named::Enter, true, true),
        named(Named::Backspace, true, false),
        named(Named::Delete, true, false),
        named(Named::Left, true, true),
        named(Named::Right, true, true),
        named(Named::Home, false, true),
        named(Named::End, false, true),
        named(Named::Escape, false, true),
        named(Named::Space, true, false),
        Chord::with(Base::Char('c'), true, false, true),
        Chord::with(Base::Char('v'), true, false, true),
        Chord::with(Base::Char(';'), true, false, false),
        Chord::with(Base::Char('\\'), true, false, false),
        Chord::with(Base::Char('?'), true, false, false),
    ])
}

pub(crate) fn from_presses(
    title: &str,
    describe: impl Fn(&crate::input::KeyPress<'_>) -> Hint,
) -> Context {
    let mut context = Context::new(title, &[]);
    for key in presses() {
        add(
            &mut context,
            super::sequence(&[key]),
            describe(&key.kept().press()),
        );
    }
    context
}

pub(crate) fn normal(context: &mut Context, description: &'static str) {
    context.rows.extend(
        from_presses("", |press| {
            if crate::vim::enters_normal_mode(press) {
                Hint::Action(description)
            } else {
                Hint::None
            }
        })
        .rows,
    );
}

pub(crate) fn paste(context: &mut Context, description: &'static str) {
    context.rows.extend(
        from_presses("", |press| {
            if crate::app::is_paste(press.key, press.modifiers) {
                Hint::Action(description)
            } else {
                Hint::None
            }
        })
        .rows,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Handler {
        enabled: bool,
        changes: usize,
    }

    impl Handler {
        described_keys! {
            fn key / describe(&mut self, stroke: Stroke) -> bool;
            match stroke => {
                Stroke::Char { ch: 'Q', ctrl: false } if self.enabled =>
                    Hint::Action("Run guarded action"),
                    { self.changes += 1; true };
                Stroke::Enter => Hint::Action("Accept"), { self.changes += 2; true };
                _ => Hint::None, false;
            }
            result
        }
    }

    #[test]
    fn shortcut_help_discovers_guarded_dispatch_without_executing_it() {
        let mut handler = Handler::default();
        let help = |handler: &Handler| list("Test", None, |stroke, _| handler.describe(stroke));
        assert!(!help(&handler).rows.iter().any(|(key, _)| key == "Q"));
        assert!(!handler.key(Stroke::char('Q')));
        handler.enabled = true;
        assert!(
            help(&handler)
                .rows
                .contains(&("Q".into(), "Run guarded action".into()))
        );
        assert_eq!(handler.changes, 0, "discovery must never run action bodies");
        assert!(handler.key(Stroke::char('Q')));
        assert_eq!(handler.changes, 1);
        assert!(handler.key(Stroke::Enter));
        assert_eq!(handler.changes, 3);
    }
}
