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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Value {
    Bool(bool),
    Number(u32),
}

/// What an option holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Bool,
    /// A whole number from `min` to `max`.
    Number {
        min: u32,
        max: u32,
    },
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
    fn check(&self, value: Value) -> Result<(), String> {
        match (self.kind, value) {
            (Kind::Bool, Value::Bool(_)) => Ok(()),
            (Kind::Number { min, max }, Value::Number(number)) if (min..=max).contains(&number) => {
                Ok(())
            }
            _ => Err(self.wants()),
        }
    }

    /// What values the option holds, for messages.
    pub(crate) fn wants(&self) -> String {
        match self.kind {
            Kind::Bool => format!("{} is true or false", self.name),
            Kind::Number { min, max } => {
                format!("{} is a whole number from {min} to {max}", self.name)
            }
        }
    }
}

/// A type options hold, and its [`Value`].
trait Held: Sized {
    fn value(self) -> Value;
    fn from_value(value: Value) -> Option<Self>;
}

impl Held for bool {
    fn value(self) -> Value {
        Value::Bool(self)
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Bool(value) => Some(value),
            Value::Number(_) => None,
        }
    }
}

impl Held for u32 {
    fn value(self) -> Value {
        Value::Number(self)
    }

    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Number(value) => Some(value),
            Value::Bool(_) => None,
        }
    }
}

/// The [`Kind`] of an option of type `bool`, or `u32` with its range.
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
                    $(stringify!($name) => Some(Held::value(self.$name)),)+
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
        info.check(value)?;
        let changed = self.get(info) != value;
        self.put(info.name, value);
        Ok(changed)
    }

    /// How `info`'s option is written in `:set`: `number`, `nonumber`, or
    /// `numberwidth=4`.
    pub(crate) fn show(&self, info: &Info) -> String {
        match self.get(info) {
            Value::Bool(true) => info.name.to_owned(),
            Value::Bool(false) => format!("no{}", info.name),
            Value::Number(number) => format!("{}={number}", info.name),
        }
    }
}

/// One change, or question, a `:set` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Setting {
    /// Set the option to this.
    To(&'static Info, Value),
    /// Turn a true-or-false option over: `nu!`, `invnu`.
    Flip(&'static Info),
    /// Say what it is: `nu?`, or a number's name alone.
    Show(&'static Info),
}

/// What the words after `:set` ask for, as Vim reads them: `number`,
/// `nonumber`, `number!` or `invnumber`, `number?`, `numberwidth=5`.
pub(crate) fn parse_set(words: &str) -> Result<Vec<Setting>, String> {
    words.split_whitespace().map(parse_setting).collect()
}

fn parse_setting(word: &str) -> Result<Setting, String> {
    if ["+=", "-=", "^="].iter().any(|sign| word.contains(sign)) {
        return Err(format!(
            "can't read {word}: a number is set with =, and +=, -= and ^= aren't supported yet"
        ));
    }
    if let Some((name, value)) = word.split_once('=') {
        let info = find(name)?;
        let value = match info.kind {
            Kind::Bool => return Err(format!("{} is set with {0} or no{0}, not =", info.name)),
            Kind::Number { .. } => value.parse().map(Value::Number).map_err(|_| info.wants())?,
        };
        info.check(value)?;
        return Ok(Setting::To(info, value));
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
        Kind::Number { .. } if flip => Err(format!(
            "{} is a number, so can't be turned over",
            info.name
        )),
        Kind::Number { .. } => Ok(Setting::Show(info)),
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
    fn set_reads_as_vims_does() {
        let (nu, rnu, nuw) = (info("nu"), info("rnu"), info("nuw"));
        assert_eq!(
            parse_set("nonu rnu! invnumber nonu! nuw=7 nuw nu?"),
            Ok(vec![
                Setting::To(nu, Value::Bool(false)),
                Setting::Flip(rnu),
                Setting::Flip(nu),
                Setting::Flip(nu),
                Setting::To(nuw, Value::Number(7)),
                Setting::Show(nuw),
                Setting::Show(nu),
            ])
        );
        for (words, wanted) in [
            ("nuw=50", "from 1 to 20"),
            ("nuw=x", "from 1 to 20"),
            ("nu=1", "set with number or nonumber"),
            ("nonuw", "no option called `nonuw`"),
            ("nuw!", "can't be turned over"),
            ("nowrap", "no option called `nowrap`"),
            ("nuw+=1", "+=, -= and ^= aren't supported"),
        ] {
            let err = parse_set(words).unwrap_err();
            assert!(err.contains(wanted), "{words}: {err}");
        }
    }
}
