//! The corpus samples the last variant of every enum the protocol carries
//! (see `src/corpus.rs`), so a variant appended to any of them shows in
//! the release fixtures. Read from the source: every enum deriving serde's
//! `Serialize` in this crate, `clackatoo-term` and `clackatoo-text`.

use std::path::{Path, PathBuf};

/// Every Rust file under `dir`.
fn sources(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("the sources can be listed") {
        let path = entry.expect("the sources can be listed").path();
        if path.is_dir() {
            files.extend(sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

/// Each serde enum in `source`, and its last variant, as `Enum::Variant`.
fn last_variants(source: &str) -> Vec<String> {
    let source = source.split("#[cfg(test)]").next().unwrap_or_default();
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let Some(name) = line.strip_prefix("pub enum ") else {
            continue;
        };
        let name: String = name
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect();
        let before = &lines[at.saturating_sub(4)..at];
        if !before.iter().any(|line| line.contains("Serialize")) {
            continue;
        }
        let last = (lines[at + 1..].iter())
            .take_while(|line| **line != "}")
            .filter_map(|line| {
                let variant = line.strip_prefix("    ")?;
                let first = variant.chars().next()?;
                first.is_ascii_uppercase().then(|| {
                    variant
                        .chars()
                        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                        .collect::<String>()
                })
            })
            .last()
            .unwrap_or_else(|| panic!("{name} has no variants"));
        found.push(format!("{name}::{last}"));
    }
    found
}

#[test]
fn the_last_variant_of_every_enum_is_sampled() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let corpus = std::fs::read_to_string(crates.join("protocol/src/corpus.rs")).unwrap();
    // The samples, not the tests after them.
    let corpus = corpus.split("#[cfg(test)]").next().unwrap_or_default();
    let mut enums = 0;
    let mut missing = Vec::new();
    for dir in ["protocol/src", "term/src", "text/src"] {
        for file in sources(&crates.join(dir)) {
            for last in last_variants(&std::fs::read_to_string(&file).unwrap()) {
                enums += 1;
                if !corpus.contains(last.as_str()) {
                    missing.push(last);
                }
            }
        }
    }
    assert!(
        enums > 50,
        "only {enums} enums found: has the source moved?"
    );
    assert!(
        missing.is_empty(),
        "the corpus samples none of {missing:?}: add samples that reach them"
    );
}

#[test]
fn last_variants_are_read_as_written() {
    let source = "#[derive(Serialize)]\npub enum Shape {\n    /// Round.\n    Round,\n    \
                  Square {\n        side: u32,\n    },\n}\n#[derive(Clone)]\npub enum Plain {\n    \
                  A,\n}\n";
    assert_eq!(last_variants(source), ["Shape::Square"]);
}
