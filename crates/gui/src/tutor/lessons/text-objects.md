# Text objects

Functions, classes and arguments as the language's grammar finds them: `af`, `if`, `ac` and `aa` after `d`, `c` or `y`, and `]f` and `[f` to move between functions. `ip` and `ap` take a paragraph as whole lines, `is` and `as` a sentence, and `it` and `at` what's between HTML tags, as Vim's do, here and in a terminal.

The practice file is Rust. If Rust's grammar isn't installed, the bottom bar asks to install it as the file opens: press `y`. Its language server needn't be.

```practice shapes.rs
struct Point {
    x: f64,
    y: f64,
}

fn first() {
    println!("delete me");
}

/// Doubles a width. Wider still.
fn double(width: u32) -> u32 {
    width * 2
}

fn area(width: u32, height: u32) -> u32 {
    width * height
}

fn last() {
    println!("<b>the end</b>");
}
```

## Delete a function with daf

`af` is a function and its body, as the grammar reads it, wherever the cursor is in it: its name, its arguments or its body. `d` deletes it, as `dw` deletes a word.

Put the cursor anywhere in `fn first`, and type `daf`.

```check
not has "fn first"
has "fn double"
```

## Change a function's body with cif

`if` is what's inside a function's body. `c` deletes it and goes to insert mode, as `cw` does.

Put the cursor in `double`'s body, type `cif`, then `width * 3`, and press Esc.

```check
has "width * 3"
not has "width * 2"
```

## Delete an argument with daa

`aa` is an argument and the comma that goes with it; `ia` is the argument alone.

In `area`, put the cursor on `height`, and type `daa`.

```check
has "fn area(width: u32) -> u32"
```

## Delete a struct with dac

`ac` is a class and its body: in Rust, a struct, an impl, a trait or an enum. `ic` is what's inside.

Put the cursor in `struct Point`, and type `dac`.

```check
not has "struct Point"
```

## Go between functions with ]f

`]f` goes to the start of the next function, and `[f` to the one before. A count goes that many: `2]f`. `]C` and `[C` do the same for classes. Each is a jump, so Ctrl+O comes back.

```setup
cursor 1
```

Go to `fn last` with `]f`.

```check
at "fn last"
```

## Growing a selection

In visual mode, `af` selects a function, and typed again grows the selection to the next one out. A count takes the one that many out: `d2af`. With none around the cursor, they take the next one.

While the daemon finds the pieces, keys you type wait their turn; if it takes over a second, or you press Esc, nothing is done. Without the grammar's own rules for a language, its pieces are found by the kinds of its nodes.

## Delete a paragraph with dap

`ip` is the paragraph around the cursor, as whole lines, and `ap` the paragraph with the blank lines after it. A count takes that many; in visual mode, typed again, they take the selection on by paragraphs.

Put the cursor in `fn area`, and type `dap`.

```check
not has "fn area"
has "fn last"
```

## Delete a sentence with das

`is` is the sentence around the cursor, and `as` the sentence with the blanks after it, or before it if there are none after. A sentence ends at `.`, `!` or `?` before a space, a tab or a line's end, and at an empty line, as Vim's do. `)` goes to the next sentence's start and `(` to this one's, or the one before's; with a count, that many.

Put the cursor on `Wider`, in the comment above `fn double`, and type `das`.

```check
not has "Wider"
has "/// Doubles a width."
```

## Change between tags with cit

`it` is what's between the HTML or XML tags around the cursor, and `at` the tags too, whatever the language. A count goes that many tags out; in visual mode, typed again, the selection grows to the tags around it.

Put the cursor on `the end`, type `cit`, then `done`, and press Esc.

```check
has "<b>done</b>"
```

## Copy whole lines with Y in visual mode

In visual mode, `Y` copies the lines the selection is on, whole, wherever on them it starts and ends, as `V` then `y` would; in a block, the block as it is.

Put the cursor in the middle of `fn double`'s first line, and type `vjY`.

```check
register 0 "fn double(width: u32) -> u32 {\n    width * 3\n"
```

## Undo goes back to where you were

`u` puts the cursor back where it was as the change began, as Vim's does: where an operator began, such as the start of what `dk` took. The last selection comes back as it was then, for `gv`. A command that changes nothing where Vim's makes a step all the same, such as `3J` on the last line, is a step of its own, which `u` undoes alone. Every window editing the file shares the steps, and what they keep follows other windows' and agents' edits.

```practice shapes.rs
struct Point {
    x: f64,
    y: f64,
}
```

```setup
cursor 3
```

Put the cursor on the `:` of `y: f64`, delete that line and the one above with `dk`, then press `u`.

```check
has "    x: f64,\n    y: f64,"
line 2
at ": f64,"
```
