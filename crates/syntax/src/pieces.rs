//! Finding the [`Piece`]s of code a tree holds: the functions, classes
//! and arguments that text objects such as `af` pick out and motions such
//! as `]f` go to.
//!
//! A pack's `textobjects.scm`, if it has one, says where they are, as
//! nvim-treesitter-textobjects' queries do: `@function.outer` is a whole
//! function and `@function.inner` its body, and so `@class.*` and
//! `@parameter.*`. Captures of one name in one match make one piece, from
//! the first node to the last, so `"," @parameter.outer . (parameter)
//! @parameter.outer` is an argument with the comma before it.
//!
//! A pack without one, such as one installed before clackatoo read them,
//! is read by the kinds of its nodes instead: a node whose kind says
//! function, method, closure or lambda, and which has a body, is a
//! function; one whose kind says class, struct, impl, trait, interface or
//! enum, with a body, is a class; and a node in a list of parameters or
//! arguments is an argument.

use std::ops::Range;

use clackatoo_text::{Construct, Piece, Rope};
use tree_sitter::{Language, Node, Query, QueryCursor, StreamingIterator, Tree};

use crate::highlight::RopeText;

/// How far ahead or back a first look for the pieces after or before a
/// place goes, in bytes. Each look after goes twice as far, so finding the
/// next piece costs about as much as the text between.
const FIRST_LOOK: usize = 4 * 1024;

/// The most starts a motion's count asks for.
const MOST_STARTS: usize = 10_000;

/// A pack's `textobjects.scm`, compiled.
#[derive(Debug)]
pub(crate) struct Objects {
    query: Query,
    /// The index of each construct's `outer` and `inner` captures, if the
    /// query has them, in the order of [`index`].
    captures: [[Option<u32>; 2]; 3],
}

impl Objects {
    /// `source` compiled for `language`, or `None` if it finds none of the
    /// pieces clackatoo uses.
    pub(crate) fn new(
        language: &Language,
        source: &str,
    ) -> Result<Option<Self>, tree_sitter::QueryError> {
        let query = Query::new(language, source)?;
        let mut captures = [[None; 2]; 3];
        for (construct, name) in [
            (Construct::Function, "function"),
            (Construct::Class, "class"),
            (Construct::Argument, "parameter"),
        ] {
            for (side, part) in ["outer", "inner"].into_iter().enumerate() {
                captures[index(construct)][side] =
                    query.capture_index_for_name(&format!("{name}.{part}"));
            }
        }
        let any = captures.iter().flatten().any(Option::is_some);
        Ok(any.then_some(Self { query, captures }))
    }
}

fn index(construct: Construct) -> usize {
    match construct {
        Construct::Function => 0,
        Construct::Class => 1,
        Construct::Argument => 2,
    }
}

/// What a question about a buffer's tree asks: where its pieces are, or
/// how its lines should be indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The pieces of this kind around a place, innermost first, each with
    /// its insides; or if there are none, the first after it.
    Around(Construct),
    /// The next `count` pieces of this kind to start after a place, or
    /// before it if `backward`, nearest first. Their insides aren't
    /// given.
    Starts {
        /// The kind.
        construct: Construct,
        /// Whether to look before the place.
        backward: bool,
        /// How many.
        count: usize,
    },
    /// How the lines from the place's to `end`'s should be indented, as
    /// the pack's `indents.scm` says.
    Indent {
        /// A byte in the last line.
        end: usize,
        /// The columns a step of indent is.
        shiftwidth: u32,
        /// The columns tabs stop at multiples of.
        tabstop: u32,
    },
}

/// A tree, and how to find the pieces in it.
pub(crate) struct Finder<'a> {
    pub(crate) tree: &'a Tree,
    pub(crate) text: &'a Rope,
    /// The pack's query, or `None` to go by kinds.
    pub(crate) objects: Option<&'a Objects>,
    /// Which constructs any of the grammar's kinds could be, by
    /// [`findable_by_kinds`], for going by kinds.
    pub(crate) findable: [bool; 3],
    pub(crate) cursor: &'a mut QueryCursor,
}

/// The two sides of a piece found, by the query or by kinds.
#[derive(Default)]
struct Found {
    /// Whole pieces.
    outer: Vec<(usize, usize)>,
    /// Their insides.
    inner: Vec<(usize, usize)>,
}

