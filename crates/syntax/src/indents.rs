//! Indenting by the syntax tree, as nvim-treesitter's indent module does,
//! from a pack's `indents.scm`.
//!
//! The query captures nodes by what they do to the lines in them:
//! `@indent.begin` indents the lines after its first by a step, and
//! `@indent.branch` takes that back for a line it starts, as `}` and
//! `else` do; `@indent.dedent` takes a step off the lines after its first,
//! `@indent.end` closes the node the line before ends, `@indent.align`
//! lines its lines up with its opening bracket, `@indent.zero` puts a line
//! at the margin, `@indent.ignore` leaves the lines in it unindented and
//! `@indent.auto` as they are. A line's indent is worked out by walking
//! from the first node on it out to the root, as [`get_indent`] in
//! nvim-treesitter's `indent.lua` does, which this follows step by step,
//! so the queries mean here what they mean there.
//!
//! Derived from nvim-treesitter's `lua/nvim-treesitter/indent.lua`,
//! Copyright the nvim-treesitter contributors, under the Apache License
//! 2.0: see `NOTICE` at the root of the repository.
//!
//! [`get_indent`]: https://github.com/nvim-treesitter/nvim-treesitter/blob/cf12346a3414fa1b06af75c79faebe7f76df080a/lua/nvim-treesitter/indent.lua

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use clackatoo_text::{LineIndent, Rope};
use regex::Regex;
use tree_sitter::{
    Language, Node, Point, Query, QueryCursor, QueryPredicateArg, StreamingIterator, Tree,
};

use crate::highlight::RopeText;

/// What the captures of `indents.scm` say of the nodes they take, as bits.
const AUTO: u8 = 1;
const BEGIN: u8 = 1 << 1;
const END: u8 = 1 << 2;
const DEDENT: u8 = 1 << 3;
const BRANCH: u8 = 1 << 4;
const IGNORE: u8 = 1 << 5;
const ALIGN: u8 = 1 << 6;
const ZERO: u8 = 1 << 7;

/// The bit for a capture called `name`, or 0 for one that says nothing
/// clackatoo reads, as one starting with `_` doesn't.
fn rule(name: &str) -> u8 {
    match name {
        "indent.auto" => AUTO,
        "indent.begin" => BEGIN,
        "indent.end" => END,
        "indent.dedent" => DEDENT,
        "indent.branch" => BRANCH,
        "indent.ignore" => IGNORE,
        "indent.align" => ALIGN,
        "indent.zero" => ZERO,
        _ => 0,
    }
}

/// What a pattern sets with `#set!`, which the nodes it captures go by.
#[derive(Debug, Default)]
struct Settings {
    /// `indent.immediate`: an `@indent.begin` on one line indents the
    /// line after it too, as Python's `if x:` does before its body's
    /// typed.
    immediate: bool,
    /// `indent.start_at_same_line`: it indents the line it starts on.
    start_at_same_line: bool,
    /// `indent.open_delimiter` and `indent.close_delimiter`: the brackets
    /// an `@indent.align` lines up inside.
    open: Option<Box<str>>,
    close: Option<Box<str>>,
    /// `indent.increment`: the columns past the opening bracket the lines
    /// line up at, 1 if not set.
    increment: Option<i64>,
    /// `indent.avoid_last_matching_next`: the last line inside goes a step
    /// further if it would line up with the body after, as Python's
    /// parameters do.
    avoid_last_matching_next: bool,
}

/// A check a pattern makes that tree-sitter doesn't make itself, written
/// in the query as Neovim's are. Each passes if its capture took no node.
#[derive(Debug)]
enum Check {
    /// `#lua-match?`: the capture's text matches a Lua pattern.
    Matches { capture: u32, pattern: Regex },
    /// `#kind-eq?`: the capture's node is one of these kinds.
    KindIs { capture: u32, kinds: Vec<Box<str>> },
    /// `#has-parent?`: the capture's node's parent is one of these kinds.
    ParentIs { capture: u32, kinds: Vec<Box<str>> },
}

