//! The thread that parses: the event loop hands it each buffer's edits, and
//! it hands back highlights, so a parse never holds up a keystroke.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::{fs, thread};

use clackatoo_text::{Edit, Rope, Span};
use tree_sitter::{Parser, QueryCursor, WasmStore, wasmtime};

use crate::document::Document;
use crate::highlight::Grammar;
use crate::pack::Pack;

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
}

/// The parsing thread, and what it's found.
#[derive(Debug)]
pub struct Syntax {
    jobs: Sender<Job>,
    found: Receiver<Highlighted>,
}

impl Syntax {
    /// Starts the thread. It calls `wake` when it has highlights to take
    /// with [`take`](Self::take), and stops when this is dropped.
    ///
    /// # Errors
    ///
    /// If the thread can't be started.
    pub fn start(wake: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (jobs, taken) = mpsc::channel();
        let (send, found) = mpsc::channel();
        thread::Builder::new()
            .name("syntax".to_owned())
            .spawn(move || Worker::new(send, Box::new(wake)).run(&taken))?;
        Ok(Self { jobs, found })
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

    /// The highlights found since last asked.
    pub fn take(&self) -> impl Iterator<Item = Highlighted> + '_ {
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
    /// Each pack's grammar, by its directory, once loaded, or `None` if it
    /// couldn't be.
    grammars: HashMap<PathBuf, Option<Arc<Grammar>>>,
    open: HashMap<Key, Open>,
    found: Sender<Highlighted>,
    wake: Box<dyn Fn() + Send>,
}

impl Worker {
    fn new(found: Sender<Highlighted>, wake: Box<dyn Fn() + Send>) -> Self {
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
            grammars: HashMap::new(),
            open: HashMap::new(),
            found,
            wake,
        }
    }

    fn run(mut self, jobs: &Receiver<Job>) {
        while let Ok(job) = jobs.recv() {
            // Everything waiting is taken at once, so a burst of edits is
            // parsed once.
            let mut changed = Vec::new();
            for job in std::iter::once(job).chain(jobs.try_iter()) {
                if let Some(buffer) = self.take(job)
                    && !changed.contains(&buffer)
                {
                    changed.push(buffer);
                }
            }
            let mut any = false;
            for buffer in changed {
                if let Some(found) = self.parse(buffer) {
                    any = true;
                    if self.found.send(found).is_err() {
                        return;
                    }
                }
            }
            if any {
                (self.wake)();
            }
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
        Grammar::new(language, &highlights).map_err(|err| format!("highlights.scm: {err}"))
    }
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

    use clackatoo_text::{Highlights, Kind};
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
}
