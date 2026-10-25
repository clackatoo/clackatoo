//! The desk's side of editors showing Markdown rendered (see
//! `editor::rendered`): `:markdown` and `<Space>tm`, which turn it on and
//! off; drawing it, which needs the fonts; its code's colours, which the
//! daemon is asked for as a chat's are; and clicks on its links.
//!
//! A link opens as it would elsewhere in the window: a URL with the
//! desktop's opener, as a terminal's links open, and a file in the
//! editor, in place of the Markdown file, as `:e` would, written from the
//! Markdown file's folder unless it's absolute. A link to `#` and a heading
//! scrolls to the heading; to another file's heading, it opens the file.

use std::path::Path;

use clackatoo_protocol::{ClientMessage, PaneId, Span};

use super::Desk;
use crate::chat::Pointer;
use crate::editor::Done;
use crate::fonts::Fonts;
use crate::links::{self, Target};
use crate::markdown::layout::TextPaint;

impl Desk {
    /// `:markdown`: the focused editor shows its Markdown rendered, or its
    /// text again, or the bar says why it can't.
    pub(super) fn toggle_markdown(&mut self) {
        let mut refused = None;
        let toggled = self.in_focused_editor(|editor, copy, _| {
            editor.toggle_rendered(copy).unwrap_or_else(|why| {
                refused = Some(why);
                Done::default()
            })
        });
        match (toggled, refused) {
            (None, _) => self.say("this pane has no file".into()),
            (_, Some(why)) => self.say(why),
            _ => {}
        }
    }

    /// Whether `pane` is an editor with no insert mode: a lesson, or one
    /// showing its Markdown rendered, which is read, not typed in. The
    /// focus arriving at one leaves it in normal mode, and the window's
    /// mode as it was, as an explorer's arriving does.
    pub(super) fn has_no_insert_mode(&self, pane: PaneId) -> bool {
        self.is_lesson_pane(pane)
            || self
                .editor(pane)
                .is_some_and(crate::editor::Editor::rendered)
    }

    /// Lays out and draws the rendered Markdown of the editor `pane`, if it
    /// shows it so, in its tile, `size` pixels with its top left at
    /// `origin`.
    pub(crate) fn paint_rendered(
        &mut self,
        pane: PaneId,
        fonts: &mut Fonts,
        origin: [f32; 2],
        size: [f32; 2],
    ) -> Option<TextPaint> {
        let (editor, copy) = self.editor_and_copy(pane)?;
        editor.paint_rendered(fonts, origin, size, copy)
    }

    /// The editor showing its Markdown rendered at `point`, if one's there,
    /// and where `point` is from its tile's top left.
    fn rendered_at(&self, point: [f32; 2]) -> Option<(PaneId, [f32; 2])> {
        let placement = self.placement()?;
        let pane = placement.pane_at(point)?;
        if !self
            .editor(pane)
            .is_some_and(crate::editor::Editor::rendered)
        {
            return None;
        }
        let (_, tile) = placement.panes.iter().find(|(id, _)| *id == pane)?;
        Some((pane, [point[0] - tile.origin[0], point[1] - tile.origin[1]]))
    }

    /// What the mouse is over at `point`, if it's an editor's rendered
    /// Markdown: a link, which a click opens, or text, which it doesn't
    /// select.
    pub(crate) fn rendered_pointer(&self, point: [f32; 2]) -> Option<Pointer> {
        let (pane, at) = self.rendered_at(point)?;
        let link = self.editor(pane)?.rendered_link_at(at);
        Some(if link.is_some() {
            Pointer::Hand
        } else {
            Pointer::Arrow
        })
    }

    /// A click at `point`, if it's in an editor's rendered Markdown: opens
    /// the link there, if there's one. Returns whether it was in one.
    pub(super) fn rendered_click(&mut self, point: [f32; 2]) -> bool {
        let Some((pane, at)) = self.rendered_at(point) else {
            return false;
        };
        let link = (self.editor(pane))
            .and_then(|editor| editor.rendered_link_at(at))
            .map(str::to_owned);
        if let Some(link) = link {
            self.open_rendered_link(pane, &link);
        }
        true
    }

