//! The thread that parses: the event loop hands it each buffer's edits, and
//! it hands back highlights, so a parse never holds up a keystroke. It
//! answers questions about the pieces of code in each buffer's tree too.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::{fs, thread};

use clackatoo_text::{Edit, Piece, Rope, Span};
use tree_sitter::{Parser, QueryCursor, WasmStore, wasmtime};

use crate::document::Document;
use crate::highlight::Grammar;
use crate::pack::Pack;
use crate::pieces::{Ask, Finder, Objects};

/// A buffer, as the event loop numbers it.
pub type Key = u64;

/// New highlights for part of a buffer.
#[derive(Debug, PartialEq, Eq)]
pub struct Highlighted {
    /// The buffer.
    pub buffer: Key,
    /// The version of its text they're for.
    pub version: u64,
    /// The bytes they're for: everything else is as it was.
    pub range: Range<usize>,
    /// The spans there, in order.
    pub spans: Vec<Span>,
}

/// The answer to a question asked with [`Syntax::ask`].
#[derive(Debug, PartialEq, Eq)]
pub struct Answered {
    /// The question's number.
    pub ticket: u64,
    /// The version of the buffer's text the pieces are in: the one the
    /// question was asked of.
    pub version: u64,
    /// The pieces, as the question asked, or `None` if the buffer isn't
    /// parsed, as when its grammar couldn't be loaded.
    pub pieces: Option<Vec<Piece>>,
}

/// What the thread has found.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// New highlights.
    Highlighted(Highlighted),
    /// An answer.
    Answered(Answered),
}

enum Job {
    Open {
        buffer: Key,
        pack: Arc<Pack>,
        text: Rope,
        version: u64,
    },
    Edit {
        buffer: Key,
        version: u64,
        edit: Edit,
        text: Rope,
    },
    Close(Key),
    Ask {
        buffer: Key,
        ticket: u64,
        at: usize,
        ask: Ask,
    },
}

/// The parsing thread, and what it's found.
#[derive(Debug)]
pub struct Syntax {
    jobs: Sender<Job>,
    found: Receiver<Found>,
    /// Whether the thread is still running.
    running: Arc<AtomicBool>,
}

impl Syntax {
    /// Starts the thread. It calls `wake` when it has highlights or
    /// answers to take with [`take`](Self::take), and stops when this is
    /// dropped.
    ///
    /// # Errors
    ///
    /// If the thread can't be started.
    pub fn start(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (jobs, taken) = mpsc::channel();
        let (send, found) = mpsc::channel();
        let worker = Worker::new(send, Box::new(wake));
        let running = Arc::clone(&worker.running);
        thread::Builder::new()
            .name("syntax".to_owned())
            .spawn(move || worker.run(&taken))?;
        Ok(Self {
            jobs,
            found,
            running,
        })
    }

    /// Whether the thread has stopped, as one whose grammar panicked
    /// has, so questions asked of it will never be answered. It wakes the
    /// event loop as it stops.
    #[must_use]
    pub fn stopped(&self) -> bool {
        !self.running.load(Ordering::Acquire)
    }

    /// Starts parsing `buffer`, whose text is `text` at `version`, as
    /// `pack`'s language.
    pub fn open(&self, buffer: Key, pack: Arc<Pack>, text: Rope, version: u64) {
        self.send(Job::Open {
            buffer,
            pack,
            text,
            version,
        });
    }

    /// Tells it of `edit` to `buffer`, which made `text`, at `version`.
    pub fn edit(&self, buffer: Key, version: u64, edit: Edit, text: Rope) {
        self.send(Job::Edit {
            buffer,
            version,
            edit,
            text,
        });
    }

    /// Forgets `buffer`.
    pub fn close(&self, buffer: Key) {
        self.send(Job::Close(buffer));
    }

    /// Asks `ask` about byte `at` of `buffer`'s text as it is after the
    /// edits told of before, answered with an [`Answered`] numbered
    /// `ticket`. Questions are answered in order with edits: the answer is
    /// about that text, whatever edits come after.
    pub fn ask(&self, buffer: Key, ticket: u64, at: usize, ask: Ask) {
        self.send(Job::Ask {
            buffer,
            ticket,
            at,
            ask,
        });
    }

