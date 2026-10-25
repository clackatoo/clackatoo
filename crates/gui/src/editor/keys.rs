//! What keys mean in an editor's normal and visual modes: every command
//! the grammar reads (see [`crate::vim`]), and the editor's own, which ask
//! a language server, go to what the grammar or git found, or record and
//! play macros.

use clackatoo_protocol::{Construct, Query};

use crate::motion::Motion;
use crate::vim::{self, Dialect, Operator, Read, Reader, Stroke, Target, is_register};

/// A command the editor takes.
pub(crate) type Command = vim::Command<EditorAction>;

/// What a command the editor takes does.
pub(crate) type Action = vim::Action<EditorAction>;

/// What keys typed in an editor make.
pub(crate) type Parsed = vim::Parsed<EditorAction>;

/// A command of the editor's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorAction {
    /// `]d`, or `[d` if `backward`: to the next diagnostic.
    Diagnostic { backward: bool },
    /// `]c`, or `[c` if `backward`: to the next hunk, a change since the
    /// file's text in the index.
    Hunk { backward: bool },
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
    /// `q` and a register: record the keys typed into it, until `q`.
    Record(char),
    /// `@` and a register: type the keys it holds, as if typed again.
    /// `@@` types the last register typed so again.
    Replay(char),
    /// `&`: the last `:s` again on the cursor's line, without its flags;
    /// or `g&`, on every line, with them, and the last search's pattern.
    RepeatSubstitute { all: bool },
    /// Esc after a count or a register: what Esc alone does, as Vim's
    /// does, the count and register forgotten.
    Escape,
}

/// How an editor reads keys: every command, and its own; `~` is an
/// operator if `tildeop`.
pub(crate) struct EditorKeys {
    pub(crate) tildeop: bool,
}

impl Dialect for EditorKeys {
    type Own = EditorAction;

    fn describe(&self, action: EditorAction) -> String {
        let text = match action {
            EditorAction::Diagnostic { backward: false } => "Next diagnostic",
            EditorAction::Diagnostic { backward: true } => "Previous diagnostic",
            EditorAction::Hunk { backward: false } => "Next change hunk",
            EditorAction::Hunk { backward: true } => "Previous change hunk",
            EditorAction::Piece {
                construct,
                backward,
            } => {
                return format!(
                    "{} {}",
                    if backward { "Previous" } else { "Next" },
                    match construct {
                        Construct::Function => "function",
                        Construct::Class => "class",
                        Construct::Argument => "argument",
                    }
                );
            }
            EditorAction::Ask(query) => match query {
                Query::Definition => "Go to definition",
                Query::Declaration => "Go to declaration",
                Query::TypeDefinition => "Go to type definition",
                Query::Implementation => "Find implementations",
                Query::References => "Find references",
                _ => "Ask language server",
            },
            EditorAction::Hover => "Show hover information",
            EditorAction::ShowDiagnostics => "Show diagnostics at cursor",
            EditorAction::Rename => "Rename symbol",
            EditorAction::CodeActions => "Show code actions",
            EditorAction::FormatSelection => "Format selection",
            EditorAction::Record(_) => "Record macro in register",
            EditorAction::Replay(_) => "Replay register",
            EditorAction::RepeatSubstitute { all: false } => "Repeat substitution on line",
            EditorAction::RepeatSubstitute { all: true } => "Repeat substitution throughout file",
            EditorAction::Escape => "Cancel command or selection",
        };
        text.into()
    }

    fn tildeop(&self) -> bool {
        self.tildeop
    }

