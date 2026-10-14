//! Folders' listings as text, to edit in an editor, as oil.nvim edits
//! them: an entry a line, folders ending `/`. What the edits come to is
//! worked out as file operations, in an order that's safe to do them in.
//!
//! Each entry's line starts with a mark, hidden from view, saying which
//! entry it was. So a line changed is a rename, not a delete and a new
//! file, and a line cut from one folder's listing and put in another's is
//! a move. The mark is made of characters from Unicode's last private-use
//! plane, which no file name has and no key types: `:s` on names, digits
//! or spaces never touches one, and a mark damaged is always refused,
//! never read as another. It names the editing session too, a number
//! picked at random, so a line pasted from another session's listing is
//! refused rather than read as one of this one's entries.
//!
//! What the lines mean, against the entries the listings were made of:
//!
//! | Lines | Mean |
//! |---|---|
//! | An entry's, as it was | Nothing |
//! | An entry's, changed | A rename, or a move if its path goes elsewhere |
//! | An entry's, twice or more | Copies, beside the one that stays or moves |
//! | None of an entry's | A delete, to the trash |
//! | One with no mark | A new file, or a new folder if it ends `/` |
//! | `..` with no mark | Nothing: it's the way up, and names nothing |
//!
//! A listing of any folder but `/` starts with a `..` line, which Enter
//! on goes up, as `-` does. It has no mark, so it can't name the folder
//! above: deleted, moved, copied or left alone, it comes to nothing, and
//! so does any other line that's `..` or `../` alone without a mark. A
//! marked line renamed `..`, or `..` in a path that ends up at the
//! listing's folder or one holding it, is still refused.
//!
//! A name is read from its listing's folder: `sub/x` is in the folder
//! `sub`, made if it isn't there, `../x` in the folder above, and `/x`
//! from the root of everything. Paths are where things end up: a line
//! can't put something in a folder that's itself being moved or deleted.
//!
//! What can't be done as asked is refused, saying which line asks it,
//! before anything is done: two lines with one name, a name something
//! already has, a mark damaged or from another listing.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use clackatoo_protocol::{Entry, EntryKind, Expect, FileOp, FileStamp};
use clackatoo_text::Rope;

#[cfg(test)]
mod tests;

/// The first character of a mark...
const MARK_START: char = '\u{10FFFD}';

/// ...the one between the session's number and the entry's, both in
/// hexadecimal...
const MARK_SPLIT: char = '\u{10FFFB}';

/// ...and its last.
const MARK_END: char = '\u{10FFFC}';

/// The most digits a mark's session number has.
const SESSION_DIGITS: usize = 8;

/// The character for hexadecimal digit 0, the next fifteen being 1 to F.
const DIGIT_ZERO: u32 = 0x10_0000;

/// The most digits a mark's number has.
const MOST_DIGITS: usize = 12;

/// An entry put in a listing, as it was then: what its mark names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Original {
    /// Its path.
    pub(crate) path: String,
    pub(crate) kind: EntryKind,
    /// Which file it was, as it was, if it's a file.
    pub(crate) stamp: Option<FileStamp>,
    /// Its listing has been made again since, as its folder changed on
    /// disk, without it: a line of it pasted, as yanked before, copies it,
    /// and none of it deletes nothing, as it's no longer what's listed.
    pub(crate) retired: bool,
}

impl Original {
    /// What a file operation on it expects to find.
    pub(crate) fn expect(&self) -> Expect {
        Expect {
            kind: self.kind,
            stamp: self.stamp,
        }
    }
}

/// One folder's listing, as edited.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Folder<'a> {
    /// The folder's path.
    pub(crate) dir: &'a str,
    /// What was in the folder when its listing was made, shown or not.
    pub(crate) entries: &'a [Entry],
    /// The listing's text.
    pub(crate) text: &'a str,
}

/// One thing to do to files, as part of what listings' edits come to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Make an empty file, or a folder if `dir`.
    Create {
        path: String,
        dir: bool,
    },
    Copy {
        from: String,
        to: String,
    },
    /// A move or rename: `aside` if it's to a name of its own, out of the
    /// way of another moving to where it was, as swapping two names needs.
    Move {
        from: String,
        to: String,
        aside: bool,
    },
    /// A delete, to the trash, of what was there, as `kind` says.
    Delete {
        path: String,
        kind: EntryKind,
    },
}

