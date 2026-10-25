//! Help candidates checked against the actual dialect and pending grammar.
//! Descriptions live here; applicability stays in the parser used by the pane.
use super::{Dialect, Keys, Mode, Parsed, Stroke};
use crate::shortcuts::{Context, notation};

pub(crate) fn context<D: Dialect>(name: &str, keys: &Keys, mode: Mode, dialect: &D) -> Context {
    let pending = keys.so_far();
    let visual = matches!(mode, Mode::Visual(_));
    let title = match keys.shown() {
        Some(shown) => format!("{name} · after {shown}"),
        None => format!("{name} · {} mode", if visual { "visual" } else { "normal" }),
    };
    let mut context = Context::new(
        title,
        &[("Esc / Ctrl+;", "Cancel the command or selection")],
    );
    if let Some(Stroke::Char {
        ch: inside @ ('i' | 'a'),
        ctrl: false,
    }) = pending.last()
        && accepts(pending, &to_strokes("w"), visual, dialect)
    {
        for &(label, sample, description) in CANDIDATES {
            if sample.starts_with(*inside) && sample.chars().count() == 2 {
                let suffix = &sample[1..];
                if accepts(pending, &to_strokes(suffix), visual, dialect) {
                    context.add(&[(&label[1..], description)]);
                }
            }
        }
        return context;
    }
    if let Some(Stroke::Char { ch, ctrl: false }) = pending.last()
        && let Some((label, sample, description)) = operand(*ch)
        && accepts(pending, &to_strokes(sample), visual, dialect)
    {
        context.add(&[(label, description)]);
        return context;
    }
    for &(label, sample, description) in CANDIDATES {
        let candidate = to_strokes(sample);
        let suffix = candidate
            .strip_prefix(pending)
            .filter(|suffix| !suffix.is_empty());
        // Prefer a known full command (after g, show d for gd). Otherwise
        // try it as a target (after d, show w for dw). Never accept a parser
        // result that finished before the last key: xg is not a command.
        let suffix = suffix.unwrap_or(&candidate);
        if accepts(pending, suffix, visual, dialect) {
            let shown = if suffix.len() < candidate.len() {
                suffix.iter().map(|stroke| stroke.shown()).collect()
            } else {
                label.to_owned()
            };
            if !context.rows.iter().any(|(key, _)| key == &shown) {
                context.rows.push((shown, description.to_owned()));
            }
        }
    }
    // Operators and objects can be composed; a representative complete
    // command checks that the dialect takes each operator before listing it.
    for (key, description) in [
        ("d", "Delete"),
        ("c", "Change"),
        ("y", "Copy"),
        (">", "Indent"),
        ("<", "Dedent"),
        ("=", "Reindent"),
        ("gu", "Lowercase"),
        ("gU", "Uppercase"),
        ("g~", "Swap case"),
        ("g?", "ROT13"),
    ] {
        let sample = if visual {
            key.to_owned()
        } else {
            format!("{key}w")
        };
        if accepts(pending, &to_strokes(&sample), visual, dialect) {
            context.rows.push((
                if visual {
                    key.to_owned()
                } else {
                    format!("{key} + motion or text object")
                },
                format!(
                    "{description} {}",
                    if visual {
                        "the selection"
                    } else {
                        "the target; repeat the operator for whole lines"
                    }
                ),
            ));
        }
    }
    if pending.is_empty() || keys.counting().is_some() {
        context.add(&[(
            "1–9, then digits",
            "Repeat the following command that many times",
        )]);
    }
    context
}

fn operand(ch: char) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match ch {
        'f' | 'F' | 't' | 'T' => ("Any character", "x", "Find this character on the line"),
        'r' => ("Any character", "x", "Replace with this character"),
        'm' => ("a–z / A–Z / ' / `", "a", "Set this mark"),
        '\'' | '`' => ("a–z / A–Z / ' / ` / . / < / >", "a", "Jump to this mark"),
        '"' => (
            "a–z / A–Z / 0–9 / \" / - / _ / + / *",
            "ayw",
            "Choose a register for the following command",
        ),
        'q' => ("a–z / A–Z / 0–9", "a", "Record a macro in this register"),
        '@' => (
            "Register / @",
            "a",
            "Replay a register; @ repeats the last macro",
        ),
        _ => return None,
    })
}

