//! The picker of a machine to connect to: what `clackatoo
//! --connect-picker` opens the window as, from the launcher's "Connect
//! to…", and `:connect` opens over the panes.
//!
//! It lists the machines connected to recently, those `~/.ssh/config`
//! names, and Tailscale's peers that are online (see [`crate::hosts`]),
//! gathered on a thread of its own, as they come: Tailscale is asked last,
//! and may take a moment. What's typed is matched against them as the
//! finder matches, and drawn as the finder is (see [`crate::finder::view`]).
//! If what's typed names a machine none of them is, it's offered too, to
//! connect to as typed: first if it has a user or a port, as
//! `jack@box:2222`, or nothing else matches, and otherwise last, so a few
//! letters of a machine's name still pick that machine.
//!
//! Opened as the window starts, picking a machine connects the window to
//! it, as `--remote` does, and Esc closes the window. If connecting fails,
//! the picker opens again, saying why. Opened by `:connect`, picking one
//! opens a new window on it, and Esc just closes the picker.
//!
//! | Key | Does |
//! |---|---|
//! | Typing, Backspace | Changes what's typed |
//! | Ctrl+U, Ctrl+W | Deletes what's typed, or its last word |
//! | Down, Ctrl+N; Up, Ctrl+P | The next or previous machine |
//! | Page Down, Page Up | A page further or back |
//! | Enter | Connects to it |
//! | Esc, Ctrl+C | Closes the picker |

use std::time::Duration;

use winit::keyboard::{Key, NamedKey};

use crate::finder::rank::{Items, Ranking};
use crate::finder::view::{FinderView, Row, fit};
use crate::hosts;
use crate::input::KeyPress;
use crate::options::Case;
use crate::render::Ink;

/// How long Tailscale may take to say who its peers are.
const TAILSCALE_TIMEOUT: Duration = Duration::from_secs(2);

/// What picking a machine does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// Connects this window to it: the window opened as the picker.
    ThisWindow,
    /// Opens a new window on it, as `:connect` does.
    NewWindow,
}

/// Machines found for a picker, by the thread gathering them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// The number of the picker they're for.
    pub(crate) picker: u64,
    /// Each machine, and where it was found, as the picker says it.
    pub(crate) hosts: Vec<(String, String)>,
    /// Whether these are the last.
    pub(crate) last: bool,
}

/// Gathers the machines to pick from, for the picker numbered `picker`,
/// on a thread of its own, passing each batch to `send` as it's found:
/// those connected to recently and in the ssh config, then Tailscale's.
pub(crate) fn gather(picker: u64, send: impl Fn(Found) + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("hosts".into())
        .spawn(move || {
            let mut found: Vec<(String, String)> = (hosts::recent().into_iter())
                .map(|host| (host, "recent".to_owned()))
                .collect();
            if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
                let configured = hosts::ssh_config_hosts(std::path::Path::new(&home));
                found.extend(
                    configured
                        .into_iter()
                        .map(|host| (host, "ssh config".into())),
                );
            }
            send(Found {
                picker,
                hosts: found,
                last: false,
            });
            let peers = hosts::tailscale_status(TAILSCALE_TIMEOUT)
                .map(|status| hosts::tailscale_peers(&status))
                .unwrap_or_default();
            let hosts = (peers.into_iter())
                .map(|peer| {
                    let mut about = format!("tailscale, online, {}", peer.os);
                    if peer.host != peer.address {
                        about.push_str(", ");
                        about.push_str(&peer.address);
                    }
                    (peer.host, about)
                })
                .collect();
            send(Found {
                picker,
                hosts,
                last: true,
            });
        });
    if let Err(err) = spawned {
        log::warn!("no thread to find machines to connect to: {err}");
    }
}

/// A machine to pick, and where it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    host: String,
    about: String,
}

/// What a key did to the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing to show.
    Nothing,
    /// Something shown changed.
    Changed,
    /// It's to close.
    Close,
    /// It's to close, connecting to this machine.
    Connect(String),
}