impl Step {
    /// The file operation that does it, which never writes over anything,
    /// and is refused if what it starts from isn't as `expect` says.
    pub(crate) fn op(&self, expect: Option<Expect>) -> FileOp {
        match self {
            Self::Create { path, dir } => FileOp::Create {
                path: path.clone(),
                dir: *dir,
            },
            Self::Copy { from, to } => FileOp::Copy {
                from: from.clone(),
                to: to.clone(),
                overwrite: None,
                expect,
            },
            Self::Move { from, to, .. } => FileOp::Move {
                from: from.clone(),
                to: to.clone(),
                overwrite: None,
                expect,
            },
            Self::Delete { path, .. } => FileOp::Delete {
                path: path.clone(),
                trash: true,
                expect,
            },
        }
    }

    /// The path it starts from, if it starts from one.
    pub(crate) fn source(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Copy { from, .. } | Self::Move { from, .. } => Some(from),
            Self::Delete { path, .. } => Some(path),
        }
    }
}

/// Why listings' edits can't be done, and where, if one line says so: the
/// listing, by its place among those given, and the line, from 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    pub(crate) at: Option<(usize, u64)>,
    pub(crate) why: String,
}

/// The mark that starts the line of entry `id` in session `session`.
pub(crate) fn mark(session: u32, id: usize) -> String {
    let mut mark = String::from(MARK_START);
    push_digits(&mut mark, &format!("{session:x}"));
    mark.push(MARK_SPLIT);
    push_digits(&mut mark, &format!("{id:x}"));
    mark.push(MARK_END);
    mark
}

/// Pushes the characters for the hexadecimal digits `hex` on `mark`.
fn push_digits(mark: &mut String, hex: &str) {
    for digit in hex.chars() {
        let value = digit.to_digit(16).expect("a hexadecimal digit");
        mark.push(char::from_u32(DIGIT_ZERO + value).expect("a private-use character"));
    }
}

/// The entry of session `session` that `line`'s mark names, if it starts
/// with a whole one of that session's.
pub(crate) fn id_of(session: u32, line: &str) -> Option<usize> {
    read_mark(line)
        .filter(|&(of, _, _)| of == session)
        .map(|(_, id, _)| id)
}

/// How many bytes of `line` its mark takes, if it starts with one, for an
/// editor to hide.
pub(crate) fn mark_len(line: &str) -> usize {
    if !line.starts_with(MARK_START) {
        return 0;
    }
    read_mark(line).map_or(0, |(_, _, rest)| line.len() - rest.len())
}

/// The session and entry `line`'s mark names, and the rest of the line,
/// if it starts with a whole mark.
fn read_mark(line: &str) -> Option<(u32, usize, &str)> {
    let rest = line.strip_prefix(MARK_START)?;
    let (session, rest) = read_digits(rest, MARK_SPLIT, SESSION_DIGITS)?;
    let (id, rest) = read_digits(rest, MARK_END, MOST_DIGITS)?;
    Some((u32::try_from(session).ok()?, id, rest))
}

/// The number written in digits at the start of `text`, up to `end`, of
/// no more than `most` digits, and what's after `end`.
fn read_digits(text: &str, end: char, most: usize) -> Option<(usize, &str)> {
    let mut number: usize = 0;
    for (count, (at, ch)) in text.char_indices().enumerate() {
        if ch == end {
            return (count > 0).then(|| (number, &text[at + ch.len_utf8()..]));
        }
        let digit = u32::from(ch)
            .checked_sub(DIGIT_ZERO)
            .filter(|&digit| digit < 16)?;
        if count == most {
            return None;
        }
        number = number * 16 + digit as usize;
    }
    None
}

/// Whether `ch` is one marks are made of.
pub(crate) fn in_marks(ch: char) -> bool {
    matches!(ch, MARK_START | MARK_SPLIT | MARK_END)
        || (DIGIT_ZERO..DIGIT_ZERO + 16).contains(&u32::from(ch))
}

/// The line for entry `id` of session `session`, called `name`: its
/// mark, then its name, with a `/` after if it's a folder, or a link to
/// one.
pub(crate) fn line_of(session: u32, id: usize, name: &str, kind: EntryKind) -> String {
    format!("{}{}", mark(session, id), shown_name(name, kind))
}

/// `name`, as a listing shows it.
fn shown_name(name: &str, kind: EntryKind) -> String {
    if kind.expands() {
        format!("{name}/")
    } else {
        name.to_owned()
    }
}

/// The way up, which a listing shows as its first line.
pub(crate) const UP: &str = "..";

/// Whether `line` is a way up: `..`, or `../` as a folder's written,
/// alone and without a mark, however it's indented. It names nothing, so
/// it comes to nothing wherever it is.
pub(crate) fn is_up(line: &str) -> bool {
    let text = line.trim_start_matches([' ', '\t']);
    !text.starts_with(MARK_START) && matches!(text.trim(), ".." | "../")
}

