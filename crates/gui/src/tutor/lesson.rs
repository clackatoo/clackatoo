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
//! item of a list. In titles, paragraphs and items, `` `code` `` and
//! `**strong**` are drawn as such, as the chat draws them, without their
//! backticks and stars (see [`Rendered::marks`]); the rest is shown as
//! written.

use std::ops::Range;

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
    /// Its title, as shown, without its Markdown.
    pub(crate) title: String,
    /// How the title's stretches are drawn.
    title_marks: Vec<(Range<usize>, Look)>,
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
    Paragraph(Inline),
    Item(Inline),
    Code(Vec<String>),
}

/// How a stretch of a lesson is drawn: as code, strongly, emphasised, or
/// as more than one, as code in a step's title is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Look {
    pub(crate) code: bool,
    pub(crate) strong: bool,
    /// Emphasised, as `*this*` is: in italics.
    pub(crate) italic: bool,
}

/// Text as a lesson shows it: a title, a paragraph or an item of a list,
/// with its Markdown read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Inline {
    text: String,
    /// The stretches of it drawn otherwise than plainly, by byte, in
    /// order, none overlapping.
    marks: Vec<(Range<usize>, Look)>,
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
    /// The stretches of the text drawn otherwise than plainly, by byte of
    /// it, in order, none overlapping: its code, its titles, and what it
    /// says strongly.
    pub(crate) marks: Vec<(Range<usize>, Look)>,
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
                let text = Inline::parse(&text, false);
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
                let title = Inline::parse(title.trim(), true);
                lesson.steps.push(Step {
                    title: title.text,
                    title_marks: title.marks,
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
            Some(Piece::Paragraph(text)) => text.text.clone(),
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
        let mut lines = Lines::default();
        let title = self.title.to_uppercase();
        let strong = Look {
            code: false,
            strong: true,
            ..Look::default()
        };
        lines.push(String::new(), &Inline::marked(&title, strong));
        lines.push(
            String::new(),
            &Inline::marked(&"═".repeat(title.chars().count()), strong),
        );
        lines.blank();
        lines.pieces(&self.intro, "");
        let practise = Inline::parse(
            "Practise in the pane beside this one: each step ticks itself off once it's \
             done. `]s` skips a step, `[s` goes back, and `:q` ends the lesson.",
            false,
        );
        lines.push(String::new(), &practise);
        lines.blank();
        let read = Inline::parse("(Read this one, then `]s` goes on.)", false);
        let mut steps = Vec::new();
        for (number, step) in self.steps.iter().enumerate() {
            let start = lines.text.len();
            let mut heading = Inline::marked(&format!("{}. ", number + 1), strong);
            heading.append(&Inline {
                text: step.title.clone(),
                marks: step.title_marks.clone(),
            });
            lines.push(String::new(), &heading);
            if step.checks.is_empty() {
                lines.push(INDENT.to_owned(), &read);
            }
            if !step.body.is_empty() {
                lines.blank();
            }
            lines.pieces(&step.body, INDENT);
            lines.trim();
            steps.push(start..lines.text.len());
            lines.blank();
        }
        let start = lines.text.len();
        let end_text = Inline::parse(
            "That's the end of the lesson. `:q` closes it, and `:Tutor` picks another.",
            false,
        );
        lines.push(String::new(), &end_text);
        let end = start..lines.text.len();
        let (text, marks) = lines.join();
        Rendered {
            text,
            steps,
            end,
            marks,
        }
    }
}

/// A lesson's lines as they're laid out, with how their stretches are
/// drawn.
#[derive(Default)]
struct Lines {
    text: Vec<String>,
    /// Each stretch's line, bytes in it, and look.
    marks: Vec<(usize, Range<usize>, Look)>,
}

impl Lines {
    /// Adds a line: `lead`, then `inline`.
    fn push(&mut self, lead: String, inline: &Inline) {
        let line = self.text.len();
        let at = lead.len();
        (self.marks).extend(
            (inline.marks.iter())
                .map(|(bytes, look)| (line, bytes.start + at..bytes.end + at, *look)),
        );
        self.text.push(lead + &inline.text);
    }

    /// Adds a blank line.
    fn blank(&mut self) {
        self.text.push(String::new());
    }

    /// Takes off the blank lines at the end.
    fn trim(&mut self) {
        while self.text.last().is_some_and(String::is_empty) {
            self.text.pop();
        }
    }