/// The picker's state.
#[derive(Debug)]
pub(crate) struct HostPicker {
    /// Its number, which the machines found for it carry.
    id: u64,
    purpose: Purpose,
    query: String,
    candidates: Vec<Candidate>,
    /// Each candidate's text, which what's typed is matched against.
    items: Items,
    ranking: Ranking,
    /// The row picked, among the candidates matching and the machine
    /// typed, if it's offered.
    selected: usize,
    /// The first row shown, likewise.
    top: usize,
    /// How many rows the list showed last, for paging.
    rows: usize,
    /// How many lines the error took last, above the list.
    error_rows: usize,
    /// Whether more machines are still to come.
    gathering: bool,
    /// Why connecting failed, last time.
    error: Option<String>,
}

impl HostPicker {
    /// An empty picker, numbered `id`, for `purpose`, matching case as
    /// `case` says, saying `error`, if connecting failed. Its machines
    /// come by [`found`](Self::found).
    pub(crate) fn new(id: u64, purpose: Purpose, case: Case, error: Option<String>) -> Self {
        Self {
            id,
            purpose,
            query: String::new(),
            candidates: Vec::new(),
            items: Items::default(),
            ranking: Ranking::new(case, false),
            selected: 0,
            top: 0,
            rows: 1,
            error_rows: 0,
            gathering: true,
            error,
        }
    }

    pub(crate) fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// Takes in machines found for it. One found twice is listed once,
    /// saying everywhere it was found. Returns whether they were its.
    pub(crate) fn found(&mut self, found: Found) -> bool {
        if found.picker != self.id {
            return false;
        }
        for (host, about) in found.hosts {
            match self.candidates.iter_mut().find(|known| known.host == host) {
                Some(known) if !known.about.split(" · ").any(|said| said == about) => {
                    known.about.push_str(" · ");
                    known.about.push_str(&about);
                }
                Some(_) => {}
                None => self.candidates.push(Candidate { host, about }),
            }
        }
        if found.last {
            self.gathering = false;
        }
        self.items.clear();
        for candidate in &self.candidates {
            self.items.push(&text(candidate));
        }
        self.ranking.clear();
        self.ranking.add(&self.items);
        true
    }

    /// What's typed, if it names a machine none of the candidates is, to
    /// connect to as typed, and the row it's offered in: first if it has
    /// a user or a port, which only a whole address has, or nothing else
    /// matches, and otherwise last.
    fn typed(&self) -> Option<(&str, usize)> {
        let typed = self.query.trim();
        if !hosts::is_host(typed) || self.candidates.iter().any(|known| known.host == typed) {
            return None;
        }
        let first = typed.contains(['@', ':']) || self.ranking.len() == 0;
        Some((typed, if first { 0 } else { self.ranking.len() }))
    }

    /// How many rows there are to pick from.
    fn len(&self) -> usize {
        usize::from(self.typed().is_some()) + self.ranking.len()
    }

    /// What's in row `row`: the machine typed, or a candidate.
    fn row(&self, row: usize) -> Picked {
        match self.typed() {
            Some((typed, at)) if at == row => Picked::Typed(typed.to_owned()),
            Some((_, 0)) => Picked::Ranked(row - 1),
            _ => Picked::Ranked(row),
        }
    }

    /// The machine in row `row`.
    fn host(&mut self, row: usize) -> Option<String> {
        match self.row(row) {
            Picked::Typed(typed) => Some(typed),
            Picked::Ranked(rank) => {
                let index = self.ranking.get(rank)?;
                Some(self.candidates[index].host.clone())
            }
        }
    }