/// Whether the listing texts `a` and `b` say the same, as far as what
/// they come to is concerned: line for line, a line break ending each,
/// but for ways up, which come to nothing. So a `..` deleted, moved or
/// put twice leaves a listing as it was.
///
/// It's asked on every edit of a listing, which may have thousands of
/// lines, so only the lines between what the two start and end with
/// alike are read as lines: an edit's are all that differ.
pub(crate) fn same(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (a_bytes, b_bytes) = (a.as_bytes(), b.as_bytes());
    let prefix = common_prefix(std::iter::once(a_bytes), std::iter::once(b_bytes));
    let suffix = common_suffix(std::iter::once(a_bytes), std::iter::once(b_bytes))
        .min(a.len().min(b.len()) - prefix);
    // From the start of the line the prefix ends in, to the end of the
    // line the suffix starts in: whole lines, the same in both outside.
    let start = (a_bytes[..prefix].iter())
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |at| at + 1);
    let from = a.len() - suffix;
    let end = (a_bytes[from..].iter())
        .position(|&byte| byte == b'\n')
        .map_or(a.len(), |at| from + at + 1);
    let tail = a.len() - end;
    let (a, b) = (&a[start..end], &b[start..b.len() - tail]);
    said(a.split_terminator('\n').map(Cow::Borrowed))
        .eq(said(b.split_terminator('\n').map(Cow::Borrowed)))
}

/// [`same`], of texts kept as ropes, as an editor's are.
pub(crate) fn same_rope(a: &Rope, b: &Rope) -> bool {
    if a == b {
        return true;
    }
    let prefix = common_prefix(a.chunks().map(str::as_bytes), b.chunks().map(str::as_bytes));
    let suffix =
        common_suffix(backward(a), backward(b)).min(a.len_bytes().min(b.len_bytes()) - prefix);
    let start = a.line_to_byte(a.byte_to_line(prefix));
    let end = a.line_to_byte(a.byte_to_line(a.len_bytes() - suffix) + 1);
    let tail = a.len_bytes() - end;
    said_between(a, start, end).eq(said_between(b, start, b.len_bytes() - tail))
}

/// The pieces of `text`, last first.
fn backward(text: &Rope) -> impl Iterator<Item = &[u8]> {
    let (chunks, ..) = text.chunks_at_byte(text.len_bytes());
    chunks.reversed().map(str::as_bytes)
}

/// What the lines of `text` from byte `start` to `end`, a line's start or
/// the end, say, for [`same_rope`].
fn said_between(text: &Rope, start: usize, end: usize) -> impl Iterator<Item = Cow<'_, str>> {
    // The last, empty, unless a line break ends what's before it, isn't a
    // line.
    let lines = (text.byte_slice(start..end).lines()).filter(|line| line.len_bytes() > 0);
    said(lines.map(|line| match Cow::from(line) {
        Cow::Borrowed(line) => Cow::Borrowed(line.strip_suffix('\n').unwrap_or(line)),
        Cow::Owned(mut line) => {
            if line.ends_with('\n') {
                line.pop();
            }
            Cow::Owned(line)
        }
    }))
}

/// How many bytes the texts whose pieces are `a` and `b` start with
/// alike: a piece at a time, a byte at a time only in the piece they part
/// in.
fn common_prefix<'a>(
    mut a: impl Iterator<Item = &'a [u8]>,
    mut b: impl Iterator<Item = &'a [u8]>,
) -> usize {
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut alike = 0;
    loop {
        if x.is_empty() {
            match a.next() {
                Some(piece) => x = piece,
                None => return alike,
            }
            continue;
        }
        if y.is_empty() {
            match b.next() {
                Some(piece) => y = piece,
                None => return alike,
            }
            continue;
        }
        let len = x.len().min(y.len());
        if x[..len] != y[..len] {
            let parted = (x.iter().zip(y)).position(|(x, y)| x != y);
            return alike + parted.unwrap_or(len);
        }
        alike += len;
        (x, y) = (&x[len..], &y[len..]);
    }
}

