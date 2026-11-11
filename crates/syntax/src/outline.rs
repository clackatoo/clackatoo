//! Compact source outlines. Parsers belong to background jobs, not the editor
//! worker; full trees are discarded after extracting bounded symbol records.

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use ring::digest::{SHA256, digest};
use tree_sitter::{Node, ParseOptions, Parser, WasmStore};

use crate::Pack;

/// Bump when extraction semantics change, invalidating persistent records.
pub const OUTLINE_VERSION: u32 = 1;

/// A definition, with one-based source lines and a body-free signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    /// The locally declared name.
    pub name: String,
    /// Enclosing definitions followed by the name, separated with `::`.
    pub qualified: String,
    /// A broad language-independent kind.
    pub kind: &'static str,
    /// First source line, counting from one.
    pub line: u32,
    /// Last source line, counting from one.
    pub end_line: u32,
    /// Declaration text, with whitespace collapsed and no body.
    pub signature: String,
}

/// Bounded syntax information for one source version.
#[derive(Debug, Default)]
pub struct Outline {
    /// Definitions in source order.
    pub definitions: Vec<Definition>,
    /// Import declarations in source order. These are syntax, not resolved edges.
    pub imports: Vec<String>,
    /// The parser recovered from at least one syntax error.
    pub syntax_errors: bool,
    /// The per-file definition limit was reached.
    pub truncated: bool,
}

/// One background parser and at most one loaded grammar. It may move between
/// workers between slices; it never retains source trees or source text.
pub struct OutlineParser {
    parser: Parser,
    loaded: Option<(PathBuf, [u8; 32])>,
    fingerprints: Vec<Fingerprint>,
}

struct Fingerprint {
    path: PathBuf,
    grammar: String,
    modified: SystemTime,
    len: u64,
    hash: [u8; 32],
}

impl std::fmt::Debug for OutlineParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutlineParser")
            .field("loaded", &self.loaded)
            .finish_non_exhaustive()
    }
}

