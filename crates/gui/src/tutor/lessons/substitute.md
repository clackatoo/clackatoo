# Substitute, global and case

Vim's commands for replacing text and working on lines: `:s` with its ranges and flags, `:g` and `:v`, and changing case with `~`, `gu` and `gU`.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

## Replace on a line

`:s/old/new/` replaces the first match on the cursor's line. As you type it, what it would do shows in place, with the replacements lit; nothing's changed until Enter, and Esc puts the view back.

```setup
cursor 1
```

On the first line, type `:s/cat/fish/` and press Enter.

```expect
one fish, two cat, red cat
a cat is not a dog
cats and dogs
```

## Every match on the line

The `g` flag replaces every match on the line, not just the first. `c` asks about each, and `n` only counts them.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

```setup
cursor 1
```

Type `:s/cat/fish/g` and press Enter.

```expect
one fish, two fish, red fish
a cat is not a dog
cats and dogs
```

## Every line

A range before `s` says which lines: `%` is every line, `2,3` lines 2 to 3, `.,$` from the cursor's to the last, and `'<,'>` the selection's, which `:` in visual mode starts with, count or not.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

Type `:%s/cat/bird/g` and press Enter. However many lines it changes, `u` undoes it in one go.

```expect
one bird, two bird, red bird
a bird is not a dog
birds and dogs
```

## Patterns and replacements

Patterns are Vim's: `\<` and `\>` for a word's ends, `\(` `\)` for a group, `\v` for fewer backslashes. In the replacement, `&` is the match and `\1` its first group, and `\u` makes the next character a capital.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

Replace only the whole word `cat`, leaving `cats`, with a capital: `:%s/\<cat\>/\u&/g`.

```expect
one Cat, two Cat, red Cat
a Cat is not a dog
cats and dogs
```

## Delete the lines that match

`:g/pattern/command` does the command on every line that matches: `:g/TODO/d` deletes them, and `:g/x/normal Atext` types on each. `:v` does it on every line that doesn't match.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

Delete the lines with a dog: `:g/dog/d`.

```expect
one cat, two cat, red cat
```

## Type on every line

`:normal` types keys on each line, as in normal mode; under `:g`, on each line that matches.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

Put `!` at the end of every line: `:g/^/normal A!`.

```expect
one cat, two cat, red cat!
a cat is not a dog!
cats and dogs!
```

## Capitals with gU

`gU` and a motion or an object makes it capitals, `gu` small letters, `g~` turns the case over, and `g?` is rot13. `~` turns over the character under the cursor.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

```setup
cursor 2
```

Put the cursor on `dog`, and type `gUiw`.

```expect
one cat, two cat, red cat
a cat is not a DOG
cats and dogs
```

## The whole line

Doubled, they take the line: `gUU`, `guu`, `g~~`. In visual mode, `U`, `u` and `~` change what's selected.

```setup
cursor 3
```

Make the last line capitals with `gUU`.

```check
has "CATS AND DOGS"
```

## Every match lit

A search lights up every match in view, the one it went to brighter than the rest, as Vim's `hlsearch` does: after `/`, `?`, `n` and `N`, in a terminal's normal mode and a chat too, and in an editor after `*`, `#`, `:s` and `:g`. Esc or `:noh` puts them out until the next search. `:set nohlsearch` lights only the one found.

```practice pets.txt
one cat, two cat, red cat
a cat is not a dog
cats and dogs
```

```setup
cursor 1
```

Type `/cat` and press Enter: every `cat` lights up. Press `n` to go to the next, then type `cw`, `dog` and Esc.

```expect
one cat, two dog, red cat
a cat is not a dog
cats and dogs
```