/// How many bytes the texts whose pieces are `a` and `b`, last first,
/// end with alike, as [`common_prefix`] counts.
fn common_suffix<'a>(
    mut a: impl Iterator<Item = &'a [u8]>,
    mut b: impl Iterator<Item = &'a [u8]>,
) -> usize {
    let (mut x, mut y): (&[u8], &[u8]) = (&[], &[]);
    let mut alike = 0;
    loop {
        if x.is_empty() {
            match a.next() {
                Some(piece) => x = piece,
                None => return alike,
            }
            continue;
        }
        if y.is_empty() {
            match b.next() {
                Some(piece) => y = piece,
                None => return alike,
            }
            continue;
        }
        let len = x.len().min(y.len());
        let (x_end, y_end) = (&x[x.len() - len..], &y[y.len() - len..]);
        if x_end != y_end {
            let parted = (x_end.iter().rev().zip(y_end.iter().rev())).position(|(x, y)| x != y);
            return alike + parted.unwrap_or(len);
        }
        alike += len;
        (x, y) = (&x[..x.len() - len], &y[..y.len() - len]);
    }
}

/// What of `lines`, a listing's, says anything, for [`same`]: those that
/// aren't ways up.
fn said<'a>(lines: impl Iterator<Item = Cow<'a, str>>) -> impl Iterator<Item = Cow<'a, str>> {
    lines.filter(|line| !is_up(line))
}

/// One line of a listing, read.
#[derive(Debug)]
struct Read {
    folder: usize,
    line: u64,
    /// The entry its mark names, if it has one.
    id: Option<usize>,
    /// The path it names, and whether it's a folder.
    path: String,
    dir: bool,
    /// It's its entry's, naming it as it was.
    stays: bool,
    /// What was typed for its name, to say it.
    typed: String,
}

impl Read {
    fn problem(&self, why: String) -> Problem {
        Problem {
            at: Some((self.folder, self.line)),
            why,
        }
    }
}

/// What the edits to `folders`' listings come to, as file operations in
/// the order to do them, with `originals` the entries their marks name.
/// Nothing at all if nothing has changed.
///
/// The order is safe: copies are made before what they're copied from
/// moves, and moves and deletes free a name before anything takes it.
/// Deletes come last, unless a name they free is wanted. Names that swap,
/// or go round, go aside first.
pub(crate) fn plan(
    session: u32,
    originals: &[Original],
    folders: &[Folder<'_>],
) -> Result<Vec<Step>, Problem> {
    let lines = read_all(session, originals, folders)?;
    let contents = Contents::of(folders);
    // The line naming each path.
    let mut taken: HashMap<&str, usize> = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(&first) = taken.get(line.path.as_str()) {
            let (other, why) = match (lines[first].stays, line.stays) {
                (true, true) => (
                    index,
                    format!(
                        "two lines are both {}: a copy needs a name of its own",
                        line.typed
                    ),
                ),
                (true, false) => (index, format!("{} is already there", line.typed)),
                (false, true) => (first, format!("{} is already there", lines[first].typed)),
                (false, false) => (index, format!("two lines are both {}", line.typed)),
            };
            return Err(lines[other].problem(why));
        }
        taken.insert(&line.path, index);
    }
    let mut steps: Vec<(Step, Option<usize>)> = Vec::new();
    let mut by_id: Vec<Vec<usize>> = vec![Vec::new(); originals.len()];
    for (index, line) in lines.iter().enumerate() {
        if let Some(id) = line.id {
            by_id[id].push(index);
        }
    }
    // The paths of entries moved or deleted, which are free once they are.
    let mut freed: HashSet<&str> = HashSet::new();
    for (original, at) in originals.iter().zip(&by_id) {
        if at.is_empty() && original.retired {
            continue;
        }
        if at.is_empty() {
            freed.insert(&original.path);
            let step = Step::Delete {
                path: original.path.clone(),
                kind: original.kind,
            };
            steps.push((step, None));
            continue;
        }
        // The first line to take it elsewhere moves it, unless one keeps
        // it where it is; the rest copy it.
        let mover =
            (!original.retired && !at.iter().any(|&index| lines[index].stays)).then_some(at[0]);
        for &index in at {
            let line = &lines[index];
            if line.stays {
                continue;
            }
            let (from, to) = (original.path.clone(), line.path.clone());
            let step = if mover == Some(index) {
                freed.insert(&original.path);
                Step::Move {
                    from,
                    to,
                    aside: false,
                }
            } else {
                Step::Copy { from, to }
            };
            steps.push((step, Some(index)));
        }
    }
    for (index, line) in lines.iter().enumerate() {
        if line.id.is_none() {
            let step = Step::Create {
                path: line.path.clone(),
                dir: line.dir,
            };
            steps.push((step, Some(index)));
        }
    }
    // Every step but a delete puts something where its line says.
    for index in steps.iter().filter_map(|(_, index)| *index) {
        check_room(&lines[index], &contents, &freed, &taken, &lines)?;
    }
    check_going(&steps, folders, &lines)?;
    order(steps, &contents, &taken, &lines)
}