    /// Adds `pieces`, each line indented by `indent`, with a blank line
    /// after each paragraph and block, and after a list. A block's lines
    /// are code.
    fn pieces(&mut self, pieces: &[Piece], indent: &str) {
        let code = Look {
            code: true,
            strong: false,
            ..Look::default()
        };
        for (index, piece) in pieces.iter().enumerate() {
            match piece {
                Piece::Paragraph(text) => self.push(indent.to_owned(), text),
                Piece::Item(text) => self.push(format!("{indent}• "), text),
                Piece::Code(lines) => {
                    for line in lines {
                        self.push(format!("{indent}    "), &Inline::marked(line, code));
                    }
                }
            }
            let list_goes_on = matches!(piece, Piece::Item(_))
                && matches!(pieces.get(index + 1), Some(Piece::Item(_)));
            if !list_goes_on {
                self.blank();
            }
        }
    }

    /// The lines as one text, each ending with a line break, and the
    /// stretches by byte of it.
    fn join(self) -> (String, Vec<(Range<usize>, Look)>) {
        let mut starts = Vec::with_capacity(self.text.len());
        let mut text = String::new();
        for line in &self.text {
            starts.push(text.len());
            text.push_str(line);
            text.push('\n');
        }
        let marks = (self.marks.into_iter())
            .map(|(line, bytes, look)| {
                let at = starts[line];
                (bytes.start + at..bytes.end + at, look)
            })
            .collect();
        (text, marks)
    }
}

impl Inline {
    /// `text`, all of it drawn as `look` says.
    fn marked(text: &str, look: Look) -> Self {
        let mut inline = Self::default();
        inline.add(text, look);
        inline
    }

    /// `source`, a title or a paragraph's text, read as Markdown, as the
    /// chat reads it: code and what's strong marked, all of it strong if
    /// `strong`, as a title is. What else Markdown makes of it, such as
    /// `<Space>` read as HTML, is shown as written; one that's a block of
    /// its own, such as a list, is shown whole as written, unmarked.
    fn parse(source: &str, strong: bool) -> Self {
        use pulldown_cmark::{Event, Parser, Tag, TagEnd};
        let mut inline = Self::default();
        let (mut strongly, mut emphasised) = (usize::from(strong), 0_usize);
        for event in Parser::new(source) {
            let look = |code| Look {
                code,
                strong: strongly > 0,
                italic: emphasised > 0,
            };
            match event {
                Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => {}
                Event::Start(Tag::Strong) => strongly += 1,
                Event::End(TagEnd::Strong) => strongly = strongly.saturating_sub(1),
                Event::Start(Tag::Emphasis) => emphasised += 1,
                Event::End(TagEnd::Emphasis) => emphasised = emphasised.saturating_sub(1),
                Event::Text(text) | Event::InlineHtml(text) | Event::Html(text) => {
                    inline.add(&text, look(false));
                }
                Event::Code(code) => inline.add(&code, look(true)),
                Event::SoftBreak | Event::HardBreak => inline.add(" ", look(false)),
                _ => {
                    let look = Look {
                        strong,
                        ..Look::default()
                    };
                    return Self::marked(source, look);
                }
            }
        }
        inline
    }

    /// Adds `text`, drawn as `look` says.
    fn add(&mut self, text: &str, look: Look) {
        let start = self.text.len();
        self.text.push_str(text);
        if look == Look::default() || text.is_empty() {
            return;
        }
        let end = self.text.len();
        match self.marks.last_mut() {
            // Joined to the stretch before, drawn the same.
            Some((bytes, was)) if *was == look && bytes.end == start => bytes.end = end,
            _ => self.marks.push((start..end, look)),
        }
    }

