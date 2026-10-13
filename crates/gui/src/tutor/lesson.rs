//! A lesson: Markdown, with fenced blocks that say what to set up and how
//! to tell a step is done.
//!
//! ````markdown
//! # Text objects
//!
//! The first paragraph is the lesson's summary, shown in the picker.
//!
//! ```practice shapes.rs
//! fn first() {}
//! ```
//!
//! ## Delete a function with `daf`
//!
//! Put the cursor in `first` and type `daf`.
//!
//! ```check
//! not has "fn first"
//! ```
//! ````
//!
//! `# ` names the lesson, and each `## ` starts a step. Before the first
//! step come the lesson's own blocks; in a step, the step's.
//!
//! | Block | Holds |
//! |---|---|
//! | `practice NAME` | A file for the scratch directory, named NAME. The lesson's first is the practice file, which the practice pane edits. In a step, the practice file's text from the step's start. |
//! | `setup` | What to do first, a line each (see [`Setup`]). |
//! | `check` | What must hold for the step to be done, a line each, all of them (see [`Check`]). |
//! | `expect` | The practice file's text, exactly, for the step to be done. |
//!
//! Any other fenced block is shown, as code. Paragraphs are joined into
//! one line each, for the editor showing them to wrap, and `- ` starts an
//! item of a list.

use super::check::{Check, words};

/// A lesson, parsed.
#[derive(Debug)]
pub(crate) struct Lesson {
    /// Its name in `:Tutor NAME`.
    pub(crate) id: &'static str,
    pub(crate) title: String,
    /// What it teaches, in a sentence or two.
    pub(crate) summary: String,
    /// What comes before the first step.
    intro: Vec<Piece>,
    /// The files for the scratch directory, by name; the first is the
    /// practice file, unless the practice pane is a terminal.
    pub(crate) files: Vec<(String, String)>,
    /// What to do as the lesson opens.
    pub(crate) setup: Vec<Setup>,
    pub(crate) steps: Vec<Step>,
}

/// One step of a lesson.
#[derive(Debug)]
pub(crate) struct Step {
    pub(crate) title: String,
    body: Vec<Piece>,
    /// The practice file's text from the step's start, if the step sets
    /// it afresh.
    pub(crate) practice: Option<String>,
    /// What to do as the step starts.
    pub(crate) setup: Vec<Setup>,
    /// What must hold for it to be done. A step with none is read, and
    /// `]s` goes on from it.
    pub(crate) checks: Vec<Check>,
}

/// Something done as a lesson or a step starts, written in a `setup`
/// block a line each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Setup {
    /// `terminal`: the practice pane is a terminal, started in the scratch
    /// directory, rather than an editor. Only for the lesson.
    Terminal,
    /// `run "COMMAND"`: types the command into the practice terminal, and
    /// Enter.
    Run(String),
    /// `explorer`: opens a file explorer on the scratch directory, down the
    /// tab's left, unless the tab has one.
    Explorer,
    /// `cursor LINE`: puts the practice editor's cursor at the start of
    /// line LINE, from 1.
    Cursor(u64),
}

/// A paragraph, an item of a list, or a block of code, as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Paragraph(String),
    Item(String),
    Code(Vec<String>),
}

/// A lesson laid out as text for the lesson pane, with where its parts
/// are, by line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rendered {
    pub(crate) text: String,
    /// Each step's lines: its heading's, then its body's.
    pub(crate) steps: Vec<std::ops::Range<usize>>,
    /// The lines that end the lesson.
    pub(crate) end: std::ops::Range<usize>,
}

/// How a step's body is indented under its heading.
const INDENT: &str = "   ";

