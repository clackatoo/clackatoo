//! How far a long task has got, as the program doing it says with
//! `OSC 9 ; 4 ; state ; percent`: the sequence `ConEmu` began, which
//! Windows Terminal, Ghostty, kitty and `WezTerm` show too, and systemd, winget and
//! more and more command-line tools print.

/// How far a program's task has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TaskProgress {
    /// Under way, this many percent through (state 1).
    Running(u8),
    /// Under way, with no telling how far (state 3).
    Busy,
    /// It failed, as far through as it says, if it does (state 2).
    Failed(Option<u8>),
    /// Paused, as far through as it says, if it does (state 4).
    Paused(Option<u8>),
}

impl TaskProgress {
    /// What `params`, those after `OSC 9 ; 4`, say: `Some(None)` takes
    /// the progress away (state 0, or none given), and `None` means they
    /// say nothing this terminal knows, which changes nothing.
    #[allow(
        clippy::option_option,
        reason = "what the program says, which may be that there's none"
    )]
    pub(crate) fn parse(params: &[&[u8]]) -> Option<Option<Self>> {
        let state = match params.first() {
            None => 0,
            Some(state) => number(state).unwrap_or(0),
        };
        let percent = (params.get(1).and_then(|percent| number(percent)))
            .map(|percent| u8::try_from(percent.min(100)).expect("at most 100"));
        Some(match state {
            0 => None,
            1 => Some(Self::Running(percent.unwrap_or(0))),
            2 => Some(Self::Failed(percent)),
            3 => Some(Self::Busy),
            4 => Some(Self::Paused(percent)),
            _ => return None,
        })
    }

    /// How far through it says it is, if it does.
    #[must_use]
    pub fn percent(self) -> Option<u8> {
        match self {
            Self::Running(percent) => Some(percent),
            Self::Failed(percent) | Self::Paused(percent) => percent,
            Self::Busy => None,
        }
    }
}

/// The number `param` writes in decimal, if it does: empty is none.
fn number(param: &[u8]) -> Option<u32> {
    std::str::from_utf8(param).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::option_option, reason = "as TaskProgress::parse")]
    fn parsed(params: &str) -> Option<Option<TaskProgress>> {
        let params: Vec<&[u8]> = if params.is_empty() {
            Vec::new()
        } else {
            params.split(';').map(str::as_bytes).collect()
        };
        TaskProgress::parse(&params)
    }

    #[test]
    fn each_state() {
        assert_eq!(parsed("1;42"), Some(Some(TaskProgress::Running(42))));
        assert_eq!(parsed("2;7"), Some(Some(TaskProgress::Failed(Some(7)))));
        assert_eq!(parsed("2"), Some(Some(TaskProgress::Failed(None))));
        assert_eq!(parsed("3"), Some(Some(TaskProgress::Busy)));
        assert_eq!(parsed("4;90"), Some(Some(TaskProgress::Paused(Some(90)))));
        assert_eq!(parsed("0"), Some(None));
        assert_eq!(parsed(""), Some(None));
    }

    #[test]
    fn what_is_out_of_range_is_bounded_or_ignored() {
        assert_eq!(parsed("1;250"), Some(Some(TaskProgress::Running(100))));
        assert_eq!(parsed("1"), Some(Some(TaskProgress::Running(0))));
        assert_eq!(parsed("1;x"), Some(Some(TaskProgress::Running(0))));
        assert_eq!(
            parsed("1;99999999999"),
            Some(Some(TaskProgress::Running(0)))
        );
        assert_eq!(parsed("5;10"), None);
        assert_eq!(parsed(";"), Some(None));
    }
}