    /// The highlights and answers found since last asked.
    pub fn take(&self) -> impl Iterator<Item = Found> + '_ {
        self.found.try_iter()
    }

    fn send(&self, job: Job) {
        if self.jobs.send(job).is_err() {
            log::warn!("the syntax thread has stopped");
        }
    }
}

/// A buffer being parsed.
struct Open {
    grammar: Arc<Grammar>,
    document: Document,
    version: u64,
}

struct Worker {
    parser: Parser,
    cursor: QueryCursor,
    /// The cursor pieces are found with, apart from highlights', as it's
    /// left limited to a range.
    pieces_cursor: QueryCursor,
    /// Each pack's grammar, by its directory, once loaded, or `None` if it
    /// couldn't be.
    grammars: HashMap<PathBuf, Option<Arc<Grammar>>>,
    open: HashMap<Key, Open>,
    found: Sender<Found>,
    wake: Box<dyn Fn() + Send>,
    /// Whether it's running, until it's dropped: on the thread, as it
    /// stops, or panics.
    running: Arc<AtomicBool>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        (self.wake)();
    }
}

impl Worker {
    fn new(found: Sender<Found>, wake: Box<dyn Fn() + Send>) -> Self {
        let mut parser = Parser::new();
        match WasmStore::new(&engine()) {
            Ok(store) => {
                if let Err(err) = parser.set_wasm_store(store) {
                    log::warn!("syntax: {err}");
                }
            }
            Err(err) => log::warn!("syntax: can't run grammars: {err}"),
        }
        Self {
            parser,
            cursor: QueryCursor::new(),
            pieces_cursor: QueryCursor::new(),
            grammars: HashMap::new(),
            open: HashMap::new(),
            found,
            wake,
            running: Arc::new(AtomicBool::new(true)),
        }
    }

    fn run(mut self, jobs: &Receiver<Job>) {
        while let Ok(job) = jobs.recv() {
            // Everything waiting is taken at once, so a burst of edits is
            // parsed once.
            let mut changed = Vec::new();
            let mut found = Vec::new();
            for job in std::iter::once(job).chain(jobs.try_iter()) {
                if let Job::Ask {
                    buffer,
                    ticket,
                    at,
                    ask,
                } = job
                {
                    // Answered of the text as it is now, edits after it
                    // not yet taken, so parsed now if it's changed.
                    if let Some(index) = changed.iter().position(|&b| b == buffer) {
                        changed.remove(index);
                        found.extend(self.parse(buffer).map(Found::Highlighted));
                    }
                    found.push(Found::Answered(self.answer(buffer, ticket, at, ask)));
                } else if let Some(buffer) = self.take(job)
                    && !changed.contains(&buffer)
                {
                    changed.push(buffer);
                }
            }
            for buffer in changed {
                found.extend(self.parse(buffer).map(Found::Highlighted));
            }
            let any = !found.is_empty();
            for found in found {
                if self.found.send(found).is_err() {
                    return;
                }
            }
            if any {
                (self.wake)();
            }
        }
    }

    /// The answer to `ask` about byte `at` of `buffer`, numbered
    /// `ticket`, from its tree as of the last parse.
    fn answer(&mut self, buffer: Key, ticket: u64, at: usize, ask: Ask) -> Answered {
        let Some(open) = self.open.get(&buffer) else {
            return Answered {
                ticket,
                version: 0,
                pieces: None,
            };
        };
        let pieces = match open.document.tree() {
            Some(tree) => Finder {
                tree,
                text: open.document.text(),
                objects: open.grammar.objects.as_ref(),
                findable: open.grammar.findable,
                cursor: &mut self.pieces_cursor,
            }
            .answer(ask, at),
            None => Vec::new(),
        };
        Answered {
            ticket,
            version: open.version,
            pieces: Some(pieces),
        }
    }

    /// Takes in `job`, saying which buffer it changed, if any.
    fn take(&mut self, job: Job) -> Option<Key> {
        match job {
            Job::Open {
                buffer,
                pack,
                text,
                version,
            } => {
                let grammar = self.grammar(&pack)?;
                self.open.insert(
                    buffer,
                    Open {
                        grammar,
                        document: Document::new(text),
                        version,
                    },
                );
                Some(buffer)
            }
            Job::Edit {
                buffer,
                version,
                edit,
                text,
            } => {
                let open = self.open.get_mut(&buffer)?;
                open.document.edit(&edit, text);
                open.version = version;
                Some(buffer)
            }
            Job::Close(buffer) => {
                self.open.remove(&buffer);
                None
            }
            Job::Ask { .. } => None,
        }
    }