impl Lesson {
    /// Parses the lesson `id`, written as the module says.
    pub(crate) fn parse(id: &'static str, source: &str) -> Result<Self, String> {
        let mut lesson = Lesson {
            id,
            title: String::new(),
            summary: String::new(),
            intro: Vec::new(),
            files: Vec::new(),
            setup: Vec::new(),
            steps: Vec::new(),
        };
        let mut lines = source.lines().enumerate().peekable();
        let mut paragraph: Option<(bool, String)> = None;
        let flush = |lesson: &mut Lesson, paragraph: &mut Option<(bool, String)>| {
            if let Some((item, text)) = paragraph.take() {
                let piece = if item {
                    Piece::Item(text)
                } else {
                    Piece::Paragraph(text)
                };
                lesson.pieces().push(piece);
            }
        };
        while let Some((number, line)) = lines.next() {
            let at = |why: String| format!("{id}.md line {}: {why}", number + 1);
            if let Some(info) = line.strip_prefix("```") {
                flush(&mut lesson, &mut paragraph);
                let mut body = Vec::new();
                loop {
                    match lines.next() {
                        Some((_, "```")) => break,
                        Some((_, line)) => body.push(line),
                        None => return Err(at("a block that isn't closed".into())),
                    }
                }
                lesson.block(info.trim(), &body).map_err(at)?;
            } else if let Some(title) = line.strip_prefix("# ") {
                if !lesson.title.is_empty() {
                    return Err(at("a second title".into()));
                }
                title.trim().clone_into(&mut lesson.title);
            } else if let Some(title) = line.strip_prefix("## ") {
                flush(&mut lesson, &mut paragraph);
                lesson.steps.push(Step {
                    title: title.trim().to_owned(),
                    body: Vec::new(),
                    practice: None,
                    setup: Vec::new(),
                    checks: Vec::new(),
                });
            } else if line.trim().is_empty() {
                flush(&mut lesson, &mut paragraph);
            } else if let Some(item) = line.strip_prefix("- ") {
                flush(&mut lesson, &mut paragraph);
                paragraph = Some((true, item.trim().to_owned()));
            } else {
                match &mut paragraph {
                    Some((_, text)) => {
                        text.push(' ');
                        text.push_str(line.trim());
                    }
                    None => paragraph = Some((false, line.trim().to_owned())),
                }
            }
        }
        flush(&mut lesson, &mut paragraph);
        lesson.finish()
    }

    /// Where pieces go now: the last step's body, or the intro.
    fn pieces(&mut self) -> &mut Vec<Piece> {
        match self.steps.last_mut() {
            Some(step) => &mut step.body,
            None => &mut self.intro,
        }
    }

    /// Takes in a fenced block, whose info string is `info`.
    fn block(&mut self, info: &str, body: &[&str]) -> Result<(), String> {
        let (kind, rest) = info.split_once(' ').unwrap_or((info, ""));
        let text = || {
            let mut text = body.join("\n");
            text.push('\n');
            text
        };
        let lines = || body.iter().filter(|line| !line.trim().is_empty());
        let in_step = !self.steps.is_empty();
        match kind {
            "practice" => {
                let name = rest.trim();
                if !in_step {
                    if name.is_empty() || name.contains('/') || name.starts_with('.') {
                        return Err(format!("a practice file needs a plain name, not `{name}`"));
                    }
                    self.files.push((name.to_owned(), text()));
                } else if self.files.first().is_some_and(|(first, _)| first == name) {
                    self.step()?.practice = Some(text());
                } else {
                    return Err(format!(
                        "a step's practice block is for the practice file, not `{name}`"
                    ));
                }
            }
            "setup" => {
                for line in lines() {
                    let setup = Setup::parse(line)?;
                    if !in_step {
                        self.setup.push(setup);
                    } else if setup == Setup::Terminal {
                        return Err("`terminal` is for the lesson, not a step".into());
                    } else {
                        self.step()?.setup.push(setup);
                    }
                }
            }
            "check" => {
                for line in lines() {
                    let check = Check::parse(line)?;
                    self.step()?.checks.push(check);
                }
            }
            "expect" => self.step()?.checks.push(Check::Text(text())),
            _ => self.pieces().push(Piece::Code(
                body.iter().map(|&line| line.to_owned()).collect(),
            )),
        }
        Ok(())
    }

