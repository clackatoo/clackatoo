//! Hyperlinks: text a program printed as a link, with OSC 8.
//!
//! `OSC 8 ; params ; URI ST` starts a link and `OSC 8 ; ; ST` ends it. Text
//! printed in between links to the URI, the way `ls --hyperlink`, gcc,
//! systemd and git print file names and documentation. `params` is a list
//! of `key=value` separated by `:`, of which only `id` means anything: cells
//! with the same `id` and URI are one link, even when they aren't next to
//! each other, as when a program draws a link that wraps in a pane of its
//! own.
//!
//! A link is part of a cell's [`Style`](crate::Style), as a [`LinkId`] into
//! the terminal's table of links. So links cost nothing in rows that have
//! none, and go everywhere styles go for free: into history, through
//! rewrapping, into snapshots and onto the wire. One table serves both
//! screens and history.
//!
//! IDs are never reused while the terminal lives. A window keeps what each
//! ID links to, learnt from the updates that showed it, so a reused ID
//! could show the old link on a row the window fetched before. Instead,
//! links no row refers to any more are forgotten from time to time, and
//! their IDs with them.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

/// The longest URI kept, in bytes. Longer links print as plain text. As
/// long as kitty's, and far longer than any real one.
pub(crate) const MAX_URI_LEN: usize = 2083 * 4;

/// The longest `id` kept, in bytes. Longer ones are ignored, so the link
/// stands alone.
pub(crate) const MAX_ID_LEN: usize = 256;

/// The most bytes the table may take, counting each link's URI and ID and
/// [`ENTRY_BYTES`] besides. Past this, new links print as plain text
/// until rows holding old ones have gone.
const MAX_TABLE_BYTES: usize = 16 * 1024 * 1024;

/// What a link costs the table beyond its URI and ID: two map entries,
/// two allocations' headers and the boxes pointing to them, roughly.
const ENTRY_BYTES: usize = 150;

/// The most links the table holds, however short their URIs.
const MAX_LINKS: usize = 65_536;

/// The fewest new links that wait for the next sweep.
const MIN_SWEEP: usize = 256;

/// A link in a terminal's table of links.
///
/// Numbered from 1 so that `Option<LinkId>` is as small as the number, and
/// never reused while the terminal lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LinkId(pub NonZeroU32);

impl LinkId {
    /// The link numbered `number`, unless it's 0, which none is.
    #[must_use]
    pub fn new(number: u32) -> Option<Self> {
        NonZeroU32::new(number).map(Self)
    }

    /// The link's number.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// What a link points to: its URI, and the `id` the program gave it, if
/// any.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    uri: Box<str>,
    id: Option<Box<str>>,
}

impl Hyperlink {
    pub(crate) fn new(uri: &str, id: Option<&str>) -> Self {
        Self {
            uri: uri.into(),
            id: id.map(Into::into),
        }
    }

    /// The URI it links to.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The `id` the program gave it. Cells of a link with an `id` belong
    /// together wherever they are; those of one without only when they're
    /// next to each other.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// What the link costs the table, in bytes.
    fn bytes(&self) -> usize {
        ENTRY_BYTES + self.uri.len() + self.id.as_ref().map_or(0, |id| id.len())
    }
}

/// A terminal's links, by ID.
#[derive(Clone, Debug)]
pub(crate) struct Links {
    by_id: HashMap<LinkId, Hyperlink>,
    /// The ID of each link in the table, so a program that prints the same
    /// link again, as one redrawing its screen does, gets the same ID.
    ids: HashMap<Hyperlink, LinkId>,
    /// The number the next new link gets.
    next: u32,
    /// What the links held cost, in bytes (see [`Hyperlink::bytes`]).
    bytes: usize,
    /// New links added or refused since the last sweep.
    new: usize,
    /// How many new links wait for the next sweep: as many as the last
    /// sweep looked at styles, or kept links, so sweeping costs little for
    /// each link a program prints, however many it prints.
    budget: usize,
}

impl Default for Links {
    fn default() -> Self {
        Self {
            by_id: HashMap::new(),
            ids: HashMap::new(),
            next: 1,
            bytes: 0,
            new: 0,
            budget: MIN_SWEEP,
        }
    }
}

