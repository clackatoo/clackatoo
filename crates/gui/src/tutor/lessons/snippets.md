# Completion and snippets

The menu of what could finish a word, as the language server says, and snippets: a function with its arguments, say, put in with places to type.

Completion comes from the file's language server, here rust-analyzer for Rust. If it isn't installed, the bottom bar asks to install Rust and it as the file opens. rust-analyzer reads some of Rust's standard library first, so the first menu can take a few seconds. Without it, read the steps and press `]s`.

```practice main.rs
fn main() {
    let name = "world";

}
```

## Trusting the folder

A language server can run the code of the project it reads: rust-analyzer builds and runs a crate's build scripts and macros, and others load plugins the project names. So one starts only in a folder you've trusted. As this file opened, the bottom bar asked "Trust …? Its language servers may run its code (y/n)", once its editor had the focus in normal mode. `y` trusts the folder, with everything inside it, and starts its servers; `n` says no, and it isn't asked about again; any other key leaves it unanswered. Only a key you press then answers it, not a macro's or an agent's, and not in the first half second it shows. A folder that isn't trusted still has its colours, but no server.

`:trust` trusts the focused file's project, and `:trust DIR` a folder you name; `:untrust` takes that back, stopping its servers. Type `:trusted` to list the folders marked either way, then `:Messages` to read the list again.

```check
messages
```

## The menu

As you type a word in insert mode, the server is asked what could finish it, once typing pauses; a character such as `.` asks at once, and so do Ctrl+Space, Ctrl+N and Ctrl+P. The menu narrows as you type, best first. Each item's label starts right under the word you're typing, with its letters that match lit and its detail dimmed, and its kind dimmed at the right. The picked item's documentation shows beside the menu: what it is, dimmed, over a line, then what it does.

Ctrl+N and Ctrl+P, or Down and Up, pick; Ctrl+Y accepts; Enter accepts the item you moved to, and otherwise starts a new line, as the option `completeaccept` says; Ctrl+E closes the menu, and Esc closes it and leaves insert mode.

## A snippet

Accepting a snippet puts it in with places to type. Its first place's text is selected: typing replaces it, and Backspace deletes it. Tab goes to the next place, and from the last to the snippet's end; Shift+Tab goes back. What you type in a place is copied to the places that mirror it, and a place with choices shows them in the menu.

```setup
cursor 3
```

On the empty line, press `i`, type `println`, pick `println!` from the menu and accept it with Ctrl+Y. Type `"hello {name}"` in its place, and press Tab to leave it.

```check
has "println!("
```

## Repeating it

Accepting puts in the item, and any import it needs elsewhere in the file, as one undo step with what you typed, and `.` types it again. Filling in a snippet is one undo step with the rest, and `.` types what you left. While you type a call's arguments, its signature shows over the cursor, with the argument you're on in bold.

Tab is Tab again once you reach the snippet's end, press Esc, or move out of it.
