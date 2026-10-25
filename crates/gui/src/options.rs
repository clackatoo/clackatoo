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
    /// A number with a fraction, as an option of [`Kind::Decimal`] holds.
    Decimal(Decimal),
    List(List),
    /// One of the words an option of [`Kind::Choice`] can be.
    Choice(&'static str),
    /// Text, as an option of [`Kind::Text`] holds.
    Text(String),
}

/// Whole numbers, as an option holds several: `colorcolumn`'s columns.
pub(crate) type List = Vec<u32>;

/// A number to two decimal places, as `fontsize`'s 10.5. It's kept in
/// hundredths, so options still compare exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Decimal(u32);

impl Decimal {
    /// The whole number `number`.
    pub(crate) const fn whole(number: u32) -> Self {
        Self(number.saturating_mul(100))
    }

    /// `number`, if it's one: not negative, and to two decimal places at
    /// most. A float only near one, as 10.1 is, counts.
    pub(crate) fn exact(number: f64) -> Option<Self> {
        let hundredths = (number * 100.0).round();
        let exact = (number * 100.0 - hundredths).abs() < 1e-6;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        (exact && (0.0..=f64::from(u32::MAX)).contains(&hundredths))
            .then_some(Self(hundredths as u32))
    }

    /// The nearest to `number` from `min` to `max`, to two decimal places,
    /// for a number from elsewhere, as the command line, that the option
    /// must still be able to hold.
    pub(crate) fn nearest(number: f32, min: u32, max: u32) -> Self {
        if number.is_nan() {
            return Self::whole(min);
        }
        let (min, max) = (Self::whole(min), Self::whole(max));
        let hundredths = (f64::from(number) * 100.0).round();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self(hundredths.clamp(f64::from(min.0), f64::from(max.0)) as u32)
    }

    /// The number, as Lua has it.
    pub(crate) fn as_f64(self) -> f64 {
        f64::from(self.0) / 100.0
    }

    /// The number, as fonts are sized in.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn as_f32(self) -> f32 {
        self.as_f64() as f32
    }

    /// Reads `text` as `:set` gives one: `12`, `10.5` or `10.25`.
    fn parse(text: &str) -> Option<Self> {
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
        if whole.is_empty() || !digits(whole) || !digits(fraction) || fraction.len() > 2 {
            return None;
        }
        let whole: u32 = whole.parse().ok()?;
        // `.5` is 50 hundredths, and `.05` five.
        let fraction: u32 = format!("{fraction:0<2}").parse().ok()?;
        whole.checked_mul(100)?.checked_add(fraction).map(Self)
    }
}

impl std::fmt::Display for Decimal {
    /// As few decimal places as it needs: `12`, `10.5`, `10.25`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (whole, fraction) = (self.0 / 100, self.0 % 100);
        match fraction {
            0 => write!(f, "{whole}"),
            _ if fraction % 10 == 0 => write!(f, "{whole}.{}", fraction / 10),
            _ => write!(f, "{whole}.{fraction:02}"),
        }
    }
}

/// What an option holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Bool,
    /// A whole number from `min` to `max`, or 0 too if `or_zero`, which
    /// means the window's own choice, as `explorerwidth`'s does.
    Number {
        min: u32,
        max: u32,
        or_zero: bool,
    },
    /// A number from `min` to `max`, to two decimal places.
    Decimal {
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
    /// Short help shown in command completion.
    pub(crate) description: &'static str,
}