impl Check {
    /// The check `operator` makes with `args`, and whether it's negated,
    /// as Neovim's `not-` does; or `None` for one it can't make.
    fn new(operator: &str, args: &[QueryPredicateArg]) -> Option<(Self, bool)> {
        let (negate, operator) = match operator.strip_prefix("not-") {
            Some(operator) => (true, operator),
            None => (false, operator),
        };
        let [QueryPredicateArg::Capture(capture), rest @ ..] = args else {
            return None;
        };
        let strings = || -> Option<Vec<Box<str>>> {
            (rest.iter())
                .map(|arg| match arg {
                    QueryPredicateArg::String(text) => Some(text.clone()),
                    QueryPredicateArg::Capture(_) => None,
                })
                .collect()
        };
        let capture = *capture;
        let check = match operator {
            "lua-match?" => match rest {
                [QueryPredicateArg::String(pattern)] => Self::Matches {
                    capture,
                    pattern: lua_pattern(pattern)?,
                },
                _ => return None,
            },
            "kind-eq?" => Self::KindIs {
                capture,
                kinds: strings()?,
            },
            "has-parent?" => Self::ParentIs {
                capture,
                kinds: strings()?,
            },
            _ => return None,
        };
        Some((check, negate))
    }

    /// Whether it passes for the node `node`, whose text is in `text`.
    fn passes(&self, node: Node<'_>, text: &Rope) -> bool {
        match self {
            Self::Matches { pattern, .. } => {
                let range = node.byte_range();
                let end = range.end.min(text.len_bytes());
                let slice = text.byte_slice(range.start.min(end)..end);
                pattern.is_match(&slice.to_string())
            }
            Self::KindIs { kinds, .. } => kinds.iter().any(|kind| &**kind == node.kind()),
            Self::ParentIs { kinds, .. } => node
                .parent()
                .is_some_and(|parent| kinds.iter().any(|kind| &**kind == parent.kind())),
        }
    }

    fn capture(&self) -> u32 {
        match self {
            Self::Matches { capture, .. }
            | Self::KindIs { capture, .. }
            | Self::ParentIs { capture, .. } => *capture,
        }
    }
}

/// A pack's `indents.scm`, compiled.
#[derive(Debug)]
pub(crate) struct IndentRules {
    query: Query,
    /// What each capture says, by its index.
    rules: Vec<u8>,
    /// What each pattern sets.
    settings: Vec<Settings>,
    /// The checks each pattern makes, each negated or not; `None` for a
    /// pattern with one that can't be made, which never matches.
    checks: Vec<Option<Vec<(Check, bool)>>>,
}

impl IndentRules {
    /// `source` compiled for `language`.
    pub(crate) fn new(language: &Language, source: &str) -> Result<Self, tree_sitter::QueryError> {
        let query = Query::new(language, source)?;
        let rules = query
            .capture_names()
            .iter()
            .map(|name| rule(name))
            .collect();
        let settings = (0..query.pattern_count())
            .map(|pattern| {
                let mut settings = Settings::default();
                for property in query.property_settings(pattern) {
                    let value = property.value.as_deref();
                    match &*property.key {
                        "indent.immediate" => settings.immediate = true,
                        "indent.start_at_same_line" => settings.start_at_same_line = true,
                        "indent.avoid_last_matching_next" => {
                            settings.avoid_last_matching_next = true;
                        }
                        "indent.open_delimiter" => settings.open = value.map(Box::from),
                        "indent.close_delimiter" => settings.close = value.map(Box::from),
                        "indent.increment" => {
                            settings.increment = value.and_then(|value| value.parse().ok());
                        }
                        _ => {}
                    }
                }
                settings
            })
            .collect();
        let checks = (0..query.pattern_count())
            .map(|pattern| {
                (query.general_predicates(pattern).iter())
                    .map(|predicate| {
                        let check = Check::new(&predicate.operator, &predicate.args);
                        if check.is_none() {
                            log::warn!(
                                "syntax: indents.scm: #{} can't be checked, so its pattern is passed over",
                                predicate.operator
                            );
                        }
                        check
                    })
                    .collect()
            })
            .collect();
        Ok(Self {
            query,
            rules,
            settings,
            checks,
        })
    }
}