/// Reads every line of `folders`' listings, but blank ones.
fn read_all(
    session: u32,
    originals: &[Original],
    folders: &[Folder<'_>],
) -> Result<Vec<Read>, Problem> {
    let mut lines = Vec::new();
    for (folder, listing) in folders.iter().enumerate() {
        for (line, text) in (0u64..).zip(listing.text.split('\n')) {
            if let Some(read) = read_line(session, originals, listing.dir, folder, line, text)? {
                lines.push(read);
            }
        }
    }
    Ok(lines)
}

/// Reads `text`, line `line` of listing `folder`, of the folder `dir`:
/// `None` if it's blank.
fn read_line(
    session: u32,
    originals: &[Original],
    dir: &str,
    folder: usize,
    line: u64,
    text: &str,
) -> Result<Option<Read>, Problem> {
    let problem = |why: String| Problem {
        at: Some((folder, line)),
        why,
    };
    // A line indented, as `>>` does, is read as if it weren't.
    let text = text.trim_start_matches([' ', '\t']);
    let (id, rest) = match read_mark(text) {
        Some((of, id, rest)) if of == session => (Some(id), rest),
        Some(_) => {
            return Err(problem(
                "this line came from another listing: type its name on a new line instead"
                    .to_owned(),
            ));
        }
        None => (None, text),
    };
    if rest.chars().any(in_marks) {
        return Err(problem(
            "this line's hidden mark is damaged, or it has another's, as joining lines \
             makes: put it back, or delete the line"
                .to_owned(),
        ));
    }
    let original = match id {
        Some(id) => Some(originals.get(id).ok_or_else(|| {
            problem("this line's hidden mark is from a listing no longer open".to_owned())
        })?),
        None => None,
    };
    if let Some(original) = original {
        let name = original.path.rsplit('/').next().unwrap_or_default();
        // As it was, in its own listing: one put in another's is a move.
        if rest == shown_name(name, original.kind) && parent_of(&original.path) == dir {
            return Ok(Some(Read {
                folder,
                line,
                id,
                path: original.path.clone(),
                dir: original.kind.expands(),
                stays: true,
                typed: rest.to_owned(),
            }));
        }
    }
    let typed = rest.trim();
    if typed.is_empty() {
        return match original {
            Some(_) => Err(problem(
                "this line has no name: delete the line to delete what it was".to_owned(),
            )),
            None => Ok(None),
        };
    }
    // The way up, or a copy of it, without a mark: it names nothing.
    if original.is_none() && is_up(typed) {
        return Ok(None);
    }
    if typed.contains('\0') {
        return Err(problem("a name can't hold a NUL".to_owned()));
    }
    let slashed = typed.ends_with('/');
    let name = typed.trim_end_matches('/');
    if name.is_empty() {
        return Err(problem("/ alone isn't a name".to_owned()));
    }
    let path = resolve(dir, name).map_err(&problem)?;
    if under(&path, dir) {
        return Err(problem(format!(
            "{typed} is where this listing's folder is, or holds it"
        )));
    }
    let dir = match original {
        Some(original) if slashed && !original.kind.expands() => {
            return Err(problem(format!(
                "{} is a file: a / at the end is for a new folder, on a line of its own",
                original.path.rsplit('/').next().unwrap_or_default()
            )));
        }
        Some(original) => original.kind.expands(),
        None => slashed,
    };
    let stays = original.is_some_and(|original| original.path == path);
    Ok(Some(Read {
        folder,
        line,
        id,
        path,
        dir,
        stays,
        typed: typed.to_owned(),
    }))
}

/// Checks there's room for what `line` puts where it names: nothing there
/// already, though hidden from its listing; and nothing on its way there
/// that's a file, or moving, or being deleted.
fn check_room(
    line: &Read,
    contents: &Contents<'_>,
    freed: &HashSet<&str>,
    taken: &HashMap<&str, usize>,
    lines: &[Read],
) -> Result<(), Problem> {
    let path = line.path.as_str();
    for &(dir, ref names) in &contents.folders {
        let Some(rest) = inside(dir, path) else {
            continue;
        };
        let (first, deeper) = match rest.split_once('/') {
            Some((first, _)) => (first, true),
            None => (rest, false),
        };
        let held = join(dir, first);
        let entry = names.get(first);
        if !deeper {
            // Something's there that isn't going. One its listing shows
            // has a line staying there, which two lines naming one path
            // catches, so this is one it doesn't, as a hidden file.
            if entry.is_some() && !freed.contains(held.as_str()) {
                return Err(line.problem(format!("{} is already there", line.typed)));
            }
            continue;
        }
        if let Some(entry) = entry {
            if freed.contains(held.as_str()) {
                return Err(line.problem(format!(
                    "{first} is being moved or deleted: put {} somewhere that stays, \
                     or do it in a write of its own",
                    line.typed
                )));
            }
            if !entry.expands() {
                return Err(line.problem(format!("{first} is a file, not a folder")));
            }
        } else if let Some(&index) = taken.get(held.as_str())
            && !lines[index].dir
        {
            // A new folder is made by its own line, or by what's put in
            // it, but not by a new file's.
            return Err(line.problem(format!("{first} is a file, not a folder")));
        }
    }
    Ok(())
}

/// What was in each folder listed, by name, to look up at once.
struct Contents<'a> {
    folders: Vec<(&'a str, HashMap<&'a str, EntryKind>)>,
}

