//! Walk the parser's actual continuations. There is no parallel key catalog:
//! the parser supplies accepted commands and operand/prefix states; actions
//! supply descriptions. Opening help never executes or mutates the grammar.
use super::grammar::{Expectation, inspect};
use super::{Action, Dialect, Keys, Mode, Parsed, Stroke};
use crate::shortcuts::{
    Context,
    bindings::{label, strokes},
};

pub(crate) fn context<D: Dialect>(name: &str, keys: &Keys, mode: Mode, dialect: &D) -> Context {
    let visual = matches!(mode, Mode::Visual(_));
    let title = keys.shown().map_or_else(
        || format!("{name} · {} mode", if visual { "visual" } else { "normal" }),
        |shown| format!("{name} · after {shown}"),
    );
    let mut context = Context::new(
        title,
        &[("Esc / Ctrl+;", "Cancel the command or selection")],
    );
    let mut path = keys.so_far().to_vec();
    let base = path.len();
    discover(&mut context, &mut path, base, visual, dialect);
    if keys.so_far().is_empty() || keys.counting().is_some() {
        context.add(&[(
            "1–9, then digits",
            "Repeat the following command that many times",
        )]);
    }
    context
}

fn discover<D: Dialect>(
    context: &mut Context,
    path: &mut Vec<Stroke>,
    base: usize,
    visual: bool,
    dialect: &D,
) {
    let (_, expectation) = inspect(path, visual, dialect);
    let prefix = shown(&path[base..]);
    match expectation {
        Expectation::Register => {
            let mut sample = path.clone();
            sample.extend([Stroke::char('a'), Stroke::char('y'), Stroke::char('w')]);
            if matches!(inspect(&sample, visual, dialect).0, Parsed::Command(_)) {
                push(
                    context,
                    if prefix.is_empty() {
                        "Register name".into()
                    } else {
                        format!("{prefix} + register")
                    },
                    "Choose a register for the following command".into(),
                );
            }
            return;
        }
        Expectation::Target(operator) if path.len() > base => {
            // A complete representative target is still checked by takes(),
            // so read-only dialects advertise only their allowed operators.
            path.push(Stroke::char('w'));
            let accepted = matches!(inspect(path, visual, dialect).0, Parsed::Command(_));
            path.pop();
            if accepted {
                push(
                    context,
                    format!("{prefix} + motion or text object"),
                    format!(
                        "{} the target; repeat the operator for whole lines",
                        operator.description()
                    ),
                );
            }
            return;
        }
        Expectation::Operand(operand) => {
            // Validators in the parser decide which operands are valid. Their
            // values do not change the command's description, so one example
            // suffices, and arbitrary characters are never recursively expanded.
            for stroke in strokes() {
                path.push(stroke);
                let parsed = inspect(path, visual, dialect).0;
                path.pop();
                if let Parsed::Command(command) = parsed {
                    let key = if prefix.is_empty() {
                        format!("Any {operand}")
                    } else {
                        format!("{prefix} + {operand}")
                    };
                    push(
                        context,
                        key,
                        command.action.description(|own| dialect.describe(own)),
                    );
                    break;
                }
            }
            return;
        }
        Expectation::Key | Expectation::Target(_) => {}
    }
    for stroke in strokes().filter(|stroke| !matches!(stroke.plain(), Some('1'..='9'))) {
        // Counts are unbounded prefixes, described once rather than walked.
        if stroke == Stroke::char('0')
            && path
                .last()
                .is_some_and(|s| s.plain().is_some_and(|c| c.is_ascii_digit()))
        {
            continue;
        }
        path.push(stroke);
        match inspect(path, visual, dialect).0 {
            Parsed::Command(command) => {
                let description = match command.action {
                    // While entering an operator's object, describe the object
                    // itself; the pending operator is already in the title.
                    Action::Operate(_, super::Target::Object { object, inner }) if base > 0 => {
                        object.description(inner)
                    }
                    action => action.description(|own| dialect.describe(own)),
                };
                push(context, shown(&path[base..]), description);
            }
            Parsed::Incomplete => discover(context, path, base, visual, dialect),
            Parsed::Nothing => {}
        }
        path.pop();
    }
}

fn shown(strokes: &[Stroke]) -> String {
    if strokes.iter().all(|stroke| stroke.plain().is_some()) {
        strokes.iter().map(|stroke| label(*stroke)).collect()
    } else {
        crate::shortcuts::sequence(
            &strokes
                .iter()
                .map(|stroke| stroke.chord())
                .collect::<Vec<_>>(),
        )
    }
}

fn push(context: &mut Context, key: String, description: String) {
    if !context.rows.iter().any(|(old, _)| old == &key) {
        context.rows.push((key, description));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::{Command, Read, Reader};

    struct ExtraKeys {
        enabled: bool,
    }

    impl Dialect for ExtraKeys {
        type Own = char;

        fn after_g(
            &self,
            stroke: Stroke,
            keys: &mut Reader<'_>,
            _visual: bool,
        ) -> Option<Read<char>> {
            if stroke != Stroke::char('Z') {
                return None;
            }
            Some(match keys.next() {
                Some(Stroke::Char {
                    ch: '!',
                    ctrl: false,
                }) => Read::Yes('!'),
                Some(_) => Read::No,
                None => Read::More,
            })
        }

        fn takes(&self, command: &Command<char>) -> bool {
            self.enabled && command.action == Action::Own('!')
        }

        fn describe(&self, _action: char) -> String {
            "New dialect command".into()
        }
    }

    #[test]
    fn shortcut_help_discovers_new_dialect_sequences_and_applies_its_filter() {
        let mut keys = Keys::default();
        let enabled = ExtraKeys { enabled: true };
        let rows = context("Test", &keys, Mode::Normal, &enabled).rows;
        assert!(rows.contains(&("gZ!".into(), "New dialect command".into())));
        assert!(!rows.iter().any(|(key, _)| key == "j"));
        assert!(
            !context("Test", &keys, Mode::Normal, &ExtraKeys { enabled: false })
                .rows
                .iter()
                .any(|(key, _)| key == "gZ!")
        );
        keys.push(Stroke::char('g'));
        keys.push(Stroke::char('Z'));
        assert!(
            context("Test", &keys, Mode::Normal, &enabled)
                .rows
                .contains(&("!".into(), "New dialect command".into()))
        );
        assert_eq!(keys.so_far(), &[Stroke::char('g'), Stroke::char('Z')]);
    }
}