impl Finder<'_> {
    /// What `ask` asks about byte `at`.
    pub(crate) fn answer(&mut self, ask: Ask, at: usize) -> Vec<Piece> {
        let at = at.min(self.text.len_bytes());
        match ask {
            Ask::Around(construct) => self.around(construct, at),
            Ask::Starts {
                construct,
                backward,
                count,
            } => self.starts(construct, at, backward, count.min(MOST_STARTS)),
            // No pieces: the indenter answers it.
            Ask::Indent { .. } => Vec::new(),
        }
    }

    fn around(&mut self, construct: Construct, at: usize) -> Vec<Piece> {
        // Every match with a node at the place, which is every piece
        // around it, and their insides with them: the query cursor looks
        // at all the children of a node it goes down, though it goes down
        // only those at the place.
        let mut near = self.find(construct, at..at + 1);
        let mut outers: Vec<(usize, usize)> = (near.outer.iter().copied())
            .filter(|&(start, end)| start <= at && at < end)
            .collect();
        if outers.is_empty() {
            // None around it: the first after it, the biggest of those
            // starting there, and what's at its start.
            let after = self.after(construct, at, 1);
            let first = after.first().map(|piece| piece.0);
            let biggest = (after.into_iter())
                .filter(|piece| Some(piece.0) == first)
                .max_by_key(|piece| piece.1);
            let Some(biggest) = biggest else {
                return Vec::new();
            };
            outers.push(biggest);
            near = self.find(construct, biggest.0..biggest.0 + 1);
        }
        outers.sort_by_key(|&(start, end)| (end - start, start));
        outers.dedup();
        // Where a query gives a piece no insides, its body's are taken:
        // one with a pattern such as `"{" _+ @class.inner "}"` matches no
        // body with a comma in it.
        let root = self.tree.root_node();
        (outers.iter())
            .map(|&outer| Piece {
                start: outer.0,
                end: outer.1,
                inner: (insides(outer, &near))
                    .or_else(|| self.objects.and_then(|_| insides_by_body(root, outer))),
            })
            .collect()
    }

    /// The starts of the first pieces after byte `at`, or before it if
    /// `backward`, at most `count`, nearest first.
    fn starts(
        &mut self,
        construct: Construct,
        at: usize,
        backward: bool,
        count: usize,
    ) -> Vec<Piece> {
        let found = if backward {
            self.before(construct, at, count)
        } else {
            self.after(construct, at, count)
        };
        let mut starts: Vec<usize> = found.iter().map(|&(start, _)| start).collect();
        starts.dedup();
        starts.truncate(count);
        (starts.into_iter())
            .filter_map(|start| {
                let end = (found.iter())
                    .filter(|piece| piece.0 == start)
                    .map(|piece| piece.1)
                    .max()?;
                Some(Piece {
                    start,
                    end,
                    inner: None,
                })
            })
            .collect()
    }

    /// The pieces starting after byte `at`, nearest first, looking further
    /// until there are `count` starts or the text ends. Those starting at
    /// one place come together.
    fn after(&mut self, construct: Construct, at: usize, count: usize) -> Vec<(usize, usize)> {
        let len = self.text.len_bytes();
        let mut found = Vec::new();
        let (mut from, mut look) = (at + 1, FIRST_LOOK);
        while from < len {
            let to = from.saturating_add(look).min(len);
            // A piece starting in the range has a node in it, so it's
            // found; one found starting earlier was found before.
            let mut new: Vec<_> = (self.find(construct, from..to).outer.into_iter())
                .filter(|&(start, _)| (from..to).contains(&start))
                .collect();
            new.sort_unstable();
            found.extend(new);
            if distinct_starts(&found) >= count {
                break;
            }
            (from, look) = (to, look.saturating_mul(2));
        }
        found
    }

    /// The pieces starting before byte `at`, nearest first, as
    /// [`after`](Self::after) finds those after.
    fn before(&mut self, construct: Construct, at: usize, count: usize) -> Vec<(usize, usize)> {
        let mut found = Vec::new();
        let (mut to, mut look) = (at, FIRST_LOOK);
        while to > 0 {
            let from = to.saturating_sub(look);
            let mut new: Vec<_> = (self.find(construct, from..to).outer.into_iter())
                .filter(|&(start, _)| (from..to).contains(&start))
                .collect();
            new.sort_unstable_by(|a, b| b.cmp(a));
            found.extend(new);
            if distinct_starts(&found) >= count {
                break;
            }
            (to, look) = (from, look.saturating_mul(2));
        }
        found
    }

    /// The pieces with a node in bytes `range`.
    fn find(&mut self, construct: Construct, range: Range<usize>) -> Found {
        match self.objects {
            Some(objects) => self.by_query(objects, construct, range),
            None if self.findable[index(construct)] => {
                by_kinds(self.tree.root_node(), construct, &range)
            }
            // None of the grammar's kinds could be one: JSON has no
            // functions.
            None => Found::default(),
        }
    }

    fn by_query(&mut self, objects: &Objects, construct: Construct, range: Range<usize>) -> Found {
        let [outer, inner] = objects.captures[index(construct)];
        let mut found = Found::default();
        if outer.is_none() && inner.is_none() {
            return found;
        }
        self.cursor.set_byte_range(range);
        let mut matches =
            (self.cursor).matches(&objects.query, self.tree.root_node(), RopeText(self.text));
        while let Some(matched) = matches.next() {
            // Every node a capture takes in one match makes one piece.
            let span = |capture: Option<u32>| -> Option<(usize, usize)> {
                let capture = capture?;
                let nodes = matched.captures().iter().filter(|c| c.index == capture);
                nodes.fold(None, |span, c| {
                    let (start, end) = (c.node.start_byte(), c.node.end_byte());
                    Some(span.map_or((start, end), |(s, e): (usize, usize)| {
                        (s.min(start), e.max(end))
                    }))
                })
            };
            found.outer.extend(span(outer));
            found.inner.extend(span(inner));
        }
        found
    }
}

