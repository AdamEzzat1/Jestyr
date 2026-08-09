# Jestyr grammar (as implemented)

This is an EBNF-style description of the syntax **the parser in `src/parser.rs`
actually accepts today** — not the language `jestyr-design.md` describes. Where
the two disagree, this file follows the code, and `DESIGN-STATUS.md` is the map
of which designed features exist at all.

It is documentation, not a generator input. Jestyr's parser is hand-written
recursive descent with a Pratt expression core, mirrored by a self-hosted parser
in `examples/std/parser.jtr` held byte-exact against it by the P2 goldens. A
generated parser would have to be mirrored twice and would not survive that
discipline. If you want a *grammar oracle* — an independent check that this file
and the parser agree — see [Conformance](#conformance) below.

## Notation

```
a b        sequence                 a | b      alternation
( a )      grouping                 [ a ]      optional
{ a }      zero or more             'x'        literal token
UPPER      token class from the lexer (IDENT, INT, STR, …)
```

Tokens come from `src/lexer.rs`. Whitespace and comments are **trivia**: they are
discarded before the parser sees anything, so they never appear below. Doc
comments (`///`, `//!`, `/** */`, `/*! */`) are collected into a side table and
are likewise invisible to the grammar — a comment can never change how code
parses. See `docs/comments.md`.

**Newlines are not tokens.** Statement boundaries are structural, not
line-based; the consequences are real and are discussed in
[Statement boundaries](#statement-boundaries).

## Compilation unit

```
Module    = { Item } EOF

Item      = { Attr } [ 'pub' ] ItemBody
ItemBody  = Fn | Enum | Const | Struct | Record | Union
          | Distinct | Extern | Import | Trait | Impl

Attr      = '@' IDENT [ '(' [ Expr { ',' Expr } ] ')' ]
```

`pub` is accepted on every item form but is only meaningful on those the module
system exports. Attributes are validated per target *after* parsing (so
`@must_use` can see the signature); `distinct`, `import` and `impl` reject
attributes outright.

## Items

```
Fn        = 'fn' IDENT [ Generics ] Params [ '->' Type ] [ ErrorSet ]
            { Contract } Block
Generics  = '[' IDENT [ ':' IDENT ] { ',' IDENT [ ':' IDENT ] } ']'
Params    = '(' [ Param { ',' Param } ] ')'
Param     = [ 'comptime' ] [ Conv ] ( 'self' | IDENT ) [ ':' Type ] [ Refine ]
Conv      = 'read' | 'mut' | 'out' | 'take'
Contract  = ( 'requires' | 'ensures' ) Expr
ErrorSet  = '!' '{' [ IDENT { ',' IDENT } ] '}'

Struct    = ( 'struct' | 'record' | 'union' ) IDENT [ CtorParams ] StructBody
StructBody= '{' { { Attr } [ 'pub' ] Field | Method } '}'
Field     = IDENT ':' Type [ '=' Expr ]
Method    = Fn

Enum      = 'enum' IDENT [ CtorParams ] '{' [ Variant { ',' Variant } ] '}'
Variant   = IDENT [ '(' [ Field { ',' Field } ] ')' ]

Const     = 'const' IDENT [ ':' Type ] '=' Expr
Distinct  = 'distinct' IDENT '=' Type
Extern    = 'extern' STR 'fn' IDENT Params [ '->' Type ]
Import    = 'import' STR [ '=' STR ]
Trait     = 'trait' IDENT '{' { TraitMethod } '}'
Impl      = 'impl' [ Generics ] IDENT 'for' Type '{' { Fn } '}'
```

`CtorParams` is the generic-constructor form (`struct Vec(comptime T: type)`);
it reuses `Params`. `Import`'s optional `= STR` is the content-hash pin
(`import "x" = "<sha256>"`).

## Types

```
Type      = '*' [ 'mut' | 'const' ] Type          -- raw pointer
          | 'indirect' Type                       -- sugar for '*' Type
          | '[' ']' Type                          -- slice
          | '[' Expr ']' Type                     -- array, Expr constant
          | '&' '[' IDENT ']' Type                -- region reference
          | '&' Type                              -- genref
          | 'type'                                -- the type of types (comptime)
          | 'fn' '(' [ Type { ',' Type } ] ')' [ '->' Type ]
          | 'dyn' IDENT
          | IDENT [ '(' Type { ',' Type } ')' ]    -- named / generic application
          | IDENT '.' IDENT                        -- module-qualified
```

## Statements

```
Block     = '{' { Stmt } '}'
Stmt      = 'return' [ Expr ]
          | ( 'let' | 'var' ) IDENT [ ':' Type ] [ '=' Expr ]
          | BlockLed
          | Expr

BlockLed  = If | Match | Unsafe | Comptime | Concurrent | Select
          | Region | For | While | Loop | Block
```

A **block-led** expression in statement position is parsed as a complete
statement: a trailing operator cannot extend it. `if c { 1 } else { 2 } + 3`
in statement position is an `if` statement followed by `+3`, not an addition —
the same rule Rust uses. In *expression* position the same forms are ordinary
expressions and do combine with operators.

## Expressions

The precedence ladder, loosest to tightest. Each level is a distinct function in
`src/parser.rs`; the binary tier is Pratt-driven from the table in `bin_op`.

```
Expr        = Assignment
Assignment  = Catch [ AssignOp Assignment ]                  -- right-assoc
AssignOp    = '=' | '+=' | '-=' | '*=' | '/=' | '%=' | '&=' | '|=' | '^='

Catch       = Binary(0) [ 'catch' [ '|' IDENT '|' ]
                          ( 'return' IDENT | Catch ) ]       -- right-assoc

Binary(bp)  = Unary { BinOp Binary(rbp) }                    -- Pratt, see table
Unary       = ( '-' | '!' | 'not' | '~' | '&' ) Unary | Cast -- right-assoc
Cast        = Postfix { 'as' Type }                          -- left-assoc
Postfix     = Primary { '.' IDENT | '(' Args ')' | '[' Expr ']' | '.*' | '?' }
```

### Binary operator table

Binding powers are `(left, right)` from `bin_op` in `src/parser.rs`. All binary
operators are **left-associative** (`right = left + 1`). Ranges sit at the
loosest binary level and build a distinct node rather than a `Binary`.

| level | operators | binding power |
|---|---|---|
| range | `..` `..=` | 5 / 6 |
| logical or | `or` | 7 / 8 |
| logical and | `and` | 9 / 10 |
| comparison | `==` `!=` `<` `<=` `>` `>=` | 11 / 12 |
| bitwise or | `\|` | 13 / 14 |
| bitwise xor | `^` | 15 / 16 |
| bitwise and | `&` | 17 / 18 |
| shift | `<<` `>>` | 19 / 20 |
| additive | `+` `-` | 21 / 22 |
| multiplicative | `*` `/` `%` | 23 / 24 |

Comparison being left-associative means `a < b < c` parses as `(a < b) < c` and
is rejected later by the type checker, not by the grammar.

`catch` deliberately binds **looser than every binary operator but tighter than
assignment**, so `let v = read(p) catch 0` groups as `v = (read(p) catch 0)`.

### Primary expressions

```
Primary   = INT | FLOAT | STR | CHAR | FSTR | 'true' | 'false' | 'null'
          | IDENT | 'self' | 'Self' | '_'
          | '(' Expr ')'
          | '[' Expr ';' Expr ']'                  -- array repeat
          | '[' [ Expr { ',' Expr } ] ']'          -- array literal
          | StructLit | GenStructLit
          | Closure | Spawn | Await | ParFor
          | Attr
          | BlockLed

StructLit    = IDENT '{' [ FieldInit { ',' FieldInit } ] [ '..' Expr ] '}'
GenStructLit = IDENT '(' Type { ',' Type } ')' '{' … '}'
FieldInit    = IDENT ':' Expr | IDENT              -- shorthand
Closure      = '|' [ IDENT { ',' IDENT } ] '|' Expr
Spawn        = 'spawn' Expr
Await        = 'await' Expr
ParFor       = 'par' 'for' IDENT 'in' Expr 'reduce' '(' Expr ')' Block
```

`par` is a **contextual keyword** — an identifier everywhere except directly
before `for`. `select` is reserved.

A bare `IDENT {` is a struct literal *except* inside a control-flow header,
where the parser sets a `no_struct` flag so the `{` opens the body block
(`if x { … }` is not a struct literal). This is the same disambiguation Rust
performs.

### Control flow

```
If        = 'if' NoStructExpr Block [ 'else' ( If | Block ) ]
Match     = 'match' NoStructExpr '{' { MatchArm } '}'
MatchArm  = Pattern [ 'if' Expr ] '=>' Expr [ ',' ]
For       = 'for' ForHead Block
ForHead   = [ Conv ] IDENT 'in' Expr        -- iteration
          | Expr                             -- condition ("while")
Region    = 'region' IDENT Block
Unsafe    = 'unsafe' Block
Comptime  = 'comptime' Block
Concurrent= 'concurrent' Block
Select    = 'select' '{' { SelectArm } '}'
```

Jestyr has one loop keyword. `for` with a `binding in …` head iterates; `for`
with a bare expression head is the `while` form. `while` and `loop` are
**reserved** and parse as a `for` with a diagnostic, so the error is "use `for`"
rather than a cascade.

### Patterns

```
Pattern     = PatternAtom { '|' PatternAtom }        -- or-pattern
PatternAtom = '_' | Literal | IDENT
            | IDENT '(' [ Pattern { ',' Pattern } ] ')'   -- tuple variant
            | IDENT '{' [ FieldPat { ',' FieldPat } ] [ '..' ] '}'
FieldPat    = IDENT [ ':' Pattern ]
```

## Statement boundaries

The lexer discards newlines, so statement boundaries are purely structural. One
consequence is documented in `src/parser.rs` and worth restating:

```
f
(x)
```

parses as the call `f(x)`, because nothing separates the two lines. The same
applies to a line starting with `(`, `[`, `-`, `&`, or `*` after an expression
statement. This is a known sharp edge, not an accident; the trade-offs and the
options for changing it are written up in
[docs/frontend-roadmap.md](frontend-roadmap.md#8-newline-and-statement-boundaries).

## Where this grammar is approximate

Stated plainly, because a grammar that overstates its own precision is worse
than none:

- **Attribute arguments** are shown as `Expr`, but each attribute has its own
  arity and argument-shape rules enforced in `src/attrs.rs`, not in the parser.
- **`Refine`** (parameter refinements, `i: usize in 0..len`) is written as a
  placeholder; the accepted forms are narrower than `Expr`.
- **Contracts** (`requires` / `ensures`) are parsed as expressions but only a
  subset is meaningful to the obligations pass.
- **Const-expression positions** (array lengths, attribute arguments) accept the
  full `Expr` grammar syntactically and are narrowed later by CTFE.
- **`select` arms** and channel syntax are summarised rather than specified.
- **Generic application** in type position (`IDENT '(' Type … ')'`) and generic
  struct literals share a surface that the parser disambiguates with lookahead
  in ways this grammar flattens.
- **Error recovery productions** are not described at all. The parser accepts
  malformed input by recording a diagnostic and continuing; the trees it builds
  in those cases are intentionally not part of the language.
- **Operator methods**: `+` on user types is resolved in typeck, not here.

Where this file and `src/parser.rs` disagree, the parser is right and this file
is a bug.

## Conformance

`src/proptests.rs` contains a `grammar_conformance` module: a table of snippets,
one or more per production above, each asserted to parse cleanly. It is a
tripwire, not a proof — it catches "a production silently stopped parsing",
which is the failure mode a hand-written parser actually has. A matching table
of *invalid* snippets asserts that each is rejected with a diagnostic and
without hanging.

Adding a production to this file without adding a snippet to that table is the
kind of drift the table exists to prevent.