/// What the captures say of one node: its rules, and the patterns its
/// `@indent.begin` and `@indent.align` come from, for their settings.
#[derive(Clone, Copy, Debug, Default)]
struct Marks {
    rules: u8,
    begin: Option<usize>,
    align: Option<usize>,
}

/// Works out how lines of a tree should be indented.
pub(crate) struct Indenter<'a> {
    pub(crate) tree: &'a Tree,
    pub(crate) text: &'a Rope,
    pub(crate) rules: &'a IndentRules,
    pub(crate) cursor: &'a mut QueryCursor,
    /// The columns a step of indent is.
    pub(crate) shiftwidth: u32,
    /// The columns tabs stop at multiples of.
    pub(crate) tabstop: u32,
}

/// Indenting took longer than it may, holding up the thread that parses
/// every buffer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TooLong;

/// How many lines, or matches, are worked through between looks at the
/// clock.
const BETWEEN_LOOKS: usize = 256;

impl Indenter<'_> {
    /// How the lines from line `first` to line `last` should be indented,
    /// each worked out as if those before it had been given theirs, as
    /// Vim's `=` does them one by one, though all are worked out of the one
    /// tree. [`TooLong`] if it's not done by `deadline`.
    pub(crate) fn indents(
        &mut self,
        first: usize,
        last: usize,
        deadline: Instant,
    ) -> Result<Vec<LineIndent>, TooLong> {
        let count = self.text.len_lines();
        let rows = first.min(count)..last.saturating_add(1).min(count);
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut work = Work {
            tree: self.tree,
            text: self.text,
            rules: self.rules,
            marks: HashMap::new(),
            given: HashMap::new(),
            lines: RefCell::default(),
            nonblank: RefCell::default(),
            delimiters: HashMap::new(),
            promoted: HashMap::new(),
            step: i64::from(self.shiftwidth.max(1)),
            tabstop: self.tabstop.max(1) as usize,
        };
        // Every node the walks go through has a byte in these lines, or
        // the line with text before the first, whose last node the first
        // looks at if it's blank: so the query need look only there.
        let from = work.nonblank_before(rows.start).unwrap_or(rows.start);
        let start = self.text.line_to_byte(from);
        let end = self.text.line_to_byte(rows.end).max(start + 1);
        work.marks = self.marks(start..end, deadline)?;
        let mut indents = Vec::with_capacity(rows.len());
        for row in rows {
            if row % BETWEEN_LOOKS == 0 && Instant::now() >= deadline {
                return Err(TooLong);
            }
            let width = work.indent(row);
            if let Some(width) = width {
                work.given.insert(row, width);
            }
            indents.push(LineIndent {
                line: self.text.line_to_byte(row),
                width: width.and_then(|width| u32::try_from(width).ok()),
            });
        }
        Ok(indents)
    }

    /// What the captures of matches with a node in bytes `range` say of
    /// the nodes they take. Where two patterns capture a node alike, the
    /// later's settings stand, as in Neovim, which goes through the
    /// captures in order.
    fn marks(
        &mut self,
        range: Range<usize>,
        deadline: Instant,
    ) -> Result<HashMap<usize, Marks>, TooLong> {
        let rules = self.rules;
        let mut marks: HashMap<usize, Marks> = HashMap::new();
        self.cursor.set_byte_range(range);
        let mut matches =
            (self.cursor).matches(&rules.query, self.tree.root_node(), RopeText(self.text));
        let mut seen = 0;
        while let Some(matched) = matches.next() {
            seen += 1;
            if seen % BETWEEN_LOOKS == 0 && Instant::now() >= deadline {
                return Err(TooLong);
            }
            let pattern = matched.pattern_index;
            let Some(checks) = &rules.checks[pattern] else {
                continue;
            };
            let passes = checks.iter().all(|(check, negate)| {
                let node = (matched.captures().iter())
                    .find(|capture| capture.index == check.capture())
                    .map(|capture| capture.node);
                node.is_none_or(|node| check.passes(node, self.text) != *negate)
            });
            if !passes {
                continue;
            }
            for capture in matched.captures() {
                let bits = rules.rules[capture.index as usize];
                if bits == 0 {
                    continue;
                }
                let mark = marks.entry(capture.node.id()).or_default();
                mark.rules |= bits;
                if bits & BEGIN != 0 {
                    mark.begin = mark.begin.max(Some(pattern));
                }
                if bits & ALIGN != 0 {
                    mark.align = mark.align.max(Some(pattern));
                }
            }
        }
        Ok(marks)
    }
}