/// How many places the pieces in `found` start at.
fn distinct_starts(found: &[(usize, usize)]) -> usize {
    let mut starts: Vec<usize> = found.iter().map(|piece| piece.0).collect();
    starts.sort_unstable();
    starts.dedup();
    starts.len()
}

/// The insides of `outer`: the biggest of `within`'s insides in it that
/// aren't in a smaller piece in it, such as a function's body but not the
/// body of a closure in it.
fn insides(outer: (usize, usize), within: &Found) -> Option<(usize, usize)> {
    let inside = |a: (usize, usize), b: (usize, usize)| b.0 <= a.0 && a.1 <= b.1;
    (within.inner.iter().copied())
        .filter(|&inner| inside(inner, outer) && inner.0 < inner.1)
        .filter(|&inner| {
            !(within.outer.iter().copied())
                .any(|smaller| smaller != outer && inside(smaller, outer) && inside(inner, smaller))
        })
        .max_by_key(|&(start, end)| (end - start, std::cmp::Reverse(start)))
}

// --- By kinds ---------------------------------------------------------------

/// What a node of kind `kind` may be, by its name alone: a function or a
/// class if it has a body too.
pub(crate) fn kind_of(kind: &str) -> Option<Construct> {
    const FUNCTIONS: &[&str] = &["function", "method", "closure", "lambda"];
    const CLASSES: &[&str] = &["class", "struct", "impl", "trait", "interface", "enum"];
    if FUNCTIONS.iter().any(|word| kind.contains(word)) {
        Some(Construct::Function)
    } else if CLASSES.iter().any(|word| kind.contains(word))
        // A struct written out, or an enum's variant, isn't one.
        && !["_expression", "_variant", "_pattern"].iter().any(|end| kind.ends_with(end))
    {
        Some(Construct::Class)
    } else {
        None
    }
}

/// Which constructs, in the order of [`index`], a node of one of
/// `language`'s kinds could be, by its name: none at all for JSON's, so
/// its trees needn't be walked.
pub(crate) fn findable_by_kinds(language: &Language) -> [bool; 3] {
    let mut findable = [false; 3];
    let count = u16::try_from(language.node_kind_count()).unwrap_or(u16::MAX);
    for id in (0..count).filter(|&id| language.node_kind_is_named(id)) {
        let Some(kind) = language.node_kind_for_id(id) else {
            continue;
        };
        if let Some(construct) = kind_of(kind) {
            findable[index(construct)] = true;
        }
        if is_list(kind) {
            findable[index(Construct::Argument)] = true;
        }
    }
    findable
}

/// Whether a node of kind `kind` lists parameters or arguments.
pub(crate) fn is_list(kind: &str) -> bool {
    ["parameters", "arguments", "parameter_list", "argument_list"]
        .iter()
        .any(|end| kind.ends_with(end))
}

