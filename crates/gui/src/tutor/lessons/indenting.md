# Indenting

Indenting by hand with `>` and `<`, and as the language says, with `=` and as you type new lines.

The practice file is Rust. Indenting as the language says needs Rust's grammar and its indent rules: if the grammar isn't installed, the bottom bar asks to install it as the file opens; press `y`.

```practice tidy.rs
fn main() {
let total = 1;
if total > 0 {
println!("yes");
}
}
```

## Indent a line with >>

`>>` indents the cursor's line by `shiftwidth` columns, and `<<` takes as much off; a count does that many lines. A file keeps the kind of indent it has: tabs or spaces, as the lines near it are indented, or spaces where none is yet, as `expandtab` says.

```setup
cursor 2
```

On `let total`, press `>>`.

```check
has "\n    let total = 1;"
```

## Indent with a motion

`>` and `<` take a motion or an object, as `d` does: `>j` indents this line and the next, `>i{` what's inside the braces. In visual mode, `>` indents the lines selected and keeps them selected, so it can be pressed again.

```practice tidy.rs
one
two
three
```

```setup
cursor 1
```

Select the first two lines with `Vj`, and press `>`.

```expect
    one
    two
three
```

## Indent as the language says with =

`=` and a motion indents the lines it moves over as the language says, in one undo step: `==` the cursor's line, `=ip` a paragraph, `=af` a function. Blank lines are emptied, and lines the rules say to keep, such as those in a string, keep their indent.

```practice tidy.rs
fn main() {
let total = 1;
if total > 0 {
println!("yes");
}
}
```

Indent the whole file with `gg=G`. If the bar says Rust has no rules for indenting, its pack was installed before they came with it: `clackatoo language install rust`, in a terminal, gets them.

```expect
fn main() {
    let total = 1;
    if total > 0 {
        println!("yes");
    }
}
```

## New lines

Enter in insert mode, and `o` and `O`, start a new line indented as the language says. It takes the indent of the line before at once, so typing never waits, and is put right a moment later. A line where `}` is typed first is worked out again.

```practice tidy.rs
fn main() {
    let total = 1;
    if total > 0 {
        println!("yes");
    }
}
```

```setup
cursor 3
```

On the `if` line, press `o`, type `let more = 2;`, and press Esc.

```check
has "\n        let more = 2;"
```

## Tabs

A tab is drawn to the next column that's a multiple of `tabstop`, 4 to begin with; `shiftwidth` is `tabstop`'s while it's 0. `:set ts=8 sw=4` draws tabs 8 wide and indents by 4, and `:set noet` indents new lines with tabs. `:set nosi` turns indenting as the language says off for new lines; `=` goes by the rules either way.
