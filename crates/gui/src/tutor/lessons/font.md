# Font

The font and size text is drawn in, changed as you go with `:set font` and `:set fontsize`, and set for good in your config.

```practice font.txt
The quick brown fox jumps over the lazy dog.
0O 1lI |!  {[( )]}  -> => != <=
┌──────┐
│ box  │
└──────┘
```

## Bigger text

`:set fontsize=17` draws text at 17 points. The window keeps its size, and the panes fit as many cells as now fit, as when you resize it: terminals are told their new size, and programs in them draw again. A size can have a fraction, such as `10.5`.

```check
option fontsize=17
```

## Another font

`:set font=` takes a font family's name, as `fc-list : family` lists them, with a backslash before each space: `:set font=JetBrains\ Mono`. A family that isn't installed is refused, and the font stays as it was. `:set font=` with nothing after it goes back to your system's monospace font, and `:set fontsize=12` to the usual size.

To keep them, set them in your `init.lua`:

```lua
client.opt.font = "JetBrains Mono"
client.opt.fontsize = 12
```

`--font` and `--font-size` on the command line, or `CLACKATOO_FONT` and `CLACKATOO_FONT_SIZE` in the environment, outdo what `init.lua` sets as it runs, for that window. What's set after, by `:set` or a key your config binds, changes it still.
