; JSON's rules for indenting, as nvim-treesitter's are: the lines inside
; an object or an array go a step in, and the bracket closing it comes back.
;
; From nvim-treesitter's queries/json/indents.scm at cf12346a, Copyright the
; nvim-treesitter contributors, under the Apache License 2.0: see NOTICE at
; the root of the repository.
[
  (object)
  (array)
] @indent.begin

[
  "}"
  "]"
] @indent.branch