    /// Parses `buffer` again, and highlights what changed.
    fn parse(&mut self, buffer: Key) -> Option<Highlighted> {
        let open = self.open.get_mut(&buffer)?;
        if let Err(err) = self.parser.set_language(&open.grammar.language) {
            log::warn!("syntax: {err}");
            return None;
        }
        let range = open.document.parse(&mut self.parser)?;
        let tree = open.document.tree()?;
        let spans =
            (open.grammar).highlight(&mut self.cursor, tree, open.document.text(), range.clone());
        Some(Highlighted {
            buffer,
            version: open.version,
            range,
            spans,
        })
    }

    /// `pack`'s grammar, loaded the first time it's asked for.
    fn grammar(&mut self, pack: &Pack) -> Option<Arc<Grammar>> {
        if let Some(grammar) = self.grammars.get(&pack.dir) {
            return grammar.clone();
        }
        let grammar = match self.load(pack) {
            Ok(grammar) => {
                log::info!("syntax: loaded {}", pack.name);
                Some(Arc::new(grammar))
            }
            Err(err) => {
                log::warn!("syntax: language {}: {err}", pack.name);
                None
            }
        };
        self.grammars.insert(pack.dir.clone(), grammar.clone());
        grammar
    }

    fn load(&mut self, pack: &Pack) -> Result<Grammar, String> {
        let wasm = fs::read(pack.dir.join("grammar.wasm"))
            .map_err(|err| format!("grammar.wasm: {err}"))?;
        let highlights = match fs::read_to_string(pack.dir.join("highlights.scm")) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(format!("highlights.scm: {err}")),
        };
        let mut store =
            (self.parser.take_wasm_store()).ok_or_else(|| "grammars can't be run".to_owned())?;
        let language = store.load_language(&pack.grammar, &wasm);
        self.parser
            .set_wasm_store(store)
            .map_err(|err| err.to_string())?;
        let language = language.map_err(|err| format!("grammar.wasm: {err}"))?;
        let mut grammar =
            Grammar::new(language, &highlights).map_err(|err| format!("highlights.scm: {err}"))?;
        grammar.objects = objects(pack, &grammar.language);
        Ok(grammar)
    }
}

/// `pack`'s query for pieces of code, if it has one, compiled for
/// `language`. One that can't be read or compiled is passed over, and the
/// pieces found by their nodes' kinds instead.
fn objects(pack: &Pack, language: &tree_sitter::Language) -> Option<Objects> {
    let source = match fs::read_to_string(pack.dir.join("textobjects.scm")) {
        Ok(source) => source,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("syntax: language {}: textobjects.scm: {err}", pack.name);
            return None;
        }
    };
    Objects::new(language, &source).unwrap_or_else(|err| {
        log::warn!("syntax: language {}: textobjects.scm: {err}", pack.name);
        None
    })
}

