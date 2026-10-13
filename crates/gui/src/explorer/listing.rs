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
//! never read as another.
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
//!
//! A name is read from its listing's folder: `sub/x` is in the folder
//! `sub`, made if it isn't there, `../x` in the folder above, and `/x`
//! from the root of everything. Paths are where things end up: a line
//! can't put something in a folder that's itself being moved or deleted.
//!
//! What can't be done as asked is refused, saying which line asks it,
//! before anything is done: two lines with one name, a name something
//! already has, a mark damaged or from another listing.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use clackatoo_protocol::{Entry, EntryKind, FileOp};

#[cfg(test)]
mod tests;

/// The first character of a mark...
const MARK_START: char = '\u{10FFFD}';

/// ...and its last. Between them, the entry's number, in hexadecimal.
const MARK_END: char = '\u{10FFFC}';

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
    /// The file operation that does it, which never writes over anything.
    pub(crate) fn op(&self) -> FileOp {
        match self {
            Self::Create { path, dir } => FileOp::Create {
                path: path.clone(),
                dir: *dir,
            },
            Self::Copy { from, to } => FileOp::Copy {
                from: from.clone(),
                to: to.clone(),
                overwrite: None,
            },
            Self::Move { from, to, .. } => FileOp::Move {
                from: from.clone(),
                to: to.clone(),
                overwrite: None,
            },
            Self::Delete { path, .. } => FileOp::Delete {
                path: path.clone(),
                trash: true,
            },
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

/// The mark that starts the line of entry `id`.
pub(crate) fn mark(id: usize) -> String {
    let mut mark = String::from(MARK_START);
    for digit in format!("{id:x}").chars() {
        let value = digit.to_digit(16).expect("a hexadecimal digit");
        mark.push(char::from_u32(DIGIT_ZERO + value).expect("a private-use character"));
    }
    mark.push(MARK_END);
    mark
}

/// The entry `line`'s mark names, if it starts with a whole one.
pub(crate) fn id_of(line: &str) -> Option<usize> {
    read_mark(line).map(|(id, _)| id)
}

/// How many bytes of `line` its mark takes, if it starts with one, for an
/// editor to hide.
pub(crate) fn mark_len(line: &str) -> usize {
    if !line.starts_with(MARK_START) {
        return 0;
    }
    read_mark(line).map_or(0, |(_, rest)| line.len() - rest.len())
}

/// The entry `line`'s mark names, and the rest of the line, if it starts
/// with a whole mark.
fn read_mark(line: &str) -> Option<(usize, &str)> {
    let rest = line.strip_prefix(MARK_START)?;
    let mut id: usize = 0;
    for (count, (at, ch)) in rest.char_indices().enumerate() {
        if ch == MARK_END {
            return (count > 0).then(|| (id, &rest[at + ch.len_utf8()..]));
        }
        let digit = u32::from(ch)
            .checked_sub(DIGIT_ZERO)
            .filter(|&digit| digit < 16)?;
        if count == MOST_DIGITS {
            return None;
        }
        id = id * 16 + digit as usize;
    }
    None
}

/// Whether `ch` is one marks are made of.
fn in_marks(ch: char) -> bool {
    ch == MARK_START || ch == MARK_END || (DIGIT_ZERO..DIGIT_ZERO + 16).contains(&u32::from(ch))
}

/// The line for entry `id`, called `name`: its mark, then its name, with
/// a `/` after if it's a folder, or a link to one.
pub(crate) fn line_of(id: usize, name: &str, kind: EntryKind) -> String {
    format!("{}{}", mark(id), shown_name(name, kind))
}

/// `name`, as a listing shows it.
fn shown_name(name: &str, kind: EntryKind) -> String {
    if kind.expands() {
        format!("{name}/")
    } else {
        name.to_owned()
    }
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
pub(crate) fn plan(originals: &[Original], folders: &[Folder<'_>]) -> Result<Vec<Step>, Problem> {
    let lines = read_all(originals, folders)?;
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
        let mover = (!at.iter().any(|&index| lines[index].stays)).then_some(at[0]);
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
    order(steps, &contents, &taken, &lines)
}

/// Reads every line of `folders`' listings, but blank ones.
fn read_all(originals: &[Original], folders: &[Folder<'_>]) -> Result<Vec<Read>, Problem> {
    let mut lines = Vec::new();
    for (folder, listing) in folders.iter().enumerate() {
        for (line, text) in (0u64..).zip(listing.text.split('\n')) {
            if let Some(read) = read_line(originals, listing.dir, folder, line, text)? {
                lines.push(read);
            }
        }
    }
    Ok(lines)
}

/// Reads `text`, line `line` of listing `folder`, of the folder `dir`:
/// `None` if it's blank.
fn read_line(
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
        Some((id, rest)) => (Some(id), rest),
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

    /// Step `first` is done, as far as what waits for it for `why` is
    /// concerned, or for anything if `why` is `None`: returns those no
    /// longer waiting for anything.
    fn done(&mut self, first: usize, why: Option<Wait>) -> Vec<usize> {
        let mut ready = Vec::new();
        let waits = std::mem::take(&mut self.after[first]);
        for (then, wait) in waits {
            if why.is_some_and(|why| why != wait) {
                self.after[first].push((then, wait));
                continue;
            }
            self.waiting[then][wait as usize] -= 1;
            if self.waiting[then] == [0; 3] {
                ready.push(then);
            }
        }
        ready
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
    loop {
        while let Some(Reverse((_, index))) = ready.pop() {
            done[index] = true;
            left -= 1;
            ordered.push(steps[index].0.clone());
            ready.extend(waits.done(index, None).into_iter().map(soon));
        }
        if left == 0 {
            return Ok(ordered);
        }
        // What's left goes round: names swapping, or moving in a ring. A
        // move nothing need read first goes aside, freeing its name.
        let aside = (0..steps.len()).find(|&index| {
            !done[index]
                && matches!(steps[index].0, Step::Move { .. })
                && waits.waiting[index][Wait::Reads as usize] == 0
        });
        let Some(index) = aside else {
            let line = (0..steps.len())
                .find(|&index| !done[index])
                .and_then(|index| steps[index].1);
            return Err(Problem {
                at: line.map(|line| (lines[line].folder, lines[line].line)),
                why: "these changes go round in a ring with a copy: make the copy in a \
                      write of its own"
                    .to_owned(),
            });
        };
        let Step::Move { from, to, .. } = steps[index].0.clone() else {
            unreachable!("a move");
        };
        let spare = spare_name(&from, contents, taken, &asides);
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
        // Its name is free now.
        ready.extend(waits.done(index, Some(Wait::Frees)).into_iter().map(soon));
    }
}

/// A name beside `path` for it to go aside to: one nothing in its folder
/// has, nor will have.
fn spare_name(
    path: &str,
    contents: &Contents<'_>,
    taken: &HashMap<&str, usize>,
    asides: &HashSet<String>,
) -> String {
    let dir = parent_of(path);
    let name = path.rsplit('/').next().unwrap_or_default();
    let names = (contents.folders.iter()).find(|(folder, _)| *folder == dir);
    // One of this many is free, as no more can be had.
    let had = names.map_or(0, |(_, names)| names.len()) + taken.len() + asides.len();
    (1..=had + 1)
        .map(|count| match count {
            1 => join(dir, &format!("{name}.moving")),
            count => join(dir, &format!("{name}.moving-{count}")),
        })
        .find(|spare| {
            let spare_name = spare.rsplit('/').next().unwrap_or_default();
            !names.is_some_and(|(_, names)| names.contains_key(spare_name))
                && !taken.contains_key(spare.as_str())
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