/// What working out a run of lines' indents keeps.
struct Work<'a> {
    tree: &'a Tree,
    text: &'a Rope,
    rules: &'a IndentRules,
    marks: HashMap<usize, Marks>,
    /// The widths given to the lines worked out so far, by line.
    given: HashMap<usize, i64>,
    /// The text of the lines looked at, as each is looked at several
    /// times.
    lines: RefCell<HashMap<usize, Rc<str>>>,
    /// The nearest line with text before each line looked for, so a run
    /// of blank lines is looked back over once.
    nonblank: RefCell<HashMap<usize, Option<usize>>>,
    /// Each `@indent.align` node's brackets, by its id and which: so a
    /// list of thousands of lines is looked through once, not once a line.
    delimiters: HashMap<(usize, bool), Option<(Node<'a>, bool)>>,
    /// The `@indent.align` each node in error has from its first child that
    /// has one, by its id, once looked for.
    promoted: HashMap<usize, Option<usize>>,
    step: i64,
    tabstop: usize,
}

impl<'a> Work<'a> {
    /// Line `row`'s text, without its line break.
    fn line(&self, row: usize) -> Rc<str> {
        let mut lines = self.lines.borrow_mut();
        let line = lines.entry(row).or_insert_with(|| {
            let line = self.text.line(row).to_string();
            Rc::from(line.trim_end_matches(['\n', '\r']))
        });
        Rc::clone(line)
    }

    /// The nearest line before `row` with more than blanks on it.
    fn nonblank_before(&self, row: usize) -> Option<usize> {
        let before = row.checked_sub(1)?;
        if !self.line(before).trim_ascii().is_empty() {
            return Some(before);
        }
        if let Some(&found) = self.nonblank.borrow().get(&before) {
            return found;
        }
        let found = (0..before)
            .rev()
            .find(|&row| !self.line(row).trim_ascii().is_empty());
        self.nonblank.borrow_mut().insert(row, found);
        found
    }

    /// The columns `text`, the start of a line, reaches.
    fn width(&self, text: &str) -> i64 {
        let cols = text.chars().fold(0, |col, ch| match ch {
            '\t' => (col / self.tabstop + 1) * self.tabstop,
            _ => col + 1,
        });
        i64::try_from(cols).unwrap_or(i64::MAX)
    }

    /// The columns line `row`'s indent reaches, as given it already, or
    /// as it is: Vim's `indent()`.
    fn indent_of(&self, row: usize) -> i64 {
        if let Some(&given) = self.given.get(&row) {
            return given;
        }
        let line = self.line(row);
        self.width(&line[..blanks(&line)])
    }

    /// The column byte `col` of line `row` is at, with its indent as given
    /// it already.
    fn column(&self, row: usize, col: usize) -> i64 {
        let line = self.line(row);
        let col = col.min(line.len());
        let indent = blanks(&line);
        let shift = (self.given.get(&row)).map_or(0, |&given| given - self.width(&line[..indent]));
        let col = self.width(line.get(..col).unwrap_or(&line));
        if col >= self.width(&line[..indent]) {
            col + shift
        } else {
            col
        }
    }