fn to_strokes(text: &str) -> Vec<Stroke> {
    crate::keymap::read_macro(&notation(text))
        .iter()
        .flat_map(|chord| super::strokes(&chord.kept().press(), false))
        .collect()
}

fn accepts<D: Dialect>(pending: &[Stroke], suffix: &[Stroke], visual: bool, dialect: &D) -> bool {
    let keys: Vec<_> = pending.iter().chain(suffix).copied().collect();
    for end in pending.len() + 1..keys.len() {
        if super::grammar::parse(&keys[..end], visual, dialect) != Parsed::Incomplete {
            return false;
        }
    }
    matches!(
        super::grammar::parse(&keys, visual, dialect),
        Parsed::Command(_)
    )
}

// A sample provides the parameter for commands with an arbitrary character.
const CANDIDATES: &[(&str, &str, &str)] = &[
    ("+", "+", "First nonblank on next line"),
    ("-", "-", "First nonblank on previous line"),
    ("_", "_", "First nonblank on counted line"),
    ("Enter", "Enter", "First nonblank on next line"),
    ("Backspace", "Backspace", "Previous character"),
    ("h", "h", "Character left"),
    ("j", "j", "Line down"),
    ("k", "k", "Line up"),
    ("l", "l", "Character right"),
    ("Left", "Left", "Character left"),
    ("Down", "Down", "Line down"),
    ("Up", "Up", "Line up"),
    ("Right", "Right", "Character right"),
    ("w", "w", "Next word"),
    ("b", "b", "Previous word"),
    ("e", "e", "End of word"),
    ("W", "W", "Next whitespace-delimited word"),
    ("B", "B", "Previous whitespace-delimited word"),
    ("E", "E", "End of whitespace-delimited word"),
    ("ge", "ge", "Previous word end"),
    ("gE", "gE", "Previous whitespace-delimited word end"),
    ("0", "0", "Start of line"),
    ("^", "^", "First nonblank character"),
    ("$", "$", "End of line"),
    ("g_", "g_", "Last nonblank character"),
    ("Home", "Home", "Start of line"),
    ("End", "End", "End of line"),
    ("|", "|", "Column given by count"),
    ("gg", "gg", "First line, or line given by count"),
    ("G", "G", "Last line, or line given by count"),
    ("H", "H", "Top of view"),
    ("M", "M", "Middle of view"),
    ("L", "L", "Bottom of view"),
    ("{", "{", "Previous paragraph"),
    ("}", "}", "Next paragraph"),
    ("(", "(", "Previous sentence"),
    (")", ")", "Next sentence"),
    ("%", "%", "Matching bracket"),
    ("gj", "gj", "Next screen line"),
    ("gk", "gk", "Previous screen line"),
    ("g0", "g0", "Start of screen line"),
    ("g^", "g^", "First nonblank on screen line"),
    ("g$", "g$", "End of screen line"),
    ("f + character", "fx", "Find character forward"),
    ("F + character", "Fx", "Find character backward"),
    ("t + character", "tx", "Before character forward"),
    ("T + character", "Tx", "After character backward"),
    (";", ";", "Repeat character find"),
    (",", ",", "Reverse character find"),
    ("/", "/", "Search forward"),
    ("?", "?", "Search backward"),
    ("n", "n", "Repeat search"),
    ("N", "N", "Reverse search"),
    ("*", "*", "Search word forward"),
    ("#", "#", "Search word backward"),
    ("Ctrl+D", "Ctrl+D", "Scroll half a page down"),
    ("Ctrl+U", "Ctrl+U", "Scroll half a page up"),
    ("Ctrl+E", "Ctrl+E", "Scroll a line down"),
    ("Ctrl+Y", "Ctrl+Y", "Scroll a line up"),
    ("Ctrl+F", "Ctrl+F", "Scroll a page down"),
    ("PageDown", "PageDown", "Scroll a page down"),
    ("PageUp", "PageUp", "Scroll a page up"),
    ("zt", "zt", "Cursor line at top"),
    ("zz", "zz", "Cursor line at centre"),
    ("zb", "zb", "Cursor line at bottom"),
    ("v", "v", "Select characters"),
    ("V", "V", "Select lines"),
    ("Ctrl+V", "Ctrl+V", "Select a block"),
    ("gv", "gv", "Restore last selection"),
    ("o", "o", "New line below, or other end of selection"),
    ("O", "O", "New line above, or other corner of block"),
    ("i", "i", "Insert before cursor"),
    ("a", "a", "Insert after cursor"),
    ("I", "I", "Insert at first nonblank"),
    ("A", "A", "Append at end of line"),
    ("x", "x", "Delete character or selection"),
    ("X", "X", "Delete previous character"),
    ("Delete", "Delete", "Delete character or selection"),
    ("D", "D", "Delete to end of line"),
    ("C", "C", "Change to end of line"),
    ("s", "s", "Substitute character or selection"),
    ("S", "S", "Change line"),
    ("Y", "Y", "Copy line"),
    ("yy", "yy", "Copy whole lines"),
    ("dd", "dd", "Delete whole lines"),
    ("cc", "cc", "Change whole lines"),
    ("p", "p", "Put after cursor"),
    ("P", "P", "Put before cursor"),
    ("J", "J", "Join lines"),
    ("r + character", "rx", "Replace character"),
    ("~", "~", "Swap case"),
    ("u", "u", "Undo, or lowercase selection"),
    ("U", "U", "Uppercase selection"),
    ("Ctrl+R", "Ctrl+R", "Redo"),
    (".", ".", "Repeat last change"),
    ("&", "&", "Repeat substitution on line"),
    ("g&", "g&", "Repeat substitution throughout file"),
    ("m + mark", "ma", "Set mark"),
    ("' + mark", "'a", "Jump to marked line"),
    ("` + mark", "`a", "Jump to exact mark"),
    ("q + register", "qa", "Record macro; q stops recording"),
    ("@ + register", "@a", "Replay macro"),
    ("@@", "@@", "Replay last macro"),
    ("gd", "gd", "Go to definition"),
    ("gD", "gD", "Go to declaration"),
    ("gy", "gy", "Go to type definition"),
    ("gri", "gri", "Find implementations"),
    ("grr", "grr", "Find references"),
    ("grn", "grn", "Rename symbol"),
    ("gra", "gra", "Show code actions"),
    ("gq", "gq", "Format selection"),
    ("K", "K", "Show hover information"),
    ("<C-w>d", "<C-w>d", "Show diagnostics at cursor"),
    ("]d", "]d", "Next diagnostic"),
    ("[d", "[d", "Previous diagnostic"),
    ("]c", "]c", "Next change hunk"),
    ("[c", "[c", "Previous change hunk"),
    ("]f", "]f", "Next function"),
    ("[f", "[f", "Previous function"),
    ("]C", "]C", "Next class"),
    ("[C", "[C", "Previous class"),
    ("gx", "gx", "Open link under cursor"),
    ("]]", "]]", "Next shell block"),
    ("[[", "[[", "Previous shell block"),
    ("iw", "iw", "Inner word"),
    ("aw", "aw", "Word including surrounding space"),
    ("iW", "iW", "Inner whitespace-delimited word"),
    ("aW", "aW", "Whitespace-delimited word including space"),
    ("is", "is", "Inner sentence"),
    ("as", "as", "Sentence including space"),
    ("ip", "ip", "Inner paragraph"),
    ("ap", "ap", "Paragraph including space"),
    ("i\"", "i\"", "Inside double quotes"),
    ("a\"", "a\"", "Including double quotes"),
    ("i'", "i'", "Inside single quotes"),
    ("a'", "a'", "Including single quotes"),
    ("i`", "i`", "Inside backticks"),
    ("a`", "a`", "Including backticks"),
    ("i(", "i(", "Inside parentheses"),
    ("a(", "a(", "Including parentheses"),
    ("i[", "i[", "Inside square brackets"),
    ("a[", "a[", "Including square brackets"),
    ("i{", "i{", "Inside braces"),
    ("a{", "a{", "Including braces"),
    ("ib", "ib", "Shell block output"),
    ("ab", "ab", "Whole shell block"),
    ("if", "if", "Function body"),
    ("af", "af", "Whole function"),
    ("ic", "ic", "Class body"),
    ("ac", "ac", "Whole class"),
    ("ia", "ia", "Argument"),
    ("aa", "aa", "Argument including comma"),
    ("i<", "i<", "Inside angle brackets"),
    ("a<", "a<", "Including angle brackets"),
];
