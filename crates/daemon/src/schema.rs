//! The schema options are declared in: what an option holds, how a value
//! is checked and read, where it's listed and where it's kept. The
//! window's options and the daemon's are declared in it alike, each where
//! they're read (the window's in `clackatoo_gui`'s `options`, the
//! daemon's in [`crate::settings`]), so `:set`, the docs' tables and the
//! settings menu go by one set of rules.

pub mod provenance;

pub use provenance::{Place, SetBy, Source};

/// An option's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// On or off, as an option of [`Kind::Bool`] holds.
    Bool(bool),
    /// A whole number, as an option of [`Kind::Number`] holds.
    Number(u32),
    /// A number with a fraction, as an option of [`Kind::Decimal`] holds.
    Decimal(Decimal),
    /// Whole numbers, as an option of [`Kind::List`] holds.
    List(List),
    /// One of the words an option of [`Kind::Choice`] can be.
    Choice(&'static str),
    /// Text, as an option of [`Kind::Text`] holds.
    Text(String),
}

/// Whole numbers, as an option holds several: `colorcolumn`'s columns.
pub type List = Vec<u32>;

/// A number to two decimal places, as `fontsize`'s 10.5. It's kept in
/// hundredths, so options still compare exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Decimal(u32);

impl Decimal {
    /// `hundredths` hundredths: 1050 is 10.5.
    #[must_use]
    pub const fn hundredths(hundredths: u32) -> Self {
        Self(hundredths)
    }

    /// The whole number `number`.
    pub const fn whole(number: u32) -> Self {
        Self(number.saturating_mul(100))
    }

    /// `number`, if it's one: not negative, and to two decimal places at
    /// most. A float only near one, as 10.1 is, counts.
    pub fn exact(number: f64) -> Option<Self> {
        let hundredths = (number * 100.0).round();
        let exact = (number * 100.0 - hundredths).abs() < 1e-6;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        (exact && (0.0..=f64::from(u32::MAX)).contains(&hundredths))
            .then_some(Self(hundredths as u32))
    }

    /// The nearest to `number` from `min` to `max`, to two decimal places,
    /// for a number from elsewhere, as the command line, that the option
    /// must still be able to hold.
    pub fn nearest(number: f32, min: u32, max: u32) -> Self {
        if number.is_nan() {
            return Self::whole(min);
        }
        let (min, max) = (Self::whole(min), Self::whole(max));
        let hundredths = (f64::from(number) * 100.0).round();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Self(hundredths.clamp(f64::from(min.0), f64::from(max.0)) as u32)
    }

    /// The number, as Lua has it.
    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 100.0
    }

    /// The number, as fonts are sized in.
    #[allow(clippy::cast_possible_truncation)]
    pub fn as_f32(self) -> f32 {
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
pub enum Kind {
    /// On or off.
    Bool,
    /// A whole number from `min` to `max`, or 0 too if `or_zero`, which
    /// means the window's own choice, as `explorerwidth`'s does.
    #[allow(missing_docs, reason = "said above")]
    Number { min: u32, max: u32, or_zero: bool },
    /// A number from `min` to `max`, to two decimal places.
    #[allow(missing_docs, reason = "said above")]
    Decimal { min: u32, max: u32 },
    /// Any number of whole numbers, each from `min` to `max`, written with
    /// commas between them: `80,120`. None is written as nothing.
    #[allow(missing_docs, reason = "said above")]
    List { min: u32, max: u32 },
    /// One of these words.
    Choice(&'static [&'static str]),
    /// Any text without control characters, such as `showbreak`'s.
    Text,
}

/// An option, as config and `:set` know it: the schema every option is
/// declared in once, which the Lua API, `:set` and its completion, the
/// docs' table of options and the settings menu all go by.
#[derive(Debug, PartialEq, Eq)]
pub struct Info {
    /// What it's called: `number`, or for the daemon's, its group and
    /// name, `web.mode`.
    pub name: &'static str,
    /// The short name, as Vim's: `nu` for `number`. Empty for those that
    /// have none.
    pub short: &'static str,
    /// What it holds.
    pub kind: Kind,
    /// Short help shown in command completion.
    pub description: &'static str,
    /// Where it's listed with others like it, as the docs and the settings
    /// menu list them.
    pub category: Category,
    /// Where it's kept when it's saved (see [`Scope`]).
    pub scope: Scope,
    /// It's read only as a window opens, so changing it waits for the next
    /// window: `:set` refuses it, and `:setpersist` saves it for then.
    pub restart: bool,
    /// It may be set for one window alone, with `:setlocal`, over the
    /// value every window shares, as Vim's window-local options can.
    pub local: bool,
}

/// Where an option's value is kept when it's saved, with `:setpersist`
/// or the settings menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The window's: `client/settings.lua`, which runs before `init.lua`.
    Window,
    /// `client/startup.toml`, under this key, as the window needs it
    /// before config runs.
    Startup(&'static str),
    /// The daemon's: `daemon/settings.lua`, which runs before the daemon's
    /// `init.lua`, as the daemon starts or upgrades.
    Daemon,
}

/// A group of options like each other, in the order they're listed: the
/// window's, then the daemon's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[allow(missing_docs, reason = "each is its title")]
pub enum Category {
    Font,
    Editor,
    Editing,
    Search,
    Code,
    Clipboard,
    Terminals,
    Agents,
    Window,
    Connections,
    /// The daemon's: this machine, as other machines know it.
    Machine,
    /// The daemon's: the chats agents start.
    Subagents,
    /// The daemon's: the web app.
    Web,
    /// The daemon's: what it keeps of terminals.
    Kept,
}

impl Category {
    /// Its name, as a heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::Font => "Font",
            Self::Editor => "Editors",
            Self::Editing => "Editing",
            Self::Search => "Searching",
            Self::Code => "Completion and language servers",
            Self::Clipboard => "The clipboard",
            Self::Terminals => "Terminals",
            Self::Agents => "Agents",
            Self::Window => "The window",
            Self::Connections => "Other machines",
            Self::Machine => "This machine",
            Self::Subagents => "Chats agents start",
            Self::Web => "The web app",
            Self::Kept => "What terminals keep",
        }
    }
}

impl Info {
    /// Checks `value` is one this option can hold, saying why not.
    pub fn check(&self, value: &Value) -> Result<(), String> {
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
    pub fn parse(&self, text: &str) -> Result<Value, String> {
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
    pub fn wants(&self) -> String {
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
