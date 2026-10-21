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
-- (1-based line, 0-based byte), and `keys` in Vim's notation (`<Esc>`,
-- `<CR>`, `<C-v>`).
--
-- Gotchas (see the Acta memory `checking-vim-behaviour-headless`):
-- - `startofline` is off in Neovim, so `gg`, `G` and linewise operators
--   keep the column. clackatoo goes to the first non-blank only for `gg`
--   and `G` with a count: without one they go to column 0, and a linewise
--   yank keeps the column. 1d makes gg/G go to the first non-blank with
--   or without a count, as Vim's default startofline.
-- - A motion that fails inside `normal!` silently ends that case's keys.
-- - `.` carries over from earlier cases.
-- - Anything that waits for input (`:s///c`) hangs the run: keep `timeout`.

-- Eight lines, each indented by its number's parity, so the first
-- non-blank and the column kept are told apart.
local eight = {
  'one', '  two', 'three', '  four', 'five', '  six', 'seven', '  eight',
}

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
}

-- Quiet: "3 fewer lines" would land in what's printed.
vim.o.report = 9999

local out = {}
local function say(text)
  out[#out + 1] = (text:gsub('%z', '^@'))
end

for _, case in ipairs(cases) do
  vim.cmd('enew!')
  vim.bo.buftype = 'nofile'
  vim.fn.setreg('"', '')
  vim.api.nvim_buf_set_lines(0, 0, -1, false, case.lines)
  vim.api.nvim_win_set_cursor(0, case.at)
  local keys = vim.api.nvim_replace_termcodes(case.keys, true, false, true)
  local ok, why = pcall(vim.cmd, 'normal! ' .. keys)
  local cursor = vim.api.nvim_win_get_cursor(0)
  say(('== %s%s'):format(case.name, ok and '' or ('  (failed: ' .. tostring(why) .. ')')))
  say(('   cursor %d,%d  mode %s'):format(cursor[1], cursor[2], vim.fn.mode()))
  say('   lines  ' .. vim.inspect(vim.api.nvim_buf_get_lines(0, 0, -1, false)))
  say(('   reg    %s %s'):format(vim.fn.getregtype('"'), vim.inspect(vim.fn.getreg('"'))))
end

io.stdout:write(table.concat(out, '\n') .. '\n')
vim.cmd('qa!')
