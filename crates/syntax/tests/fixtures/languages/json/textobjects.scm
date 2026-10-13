; Not a real language's: JSON's pieces, as if they were code, to test
; finding them. A pair is a function, and its value the body; an object
; is a class; an array's items are arguments.
;
; The patterns for an array's items are adapted from those for C's
; parameter lists in nvim-treesitter-textobjects' queries/c/textobjects.scm
; at 851e8653, Copyright the nvim-treesitter-textobjects contributors,
; under the Apache License 2.0: see NOTICE at the root of the repository.

(pair) @function.outer

(pair
  value: (_) @function.inner)

(object) @class.outer

(object
  .
  "{"
  _+ @class.inner
  "}")

(array
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(array
  .
  "["
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)
