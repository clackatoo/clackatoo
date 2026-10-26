//! What must hold for a step of a lesson to be done, written in a
//! `check` block a line each.
//!
//! | Line | Holds when |
//! |---|---|
//! | `has "TEXT"` | The practice file's text has TEXT in it. |
//! | `line N` | The focused editor's cursor is on line N, from 1. |
//! | `at "TEXT"` | The focused editor's cursor is in TEXT, on its line. |
//! | `register R "TEXT"` | Register R holds TEXT, exactly. |
//! | `copied "TEXT"` | What was last copied to the clipboard, or yanked, has TEXT in it. |
//! | `mode MODE` | The focused pane is in `normal`, `insert`, `visual`, `visual-line` or `visual-block` mode. |
//! | `pane KIND` | The lesson's tab has a pane of KIND: `terminal`, `editor`, `folder` (an editor showing a folder), `blank` (an editor showing a buffer with no file yet), `explorer`, `chat` or `board`. |
//! | `panes N` | The lesson's tab has N panes. |
//! | `focused WHAT` | The focused pane is the `lesson` pane, the `practice` pane, or one of KIND. |
//! | `shown lesson`, `shown elsewhere` | The lesson's tab is shown, or another. |
//! | `workspace "NAME"` | The workspace shown is called NAME. |
//! | `exited N` | The practice terminal's last command exited with status N. |
//! | `option SETTING` | An option is as `:set` writes it: `option nowrap`, `option tabstop=8`. |
//! | `finder`, `finder KIND` | The finder is open: any, or `files`, `grep`, `buffers`, `diagnostics`, `marks`, `places`, `boards` or `background`; or `agent`, the start page `:Agent` opens. |
//! | `listed "PATH"` | A file explorer in the lesson's tab lists PATH, from its root. |
//! | `rooted "PATH"` | A file explorer in the lesson's tab is rooted at PATH, from the lesson's folder: `.` for the folder itself. |
//! | `file "NAME"` | The focused editor's file is called NAME. |
//! | `recording R` | A macro is being recorded into register R. |
//! | `messages` | `:Messages` shows what the bar has said lately. |
//! | `saved` | The practice file has no changes that aren't written. |
//! | `rendered` | The focused editor shows its Markdown rendered. |
//! | `background N` | The workspace shown has N background tabs. |
//! | `kept N` | N terminals of the workspace shown are kept running. |
//! | `not CHECK` | CHECK doesn't hold. |
//!
//! Text is written in double quotes, with `\n` a line break, `\t` a tab,
//! and `\"` and `\\` themselves.

/// One thing that must hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Check {
    Has(String),
    /// The practice file's text, exactly, from an `expect` block.
    Text(String),
    Line(usize),
    At(String),
    Register(char, String),
    Copied(String),
    Mode(Mode),
    Pane(Kind),
    Panes(usize),
    Focused(Focus),
    /// `shown lesson` if true, `shown elsewhere` if not.
    Shown(bool),
    Workspace(String),
    Exited(i32),
    /// The option's name, and how `:set` writes it as it must be.
    Option(&'static str, String),
    Finder(Option<String>),
    Listed(String),
    Rooted(String),
    File(String),
    Recording(char),
    Messages,
    Saved,
    Rendered,
    Background(usize),
    Kept(usize),
    Not(Box<Check>),
}

/// A pane's mode, as a check names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Normal,
    Insert,
    Visual,
    VisualLine,
    VisualBlock,
}

/// A kind of pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Terminal,
    Editor,
    /// An editor showing a folder.
    Folder,
    /// An editor showing a buffer with no file yet, as `:e` alone makes.
    Blank,
    Explorer,
    Chat,
    Board,
    Review,
    Browser,
    /// A pane of a kind this window doesn't know, from a newer daemon: no
    /// lesson asks for one.
    Other,
}

/// Which pane has the focus, as a check names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Lesson,
    Practice,
    Kind(Kind),
}

/// The finder's kinds, as checks name them, and the start page `:Agent`
/// opens, which is drawn as the finder is.
const FINDERS: [&str; 11] = [
    "background",
    "files",
    "grep",
    "buffers",
    "diagnostics",
    "marks",
    "places",
    "boards",
    "symbols",
    "projectsymbols",
    "agent",
];