    /// The step being read, for a block that must be in one.
    fn step(&mut self) -> Result<&mut Step, String> {
        (self.steps.last_mut()).ok_or_else(|| "checks are for a step, after its `## `".into())
    }

    /// Checks the lesson is whole, and takes its summary from the intro.
    fn finish(mut self) -> Result<Self, String> {
        let id = self.id;
        if self.title.is_empty() {
            return Err(format!("{id}.md has no title"));
        }
        if self.steps.is_empty() {
            return Err(format!("{id}.md has no steps"));
        }
        self.summary = match self.intro.first() {
            Some(Piece::Paragraph(text)) => text.clone(),
            _ => return Err(format!("{id}.md starts with no summary")),
        };
        if !self.terminal() && self.files.is_empty() {
            return Err(format!(
                "{id}.md has no practice file, and its practice pane isn't a terminal"
            ));
        }
        for step in &self.steps {
            let needs_terminal = (step.setup.iter()).any(|setup| matches!(setup, Setup::Run(_)));
            if needs_terminal && !self.terminal() {
                return Err(format!(
                    "{id}.md: `run` in \"{}\", with no terminal to run in",
                    step.title
                ));
            }
        }
        Ok(self)
    }

    /// Whether the practice pane is a terminal.
    pub(crate) fn terminal(&self) -> bool {
        self.setup.contains(&Setup::Terminal)
    }

    /// The practice file's name and text, unless the practice pane is a
    /// terminal.
    pub(crate) fn practice_file(&self) -> Option<&(String, String)> {
        (!self.terminal()).then(|| self.files.first()).flatten()
    }

    /// The practice file's text at the start of step `step`: the nearest
    /// step's at or before it that sets it, or else the lesson's.
    pub(crate) fn practice_at(&self, step: usize) -> Option<&str> {
        let set =
            (self.steps.iter().take(step + 1).rev()).find_map(|step| step.practice.as_deref());
        set.or_else(|| self.practice_file().map(|(_, text)| text.as_str()))
    }

    /// The lesson as text for its pane, with where each step is.
    pub(crate) fn render(&self) -> Rendered {
        let mut lines: Vec<String> = Vec::new();
        let title = self.title.to_uppercase();
        lines.push(title.clone());
        lines.push("═".repeat(title.chars().count()));
        lines.push(String::new());
        push_pieces(&mut lines, &self.intro, "");
        lines.push(
            "Practise in the pane beside this one: each step ticks itself off once \
             it's done. ]s skips a step, [s goes back, and :q ends the lesson."
                .into(),
        );
        lines.push(String::new());
        let mut steps = Vec::new();
        for (number, step) in self.steps.iter().enumerate() {
            let start = lines.len();
            lines.push(format!("{}. {}", number + 1, step.title));
            if step.checks.is_empty() {
                lines.push(format!("{INDENT}(Read this one, then ]s goes on.)"));
            }
            if !step.body.is_empty() {
                lines.push(String::new());
            }
            push_pieces(&mut lines, &step.body, INDENT);
            while lines.last().is_some_and(String::is_empty) {
                lines.pop();
            }
            steps.push(start..lines.len());
            lines.push(String::new());
        }
        let start = lines.len();
        lines.push("That's the end of the lesson. :q closes it, and :Tutor picks another.".into());
        let end = start..lines.len();
        let mut text = lines.join("\n");
        text.push('\n');
        Rendered { text, steps, end }
    }
}

/// Adds `pieces` to `lines`, each line indented by `indent`, with a blank
/// line after each paragraph and block, and after a list.
fn push_pieces(lines: &mut Vec<String>, pieces: &[Piece], indent: &str) {
    for (index, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Paragraph(text) => lines.push(format!("{indent}{text}")),
            Piece::Item(text) => lines.push(format!("{indent}• {text}")),
            Piece::Code(code) => {
                lines.extend(code.iter().map(|line| format!("{indent}    {line}")));
            }
        }
        let list_goes_on = matches!(piece, Piece::Item(_))
            && matches!(pieces.get(index + 1), Some(Piece::Item(_)));
        if !list_goes_on {
            lines.push(String::new());
        }
    }
}

