# Text objects

Functions, classes and arguments as the language's grammar finds them: `af`, `if`, `ac` and `aa` after `d`, `c` or `y`, and `]f` and `[f` to move between functions. `ip` and `ap` take a paragraph as whole lines, as Vim's do, here and in a terminal.

The practice file is Rust. If Rust's grammar isn't installed, the bottom bar asks to install it as the file opens: press `y`. Its language server needn't be.

```practice shapes.rs
struct Point {
    x: f64,
    y: f64,
}

fn first() {
    println!("delete me");
}

fn double(width: u32) -> u32 {
    width * 2
}

fn area(width: u32, height: u32) -> u32 {
    width * height
}

fn last() {
    println!("the end");
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