    fn own(
        &self,
        stroke: Stroke,
        keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<EditorAction>> {
        Some(match stroke {
            Stroke::Escape => Read::Yes(EditorAction::Escape),
            // Ctrl+W `d`, or Ctrl+W Ctrl+D, as Neovim's.
            Stroke::Char {
                ch: 'w',
                ctrl: true,
            } => match keys.next() {
                None => Read::More,
                Some(Stroke::Char { ch: 'd', .. }) => Read::Yes(EditorAction::ShowDiagnostics),
                Some(_) => Read::No,
            },
            Stroke::Char { ch, ctrl: false } => match ch {
                'K' => Read::Yes(EditorAction::Hover),
                '&' => Read::Yes(EditorAction::RepeatSubstitute { all: false }),
                'q' => keys.then(
                    "register",
                    |name| name.is_ascii_alphanumeric(),
                    EditorAction::Record,
                ),
                '@' => keys.then(
                    "register",
                    |name| (is_register(name) && name != '_') || name == '@',
                    EditorAction::Replay,
                ),
                _ => return None,
            },
            _ => return None,
        })
    }

    fn after_g(
        &self,
        stroke: Stroke,
        keys: &mut Reader<'_>,
        visual: bool,
    ) -> Option<Read<EditorAction>> {
        let ask = |query| Read::Yes(EditorAction::Ask(query));
        Some(match stroke.plain()? {
            // `gq` on a selection formats it, as its language server does.
            'q' if visual => Read::Yes(EditorAction::FormatSelection),
            '&' => Read::Yes(EditorAction::RepeatSubstitute { all: true }),
            // What a language server knows: `gd`, `gD`, `gy`, `gri`, `grr`,
            // `grn` and `gra`, as Neovim's are.
            'd' => ask(Query::Definition),
            'D' => ask(Query::Declaration),
            'y' => ask(Query::TypeDefinition),
            'r' => match keys.next() {
                None => Read::More,
                Some(last) => match last.plain() {
                    Some('i') => ask(Query::Implementation),
                    Some('r') => ask(Query::References),
                    Some('n') => Read::Yes(EditorAction::Rename),
                    Some('a') => Read::Yes(EditorAction::CodeActions),
                    _ => Read::No,
                },
            },
            _ => return None,
        })
    }

    /// The next diagnostic, hunk, function or class, or the one before.
    fn after_bracket(
        &self,
        backward: bool,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<EditorAction>> {
        Some(Read::Yes(match stroke.plain()? {
            'd' => EditorAction::Diagnostic { backward },
            'c' => EditorAction::Hunk { backward },
            key @ ('f' | 'C') => EditorAction::Piece {
                construct: if key == 'f' {
                    Construct::Function
                } else {
                    Construct::Class
                },
                backward,
            },
            _ => return None,
        }))
    }
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

    /// Whether, in visual mode, it acts on what's selected: an operator,
    /// `p` and `P`, `J` and `r`.
    pub(crate) fn on_selection(self) -> bool {
        matches!(
            self,
            Self::Operate(_, Target::Selection) | Self::Put { .. } | Self::Join | Self::Replace(_)
        )
    }

    /// Whether it's a jump, which the jump list records, as Vim's are:
    /// `gg`, `G` and Ctrl+End, `H`, `M` and `L`, `%`, `{` and `}`, `n` and
    /// `N`, `*` and `#`, `]d` and `[d`, `]c` and `[c`, `]f` and the like,
    /// and going to a mark. A search typed after `/` is too, once Enter
    /// finds it, as is going where a language server says, once it
    /// answers.
    pub(crate) fn jumps(self) -> bool {
        matches!(
            self,
            Self::Move(
                Motion::First
                    | Motion::Last
                    | Motion::TextEnd
                    | Motion::ViewTop
                    | Motion::ViewMiddle
                    | Motion::ViewBottom
                    | Motion::Match
                    | Motion::Paragraph { .. }
            ) | Self::SearchAgain { .. }
                | Self::SearchWord { .. }
                | Self::Own(
                    EditorAction::Diagnostic { .. }
                        | EditorAction::Hunk { .. }
                        | EditorAction::Piece { .. }
                )
                | Self::Mark { .. }
        )
    }
}

/// Whether `command` is a jump, which the jump list records: one whose
/// action is (see [`Action::jumps`]), or `zt`, `zz` or `zb` with a count,
/// going to that line, as Vim's are.
pub(crate) fn jumps(command: &Command) -> bool {
    command.action.jumps() || matches!(command.action, Action::View(_)) && command.count.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::Object;
    use crate::vim::{Entry, Recase};

    fn read(text: &str) -> Parsed {
        let keys: Vec<Stroke> = text.chars().map(Stroke::char).collect();
        vim::parse(&keys, false, &EditorKeys { tildeop: false })
    }

    fn command(count: Option<usize>, action: Action) -> Parsed {
        Parsed::Command(Command {
            count,
            action,
            register: None,
        })
    }

    fn own(count: Option<usize>, action: EditorAction) -> Parsed {
        command(count, Action::Own(action))
    }

    #[test]
    fn the_language_servers_keys() {
        assert_eq!(read("gd"), own(None, EditorAction::Ask(Query::Definition)));
        assert_eq!(read("gr"), Parsed::Incomplete);
        assert_eq!(
            read("gri"),
            own(None, EditorAction::Ask(Query::Implementation))
        );
        assert_eq!(read("grn"), own(None, EditorAction::Rename));
        assert_eq!(read("grx"), Parsed::Nothing);
        assert_eq!(read("K"), own(None, EditorAction::Hover));
        assert_eq!(
            vim::parse(
                &[Stroke::ctrl('w'), Stroke::char('d')],
                false,
                &EditorKeys { tildeop: false }
            ),
            own(None, EditorAction::ShowDiagnostics)
        );
        // `gq` formats a selection, and is nothing without one.
        assert_eq!(read("gq"), Parsed::Nothing);
        let visual: Vec<Stroke> = "gq".chars().map(Stroke::char).collect();
        assert_eq!(
            vim::parse(&visual, true, &EditorKeys { tildeop: false }),
            own(None, EditorAction::FormatSelection)
        );
        // None of them is what an operator acts on, so `dgr` is nothing at
        // once, as in Neovim, where `gr` and the keys after it are normal
        // mode's alone.
        assert_eq!(read("dgd"), Parsed::Nothing);
        assert_eq!(read("dgr"), Parsed::Nothing);
    }

    #[test]
    fn h_m_and_l_are_jumps() {
        assert_eq!(
            read("3L"),
            command(Some(3), Action::Move(Motion::ViewBottom))
        );
        assert!(Action::Move(Motion::ViewMiddle).jumps());
    }

    #[test]
    fn macros() {
        assert_eq!(read("q"), Parsed::Incomplete);
        assert_eq!(read("qa"), own(None, EditorAction::Record('a')));
        assert_eq!(read("q-"), Parsed::Nothing);
        assert_eq!(read("3@a"), own(Some(3), EditorAction::Replay('a')));
        assert_eq!(read("@@"), own(None, EditorAction::Replay('@')));
        assert_eq!(read("@_"), Parsed::Nothing);
    }

    #[test]
    fn brackets_go_to_pieces_of_code_hunks_and_diagnostics() {
        let piece = |construct, backward| EditorAction::Piece {
            construct,
            backward,
        };
        assert_eq!(read("2]f"), own(Some(2), piece(Construct::Function, false)));
        assert_eq!(read("[C"), own(None, piece(Construct::Class, true)));
        assert_eq!(
            read("]c"),
            own(None, EditorAction::Hunk { backward: false })
        );
        assert_eq!(
            read("[d"),
            own(None, EditorAction::Diagnostic { backward: true })
        );
        assert_eq!(read("]"), Parsed::Incomplete);
        assert_eq!(read("]x"), Parsed::Nothing);
        assert!(Action::Own(piece(Construct::Function, true)).jumps());
    }

    #[test]
    fn ampersand_does_the_last_substitute_again() {
        assert_eq!(
            read("&"),
            own(None, EditorAction::RepeatSubstitute { all: false })
        );
        assert_eq!(
            read("g&"),
            own(None, EditorAction::RepeatSubstitute { all: true })
        );
        assert!(!Action::Own(EditorAction::RepeatSubstitute { all: false }).changes());
    }

    #[test]
    fn what_changes_text_and_what_acts_on_a_selection() {
        let lines = |recase| Action::Operate(Operator::Recase(recase), Target::Lines);
        assert!(Action::Tilde.changes() && lines(Recase::Lower).changes());
        assert!(!Action::Reselect.changes());
        // What `.` makes again on as much text, made in visual mode.
        assert!(Action::Operate(Operator::Indent, Target::Selection).on_selection());
        assert!(Action::Replace('x').on_selection());
        assert!(!Action::Operate(Operator::Change, Target::Lines).on_selection());
        assert!(!Action::Insert(Entry::LineStart).on_selection());
        let word = Action::Select {
            object: Object::Word,
            inner: false,
        };
        assert!(!word.changes());
    }
}
