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
    News {
        id: "review-subagents",
        date: "2026-10-13",
        lesson: "agents",
        what: "A chat's :Review takes in its subagents' changes, open or closed, each hunk saying whose",
    },
    News {
        id: "phone",
        date: "2026-10-13",
        lesson: "remote",
        what: "Your chats from your phone: a web app the daemon serves, with notifications, paired with :Devices",
    },
    News {
        id: "slow-session",
        date: "2026-10-13",
        lesson: "agents",
        what: "An agent slow to open a chat's session says so after 30 s, and what it may be waiting on, with Retry and Stop",
    },
    News {
        id: "editor-folders",
        date: "2026-10-13",
        lesson: "explorer",
        what: "Editors open folders: :e . lists one to browse and edit, Enter opens a file in place, - goes up",
    },
    News {
        id: "startpage",
        date: "2026-10-13",
        lesson: "agents",
        what: ":Agent opens a start page: start a chat, or go on with one, the agent's own past sessions too",
    },
    News {
        id: "compat",
        date: "2026-10-14",
        lesson: "remote",
        what: "Windows and daemons a release or so apart work together: your laptop needn't update first",
    },
    News {
        id: "onemode",
        date: "2026-10-14",
        lesson: "basics",
        what: "One mode for the whole window: every pane, tab and workspace you go to is in insert or normal mode as you were",
    },
    News {
        id: "branchindent",
        date: "2026-10-14",
        lesson: "indenting",
        what: "A line where you type else, end or the like first is indented again; ]a stops once at each argument",
    },
    News {
        id: "phoneterminals",
        date: "2026-10-14",
        lesson: "remote",
        what: "Your terminals from your phone, at their own size: typed into, with a row of keys, and Fit",
    },
    News {
        id: "hoverhighlight",
        date: "2026-10-14",
        lesson: "finder",
        what: "K colours the code in what the language server says, in its language, and underlines what it's about",
    },
    News {
        id: "symbols",
        date: "2026-10-14",
        lesson: "finder",
        what: "<Space>bs lists the file's symbols, <Space>ps finds the project's, and grn asks the language server first",
    },
    News {
        id: "wordchars",
        date: "2026-10-14",
        lesson: "words",
        what: "Names such as margin-top are one word to w, e, b, iw, * and completion, in languages whose packs say so",
    },
    News {
        id: "highlightuses",
        date: "2026-10-14",
        lesson: "finder",
        what: "Uses of the name under the cursor are lit, gq formats a selection, and a failed format says why",
    },
    News {
        id: "working",
        date: "2026-10-14",
        lesson: "agents",
        what: "A chat at work says above its box what the agent is doing, with pulsing dots, and its tab shows ⋯",
    },
    News {
        id: "inlayhints",
        date: "2026-10-14",
        lesson: "finder",
        what: "Language servers' inlay hints, types and parameter names, are written between the code; <Space>th toggles them",
    },
    News {
        id: "usage",
        date: "2026-10-14",
        lesson: "agents",
        what: "Chats show how full their context is, and Claude's limits; tabs and workspaces mark ⋯ working, ? asking, • unseen",
    },
    News {
        id: "explorer-up",
        date: "2026-10-14",
        lesson: "explorer",
        what: "A .. line tops the explorer's tree: Enter or a double click on it goes up a folder",
    },
    News {
        id: "editor-drag",
        date: "2026-10-14",
        lesson: "mouse",
        what: "Drag the mouse in an editor to select, by words after a double click and by lines after a triple",
    },
    News {
        id: "listing-up",
        date: "2026-10-14",
        lesson: "explorer",
        what: "A folder's listing starts with a .. line that Enter or a double click goes up from",
    },
    News {
        id: "visual-repeat",
        date: "2026-10-14",
        lesson: "mouse",
        what: "gv selects the last selection again, and the dot command makes a change made in visual mode again on as much text",
    },
    News {
        id: "port-forward",
        date: "2026-10-15",
        lesson: "remote",
        what: ":PortForward 8080 lets a browser here reach port 8080 on a remote window's machine",
    },
    News {
        id: "launch-focus",
        date: "2026-10-15",
        lesson: "basics",
        what: "Opened from your app launcher, the window takes focus and the launcher stops spinning",
    },
    News {
        id: "bar-overflow",
        date: "2026-10-15",
        lesson: "basics",
        what: "Crowded bars scroll their tabs and workspaces, messages keep off the tabs, and :messages shows them again",
    },
    News {
        id: "forward-pace",
        date: "2026-10-15",
        lesson: "remote",
        what: ":PortForward downloads no longer delay your keys on a slow link (:set noforwardpace)",
    },
    News {
        id: "answer-in-normal",
        date: "2026-10-15",
        lesson: "agents",
        what: "A chat's question is answered in normal mode too: its number, then the same again or Enter",
    },
    News {
        id: "interface-font-fallback",
        date: "2026-10-15",
        lesson: "font",
        what: "With the desktop's interface font missing, the bars and finder use your sans-serif font, as chats do",
    },
    News {
        id: "finder-nothing-matches",
        date: "2026-10-15",
        lesson: "finder",
        what: "The finder says when nothing matches, and the row picked in lists and menus stands out",
    },
    News {
        id: "command-line",
        date: "2026-10-15",
        lesson: "basics",
        what: "The command line completes with Tab, goes back through what you've typed with Up and Down, edits with Ctrl+U, Ctrl+W, Ctrl+R and the arrows, and keeps the bottom bar showing",
    },
    News {
        id: "terminal-counts",
        date: "2026-10-15",
        lesson: "basics",
        what: "A terminal's normal mode takes counts, as 5k, 3w, 10G and 3yy",
    },
    News {
        id: "lessons-read-well",
        date: "2026-10-15",
        lesson: "basics",
        what: "Lessons draw their code as the chat does, keep it on one row, and tick off the steps done in green",
    },
    News {
        id: "explorer-keys",
        date: "2026-10-15",
        lesson: "explorer",
        what: "g? lists an explorer's keys, listings colour folders, an empty one says how to make a file, and :|Explore and :-Review open beside the focused pane",
    },
    News {
        id: "visual-ends-at-text",
        date: "2026-10-15",
        lesson: "basics",
        what: "A terminal's selection ends a cell past each line's text, as what y copies does, not at the pane's edge",
    },
    News {
        id: "chat-markdown",
        date: "2026-10-15",
        lesson: "agents",
        what: "Chats draw tables as grids that scroll sideways, colour code in its language, and line code and quotes up",
    },
    News {
        id: "chat-cursor",
        date: "2026-10-15",
        lesson: "agents",
        what: "A chat's normal mode shows a solid cursor at once, in view; zR, zM and za fold subagents' cards too",
    },
    News {
        id: "chat-wording",
        date: "2026-10-15",
        lesson: "agents",
        what: "Chats say Running: cargo test · 12s, their questions say Reject, and long messages type faster",
    },
    News {
        id: "did-you-mean",
        date: "2026-10-15",
        lesson: "basics",
        what: "A command, option or lesson that isn't one says so in a clause, and asks \"did you mean…?\" of a slip",
    },
    News {
        id: "finder-colours",
        date: "2026-10-15",
        lesson: "finder",
        what: "The finder picks out what's typed in every list, colours what's wrong, and colours open files' previews",
    },
    News {
        id: "quieter-look",
        date: "2026-10-15",
        lesson: "agents",
        what: "Amber is kept for focus and what to act on: links and matched letters are blue, headings bold, Copy shows on hover, and the ~ past a file's end is faint",
    },
    News {
        id: "empty-chat",
        date: "2026-10-15",
        lesson: "agents",
        what: "A new chat says which agent and folder it has, and in normal mode, to press i to write",
    },
    News {
        id: "which-key",
        date: "2026-10-15",
        lesson: "basics",
        what: "Ctrl+B lists every key it takes and what each does, in a panel, and the bar marks it ^B",
    },
    News {
        id: "prompt-focus",
        date: "2026-10-15",
        lesson: "finder",
        what: "A prompt leaves the pane's cursor hollow, and a new name starts selected, so typing replaces it",
    },
    News {
        id: "plan-asks-in-place",
        date: "2026-10-15",
        lesson: "explorer",
        what: ":w's changes to a folder are titled with it, and asked about right under them",
    },
    News {
        id: "completion-columns",
        date: "2026-10-15",
        lesson: "snippets",
        what: "The completion menu's labels start under the word you type, with kinds at the right, and its documentation sits apart",
    },
    News {
        id: "finder-previews-coloured",
        date: "2026-10-15",
        lesson: "finder",
        what: "The finder colours previews of files that aren't open too, and the first chat opens without a pause",
    },
    News {
        id: "failed-marks",
        date: "2026-10-15",
        lesson: "shell",
        what: "A command that failed ends its prompt's line with ✗ and its exit status; blockmarks=all marks the rest ✓",
    },
    News {
        id: "scrollback-pill",
        date: "2026-10-15",
        lesson: "basics",
        what: "A terminal scrolled back says how far, and how to get back, in a pill; searches say when they wrap",
    },
    News {
        id: "tab-folders",
        date: "2026-10-15",
        lesson: "shell",
        what: "A terminal's tab with no title is named after its shell's folder, ~ for home",
    },
    News {
        id: "block-visual",
        date: "2026-10-15",
        lesson: "blocks",
        what: "Ctrl+V selects a block of columns, as in Vim: I and A type on every line of it; Alt+drag selects one too",
    },
    News {
        id: "copy-limits",
        date: "2026-10-15",
        lesson: "basics",
        what: "Copying from a terminal stops at 100,000 lines or 4 MiB, and says so, so a huge history never holds it up",
    },
    News {
        id: "keys-desc",
        date: "2026-10-15",
        lesson: "basics",
        what: "Keys your config binds say what their desc says in the panel of keys, and client.keys.group names keys that start several",
    },
    News {
        id: "counted-keys",
        date: "2026-10-15",
        lesson: "basics",
        what: "3 Ctrl+O goes back three places, and a function your config binds to keys hears the count typed before them",
    },
    News {
        id: "web-failed-marks",
        date: "2026-10-15",
        lesson: "shell",
        what: "The web app marks commands that failed, as the window does",
    },
    News {
        id: "e-alone",
        date: "2026-10-15",
        lesson: "explorer",
        what: ":e alone opens the explorer at the folder you're in beside an empty file, which :w NAME names",
    },
    News {
        id: "recovery",
        date: "2026-10-16",
        lesson: "explorer",
        what: "Changes not written, [No Name]'s too, outlive a crash or a stop, and :recover shows them",
    },
    News {
        id: "restore",
        date: "2026-10-16",
        lesson: "shell",
        what: "Workspaces, tabs and panes come back after a restart; R puts what a terminal ran at its prompt",
    },
    News {
        id: "rerunbash",
        date: "2026-10-16",
        lesson: "shell",
        what: "bash too offers what it was running after a restart, and only the shell's own marks count",
    },
    News {
        id: "connect",
        date: "2026-10-18",
        lesson: "remote",
        what: ":connect HOST brings another machine's workspaces, terminals and chats into your bar",
    },
    News {
        id: "relayed",
        date: "2026-10-18",
        lesson: "remote",
        what: "In another machine's workspaces, editors, explorers, the finder, reviews, boards and :PortForward work too",
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
