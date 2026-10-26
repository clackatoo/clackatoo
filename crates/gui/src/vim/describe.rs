//! Descriptions belong to commands, not to a second key-to-command map.
use super::{Action, Entry, Operator, Recase, Scroll, Target, ViewAt};
use crate::motion::Motion;
use crate::objects::Object;

impl Motion {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Left => "Character left",
            Self::Right => "Character right",
            Self::LeftOver => "Previous character, crossing lines",
            Self::RightOver => "Next character, crossing lines",
            Self::Up(_) => "Line up",
            Self::Down(_) => "Line down",
            Self::LineStart => "Start of line",
            Self::LineEnd => "End of line",
            Self::WordStart => "Next word",
            Self::WordBack => "Previous word",
            Self::WordEnd => "End of word",
            Self::First => "First line, or line given by count",
            Self::Last => "Last line, or line given by count",
            Self::FirstNonBlank => "First nonblank character",
            Self::LastNonBlank => "Last nonblank character",
            Self::BigWordStart => "Next whitespace-delimited word",
            Self::BigWordBack => "Previous whitespace-delimited word",
            Self::BigWordEnd => "End of whitespace-delimited word",
            Self::Find {
                forward: true,
                till: false,
                ..
            } => "Find character forward",
            Self::Find {
                forward: false,
                till: false,
                ..
            } => "Find character backward",
            Self::Find {
                forward: true,
                till: true,
                ..
            } => "Before character forward",
            Self::Find {
                forward: false,
                till: true,
                ..
            } => "After character backward",
            Self::FindAgain { reverse: false } => "Repeat character find",
            Self::FindAgain { reverse: true } => "Reverse character find",
            Self::Match => "Matching bracket",
            Self::Paragraph { forward: true } => "Next paragraph",
            Self::Paragraph { forward: false } => "Previous paragraph",
            Self::RowUp(_) => "Previous screen line",
            Self::RowDown(_) => "Next screen line",
            Self::RowStart => "Start of screen line",
            Self::RowEnd => "End of screen line",
            Self::TextEnd => "End of text",
            Self::ViewTop => "Top of view",
            Self::ViewMiddle => "Middle of view",
            Self::ViewBottom => "Bottom of view",
            Self::NextLine => "First nonblank on next line",
            Self::PreviousLine => "First nonblank on previous line",
            Self::LineFirstNonBlank => "First nonblank on counted line",
            Self::Column => "Column given by count",
            Self::WordEndBack => "Previous word end",
            Self::BigWordEndBack => "Previous whitespace-delimited word end",
            Self::Sentence { forward: true } => "Next sentence",
            Self::Sentence { forward: false } => "Previous sentence",
        }
    }
}

impl Object {
    pub(crate) fn description(self, inner: bool) -> String {
        // A chat's `am` is more than a message: its whole turn.
        match (self, inner) {
            (Self::Message, true) => return "Message, answer or card".into(),
            (Self::Message, false) => return "Whole turn".into(),
            _ => {}
        }
        let name = match self {
            Self::Word => "word",
            Self::BigWord => "whitespace-delimited word",
            Self::Paragraph => "paragraph",
            Self::Sentence => "sentence",
            Self::Tag => "tag block",
            Self::Block => "shell block",
            Self::Quote('"') => "double quotes",
            Self::Quote('\'') => "single quotes",
            Self::Quote(_) => "backticks",
            Self::Bracket { open: '(', .. } => "parentheses",
            Self::Bracket { open: '[', .. } => "square brackets",
            Self::Bracket { open: '{', .. } => "braces",
            Self::Bracket { .. } => "angle brackets",
            Self::Code(clackatoo_protocol::Construct::Function) => "function",
            Self::Code(clackatoo_protocol::Construct::Class) => "class",
            Self::Code(clackatoo_protocol::Construct::Argument) => "argument",
            Self::CodeBlock => "code block",
            Self::Message => "message",
        };
        format!("{} {name}", if inner { "Inner" } else { "Whole" })
    }
}

