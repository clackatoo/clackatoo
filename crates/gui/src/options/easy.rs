//! The settings menu's Easy mode, as the schema declares it: which options
//! it shows, on which page and under which heading, in words a person who
//! doesn't know the option's name reads, and with which control.
//!
//! An option is declared once, in [`super::options!`]; this table only
//! says how Easy shows the ones it shows, in the order it shows them. An
//! option it doesn't name is in Advanced only, as every option is. One
//! entry can show several options as one control, as "Line numbers: Off,
//! On, Relative" sets `number` and `relativenumber` together. A test
//! checks every entry names options that exist, with a control that fits
//! what they hold.
//!
//! The Keys page shows keys for some of the window's own actions, to
//! record more for, rather than options.

use clackatoo_protocol::Side;

use crate::command::Kind as PaneKind;
use crate::desk::Action;
use crate::render::icons::Icon;

/// A page of Easy, down its side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Page {
    Appearance,
    Editor,
    Terminals,
    Agents,
    Permissions,
    Skills,
    Specialists,
    Mcp,
    Keys,
    Machines,
}

impl Page {
    /// Every page, in the order the side lists them.
    pub(crate) const ALL: [Self; 10] = [
        Self::Appearance,
        Self::Editor,
        Self::Terminals,
        Self::Keys,
        Self::Machines,
        Self::Agents,
        Self::Specialists,
        Self::Skills,
        Self::Mcp,
        Self::Permissions,
    ];

    /// Settings pages are grouped by what they configure.
    pub(crate) fn group(self) -> &'static str {
        match self {
            Self::Agents | Self::Permissions | Self::Skills | Self::Specialists | Self::Mcp => {
                "Agents"
            }
            _ => "General",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Editor => "Editor",
            Self::Terminals => "Terminals",
            Self::Agents => "Agents",
            Self::Permissions => "Permissions",
            Self::Skills => "Skills",
            Self::Mcp => "MCP Servers",
            Self::Specialists => "Specialists",
            Self::Keys => "Keys",
            Self::Machines => "Phone & other machines",
        }
    }

    /// The line under its title, saying what it's for.
    pub(crate) fn subtitle(self) -> &'static str {
        match self {
            Self::Appearance => "How text looks in terminals and editors.",
            Self::Editor => "Indenting, searching, and what language servers add.",
            Self::Terminals => "Marks, what's kept after a restart, and pasting.",
            Self::Agents => "Which agent a new chat starts with, and what it may do.",
            Self::Permissions => {
                "Choose who reviews approval requests, and manage Clackatoo's rules."
            }
            Self::Specialists => "Create and customize reusable agent roles.",
            Self::Mcp => "Manage server connections, credentials and provider configuration.",
            Self::Skills => "Shared skills and skillsets available to every agent in this project.",
            Self::Keys => "Keys for what you do most, recorded as you press them.",
            Self::Machines => "Working on other machines, and how they show here.",
        }
    }

    /// What it says at its foot, if anything: what's set elsewhere.
    pub(crate) fn note(self) -> Option<&'static str> {
        match self {
            Self::Terminals => Some(
                "What's kept after a restart, and wide emoji, are the daemon's: saved in its \
                 daemon/settings.lua.",
            ),
            Self::Agents => Some(
                "Add another agent, such as OpenCode, in the daemon's daemon/init.lua: new chats \
                 can start with it at once. Advanced has every limit on subagents.",
            ),
            Self::Keys => Some("Every key, in every mode, and what set it, is in Advanced."),
            Self::Skills => Some(
                "Add skills in .agents/skills, or bundles in .agents/skillsets. Use ~/.agents for every project. Reopen Settings to refresh; invoking a skill reads its latest instructions.",
            ),
            Self::Machines => Some(
                "The phone app and the web's password are the daemon's: clackatoo web sets them \
                 up.",
            ),
            Self::Mcp => Some(
                "Managed servers use Clackatoo proxies. Claude Code and the bundled Codex adapter support provider-server suppression; other adapters must explicitly support it. Provider policy changes apply after a daemon upgrade or restart.",
            ),
            Self::Specialists => Some(
                "Specialists are SPECIALIST.md files. Create personal or project roles, or copy a packaged role to customize it. Existing chats keep their current role.",
            ),
            Self::Permissions => Some(
                "Rules review requests that reach Clackatoo. Provider-approved operations keep their existing permissions; the provider's sandbox still applies. Rules are not an operating-system sandbox.",
            ),
            Self::Appearance | Self::Editor => None,
        }
    }

    pub(crate) fn icon(self) -> Icon {
        match self {
            Self::Appearance => Icon::Appearance,
            Self::Editor | Self::Skills | Self::Mcp => Icon::Code,
            Self::Terminals => Icon::Terminal,
            Self::Agents | Self::Specialists | Self::Permissions => Icon::Star,
            Self::Keys => Icon::Keyboard,
            Self::Machines => Icon::Phone,
        }
    }

    /// Whether it starts with a sample of text, drawn as the options
    /// shown say.
    pub(crate) fn previews(self) -> bool {
        self == Self::Appearance
    }
}