    /// The smallest node holding byte `col` of line `row`, or the root.
    fn node_at(&self, row: usize, col: usize) -> Node<'a> {
        let root = self.tree.root_node();
        let point = |column| Point { row, column };
        root.descendant_for_point_range(point(col), point(col + 1))
            .unwrap_or(root)
    }

    /// The first node on line `row`: at its first byte past its indent.
    fn first_node(&self, row: usize) -> Node<'a> {
        self.node_at(row, blanks(&self.line(row)))
    }

    fn marks(&self, node: Node<'_>) -> Marks {
        self.marks.get(&node.id()).copied().unwrap_or_default()
    }

    /// How line `row` should be indented, in columns, or `None` to keep
    /// it as it is: `get_indent` in nvim-treesitter's `indent.lua`, step
    /// by step.
    fn indent(&mut self, row: usize) -> Option<i64> {
        let line = self.line(row);
        let node = if line.trim_ascii().is_empty() {
            // A new line: what's before it says.
            let Some(before) = self.nonblank_before(row) else {
                return Some(0);
            };
            let text = self.line(before);
            let indent = blanks(&text);
            let trimmed = text.trim_ascii();
            let mut node = self.node_at(before, (indent + trimmed.len()).saturating_sub(1));
            if node.kind().contains("comment") {
                // Unless the line's all comment, the last node before
                // the comment at its end.
                let first = self.node_at(before, indent);
                if first.id() != node.id() {
                    let start = node.start_position().column.clamp(indent, text.len());
                    let code = text[indent..start].trim_ascii();
                    node = self.node_at(before, (indent + code.len()).saturating_sub(1));
                }
            }
            if self.marks(node).rules & END != 0 {
                node = self.first_node(row);
            }
            node
        } else {
            self.first_node(row)
        };
        if self.marks(node).rules & ZERO != 0 {
            return Some(0);
        }
        let step = self.step;
        let mut indent = 0;
        // The lines nodes started on that have indented already, so a
        // line's indented at most once by the nodes starting on it.
        let mut indented: Vec<usize> = Vec::new();
        // The nodes from the root down to it, as asking a node for its
        // parent goes down from the root each time.
        let root = self.tree.root_node();
        let mut chain = vec![root];
        while let Some(&last) = chain.last()
            && last.id() != node.id()
            && let Some(child) = last.child_with_descendant(node)
        {
            chain.push(child);
        }
        for depth in (0..chain.len()).rev() {
            let node = chain[depth];
            let parent = depth.checked_sub(1).map(|depth| chain[depth]);
            let mut marks = self.marks(node);
            let (srow, erow) = (node.start_position().row, node.end_position().row);
            let has = |rule: u8| marks.rules & rule != 0;
            let inside = srow < row && row <= erow;
            if !has(BEGIN) && !has(ALIGN) && has(AUTO) && inside {
                return None;
            }
            if !has(BEGIN) && has(IGNORE) && inside {
                return Some(0);
            }
            let fresh = !indented.contains(&srow);
            let mut done = false;
            if fresh && ((has(BRANCH) && srow == row) || (has(DEDENT) && srow != row)) {
                indent -= step;
                done = true;
            }
            let in_error = fresh && parent.is_some_and(|parent| parent.has_error());
            if fresh && let Some(pattern) = marks.begin {
                let settings = &self.rules.settings[pattern];
                if (srow != erow || in_error || settings.immediate)
                    && (srow != row || settings.start_at_same_line)
                {
                    indent += step;
                    done = true;
                }
            }
            if in_error && marks.align.is_none() {
                // A node in error lines up as its first child that does,
                // as a pattern can't capture the error node itself.
                // Looked for once for each node, whose children may be
                // thousands.
                if let Some(&align) = self.promoted.get(&node.id()) {
                    marks.align = align;
                } else {
                    let mut cursor = node.walk();
                    let align =
                        (node.children(&mut cursor)).find_map(|child| self.marks(child).align);
                    self.promoted.insert(node.id(), align);
                    // And it stays so, as in Neovim.
                    if align.is_some() {
                        self.marks.entry(node.id()).or_default().align = align;
                    }
                    marks.align = align;
                }
            }
            if fresh
                && let Some(pattern) = marks.align
                && (srow != erow || in_error)
                && srow != row
                && let Some(aligned) = self.align(node, pattern, row, indent)
            {
                match aligned {
                    Aligned::Absolute(at) => return Some(at),
                    Aligned::By(to) => indent = to,
                }
                done = true;
            }
            if done {
                indented.push(srow);
            }
        }
        Some(indent)
    }

    /// What `node`, an `@indent.align` from `pattern`, makes of line
    /// `row`'s indent, which is `indent` so far: `None` if it has no
    /// opening bracket.
    fn align(
        &mut self,
        node: Node<'a>,
        pattern: usize,
        row: usize,
        indent: i64,
    ) -> Option<Aligned> {
        let rules = self.rules;
        let settings = &rules.settings[pattern];
        let step = self.step;
        let (open, open_last) = match &settings.open {
            Some(delimiter) => self.delimiter(node, delimiter, true)?,
            None => (node, false),
        };
        let (close, close_last) = match &settings.close {
            Some(delimiter) => match self.delimiter(node, delimiter, false) {
                Some((close, last)) => (Some(close), last),
                None => (None, false),
            },
            None => (Some(node), false),
        };
        let open_row = open.start_position().row;
        let close_row = close.map(|close| close.start_position().row);
        let mut indent = indent;
        let mut absolute = false;
        if open_last {
            // A hanging indent: the line ends with the bracket, so the
            // lines inside are indented a step.
            indent += step;
            if close_last && close_row.is_some_and(|close| close < row) {
                indent = (indent - step).max(0);
            }
        } else if close_last && close_row.is_some_and(|close| open_row != close && close < row) {
            // The line is past the brackets.
            indent = (indent - step).max(0);
        } else {
            indent = self.column(open_row, open.start_position().column)
                + settings.increment.unwrap_or(1);
            absolute = true;
        }
        // The last line inside, with the closing bracket, steps further
        // if it would line up with what comes after, as a body would.
        if settings.avoid_last_matching_next
            && close_row.is_some_and(|close| close != open_row && close == row)
            && indent <= self.indent_of(open_row) + step
        {
            indent += step;
        }
        Some(if absolute {
            Aligned::Absolute(indent)
        } else {
            Aligned::By(indent)
        })
    }

    /// `node`'s first child of kind `delimiter`, its opening bracket if
    /// `open`, and whether nothing but blanks and the delimiter's
    /// characters come after it on its line. Found once for each node.
    fn delimiter(
        &mut self,
        node: Node<'a>,
        delimiter: &str,
        open: bool,
    ) -> Option<(Node<'a>, bool)> {
        if let Some(&found) = self.delimiters.get(&(node.id(), open)) {
            return found;
        }
        let mut cursor = node.walk();
        let child = (node.children(&mut cursor)).find(|child| child.kind() == delimiter);
        let found = child.map(|child| {
            let line = self.line(child.start_position().row);
            let after = line.get(child.end_position().column..).unwrap_or("");
            let last = (after.chars()).all(|ch| ch.is_ascii_whitespace() || delimiter.contains(ch));
            (child, last)
        });
        self.delimiters.insert((node.id(), open), found);
        found
    }
}