    /// Opens what a link in the rendered Markdown of the editor `pane` goes
    /// to, `to`, as written.
    pub(super) fn open_rendered_link(&mut self, pane: PaneId, to: &str) {
        if to.trim().is_empty() {
            self.say("the link goes nowhere".into());
            return;
        }
        if let Some(heading) = to.strip_prefix('#') {
            self.go_to_rendered_heading(pane, heading);
            return;
        }
        if has_scheme(to) {
            match links::target_of_uri(to) {
                Some(Target::Url(url)) => self.opened.push(url),
                Some(Target::File { path, .. }) => {
                    self.open_here(pane, path);
                }
                Some(Target::Elsewhere { uri, host }) => {
                    self.say(format!("{uri} is on {host}, not this machine"));
                }
                None => self.say(format!("‘{to}’ isn't a link clackatoo opens")),
            }
            return;
        }
        // A file, from the Markdown file's folder. A heading of this file
        // is scrolled to; another's isn't looked for.
        let (file, heading) = to.split_once('#').unwrap_or((to, ""));
        let file = file.split('?').next().unwrap_or_default();
        let Some((_, copy)) = self.editor_and_copy(pane) else {
            return;
        };
        let this = copy.path().to_owned();
        let folder = Path::new(&this).parent().and_then(Path::to_str);
        let path = super::links::absolute(&links::percent_decode(file), folder);
        match (path == this, heading) {
            (false, _) => {
                self.open_here(pane, path);
            }
            (true, "") => self.say("nothing opened: it's the file shown".into()),
            (true, heading) => self.go_to_rendered_heading(pane, heading),
        }
    }

    /// Scrolls the rendered Markdown of the editor `pane` to the heading
    /// `heading` names, or says there's none.
    fn go_to_rendered_heading(&mut self, pane: PaneId, heading: &str) {
        let found = (self.editor_and_copy(pane))
            .is_some_and(|(editor, _)| editor.rendered_heading(heading));
        if !found {
            self.say(format!("no heading ‘{heading}’"));
        }
    }

    /// Asks the daemon to colour the code editors' rendered Markdown laid
    /// out since last asked, as `colours` says it can, or tells them it
    /// can't be. Returns whether there's anything to send.
    pub(super) fn ask_rendered_colours(&mut self, colours: bool) -> bool {
        let mut asks = Vec::new();
        for (pane, editor) in self.editors_mut() {
            for code in editor.take_colour_asks() {
                if colours {
                    asks.push((pane, code));
                } else {
                    editor.colours_asked(None, code);
                }
            }
        }
        let sent = !asks.is_empty();
        for (pane, code) in asks {
            self.next_colour += 1;
            let id = self.next_colour;
            (self.outbox).push(ClientMessage::Colour {
                id,
                code: code.clone(),
            });
            self.rendered_colour_asks.insert(id, pane);
            if let Some((editor, _)) = self.editor_and_copy(pane) {
                editor.colours_asked(Some(id), code);
            }
        }
        sent
    }

    /// Whether an editor's rendered Markdown made the ask for colours
    /// numbered `id`.
    pub(super) fn rendered_asked(&self, id: u64) -> bool {
        self.rendered_colour_asks.contains_key(&id)
    }

    /// The daemon's colours for the ask numbered `id`, which an editor's
    /// rendered Markdown made: to it, if it's still here. Returns whether to
    /// draw.
    pub(super) fn rendered_coloured(&mut self, id: u64, spans: Vec<Vec<Span>>) -> bool {
        let Some(pane) = self.rendered_colour_asks.remove(&id) else {
            return false;
        };
        (self.editor_and_copy(pane)).is_some_and(|(editor, _)| editor.coloured(id, spans))
    }

    /// Forgets the asks for colours editors' rendered Markdown made that
    /// aren't answered, as the connection's gone: they ask again.
    pub(super) fn forget_rendered_colour_asks(&mut self) {
        self.rendered_colour_asks.clear();
        for (_, editor) in self.editors_mut() {
            editor.forget_colour_asks();
        }
    }
}

/// Whether `link` starts with a URI's scheme, as `https:` or `mailto:`: a
/// letter and then letters, digits, `+`, `-` or `.`, more than one, so a
/// Windows drive's letter isn't one.
fn has_scheme(link: &str) -> bool {
    link.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme.starts_with(|ch: char| ch.is_ascii_alphabetic())
            && (scheme.chars())
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    })
}