impl Links {
    /// The link `id`, if the table has it.
    pub(crate) fn get(&self, id: LinkId) -> Option<&Hyperlink> {
        self.by_id.get(&id)
    }

    /// Whether adding a link may first need [`sweep`](Self::sweep): as
    /// many new links have been added or refused since the last as it
    /// waits for.
    pub(crate) fn wants_sweep(&self) -> bool {
        self.new >= self.budget
    }

    /// Forgets every link not in `live`, found by looking at `scanned`
    /// styles. The next sweep waits for as many new links as this one cost,
    /// or as there are links kept, whichever is more.
    pub(crate) fn sweep(&mut self, live: &HashSet<LinkId>, scanned: usize) {
        self.by_id.retain(|id, _| live.contains(id));
        self.ids.retain(|_, id| live.contains(id));
        self.bytes = self.by_id.values().map(Hyperlink::bytes).sum();
        self.budget = self.by_id.len().max(scanned).max(MIN_SWEEP);
        self.new = 0;
    }

    /// The ID of the link to `uri` with `id`, adding it if it's new. `None`
    /// if there's no room for it, so its text prints as plain text.
    pub(crate) fn intern(&mut self, uri: &str, id: Option<&str>) -> Option<LinkId> {
        let link = Hyperlink::new(uri, id);
        if let Some(&known) = self.ids.get(&link) {
            return Some(known);
        }
        self.new += 1;
        if self.bytes + link.bytes() > MAX_TABLE_BYTES || self.by_id.len() >= MAX_LINKS {
            return None;
        }
        let new = LinkId::new(self.next)?;
        self.next = self.next.checked_add(1)?;
        self.bytes += link.bytes();
        self.by_id.insert(new, link.clone());
        self.ids.insert(link, new);
        Some(new)
    }

    /// Every link, and the number the next new one gets, for a snapshot.
    pub(crate) fn parts(&self) -> (impl Iterator<Item = (LinkId, &Hyperlink)>, u32) {
        (self.by_id.iter().map(|(&id, link)| (id, link)), self.next)
    }

    /// A table holding `links`, whose next new link is numbered `next`.
    pub(crate) fn from_parts(
        links: impl IntoIterator<Item = (LinkId, Hyperlink)>,
        next: u32,
    ) -> Self {
        let mut table = Self::default();
        for (id, link) in links {
            table.bytes += link.bytes();
            table.ids.insert(link.clone(), id);
            table.by_id.insert(id, link);
        }
        let past = table
            .by_id
            .keys()
            .map(|id| id.get().saturating_add(1))
            .max();
        table.next = next.max(past.unwrap_or(1)).max(1);
        table.budget = table.by_id.len().max(MIN_SWEEP);
        table
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.by_id.len()
    }
}

/// `uri` with the host left out if it's a `file://` URI naming
/// `local_host` (in any case), as `ls --hyperlink` names the machine it
/// runs on: `file:///path`, which means this machine wherever it's read.
pub(crate) fn local_file_uri<'a>(uri: &'a str, local_host: Option<&str>) -> Cow<'a, str> {
    let named = local_host.filter(|host| !host.is_empty()).and_then(|host| {
        let rest = uri.strip_prefix("file://")?;
        let (named, path) = rest.split_at(rest.find('/')?);
        named.eq_ignore_ascii_case(host).then_some(path)
    });
    match named {
        Some(path) => Cow::Owned(format!("file://{path}")),
        None => Cow::Borrowed(uri),
    }
}

