//! Formatting a file with its language server: `:Format`, and `:w` when
//! the `formatonsave` option is on.
//!
//! The daemon makes the server's edits to the buffer, as the user's, one
//! undo step, then answers. A `:w` that formats first writes once that
//! answer has come, whatever it says, so a file whose server can't format
//! is still written; or after a moment, if the server is slow to answer,
//! written as it is.

use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, BufferId, PaneId, Query, Request};

use super::{Asked, Desk};

/// How long a `:w` waits for formatting before it writes the file as it
/// is: longer than the daemon waits to ask again a server that's busy
/// reading the project, as rust-analyzer can be.
const FORMAT_WAIT: Duration = Duration::from_secs(3);

/// A file being formatted.
#[derive(Debug)]
pub(super) struct Formatting {
    buffer: BufferId,
    /// What to write once it's formatted: over changes made since if
    /// `force`, and closing this pane after if there's one, as `:wq` does.
    write: Option<Write>,
}

/// A file to write once it's formatted.
#[derive(Debug)]
struct Write {
    force: bool,
    then_close: Option<PaneId>,
    /// When to write it anyway.
    by: Instant,
}

impl Desk {
    /// `:Format`: formats the focused editor's file.
    pub(super) fn format_file(&mut self) {
        self.format(None);
    }

    /// `:w` with `formatonsave` on: formats the focused editor's file,
    /// then writes it, over changes since if `force`, and closes the pane
    /// after if `quit`.
    pub(super) fn format_then_write(&mut self, force: bool, quit: bool) {
        let then_close = quit.then(|| self.shown.map(|shown| shown.pane)).flatten();
        self.format(Some(Write {
            force,
            then_close,
            by: Instant::now() + FORMAT_WAIT,
        }));
    }

    fn format(&mut self, write: Option<Write>) {
        // Indenting as `>>` does.
        let query = Query::Format {
            tab_size: match self.options.shiftwidth {
                0 => self.options.tabstop,
                width => width,
            },
            spaces: self.options.expandtab,
        };
        // A file being formatted isn't asked again: its answer, when it
        // comes, writes it too if it's to be.
        let focused = self.focused_editor().map(|(editor, _)| editor.buffer());
        if let Some(pending) =
            (self.formatting.values_mut()).find(|formatting| Some(formatting.buffer) == focused)
        {
            match write {
                Some(write) if pending.write.is_none() => pending.write = Some(write),
                Some(_) => {}
                None => self.message = Some("it's being formatted already".into()),
            }
            return;
        }
        let Some((buffer, id)) = self.ask_about_focused_file(query) else {
            self.message = Some("this pane has no file to format".into());
            return;
        };
        self.formatting.insert(id, Formatting { buffer, write });
    }

    /// The answer to query `id`, if it's formatting's: says how it went,
    /// and writes the file if it was to be. Returns whether it was.
    pub(super) fn formatted(&mut self, id: u64, answer: &Answer) -> bool {
        let Some(formatting) = self.formatting.remove(&id) else {
            return false;
        };
        let said = match answer {
            Answer::Formatted { changed: true } => None,
            Answer::Formatted { changed: false } => Some("already formatted".to_owned()),
            Answer::Failed(why) => Some(format!("not formatted: {why}")),
            _ => Some("not formatted".to_owned()),
        };
        match formatting.write {
            Some(write) => {
                // Said when it's written, if formatting went wrong, but
                // for a file there's no formatting.
                if let Answer::Failed(why) = answer
                    && !why.contains("no language server")
                    && !why.contains("doesn't format files")
                {
                    self.unformatted.insert(formatting.buffer, why.clone());
                }
                self.write(formatting.buffer, &write);
            }
            None => self.message = Some(said.unwrap_or_else(|| "formatted".into())),
        }
        true
    }

    /// Writes the files whose formatting has taken too long by `now`, as
    /// they are. Returns whether any was.
    pub(crate) fn format_if_due(&mut self, now: Instant) -> bool {
        let due: Vec<u64> = (self.formatting.iter())
            .filter(|(_, formatting)| {
                formatting
                    .write
                    .as_ref()
                    .is_some_and(|write| write.by <= now)
            })
            .map(|(&id, _)| id)
            .collect();
        for id in &due {
            let Some(formatting) = self.formatting.remove(id) else {
                continue;
            };
            if let Some(write) = formatting.write {
                let why = "its language server took too long".to_owned();
                self.unformatted.insert(formatting.buffer, why);
                self.write(formatting.buffer, &write);
            }
        }
        !due.is_empty()
    }

    /// When the window must wake to write a file whose formatting is
    /// taking too long.
    pub(crate) fn format_at(&self) -> Option<Instant> {
        (self.formatting.values())
            .filter_map(|formatting| formatting.write.as_ref().map(|write| write.by))
            .min()
    }

    fn write(&mut self, buffer: BufferId, write: &Write) {
        self.request(
            Request::Save {
                buffer,
                force: write.force,
            },
            Asked::Saved {
                buffer,
                then_close: write.then_close,
            },
        );
    }
}