impl<'a> Contents<'a> {
    fn of(folders: &[Folder<'a>]) -> Self {
        let folders = (folders.iter())
            .map(|folder| {
                let names = (folder.entries.iter())
                    .map(|entry| (entry.name.as_str(), entry.kind))
                    .collect();
                (folder.dir, names)
            })
            .collect();
        Self { folders }
    }
}

/// Checks nothing's said to stay in a folder being deleted, as a line of
/// its own listing left as it was says; and nothing's put in a folder
/// where one is moving or being copied to, whose contents, coming with
/// it, aren't known to have room.
fn check_going(
    steps: &[(Step, Option<usize>)],
    folders: &[Folder<'_>],
    lines: &[Read],
) -> Result<(), Problem> {
    let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    for (step, _) in steps {
        let Step::Delete { path, .. } = step else {
            continue;
        };
        let Some(folder) = folders.iter().position(|folder| folder.dir == path) else {
            continue;
        };
        if let Some(line) = (lines.iter()).find(|line| line.folder == folder && line.stays) {
            return Err(line.problem(format!(
                "{} is being deleted, but this line keeps {} in it: delete the line too, \
                 or keep {}",
                name(path),
                line.typed,
                name(path)
            )));
        }
    }
    // Folders moving or copied, by where they're going.
    let going: HashMap<&str, &str> = (steps.iter())
        .filter_map(|(step, index)| match (step, index) {
            (Step::Move { from, to, .. } | Step::Copy { from, to }, Some(index))
                if lines[*index].dir =>
            {
                Some((to.as_str(), from.as_str()))
            }
            _ => None,
        })
        .collect();
    for (step, index) in steps {
        let (Some(to), Some(index)) = (step_takes(step), index) else {
            continue;
        };
        let line = &lines[*index];
        let mut above = parent_of(to);
        while above != "/" {
            if let Some(&folder) = going.get(above) {
                let why = match step.source() {
                    Some(from) if inside(folder, from).is_some() => format!(
                        "{} goes along with {}, which moves to {}: leave its line as it was",
                        line.typed,
                        name(folder),
                        name(above)
                    ),
                    _ => format!(
                        "{} would go in {}, which {} is going to: put it there in a write of \
                         its own",
                        line.typed,
                        name(above),
                        name(folder)
                    ),
                };
                return Err(line.problem(why));
            }
            above = parent_of(above);
        }
    }
    Ok(())
}

/// The path `step` puts something at, if it puts something somewhere.
fn step_takes(step: &Step) -> Option<&str> {
    match step {
        Step::Create { path, .. } => Some(path),
        Step::Copy { to, .. } | Step::Move { to, .. } => Some(to),
        Step::Delete { .. } => None,
    }
}

/// Why one step must wait for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    /// The other frees the name this one takes.
    Frees,
    /// This one frees what the other reads, or a folder it's in.
    Reads,
    /// The other takes the folder this one puts something in.
    Holds,
}

/// What a step reads, frees and takes, and how soon it goes, all else
/// being equal.
struct Parts {
    reads: Option<String>,
    frees: Option<String>,
    takes: Option<String>,
    soon: u8,
}

impl Parts {
    fn of(step: &Step) -> Self {
        let (reads, frees, takes, soon) = match step {
            Step::Copy { from, to } => (Some(from), None, Some(to), 0),
            Step::Create { path, .. } => (None, None, Some(path), 1),
            Step::Move { from, to, .. } => (Some(from), Some(from), Some(to), 2),
            Step::Delete { path, .. } => (Some(path), Some(path), None, 3),
        };
        Self {
            reads: reads.cloned(),
            frees: frees.cloned(),
            takes: takes.cloned(),
            soon,
        }
    }
}

/// Who waits for whom among steps, and why.
struct Waits {
    /// Those waiting for each step, and why.
    after: Vec<Vec<(usize, Wait)>>,
    /// How many each is still waiting for, for each reason.
    waiting: Vec<[u32; 3]>,
}

impl Waits {
    /// Who waits for whom among the steps `parts` describe.
    fn of(parts: &[Parts]) -> Self {
        let count = parts.len();
        let mut freeing: HashMap<&str, usize> = HashMap::new();
        let mut taking: HashMap<&str, usize> = HashMap::new();
        let mut reading: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, parts) in parts.iter().enumerate() {
            if let Some(frees) = &parts.frees {
                freeing.insert(frees, index);
            }
            if let Some(takes) = &parts.takes {
                taking.insert(takes, index);
            }
            if let Some(reads) = &parts.reads {
                reading.entry(reads).or_default().push(index);
            }
        }
        let mut waits = Self {
            after: vec![Vec::new(); count],
            waiting: vec![[0; 3]; count],
        };
        for (index, parts) in parts.iter().enumerate() {
            if let Some(takes) = &parts.takes {
                if let Some(&other) = freeing.get(takes.as_str()) {
                    waits.wait(other, index, Wait::Frees);
                }
                let mut above = parent_of(takes);
                while above != "/" {
                    if let Some(&other) = taking.get(above) {
                        waits.wait(other, index, Wait::Holds);
                    }
                    above = parent_of(above);
                }
            }
            if let Some(frees) = &parts.frees {
                // What reads it, or anything in it.
                let within = format!("{frees}/");
                let readers = (reading.get(frees.as_str()).into_iter().flatten().copied())
                    .chain(
                        (reading.range(within.as_str()..))
                            .take_while(|(path, _)| path.starts_with(&within))
                            .flat_map(|(_, readers)| readers.iter().copied()),
                    )
                    .collect::<Vec<_>>();
                for other in readers {
                    waits.wait(other, index, Wait::Reads);
                }
            }
        }
        waits
    }