impl Operator {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Delete => "Delete",
            Self::Change => "Change",
            Self::Yank => "Copy",
            Self::Indent => "Indent",
            Self::Dedent => "Dedent",
            Self::Reindent => "Reindent",
            Self::Recase(Recase::Lower) => "Lowercase",
            Self::Recase(Recase::Upper) => "Uppercase",
            Self::Recase(Recase::Toggle) => "Swap case",
            Self::Recase(Recase::Rot13) => "ROT13",
        }
    }
}

impl<O: Copy> Action<O> {
    pub(crate) fn description(self, own: impl FnOnce(O) -> String) -> String {
        let text = match self {
            Self::Move(motion) => motion.description(),
            Self::Operate(operator, target) => {
                return format!(
                    "{} {}",
                    operator.description(),
                    match target {
                        Target::Selection => "the selection".into(),
                        Target::SelectedLines => "the selected lines".into(),
                        Target::Lines => "whole lines".into(),
                        Target::Motion(motion) =>
                            format!("to {}", motion.description().to_lowercase()),
                        Target::Object { object, inner } =>
                            object.description(inner).to_lowercase(),
                        Target::Mark { .. } => "to mark".into(),
                    }
                );
            }
            Self::DeleteChars { before: false } => "Delete character",
            Self::DeleteChars { before: true } => "Delete previous character",
            Self::DeleteToEnd { change: false } => "Delete to end of line",
            Self::DeleteToEnd { change: true } => "Change to end of line",
            Self::Substitute => "Substitute character",
            Self::Put { before: false } => "Put after cursor",
            Self::Put { before: true } => "Put before cursor",
            Self::Join => "Join lines",
            Self::Replace(_) => "Replace character",
            Self::Insert(entry) => match entry {
                Entry::Before => "Insert before cursor",
                Entry::After => "Insert after cursor",
                Entry::LineStart => "Insert at first nonblank",
                Entry::LineEnd => "Append at end of line",
                Entry::Below => "New line below",
                Entry::Above => "New line above",
            },
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Repeat => "Repeat last change",
            Self::Visual { lines: false } => "Select characters",
            Self::Visual { lines: true } => "Select lines",
            Self::BlockVisual => "Select a block",
            Self::Search { backward: false } => "Search forward",
            Self::Search { backward: true } => "Search backward",
            Self::SearchAgain { reverse: false } => "Repeat search",
            Self::SearchAgain { reverse: true } => "Reverse search",
            Self::SearchWord { backward: false } => "Search word forward",
            Self::SearchWord { backward: true } => "Search word backward",
            Self::FindAgain { reverse: false } => "Repeat character find",
            Self::FindAgain { reverse: true } => "Reverse character find",
            Self::Scroll(scroll) => scroll.description(),
            Self::View(at) => match at {
                ViewAt::Top => "Cursor line at top",
                ViewAt::Middle => "Cursor line at centre",
                ViewAt::Bottom => "Cursor line at bottom",
            },
            Self::OtherEnd => "Other end of selection",
            Self::OtherCorner => "Other corner of selection",
            Self::Reselect => "Restore last selection",
            Self::Select { object, inner } => return object.description(inner),
            Self::SetMark(_) => "Set mark",
            Self::Mark { exact: false, .. } => "Jump to marked line",
            Self::Mark { exact: true, .. } => "Jump to exact mark",
            Self::Tilde => "Swap case",
            Self::Own(action) => return own(action),
        };
        text.into()
    }
}

impl Scroll {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Scroll::Line { down: true } => "Scroll a line down",
            Scroll::Line { down: false } => "Scroll a line up",
            Scroll::HalfPage { down: true } => "Scroll half a page down",
            Scroll::HalfPage { down: false } => "Scroll half a page up",
            Scroll::Page { down: true } => "Scroll a page down",
            Scroll::Page { down: false } => "Scroll a page up",
        }
    }
}