/// The WebAssembly engine grammars run in. Each grammar's memory is
/// reserved as it grows, rather than 4 GiB of address space up front for
/// each, which is wasmtime's default.
fn engine() -> wasmtime::Engine {
    let mut config = wasmtime::Config::new();
    config.memory_reservation(0);
    config.memory_guard_size(0);
    wasmtime::Engine::new(&config).unwrap_or_else(|err| {
        log::warn!("syntax: wasmtime's settings: {err}");
        wasmtime::Engine::default()
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clackatoo_text::{Construct, Highlights, Kind};
    use proptest::prelude::*;

    use super::*;

    fn json() -> Arc<Pack> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/languages/json");
        Arc::new(Pack::read(&dir).unwrap())
    }

    fn worker() -> Worker {
        let (found, _) = mpsc::channel();
        Worker::new(found, Box::new(|| {}))
    }

    thread_local! {
        /// One worker for the property test, which compiles the grammar
        /// once rather than for each case.
        static WORKER: std::cell::RefCell<Worker> = std::cell::RefCell::new(worker());
    }

    /// Each byte's kind.
    fn kinds(len: usize, highlights: &Highlights) -> Vec<Option<Kind>> {
        let mut kinds = vec![None; len];
        for span in highlights.spans() {
            kinds[span.start..span.end].fill(Some(span.kind));
        }
        kinds
    }

    /// The highlights of a whole text, parsed afresh.
    fn fresh(text: &str) -> Highlights {
        WORKER.with_borrow_mut(|worker| fresh_by(worker, text))
    }

    fn fresh_by(worker: &mut Worker, text: &str) -> Highlights {
        worker.take(Job::Open {
            buffer: 1,
            pack: json(),
            text: Rope::from_str(text),
            version: 0,
        });
        let found = worker.parse(1).unwrap();
        assert_eq!(found.range, 0..text.len());
        let mut highlights = Highlights::new();
        highlights.replace(found.range.start, found.range.end, found.spans);
        highlights
    }

    #[test]
    fn a_grammar_loads_and_highlights() {
        let text = "{\"a\": [1, true, \"x\\n\"]}";
        let highlights = fresh(text);
        let span = |start, end, kind| Span { start, end, kind };
        assert_eq!(
            highlights.spans(),
            [
                // The key: the first pattern to capture it wins.
                span(1, 4, Kind::StringSpecial),
                span(7, 8, Kind::Number),
                span(10, 14, Kind::Constant),
                span(16, 18, Kind::String),
                span(18, 20, Kind::Escape),
                span(20, 21, Kind::String),
            ]
        );
    }

    #[test]
    fn a_pack_that_cant_be_loaded_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("language.toml"), "grammar = \"json\"").unwrap();
        fs::write(dir.path().join("grammar.wasm"), "not wasm").unwrap();
        let mut worker = worker();
        let open = Job::Open {
            buffer: 1,
            pack: Arc::new(Pack::read(dir.path()).unwrap()),
            text: Rope::from_str("{}"),
            version: 0,
        };
        assert_eq!(worker.take(open), None);
        assert!(worker.parse(1).is_none());
        // Asked about, it says it isn't parsed.
        let ask = Ask::Around(Construct::Function);
        assert_eq!(worker.answer(1, 5, 0, ask).pieces, None);
    }

    #[test]
    fn a_worker_stopping_says_so_and_wakes_the_loop() {
        let (found, _) = mpsc::channel();
        let woken = Arc::new(AtomicBool::new(false));
        let wake = Arc::clone(&woken);
        let worker = Worker::new(found, Box::new(move || wake.store(true, Ordering::Release)));
        let running = Arc::clone(&worker.running);
        assert!(running.load(Ordering::Acquire));
        drop(worker);
        assert!(!running.load(Ordering::Acquire));
        assert!(woken.load(Ordering::Acquire));
    }

    fn json_text() -> impl Strategy<Value = String> {
        "[{}\\[\\]\",:0-9a-z \n\\\\]{0,40}"
    }

    /// A change: where, how many bytes it takes away, and what it puts in.
    type Change = (prop::sample::Index, usize, String);

    /// Edits of one or two changes, each made to the text the one before
    /// made, and whether to parse after each.
    fn steps() -> impl Strategy<Value = Vec<(Vec<Change>, bool)>> {
        let change = (
            any::<prop::sample::Index>(),
            0..4usize,
            json_text().prop_map(|text| text.chars().take(4).collect::<String>()),
        );
        let edit = (prop::collection::vec(change, 1..3), any::<bool>());
        prop::collection::vec(edit, 1..6)
    }

    /// An edit to `rope` making `changes`.
    fn edit_of(rope: &Rope, changes: Vec<Change>) -> Edit {
        let len = rope.len_bytes();
        let mut places: Vec<_> = (changes.into_iter())
            .map(|(at, removing, inserted)| {
                let at = at.index(len + 1);
                (at, (at + removing).min(len), inserted)
            })
            .collect();
        places.sort_by_key(|&(at, ..)| at);
        let mut edit = Edit::new();
        let mut done = 0;
        for (at, end, inserted) in places {
            let at = at.max(done);
            let end = end.max(at);
            let removed = rope.byte_slice(at..end).to_string();
            edit.retain(at - done).insert(&inserted).delete(&removed);
            done = end;
        }
        edit.retain(len - done);
        edit
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        #[test]
        fn highlights_kept_up_to_date_match_a_fresh_parse(text in json_text(), steps in steps()) {
            WORKER.with_borrow_mut(|worker| kept_up_to_date(worker, &text, steps))?;
        }
    }

    /// Edits `text` as `steps` say, keeping its highlights up to date as a
    /// window does, and checks they end as a fresh parse finds them.
    fn kept_up_to_date(
        worker: &mut Worker,
        text: &str,
        steps: Vec<(Vec<Change>, bool)>,
    ) -> Result<(), TestCaseError> {
        let mut rope = Rope::from_str(text);
        let (pack, text) = (json(), rope.clone());
        worker.take(Job::Open {
            buffer: 2,
            pack,
            text,
            version: 0,
        });
        let mut highlights = Highlights::new();
        let parse = |worker: &mut Worker, highlights: &mut Highlights, version| {
            if let Some(found) = worker.parse(2) {
                assert_eq!(found.version, version);
                highlights.replace(found.range.start, found.range.end, found.spans);
            }
        };
        parse(worker, &mut highlights, 0);
        let mut version = 0;
        for (changes, parse_now) in steps {
            let edit = edit_of(&rope, changes);
            edit.apply(&mut rope).unwrap();
            highlights.map(&edit);
            version += 1;
            let text = rope.clone();
            worker.take(Job::Edit {
                buffer: 2,
                version,
                edit,
                text,
            });
            if parse_now {
                parse(worker, &mut highlights, version);
            }
        }
        parse(worker, &mut highlights, version);
        let text = rope.to_string();
        prop_assert_eq!(
            kinds(text.len(), &highlights),
            kinds(text.len(), &fresh_by(worker, &text)),
            "{:?}",
            text
        );
        Ok(())
    }

    // --- Pieces -------------------------------------------------------------

    /// What `ask` about byte `at` finds in `text`, read as `pack` says.
    fn ask_by(worker: &mut Worker, pack: Arc<Pack>, text: &str, at: usize, ask: Ask) -> Vec<Piece> {
        worker.take(Job::Open {
            buffer: 3,
            pack,
            text: Rope::from_str(text),
            version: 0,
        });
        worker.parse(3);
        worker.answer(3, 0, at, ask).pieces.unwrap()
    }

    /// The text of each piece found, and of its insides.
    fn texts(text: &str, pieces: &[Piece]) -> Vec<(String, Option<String>)> {
        (pieces.iter())
            .map(|piece| {
                let inner = piece.inner.map(|(start, end)| text[start..end].to_owned());
                (text[piece.start..piece.end].to_owned(), inner)
            })
            .collect()
    }

    fn piece(whole: &str, inner: Option<&str>) -> (String, Option<String>) {
        (whole.to_owned(), inner.map(str::to_owned))
    }

    const JSON: &str = "{\"a\": [1, 22, 333], \"b\": {\"c\": 4}}";

    fn json_ask(at: usize, ask: Ask) -> Vec<(String, Option<String>)> {
        let pieces = WORKER.with_borrow_mut(|worker| ask_by(worker, json(), JSON, at, ask));
        texts(JSON, &pieces)
    }

    #[test]
    fn the_pieces_around_a_place_are_found_innermost_first_with_their_insides() {
        let at = |text: &str| JSON.find(text).unwrap();
        assert_eq!(
            json_ask(at("4"), Ask::Around(Construct::Function)),
            [
                piece("\"c\": 4", Some("4")),
                piece("\"b\": {\"c\": 4}", Some("{\"c\": 4}")),
            ]
        );
        assert_eq!(
            json_ask(at("4"), Ask::Around(Construct::Class)),
            [
                piece("{\"c\": 4}", Some("\"c\": 4")),
                piece(JSON, Some(&JSON[1..JSON.len() - 1])),
            ]
        );
        // An argument takes the comma before it, or the first, after it.
        assert_eq!(
            json_ask(at("22"), Ask::Around(Construct::Argument)),
            [piece(", 22", Some("22"))]
        );
        assert_eq!(
            json_ask(at("1"), Ask::Around(Construct::Argument)),
            [piece("1,", Some("1"))]
        );
        // On the comma, the argument after it.
        assert_eq!(
            json_ask(at(", 333"), Ask::Around(Construct::Argument)),
            [piece(", 333", Some("333"))]
        );
    }

    #[test]
    fn with_none_around_a_place_the_next_is_found() {
        assert_eq!(
            json_ask(0, Ask::Around(Construct::Function)),
            [piece("\"a\": [1, 22, 333]", Some("[1, 22, 333]"))]
        );
        let end = JSON.len() - 1;
        assert_eq!(json_ask(end, Ask::Around(Construct::Argument)), []);
    }

    #[test]
    fn starts_are_found_after_and_before_a_place_nearest_first() {
        let starts = |at, backward, count| {
            let ask = Ask::Starts {
                construct: Construct::Function,
                backward,
                count,
            };
            let pieces = WORKER.with_borrow_mut(|worker| ask_by(worker, json(), JSON, at, ask));
            pieces.iter().map(|piece| piece.start).collect::<Vec<_>>()
        };
        let (a, b, c) = (1, JSON.find("\"b\"").unwrap(), JSON.find("\"c\"").unwrap());
        assert_eq!(starts(0, false, 2), [a, b]);
        assert_eq!(starts(a, false, 9), [b, c]);
        assert_eq!(starts(JSON.len(), true, 9), [c, b, a]);
        assert_eq!(starts(b, true, 1), [a]);
        assert!(starts(a, true, 1).is_empty());
    }

    #[test]
    fn a_pack_without_textobjects_finds_pieces_by_kinds() {
        // JSON's kinds aren't code's: none.
        let dir = tempfile::tempdir().unwrap();
        let fixture = json();
        for file in ["language.toml", "grammar.wasm", "highlights.scm"] {
            fs::copy(fixture.dir.join(file), dir.path().join(file)).unwrap();
        }
        let pack = Arc::new(Pack::read(dir.path()).unwrap());
        let mut worker = worker();
        let ask = Ask::Around(Construct::Function);
        assert_eq!(ask_by(&mut worker, pack.clone(), JSON, 9, ask), []);
        assert!(worker.grammar(&pack).unwrap().objects.is_none());
        assert!(worker.grammar(&json()).unwrap().objects.is_some());
    }

    const RUST: &str = "struct Point { x: i32, y: i32 }