    /// Has step `then` wait for step `first`, for `why`.
    fn wait(&mut self, first: usize, then: usize, why: Wait) {
        if first != then {
            self.after[first].push((then, why));
            self.waiting[then][why as usize] += 1;
        }
    }

    /// Step `first` is done, as far as what waits for it for any of
    /// `why` is concerned: returns those no longer waiting for anything,
    /// and how many waits it ended.
    fn done(&mut self, first: usize, why: &[Wait]) -> (Vec<usize>, usize) {
        let mut ready = Vec::new();
        let mut ended = 0;
        let waits = std::mem::take(&mut self.after[first]);
        for (then, wait) in waits {
            if !why.contains(&wait) {
                self.after[first].push((then, wait));
                continue;
            }
            ended += 1;
            self.waiting[then][wait as usize] -= 1;
            if self.waiting[then] == [0; 3] {
                ready.push(then);
            }
        }
        (ready, ended)
    }
}

/// `steps` in an order safe to do them in, as [`plan`] says, with the
/// line that asked for each, if one did.
fn order(
    mut steps: Vec<(Step, Option<usize>)>,
    contents: &Contents<'_>,
    taken: &HashMap<&str, usize>,
    lines: &[Read],
) -> Result<Vec<Step>, Problem> {
    let parts: Vec<Parts> = steps.iter().map(|(step, _)| Parts::of(step)).collect();
    let freed: HashSet<&str> = parts
        .iter()
        .filter_map(|parts| parts.frees.as_deref())
        .collect();
    let mut waits = Waits::of(&parts);
    let soon = |index: usize| Reverse((parts[index].soon, index));
    let mut ready: BinaryHeap<Reverse<(u8, usize)>> = (0..steps.len())
        .filter(|&index| waits.waiting[index] == [0; 3])
        .map(soon)
        .collect();
    let mut done = vec![false; steps.len()];
    let mut left = steps.len();
    let mut ordered = Vec::with_capacity(steps.len());
    let mut asides: HashSet<String> = HashSet::new();
    // Every path a line names, and the folders it's in, which putting
    // something there makes if they aren't there.
    let mut wanted: HashSet<&str> = HashSet::new();
    for &path in taken.keys() {
        let mut at = path;
        while at != "/" && wanted.insert(at) {
            at = parent_of(at);
        }
    }
    let mut set_aside: HashSet<usize> = HashSet::new();
    // Each step once, and each move aside at most once more: never more.
    let most = 2 * steps.len();
    loop {
        while let Some(Reverse((_, index))) = ready.pop() {
            done[index] = true;
            left -= 1;
            ordered.push(steps[index].0.clone());
            let (now, _) = waits.done(index, &[Wait::Frees, Wait::Reads, Wait::Holds]);
            ready.extend(now.into_iter().map(soon));
        }
        if left == 0 {
            return Ok(ordered);
        }
        let stuck = (0..steps.len()).find(|&index| !done[index]);
        if ordered.len() >= most {
            return Err(tangled(&steps, lines, stuck));
        }
        // What's left goes round: names swapping, or moving in a ring, or
        // out of a folder to where the folder is. A move nothing need read
        // first goes aside, once, out of every folder that's going.
        let aside = (0..steps.len()).find(|&index| {
            !done[index]
                && !set_aside.contains(&index)
                && matches!(steps[index].0, Step::Move { .. })
                && waits.waiting[index][Wait::Reads as usize] == 0
        });
        let Some(index) = aside else {
            return Err(tangled(&steps, lines, stuck));
        };
        set_aside.insert(index);
        let Step::Move { from, to, .. } = steps[index].0.clone() else {
            unreachable!("a move");
        };
        let spare = spare_name(&from, &freed, contents, &wanted, &asides);
        asides.insert(spare.clone());
        ordered.push(Step::Move {
            from,
            to: spare.clone(),
            aside: true,
        });
        steps[index].0 = Step::Move {
            from: spare,
            to,
            aside: false,
        };
        // Its name is free now, and the folders that are going no longer
        // hold it.
        let (now, ended) = waits.done(index, &[Wait::Frees, Wait::Reads]);
        if ended == 0 {
            return Err(tangled(&steps, lines, Some(index)));
        }
        ready.extend(now.into_iter().map(soon));
    }
}

