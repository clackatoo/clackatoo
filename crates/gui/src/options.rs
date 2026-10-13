//! Options: settings for how the window behaves, as Vim's are. Config sets
//! them with `client.opt`, and `:set` changes them as you go.
//!
//! Each option is declared once, in the [`options!`] list below, with a
//! short name as Vim's have, and the window keeps their values in an
//! [`Options`], with a field for each, so reading one costs nothing.
//!
//! The window's `Options` is the one that counts. Lua keeps a copy, to
//! read from and to check what it sets, and the window tells it every
//! change it makes, whoever asked for it, so the copy ends up the same
//! whatever order changes cross in.

/// An option's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Bool(bool),
    Number(u32),
    List(List),
    /// One of the words an option of [`Kind::Choice`] can be.
    Choice(&'static str),
    /// Text, as an option of [`Kind::Text`] holds.
    Text(String),
}

/// Whole numbers, as an option holds several: `colorcolumn`'s columns.
pub(crate) type List = Vec<u32>;

/// What an option holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Bool,
    /// A whole number from `min` to `max`.
    Number {
        min: u32,
        max: u32,
    },
    /// Any number of whole numbers, each from `min` to `max`, written with
    /// commas between them: `80,120`. None is written as nothing.
    List {
        min: u32,
        max: u32,
    },
    /// One of these words.
    Choice(&'static [&'static str]),
    /// Any text without control characters, such as `showbreak`'s.
    Text,
}

/// An option, as config and `:set` know it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Info {
    pub(crate) name: &'static str,
    /// The short name, as Vim's: `nu` for `number`.
    pub(crate) short: &'static str,
    pub(crate) kind: Kind,
}

impl Info {
    /// Checks `value` is one this option can hold, saying why not.
    fn check(&self, value: &Value) -> Result<(), String> {
        match (self.kind, value) {
            (Kind::Bool, Value::Bool(_)) => Ok(()),
            (Kind::Number { min, max }, Value::Number(number)) if (min..=max).contains(number) => {
                Ok(())
            }
            (Kind::List { min, max }, Value::List(numbers))
                if numbers.iter().all(|number| (min..=max).contains(number)) =>
            {
                Ok(())
            }
            (Kind::Choice(words), Value::Choice(word)) if words.contains(word) => Ok(()),
            (Kind::Text, Value::Text(text)) if !text.chars().any(char::is_control) => Ok(()),
            _ => Err(self.wants()),
        }
    }

    /// Reads `text` as a value for this option, as `:set` gives one after
    /// `=`: a number, or for a list, numbers with commas between them, or
    /// for text, the text.
    pub(crate) fn parse(&self, text: &str) -> Result<Value, String> {
        let number = |text: &str| text.parse::<u32>().map_err(|_| self.wants());
        let value = match self.kind {
            Kind::Bool => return Err(format!("{} is set with {0} or no{0}, not =", self.name)),
            Kind::Number { .. } => Value::Number(number(text)?),
            Kind::List { .. } if text.is_empty() => Value::List(List::new()),
            Kind::List { .. } => {
                Value::List(text.split(',').map(number).collect::<Result<_, _>>()?)
            }
            Kind::Choice(words) => Value::Choice(
                (words.iter().find(|&&word| word == text)).ok_or_else(|| self.wants())?,
            ),
            Kind::Text => Value::Text(text.to_owned()),
        };
        self.check(&value)?;
        Ok(value)
    }

    /// What values the option holds, for messages.
    pub(crate) fn wants(&self) -> String {
        match self.kind {
            Kind::Bool => format!("{} is true or false", self.name),
            Kind::Number { min, max } => {
                format!("{} is a whole number from {min} to {max}", self.name)
            }
            Kind::List { min, max } => format!(
                "{} is whole numbers from {min} to {max}, with commas between them",
                self.name
            ),
            Kind::Choice(words) => match words {
                [] => format!("{} can't be set", self.name),
                [only] => format!("{} is {only}", self.name),
                [first @ .., last] => {
                    format!("{} is {} or {last}", self.name, first.join(", "))
                }
            },
            Kind::Text => format!("{} is text, without control characters", self.name),
        }
    }
}