/// What an `@indent.align` makes of a line's indent.
enum Aligned {
    /// This many columns, whatever the nodes further out say.
    Absolute(i64),
    /// This many so far, for the nodes further out to add to.
    By(i64),
}

/// How many bytes of blanks `line` starts with.
fn blanks(line: &str) -> usize {
    line.len() - line.trim_ascii_start().len()
}

/// The [`Regex`] that matches what the Lua pattern `pattern` does, as
/// Neovim's `#lua-match?` reads it, or `None` if it uses what this doesn't
/// read: `%b`, `%f` and back references.
fn lua_pattern(pattern: &str) -> Option<Regex> {
    let mut regex = String::from("(?s)");
    let mut chars = pattern.chars().peekable();
    let mut first = true;
    while let Some(ch) = chars.next() {
        let at_start = std::mem::replace(&mut first, false);
        match ch {
            '^' if at_start => regex.push('^'),
            '$' if chars.peek().is_none() => regex.push('$'),
            '%' => regex.push_str(&lua_class(chars.next()?)?),
            '[' => {
                regex.push('[');
                if chars.peek() == Some(&'^') {
                    chars.next();
                    regex.push('^');
                }
                // A `]` first in a set is in it.
                if chars.peek() == Some(&']') {
                    chars.next();
                    regex.push_str("\\]");
                }
                loop {
                    match chars.next()? {
                        ']' => break,
                        '%' => {
                            let class = lua_class(chars.next()?)?;
                            // Inside a set, a class goes in without its own
                            // brackets; a negated one can't.
                            match class.strip_prefix('[').and_then(|c| c.strip_suffix(']')) {
                                Some(inner) if inner.starts_with('^') => return None,
                                Some(inner) => regex.push_str(inner),
                                None => regex.push_str(&class),
                            }
                        }
                        '-' => regex.push('-'),
                        ch => regex.push_str(&regex::escape(&ch.to_string())),
                    }
                }
                regex.push(']');
            }
            '.' => regex.push('.'),
            '*' | '+' | '?' => regex.push(ch),
            // Lua's lazy `*`.
            '-' => regex.push_str("*?"),
            ch => regex.push_str(&regex::escape(&ch.to_string())),
        }
    }
    Regex::new(&regex).ok()
}