impl Point {
    fn add(&self, other: Point, scale: i32) -> Point {
        let f = |a: i32| a * scale;
        Point { x: f(self.x + other.x), y: self.y }
    }
}
";

    /// Checks the pieces `rust` finds in [`RUST`], by its query if
    /// `query`, or else by kinds, where they differ.
    fn rust_pieces(worker: &mut Worker, rust: &Arc<Pack>, query: bool) {
        let mut ask = |at: &str, ask| {
            let at = RUST.find(at).unwrap();
            texts(RUST, &ask_by(worker, rust.clone(), RUST, at, ask))
        };
        let body =
            "let f = |a: i32| a * scale;\n        Point { x: f(self.x + other.x), y: self.y }";
        let function =
            format!("fn add(&self, other: Point, scale: i32) -> Point {{\n        {body}\n    }}");
        assert_eq!(
            ask("other.x", Ask::Around(Construct::Function)),
            [piece(&function, Some(body))]
        );
        assert_eq!(
            ask("other.x", Ask::Around(Construct::Class)),
            [piece(
                &format!("impl Point {{\n    {function}\n}}"),
                Some(&function)
            )]
        );
        assert_eq!(
            ask("x: i32", Ask::Around(Construct::Class)),
            [piece(
                "struct Point { x: i32, y: i32 }",
                Some("x: i32, y: i32")
            )]
        );
        let argument = Ask::Around(Construct::Argument);
        assert_eq!(
            ask("other: Point", argument),
            [piece(", other: Point", Some("other: Point"))]
        );
        assert_eq!(ask("&self", argument), [piece("&self,", Some("&self"))]);
        // On the comma after the first, both it and the next have it.
        assert_eq!(
            ask(", other", argument),
            [
                piece("&self,", Some("&self")),
                piece(", other: Point", Some("other: Point"))
            ]
        );
        // The query takes a struct's field too, the next out.
        assert_eq!(
            ask("self.x +", argument)[0],
            piece("self.x + other.x", Some("self.x + other.x"))
        );
        // The closure is a function only by kinds.
        let starts = (ask(
            "struct",
            Ask::Starts {
                construct: Construct::Function,
                backward: false,
                count: 2,
            },
        ))
        .len();
        assert_eq!(starts, if query { 1 } else { 2 });
    }

    #[test]
    #[ignore = "downloads every known language from GitHub: run with --ignored"]
    fn known_languages_install_and_find_their_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let mut worker = worker();
        for known in crate::KNOWN {
            crate::install(known, dir.path()).unwrap();
            let pack = Pack::read(&dir.path().join(known.name)).unwrap();
            let grammar = (worker.grammar(&pack)).unwrap_or_else(|| panic!("{}", known.name));
            let has = !known.textobjects.is_empty();
            assert_eq!(grammar.objects.is_some(), has, "{}", known.name);
        }

        let rust = Arc::new(Pack::read(&dir.path().join("rust")).unwrap());
        rust_pieces(&mut worker, &rust, true);
        // A pack installed before textobjects were: by kinds.
        let old = dir.path().join("old-rust");
        fs::create_dir(&old).unwrap();
        for file in ["language.toml", "grammar.wasm", "highlights.scm"] {
            fs::copy(rust.dir.join(file), old.join(file)).unwrap();
        }
        let old = Arc::new(Pack::read(&old).unwrap());
        rust_pieces(&mut worker, &old, false);

        // How long questions take about a big file: this crate's
        // biggest neighbour.
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gui/src/editor/mod.rs");
        let text = fs::read_to_string(path).unwrap();
        for pack in [&rust, &old] {
            worker.take(Job::Open {
                buffer: 9,
                pack: pack.clone(),
                text: Rope::from_str(&text),
                version: 0,
            });
            worker.parse(9);
            let middle = text.len() / 2;
            let starts = |backward| Ask::Starts {
                construct: Construct::Function,
                backward,
                count: 1,
            };
            for (what, at, ask) in [
                ("af", middle, Ask::Around(Construct::Function)),
                ("ac", middle, Ask::Around(Construct::Class)),
                ("aa", middle, Ask::Around(Construct::Argument)),
                (
                    "af at the end",
                    text.len(),
                    Ask::Around(Construct::Function),
                ),
                ("]f", middle, starts(false)),
                ("[f", middle, starts(true)),
                ("[f at the end", text.len(), starts(true)),
            ] {
                // The quickest of a few, as a busy machine can stop the
                // thread for a while in any one.
                let (found, took) = (0..5)
                    .map(|_| {
                        let took = std::time::Instant::now();
                        let found = worker.answer(9, 0, at, ask).pieces.unwrap().len();
                        (found, took.elapsed())
                    })
                    .min_by_key(|&(_, took)| took)
                    .unwrap();
                eprintln!("{} {what}: {found} in {took:?}", pack.name);
                assert!(
                    took < std::time::Duration::from_millis(100),
                    "{what}: {took:?}"
                );
            }
        }
    }

    #[test]
    fn a_question_is_answered_of_the_text_it_was_asked_of() {
        let (send, found) = mpsc::channel();
        let worker = Worker::new(send, Box::new(|| {}));
        let (jobs, taken) = mpsc::channel();
        let mut rope = Rope::from_str("[1]");
        // An edit putting `text` in at `at`, as version `version`.
        let mut insert = |at: usize, text: &str, version| {
            let mut edit = Edit::new();
            edit.retain(at).insert(text).retain(rope.len_bytes() - at);
            edit.apply(&mut rope).unwrap();
            Job::Edit {
                buffer: 1,
                version,
                edit,
                text: rope.clone(),
            }
        };
        let open = Job::Open {
            buffer: 1,
            pack: json(),
            text: Rope::from_str("[1]"),
            version: 0,
        };
        // All taken at once: an edit, the question, and an edit after it
        // that moves what it asks about.
        let first = insert(2, ", 2", 1);
        let ask = Job::Ask {
            buffer: 1,
            ticket: 7,
            at: 4,
            ask: Ask::Around(Construct::Argument),
        };
        let after = insert(1, "0, ", 2);
        for job in [open, first, ask, after] {
            jobs.send(job).unwrap();
        }
        drop(jobs);
        worker.run(&taken);
        let answer = found.try_iter().find_map(|found| match found {
            Found::Answered(answer) => Some(answer),
            Found::Highlighted(_) => None,
        });
        // "[1, 2]": the 2, with its comma, as the text was at version 1.
        let piece = Piece {
            start: 2,
            end: 5,
            inner: Some((4, 5)),
        };
        assert_eq!(
            answer,
            Some(Answered {
                ticket: 7,
                version: 1,
                pieces: Some(vec![piece]),
            })
        );
    }
}