/// What checks look at: the window's state, as it is now.
pub(crate) trait Facts {
    /// The practice file's text, if it's open.
    fn practice_text(&self) -> Option<String>;
    /// Whether the practice file has no changes that aren't written, if
    /// it's open.
    fn practice_saved(&self) -> Option<bool>;
    /// The focused editor's cursor: its line, from 0, the line's text, and
    /// the byte of it the cursor's on.
    fn cursor(&self) -> Option<(usize, String, usize)>;
    fn register(&self, name: char) -> Option<String>;
    /// The focused pane's mode.
    fn mode(&self) -> Option<Mode>;
    /// The kinds of the panes in the lesson's tab.
    fn panes(&self) -> Vec<Kind>;
    /// The focused pane's kind, and whether it's the lesson pane or the
    /// practice pane.
    fn focused(&self) -> Option<(Kind, Option<Focus>)>;
    /// Whether the lesson's tab is shown.
    fn lesson_shown(&self) -> bool;
    fn workspace(&self) -> Option<String>;
    /// The exit status of the practice terminal's last command, once it
    /// has finished.
    fn last_exit(&self) -> Option<i32>;
    /// How `:set` writes the option called `name`.
    fn option(&self, name: &str) -> String;
    /// The open finder's kind, as checks name it.
    fn finder(&self) -> Option<&'static str>;
    /// Whether an explorer in the lesson's tab lists `path`, from its
    /// root.
    fn listed(&self, path: &str) -> bool;
    /// Whether an explorer in the lesson's tab is rooted at `path`, from
    /// the lesson's folder, `.` being the folder itself.
    fn rooted(&self, path: &str) -> bool;
    /// The focused editor's file's name.
    fn file(&self) -> Option<String>;
    /// The register a macro is being recorded into.
    fn recording(&self) -> Option<char>;
    /// Whether `:Messages` shows what the bar has said lately.
    fn messages_shown(&self) -> bool;
    /// Whether the focused editor shows its Markdown rendered.
    fn rendered(&self) -> bool;
    /// How many background tabs the workspace shown has.
    fn background_tabs(&self) -> usize;
    /// How many terminals of the workspace shown are kept running.
    fn kept_terminals(&self) -> usize;
}

impl Check {
    /// Reads a line of a `check` block.
    pub(crate) fn parse(line: &str) -> Result<Self, String> {
        let words = words(line)?;
        Self::from_words(&words.iter().map(String::as_str).collect::<Vec<_>>())
            .map_err(|why| format!("`{}`: {why}", line.trim()))
    }

    fn from_words(words: &[&str]) -> Result<Self, String> {
        let number = |word: &str| {
            word.parse::<usize>()
                .map_err(|_| format!("`{word}` isn't a number"))
        };
        let register = |word: &str| match word.chars().collect::<Vec<_>>()[..] {
            [name] if name.is_ascii_alphanumeric() || "\"-+*".contains(name) => Ok(name),
            _ => Err(format!("`{word}` isn't a register")),
        };
        Ok(match words {
            ["not", rest @ ..] => Self::Not(Box::new(Self::from_words(rest)?)),
            ["has", text] => Self::Has((*text).to_owned()),
            ["line", line] => match number(line)? {
                0 => return Err("lines count from 1".into()),
                line => Self::Line(line),
            },
            ["at", text] if !text.is_empty() => Self::At((*text).to_owned()),
            ["register", name, text] => Self::Register(register(name)?, (*text).to_owned()),
            ["copied", text] => Self::Copied((*text).to_owned()),
            ["mode", mode] => Self::Mode(match *mode {
                "normal" => Mode::Normal,
                "insert" => Mode::Insert,
                "visual" => Mode::Visual,
                "visual-line" => Mode::VisualLine,
                "visual-block" => Mode::VisualBlock,
                other => return Err(format!("`{other}` isn't a mode")),
            }),
            ["pane", kind] => Self::Pane(kind_of(kind)?),
            ["panes", count] => Self::Panes(number(count)?),
            ["focused", "lesson"] => Self::Focused(Focus::Lesson),
            ["focused", "practice"] => Self::Focused(Focus::Practice),
            ["focused", kind] => Self::Focused(Focus::Kind(kind_of(kind)?)),
            ["shown", "lesson"] => Self::Shown(true),
            ["shown", "elsewhere"] => Self::Shown(false),
            ["workspace", name] => Self::Workspace((*name).to_owned()),
            ["exited", status] => Self::Exited(
                status
                    .parse()
                    .map_err(|_| format!("`{status}` isn't a status"))?,
            ),
            ["option", setting] => {
                let name = setting.split('=').next().unwrap_or_default();
                let info = crate::options::find(name)
                    .or_else(|_| crate::options::find(name.trim_start_matches("no")))?;
                Self::Option(info.name, (*setting).to_owned())
            }
            ["finder"] => Self::Finder(None),
            ["finder", kind] if FINDERS.contains(kind) => Self::Finder(Some((*kind).to_owned())),
            ["listed", path] => Self::Listed((*path).to_owned()),
            ["rooted", path] => Self::Rooted((*path).to_owned()),
            ["file", name] => Self::File((*name).to_owned()),
            ["recording", name] => Self::Recording(register(name)?),
            ["messages"] => Self::Messages,
            ["saved"] => Self::Saved,
            ["rendered"] => Self::Rendered,
            ["background", count] => Self::Background(number(count)?),
            ["kept", count] => Self::Kept(number(count)?),
            _ => return Err("that isn't a check".into()),
        })
    }

