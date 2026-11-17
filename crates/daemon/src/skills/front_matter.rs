//! The front matter a `SKILL.md` or a `SPECIALIST.md` starts with: YAML
//! between two lines of `---`, before the Markdown the file is for. Both
//! kinds of file are split here, so both take the same files: one saved
//! with a byte-order mark, with Windows line ends, with spaces after a
//! `---`, or that ends at its closing `---` with no line break.

/// Why a file's front matter couldn't be told from its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unsplit {
    /// Its first line isn't `---`.
    Missing,
    /// No later line is `---`.
    Unclosed,
}

impl std::fmt::Display for Unsplit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Missing => "it doesn't start with a line of ---, for its YAML front matter",
            Self::Unclosed => "no line of --- ends its YAML front matter",
        })
    }
}

/// The YAML between `text`'s two `---` lines, and everything after them.
pub(crate) fn split(text: &str) -> Result<(&str, &str), Unsplit> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().filter(|line| delimits(line));
    let start = first.ok_or(Unsplit::Missing)?.len();
    let mut end = start;
    for line in lines {
        if delimits(line) {
            return Ok((&text[start..end], &text[end + line.len()..]));
        }
        end += line.len();
    }
    Err(Unsplit::Unclosed)
}

/// Whether `line` is a `---`, whatever space or line end follows it.
fn delimits(line: &str) -> bool {
    line.trim_end() == "---"
}

/// Whether `body` asks for a command's output to be put in its place, as
/// Claude Code's skills can, with a line starting `` !` ``. Shared skills
/// run nothing, so one that does can't be offered. The same characters
/// in a code sample, or in the middle of a line, are only text.
pub(crate) fn runs_commands(body: &str) -> bool {
    let mut fenced = false;
    for line in body.lines() {
        let fence = line.trim_start();
        if fence.starts_with("```") || fence.starts_with("~~~") {
            fenced = !fenced;
        } else if !fenced && line.starts_with("!`") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_matter_is_split_whatever_saved_the_file() {
        assert_eq!(split("---\na: 1\n---\nbody\n"), Ok(("a: 1\n", "body\n")));
        // A byte-order mark, Windows line ends and space after a `---`.
        assert_eq!(
            split("\u{feff}---  \r\na: 1\r\n--- \r\nbody"),
            Ok(("a: 1\r\n", "body"))
        );
        // It ends at its closing line, with no line break.
        assert_eq!(split("---\na: 1\n---"), Ok(("a: 1\n", "")));
        assert_eq!(split("---\n---\n"), Ok(("", "")));
        // A `---` further into a line, or in the body, isn't one.
        assert_eq!(
            split("---\na: b---\n---\none\n---\ntwo"),
            Ok(("a: b---\n", "one\n---\ntwo"))
        );
        assert_eq!(split(""), Err(Unsplit::Missing));
        assert_eq!(split("body\n---\n"), Err(Unsplit::Missing));
        assert_eq!(split("---\na: 1\nbody"), Err(Unsplit::Unclosed));
    }

    #[test]
    fn only_a_line_starting_with_a_command_outside_code_runs_one() {
        assert!(runs_commands("Status:\n!`git status`\n"));
        assert!(!runs_commands("Write !`cmd` to run one.\n"));
        assert!(!runs_commands("```\n!`git status`\n```\n"));
        assert!(!runs_commands("~~~sh\n!`git status`\n~~~\n"));
        assert!(runs_commands("```\ncode\n```\n!`git status`\n"));
    }
}
