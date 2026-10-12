; Not a real language's: JSON's pieces, as if they were code, to test
; finding them. A pair is a function, and its value the body; an object
; is a class; an array's items are arguments.

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