    /// Whether it holds, as `facts` are.
    pub(crate) fn holds(&self, facts: &dyn Facts) -> bool {
        match self {
            Self::Has(text) => facts
                .practice_text()
                .is_some_and(|all| all.contains(text.as_str())),
            Self::Text(text) => facts.practice_text().is_some_and(|all| all == *text),
            Self::Line(line) => facts.cursor().is_some_and(|(at, _, _)| at + 1 == *line),
            Self::At(text) => facts.cursor().is_some_and(|(_, line, byte)| {
                line.match_indices(text.as_str())
                    .any(|(start, found)| (start..start + found.len()).contains(&byte))
            }),
            Self::Register(name, text) => facts.register(*name).is_some_and(|held| held == *text),
            // The unnamed register too, for a yank kept off the clipboard by
            // `clipboard`.
            Self::Copied(text) => ['+', '"'].into_iter().any(|name| {
                facts
                    .register(name)
                    .is_some_and(|held| held.contains(text.as_str()))
            }),
            Self::Mode(mode) => facts.mode() == Some(*mode),
            Self::Pane(kind) => facts.panes().contains(kind),
            Self::Panes(count) => facts.panes().len() == *count,
            Self::Focused(focus) => facts.focused().is_some_and(|(kind, role)| match focus {
                Focus::Kind(wanted) => kind == *wanted,
                role_wanted => role == Some(*role_wanted),
            }),
            Self::Shown(lesson) => facts.lesson_shown() == *lesson,
            Self::Workspace(name) => facts.workspace().is_some_and(|shown| shown == *name),
            Self::Exited(status) => facts.last_exit() == Some(*status),
            Self::Option(name, setting) => facts.option(name) == *setting,
            Self::Finder(kind) => match (facts.finder(), kind) {
                (Some(_), None) => true,
                (Some(open), Some(kind)) => open == kind,
                (None, _) => false,
            },
            Self::Listed(path) => facts.listed(path),
            Self::Rooted(path) => facts.rooted(path),
            Self::File(name) => facts.file().is_some_and(|file| file == *name),
            Self::Recording(name) => facts.recording() == Some(*name),
            Self::Messages => facts.messages_shown(),
            Self::Saved => facts.practice_saved() == Some(true),
            Self::Rendered => facts.rendered(),
            Self::Background(count) => facts.background_tabs() == *count,
            Self::Kept(count) => facts.kept_terminals() == *count,
            Self::Not(check) => !check.holds(facts),
        }
    }

    /// Whether it looks at the practice terminal's last command, which
    /// the window must ask the daemon about.
    pub(crate) fn wants_exit(&self) -> bool {
        match self {
            Self::Exited(_) => true,
            Self::Not(check) => check.wants_exit(),
            _ => false,
        }
    }

    /// Whether it looks only at the practice file's text, so holds or
    /// not on that alone, as the tests that lint lessons need to know.
    #[cfg(test)]
    pub(crate) fn text_only(&self) -> bool {
        match self {
            Self::Has(_) | Self::Text(_) => true,
            Self::Not(check) => check.text_only(),
            _ => false,
        }
    }
}

