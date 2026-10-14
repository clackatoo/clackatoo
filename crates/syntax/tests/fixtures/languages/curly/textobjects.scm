; curly's pieces. A function's parameters are found as GCSL's are, each
; taking the comma after it, or the last the comma before it: so a last
; parameter followed by a comma is matched both ways, as `.` passes over
; the comma between it and the `)`. A call's arguments are found as the
; JSON fixture's array items are, each taking the comma before it, or the
; first the comma after it.
;
; The patterns for arguments are adapted from those for C's parameter lists
; in nvim-treesitter-textobjects' queries/c/textobjects.scm at 851e8653,
; Copyright the nvim-treesitter-textobjects contributors, under the Apache
; License 2.0: see NOTICE at the root of the repository.

(function) @function.outer

(function
  body: (block
    .
    "{"
    .
    (_) @function.inner
    (_)? @function.inner
    .
    "}"))

(parameters
  (parameter) @parameter.inner)

(parameters
  (parameter) @parameter.outer
  .
  "," @parameter.outer)

(parameters
  "," @parameter.outer
  .
  (parameter) @parameter.outer
  .
  ")")

(parameters
  "("
  .
  (parameter) @parameter.outer
  .
  ")")

(arguments
  "," @parameter.outer
  .
  (_) @parameter.inner @parameter.outer)

(arguments
  .
  "("
  .
  (_) @parameter.inner @parameter.outer
  .
  ","? @parameter.outer)
