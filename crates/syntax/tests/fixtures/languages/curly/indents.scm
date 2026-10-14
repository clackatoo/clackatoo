; curly's rules for indenting, in nvim-treesitter's captures, as GCSL's
; are: the lines inside a block, a list of parameters or arguments, or a
; `let` go a step in, and the bracket closing it comes back. An `if`, with
; its `else`, opens one step, its blocks none of their own; an `else`
; starting a line is back at its `if`'s indent, as the `}` before it is.
[
  (parameters)
  (arguments)
  (let)
] @indent.begin

((block) @indent.begin
  (#not-has-parent? @indent.begin if else_clause))

((if) @indent.begin
  (#not-has-parent? @indent.begin else_clause))

(block
  "}" @indent.branch @indent.end)

(parameters
  ")" @indent.branch @indent.end)

(arguments
  ")" @indent.branch @indent.end)

(else_clause
  "else" @indent.branch)