impl OutlineParser {
    /// Creates an independent parser. Does not load a grammar yet.
    ///
    /// # Errors
    /// If the WebAssembly parser store cannot be created.
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            parser: parser()?,
            loaded: None,
            fingerprints: Vec::new(),
        })
    }

    /// Content fingerprint of the installed grammar and extraction version.
    ///
    /// # Errors
    /// If the grammar cannot be read or exceeds its size limit.
    pub fn fingerprint(&mut self, pack: &Pack) -> Result<[u8; 32], String> {
        let path = pack.dir.join("grammar.wasm");
        let meta = path.metadata().map_err(|err| err.to_string())?;
        let modified = meta.modified().map_err(|err| err.to_string())?;
        if let Some(cached) = self.fingerprints.iter().find(|cached| {
            cached.path == path
                && cached.grammar == pack.grammar
                && cached.modified == modified
                && cached.len == meta.len()
        }) {
            return Ok(cached.hash);
        }
        if meta.len() > 32 * 1024 * 1024 {
            return Err("Grammar is larger than 32 MiB.".into());
        }
        let mut bytes = std::fs::read(&path).map_err(|err| err.to_string())?;
        bytes.extend(OUTLINE_VERSION.to_le_bytes());
        bytes.extend(pack.grammar.as_bytes());
        let mut hash = [0; 32];
        hash.copy_from_slice(digest(&SHA256, &bytes).as_ref());
        self.fingerprints.retain(|cached| cached.path != path);
        if self.fingerprints.len() >= 32 {
            self.fingerprints.remove(0);
        }
        self.fingerprints.push(Fingerprint {
            path,
            grammar: pack.grammar.clone(),
            modified,
            len: meta.len(),
            hash,
        });
        Ok(hash)
    }

    /// Extracts definitions and imports, with a deadline and cooperative
    /// cancellation. Source larger than 4 MiB is deliberately refused.
    ///
    /// # Errors
    /// If loading fails, the source exceeds a limit, or parsing is cancelled.
    pub fn extract(
        &mut self,
        pack: &Pack,
        text: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Outline, String> {
        if text.len() > 4 * 1024 * 1024 {
            return Err("Source exceeds the 4 MiB outline limit.".into());
        }
        if cancelled() {
            return Err("Outline extraction cancelled.".into());
        }
        let hash = self.fingerprint(pack)?;
        if self
            .loaded
            .as_ref()
            .is_none_or(|(path, was)| *path != pack.dir || *was != hash)
        {
            self.loaded = None;
            self.parser = parser()?;
            let mut store = self
                .parser
                .take_wasm_store()
                .ok_or("No WebAssembly grammar store.")?;
            let wasm =
                std::fs::read(pack.dir.join("grammar.wasm")).map_err(|err| err.to_string())?;
            let language = store
                .load_language(&pack.grammar, &wasm)
                .map_err(|err| err.to_string());
            self.parser
                .set_wasm_store(store)
                .map_err(|err| err.to_string())?;
            self.parser
                .set_language(&language?)
                .map_err(|err| err.to_string())?;
            self.loaded = Some((pack.dir.clone(), hash));
        }
        let due = Instant::now() + Duration::from_millis(500);
        let stop = || cancelled() || Instant::now() >= due;
        let mut progress = |_: &tree_sitter::ParseState| {
            if stop() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        self.parser.reset();
        let mut read =
            |byte: usize, _: tree_sitter::Point| text.as_bytes().get(byte..).unwrap_or_default();
        let tree = self
            .parser
            .parse_with_options(
                &mut read,
                None,
                Some(ParseOptions::new().progress_callback(&mut progress)),
            )
            .ok_or("Outline parsing was cancelled or exceeded 500 ms.")?;
        collect(&tree, text, &stop)
    }
}

fn collect(
    tree: &tree_sitter::Tree,
    text: &str,
    stop: &dyn Fn() -> bool,
) -> Result<Outline, String> {
    let mut result = Outline {
        syntax_errors: tree.root_node().has_error(),
        ..Outline::default()
    };
    let mut cursor = tree.walk();
    let mut scopes: Vec<(usize, String, &'static str)> = Vec::new();
    let mut visited = 0;
    loop {
        let node = cursor.node();
        visited += 1;
        if visited % 256 == 0 && stop() {
            return Err("Outline extraction cancelled or timed out.".into());
        }
        while scopes
            .last()
            .is_some_and(|(end, ..)| *end <= node.start_byte())
        {
            scopes.pop();
        }
        if let Some(kind) = definition_kind(node) {
            let local = scopes.iter().any(|(_, _, kind)| *kind == "function");
            if !(local && kind == "constant")
                && let Some(name) = name_of(node, text)
            {
                let parents: Vec<_> = scopes
                    .iter()
                    .rev()
                    .take(3)
                    .map(|(_, name, _)| name.as_str())
                    .collect();
                let qualified = parents
                    .into_iter()
                    .rev()
                    .chain(std::iter::once(name.as_str()))
                    .collect::<Vec<_>>()
                    .join("::");
                let qualified = if scopes.len() > 3 {
                    format!("…::{qualified}")
                } else {
                    qualified
                };
                result.definitions.push(Definition {
                    name: name.clone(),
                    qualified,
                    kind,
                    line: line(node.start_position().row),
                    end_line: line(node.end_position().row),
                    signature: signature(node, text),
                });
                if matches!(kind, "function" | "class" | "module" | "impl") {
                    scopes.push((node.end_byte(), name, kind));
                }
            }
        } else if matches!(
            node.kind(),
            "use_declaration"
                | "import_statement"
                | "import_from_statement"
                | "import_declaration"
                | "preproc_include"
        ) && result.imports.len() < 256
        {
            result.imports.push(compact(
                node.utf8_text(text.as_bytes()).unwrap_or_default(),
                256,
            ));
        }
        if result.definitions.len() >= 4096 {
            result.truncated = true;
            break;
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(result);
            }
        }
    }
    Ok(result)
}

fn parser() -> Result<Parser, String> {
    let mut parser = Parser::new();
    let store = WasmStore::new(&crate::worker::engine()).map_err(|err| err.to_string())?;
    parser
        .set_wasm_store(store)
        .map_err(|err| err.to_string())?;
    Ok(parser)
}

fn line(row: usize) -> u32 {
    u32::try_from(row.saturating_add(1)).unwrap_or(u32::MAX)
}

fn definition_kind(node: Node<'_>) -> Option<&'static str> {
    Some(match node.kind() {
        "function_item"
        | "function_definition"
        | "function_declaration"
        | "function"
        | "method_definition"
        | "method_declaration"
        | "method"
        | "constructor_declaration" => "function",
        "class_definition"
        | "class_declaration"
        | "struct_item"
        | "struct_specifier"
        | "interface_declaration"
        | "trait_item"
        | "enum_item"
        | "enum_specifier"
        | "type_spec" => "class",
        "type_alias_declaration" | "type_item" => "type",
        "mod_item" | "module_definition" | "internal_module" => "module",
        "impl_item" => "impl",
        "const_item" | "static_item" | "constant_declaration" => "constant",
        "variable_declarator"
            if node.child_by_field_name("value").is_some_and(|value| {
                matches!(value.kind(), "arrow_function" | "function_expression")
            }) =>
        {
            "function"
        }
        _ => return None,
    })
}

fn name_of(node: Node<'_>, text: &str) -> Option<String> {
    let mut name = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("declarator"))
        .or_else(|| node.child_by_field_name("type"))?;
    for _ in 0..8 {
        let Some(inner) = name.child_by_field_name("declarator") else {
            break;
        };
        name = inner;
    }
    let name = compact(name.utf8_text(text.as_bytes()).ok()?, 160);
    (!name.is_empty()).then_some(name)
}

fn signature(node: Node<'_>, text: &str) -> String {
    let end = node
        .child_by_field_name("body")
        .or_else(|| {
            node.child_by_field_name("value")
                .and_then(|value| value.child_by_field_name("body"))
        })
        .map_or(node.end_byte(), |body| body.start_byte());
    let end = end.min(node.start_byte().saturating_add(1024));
    let mut end = end;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    compact(&text[node.start_byte()..end], 320)
}

fn compact(text: &str, most: usize) -> String {
    let mut out = String::new();
    let mut space = false;
    let mut count = 0;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space && count < most {
            out.push(' ');
            count += 1;
        }
        if count >= most {
            break;
        }
        out.push(ch);
        count += 1;
        space = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack() -> Pack {
        Pack::read(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/languages/curly"),
        )
        .unwrap()
    }

    #[test]
    fn a_changed_grammar_export_invalidates_the_cached_fingerprint() {
        let mut parser = OutlineParser::new().unwrap();
        let mut pack = pack();
        let first = parser.fingerprint(&pack).unwrap();
        assert_eq!(parser.fingerprint(&pack).unwrap(), first);
        pack.grammar = "another_export".into();
        assert_ne!(parser.fingerprint(&pack).unwrap(), first);
    }

    #[test]
    fn a_replaced_grammar_can_recover_after_an_invalid_installation() {
        let dir = tempfile::tempdir().unwrap();
        let mut pack = pack();
        let wasm = std::fs::read(pack.dir.join("grammar.wasm")).unwrap();
        pack.dir = dir.path().to_owned();
        let grammar = pack.dir.join("grammar.wasm");
        std::fs::write(&grammar, &wasm).unwrap();
        let mut parser = OutlineParser::new().unwrap();
        let source = "fn example() {}";
        assert!(parser.extract(&pack, source, &|| false).is_ok());
        std::fs::write(&grammar, b"invalid wasm").unwrap();
        assert!(parser.extract(&pack, source, &|| false).is_err());
        std::fs::write(&grammar, &wasm).unwrap();
        assert_eq!(
            parser
                .extract(&pack, source, &|| false)
                .unwrap()
                .definitions[0]
                .name,
            "example"
        );
    }

    #[test]
    fn extracts_nested_definitions_without_retaining_bodies_and_obeys_cancellation() {
        let mut parser = OutlineParser::new().unwrap();
        let outline = parser
            .extract(&pack(), "fn outer(a) { fn inner(b) { b; } a; }", &|| false)
            .unwrap();
        assert_eq!(
            outline
                .definitions
                .iter()
                .map(|symbol| symbol.qualified.as_str())
                .collect::<Vec<_>>(),
            ["outer", "outer::inner"]
        );
        assert_eq!(outline.definitions[0].signature, "fn outer(a)");
        assert!(!outline.syntax_errors);
        assert!(parser.extract(&pack(), "fn x() {}", &|| true).is_err());
    }

    #[test]
    #[ignore = "qualification against installed language packs; set CLACKATOO_OUTLINE_LANGUAGES"]
    fn qualifies_installed_language_grammars() {
        let root = PathBuf::from(
            std::env::var_os("CLACKATOO_OUTLINE_LANGUAGES").expect("language pack directory"),
        );
        let cases = [
            (
                "rust",
                "pub struct Auth {} impl Auth { pub fn login(&self) -> bool { true } }",
                "Auth::login",
            ),
            (
                "python",
                "class Auth:\n    def login(self):\n        return True\n",
                "Auth::login",
            ),
            ("c", "int login(int user) { return user; }", "login"),
            (
                "javascript",
                "class Auth { login(user) { return user; } }",
                "Auth::login",
            ),
            ("lua", "function login(user) return user end", "login"),
        ];
        let mut checked = 0;
        for (language, source, expected) in cases {
            if !root.join(language).join("grammar.wasm").is_file() {
                continue;
            }
            let pack = Pack::read(&root.join(language)).unwrap();
            let result = OutlineParser::new()
                .unwrap()
                .extract(&pack, source, &|| false)
                .unwrap();
            assert!(
                result
                    .definitions
                    .iter()
                    .any(|definition| definition.qualified == expected),
                "{language}: {result:?}"
            );
            assert!(
                result
                    .definitions
                    .iter()
                    .all(|definition| !definition.signature.contains("return")),
                "{language}: {result:?}"
            );
            checked += 1;
        }
        assert!(
            checked >= 3,
            "at least three installed grammars required for qualification"
        );
    }
}