/// The pieces with a node in bytes `range` of the tree under `root`, by
/// their nodes' kinds.
///
/// It goes down every node with a byte in the range, and no others, and
/// looks at their children from the one before the range to the first
/// past it, much as the query cursor does, so an argument with its comma
/// in the range is found from its comma.
fn by_kinds(root: Node<'_>, construct: Construct, range: &Range<usize>) -> Found {
    let mut found = Found::default();
    let mut cursor = root.walk();
    let touches = |node: Node<'_>| {
        (node.start_byte() < range.end && range.start < node.end_byte())
            || node.start_byte() == range.start
    };
    loop {
        let node = cursor.node();
        piece_of(node, construct, &mut found);
        if touches(node) && cursor.goto_first_child_for_byte(range.start).is_some() {
            // And the child before, whose comma may be in the range.
            if let Some(before) = cursor.node().prev_sibling() {
                piece_of(before, construct, &mut found);
            }
            continue;
        }
        // The children after the first past the range aren't looked at.
        let mut next = node.start_byte() < range.end;
        loop {
            if next && cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return found;
            }
            // A node gone down has a byte in the range.
            next = true;
        }
    }
}

/// Adds `node` to `found` if it's a piece of kind `construct`.
fn piece_of(node: Node<'_>, construct: Construct, found: &mut Found) {
    if !node.is_named() || node.is_extra() {
        return;
    }
    let span = |node: Node<'_>| (node.start_byte(), node.end_byte());
    if construct == Construct::Argument {
        if !node.parent().is_some_and(|parent| is_list(parent.kind())) {
            return;
        }
        let comma = |sibling: &Node<'_>| sibling.kind() == ",";
        let outer = if let Some(before) = node.prev_sibling().filter(comma) {
            (before.start_byte(), node.end_byte())
        } else if let Some(after) = node.next_sibling().filter(comma) {
            (node.start_byte(), after.end_byte())
        } else {
            span(node)
        };
        found.outer.push(outer);
        found.inner.push(span(node));
        return;
    }
    if kind_of(node.kind()) != Some(construct) {
        return;
    }
    if node.child_by_field_name("body").is_none() {
        return;
    }
    found.outer.push(span(node));
    found.inner.extend(body_insides(node));
}

/// The insides of `node`'s body, if it has one: inside its brackets, if
/// it has them, as `{` and `}`. None if there's nothing inside them.
fn body_insides(node: Node<'_>) -> Option<(usize, usize)> {
    let body = node.child_by_field_name("body")?;
    let count = body.child_count();
    let bracket = |child: Option<Node<'_>>| child.is_some_and(|child| !child.is_named());
    if count < 2 || !bracket(body.child(0)) || !bracket(body.child(count - 1)) {
        return Some((body.start_byte(), body.end_byte()));
    }
    let (first, last) = (body.child(1)?, body.child(count - 2)?);
    (count > 2).then(|| (first.start_byte(), last.end_byte()))
}

/// The insides of the node that's all of bytes `start` to `end`, by its
/// body, for a piece a query found without them.
fn insides_by_body(root: Node<'_>, (start, end): (usize, usize)) -> Option<(usize, usize)> {
    let mut node = root.descendant_for_byte_range(start, end)?;
    loop {
        if let Some(insides) = body_insides(node) {
            return Some(insides);
        }
        // The outermost of the nodes that are all of it is as likely.
        match node.parent() {
            Some(parent) if parent.byte_range() == (start..end) => node = parent,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_say_what_a_node_may_be() {
        for function in [
            "function_item",
            "method_definition",
            "closure_expression",
            "lambda",
        ] {
            assert_eq!(kind_of(function), Some(Construct::Function), "{function}");
        }
        for class in [
            "struct_item",
            "impl_item",
            "class_declaration",
            "interface_declaration",
        ] {
            assert_eq!(kind_of(class), Some(Construct::Class), "{class}");
        }
        assert_eq!(kind_of("enum_variant"), None);
        assert_eq!(kind_of("struct_expression"), None);
        assert_eq!(kind_of("identifier"), None);
        assert!(is_list("formal_parameters") && is_list("argument_list"));
        assert!(!is_list("parameter"));
    }

    #[test]
    fn json_has_no_kinds_to_find_pieces_by() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/languages/json/grammar.wasm");
        let wasm = std::fs::read(dir).unwrap();
        let engine = tree_sitter::wasmtime::Engine::default();
        let mut store = tree_sitter::WasmStore::new(&engine).unwrap();
        let json = store.load_language("json", &wasm).unwrap();
        assert_eq!(findable_by_kinds(&json), [false; 3]);
    }

    #[test]
    fn a_piece_has_the_insides_that_are_its_own() {
        let within = Found {
            outer: vec![(0, 100), (20, 50)],
            // Its body, and the body of the smaller piece in it.
            inner: vec![(10, 90), (30, 40)],
        };
        assert_eq!(insides((0, 100), &within), Some((10, 90)));
        assert_eq!(insides((20, 50), &within), Some((30, 40)));
        assert_eq!(insides((60, 70), &within), None);
    }
}
