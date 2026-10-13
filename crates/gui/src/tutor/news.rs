//! What's new: a line for each feature, which `:Tutor new` lists, each
//! going to the lesson that teaches it.
//!
//! **Each new feature adds a line at the end**, with a name of its own,
//! dated the day it's built, naming the lesson that teaches it (adding or
//! extending one), in a few words. That's all: a window that hasn't seen
//! the line says "clackatoo updated" once, and `:Tutor new` lists it
//! first.

/// A feature, as `:Tutor new` lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct News {
    /// Its name, which windows remember having seen it by: unique, and
    /// never changed once built, so its words can be.
    pub(crate) id: &'static str,
    /// When it was built, as `YYYY-MM-DD`.
    pub(crate) date: &'static str,
    /// The lesson that teaches it.
    pub(crate) lesson: &'static str,
    /// What it is, in a few words.
    pub(crate) what: &'static str,
}

/// Every feature with a lesson, oldest first. Add new ones at the end.
pub(crate) const NEWS: &[News] = &[
    News {
        id: "finder",
        date: "2026-10-12",
        lesson: "finder",
        what: "The finder: a project's files with <Space>pf, a grep with <Space>pgf, the files open with <Space>?, and what's wrong with <Space>ds",
    },
    News {
        id: "terminal-links",
        date: "2026-10-13",
        lesson: "shell",
        what: "Links in terminals, opened with Ctrl+click or gx",
    },
    News {
        id: "text-objects",
        date: "2026-10-13",
        lesson: "text-objects",
        what: "af, if, ac, aa and ]f: functions, classes and arguments, as the grammar finds them",
    },
    News {
        id: "registers",
        date: "2026-10-13",
        lesson: "registers",
        what: "Registers, macros and marks in editors, with macros that go through the command line and other panes",
    },
    News {
        id: "soft-wrap",
        date: "2026-10-13",
        lesson: "wrap",
        what: "Soft wrap, with linebreak, breakindent and showbreak, and gj and gk by screen row",
    },
    News {
        id: "substitute",
        date: "2026-10-13",
        lesson: "substitute",
        what: ":s with ranges, flags and a live preview, :g and :v, and case changes with ~, gu and gU",
    },
    News {
        id: "indent-by-syntax",
        date: "2026-10-13",
        lesson: "indenting",
        what: "Indenting as the language says: new lines, closing brackets, and =",
    },
    News {
        id: "snippets",
        date: "2026-10-13",
        lesson: "snippets",
        what: "Snippets in completion, with Tab and Shift+Tab between their places",
    },
    News {
        id: "zsh-marks",
        date: "2026-10-13",
        lesson: "shell",
        what: "zsh marks its commands too, so ]], [[ and yib work in it",
    },
    News {
        id: "explorer",
        date: "2026-10-13",
        lesson: "explorer",
        what: "File explorer panes: <Space>e, with git's marks, and a, r and d to make, rename and delete",
    },
    News {
        id: "agent-edits",
        date: "2026-10-13",
        lesson: "agents",
        what: "An agent's edits land in your editor as its own, beside your unsaved changes",
    },
    News {
        id: "agents-by-name",
        date: "2026-10-13",
        lesson: "agents",
        what: "Codex and Gemini beside Claude Code: :Agent codex",
    },
    News {
        id: "agent-tools",
        date: "2026-10-13",
        lesson: "agents",
        what: "Agents show you files and run commands in terminals you can watch",
    },
    News {
        id: "agent-undo",
        date: "2026-10-13",
        lesson: "agents",
        what: "u in a chat undoes the agent's last turn, keeping your edits since, and Ctrl+R makes it again",
    },
    News {
        id: "tutor",
        date: "2026-10-13",
        lesson: "basics",
        what: "The tutor: :Tutor teaches clackatoo a lesson at a time, and :Tutor new lists what's new",
    },
    News {
        id: "app-launcher",
        date: "2026-10-13",
        lesson: "basics",
        what: "clackatoo in your app launcher: `clackatoo desktop install`",
    },
    News {
        id: "claude-task",
        date: "2026-10-13",
        lesson: "agents",
        what: "Claude Code's hidden Task helpers are off unless you :set claudetask, so it hands work on in chats you can watch",
    },
    News {
        id: "remote-reconnect",
        date: "2026-10-13",
        lesson: "remote",
        what: "Remote windows ride out bad networks, reconnect by themselves, and keep what you typed into editors",
    },
    News {
        id: "connect-picker",
        date: "2026-10-13",
        lesson: "remote",
        what: "Connect to… in the app launcher, :connect and Ctrl+B r pick a machine to open a window on",
    },
    News {
        id: "subagents",
        date: "2026-10-13",
        lesson: "agents",
        what: "Subagents: background chats agents start, several at once, as profiles like explore, shown as a card",
    },
    News {
        id: "explorer-text",
        date: "2026-10-13",
        lesson: "explorer",
        what: "Edit folders as text in the explorer with e or :Explore!, and :w to rename, move, copy, make and delete",
    },
    News {
        id: "update-for-newer-daemon",
        date: "2026-10-13",
        lesson: "remote",
        what: "A window meeting a newer daemon updates clackatoo with Enter and opens again, connected where it was",
    },
    News {
        id: "chat-polish",
        date: "2026-10-13",
        lesson: "agents",
        what: "Chats show the time between turns far apart, Copy under each answer, and scroll as you select past them",
    },
    News {
        id: "long-lines",
        date: "2026-10-13",
        lesson: "wrap",
        what: "w, f, % and other motions and text objects work on lines of any length, as a minified file's",
    },
    News {
        id: "browser",
        date: "2026-10-13",
        lesson: "agents",
        what: "With a chat's Browser setting on, its agent drives a browser you watch in a pane",
    },
    News {
        id: "font-option",
        date: "2026-10-13",
        lesson: "font",
        what: "The font and its size as options: :set font and :set fontsize as you go, client.opt.font in config",
    },
];

impl News {
    /// What a window remembers of having seen it by.
    pub(crate) fn key(&self) -> String {
        self.id.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_is_named_once_dated_in_order_and_goes_to_a_lesson() {
        let mut ids = std::collections::HashSet::new();
        for pair in NEWS.windows(2) {
            assert!(pair[0].date <= pair[1].date, "{pair:?} out of order");
        }
        for news in NEWS {
            let date: Vec<&str> = news.date.split('-').collect();
            assert!(
                date.len() == 3 && date.iter().all(|part| part.parse::<u32>().is_ok()),
                "{news:?}"
            );
            assert!(
                super::super::lesson(news.lesson).is_some(),
                "{news:?} goes to no lesson"
            );
            // A name is one word, as the state file keeps it.
            assert!(
                !news.id.is_empty() && !news.id.contains(char::is_whitespace),
                "{news:?}"
            );
            assert!(ids.insert(news.id), "{news:?}'s name is taken");
        }
    }
}