impl Info {
    /// Checks `value` is one this option can hold, saying why not.
    fn check(&self, value: &Value) -> Result<(), String> {
        match (self.kind, value) {
            (Kind::Bool, Value::Bool(_)) => Ok(()),
            (Kind::Number { min, max, or_zero }, Value::Number(number))
                if (min..=max).contains(number) || (or_zero && *number == 0) =>
            {
                Ok(())
            }
            (Kind::Decimal { min, max }, Value::Decimal(number))
                if (Decimal::whole(min)..=Decimal::whole(max)).contains(number) =>
            {
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
    /// `=`: a number, one with a fraction for a decimal, or for a list,
    /// numbers with commas between them, or for text, the text.
    pub(crate) fn parse(&self, text: &str) -> Result<Value, String> {
        let number = |text: &str| text.parse::<u32>().map_err(|_| self.wants());
        let value = match self.kind {
            Kind::Bool => return Err(format!("{} is set with {0} or no{0}, not =", self.name)),
            Kind::Number { .. } => Value::Number(number(text)?),
            Kind::Decimal { .. } => {
                Value::Decimal(Decimal::parse(text).ok_or_else(|| self.wants())?)
            }
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
            Kind::Number {
                min,
                max,
                or_zero: true,
            } => format!("{} is 0, or a whole number from {min} to {max}", self.name),
            Kind::Number { min, max, .. } => {
                format!("{} is a whole number from {min} to {max}", self.name)
            }
            Kind::Decimal { min, max } => format!(
                "{} is a number from {min} to {max}, to two decimal places at most",
                self.name
            ),
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

/// A whole number that may be 0 as well as in its range, which means the
/// window's own choice: an option of this type is a [`Kind::Number`] with
/// `or_zero`.
pub(crate) type OrZero = u32;

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

impl Held for Decimal {
    fn value(&self) -> Value {
        Value::Decimal(*self)
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Decimal(value) => Some(value),
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

/// The [`Kind`] of an option of type `bool`, or `u32`, [`Decimal`] or
/// [`List`] with its range, `String`, or a [`Choice`].
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
            or_zero: false,
        }
    };
    (OrZero $min:literal ..= $max:literal) => {
        Kind::Number {
            min: $min,
            max: $max,
            or_zero: true,
        }
    };
    (Decimal $min:literal ..= $max:literal) => {
        Kind::Decimal {
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
        $name:ident ($short:literal, $description:literal): $type:ident $(($min:literal ..= $max:literal))? = $default:expr;
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
                description: $description,
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
    number("nu", "Show line numbers"): bool = true;
    /// Each line's number as how far it is from the cursor's line, so the
    /// count for a motion such as `5j` can be read off. With `number`,
    /// the cursor's line shows its own number; without, 0.
    relativenumber("rnu", "Show line numbers relative to the cursor"): bool = true;
    /// The fewest cells the line numbers take, with the space after them.
    numberwidth("nuw", "Minimum width of the line-number column"): u32 (1..=20) = 4;
    /// The cursor's line lit faintly, across the pane, so the eye finds it.
    cursorline("cul", "Highlight the cursor’s line"): bool = true;
    /// Columns marked down an editor, such as 80, to keep lines short by.
    /// They count from 1, as the bar's column does.
    colorcolumn("cc", "Mark guide columns in the editor"): List (1..=9999) = List::new();
    /// What's wrong with a line, as its language server says, written
    /// after the line's end: the worst, if there are several.
    virtualtext("vt", "Show diagnostics beside editor lines"): bool = true;
    /// A column of signs left of the line numbers, saying which lines are
    /// added, changed or deleted since the file's text in git's index, as
    /// gitsigns.nvim shows them. Kept for every file while it's on, so the
    /// text doesn't move when the first change comes.
    gitsigns("gs", "Show Git changes in the gutter"): bool = true;
    /// A column of its own, right of git's signs, colouring the lines an
    /// agent changed that you haven't reviewed, in a colour for each
    /// chat. It shows only while a file has such lines, or `:blame` is
    /// on.
    agentsigns("as", "Mark unreviewed agent changes in the gutter"): bool = true;
    /// How wide a file explorer down the tab's left opens, in cells, as
    /// `<Space>e` and `:Explore` open it. It keeps its share of the tab as
    /// the window resizes. 0, as it starts, opens it at 30 cells, or a
    /// quarter of the tab if that's narrower, but never under 10.
    explorerwidth("ew", "Initial width of the file explorer"): OrZero (10..=200) = 0;
    /// File explorers show hidden files, those whose names start with `.`,
    /// as they open. `.` shows or hides them in each.
    explorerhidden("eh", "Show hidden files in new explorers"): bool = false;
    /// The fewest lines kept in view above and below the cursor, so what's
    /// around it shows as it moves. Half the pane or more keeps the
    /// cursor's line in the middle.
    scrolloff("so", "Lines kept visible above and below the cursor"): u32 (0..=9999) = 5;
    /// Ease mouse-wheel scrolling in native panes. Trackpad pixels stay direct.
    smoothscroll("sms", "Animate mouse-wheel scrolling"): bool = true;
    /// A line too long for the pane goes on over as many rows as it needs,
    /// rather than off the pane's right edge. `j` and `k` still move by
    /// line, and `gj` and `gk` by row.
    wrap("wrap", "Wrap long lines to fit the pane"): bool = true;
    /// With `wrap`, a line is broken between words, after a blank or one
    /// of `!@*-+;:,./?`, rather than at the last cell that fits.
    linebreak("lbr", "Wrap at word boundaries"): bool = true;
    /// With `wrap`, the rows a line goes on over start as far in as the
    /// line is indented, so the code's shape still shows. At least 20
    /// cells are kept for the text.
    breakindent("bri", "Preserve indentation on wrapped rows"): bool = true;
    /// With `wrap`, drawn at the start of the rows a line goes on over,
    /// after their indent: `↪ `, say. In `:set`, a space is written `\ `.
    showbreak("sbr", "Text marking the start of wrapped rows"): String = String::new();
    /// How wide a tab is drawn: to the next column that's a multiple of it.
    tabstop("ts", "Display width of a tab"): u32 (1..=32) = 4;
    /// The columns `>>`, `<<` and Tab indent by. 0 means `tabstop`'s, so
    /// the two can't drift apart.
    shiftwidth("sw", "Number of columns per indentation step"): u32 (0..=32) = 0;
    /// Indent with spaces, not tabs, where no line nearby is indented yet.
    /// A file keeps the kind of indent it has.
    expandtab("et", "Indent new code with spaces"): bool = true;
    /// A new line, one where `}`, `]` or `)` is typed first, and one where
    /// `:` ends `else`, `elif`, `except`, `finally`, `case` or `default`,
    /// is indented as the file's language says: by the rules of its
    /// grammar's `indents.scm`, as nvim-treesitter's are, not Vim's rules
    /// for C. It takes the indent of the line before at once, and is put
    /// right a moment later, unless its indent has been typed in, or
    /// another change made meanwhile. Off, a new line keeps the line
    /// before's. `=` indents by the rules either way.
    smartindent("si", "Indent code using its language’s rules"): bool = true;
    /// Searches ignore case...
    ignorecase("ic", "Ignore letter case when searching"): bool = true;
    /// ...unless, with `ignorecase`, what's searched for has a capital.
    smartcase("scs", "Match case when the search contains capitals"): bool = true;
    /// While `:s` is typed, show what it would do, in place, as Neovim
    /// does; and the matches of `:g`'s pattern. Nothing changes until
    /// Enter, and Esc puts the view back. `none` shows nothing.
    inccommand("icm", "Preview substitutions while typing"): InCommand = InCommand::NoSplit;
    /// `~` is an operator, as `g~` is, taking a motion: `~w`. Off, it
    /// turns over the case of the character under the cursor, and moves
    /// on past it.
    tildeop("top", "Make ~ take a motion like g~"): bool = false;
    /// `gg`, `G` and Ctrl+Home, `H`, `M` and `L`, the paging keys, and in
    /// an editor `:` and a line number and deleting, shifting or
    /// indenting whole lines, go to the line's first character that isn't
    /// blank, as Vim's do. Off, as Neovim's are, they keep the column, as
    /// `j` and `k` do.
    startofline("sol", "Move to the first non-blank character on line jumps"): bool = false;
    /// `:w` formats the file first, as its language server does.
    formatonsave("fos", "Format files before saving"): bool = false;
    /// A file changed on disk by something other than an agent, as a git
    /// checkout or another editor changes one, is read again, if its
    /// buffer has no changes not saved: as an edit, which `u` undoes. Off,
    /// or with changes, the bar says so, and `:e!` takes the file's
    /// version, `:w!` keeps yours. Agents' changes are taken either way.
    autoread("ar", "Reload unchanged buffers when their files change"): bool = true;
    /// What's done when a file is opened in a language that isn't
    /// installed, or whose language server isn't.
    languageinstall("li", "How missing language support is installed"): LanguageInstall = LanguageInstall::Ask;
    /// Type through the desktop's input method, as Chinese, Japanese and
    /// Korean are typed, and as an emoji picker sends its emoji, while keys
    /// type text. Off, keys go straight to the pane, in case the input
    /// method gets in the way.
    ime("ime", "Enable the desktop input method"): bool = true;
    /// As a word is typed in an editor, or a character its language server
    /// says should ask, such as `.`, ask the server what could be typed
    /// and show it in a menu under the cursor. Off, Ctrl+Space, Ctrl+N and
    /// Ctrl+P still ask.
    completion("cmp", "Suggest code as you type in editors"): bool = true;
    /// How long typing a word pauses, in milliseconds, before the language
    /// server is asked what could finish it, so a burst of keys asks once.
    /// 0 asks at every key.
    completedelay("cmpd", "Delay before asking for code completion, in ms"): u32 (0..=2000) = 40;
    /// How long keys that start others wait, in milliseconds, before a
    /// panel lists what can come next, as which-key.nvim's does, so keys
    /// typed quickly, as `<Space>pf`, never flash it. Keys held with Ctrl
    /// or Alt, as Ctrl+B, show it at once, as the window's own prefixes;
    /// the bar marks the keys waiting at once either way. 0 shows it at
    /// once for every key.
    whichkeydelay("wkd", "Delay before showing keybinding hints, in ms"): u32 (0..=5000) = 400;
    /// Which keys accept the item picked in the completion menu, besides
    /// Ctrl+Y, which always does: Enter, once an item has been moved to
    /// with Ctrl+N, Ctrl+P or the arrows; Enter, always; or Ctrl+Y alone,
    /// Enter always making a new line.
    completeaccept("cmpa", "Keys that accept an editor completion"): CompleteAccept = CompleteAccept::Picked;
    /// Tab accepts the item picked in the completion menu too, as in VS
    /// Code, rather than indenting.
    completetab("cmpt", "Accept editor completions with Tab"): bool = false;
    /// While a call's arguments are typed in an editor, its signature shows
    /// over the cursor, with the argument being typed in bold, as the
    /// language server says.
    signaturehelp("sig", "Show function signatures while typing arguments"): bool = true;
    /// Once the cursor rests on a name in an editor in normal mode, the
    /// other places it's used in the file are lit, as its language server
    /// says: where it's written a little brighter than where it's read.
    highlightuses("hlu", "Highlight uses of the name under the cursor"): bool = true;
    /// What the language server would have written inline between a
    /// file's text is drawn there, muted: the types of names bound without
    /// one written, and the names of the parameters arguments are for.
    /// They're never part of the text: the cursor, motions, searches and
    /// yanks pass them by. `<Space>th` toggles it.
    inlayhints("ih", "Show type and parameter hints in editors"): bool = true;
    /// How a Markdown file opens in an editor: as its text (`raw`), or
    /// rendered, as prose, with its headings, lists, tables and code drawn
    /// as such (`rendered`). `:markdown` and `<Space>tm` turn each editor
    /// from one to the other.
    markdownview("mdv", "How Markdown files open"): MarkdownView = MarkdownView::Raw;
    /// Which of the desktop's the unnamed register goes along with, as in
    /// Neovim: the clipboard, so what's yanked or deleted in an editor can
    /// be pasted anywhere, and `p` puts what was copied anywhere; the
    /// primary selection, which a middle click pastes; or neither, the
    /// registers being the window's own but for `"+` and `"*`.
    clipboard("cb", "Clipboard used by unnamed editor registers"): Clipboard = Clipboard::UnnamedPlus;
    /// What programs in terminals may do with the clipboard, through OSC
    /// 52, as Neovim and tmux do, even over ssh: put text on it (`copy`),
    /// read what's there (`paste`), both, or neither. Reading is off
    /// unless asked for, since the clipboard may hold a password, and
    /// whatever runs in a terminal would get it.
    osc52("osc52", "Clipboard access allowed to terminal programs"): Osc52 = Osc52::Copy;
    /// The agent `:Agent` and Ctrl+B `a` open a chat with, by its name:
    /// `claude`, `codex`, `gemini`, or one the daemon's `agents.toml`
    /// adds. Empty is the daemon's default, Claude Code.
    agent("ag", "Default provider for new agent chats"): String = "claude".to_owned();
    /// What a new chat's agent may do with clackatoo's own tools, which
    /// show you things beside the chat: open files, run commands in
    /// terminals you can watch, and read them. `ask` asks in the chat
    /// before it runs a command, types into a terminal, or shows what's
    /// outside the chat's project; `allow` runs commands in its own
    /// terminals without asking, but asks the rest; `off` gives it no
    /// tools. The chat's Tools setting changes it later.
    agenttools("agt", "Default tool permissions for new chats"): AgentTools = AgentTools::Ask;
    /// A new chat with Claude Code keeps Claude Code's own Task tool,
    /// which runs helpers you don't see inside one turn. Off, as it is
    /// unless you turn it on, Claude hands work on as every agent does,
    /// with clackatoo's `start_chat` and `start_chats`, in chats you see
    /// as cards and can open, steer and undo. Other agents have no Task
    /// tool.
    claudetask("ct", "Allow Claude’s internal Task tool in new chats"): bool = false;
    /// A new chat starts with its Browser setting on: its agent gets tools
    /// that drive a headless browser of clackatoo's, with a profile of its
    /// own, never yours, and asks before its first page. Off, as it is
    /// unless you turn it on, a chat gets no browser until you turn it on
    /// in the chat's settings.
    chatbrowser("cb", "Enable browser tools in new chats"): bool = false;
    /// About the most of a conversation, in tokens, a chat tells an agent
    /// it moves to, as the model menu moves it from Claude Code to Codex,
    /// say: the newest turns whole, older ones a line each. An agent it
    /// goes back to hears only what happened since, as briefly.
    carryover("co", "Token budget for context handed to another provider"): u32 (1000..=200_000) = 20_000;
    /// Look for a new release of clackatoo, at most once a day, a while
    /// after the window opens, and say in the bar when one is out. It's
    /// never installed until `:update` or `clackatoo update` asks.
    updatecheck("uc", "Check for new Clackatoo releases"): bool = true;
    /// What's typed in a terminal shows at once, underlined, before the
    /// daemon's screen shows the program's echo, as mosh shows it, so
    /// typing over a slow link doesn't wait for the round trip: `auto`
    /// does this only for a daemon on another machine, while its round
    /// trip is over 30 ms or a key has just taken a long time; `always`
    /// does it even for a daemon on this machine; `never` doesn't.
    predictecho("pe", "When to predict typing over remote connections"): PredictEcho = PredictEcho::Auto;
    /// Which commands a terminal marks at the end of their prompt's line,
    /// once they've finished, where the shell says how they ended:
    /// `failed` marks those that failed with a dim red `✗` and their exit
    /// status, as `✗ 2`; `all` marks those that worked too, with a dim
    /// `✓`; `none` marks none.
    blockmarks("bm", "Which finished shell commands receive status marks"): BlockMarks = BlockMarks::Failed;
    /// When a workspace from another machine, one `:connect` brought in,
    /// says which machine it's on after its name, as `notes@desk`:
    /// `ambiguous` only while another workspace has the same name,
    /// `always` for every one, `never` for none. Its ring, in place of the
    /// dot, says it's from another machine whatever this says, and the
    /// mouse over it says which, and how its connection is.
    showhost("shh", "When workspace names include their machine"): ShowHost = ShowHost::Ambiguous;
    /// Ports forwarded to a daemon on another machine (`:portforward`)
    /// send as background traffic: only as fast as the link takes them
    /// without a queue growing, so keys and screens never wait behind a
    /// download. Off, they go as fast as they can, taking the whole of a
    /// slow link at the cost of keys waiting in its queue: worth it on a
    /// link that's slower whenever it's busy, as some Wi-Fi is, where
    /// background traffic gets little. A change counts for connections
    /// made after; what the window sends changes at once.
    forwardpace("fp", "Pace forwarded traffic to keep remote input responsive"): bool = true;
    /// The font family terminals and editors are drawn in, by its name, as
    /// `fc-list : family` lists them: `JetBrains Mono`, say. Empty is your
    /// system's monospace font. In `:set`, a space is written `\ `. A
    /// family that isn't installed is refused, and the font stays as it
    /// was. `--font` or `$CLACKATOO_FONT` overrides what `init.lua` sets
    /// as it runs, but not what's set after, by `:set` or a key it binds.
    font("gfn", "Font family for terminals and editors"): String = String::new();
    /// The size text is drawn at, in points, as 12 or 10.5. Changing it,
    /// or `font`, keeps the window's size, and fits as many cells in as
    /// now fit, as resizing the window does. `--font-size` or
    /// `$CLACKATOO_FONT_SIZE` overrides what `init.lua` sets as it runs,
    /// but not what's set after.
    fontsize("fsz", "Font size in points"): Decimal (4..=200) = Decimal::whole(12);
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
    /// When `showhost` names a workspace's machine after its name.
    ShowHost {
        /// Only while another workspace has the same name.
        Ambiguous = "ambiguous",
        /// For every workspace from another machine.
        Always = "always",
        /// Never.
        Never = "never",
    }
}

choice! {
    /// Which finished commands `blockmarks` marks.
    BlockMarks {
        /// Those that failed.
        Failed = "failed",
        /// Every one, those that worked too.
        All = "all",
        /// None.
        Off = "none",
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
    /// How `markdownview` says a Markdown file opens.
    MarkdownView {
        /// As its text.
        Raw = "raw",
        /// Rendered, as prose.
        Rendered = "rendered",
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

/// The option called `name`, or by its short name; or why there's none,
/// with the name nearest it if it looks like a slip. Tab lists them all,
/// so the error doesn't.
pub(crate) fn find(name: &str) -> Result<&'static Info, String> {
    lookup(name).ok_or_else(|| match nearest_name(name, false) {
        Some(near) => format!("no option ‘{name}’: did you mean ‘{near}’?"),
        None => format!("no option ‘{name}’"),
    })
}

fn lookup(name: &str) -> Option<&'static Info> {
    ALL.iter()
        .find(|info| info.name == name || info.short == name)
}

/// The name of an option, or its short name, nearest `name`, of those
/// that are true or false if `bools`.
fn nearest_name(name: &str, bools: bool) -> Option<&'static str> {
    let names = (ALL.iter())
        .filter(|info| !bools || info.kind == Kind::Bool)
        .flat_map(|info| [info.name, info.short])
        .filter(|name| !name.is_empty());
    crate::cmdline::nearest::nearest(name, names)
}

/// Why `word`, set with `:set`, means nothing, as `name`, the part of it
/// naming the option, names none: with the word as it was probably meant,
/// if it looks like a slip, as `:set nonumbr` for `:set nonumber`.
fn unknown(word: &str, name: &str) -> String {
    let near = nearest_name(name, false).map(str::to_owned).or_else(|| {
        // Turned off, or over, as only what's true or false can be.
        let (prefix, rest) = (name.strip_prefix("no").map(|rest| ("no", rest)))
            .or_else(|| name.strip_prefix("inv").map(|rest| ("inv", rest)))?;
        nearest_name(rest, true).map(|near| format!("{prefix}{near}"))
    });
    match near {
        Some(near) => format!(
            "no option ‘{name}’: did you mean ‘:set {}’?",
            word.replacen(name, &near, 1)
        ),
        None => format!("no option ‘{name}’"),
    }
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
            Value::Decimal(number) => format!("{}={number}", info.name),
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

impl Options {
    /// The options as a window starts, drawing text as `font` says: their
    /// defaults, but for `font` and `fontsize`. A size `fontsize` can't
    /// hold, as the command line can give, is brought into its range.
    pub(crate) fn drawing_with(font: &crate::FontOptions) -> Self {
        let Ok(Info {
            kind: Kind::Decimal { min, max },
            ..
        }) = find("fontsize")
        else {
            unreachable!("fontsize is a decimal")
        };
        Self {
            font: font.family.clone().unwrap_or_default(),
            fontsize: Decimal::nearest(font.size, *min, *max),
            ..Self::default()
        }
    }

    /// What to draw text with, as `font` and `fontsize` say.
    pub(crate) fn font_options(&self) -> crate::FontOptions {
        crate::FontOptions {
            family: Some(self.font.clone()).filter(|family| !family.is_empty()),
            size: self.fontsize.as_f32(),
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
    let find = |name| lookup(name).ok_or_else(|| unknown(word, name));
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
            Some(name) if lookup(name).is_some() => (name, true),
            _ => (word, false),
        },
    };
    let (info, on) = match lookup(name) {
        Some(info) => (info, true),
        None => match name.strip_prefix("no").and_then(lookup) {
            Some(info) if info.kind == Kind::Bool => (info, false),
            Some(info) => {
                return Err(format!(
                    "{} isn't true or false, so can't be turned off",
                    info.name
                ));
            }
            None => return Err(unknown(word, name)),
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
        // In a clause, with no list of them all, which Tab has.
        assert_eq!(find("spell").unwrap_err(), "no option ‘spell’");
        assert_eq!(
            find("numbr").unwrap_err(),
            "no option ‘numbr’: did you mean ‘number’?"
        );
    }

    #[test]
    fn a_slip_in_set_offers_the_setting_meant() {
        for (words, said) in [
            ("nosuchoption", "no option ‘nosuchoption’"),
            ("numbr", "no option ‘numbr’: did you mean ‘:set number’?"),
            (
                "nonumbr",
                "no option ‘nonumbr’: did you mean ‘:set nonumber’?",
            ),
            (
                "invnumbr",
                "no option ‘invnumbr’: did you mean ‘:set invnumber’?",
            ),
            ("numbr!", "no option ‘numbr’: did you mean ‘:set number!’?"),
            ("numbr?", "no option ‘numbr’: did you mean ‘:set number?’?"),
            (
                "numberwidht=3",
                "no option ‘numberwidht’: did you mean ‘:set numberwidth=3’?",
            ),
            // Only what's true or false is offered turned off.
            ("nonumberwidht", "no option ‘nonumberwidht’"),
            (
                "nonuw",
                "numberwidth isn't true or false, so can't be turned off",
            ),
        ] {
            assert_eq!(parse_set(words).unwrap_err(), said, "{words}");
        }
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
        // 0, the window's choice, or one in the range, but nothing between.
        assert_eq!(options.set(info("ew"), Value::Number(30)), Ok(true));
        assert_eq!(options.set(info("ew"), Value::Number(0)), Ok(true));
        assert_eq!(
            options.set(info("ew"), Value::Number(5)),
            Err("explorerwidth is 0, or a whole number from 10 to 200".into())
        );
        assert_eq!(options.explorerwidth, 0);
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
    fn a_decimal_is_kept_to_two_places_and_written_with_as_few_as_it_needs() {
        let mut options = Options::default();
        let fsz = info("fsz");
        assert_eq!(fsz.name, "fontsize");
        assert_eq!(options.show(fsz), "fontsize=12");
        for (text, shown) in [
            ("10.5", "10.5"),
            ("10.25", "10.25"),
            ("10.05", "10.05"),
            ("10.50", "10.5"),
            ("13.", "13"),
            ("200", "200"),
            ("4", "4"),
        ] {
            let value = fsz.parse(text).unwrap();
            options.set(fsz, value).unwrap();
            assert_eq!(options.show(fsz), format!("fontsize={shown}"), "{text}");
        }
        let wants = "fontsize is a number from 4 to 200, to two decimal places at most";
        for text in [
            "3.99", "200.01", "10.125", "", ".5", "-1", "1e2", "ten", "1,5", "+5",
        ] {
            assert_eq!(fsz.parse(text), Err(wants.into()), "{text}");
        }
        assert_eq!(
            options.set(fsz, Value::Number(12)),
            Err(wants.into()),
            "a whole number isn't a decimal"
        );
        assert_eq!(Decimal::exact(10.1), Some(Decimal(1010)));
        assert_eq!(Decimal::exact(10.125), None);
        assert_eq!(Decimal::exact(-1.0), None);
        assert_eq!(Decimal::exact(f64::NAN), None);
        assert_eq!(Decimal::whole(u32::MAX), Decimal(u32::MAX));
    }

    #[test]
    fn the_font_is_text_and_starts_as_the_window_was_told() {
        let mut options = Options::default();
        let gfn = info("gfn");
        assert_eq!(gfn.name, "font");
        assert_eq!(options.show(gfn), "font=");
        assert_eq!(
            parse_set("gfn=JetBrains\\ Mono fsz=10.5"),
            Ok(vec![
                Setting::To(gfn, Value::Text("JetBrains Mono".into())),
                Setting::To(info("fontsize"), Value::Decimal(Decimal(1050))),
            ])
        );
        options
            .set(gfn, Value::Text("JetBrains Mono".into()))
            .unwrap();
        assert_eq!(options.show(gfn), "font=JetBrains\\ Mono");

        let font = |family: Option<&str>, size| crate::FontOptions {
            family: family.map(str::to_owned),
            size,
        };
        let start = Options::drawing_with(&font(Some("Iosevka"), 13.5));
        assert_eq!(
            (start.font.as_str(), start.fontsize),
            ("Iosevka", Decimal(1350))
        );
        let back = start.font_options();
        assert_eq!((back.family.as_deref(), back.size), (Some("Iosevka"), 13.5));
        // The system's font is no family; a size from the command line is
        // brought into range.
        let start = Options::drawing_with(&font(None, 1000.0));
        assert_eq!(
            (start.font.as_str(), start.fontsize),
            ("", Decimal::whole(200))
        );
        assert_eq!(start.font_options().family, None);
        for (size, kept) in [(0.5, 400), (f32::NAN, 400), (10.333, 1033)] {
            let start = Options::drawing_with(&font(None, size));
            assert_eq!(start.fontsize, Decimal(kept), "{size}");
        }
        assert_eq!(
            Options::drawing_with(&crate::FontOptions::default()),
            Options::default()
        );
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
            ("nonuw", "numberwidth isn't true or false"),
            ("nuw!", "can't be turned over"),
            ("cc!", "can't be turned over"),
            ("nocc", "colorcolumn isn't true or false"),
            ("cc=80,x", "with commas between them"),
            ("nospell", "no option ‘nospell’"),
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
