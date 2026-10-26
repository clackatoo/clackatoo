-- What Neovim does with keys, case by case, for the editor's tests to be
-- checked against. Not run by CI: run it by hand from this directory,
--
--   timeout 60 nvim --clean --headless -n -i NONE -c 'luafile check.lua'
--
-- and it prints, for each case, the lines left, where the cursor is
-- (1-based line, 0-based byte), the mode, and what the unnamed register
-- holds and how (`V` for lines, `v` for characters). Transcribe what it
-- says into a test module under `crates/gui/src/editor/tests/`, whose doc
-- says it was checked against Neovim, and note where clackatoo differs on
-- purpose.
--
-- Add a case to `cases` below: `lines`, the cursor `at` as {line, byte}
-- (1-based line, 0-based byte), `keys` in Vim's notation (`<Esc>`,
-- `<CR>`, `<C-v>`), and `set`, options to `:set` for it alone, if any
-- (`'sol'`). For keys that depend on the view (`H`, Ctrl+D), `view` sets
-- the window's `height` in rows, its `top` line, and its `columns` for
-- lines to wrap at; the view's top line is printed too.
--
-- Gotchas (see the Acta memory `checking-vim-behaviour-headless`):
-- - `startofline` is off in Neovim, so `gg`, `G`, the paging keys and
--   linewise operators keep the column, as clackatoo's do with its own
--   off, which is its default too. A case can `set = 'sol'` for Vim's
--   way.
-- - A motion that fails inside `normal!` silently ends that case's keys.
-- - `.` carries over from earlier cases.
-- - Anything that waits for input (`:s///c`) hangs the run: keep `timeout`.
-- - `normal!` skips blanks its keys start with: a case starting with
--   Space starts `1<Space>`.

-- Eight lines, each indented by its number's parity, so the first
-- non-blank and the column kept are told apart.
local eight = {
  'one', '  two', 'three', '  four', 'five', '  six', 'seven', '  eight',
}

-- Words and WORDs apart, indented.
local dotted = { 'a.b c.d e.f', '  x.y z', '  last.one end' }

-- Sixty lines, every other one indented.
local sixty = {}
for n = 1, 60 do
  sixty[n] = (n % 2 == 0 and '  ' or '') .. 'line ' .. n
end

-- Sixty lines, the odd ones long enough to wrap over two rows of 20.
local wrapping = {}
for n = 1, 60 do
  wrapping[n] = n % 2 == 1 and (('%02d'):format(n) .. string.rep('x', 28)) or ('line ' .. n)
end

-- Sixty lines, the odd ones over three rows of 20.
local tall = {}
for n = 1, 60 do
  tall[n] = n % 2 == 1 and (('%02d'):format(n) .. string.rep('x', 48)) or ('line ' .. n)
end

-- `wrapping`, and a 61st line over five rows of 20.
local endlong = {}
for n = 1, 60 do
  endlong[n] = wrapping[n]
end
endlong[61] = '61' .. string.rep('z', 88)

-- Thirty lines, all but every fifth over four rows of 20.
local long = {}
for n = 1, 30 do
  long[n] = n % 5 == 0 and ('  line ' .. n) or (('%02d'):format(n) .. string.rep('y', 68))
end

-- Indents, blanks after, an empty line and punctuation, for `+`, `-`,
-- `_`, `g_`, `|`, `ge` and Space and Backspace.
local spaced = { '  one two.three', 'four  five', '', '  six seven  ', 'eight' }

-- Two paragraphs, two blank lines, two more, for `ip` and `ap`.
local para = { 'one', '  two', '', '', 'three', 'four', '', 'five' }

-- Sentences, for `(`, `)`, `is` and `as`: ends with `.`, `!` and `?`,
-- closing quotes and brackets after them, and a paragraph's end.
local prose = { 'One. Two!  Three (four.)  Five', 'six? Seven "eight." Nine.', '', 'Ten. Last' }

-- Tags, for `it` and `at`: nested, one with a `>` in quotes, a closing
-- tag on a line of its own.
local html = { '<div><p>Hi <b>bold</b> there</p>', '  <a href="x>y">link</a>', '</div>' }

-- Four lines, for `Y` in visual mode.
local four = { 'alpha beta', 'gamma delta', 'epsilon', 'zeta eta' }

