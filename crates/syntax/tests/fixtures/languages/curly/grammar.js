/**
 * Not a real language: a small one with braces, for testing indenting and
 * text objects. It's shaped like Rust: `if c { } else { }` is an `if` with
 * an `else_clause`, an `if` can be a value, and a function's parameters and
 * a call's arguments may end with a comma.
 *
 * grammar.wasm is built from this with tree-sitter 0.27, in a folder of its
 * own: `tree-sitter generate`, then `tree-sitter build --wasm -o grammar.wasm`.
 */
module.exports = grammar({
  name: 'curly',

  extras: $ => [/\s/],

  word: $ => $.identifier,

  rules: {
    source_file: $ => repeat($._statement),

    _statement: $ => choice($.function, $.let, $.if, $.expression_statement),

    function: $ => seq(
      'fn',
      field('name', $.identifier),
      field('parameters', $.parameters),
      field('body', $.block),
    ),

    parameters: $ => seq('(', optional(seq(commaSep1($.parameter), optional(','))), ')'),

    parameter: $ => $.identifier,

    let: $ => seq('let', $.identifier, '=', $._expression, ';'),

    expression_statement: $ => seq(choice($.identifier, $.number, $.call), ';'),

    _expression: $ => choice($.identifier, $.number, $.call, $.if),

    call: $ => seq(field('function', $.identifier), field('arguments', $.arguments)),

    arguments: $ => seq('(', optional(seq(commaSep1($._expression), optional(','))), ')'),

    if: $ => seq(
      'if',
      field('condition', $.identifier),
      field('consequence', $.block),
      optional(field('alternative', $.else_clause)),
    ),

    else_clause: $ => seq('else', choice($.block, $.if)),

    block: $ => seq('{', repeat($._statement), '}'),

    identifier: _ => /[a-z_][a-z0-9_]*/,

    number: _ => /\d+/,
  },
});

function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)));
}