    /// Adds `other` after it.
    fn append(&mut self, other: &Inline) {
        let at = self.text.len();
        for (bytes, look) in &other.marks {
            let start = at + bytes.start;
            match self.marks.last_mut() {
                Some((was_bytes, was)) if *was == *look && was_bytes.end == start => {
                    was_bytes.end = at + bytes.end;
                }
                _ => self.marks.push((start..at + bytes.end, *look)),
            }
        }
        self.text.push_str(&other.text);
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
        assert_eq!(lines[first.start], "1. First x");
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

    /// The text of each stretch of `rendered` drawn otherwise than
    /// plainly, and how.
    fn marked(rendered: &Rendered) -> Vec<(&str, Look)> {
        (rendered.marks.iter())
            .map(|(bytes, look)| (&rendered.text[bytes.clone()], *look))
            .collect()
    }

    #[test]
    fn code_and_strong_are_drawn_as_such_without_their_markdown() {
        let source = "# Marked\n\nUse `:Tutor new` and `` `a ``, **strongly**, or <Space>e.\n\n\
                      ```practice a.txt\nx\n```\n\n## Delete with `daf`\n\n\
                      - **all** of `it`\n\n```\nfn a() {}\n```\n";
        let lesson = Lesson::parse("marked", source).unwrap();
        assert_eq!(
            lesson.steps[0].title, "Delete with daf",
            "as the finder shows it"
        );
        assert_eq!(
            lesson.summary,
            "Use :Tutor new and `a, strongly, or <Space>e."
        );
        let rendered = lesson.render();
        let code = Look {
            code: true,
            strong: false,
            ..Look::default()
        };
        let strong = Look {
            code: false,
            strong: true,
            ..Look::default()
        };
        let both = Look {
            code: true,
            strong: true,
            ..Look::default()
        };
        let lines: Vec<&str> = rendered.text.lines().collect();
        assert_eq!(lines[3], "Use :Tutor new and `a, strongly, or <Space>e.");
        let marks = marked(&rendered);
        assert_eq!(
            marks[..6],
            [
                ("MARKED", strong),
                ("══════", strong),
                (":Tutor new", code),
                ("`a", code),
                ("strongly", strong),
                ("]s", code),
            ],
            "the title and its rule, then the summary's"
        );
        // A step's title is strong, its code too; its body as written.
        let at = rendered.steps[0].start;
        assert_eq!(lines[at], "1. Delete with daf");
        let title = rendered
            .text
            .lines()
            .take(at)
            .map(|line| line.len() + 1)
            .sum::<usize>();
        let from = (rendered.marks.iter())
            .position(|(bytes, _)| bytes.start == title)
            .unwrap();
        assert_eq!(
            marks[from..from + 6],
            [
                ("1. Delete with ", strong),
                ("daf", both),
                ("]s", code),
                ("all", strong),
                ("it", code),
                ("fn a() {}", code),
            ]
        );
        assert_eq!(lines[at + 3], "   • all of it");
        // None overlap, and each is in order.
        for pair in rendered.marks.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{pair:?}");
        }
    }

    #[test]
    fn a_paragraph_markdown_makes_more_of_is_shown_as_written() {
        let inline = Inline::parse("1. not a list", false);
        assert_eq!(inline.text, "1. not a list");
        assert!(inline.marks.is_empty());
        let heading = Inline::parse("# not a heading `x`", true);
        assert_eq!(heading.text, "# not a heading `x`");
    }

    #[test]
    fn emphasis_is_italic_and_leaves_the_rest_read() {
        let inline = Inline::parse("an *italic* word and `code`", false);
        assert_eq!(inline.text, "an italic word and code");
        let italic = Look {
            italic: true,
            ..Look::default()
        };
        let code = Look {
            code: true,
            ..Look::default()
        };
        assert_eq!(inline.marks, [(3..9, italic), (19..23, code)]);
    }

    #[test]
    fn every_lessons_markdown_is_read_losing_nothing_but_its_marks() {
        // Each paragraph and item, read, says what its source does but for
        // backticks, stars and spaces, which code and strong take: nothing
        // escaped, made an entity of or taken for HTML is lost.
        let kept = |text: &str| -> String {
            (text.chars())
                .filter(|ch| !matches!(ch, '`' | '*') && !ch.is_whitespace())
                .collect()
        };
        for (id, source) in super::super::SOURCES {
            let lesson = Lesson::parse(id, source).unwrap();
            let pieces =
                (lesson.intro.iter()).chain(lesson.steps.iter().flat_map(|step| &step.body));
            let mut texts: Vec<&Inline> = Vec::new();
            for piece in pieces {
                if let Piece::Paragraph(text) | Piece::Item(text) = piece {
                    texts.push(text);
                }
            }
            let whole = kept(source);
            for text in texts {
                assert!(whole.contains(&kept(&text.text)), "{id}: {:?}", text.text);
                // A paragraph shown as written, as one Markdown makes a
                // block of is, keeps its backticks and stars outside code.
                let in_code = |at: usize| {
                    (text.marks.iter()).any(|(bytes, look)| look.code && bytes.contains(&at))
                };
                let left = (text.text.char_indices())
                    .find(|&(at, ch)| matches!(ch, '`' | '*') && !in_code(at));
                assert!(
                    left.is_none(),
                    "{id}: {:?} has Markdown left in it",
                    text.text
                );
            }
            for step in &lesson.steps {
                assert!(whole.contains(&kept(&step.title)), "{id}: {:?}", step.title);
                assert!(
                    !step.title.contains('`'),
                    "{id}: the title {:?} has a backtick",
                    step.title
                );
            }
        }
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
