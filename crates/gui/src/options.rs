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
            _ => Err(self.wants()),
        }
    }

    /// Reads `text` as a value for this option, as `:set` gives one after
    /// `=`: a number, or for a list, numbers with commas between them.
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
/// its range, or a [`Choice`].
macro_rules! kind {
    (bool) => {
        Kind::Bool
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
    /// The fewest lines kept in view above and below the cursor, so what's
    /// around it shows as it moves. Half the pane or more keeps the
    /// cursor's line in the middle.
    scrolloff("so"): u32 (0..=9999) = 5;
    /// How wide a tab is drawn: to the next column that's a multiple of it.
    tabstop("ts"): u32 (1..=32) = 4;
    /// The columns `>>`, `<<` and Tab indent by. 0 means `tabstop`'s, so
    /// the two can't drift apart.
    shiftwidth("sw"): u32 (0..=32) = 0;
    /// Indent with spaces, not tabs, where no line nearby is indented yet.
    /// A file keeps the kind of indent it has.
    expandtab("et"): bool = true;
    /// Searches ignore case...
    ignorecase("ic"): bool = true;
    /// ...unless, with `ignorecase`, what's searched for has a capital.
    smartcase("scs"): bool = true;
    /// `:w` formats the file first, as its language server does.
    formatonsave("fos"): bool = false;
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
/// `colorcolumn=80,120`, and `colorcolumn=` for none.
pub(crate) fn parse_set(words: &str) -> Result<Vec<Setting>, String> {
    words.split_whitespace().map(parse_setting).collect()
}

fn parse_setting(word: &str) -> Result<Setting, String> {
    if ["+=", "-=", "^="].iter().any(|sign| word.contains(sign)) {
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
        let err = find("wrap").unwrap_err();
        assert!(
            err.contains("`wrap`") && err.contains("number, relativenumber"),
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
            ("nowrap", "no option called `nowrap`"),
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