local cases = {
  -- CLACKA-127: a count before `gg`, or `G` as an operator's target, is a
  -- line number.
  { name = '5gg from the last line', lines = eight, at = { 8, 4 }, keys = '5gg' },
  { name = '3G from the first line', lines = eight, at = { 1, 2 }, keys = '3G' },
  { name = '1G from the last line', lines = eight, at = { 8, 4 }, keys = '1G' },
  { name = '99G', lines = eight, at = { 1, 0 }, keys = '99G' },
  { name = '99gg', lines = eight, at = { 1, 0 }, keys = '99gg' },
  { name = 'd3G from the first line', lines = eight, at = { 1, 0 }, keys = 'd3G' },
  { name = 'd3G then . from the first line', lines = eight, at = { 1, 0 }, keys = 'd3G.' },
  { name = 'd3gg from the last line', lines = eight, at = { 8, 4 }, keys = 'd3gg' },
  { name = 'd3G from the last line', lines = eight, at = { 8, 4 }, keys = 'd3G' },
  { name = 'd1G from line 4', lines = eight, at = { 4, 2 }, keys = 'd1G' },
  { name = 'dG from line 4', lines = eight, at = { 4, 2 }, keys = 'dG' },
  { name = 'dgg from line 4', lines = eight, at = { 4, 2 }, keys = 'dgg' },
  { name = 'd99G from line 3', lines = eight, at = { 3, 0 }, keys = 'd99G' },
  { name = '2d2G from the first line', lines = eight, at = { 1, 0 }, keys = '2d2G' },
  { name = '2d3gg from the last line', lines = eight, at = { 8, 4 }, keys = '2d3gg' },
  { name = 'y2gg from the last line', lines = eight, at = { 8, 4 }, keys = 'y2gg' },
  { name = 'y2gg then P', lines = eight, at = { 8, 4 }, keys = 'y2ggP' },
  { name = 'y3G from line 6', lines = eight, at = { 6, 2 }, keys = 'y3G' },
  { name = 'ygg from line 4', lines = eight, at = { 4, 2 }, keys = 'ygg' },
  { name = 'V2ggd from line 5', lines = eight, at = { 5, 0 }, keys = 'V2ggd' },
  -- `startofline`: off, as Neovim's is, `gg`, `G`, `H`, `M` and `L` keep
  -- the column; on, as Vim's is, they go to the first non-blank.
  { name = 'gg then j from the last line', lines = eight, at = { 8, 4 }, keys = 'ggj' },
  { name = 'G from line 1', lines = eight, at = { 1, 2 }, keys = 'G' },
  { name = 'H from line 5', lines = eight, at = { 5, 3 }, keys = 'H' },
  { name = 'L from line 1', lines = eight, at = { 1, 2 }, keys = 'L' },
  { name = 'M from line 1', lines = eight, at = { 1, 2 }, keys = 'M' },
  { name = 'gg with sol', lines = eight, at = { 8, 4 }, keys = 'gg', set = 'sol' },
  { name = '6G with sol', lines = eight, at = { 1, 2 }, keys = '6G', set = 'sol' },
  { name = 'H with sol', lines = eight, at = { 5, 3 }, keys = 'H', set = 'sol' },
  { name = 'L with sol', lines = eight, at = { 1, 2 }, keys = 'L', set = 'sol' },
  { name = ':5 from the last line', lines = eight, at = { 8, 4 }, keys = ':5<CR>' },
  { name = ':6 with sol', lines = eight, at = { 1, 0 }, keys = ':6<CR>', set = 'sol' },
  { name = ':6 from line 1', lines = eight, at = { 1, 0 }, keys = ':6<CR>' },
  { name = '$ then :7', lines = eight, at = { 1, 0 }, keys = '$:7<CR>' },
  -- Ctrl and Shift with the arrows, Home and End, as motion.txt says.
  { name = '<C-Right>', lines = dotted, at = { 1, 0 }, keys = '<C-Right>' },
  { name = '<S-Right>', lines = dotted, at = { 1, 0 }, keys = '<S-Right>' },
  { name = '<C-Left>', lines = dotted, at = { 1, 10 }, keys = '<C-Left>' },
  { name = '<S-Left>', lines = dotted, at = { 1, 10 }, keys = '<S-Left>' },
  { name = 'd<C-End> from line 2', lines = dotted, at = { 2, 3 }, keys = 'd<C-End>' },
  { name = '<C-End>', lines = dotted, at = { 1, 0 }, keys = '<C-End>' },
  { name = '<C-Home>', lines = dotted, at = { 3, 5 }, keys = '<C-Home>' },
  { name = 'insert <C-Left>', lines = dotted, at = { 1, 10 }, keys = 'a<C-Left>X<Esc>' },
  { name = 'insert <S-Left>', lines = dotted, at = { 1, 10 }, keys = 'a<S-Left>X<Esc>' },
  -- Esc after keys half typed in visual mode.
  { name = 'v alone', lines = dotted, at = { 1, 0 }, keys = 'vl' },
  { name = 'vg<Esc>', lines = dotted, at = { 1, 0 }, keys = 'vlg<Esc>' },
  { name = 'vi<Esc>', lines = dotted, at = { 1, 0 }, keys = 'vli<Esc>' },
  { name = 'v3<Esc>', lines = dotted, at = { 1, 0 }, keys = 'vl3<Esc>' },
  { name = 'v"a<Esc>', lines = dotted, at = { 1, 0 }, keys = 'vl"a<Esc>' },
  -- Paging, in a view of ten rows from line 11, or 21.
  { name = '<C-d>', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '<C-d>' },
  { name = '3<C-d> then <C-d>', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '3<C-d><C-d>' },
  { name = '<C-u>', lines = sixty, at = { 25, 4 }, view = { height = 10, top = 21 }, keys = '<C-u>' },
  { name = '<C-f>', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '<C-f>' },
  { name = '<C-f> so=3', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '<C-f>', set = 'scrolloff=3' },
  { name = '2<C-f>', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '2<C-f>' },
  { name = '<C-b>', lines = sixty, at = { 25, 4 }, view = { height = 10, top = 21 }, keys = '<C-b>' },
  { name = '<C-d> wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = '<C-d>' },
  { name = '<C-f> wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = '<C-f>' },
  -- H, M and L, in a view of ten rows from line 11.
  { name = 'H', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'H' },
  { name = '3H', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '3H' },
  { name = 'M', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'M' },
  { name = 'L', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'L' },
  { name = '3L', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '3L' },
  { name = 'H so=3', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'H', set = 'scrolloff=3' },
  { name = '2H so=3', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '2H', set = 'scrolloff=3' },
  { name = '5H so=3', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '5H', set = 'scrolloff=3' },
  { name = 'L so=3', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'L', set = 'scrolloff=3' },
  { name = '99H so=3', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '99H', set = 'scrolloff=3' },
  { name = 'H so=3 at the top', lines = sixty, at = { 5, 4 }, view = { height = 10, top = 1 }, keys = 'H', set = 'scrolloff=3' },
  { name = 'H sol', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 12 }, keys = 'H', set = 'sol' },
  { name = 'dL so=3', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'dL', set = 'scrolloff=3' },
  { name = 'yH', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'yH' },
  { name = 'M short text', lines = { 'a', 'b', 'c', 'd' }, at = { 1, 0 }, view = { height = 10, top = 1 }, keys = 'M' },
  { name = 'H wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = 'H' },
  { name = 'M wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = 'M' },
  { name = 'L wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = 'L' },
  { name = '3H wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = '3H' },
  { name = '3L wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = '3L' },
  { name = 'H so=2 wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = 'H', set = 'scrolloff=2' },
  { name = 'L so=2 wrapped', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = 'L', set = 'scrolloff=2' },
  -- Ctrl+E and Ctrl+Y: the view a line, the cursor kept in it.
  { name = '<C-e>', lines = sixty, at = { 11, 4 }, view = { height = 10, top = 11 }, keys = '<C-e>' },
  { name = '3<C-e>', lines = sixty, at = { 11, 4 }, view = { height = 10, top = 11 }, keys = '3<C-e>' },
  { name = '<C-y>', lines = sixty, at = { 20, 4 }, view = { height = 10, top = 11 }, keys = '<C-y>' },
  { name = '<C-e> so=3', lines = sixty, at = { 14, 4 }, view = { height = 10, top = 11 }, keys = '<C-e>', set = 'scrolloff=3' },
  -- startofline's reach: paging, and linewise d, <<, >> and ==, but not
  -- y, nor Ctrl+E, nor what an operator takes. Indents as clackatoo's
  -- start: spaces, four columns.
  { name = '<C-d> sol', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '<C-d>', set = 'sol' },
  { name = '<C-u> sol', lines = sixty, at = { 25, 4 }, view = { height = 10, top = 21 }, keys = '<C-u>', set = 'sol' },
  { name = '<C-f> sol', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '<C-f>', set = 'sol' },
  { name = '<PageUp> sol', lines = sixty, at = { 25, 4 }, view = { height = 10, top = 21 }, keys = '<PageUp>', set = 'sol' },
  { name = '<C-e> sol', lines = sixty, at = { 11, 4 }, view = { height = 10, top = 11 }, keys = '<C-e>', set = 'sol' },
  { name = 'dd', lines = eight, at = { 3, 4 }, keys = 'dd', set = 'et sw=4 ts=4' },
  { name = 'dd sol', lines = eight, at = { 3, 4 }, keys = 'dd', set = 'sol et sw=4 ts=4' },
  { name = 'dj', lines = eight, at = { 3, 4 }, keys = 'dj', set = 'et sw=4 ts=4' },
  { name = 'dj sol', lines = eight, at = { 3, 4 }, keys = 'dj', set = 'sol et sw=4 ts=4' },
  { name = 'dk sol', lines = eight, at = { 5, 3 }, keys = 'dk', set = 'sol et sw=4 ts=4' },
  { name = 'dk', lines = eight, at = { 5, 3 }, keys = 'dk', set = 'et sw=4 ts=4' },
  { name = 'dG sol', lines = eight, at = { 5, 3 }, keys = 'dG', set = 'sol et sw=4 ts=4' },
  { name = 'dG', lines = eight, at = { 5, 3 }, keys = 'dG', set = 'et sw=4 ts=4' },
  { name = '>>', lines = eight, at = { 3, 2 }, keys = '>>', set = 'et sw=4 ts=4' },
  { name = '>> sol', lines = eight, at = { 3, 2 }, keys = '>>', set = 'sol et sw=4 ts=4' },
  { name = '<<', lines = eight, at = { 4, 4 }, keys = '<<', set = 'et sw=4 ts=4' },
  { name = '<< sol', lines = eight, at = { 4, 4 }, keys = '<<', set = 'sol et sw=4 ts=4' },
  { name = '>j', lines = eight, at = { 3, 2 }, keys = '>j', set = 'et sw=4 ts=4' },
  { name = '>j sol', lines = eight, at = { 3, 2 }, keys = '>j', set = 'sol et sw=4 ts=4' },
  { name = 'yj sol', lines = eight, at = { 3, 4 }, keys = 'yj', set = 'sol et sw=4 ts=4' },
  { name = 'yk sol', lines = eight, at = { 3, 4 }, keys = 'yk', set = 'sol et sw=4 ts=4' },
  { name = 'ygg sol', lines = eight, at = { 4, 3 }, keys = 'ygg', set = 'sol et sw=4 ts=4' },
  { name = 'yy sol', lines = eight, at = { 3, 4 }, keys = 'yy', set = 'sol et sw=4 ts=4' },
  -- Stage 2's review: pages put the cursor at the new view's top or
  -- bottom whenever the view moved, and leave it where it was if it
  -- couldn't.
  { name = '<C-f> from low in view', lines = sixty, at = { 20, 4 }, view = { height = 10, top = 11 }, keys = '<C-f>' },
  { name = '<PageUp> from the top line', lines = sixty, at = { 21, 4 }, view = { height = 10, top = 21 }, keys = '<PageUp>' },
  { name = '<PageUp> near the top', lines = sixty, at = { 8, 4 }, view = { height = 10, top = 4 }, keys = '<PageUp>' },
  { name = '<PageUp> at the top', lines = sixty, at = { 5, 4 }, view = { height = 10, top = 1 }, keys = '<PageUp>' },
  { name = '<PageUp> at the top sol', lines = sixty, at = { 6, 4 }, view = { height = 10, top = 1 }, keys = '<PageUp>', set = 'sol' },
  { name = '2<PageUp>', lines = sixty, at = { 35, 4 }, view = { height = 10, top = 31 }, keys = '2<PageUp>' },
  { name = '<C-f> with the end in view', lines = sixty, at = { 55, 4 }, view = { height = 10, top = 51 }, keys = '<C-f>' },
  { name = '<C-f> so=3 near the end', lines = sixty, at = { 50, 4 }, view = { height = 10, top = 45 }, keys = '<C-f>', set = 'scrolloff=3' },
  { name = '<C-f> short', lines = { 'a', 'b', 'c' }, at = { 1, 0 }, view = { height = 10, top = 1 }, keys = '<C-f>' },
  { name = '<PageUp> wrapped', lines = wrapping, at = { 24, 2 }, view = { height = 10, top = 21, columns = 20 }, keys = '<PageUp>' },
  { name = '<C-f> wrapped onto a long line', lines = wrapping, at = { 13, 2 }, view = { height = 10, top = 12, columns = 20 }, keys = '<C-f>' },
  { name = '<C-b> wrapped from the top line', lines = wrapping, at = { 30, 2 }, view = { height = 10, top = 30, columns = 20 }, keys = '<C-b>' },
  { name = '3zt sol', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '3zt', set = 'sol' },
  -- Half pages don't scroll past the text's end, and go as far as the
  -- view at most.
  { name = '<C-d> near the end', lines = sixty, at = { 50, 4 }, view = { height = 10, top = 48 }, keys = '<C-d>' },
  { name = '<C-d> with the end in view', lines = sixty, at = { 55, 4 }, view = { height = 10, top = 51 }, keys = '<C-d>' },
  { name = '<C-d> short', lines = { 'a', 'b', 'c' }, at = { 1, 0 }, view = { height = 10, top = 1 }, keys = '<C-d>' },
  { name = '<C-u> near the top', lines = sixty, at = { 8, 4 }, view = { height = 10, top = 3 }, keys = '<C-u>' },
  { name = '20<C-d>', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '20<C-d>' },
  { name = '<C-u> wrapped', lines = wrapping, at = { 24, 2 }, view = { height = 10, top = 21, columns = 20 }, keys = '<C-u>' },
  { name = '<C-d> wrapped onto a long line', lines = wrapping, at = { 13, 2 }, view = { height = 10, top = 12, columns = 20 }, keys = '<C-d>' },
  -- H, M and L with a scrolloff of half the view or more, and taken
  -- by an operator with a count past the view.
  { name = '<C-e>H wrapped sms', lines = wrapping, at = { 14, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = '<C-e>H', set = 'smoothscroll' },
  { name = 'H so=4', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'H', set = 'scrolloff=4' },
  { name = 'L so=4', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'L', set = 'scrolloff=4' },
  { name = 'H so=5', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'H', set = 'scrolloff=5' },
  { name = 'L so=5', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'L', set = 'scrolloff=5' },
  { name = 'L so=10', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'L', set = 'scrolloff=10' },
  { name = '99L', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '99L' },
  { name = 'd99L', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'd99L' },
  { name = 'd99H', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = 'd99H' },
  -- `:d` goes to the first non-blank whatever startofline says.
  { name = ':3d', lines = eight, at = { 1, 0 }, keys = ':3d<CR>' },
  { name = ':d', lines = eight, at = { 4, 4 }, keys = ':d<CR>' },
  -- y'a leaves the cursor at the mark's line's first non-blank.
  { name = "y'a", lines = eight, at = { 2, 4 }, keys = "ma3jy'a" },
  -- zt, zz and zb with a count: that line at the top, middle or bottom.
  { name = '5zt', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '5zt' },
  { name = '30zz', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '30zz' },
  { name = '40zb', lines = sixty, at = { 15, 4 }, view = { height = 10, top = 11 }, keys = '40zb' },
  { name = 'zz', lines = sixty, at = { 20, 4 }, view = { height = 10, top = 11 }, keys = 'zz' },
  { name = 'zz wrapped', lines = wrapping, at = { 21, 2 }, view = { height = 10, top = 11, columns = 20 }, keys = 'zz' },
  { name = 'zz wrapped, the second row', lines = wrapping, at = { 21, 25 }, view = { height = 10, top = 11, columns = 20 }, keys = 'zz' },
  { name = 'zz in nine rows', lines = sixty, at = { 20, 4 }, view = { height = 9, top = 11 }, keys = 'zz' },
  { name = '5zt from the top', lines = sixty, at = { 1, 4 }, view = { height = 10, top = 1 }, keys = '5zt' },
  -- The stage 2 fix round's review: zz and zb start the view at a line's
  -- start; a counted zt keeps the column it's in; pages keep scrolloff
  -- in whole lines; zt at the end keeps scrolloff; big scrolloff.
  { name = 'zz wrapped from line 14', lines = wrapping, at = { 14, 4 }, view = { height = 10, top = 11, columns = 20 }, keys = 'zz' },
  { name = 'zz wrapped in nine rows', lines = wrapping, at = { 4, 4 }, view = { height = 9, top = 1, columns = 20 }, keys = 'zz' },
  { name = 'zb tall', lines = tall, at = { 57, 4 }, view = { height = 9, top = 57, columns = 20 }, keys = 'zb' },
  { name = 'k10zt', lines = sixty, at = { 12, 8 }, view = { height = 10, top = 5 }, keys = 'k10zt' },
  { name = 'k10ztjj', lines = sixty, at = { 12, 8 }, view = { height = 10, top = 5 }, keys = 'k10ztjj' },
  { name = '<PageUp> so=3 endlong', lines = endlong, at = { 4, 4 }, view = { height = 10, top = 2, columns = 20 }, keys = '<PageUp>', set = 'scrolloff=3' },
  { name = '<PageUp> so=3 wrapping', lines = wrapping, at = { 5, 4 }, view = { height = 10, top = 2, columns = 20 }, keys = '<PageUp>', set = 'scrolloff=3' },
  { name = '<PageDown> so=3 endlong', lines = endlong, at = { 55, 4 }, view = { height = 10, top = 52, columns = 20 }, keys = '<PageDown>', set = 'scrolloff=3' },
  { name = 'zt so=3 at the end', lines = sixty, at = { 60, 4 }, view = { height = 10, top = 51 }, keys = 'zt', set = 'scrolloff=3' },
  { name = '<C-f> so=3 end in view', lines = sixty, at = { 55, 4 }, view = { height = 10, top = 52 }, keys = '<C-f>', set = 'scrolloff=3' },
  { name = '3H so=5 sol at the end', lines = sixty, at = { 56, 4 }, view = { height = 10, top = 52 }, keys = '3H', set = 'scrolloff=5 sol' },
  -- Finds, which a terminal takes too.
  { name = 't.;', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = 't.;' },
  { name = 't.;;,', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = 't.;;,' },
  { name = 'F.,', lines = { 'ab.cd.ef.gh' }, at = { 1, 10 }, keys = 'F.,' },
  { name = 'f.2;', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = 'f.2;' },
  { name = 'yt.', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = 'yt.' },
  { name = 'yF.', lines = { 'ab.cd.ef.gh' }, at = { 1, 10 }, keys = 'yF.' },
  { name = '2t.', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = '2t.' },
  { name = '4f.', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = '4f.' },
  { name = 'd2t.', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = 'd2t.' },
  { name = 'd4f.', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = 'd4f.' },
  { name = 't.2;', lines = { 'ab.cd.ef.gh' }, at = { 1, 0 }, keys = 't.2;' },
  { name = '2T.', lines = { 'ab.cd.ef.gh' }, at = { 1, 10 }, keys = '2T.' },
  { name = 'T.;', lines = { 'ab.cd.ef.gh' }, at = { 1, 10 }, keys = 'T.;' },
  -- Stage 3's motions, written once for every pane.
  { name = '+', lines = spaced, at = { 1, 5 }, keys = '+' },
  { name = '<CR>', lines = spaced, at = { 1, 5 }, keys = '<CR>' },
  { name = '3+', lines = spaced, at = { 1, 5 }, keys = '3+' },
  { name = '9+', lines = spaced, at = { 1, 5 }, keys = '9+' },
  { name = '+ on the last line', lines = spaced, at = { 5, 2 }, keys = '+' },
  { name = '-', lines = spaced, at = { 2, 3 }, keys = '-' },
  { name = '3-', lines = spaced, at = { 5, 3 }, keys = '3-' },
  { name = 'd+', lines = spaced, at = { 1, 5 }, keys = 'd+' },
  { name = 'y-', lines = spaced, at = { 2, 3 }, keys = 'y-' },
  { name = '_', lines = spaced, at = { 1, 8 }, keys = '_' },
  { name = '3_', lines = spaced, at = { 2, 8 }, keys = '3_' },
  { name = '9_', lines = spaced, at = { 2, 8 }, keys = '9_' },
  { name = 'd2_', lines = spaced, at = { 1, 8 }, keys = 'd2_' },
  { name = 'd2_ on the last line', lines = spaced, at = { 5, 2 }, keys = 'd2_' },
  { name = 'g_', lines = spaced, at = { 4, 0 }, keys = 'g_' },
  { name = '2g_', lines = spaced, at = { 3, 0 }, keys = '2g_' },
  { name = 'dg_', lines = spaced, at = { 4, 2 }, keys = 'dg_' },
  { name = 'g_ empty', lines = spaced, at = { 3, 0 }, keys = 'g_' },
  { name = '5|', lines = spaced, at = { 1, 12 }, keys = '5|' },
  { name = '30|', lines = spaced, at = { 1, 2 }, keys = '30|' },
  { name = '|', lines = spaced, at = { 1, 12 }, keys = '|' },
  { name = '30|j', lines = spaced, at = { 1, 2 }, keys = '30|j' },
  { name = 'd5|', lines = spaced, at = { 1, 12 }, keys = 'd5|' },
  { name = 'ge', lines = spaced, at = { 1, 6 }, keys = 'ge' },
  { name = 'ge from within', lines = spaced, at = { 1, 8 }, keys = 'ge' },
  { name = 'ge over a line', lines = spaced, at = { 2, 1 }, keys = 'ge' },
  { name = 'ge to an empty line', lines = spaced, at = { 4, 3 }, keys = 'ge' },
  { name = 'ge from an empty line', lines = spaced, at = { 3, 0 }, keys = 'ge' },
  { name = '3ge', lines = spaced, at = { 2, 6 }, keys = '3ge' },
  { name = 'gE', lines = spaced, at = { 1, 14 }, keys = 'gE' },
  { name = 'ge at the start', lines = spaced, at = { 1, 0 }, keys = 'ge' },
  { name = 'ge at the first word', lines = spaced, at = { 1, 3 }, keys = 'ge' },
  { name = 'dge', lines = spaced, at = { 1, 6 }, keys = 'dge' },
  { name = 'dgE', lines = spaced, at = { 2, 6 }, keys = 'dgE' },
  { name = '3<Space>', lines = spaced, at = { 1, 13 }, keys = '3<Space><Esc>' },
  { name = '<Space> at the end', lines = spaced, at = { 2, 9 }, keys = '1<Space><Esc>' },
  { name = '<Space> to an empty line', lines = spaced, at = { 2, 9 }, keys = '2<Space><Esc>' },
  { name = 'd<Space> at the end', lines = spaced, at = { 2, 9 }, keys = 'd<Space><Esc>' },
  { name = 'd2<Space> at the end', lines = spaced, at = { 2, 9 }, keys = 'd2<Space><Esc>' },
  { name = 'd3<Space> at the end', lines = spaced, at = { 2, 9 }, keys = 'd3<Space><Esc>' },
  { name = 'd4<Space> at the end', lines = spaced, at = { 2, 9 }, keys = 'd4<Space><Esc>' },
  { name = 'd9<Space> on the last line', lines = spaced, at = { 5, 2 }, keys = 'd9<Space><Esc>' },
  { name = '<BS> at the start', lines = spaced, at = { 2, 0 }, keys = '<BS>' },
  { name = '<BS> to an empty line', lines = spaced, at = { 4, 0 }, keys = '<BS>' },
  { name = '3<BS>', lines = spaced, at = { 2, 1 }, keys = '3<BS>' },
  { name = 'd<BS> at the start', lines = spaced, at = { 2, 0 }, keys = 'd<BS>' },
  { name = 'd2<BS> at the start', lines = spaced, at = { 2, 0 }, keys = 'd2<BS>' },
  { name = 'd3<BS>', lines = spaced, at = { 2, 1 }, keys = 'd3<BS>' },
  { name = 'y3<BS>', lines = spaced, at = { 2, 1 }, keys = 'y3<BS>' },
  { name = 'y<BS> at the start', lines = spaced, at = { 2, 0 }, keys = 'y<BS>' },
  -- Paragraphs, whole lines.
  { name = 'yip', lines = para, at = { 2, 3 }, keys = 'yip' },
  { name = 'yap', lines = para, at = { 2, 3 }, keys = 'yap' },
  { name = 'dip', lines = para, at = { 6, 1 }, keys = 'dip' },
  { name = 'dap', lines = para, at = { 6, 1 }, keys = 'dap' },
  { name = 'dap last', lines = para, at = { 8, 2 }, keys = 'dap' },
  { name = 'yip blank', lines = para, at = { 3, 0 }, keys = 'yip' },
  { name = 'yap blank', lines = para, at = { 3, 0 }, keys = 'yap' },
  { name = '2yip', lines = para, at = { 1, 0 }, keys = '2yip' },
  { name = '2yap', lines = para, at = { 1, 0 }, keys = '2yap' },
  { name = 'vip', lines = para, at = { 2, 3 }, keys = 'vip' },
  { name = '3yip last', lines = para, at = { 8, 0 }, keys = '3yip' },
  { name = 'cip', lines = para, at = { 2, 3 }, keys = 'cipx<Esc>' },
  { name = 'y2ap', lines = para, at = { 5, 2 }, keys = 'y2ap' },
  { name = 'M with a line cut off', lines = long, at = { 3, 4 }, view = { height = 10, top = 2, columns = 20 }, keys = 'M' },
  -- CLACKA-127: sentences, tags, and `Y` in visual mode.
  { name = ')', lines = prose, at = { 1, 0 }, keys = ')' },
  { name = '3)', lines = prose, at = { 1, 0 }, keys = '3)' },
  { name = '( from Nine', lines = prose, at = { 2, 21 }, keys = '(' },
  { name = '2( from Ten', lines = prose, at = { 4, 0 }, keys = '2(' },
  { name = ') to the end', lines = prose, at = { 4, 6 }, keys = ')' },
  { name = 'd) at Two', lines = prose, at = { 1, 5 }, keys = 'd)' },
  { name = 'd) at the last word', lines = prose, at = { 4, 6 }, keys = 'd)' },
  { name = 'dis in Three', lines = prose, at = { 1, 13 }, keys = 'dis' },
  { name = 'das in Three', lines = prose, at = { 1, 13 }, keys = 'das' },
  { name = 'yas over a line end', lines = prose, at = { 1, 27 }, keys = 'yas' },
  { name = '2yis', lines = prose, at = { 1, 0 }, keys = '2yis' },
  { name = 'vasas', lines = prose, at = { 1, 6 }, keys = 'vasasy' },
  { name = 'cis', lines = prose, at = { 2, 8 }, keys = 'cisX<Esc>' },
  { name = 'dit in bold', lines = html, at = { 1, 14 }, keys = 'dit' },
  { name = 'dat in bold', lines = html, at = { 1, 14 }, keys = 'dat' },
  { name = '2yit in bold', lines = html, at = { 1, 14 }, keys = '2yit' },
  { name = 'yit with > in quotes', lines = html, at = { 2, 18 }, keys = 'yit' },
  { name = 'dit over lines', lines = html, at = { 2, 4 }, keys = '3dit' },
  { name = 'vitit', lines = html, at = { 1, 14 }, keys = 'vitity' },
  { name = 'vY', lines = four, at = { 1, 3 }, keys = 'vY' },
  { name = 'vjlY', lines = four, at = { 1, 6 }, keys = 'vjlY' },
  { name = '<C-v>jlY', lines = four, at = { 1, 2 }, keys = '<C-v>jlY' },
  { name = 'M so=5 at the end', lines = sixty, at = { 56, 4 }, view = { height = 10, top = 51 }, keys = 'M', set = 'scrolloff=5' },
}

-- Quiet: "3 fewer lines" would land in what's printed, as would a
-- line number typed after `:`, which `silent` keeps out.
vim.o.report = 9999

local out = {}
local function say(text)
  out[#out + 1] = (text:gsub('%z', '^@'))
end

for _, case in ipairs(cases) do
  vim.cmd('enew!')
  -- Window options carry over from case to case: `smoothscroll` too.
  vim.cmd('set nostartofline nosmoothscroll scrolloff=0')
  vim.wo.scroll = 0
  vim.o.columns = (case.view and case.view.columns) or 80
  if case.set then
    vim.cmd('set ' .. case.set)
  end
  vim.bo.buftype = 'nofile'
  vim.fn.setreg('"', '')
  vim.api.nvim_buf_set_lines(0, 0, -1, false, case.lines)
  if case.view then
    vim.api.nvim_win_set_height(0, case.view.height)
    vim.fn.winrestview({
      topline = case.view.top,
      lnum = case.at[1],
      col = case.at[2],
      curswant = case.at[2],
      skipcol = 0,
    })
  else
    vim.api.nvim_win_set_cursor(0, case.at)
  end
  local keys = vim.api.nvim_replace_termcodes(case.keys, true, false, true)
  local ok, why = pcall(vim.cmd, 'silent normal! ' .. keys)
  -- As a redraw would, so the view's top is as Neovim settles it.
  vim.fn.line('w0')
  local cursor = vim.api.nvim_win_get_cursor(0)
  say(('== %s%s'):format(case.name, ok and '' or ('  (failed: ' .. tostring(why) .. ')')))
  say(('   cursor %d,%d  mode %s  top %d'):format(
    cursor[1], cursor[2], vim.fn.mode(), vim.fn.winsaveview().topline))
  say('   lines  ' .. vim.inspect(vim.api.nvim_buf_get_lines(0, 0, -1, false)))
  say(('   reg    %s %s'):format(vim.fn.getregtype('"'), vim.inspect(vim.fn.getreg('"'))))
end

io.stdout:write(table.concat(out, '\n') .. '\n')
vim.cmd('qa!')
