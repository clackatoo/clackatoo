# Words in a language

Some languages have names with more than letters, digits and `_` in them, such as CSS's `margin-top`. In those, `w`, `e`, `b`, `iw`, `*` and completion take the whole name as one word, as the language's pack says.

The practice file is CSS. Its grammar needn't be installed: clackatoo knows CSS's words.

```practice style.css
.card {
  margin-top: 4px;
  border-radius: 2px;
}

.wide {
  margin-top: 8px;
}
```

## Delete a whole name with diw

`iw` is the word under the cursor. In plain text, `margin-top` is two words with a `-` between; in CSS it's one.

```setup
cursor 2
```

Put the cursor on `border`, and type `diw` to delete `border-radius`.

```check
not has "border-radius"
has "  : 2px;"
```

## Find a name with *

`*` searches for the word under the cursor, as a whole word. Here that's all of `margin-top`, and `n` goes on to the next.

```setup
cursor 2
```

With the cursor on `margin-top`, type `*` to go to the next one. Then type `cw`, `padding-top` and Esc: `cw` changes the whole name.

```check
has "  padding-top: 8px;"
has "  margin-top: 4px;"
```

A language pack you write says what else its words are made of in its `language.toml`, as Vim's `iskeyword` does: `word = "-"`, or for Lisp, `word = "-?!*"`.