/// The regex for what follows `%` in a Lua pattern: a class of
/// characters, or the character itself.
fn lua_class(ch: char) -> Option<String> {
    let class = match ch.to_ascii_lowercase() {
        'a' => "[A-Za-z]",
        'd' => "[0-9]",
        'l' => "[a-z]",
        'u' => "[A-Z]",
        's' => "[ \\t\\n\\x0B\\x0C\\r]",
        'w' => "[A-Za-z0-9]",
        'x' => "[0-9A-Fa-f]",
        'p' => "[!-/:-@\\[-`{-~]",
        'c' => "[\\x00-\\x1F\\x7F]",
        _ if ch.is_ascii_alphanumeric() => return None,
        _ => return Some(regex::escape(&ch.to_string())),
    };
    Some(if ch.is_ascii_uppercase() {
        format!("[^{}]", &class[1..class.len() - 1])
    } else {
        class.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_patterns_read_as_neovim_reads_them() {
        let matches = |pattern: &str, text: &str| lua_pattern(pattern).unwrap().is_match(text);
        // The four nvim-treesitter's indents use.
        assert!(matches("^%([^\n]", "(a or b)"));
        assert!(!matches("^%([^\n]", "(\n  a)"));
        assert!(matches("[^\n ]%)$", "(\n  a)"));
        assert!(!matches("[^\n ]%)$", "(\n  a\n)"));
        assert!(matches("^elif", "elif x"));
        assert!(!matches("^else", "x else"));
        // Classes, negated ones, and what's in sets.
        assert!(matches("^%s*%d+$", "  42"));
        assert!(!matches("^%S+$", "a b"));
        assert!(matches("^[%a_]+$", "snake_case"));
        assert!(matches("a.-b", "axxb"));
        assert!(matches("^%.$", "."));
        assert!(!matches("^%.$", "x"));
        // What it can't read.
        assert!(lua_pattern("%b()").is_none());
        assert!(lua_pattern("%1").is_none());
    }

    #[test]
    fn only_indent_captures_say_anything() {
        assert_eq!(rule("indent.begin"), BEGIN);
        assert_eq!(rule("indent.zero"), ZERO);
        assert_eq!(rule("_outer"), 0);
        assert_eq!(rule("indent"), 0);
    }
}