/// One of a control's labelled choices, and what it sets each of the
/// control's options to, in order: a word as `:set` takes it after `=`,
/// or `true` or `false` for an option that's on or off.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) label: &'static str,
    pub(crate) values: &'static [&'static str],
    /// Other values that do the same, which light it too, as
    /// `noignorecase smartcase` matches case as `Match` does.
    pub(crate) also: &'static [&'static [&'static str]],
    /// Values no choice is, but this is the nearest to, which `h` and `l`
    /// step from, unlit: `nonumber relativenumber` from `Relative`.
    pub(crate) near: &'static [&'static [&'static str]],
}

/// How Easy shows and changes an entry's options.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Control {
    /// A switch, for an option that's on or off.
    Toggle(&'static str),
    /// Buttons side by side, one lit: each a [`Choice`] of values for the
    /// options, together.
    Segmented {
        options: &'static [&'static str],
        choices: &'static [Choice],
    },
    /// A slider, from `min` to `max` in steps of `step`, each in
    /// hundredths, with its value and `unit` beside it.
    Slider {
        option: &'static str,
        min: u32,
        max: u32,
        step: u32,
        unit: &'static str,
    },
    /// A number, with − and + either side, by `step`, and `unit` after.
    Stepper {
        option: &'static str,
        step: u32,
        unit: &'static str,
    },
    /// A card for each agent the daemon knows, the one picked lit.
    Cards(&'static str),
    /// The font picker.
    Font(&'static str),
    /// Text typed, as `:set` takes it after `=`.
    Text(&'static str),
    /// The keys bound to one of the window's own actions, to record more.
    Keys(Action),
}

impl Entry {
    /// Whether it shows the daemon's options, which are named
    /// `group.name`, as `history.keep`.
    pub(crate) fn daemons(&self) -> bool {
        (self.control.options().iter()).any(|name| name.contains('.'))
    }
}

impl Control {
    /// The options it shows, by name.
    pub(crate) fn options(&self) -> &[&'static str] {
        match self {
            Self::Toggle(option)
            | Self::Slider { option, .. }
            | Self::Stepper { option, .. }
            | Self::Cards(option)
            | Self::Font(option)
            | Self::Text(option) => std::slice::from_ref(option),
            Self::Segmented { options, .. } => options,
            Self::Keys(_) => &[],
        }
    }
}

/// An entry of Easy: one row, under a heading of a page.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) page: Page,
    /// The heading it's under, as written; drawn in capitals.
    pub(crate) section: &'static str,
    pub(crate) label: &'static str,
    /// A line more, quietly under the label, or nothing.
    pub(crate) help: &'static str,
    pub(crate) control: Control,
}

const fn choice(label: &'static str, values: &'static [&'static str]) -> Choice {
    Choice {
        label,
        values,
        also: &[],
        near: &[],
    }
}

const fn toggle(
    page: Page,
    section: &'static str,
    label: &'static str,
    help: &'static str,
    option: &'static str,
) -> Entry {
    Entry {
        page,
        section,
        label,
        help,
        control: Control::Toggle(option),
    }
}

const fn choices(
    page: Page,
    section: &'static str,
    (label, help): (&'static str, &'static str),
    options: &'static [&'static str],
    choices: &'static [Choice],
) -> Entry {
    Entry {
        page,
        section,
        label,
        help,
        control: Control::Segmented { options, choices },
    }
}

const fn stepper(
    page: Page,
    section: &'static str,
    (label, help): (&'static str, &'static str),
    option: &'static str,
    (step, unit): (u32, &'static str),
) -> Entry {
    Entry {
        page,
        section,
        label,
        help,
        control: Control::Stepper { option, step, unit },
    }
}

const fn keys(label: &'static str, action: Action) -> Entry {
    Entry {
        page: Page::Keys,
        section: "Your keys",
        label,
        help: "",
        control: Control::Keys(action),
    }
}

use Page::{Agents, Appearance, Editor, Keys, Machines, Mcp, Permissions, Terminals};

/// Every entry, page by page, in the order each page shows them.
pub(crate) const ENTRIES: &[Entry] = &[
    choices(
        Permissions,
        "Default for new chats",
        (
            "Permission engine",
            "Existing chats keep their selection. Change a chat in its permissions menu. Claude uses Manual; Codex uses Workspace access. Other providers use Provider permissions.",
        ),
        &["permissions.engine"],
        &[
            choice("Provider permissions", &["provider"]),
            choice("Clackatoo rules", &["clackatoo"]),
        ],
    ),
    // Appearance.
    Entry {
        page: Appearance,
        section: "Text",
        label: "Font",
        help: "Monospace fonts installed on this machine.",
        control: Control::Font("font"),
    },
    Entry {
        page: Appearance,
        section: "Text",
        label: "Size",
        help: "Windows keep their size; panes fit as many cells as now fit.",
        control: Control::Slider {
            option: "fontsize",
            min: 800,
            max: 2400,
            step: 50,
            unit: "pt",
        },
    },
    choices(
        Appearance,
        "In editors",
        ("Line numbers", ""),
        &["number", "relativenumber"],
        &[
            choice("Off", &["false", "false"]),
            choice("On", &["true", "false"]),
            Choice {
                near: &[&["false", "true"]],
                ..choice("Relative", &["true", "true"])
            },
        ],
    ),
    toggle(
        Appearance,
        "In editors",
        "Highlight the cursor's line",
        "",
        "cursorline",
    ),
    toggle(
        Appearance,
        "In editors",
        "Wrap long lines",
        "A line too long goes on over more rows, not off the pane's edge.",
        "wrap",
    ),
    Entry {
        page: Appearance,
        section: "In editors",
        label: "Guide columns",
        help: "Columns marked down an editor, as 80 or 80,120.",
        control: Control::Text("colorcolumn"),
    },
    toggle(
        Appearance,
        "The window",
        "Smooth scrolling",
        "The mouse's wheel glides, rather than jumping a line at a time.",
        "smoothscroll",
    ),
    // Editor.
    stepper(
        Editor,
        "Indenting",
        ("Tab width", "How wide a tab is drawn."),
        "tabstop",
        (1, ""),
    ),
    toggle(
        Editor,
        "Indenting",
        "Indent with spaces",
        "Where no line nearby is indented yet: a file keeps the indent it has.",
        "expandtab",
    ),
    toggle(
        Editor,
        "Indenting",
        "Indent as the language does",
        "New lines are indented by the rules of the file's grammar.",
        "smartindent",
    ),
    toggle(
        Editor,
        "Long lines",
        "Break between words",
        "A wrapped line breaks after a blank or punctuation.",
        "linebreak",
    ),
    toggle(
        Editor,
        "Long lines",
        "Keep the indent on wrapped rows",
        "",
        "breakindent",
    ),
    stepper(
        Editor,
        "Long lines",
        (
            "Lines kept round the cursor",
            "The fewest lines in view above and below it.",
        ),
        "scrolloff",
        (1, ""),
    ),
    choices(
        Editor,
        "Searching",
        (
            "Letter case",
            "Smart ignores it, unless what you search for has a capital.",
        ),
        &["ignorecase", "smartcase"],
        &[
            choice("Smart", &["true", "true"]),
            choice("Ignore", &["true", "false"]),
            // Without ignorecase, smartcase does nothing.
            Choice {
                also: &[&["false", "true"]],
                ..choice("Match", &["false", "false"])
            },
        ],
    ),
    toggle(
        Editor,
        "Searching",
        "Light up every match",
        "Until Esc, or the next search.",
        "hlsearch",
    ),
    toggle(
        Editor,
        "Searching",
        "Go to matches as you type",
        "Esc puts the view back; Enter keeps it.",
        "incsearch",
    ),
    toggle(
        Editor,
        "Files",
        "Reload files changed on disk",
        "If you haven't changed them since: u undoes it.",
        "autoread",
    ),
    toggle(
        Editor,
        "Files",
        "Format before saving",
        "As the file's language server formats it.",
        "formatonsave",
    ),
    choices(
        Editor,
        "Files",
        ("Markdown opens as", ""),
        &["markdownview"],
        &[choice("Text", &["raw"]), choice("Rendered", &["rendered"])],
    ),
    toggle(
        Editor,
        "Files",
        "Show hidden files in explorers",
        "In explorers opened from now on.",
        "explorerhidden",
    ),
    toggle(Editor, "Code", "Suggest code as you type", "", "completion"),
    toggle(
        Editor,
        "Code",
        "Accept suggestions with Tab",
        "Enter and Ctrl+Y accept them too.",
        "completetab",
    ),
    toggle(
        Editor,
        "Code",
        "Show a function's arguments as you type them",
        "",
        "signaturehelp",
    ),
    toggle(
        Editor,
        "Code",
        "Light up uses of the name under the cursor",
        "",
        "highlightuses",
    ),
    toggle(
        Editor,
        "Code",
        "Show types and argument names in the code",
        "As the language server suggests them, dimmed.",
        "inlayhints",
    ),
    toggle(
        Editor,
        "Code",
        "Say what's wrong after a line's end",
        "The worst of what the language server says of it.",
        "virtualtext",
    ),
    toggle(
        Editor,
        "Code",
        "Show changes since git's index",
        "Signs in the gutter, as gitsigns.nvim shows them.",
        "gitsigns",
    ),
    choices(
        Editor,
        "Code",
        (
            "Install language support",
            "For a language opened that isn't installed.",
        ),
        &["languageinstall"],
        &[
            choice("Ask", &["ask"]),
            choice("Always", &["always"]),
            choice("Never", &["never"]),
        ],
    ),
    choices(
        Editor,
        "The clipboard",
        ("Yanks go to", "And p pastes from it."),
        &["clipboard"],
        &[
            choice("The clipboard", &["unnamedplus"]),
            choice("The selection", &["unnamed"]),
            choice("Neither", &["none"]),
        ],
    ),
    // Terminals.
    choices(
        Terminals,
        "Commands",
        (
            "Mark finished commands",
            "A ✗ by a failed one's prompt, with its exit status; or a ✓ by every one.",
        ),
        &["blockmarks"],
        &[
            choice("Failed", &["failed"]),
            choice("All", &["all"]),
            choice("None", &["none"]),
        ],
    ),
    toggle(
        Terminals,
        "After a restart",
        "Keep what terminals showed",
        "Scrollback comes back dimmed above the new prompt. Kept only on the daemon's machine.",
        "history.keep",
    ),
    choices(
        Terminals,
        "After a restart",
        ("Lines kept of each terminal", ""),
        &["history.lines"],
        &[
            choice("1,000", &["1000"]),
            choice("10,000", &["10000"]),
            choice("50,000", &["50000"]),
        ],
    ),
    toggle(
        Terminals,
        "Drawing",
        "Wide emoji",
        "An emoji with variation selector 16 takes two columns, in terminals started from now on.",
        "terminals.vs16",
    ),
    choices(
        Terminals,
        "Pasting",
        (
            "Paste an image (Ctrl+Shift+Alt+V) as",
            "A file path works with Claude Code and Codex on any machine.",
        ),
        &["pasteimage"],
        &[
            choice("A file path", &["path"]),
            choice("The clipboard", &["clipboard"]),
        ],
    ),
    choices(
        Terminals,
        "Pasting",
        (
            "Programs may use the clipboard",
            "Through OSC 52, as Neovim and tmux do, even over ssh.",
        ),
        &["osc52"],
        &[
            choice("Copy", &["copy"]),
            choice("Paste", &["paste"]),
            choice("Both", &["copy-paste"]),
            choice("Neither", &["off"]),
        ],
    ),
    // Agents.
    Entry {
        page: Agents,
        section: "New chats start with",
        label: "Agent",
        help: "",
        control: Control::Cards("agent"),
    },
    choices(
        Agents,
        "New chats",
        (
            "Tools",
            "What the agent may do with clackatoo's own: the chat's Tools setting changes it later.",
        ),
        &["agenttools"],
        &[
            choice("Ask first", &["ask"]),
            choice("Allow", &["allow"]),
            choice("None", &["off"]),
        ],
    ),
    toggle(
        Agents,
        "New chats",
        "Browser tools",
        "A headless browser of clackatoo's for the agent to drive, never yours.",
        "chatbrowser",
    ),
    toggle(
        Agents,
        "New chats",
        "Claude's own Task tool",
        "Helpers inside one turn, folded under the Task's card.",
        "claudetask",
    ),
    stepper(
        Agents,
        "Subagents",
        (
            "Working at once",
            "Chats agents start that may work at the same time, in all.",
        ),
        "subagents.working",
        (1, ""),
    ),
    choices(
        Agents,
        "While they work",
        ("Notify me about", "Only for chats you can't see."),
        &["notify"],
        &[
            choice("Everything", &["all"]),
            choice("Questions", &["questions"]),
            choice("Nothing", &["off"]),
        ],
    ),
    stepper(
        Agents,
        "While they work",
        (
            "When a long command finishes, after",
            "A notification for a command in a terminal you can't see.",
        ),
        "terminals.notify_after",
        (5, "s"),
    ),
    toggle(
        Agents,
        "While they work",
        "Show what turns spend",
        "",
        "chatcost",
    ),
    toggle(
        Agents,
        "While they work",
        "Show where agents changed files",
        "Marks in an editor's gutter, in the agents' teal.",
        "agentsigns",
    ),
    choices(
        Agents,
        "Rewinding",
        ("Files a rewind takes out", ""),
        &["rewindfiles"],
        &[
            choice("Ask", &["ask"]),
            choice("Undo them", &["undo"]),
            choice("Keep them", &["keep"]),
        ],
    ),
    toggle(
        Agents,
        "Native plugins",
        "Allow Claude Code plugins",
        "Use Claude's plugins and native skill catalogue alongside shared skills. Takes effect after a daemon restart or upgrade.",
        "plugins.claude",
    ),
    toggle(
        Agents,
        "Native plugins",
        "Allow Codex plugins",
        "Use Codex's plugins and native skill catalogue alongside shared skills. Takes effect after a daemon restart or upgrade.",
        "plugins.codex",
    ),
    toggle(
        Mcp,
        "Provider-managed MCP",
        "Allow provider-managed MCP servers",
        "Allow additional servers from the agent's own configuration. Managed servers remain available independently.",
        "mcp.providers",
    ),
    choices(
        Mcp,
        "Provider-managed MCP",
        (
            "Claude Code override",
            "Uses strict MCP configuration when off.",
        ),
        &["mcp.claude"],
        &[
            choice("Default", &["inherit"]),
            choice("Off", &["off"]),
            choice("Allow", &["allow"]),
        ],
    ),
    choices(
        Mcp,
        "Provider-managed MCP",
        (
            "Codex override",
            "Requires Clackatoo's bundled Codex adapter to suppress every configured layer.",
        ),
        &["mcp.codex"],
        &[
            choice("Default", &["inherit"]),
            choice("Off", &["off"]),
            choice("Allow", &["allow"]),
        ],
    ),
    // Keys.
    keys("New tab", Action::NewTab),
    keys(
        "Split, a terminal to the right",
        Action::Split(Side::Right, PaneKind::Terminal),
    ),
    keys(
        "Split, a terminal below",
        Action::Split(Side::Below, PaneKind::Terminal),
    ),
    keys(
        "A chat with an agent",
        Action::Split(Side::Right, PaneKind::Agent),
    ),
    keys("Close the pane", Action::Close),
    keys("Zoom the pane", Action::Zoom),
    keys("Workspaces", Action::PickWorkspace),
    stepper(
        Keys,
        "Hints",
        (
            "Show what keys do after",
            "Once you've pressed the start of some, as Ctrl+B.",
        ),
        "whichkeydelay",
        (100, "ms"),
    ),
    toggle(
        Keys,
        "Typing",
        "Use the desktop's input method",
        "For typing accents and other scripts.",
        "ime",
    ),
    // Phone and other machines.
    choices(
        Machines,
        "Workspaces",
        (
            "Name a workspace's machine",
            "After its name, as notes@desk.",
        ),
        &["showhost"],
        &[
            choice("When names clash", &["ambiguous"]),
            choice("Always", &["always"]),
            choice("Never", &["never"]),
        ],
    ),
    choices(
        Machines,
        "Over slow links",
        (
            "Show what you type before its echo",
            "Underlined, until the other machine's screen shows it.",
        ),
        &["predictecho"],
        &[
            choice("When slow", &["auto"]),
            choice("Always", &["always"]),
            choice("Never", &["never"]),
        ],
    ),
    toggle(
        Machines,
        "Over slow links",
        "Keep typing quick while forwarding",
        "Forwarded ports' traffic gives way to what you type.",
        "forwardpace",
    ),
    toggle(
        Machines,
        "This machine",
        "Check for new releases",
        "At most once a day; nothing is installed until you ask.",
        "updatecheck",
    ),
];

/// The entries of `page`, in order.
pub(crate) fn on(page: Page) -> impl Iterator<Item = &'static Entry> {
    ENTRIES.iter().filter(move |entry| entry.page == page)
}

/// Whether Easy shows the option called `name`.
pub(crate) fn shows(name: &str) -> bool {
    ENTRIES
        .iter()
        .any(|entry| entry.control.options().contains(&name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Kind;
    use crate::settings::find;

    #[test]
    fn every_entry_names_options_that_exist_with_a_control_that_fits_them() {
        let mut seen = std::collections::HashSet::new();
        for entry in ENTRIES {
            let label = entry.label;
            for name in entry.control.options() {
                let info = find(name).unwrap_or_else(|why| panic!("{label}: {why}"));
                assert!(seen.insert(*name), "{name} is in Easy twice");
                let kind = info.kind;
                match &entry.control {
                    Control::Toggle(_) => assert_eq!(kind, Kind::Bool, "{label}"),
                    Control::Segmented { .. } => assert!(
                        matches!(kind, Kind::Bool | Kind::Choice(_) | Kind::Number { .. }),
                        "{label}: {name} is {kind:?}"
                    ),
                    Control::Slider { min, max, step, .. } => {
                        let (low, high) = match kind {
                            Kind::Decimal { min, max } | Kind::Number { min, max, .. } => {
                                (min * 100, max * 100)
                            }
                            _ => panic!("{label}: a slider for {kind:?}"),
                        };
                        assert!(low <= *min && min < max && *max <= high, "{label}");
                        assert!(*step > 0 && (max - min) % step == 0, "{label}");
                    }
                    Control::Stepper { step, .. } => {
                        let Kind::Number { min, max, .. } = kind else {
                            panic!("{label}: a stepper for {kind:?}");
                        };
                        assert!(*step > 0 && *step <= max - min, "{label}");
                    }
                    Control::Cards(_) | Control::Text(_) => assert!(
                        matches!(kind, Kind::Text | Kind::List { .. }),
                        "{label}: {kind:?}"
                    ),
                    Control::Font(_) => assert_eq!(*name, "font", "{label}"),
                    Control::Keys(_) => unreachable!("keys name no option"),
                }
            }
            if let Control::Segmented { options, choices } = &entry.control {
                assert!(choices.len() >= 2, "{label}");
                let mut distinct = std::collections::HashSet::new();
                for choice in *choices {
                    let every = std::iter::once(choice.values)
                        .chain(choice.also.iter().copied())
                        .chain(choice.near.iter().copied());
                    for values in every {
                        assert_eq!(values.len(), options.len(), "{label}");
                        assert!(distinct.insert(values), "{label}: {values:?} twice");
                        for (name, value) in options.iter().zip(values) {
                            let info = find(name).unwrap();
                            match info.kind {
                                Kind::Bool => {
                                    assert!(["true", "false"].contains(value), "{label}");
                                }
                                _ => assert!(info.parse(value).is_ok(), "{label}: {value}"),
                            }
                        }
                    }
                }
                // A single option's choices are all it can be.
                if let [name] = options
                    && let Kind::Choice(words) = find(name).unwrap().kind
                {
                    assert_eq!(choices.len(), words.len(), "{label}: a word left out");
                }
            }
        }
        for page in Page::ALL {
            // Catalogue pages are populated by the daemon, rather than static options.
            if matches!(page, Page::Skills | Page::Specialists) {
                continue;
            }
            assert!(on(page).next().is_some(), "{page:?} shows nothing");
        }
    }

    #[test]
    fn the_keys_page_names_actions_the_window_binds_as_it_starts() {
        let keys = crate::desk::Keys::default();
        for entry in on(Page::Keys) {
            let Control::Keys(action) = entry.control else {
                continue;
            };
            let binding = crate::desk::Binding::Window(action);
            let bound =
                (keys.bound(crate::keymap::KeyMode::Normal)).any(|(_, bound, _)| *bound == binding);
            assert!(bound, "{}: nothing binds {action:?}", entry.label);
        }
    }
}