    /// Handles a key.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> Outcome {
        let modifiers = press.modifiers;
        // Not with Shift: Ctrl+Shift+C and the like are the terminal's.
        let ctrl = match press.key {
            Key::Character(text) if modifiers.ctrl && !modifiers.alt && !modifiers.shift => {
                text.chars().next().map(|ch| ch.to_ascii_lowercase())
            }
            _ => None,
        };
        match (press.key, ctrl) {
            (Key::Named(NamedKey::Escape), _) | (_, Some('c')) => Outcome::Close,
            (Key::Named(NamedKey::Enter), _) => self.connect(),
            (Key::Named(NamedKey::ArrowDown), _) | (_, Some('n')) => self.go(1),
            (Key::Named(NamedKey::ArrowUp), _) | (_, Some('p')) => self.go(-1),
            (Key::Named(NamedKey::PageDown), _) => self.go(self.page()),
            (Key::Named(NamedKey::PageUp), _) => self.go(-self.page()),
            (Key::Named(NamedKey::Backspace), _) => {
                if self.query.pop().is_none() {
                    return Outcome::Nothing;
                }
                self.typed_changed()
            }
            (_, Some('u')) => {
                self.query.clear();
                self.typed_changed()
            }
            (_, Some('w')) => {
                let kept = (self.query.trim_end().rfind(' ')).map_or(0, |space| space + 1);
                self.query.truncate(kept);
                self.typed_changed()
            }
            _ if modifiers.ctrl || modifiers.alt => Outcome::Nothing,
            _ => match press.text {
                Some(text) => self.paste(text),
                None => Outcome::Nothing,
            },
        }
    }

    /// Types `text`.
    pub(crate) fn paste(&mut self, text: &str) -> Outcome {
        let before = self.query.len();
        (self.query).extend(text.chars().filter(|ch| !ch.is_control()));
        if self.query.len() == before {
            return Outcome::Nothing;
        }
        self.typed_changed()
    }

    /// What's typed changed: the list is matched against it again.
    fn typed_changed(&mut self) -> Outcome {
        self.selected = 0;
        self.top = 0;
        self.ranking.set_query(&self.query, &self.items);
        Outcome::Changed
    }

    /// Moves what's picked by `by` rows.
    fn go(&mut self, by: isize) -> Outcome {
        let last = self.len().saturating_sub(1);
        let selected = self.selected.saturating_add_signed(by).min(last);
        if selected == self.selected {
            return Outcome::Nothing;
        }
        self.selected = selected;
        Outcome::Changed
    }

    /// Moves what's picked by `by` rows, as the mouse's wheel does.
    pub(crate) fn scroll(&mut self, by: isize) -> Outcome {
        self.go(by)
    }

    /// Picks the machine shown in row `row` of the list, as a click does,
    /// and connects to it if `open`, as a double click does.
    pub(crate) fn click(&mut self, row: usize, open: bool) -> Outcome {
        let Some(row) = row.checked_sub(self.error_rows) else {
            return Outcome::Nothing;
        };
        let picked = self.top + row;
        if picked >= self.len() {
            return Outcome::Nothing;
        }
        self.selected = picked;
        if open {
            return self.connect();
        }
        Outcome::Changed
    }

    /// How many rows a page is.
    fn page(&self) -> isize {
        isize::try_from(self.rows.max(1)).unwrap_or(1)
    }

    /// Connects to the machine picked, if any. What's typed that names no
    /// machine, and nothing else matches, is said to be no machine.
    fn connect(&mut self) -> Outcome {
        if let Some(host) = self.host(self.selected) {
            return Outcome::Connect(host);
        }
        if self.query.trim().is_empty() {
            return Outcome::Nothing;
        }
        self.error = Some(format!(
            "{} isn't a machine to connect to: type a name, user@host or user@host:port",
            self.query.trim()
        ));
        Outcome::Changed
    }

    /// What to draw, for a list `rows` things tall and `cols` cells wide.
    /// Why connecting failed, if it did, goes above the machines.
    pub(crate) fn view(&mut self, (rows, cols): (usize, usize)) -> FinderView {
        let mut shown: Vec<Row> = (self.error.as_deref())
            .map(|error| wrap(error, cols))
            .unwrap_or_default();
        // Room for one machine at least.
        shown.truncate(rows.saturating_sub(1).max(1));
        self.error_rows = shown.len();
        let rows = rows.saturating_sub(self.error_rows).max(1);
        self.rows = rows;
        let count = self.len();
        self.selected = self.selected.min(count.saturating_sub(1));
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        for row in self.top..count.min(self.top + rows) {
            match self.row(row) {
                Picked::Typed(typed) => {
                    let text = format!("{typed}  as typed");
                    let typed_chars = typed.chars().count();
                    let inks: Vec<Ink> = (0..text.chars().count())
                        .map(|at| {
                            if at < typed_chars {
                                Ink::Plain
                            } else {
                                Ink::Muted
                            }
                        })
                        .collect();
                    shown.push(fit(&text, &inks, cols, false));
                }
                Picked::Ranked(rank) => {
                    if let Some(index) = self.ranking.get(rank) {
                        let candidate = &self.candidates[index];
                        let text = self.items.get(index);
                        let accents = self.ranking.matched_chars(text);
                        let host_chars = candidate.host.chars().count();
                        let inks: Vec<Ink> = (0..text.chars().count())
                            .map(|at| {
                                let at_u32 = u32::try_from(at).unwrap_or(u32::MAX);
                                if accents.binary_search(&at_u32).is_ok() {
                                    Ink::Accent
                                } else if at < host_chars {
                                    Ink::Plain
                                } else {
                                    Ink::Muted
                                }
                            })
                            .collect();
                        shown.push(fit(text, &inks, cols, false));
                    }
                }
            }
        }
        let more = if self.gathering { "…" } else { "" };
        FinderView {
            title: "Connect to",
            place: None,
            query: format!("{}▏", self.query),
            count: format!("{} / {}{more}", self.ranking.len(), self.candidates.len()),
            selected: (count > 0).then(|| self.error_rows + self.selected - self.top),
            rows: shown,
            preview: None,
            empty: None,
        }
    }
}

