; JSON's rules for indenting, as nvim-treesitter's are: the lines inside
; an object or an array go a step in, and the bracket closing it comes back.
[
  (object)
  (array)
] @indent.begin

[
  "}"
  "]"
] @indent.branch