fn kind_of(word: &str) -> Result<Kind, String> {
    Ok(match word {
        "terminal" => Kind::Terminal,
        "editor" => Kind::Editor,
        "folder" => Kind::Folder,
        "blank" => Kind::Blank,
        "explorer" => Kind::Explorer,
        "chat" => Kind::Chat,
        "board" => Kind::Board,
        "review" => Kind::Review,
        "browser" => Kind::Browser,
        other => return Err(format!("`{other}` isn't a kind of pane")),
    })
}

/// Splits `line` into words at blanks, a word in double quotes being one,
/// with its escapes read.
pub(crate) fn words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut chars = line.trim().chars().peekable();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }
        let mut word = String::new();
        if ch == '"' {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => word.push(match chars.next() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('"') => '"',
                        Some('\\') => '\\',
                        other => {
                            return Err(format!("`\\{}` isn't an escape", other.unwrap_or(' ')));
                        }
                    }),
                    Some(ch) => word.push(ch),
                    None => return Err(format!("text that isn't closed: `{line}`")),
                }
            }
        } else {
            while let Some(&ch) = chars.peek().filter(|ch| !ch.is_whitespace()) {
                word.push(ch);
                chars.next();
            }
        }
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts made up for a test.
    #[derive(Default)]
    struct Made {
        text: Option<String>,
        saved: Option<bool>,
        cursor: Option<(usize, String, usize)>,
        registers: Vec<(char, String)>,
        mode: Option<Mode>,
        panes: Vec<Kind>,
        focused: Option<(Kind, Option<Focus>)>,
        lesson_shown: bool,
        workspace: Option<String>,
        exit: Option<i32>,
        finder: Option<&'static str>,
        listed: Vec<String>,
        rooted: Option<String>,
        file: Option<String>,
        recording: Option<char>,
        messages: bool,
        rendered: bool,
        background: usize,
    }

    impl Facts for Made {
        fn practice_text(&self) -> Option<String> {
            self.text.clone()
        }
        fn practice_saved(&self) -> Option<bool> {
            self.saved
        }
        fn cursor(&self) -> Option<(usize, String, usize)> {
            self.cursor.clone()
        }
        fn register(&self, name: char) -> Option<String> {
            (self.registers.iter()).find_map(|(held, text)| (*held == name).then(|| text.clone()))
        }
        fn mode(&self) -> Option<Mode> {
            self.mode
        }
        fn panes(&self) -> Vec<Kind> {
            self.panes.clone()
        }
        fn focused(&self) -> Option<(Kind, Option<Focus>)> {
            self.focused
        }
        fn lesson_shown(&self) -> bool {
            self.lesson_shown
        }
        fn workspace(&self) -> Option<String> {
            self.workspace.clone()
        }
        fn last_exit(&self) -> Option<i32> {
            self.exit
        }
        fn option(&self, name: &str) -> String {
            let mut options = crate::options::Options::default();
            if name == "wrap" {
                options.wrap = false;
            }
            options.show(crate::options::find(name).unwrap())
        }
        fn finder(&self) -> Option<&'static str> {
            self.finder
        }
        fn listed(&self, path: &str) -> bool {
            self.listed.iter().any(|listed| listed == path)
        }
        fn rooted(&self, path: &str) -> bool {
            self.rooted.as_deref() == Some(path)
        }
        fn file(&self) -> Option<String> {
            self.file.clone()
        }
        fn recording(&self) -> Option<char> {
            self.recording
        }
        fn messages_shown(&self) -> bool {
            self.messages
        }
        fn rendered(&self) -> bool {
            self.rendered
        }
        fn background_tabs(&self) -> usize {
            self.background
        }
        fn kept_terminals(&self) -> usize {
            0
        }
    }

    fn holds(line: &str, facts: &Made) -> bool {
        Check::parse(line).unwrap().holds(facts)
    }

    #[test]
    fn words_are_split_at_blanks_with_quoted_text_one_word() {
        assert_eq!(
            words(r#"register a "two words\n\"q\"\\""#).unwrap(),
            ["register", "a", "two words\n\"q\"\\"]
        );
        assert!(words(r#"has "open"#).is_err());
        assert!(words(r#"has "\q""#).is_err());
    }

    #[test]
    fn checks_on_the_practice_file_look_at_its_text() {
        let facts = Made {
            text: Some("fn b() {}\n".into()),
            saved: Some(false),
            ..Made::default()
        };
        assert!(holds(r#"has "fn b""#, &facts));
        assert!(!holds(r#"has "fn a""#, &facts));
        assert!(holds(r#"not has "fn a""#, &facts));
        assert!(Check::Text("fn b() {}\n".into()).holds(&facts));
        assert!(!Check::Text("fn b() {}".into()).holds(&facts));
        assert!(!holds("saved", &facts));
        assert!(!holds(r#"has "x""#, &Made::default()));
    }

    #[test]
    fn checks_on_the_cursor_look_at_the_focused_editor() {
        let facts = Made {
            cursor: Some((2, "    let total = 3;".into(), 9)),
            file: Some("a.rs".into()),
            ..Made::default()
        };
        assert!(holds("line 3", &facts));
        assert!(!holds("line 2", &facts));
        assert!(holds(r#"at "total""#, &facts));
        assert!(!holds(r#"at "let""#, &facts));
        assert!(holds(r#"file "a.rs""#, &facts));
        assert!(Check::parse("line 0").is_err());
        assert!(!holds("rendered", &facts));
        let rendered = Made {
            rendered: true,
            ..Made::default()
        };
        assert!(holds("rendered", &rendered));
    }

    #[test]
    fn checks_on_registers_and_the_clipboard() {
        let facts = Made {
            registers: vec![('a', "one\n".into()), ('+', "out: 42".into())],
            recording: Some('q'),
            ..Made::default()
        };
        assert!(holds(r#"register a "one\n""#, &facts));
        assert!(!holds(r#"register a "one""#, &facts));
        assert!(holds(r#"copied "42""#, &facts));
        assert!(!holds(r#"copied "one""#, &facts));
        // A yank `clipboard` kept off the clipboard is copied too.
        let yanked = Made {
            registers: vec![('"', "hello\n".into())],
            ..Made::default()
        };
        assert!(holds(r#"copied "hello""#, &yanked));
        assert!(holds("recording q", &facts));
        assert!(!holds("messages", &facts));
        let facts = Made {
            messages: true,
            ..facts
        };
        assert!(holds("messages", &facts));
        assert!(!holds("recording a", &facts));
        assert!(Check::parse(r#"register ab "x""#).is_err());
    }

    #[test]
    fn checks_on_panes_modes_and_where_the_focus_is() {
        let facts = Made {
            mode: Some(Mode::Insert),
            panes: vec![Kind::Editor, Kind::Terminal, Kind::Terminal],
            focused: Some((Kind::Terminal, Some(Focus::Practice))),
            lesson_shown: true,
            workspace: Some("w".into()),
            ..Made::default()
        };
        assert!(holds("mode insert", &facts));
        assert!(!holds("mode normal", &facts));
        assert!(holds("pane terminal", &facts));
        assert!(holds("not pane chat", &facts));
        assert!(holds("panes 3", &facts));
        assert!(holds("focused practice", &facts));
        assert!(holds("focused terminal", &facts));
        assert!(!holds("focused lesson", &facts));
        assert!(holds("shown lesson", &facts));
        assert!(!holds("shown elsewhere", &facts));
        assert!(holds(r#"workspace "w""#, &facts));
        assert!(Check::parse("pane window").is_err());
        assert!(Check::parse("mode replace").is_err());
    }

    #[test]
    fn checks_on_commands_options_the_finder_and_explorers() {
        let facts = Made {
            exit: Some(0),
            finder: Some("grep"),
            listed: vec!["notes/todo.txt".into()],
            rooted: Some("notes".into()),
            ..Made::default()
        };
        assert!(holds("exited 0", &facts));
        assert!(!holds("exited 1", &facts));
        assert!(Check::parse("exited 0").unwrap().wants_exit());
        assert!(holds("option nowrap", &facts));
        assert!(!holds("option wrap", &facts));
        assert!(holds("option tabstop=4", &facts));
        assert!(holds("finder", &facts));
        assert!(holds("finder grep", &facts));
        assert!(!holds("finder files", &facts));
        assert!(holds(r#"listed "notes/todo.txt""#, &facts));
        assert!(!holds(r#"listed "notes""#, &facts));
        assert!(holds(r#"rooted "notes""#, &facts));
        assert!(!holds(r#"rooted ".""#, &facts));
        assert!(Check::parse("option nothing").is_err());
        assert!(Check::parse("finder windows").is_err());
    }
}