/// What `OSC 8 ; params ; URI` asks for: the link's URI, and the `id` among
/// `params`. `None` ends the link, as an empty URI does; a URI or ID too
/// long to keep ends it too, so the text prints as plain text.
pub(crate) fn parse(params: &[u8], uri: &[u8]) -> Option<(String, Option<String>)> {
    if uri.is_empty() || uri.len() > MAX_URI_LEN {
        return None;
    }
    // URIs are ASCII, but tolerate a program that didn't encode its UTF-8.
    let uri = String::from_utf8_lossy(uri).into_owned();
    let id = params
        .split(|&byte| byte == b':')
        .find_map(|param| param.strip_prefix(b"id="))
        .filter(|id| !id.is_empty() && id.len() <= MAX_ID_LEN)
        .map(|id| String::from_utf8_lossy(id).into_owned());
    Some((uri, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_link_printed_again_keeps_its_id() {
        let mut links = Links::default();
        let a = links.intern("https://a", None).unwrap();
        let b = links.intern("https://a", Some("x")).unwrap();
        assert_ne!(a, b);
        assert_eq!(links.intern("https://a", None), Some(a));
        assert_eq!(links.intern("https://a", Some("x")), Some(b));
        assert_eq!(links.get(b).unwrap().id(), Some("x"));
    }

    #[test]
    fn ids_are_never_reused_after_a_sweep() {
        let mut links = Links::default();
        let a = links.intern("https://a", None).unwrap();
        let b = links.intern("https://b", None).unwrap();
        links.sweep(&HashSet::from([b]), 2);
        assert_eq!(links.get(a), None);
        let again = links.intern("https://a", None).unwrap();
        assert!(again > b, "{again:?} reuses an old number");
    }

    #[test]
    fn params_give_the_id_and_overlong_links_are_dropped() {
        assert_eq!(
            parse(b"foo=bar:id=7", b"https://x"),
            Some(("https://x".to_owned(), Some("7".to_owned())))
        );
        assert_eq!(
            parse(b"", b"https://x"),
            Some(("https://x".to_owned(), None))
        );
        assert_eq!(parse(b"id=1", b""), None);
        assert_eq!(parse(b"", &vec![b'a'; MAX_URI_LEN + 1]), None);
        let long_id = [b"id=".as_slice(), &[b'i'; MAX_ID_LEN + 1]].concat();
        assert_eq!(parse(&long_id, b"https://x").unwrap().1, None);
    }

    #[test]
    fn a_full_table_refuses_new_links() {
        let mut links = Links::default();
        let big = "x".repeat(MAX_URI_LEN);
        let mut made = 0;
        while links.intern(&format!("{made}{big}"), None).is_some() {
            made += 1;
        }
        assert!(links.bytes <= MAX_TABLE_BYTES);
        // All of them still in use: the next sweep waits until as many
        // more have been refused as it may hold.
        let live = links.by_id.keys().copied().collect();
        links.sweep(&live, made);
        assert_eq!(links.len(), made);
        assert!(!links.wants_sweep());
        for refused in 0..links.budget {
            assert!(!links.wants_sweep());
            assert!(links.intern(&format!("x{refused}{big}"), None).is_none());
        }
        assert!(links.wants_sweep());
    }

    #[test]
    fn file_uris_naming_this_machine_lose_the_name() {
        let local = |uri| local_file_uri(uri, Some("Desk")).into_owned();
        assert_eq!(local("file://desk/home/j/a.rs"), "file:///home/j/a.rs");
        assert_eq!(
            local("file://laptop/home/j/a.rs"),
            "file://laptop/home/j/a.rs"
        );
        assert_eq!(local("file:///etc/hosts"), "file:///etc/hosts");
        assert_eq!(local("https://desk/x"), "https://desk/x");
        assert_eq!(local_file_uri("file://desk/a", None), "file://desk/a");
    }

    #[test]
    fn short_links_are_capped_by_count_and_by_their_cost_as_entries() {
        let mut links = Links::default();
        let mut made = 0;
        while links.intern(&format!("h:{made}"), None).is_some() {
            made += 1;
        }
        assert_eq!(made, MAX_LINKS);
        // Each costs far more than its few bytes of URI.
        assert!(links.bytes >= MAX_LINKS * ENTRY_BYTES);
        assert!(links.bytes <= MAX_TABLE_BYTES);
    }

    #[test]
    fn the_next_sweep_waits_for_as_many_links_as_this_one_looked_at_styles() {
        let mut links = Links::default();
        for n in 0..MIN_SWEEP {
            links.intern(&format!("h:{n}"), None);
        }
        assert!(links.wants_sweep());
        // History full of styles, and no link kept: the next sweep waits
        // until as many new links have come.
        links.sweep(&HashSet::new(), 100_000);
        for n in 0..99_999 {
            links.intern(&format!("m:{n}"), None);
        }
        assert!(!links.wants_sweep());
        links.intern("m:last", None);
        assert!(links.wants_sweep());
    }
}