/// Why steps can't be ordered: they're tangled, at step `index`'s line,
/// if it has one.
fn tangled(steps: &[(Step, Option<usize>)], lines: &[Read], index: Option<usize>) -> Problem {
    let line = index.and_then(|index| steps[index].1);
    Problem {
        at: line.map(|line| (lines[line].folder, lines[line].line)),
        why: "these changes are tangled together: make some of them in a write of their own"
            .to_owned(),
    }
}

/// A name for what's at `path` to go aside to: beside it, or beside the
/// outermost folder it's in that's going, as `freed` says, so it's out of
/// every one; and one nothing in that folder has, nor will have, as
/// `wanted`, the paths lines name and the folders they're in, says.
fn spare_name(
    path: &str,
    freed: &HashSet<&str>,
    contents: &Contents<'_>,
    wanted: &HashSet<&str>,
    asides: &HashSet<String>,
) -> String {
    let mut outermost = path;
    let mut above = parent_of(path);
    while above != "/" {
        if freed.contains(above) {
            outermost = above;
        }
        above = parent_of(above);
    }
    let dir = parent_of(outermost);
    let name = path.rsplit('/').next().unwrap_or_default();
    let names = (contents.folders.iter()).find(|(folder, _)| *folder == dir);
    // One of this many is free, as no more can be had.
    let had = names.map_or(0, |(_, names)| names.len()) + wanted.len() + asides.len();
    (1..=had + 1)
        .map(|count| match count {
            1 => join(dir, &format!("{name}.moving")),
            count => join(dir, &format!("{name}.moving-{count}")),
        })
        .find(|spare| {
            let spare_name = spare.rsplit('/').next().unwrap_or_default();
            !names.is_some_and(|(_, names)| names.contains_key(spare_name))
                && !wanted.contains(spare.as_str())
                && !asides.contains(spare)
        })
        .expect("a name no one has")
}

/// The path `typed` names, from the folder `dir`, with `.` and `..` worked
/// out.
fn resolve(dir: &str, typed: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = if typed.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|part| !part.is_empty()).collect()
    };
    for part in typed.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("{typed} goes above /"));
                }
            }
            part => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err(format!("{typed} is the root of everything"));
    }
    Ok(format!("/{}", parts.join("/")))
}

/// The path of `name` in the folder `dir`.
fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// The folder `path` is in.
fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(slash) => &path[..slash],
    }
}

/// Whether `path` is `dir` or in it.
fn under(dir: &str, path: &str) -> bool {
    dir == "/" || path == dir || inside(dir, path).is_some()
}

/// What of `path` is inside the folder `dir`, if it's inside it.
fn inside<'a>(dir: &str, path: &'a str) -> Option<&'a str> {
    let rest = path.strip_prefix(dir)?;
    let rest = if dir.ends_with('/') {
        rest
    } else {
        rest.strip_prefix('/')?
    };
    (!rest.is_empty()).then_some(rest)
}