/// What's in a row of the picker.
#[derive(Debug)]
enum Picked {
    /// The machine typed.
    Typed(String),
    /// The candidate ranked so, from 0 for the best.
    Ranked(usize),
}

/// A candidate's text, as it's matched and shown: the machine, then
/// where it was found.
fn text(candidate: &Candidate) -> String {
    format!("{}  {}", candidate.host, candidate.about)
}

/// `text` in the warning colour, broken between words into lines of at
/// most `cols` cells.
fn wrap(text: &str, cols: usize) -> Vec<Row> {
    let cols = cols.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let fits = line.chars().count() + 1 + word.chars().count() <= cols;
        if !line.is_empty() && !fits {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    (lines.iter())
        .map(|line| fit(line, &vec![Ink::Warning; line.chars().count()], cols, false))
        .collect()
}

#[cfg(test)]
mod tests {
    use winit::keyboard::KeyLocation;

    use super::*;
    use crate::input::Modifiers;

    fn picker(error: Option<&str>) -> HostPicker {
        let case = Case::of(&crate::options::Options::default());
        let mut picker = HostPicker::new(7, Purpose::ThisWindow, case, error.map(str::to_owned));
        let found = |hosts: &[(&str, &str)], last| Found {
            picker: 7,
            hosts: (hosts.iter())
                .map(|&(host, about)| (host.to_owned(), about.to_owned()))
                .collect(),
            last,
        };
        assert!(picker.found(found(
            &[
                ("jack-ubuntu-desktop", "recent"),
                ("server", "recent"),
                ("work-box", "ssh config"),
                ("server", "ssh config"),
            ],
            false
        )));
        assert!(picker.found(found(
            &[
                ("build-server", "tailscale, online, linux"),
                ("jack-ubuntu-desktop", "tailscale, online, linux"),
            ],
            true
        )));
        picker
    }

    fn press(picker: &mut HostPicker, key: &Key, ctrl: bool) -> Outcome {
        let text = match key {
            Key::Character(text) if !ctrl => Some(text.to_string()),
            _ => None,
        };
        picker.key(&KeyPress {
            key,
            text: text.as_deref(),
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers {
                ctrl,
                ..Modifiers::default()
            },
        })
    }

    fn named(picker: &mut HostPicker, key: NamedKey) -> Outcome {
        press(picker, &Key::Named(key), false)
    }

    fn type_text(picker: &mut HostPicker, text: &str) {
        for ch in text.chars() {
            press(picker, &Key::Character(ch.to_string().into()), false);
        }
    }

    /// The text of each row shown, in a list ten tall and wide.
    fn shown(picker: &mut HostPicker) -> Vec<String> {
        let view = picker.view((10, 60));
        view.rows.into_iter().map(|row| row.text).collect()
    }

    #[test]
    fn machines_are_listed_once_recent_first_saying_where_they_were_found() {
        let mut picker = picker(None);
        assert_eq!(
            shown(&mut picker),
            [
                "jack-ubuntu-desktop  recent · tailscale, online, linux",
                "server  recent · ssh config",
                "work-box  ssh config",
                "build-server  tailscale, online, linux",
            ]
        );
        let view = picker.view((10, 60));
        assert_eq!(view.title, "Connect to");
        assert_eq!(view.count, "4 / 4");
        assert_eq!(view.selected, Some(0));
        assert_eq!(
            named(&mut picker, NamedKey::Enter),
            Outcome::Connect("jack-ubuntu-desktop".into())
        );
    }

    #[test]
    fn whats_found_for_another_picker_is_dropped_and_the_count_says_more_is_coming() {
        let case = Case::of(&crate::options::Options::default());
        let mut picker = HostPicker::new(1, Purpose::NewWindow, case, None);
        assert!(!picker.found(Found {
            picker: 2,
            hosts: vec![("old".into(), "recent".into())],
            last: true,
        }));
        assert_eq!(picker.view((10, 60)).count, "0 / 0…");
        assert!(picker.found(Found {
            picker: 1,
            hosts: vec![("desk".into(), "recent".into())],
            last: false,
        }));
        assert_eq!(picker.view((10, 60)).count, "1 / 1…");
        assert_eq!(picker.purpose(), Purpose::NewWindow);
    }

    #[test]
    fn typing_matches_fuzzily_and_keys_move_between_machines() {
        let mut picker = picker(None);
        type_text(&mut picker, "srv");
        assert_eq!(
            shown(&mut picker),
            [
                "server  recent · ssh config",
                "build-server  tailscale, online, linux",
                "srv  as typed"
            ]
        );
        assert_eq!(named(&mut picker, NamedKey::ArrowDown), Outcome::Changed);
        assert_eq!(
            named(&mut picker, NamedKey::Enter),
            Outcome::Connect("build-server".into())
        );
        assert_eq!(named(&mut picker, NamedKey::ArrowDown), Outcome::Changed);
        assert_eq!(
            named(&mut picker, NamedKey::ArrowDown),
            Outcome::Nothing,
            "the last"
        );
        assert_eq!(
            press(&mut picker, &Key::Character("p".into()), true),
            Outcome::Changed
        );
        assert_eq!(
            press(&mut picker, &Key::Character("p".into()), true),
            Outcome::Changed
        );
        assert_eq!(
            named(&mut picker, NamedKey::Enter),
            Outcome::Connect("server".into())
        );
        // Where it was found is matched too.
        press(&mut picker, &Key::Character("u".into()), true);
        type_text(&mut picker, "tailscale");
        assert_eq!(shown(&mut picker).len(), 3, "and the name typed");
        assert_eq!(named(&mut picker, NamedKey::Escape), Outcome::Close);
        assert_eq!(
            press(&mut picker, &Key::Character("c".into()), true),
            Outcome::Close
        );
    }

    #[test]
    fn a_machine_typed_that_isnt_listed_is_offered_to_connect_to_as_typed() {
        let mut picker = picker(None);
        type_text(&mut picker, "jack@server:2222");
        let rows = shown(&mut picker);
        assert_eq!(rows[0], "jack@server:2222  as typed");
        assert_eq!(
            named(&mut picker, NamedKey::Enter),
            Outcome::Connect("jack@server:2222".into())
        );
        // One listed already isn't offered twice.
        press(&mut picker, &Key::Character("u".into()), true);
        type_text(&mut picker, "server");
        assert_eq!(shown(&mut picker)[0], "server  recent · ssh config");
        // A few letters of a machine's name pick it, with what's typed
        // offered last.
        press(&mut picker, &Key::Character("u".into()), true);
        type_text(&mut picker, "desk");
        assert_eq!(
            shown(&mut picker),
            [
                "jack-ubuntu-desktop  recent · tailscale, online, linux",
                "desk  as typed"
            ]
        );
        assert_eq!(
            named(&mut picker, NamedKey::Enter),
            Outcome::Connect("jack-ubuntu-desktop".into())
        );
        assert_eq!(named(&mut picker, NamedKey::ArrowDown), Outcome::Changed);
        assert_eq!(
            named(&mut picker, NamedKey::Enter),
            Outcome::Connect("desk".into())
        );
        // A name nothing matches comes first.
        type_text(&mut picker, "zz");
        assert_eq!(shown(&mut picker), ["deskzz  as typed"]);
        assert_eq!(
            named(&mut picker, NamedKey::Enter),
            Outcome::Connect("deskzz".into())
        );
        // Ctrl+W deletes the last word, Backspace a character.
        press(&mut picker, &Key::Character("w".into()), true);
        assert_eq!(picker.query, "");
        assert_eq!(named(&mut picker, NamedKey::Backspace), Outcome::Nothing);
    }

    #[test]
    fn what_names_no_machine_is_said_to_be_none_and_never_connected_to() {
        let mut picker = picker(None);
        type_text(&mut picker, "-oProxyCommand=x");
        assert_eq!(shown(&mut picker), Vec::<String>::new());
        assert_eq!(named(&mut picker, NamedKey::Enter), Outcome::Changed);
        let rows = shown(&mut picker);
        assert!(
            rows[0].starts_with("-oProxyCommand=x isn't a machine"),
            "{rows:?}"
        );
        // Nothing typed, nothing listed: Enter does nothing.
        let case = Case::of(&crate::options::Options::default());
        let mut empty = HostPicker::new(1, Purpose::ThisWindow, case, None);
        assert_eq!(named(&mut empty, NamedKey::Enter), Outcome::Nothing);
    }

    #[test]
    fn why_connecting_failed_goes_above_the_machines_which_can_still_be_picked() {
        let mut picker = picker(Some(
            "couldn't reach clackatoo on server: ssh: connect to host server port 22: Connection refused",
        ));
        let view = picker.view((6, 40));
        let texts: Vec<&str> = view.rows.iter().map(|row| row.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "couldn't reach clackatoo on server: ssh:",
                "connect to host server port 22:",
                "Connection refused",
                "jack-ubuntu-desktop  recent · tailscale…",
                "server  recent · ssh config",
                "work-box  ssh config",
            ]
        );
        assert_eq!(view.rows[0].runs, [(view.rows[0].text.len(), Ink::Warning)]);
        // The first machine is picked, below the error.
        assert_eq!(view.selected, Some(3));
        // Clicking the error picks nothing; a machine, it.
        assert_eq!(picker.click(1, true), Outcome::Nothing);
        assert_eq!(picker.click(4, false), Outcome::Changed);
        assert_eq!(picker.view((6, 40)).selected, Some(4));
        assert_eq!(picker.click(5, true), Outcome::Connect("work-box".into()));
        // Paging and scrolling keep to the machines.
        assert_eq!(named(&mut picker, NamedKey::PageDown), Outcome::Changed);
        let paged = picker.view((6, 40));
        assert_eq!(paged.rows[5].text, "build-server  tailscale, online, linux");
        assert_eq!(paged.selected, Some(5));
        assert_eq!(picker.scroll(-3), Outcome::Changed);
        assert_eq!(picker.view((6, 40)).selected, Some(3));
    }
}
