//! An editor laid out and drawn as a frame lays it out, for the `layout`
//! benchmark: no daemon, no GPU, just the work the window does for a pane
//! on each frame, on each key and on each turn of the wheel.

use clackatoo_protocol::{
    Answer, BufferId, BufferUpdate, InlayHint, InlayHintKind, ServerFeatures, Size,
};

use super::{BufferCopy, Editor, Shared, Stroke};
use crate::options::Options;

/// An editor showing a buffer only it has, which answers its own changes.
#[derive(Debug)]
pub struct EditorBench {
    editor: Editor,
    copy: BufferCopy,
    shared: Shared,
}

impl EditorBench {
    /// An editor `cols` by `rows` cells showing `text`, its long lines
    /// wrapped if `wrap`.
    ///
    /// # Panics
    ///
    /// If the size is nothing.
    #[must_use]
    pub fn new(text: &str, (cols, rows): (u16, u16), wrap: bool) -> Self {
        let copy = BufferCopy::scratch(BufferId(u32::MAX), "/bench.rs", text);
        let options = Options {
            wrap,
            ..Options::default()
        };
        let mut editor = Editor::new(BufferId(u32::MAX), &options);
        editor.resize(Size::new(cols, rows).expect("a size"));
        editor.set_options(&options, &copy);
        let mut bench = Self {
            editor,
            copy,
            shared: Shared::default(),
        };
        bench.settle();
        bench
    }

    /// Gives the text inlay hints, as a language server would: a type
    /// after the name each `let` binds, and a parameter's name before the
    /// first argument of each call.
    pub fn hint(&mut self) {
        let text = self.copy.text().to_string();
        let hint = |at, label: &str, kind| InlayHint {
            at,
            label: label.into(),
            kind,
            pad_left: false,
            pad_right: kind == InlayHintKind::Parameter,
        };
        let mut hints = Vec::new();
        for (at, _) in text.match_indices("let ") {
            let name = at + 4;
            let end = text[name..]
                .find(|ch: char| !ch.is_alphanumeric() && ch != '_')
                .map_or(text.len(), |end| name + end);
            hints.push(hint(end, ": usize", InlayHintKind::Type));
        }
        for (at, _) in text.match_indices('(') {
            hints.push(hint(at + 1, "value:", InlayHintKind::Parameter));
        }
        hints.sort_by_key(|hint| hint.at);
        let version = self.copy.place_now(0).map_or(0, |(_, version, _)| version);
        self.copy.update(BufferUpdate::Features(ServerFeatures {
            inlay_hints: true,
            ..ServerFeatures::default()
        }));
        let answer = Answer::InlayHints {
            start: 0,
            end: text.len(),
            hints,
        };
        let mut heard = (self.copy).update(BufferUpdate::Answer {
            id: 0,
            version,
            answer,
        });
        self.editor.heard(&mut heard, true, &mut self.copy);
        self.settle();
    }

    /// Lays out and draws the rows shown, as a frame does.
    pub fn settle(&mut self) {
        self.editor.settle(&self.copy);
    }

    /// Turns the wheel by `rows`, up if positive.
    pub fn scroll(&mut self, rows: i64) {
        self.editor.wheel(rows, &self.copy);
    }

    /// Types `text` as keys, each a character, but for `\u{8}`, which is
    /// Backspace, and `\u{1b}`, Esc; then takes in the copy's answers to
    /// its own changes.
    pub fn keys(&mut self, text: &str) {
        for ch in text.chars() {
            let stroke = match ch {
                '\u{8}' => Stroke::Backspace,
                '\u{1b}' => Stroke::Escape,
                ch => Stroke::char(ch),
            };
            self.editor
                .typed(stroke, &mut self.copy, &mut self.shared, false);
        }
        for update in self.copy.take_answers() {
            let mut heard = self.copy.update(update);
            self.editor.heard(&mut heard, true, &mut self.copy);
        }
        self.copy.take_made();
        self.copy.take_outbox();
    }
}