/// A type options hold, and its [`Value`].
trait Held: Sized {
    fn value(&self) -> Value;
    fn from_value(value: Value) -> Option<Self>;
}

impl Held for bool {
    fn value(&self) -> Value {
        Value::Bool(*self)
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Bool(value) => Some(value),
            _ => None,
        }
    }
}

impl Held for u32 {
    fn value(&self) -> Value {
        Value::Number(*self)
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Number(value) => Some(value),
            _ => None,
        }
    }
}

impl Held for List {
    fn value(&self) -> Value {
        Value::List(self.clone())
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::List(value) => Some(value),
            _ => None,
        }
    }
}

impl Held for String {
    fn value(&self) -> Value {
        Value::Text(self.clone())
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Text(value) => Some(value),
            _ => None,
        }
    }
}

/// An option's type that's one of a few words, declared by [`choice!`].
trait Choice: Held {
    /// The words, in the order they're listed.
    const WORDS: &'static [&'static str];
}

/// Declares an option's type that's one of a few words: an enum with a
/// variant for each, as [`Choice`] and [`Held`].
macro_rules! choice {
    ($(#[doc = $doc:literal])+ $name:ident { $($(#[doc = $variant_doc:literal])+ $variant:ident = $word:literal,)+ }) => {
        $(#[doc = $doc])+
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum $name {
            $($(#[doc = $variant_doc])+ $variant,)+
        }

        impl Choice for $name {
            const WORDS: &'static [&'static str] = &[$($word),+];
        }

        impl Held for $name {
            fn value(&self) -> Value {
                Value::Choice(match self {
                    $(Self::$variant => $word,)+
                })
            }

            fn from_value(value: Value) -> Option<Self> {
                match value {
                    $(Value::Choice($word) => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

/// The [`Kind`] of an option of type `bool`, or `u32` or [`List`] with
/// its range, `String`, or a [`Choice`].
macro_rules! kind {
    (bool) => {
        Kind::Bool
    };
    (String) => {
        Kind::Text
    };
    (u32 $min:literal ..= $max:literal) => {
        Kind::Number {
            min: $min,
            max: $max,
        }
    };
    (List $min:literal ..= $max:literal) => {
        Kind::List {
            min: $min,
            max: $max,
        }
    };
    ($choice:ident) => {
        Kind::Choice(<$choice as Choice>::WORDS)
    };
}

/// Declares the options: [`Options`], with a field for each, and [`ALL`].
macro_rules! options {
    ($(
        $(#[doc = $doc:literal])+
        $name:ident ($short:literal): $type:ident $(($min:literal ..= $max:literal))? = $default:expr;
    )+) => {
        /// The value of every option.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub(crate) struct Options {
            $(
                $(#[doc = $doc])+
                pub(crate) $name: $type,
            )+
        }

        impl Default for Options {
            fn default() -> Self {
                Self {
                    $($name: $default,)+
                }
            }
        }

        /// Every option, in the order they're listed.
        pub(crate) const ALL: &[Info] = &[$(
            Info {
                name: stringify!($name),
                short: $short,
                kind: kind!($type $($min ..= $max)?),
            },
        )+];

        impl Options {
            /// The value of the option called `name`.
            fn value(&self, name: &str) -> Option<Value> {
                match name {
                    $(stringify!($name) => Some(Held::value(&self.$name)),)+
                    _ => None,
                }
            }

            /// Sets the option called `name`, which [`Info::check`] has
            /// checked `value` suits.
            fn put(&mut self, name: &str, value: Value) {
                match name {
                    $(stringify!($name) => {
                        if let Some(value) = Held::from_value(value) {
                            self.$name = value;
                        }
                    })+
                    _ => {}
                }
            }
        }
    };
}

options! {
    /// Line numbers down an editor's left.
    number("nu"): bool = true;
    /// Each line's number as how far it is from the cursor's line, so the
    /// count for a motion such as `5j` can be read off. With `number`,
    /// the cursor's line shows its own number; without, 0.
    relativenumber("rnu"): bool = true;
    /// The fewest cells the line numbers take, with the space after them.
    numberwidth("nuw"): u32 (1..=20) = 4;
    /// The cursor's line lit faintly, across the pane, so the eye finds it.
    cursorline("cul"): bool = true;
    /// Columns marked down an editor, such as 80, to keep lines short by.
    /// They count from 1, as the bar's column does.
    colorcolumn("cc"): List (1..=9999) = List::new();
    /// What's wrong with a line, as its language server says, written
    /// after the line's end: the worst, if there are several.
    virtualtext("vt"): bool = true;
    /// A column of signs left of the line numbers, saying which lines are
    /// added, changed or deleted since the file's text in git's index, as
    /// gitsigns.nvim shows them. Kept for every file while it's on, so the
    /// text doesn't move when the first change comes.
    gitsigns("gs"): bool = true;
    /// A column of its own, right of git's signs, colouring the lines an
    /// agent changed that you haven't reviewed, in a colour for each
    /// chat. It shows only while a file has such lines, or `:Blame` is
    /// on.
    agentsigns("as"): bool = true;
    /// How wide a file explorer down the tab's left opens, in cells, as
    /// `<Space>e` and `:Explore` open it. It keeps its share of the tab as
    /// the window resizes.
    explorerwidth("ew"): u32 (10..=200) = 30;
    /// File explorers show hidden files, those whose names start with `.`,
    /// as they open. `.` shows or hides them in each.
    explorerhidden("eh"): bool = false;
    /// The fewest lines kept in view above and below the cursor, so what's
    /// around it shows as it moves. Half the pane or more keeps the
    /// cursor's line in the middle.
    scrolloff("so"): u32 (0..=9999) = 5;
    /// A line too long for the pane goes on over as many rows as it needs,
    /// rather than off the pane's right edge. `j` and `k` still move by
    /// line, and `gj` and `gk` by row.
    wrap("wrap"): bool = true;
    /// With `wrap`, a line is broken between words, after a blank or one
    /// of `!@*-+;:,./?`, rather than at the last cell that fits.
    linebreak("lbr"): bool = true;
    /// With `wrap`, the rows a line goes on over start as far in as the
    /// line is indented, so the code's shape still shows. At least 20
    /// cells are kept for the text.
    breakindent("bri"): bool = true;
    /// With `wrap`, drawn at the start of the rows a line goes on over,
    /// after their indent: `↪ `, say. In `:set`, a space is written `\ `.
    showbreak("sbr"): String = String::new();
    /// How wide a tab is drawn: to the next column that's a multiple of it.
    tabstop("ts"): u32 (1..=32) = 4;
    /// The columns `>>`, `<<` and Tab indent by. 0 means `tabstop`'s, so
    /// the two can't drift apart.
    shiftwidth("sw"): u32 (0..=32) = 0;
    /// Indent with spaces, not tabs, where no line nearby is indented yet.
    /// A file keeps the kind of indent it has.
    expandtab("et"): bool = true;
    /// A new line, one where `}`, `]` or `)` is typed first, and one where
    /// `:` ends `else`, `elif`, `except`, `finally`, `case` or `default`,
    /// is indented as the file's language says: by the rules of its
    /// grammar's `indents.scm`, as nvim-treesitter's are, not Vim's rules
    /// for C. It takes the indent of the line before at once, and is put
    /// right a moment later, unless its indent has been typed in, or
    /// another change made meanwhile. Off, a new line keeps the line
    /// before's. `=` indents by the rules either way.
    smartindent("si"): bool = true;
    /// Searches ignore case...
    ignorecase("ic"): bool = true;
    /// ...unless, with `ignorecase`, what's searched for has a capital.
    smartcase("scs"): bool = true;
    /// While `:s` is typed, show what it would do, in place, as Neovim
    /// does; and the matches of `:g`'s pattern. Nothing changes until
    /// Enter, and Esc puts the view back. `none` shows nothing.
    inccommand("icm"): InCommand = InCommand::NoSplit;
    /// `~` is an operator, as `g~` is, taking a motion: `~w`. Off, it
    /// turns over the case of the character under the cursor, and moves
    /// on past it.
    tildeop("top"): bool = false;
    /// `:w` formats the file first, as its language server does.
    formatonsave("fos"): bool = false;
    /// A file changed on disk by something other than an agent, as a git
    /// checkout or another editor changes one, is read again, if its
    /// buffer has no changes not saved: as an edit, which `u` undoes. Off,
    /// or with changes, the bar says so, and `:e!` takes the file's
    /// version, `:w!` keeps yours. Agents' changes are taken either way.
    autoread("ar"): bool = true;
    /// What's done when a file is opened in a language that isn't
    /// installed, or whose language server isn't.
    languageinstall("li"): LanguageInstall = LanguageInstall::Ask;
    /// Type through the desktop's input method, as Chinese, Japanese and
    /// Korean are typed, and as an emoji picker sends its emoji, while keys
    /// type text. Off, keys go straight to the pane, in case the input
    /// method gets in the way.
    ime("ime"): bool = true;
    /// As a word is typed in an editor, or a character its language server
    /// says should ask, such as `.`, ask the server what could be typed
    /// and show it in a menu under the cursor. Off, Ctrl+Space, Ctrl+N and
    /// Ctrl+P still ask.
    completion("cmp"): bool = true;
    /// How long typing a word pauses, in milliseconds, before the language
    /// server is asked what could finish it, so a burst of keys asks once.
    /// 0 asks at every key.
    completedelay("cmpd"): u32 (0..=2000) = 40;
    /// Which keys accept the item picked in the completion menu, besides
    /// Ctrl+Y, which always does: Enter, once an item has been moved to
    /// with Ctrl+N, Ctrl+P or the arrows; Enter, always; or Ctrl+Y alone,
    /// Enter always making a new line.
    completeaccept("cmpa"): CompleteAccept = CompleteAccept::Picked;
    /// Tab accepts the item picked in the completion menu too, as in VS
    /// Code, rather than indenting.
    completetab("cmpt"): bool = false;
    /// While a call's arguments are typed in an editor, its signature shows
    /// over the cursor, with the argument being typed in bold, as the
    /// language server says.
    signaturehelp("sig"): bool = true;
    /// Which of the desktop's the unnamed register goes along with, as in
    /// Neovim: the clipboard, so what's yanked or deleted in an editor can
    /// be pasted anywhere, and `p` puts what was copied anywhere; the
    /// primary selection, which a middle click pastes; or neither, the
    /// registers being the window's own but for `"+` and `"*`.
    clipboard("cb"): Clipboard = Clipboard::UnnamedPlus;
    /// What programs in terminals may do with the clipboard, through OSC
    /// 52, as Neovim and tmux do, even over ssh: put text on it (`copy`),
    /// read what's there (`paste`), both, or neither. Reading is off
    /// unless asked for, since the clipboard may hold a password, and
    /// whatever runs in a terminal would get it.
    osc52("osc52"): Osc52 = Osc52::Copy;
    /// The agent `:Agent` and Ctrl+B `a` open a chat with, by its name:
    /// `claude`, `codex`, `gemini`, or one the daemon's `agents.toml`
    /// adds. Empty is the daemon's default, Claude Code.
    agent("ag"): String = "claude".to_owned();
    /// What a new chat's agent may do with clackatoo's own tools, which
    /// show you things beside the chat: open files, run commands in
    /// terminals you can watch, and read them. `ask` asks in the chat
    /// before it runs a command, types into a terminal, or shows what's
    /// outside the chat's project; `allow` runs commands in its own
    /// terminals without asking, but asks the rest; `off` gives it no
    /// tools. The chat's Tools setting changes it later.
    agenttools("agt"): AgentTools = AgentTools::Ask;
    /// A new chat with Claude Code keeps Claude Code's own Task tool,
    /// which runs helpers you don't see inside one turn. Off, as it is
    /// unless you turn it on, Claude hands work on as every agent does,
    /// with clackatoo's `start_chat` and `start_chats`, in chats you see
    /// as cards and can open, steer and undo. Other agents have no Task
    /// tool.
    claudetask("ct"): bool = false;
    /// About the most of a conversation, in tokens, a chat tells an agent
    /// it moves to, as the model menu moves it from Claude Code to Codex,
    /// say: the newest turns whole, older ones a line each. An agent it
    /// goes back to hears only what happened since, as briefly.
    carryover("co"): u32 (1000..=200_000) = 20_000;
    /// Look for a new release of clackatoo, at most once a day, a while
    /// after the window opens, and say in the bar when one is out. It's
    /// never installed until `:update` or `clackatoo update` asks.
    updatecheck("uc"): bool = true;
    /// What's typed in a terminal shows at once, underlined, before the
    /// daemon's screen shows the program's echo, as mosh shows it, so
    /// typing over a slow link doesn't wait for the round trip: `auto`
    /// does this only for a daemon on another machine, while its round
    /// trip is over 30 ms or a key has just taken a long time; `always`
    /// does it even for a daemon on this machine; `never` doesn't.
    predictecho("pe"): PredictEcho = PredictEcho::Auto;
}

choice! {
    /// When `predictecho` shows what's typed before its echo.
    PredictEcho {
        /// On a slow link to another machine.
        Auto = "auto",
        /// Always, even on this machine.
        Always = "always",
        /// Never.
        Never = "never",
    }
}

choice! {
    /// Which of the desktop's `clipboard` says the unnamed register goes
    /// along with.
    Clipboard {
        /// The clipboard, `"+`.
        UnnamedPlus = "unnamedplus",
        /// The primary selection, `"*`.
        Unnamed = "unnamed",
        /// Neither.
        Off = "none",
    }
}

choice! {
    /// What `inccommand` shows while `:s` is typed.
    InCommand {
        /// What it would do, in place.
        NoSplit = "nosplit",
        /// Nothing.
        Off = "none",
    }
}

choice! {
    /// Which keys `completeaccept` says accept a completion, besides
    /// Ctrl+Y.
    CompleteAccept {
        /// Enter, once an item has been moved to.
        Picked = "picked",
        /// Enter, always.
        Enter = "enter",
        /// Neither: Ctrl+Y alone.
        CtrlY = "ctrl-y",
    }
}

choice! {
    /// What `osc52` lets programs in terminals do with the clipboard.
    Osc52 {
        /// Put text on it.
        Copy = "copy",
        /// Read what's there.
        Paste = "paste",
        /// Both.
        CopyPaste = "copy-paste",
        /// Neither.
        Off = "off",
    }
}

impl Osc52 {
    /// Whether programs may put text on the clipboard.
    pub(crate) fn copies(self) -> bool {
        matches!(self, Self::Copy | Self::CopyPaste)
    }

    /// Whether programs may read what's on the clipboard.
    pub(crate) fn pastes(self) -> bool {
        matches!(self, Self::Paste | Self::CopyPaste)
    }
}

choice! {
    /// What `agenttools` lets a new chat's agent do.
    AgentTools {
        /// Use the tools, asking first to run a command or type.
        Ask = "ask",
        /// Run commands in its own terminals without asking.
        Allow = "allow",
        /// Nothing: it gets none.
        Off = "off",
    }
}

impl AgentTools {
    /// As a new chat asks for it.
    pub(crate) fn asked(self) -> clackatoo_protocol::AgentTools {
        match self {
            Self::Ask => clackatoo_protocol::AgentTools::Ask,
            Self::Allow => clackatoo_protocol::AgentTools::Allow,
            Self::Off => clackatoo_protocol::AgentTools::Off,
        }
    }
}

choice! {
    /// What `languageinstall` says to do about a language that isn't
    /// installed.
    LanguageInstall {
        /// Ask, in the bottom bar.
        Ask = "ask",
        /// Install it without asking.
        Always = "always",
        /// Don't.
        Never = "never",
    }
}

/// The option called `name`, or by its short name.
pub(crate) fn find(name: &str) -> Result<&'static Info, String> {
    ALL.iter()
        .find(|info| info.name == name || info.short == name)
        .ok_or_else(|| {
            let names: Vec<&str> = ALL.iter().map(|info| info.name).collect();
            format!(
                "there's no option called `{name}`: the options are {}",
                names.join(", ")
            )
        })
}

impl Options {
    /// The value of `info`'s option.
    pub(crate) fn get(&self, info: &Info) -> Value {
        self.value(info.name)
            .expect("every option listed has a field")
    }

    /// Sets `info`'s option to `value`, if it can hold it. Returns whether
    /// that changed it.
    pub(crate) fn set(&mut self, info: &Info, value: Value) -> Result<bool, String> {
        info.check(&value)?;
        let changed = self.get(info) != value;
        self.put(info.name, value);
        Ok(changed)
    }

    /// How `info`'s option is written in `:set`: `number`, `nonumber`,
    /// `numberwidth=4`, or `colorcolumn=80,120`.
    pub(crate) fn show(&self, info: &Info) -> String {
        match self.get(info) {
            Value::Bool(true) => info.name.to_owned(),
            Value::Bool(false) => format!("no{}", info.name),
            Value::Number(number) => format!("{}={number}", info.name),
            Value::List(numbers) => {
                let numbers: Vec<String> = numbers.iter().map(u32::to_string).collect();
                format!("{}={}", info.name, numbers.join(","))
            }
            Value::Choice(word) => format!("{}={word}", info.name),
            Value::Text(text) => {
                let text = (text.replace('\\', "\\\\"))
                    .replace(' ', "\\ ")
                    .replace('\t', "\\\t");
                format!("{}={text}", info.name)
            }
        }
    }
}

/// How searches treat case, as `ignorecase` and `smartcase` say. An
/// editor's search and a terminal's in normal mode both go by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Case {
    ignore: bool,
    smart: bool,
}

impl Case {
    pub(crate) fn of(options: &Options) -> Self {
        Self {
            ignore: options.ignorecase,
            smart: options.smartcase,
        }
    }

    /// Whether a search for `query` matches case exactly: always without
    /// `ignorecase`, and with `smartcase`, when `query` has a capital.
    pub(crate) fn exact(self, query: &str) -> bool {
        !self.ignore || (self.smart && query.chars().any(char::is_uppercase))
    }

    /// Whether a search matches case whatever it's for, as `ignorecase`
    /// says, smart or not: `*`'s.
    pub(crate) fn exact_always(self) -> bool {
        !self.ignore
    }
}

impl Default for Case {
    fn default() -> Self {
        Self::of(&Options::default())
    }
}

/// One change, or question, a `:set` asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Setting {
    /// Set the option to this.
    To(&'static Info, Value),
    /// Turn a true-or-false option over: `nu!`, `invnu`.
    Flip(&'static Info),
    /// Say what it is: `nu?`, or a number's name alone.
    Show(&'static Info),
}

/// What the words after `:set` ask for, as Vim reads them: `number`,
/// `nonumber`, `number!` or `invnumber`, `number?`, `numberwidth=5`,
/// `colorcolumn=80,120`, `colorcolumn=` for none, and `showbreak=>\ ` for
/// text with a space.
pub(crate) fn parse_set(words: &str) -> Result<Vec<Setting>, String> {
    split_words(words)
        .iter()
        .map(|word| parse_setting(word))
        .collect()
}

/// `text` split at spaces and tabs, as Vim splits `:set`'s words: one
/// after a backslash is part of the word, and `\\` is one backslash.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.next() {
                Some(next @ (' ' | '\t' | '\\')) => word.push(next),
                Some(next) => word.extend(['\\', next]),
                None => word.push('\\'),
            },
            ' ' | '\t' => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            ch => word.push(ch),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn parse_setting(word: &str) -> Result<Setting, String> {
    // Only before the `=`: text set after it can have them, as `sbr=-=>`.
    if let Some((name, _)) = word.split_once('=')
        && name.ends_with(['+', '-', '^'])
    {
        return Err(format!(
            "can't read {word}: an option is set with =, and +=, -= and ^= aren't supported yet"
        ));
    }
    if let Some((name, value)) = word.split_once('=') {
        let info = find(name)?;
        return Ok(Setting::To(info, info.parse(value)?));
    }
    if let Some(name) = word.strip_suffix('?') {
        return Ok(Setting::Show(find(name)?));
    }
    // `nu!`, `nonu!` and `invnu` turn it over; `nonu` turns it off.
    let (name, flip) = match word.strip_suffix('!') {
        Some(name) => (name, true),
        None => match word.strip_prefix("inv") {
            Some(name) if find(name).is_ok() => (name, true),
            _ => (word, false),
        },
    };
    let (info, on) = match find(name) {
        Ok(info) => (info, true),
        Err(err) => match name.strip_prefix("no").map(find) {
            Some(Ok(info)) if info.kind == Kind::Bool => (info, false),
            _ => return Err(err),
        },
    };
    match info.kind {
        Kind::Bool if flip => Ok(Setting::Flip(info)),
        Kind::Bool => Ok(Setting::To(info, Value::Bool(on))),
        _ if flip => Err(format!(
            "{} isn't true or false, so can't be turned over",
            info.name
        )),
        _ => Ok(Setting::Show(info)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str) -> &'static Info {
        find(name).unwrap()
    }

    #[test]
    fn options_are_found_by_name_or_short_name() {
        assert_eq!(info("nu").name, "number");
        assert_eq!(info("relativenumber").short, "rnu");
        let err = find("spell").unwrap_err();
        assert!(
            err.contains("`spell`") && err.contains("number, relativenumber"),
            "{err}"
        );
    }

    #[test]
    fn values_are_checked_and_changes_noticed() {
        let mut options = Options::default();
        assert_eq!(options.set(info("nu"), Value::Bool(false)), Ok(true));
        assert!(!options.number);
        assert_eq!(options.set(info("nu"), Value::Bool(false)), Ok(false));
        assert_eq!(options.set(info("nuw"), Value::Number(6)), Ok(true));
        assert_eq!(options.numberwidth, 6);
        assert_eq!(
            options.set(info("nuw"), Value::Number(0)),
            Err("numberwidth is a whole number from 1 to 20".into())
        );
        assert_eq!(
            options.set(info("rnu"), Value::Number(1)),
            Err("relativenumber is true or false".into())
        );
        assert_eq!(options.show(info("number")), "nonumber");
        assert_eq!(options.show(info("numberwidth")), "numberwidth=6");
    }

    #[test]
    fn a_list_holds_any_number_of_numbers() {
        let mut options = Options::default();
        let cc = info("cc");
        assert_eq!(options.show(cc), "colorcolumn=");
        assert_eq!(options.set(cc, Value::List(vec![80, 120])), Ok(true));
        assert_eq!(options.colorcolumn, [80, 120]);
        assert_eq!(options.show(cc), "colorcolumn=80,120");
        assert_eq!(
            options.set(cc, Value::List(vec![80, 0])),
            Err("colorcolumn is whole numbers from 1 to 9999, with commas between them".into())
        );
        assert_eq!(cc.parse("100"), Ok(Value::List(vec![100])));
        assert_eq!(cc.parse(""), Ok(Value::List(Vec::new())));
        for text in ["80,", "80 120", "x", "0"] {
            assert!(cc.parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn a_choice_is_one_of_its_words() {
        let mut options = Options::default();
        let li = info("li");
        assert_eq!(options.show(li), "languageinstall=ask");
        assert_eq!(li.parse("never"), Ok(Value::Choice("never")));
        assert_eq!(options.set(li, Value::Choice("never")), Ok(true));
        assert_eq!(options.languageinstall, LanguageInstall::Never);
        assert_eq!(
            li.parse("sometimes"),
            Err("languageinstall is ask, always or never".into())
        );
    }

    #[test]
    fn text_is_any_but_control_characters_and_set_escapes_its_blanks() {
        let mut options = Options::default();
        let sbr = info("sbr");
        assert_eq!(options.show(sbr), "showbreak=");
        assert_eq!(options.set(sbr, Value::Text("↪ ".into())), Ok(true));
        assert_eq!(options.showbreak, "↪ ");
        assert_eq!(options.show(sbr), "showbreak=↪\\ ");
        assert_eq!(
            options.set(sbr, Value::Text("a\tb".into())),
            Err("showbreak is text, without control characters".into())
        );
        assert_eq!(
            parse_set("sbr=>\\ \\\\ nowrap sbr= sbr=a\\b"),
            Ok(vec![
                Setting::To(sbr, Value::Text("> \\".into())),
                Setting::To(info("wrap"), Value::Bool(false)),
                Setting::To(sbr, Value::Text(String::new())),
                Setting::To(sbr, Value::Text("a\\b".into())),
            ])
        );
        assert!(
            parse_set("sbr!")
                .unwrap_err()
                .contains("can't be turned over")
        );
        // += and its kin are refused only before the =, and only spaces and
        // tabs split words.
        assert_eq!(
            parse_set("sbr=-=> sbr=a\u{a0}b"),
            Ok(vec![
                Setting::To(sbr, Value::Text("-=>".into())),
                Setting::To(sbr, Value::Text("a\u{a0}b".into())),
            ])
        );
        let err = parse_set("sbr+=x").unwrap_err();
        assert!(err.contains("aren't supported"), "{err}");
    }

    #[test]
    fn set_reads_as_vims_does() {
        let (nu, rnu, nuw, cc) = (info("nu"), info("rnu"), info("nuw"), info("cc"));
        assert_eq!(
            parse_set("nonu rnu! invnumber nonu! nuw=7 nuw nu? cc=80,120 cc= cc"),
            Ok(vec![
                Setting::To(nu, Value::Bool(false)),
                Setting::Flip(rnu),
                Setting::Flip(nu),
                Setting::Flip(nu),
                Setting::To(nuw, Value::Number(7)),
                Setting::Show(nuw),
                Setting::Show(nu),
                Setting::To(cc, Value::List(vec![80, 120])),
                Setting::To(cc, Value::List(Vec::new())),
                Setting::Show(cc),
            ])
        );
        for (words, wanted) in [
            ("nuw=50", "from 1 to 20"),
            ("nuw=x", "from 1 to 20"),
            ("nu=1", "set with number or nonumber"),
            ("nonuw", "no option called `nonuw`"),
            ("nuw!", "can't be turned over"),
            ("cc!", "can't be turned over"),
            ("nocc", "no option called `nocc`"),
            ("cc=80,x", "with commas between them"),
            ("nospell", "no option called `nospell`"),
            ("nuw+=1", "+=, -= and ^= aren't supported"),
        ] {
            let err = parse_set(words).unwrap_err();
            assert!(err.contains(wanted), "{words}: {err}");
        }
    }

    #[test]
    fn case_is_matched_as_ignorecase_and_smartcase_say() {
        let case = |ignorecase, smartcase| {
            Case::of(&Options {
                ignorecase,
                smartcase,
                ..Options::default()
            })
        };
        assert_eq!(Case::default(), case(true, true));
        for (ic, scs, exact_lower, exact_upper) in [
            (true, true, false, true),
            (true, false, false, false),
            // As in Vim, smartcase does nothing without ignorecase.
            (false, true, true, true),
            (false, false, true, true),
        ] {
            assert_eq!(case(ic, scs).exact("word"), exact_lower, "{ic} {scs}");
            assert_eq!(case(ic, scs).exact("Word"), exact_upper, "{ic} {scs}");
        }
    }
}