impl Setup {
    fn parse(line: &str) -> Result<Self, String> {
        let words = words(line)?;
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["terminal"] => Ok(Self::Terminal),
            ["run", command] => Ok(Self::Run((*command).to_owned())),
            ["explorer"] => Ok(Self::Explorer),
            ["cursor", line] => match line.parse() {
                Ok(line) if line > 0 => Ok(Self::Cursor(line)),
                _ => Err(format!("`cursor` takes a line, from 1, not `{line}`")),
            },
            _ => Err(format!(
                "a setup line that means nothing: `{}`",
                line.trim()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# Sample

What it teaches,
over two lines.

```practice a.txt
one
two
```

## First `x`

Do this.

- one thing
- another

```check
has "x"
```

## Second

```practice a.txt
fresh
```

```setup
cursor 2
```

```expect
done
```

## Just read
"#;

    #[test]
    fn a_lesson_is_parsed_into_its_files_steps_and_checks() {
        let lesson = Lesson::parse("sample", SAMPLE).unwrap();
        assert_eq!(lesson.title, "Sample");
        assert_eq!(lesson.summary, "What it teaches, over two lines.");
        assert_eq!(lesson.files, [("a.txt".into(), "one\ntwo\n".into())]);
        assert_eq!(lesson.steps.len(), 3);
        assert_eq!(lesson.steps[0].checks, [Check::Has("x".into())]);
        assert_eq!(lesson.steps[1].practice.as_deref(), Some("fresh\n"));
        assert_eq!(lesson.steps[1].setup, [Setup::Cursor(2)]);
        assert_eq!(lesson.steps[1].checks, [Check::Text("done\n".into())]);
        assert!(lesson.steps[2].checks.is_empty());
        assert_eq!(lesson.practice_at(0), Some("one\ntwo\n"));
        assert_eq!(lesson.practice_at(2), Some("fresh\n"));
    }

    #[test]
    fn a_lesson_is_laid_out_as_text_with_where_its_steps_are() {
        let rendered = Lesson::parse("sample", SAMPLE).unwrap().render();
        let lines: Vec<&str> = rendered.text.lines().collect();
        assert_eq!(lines[0], "SAMPLE");
        assert_eq!(lines[1], "══════");
        assert_eq!(lines[3], "What it teaches, over two lines.");
        let first = rendered.steps[0].clone();
        assert_eq!(lines[first.start], "1. First `x`");
        assert_eq!(
            &lines[first.start + 1..first.end],
            ["", "   Do this.", "", "   • one thing", "   • another"]
        );
        assert_eq!(lines[rendered.steps[2].start], "3. Just read");
        assert_eq!(
            lines[rendered.steps[2].start + 1],
            "   (Read this one, then ]s goes on.)"
        );
        assert!(lines[rendered.end.start].starts_with("That's the end"));
    }

    #[test]
    fn mistakes_say_where_they_are() {
        let unclosed = "# T\n\nS.\n\n## A\n\n```check\nhas \"x\"\n";
        assert!(Lesson::parse("t", unclosed).unwrap_err().contains("line 7"));
        let unknown = "# T\n\nS.\n\n```practice a\nx\n```\n\n## A\n\n```check\nsmells \"x\"\n```\n";
        assert!(Lesson::parse("t", unknown).unwrap_err().contains("smells"));
        let no_file = "# T\n\nS.\n\n## A\n";
        assert!(
            Lesson::parse("t", no_file)
                .unwrap_err()
                .contains("no practice file")
        );
        let run_without =
            "# T\n\nS.\n\n```practice a\nx\n```\n\n## A\n\n```setup\nrun \"ls\"\n```\n";
        assert!(
            Lesson::parse("t", run_without)
                .unwrap_err()
                .contains("no terminal")
        );
    }
}
