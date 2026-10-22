# ts_r_codegen

AST → JavaScript (or TypeScript) printer with V3 source maps, used after `ts_r_transform` by
ts-r-run, the dev server and (later) the bundler. Root rules: [../../AGENTS.md](../../AGENTS.md).
Design: [../../docs/design/0002-transform-and-codegen.md](../../docs/design/0002-transform-and-codegen.md);
AST: [../../docs/design/0001-ast.md](../../docs/design/0001-ast.md).

## API in one breath

```rust
let out = ts_r_codegen::print(&ast, src, &CodegenOptions::default());
// or print_with(&ast, src, &opts, &PrintHints { erased, prelude, single_line, multi_line, map_ranges, moved_comments, .. })
// after a transform (`..Default::default()` for the hints you do not have)
out.code; out.map /* Option<SourceMap>: to_json, to_data_url, decode */; out.specifiers;
```

`print` is TypeScript output and `print_with` JavaScript output; `print_as(.., Syntax)` (or
`Printer::with_syntax`) picks explicitly. `Syntax` only decides modifiers (see Invariants).

`CodegenOptions`: `layout` (`Readable` | `Compact` | `Tsc`), `comments`, `source_map:
Option<SourceMapOptions>`, `indent_width`, `newline`, `collect_specifiers`. `print_node` prints
one node (tests, diagnostics). `shape::shape(&ast)` is the span-free dump used by every
round-trip and oracle comparison. `max_depth` / `with_stack_for` run deep trees on a big stack.

Files: `printer.rs` (state, tokens, spacing, comments, program), `stmt.rs` (statements,
declarations, classes, functions, modules), `expr.rs` (expressions, precedence, parentheses,
arrows, JSX), `types.rs` (TypeScript types), `literals.rs` (string/number/template/regex
printing from raw text or cooked values), `sourcemap.rs` (VLQ, builder, line lookup, base64),
`shape.rs`.

## Invariants

- **Never edits source text.** Parsed literals print their raw source slice; everything else
  is printed from the tree. One `Layout::Tsc` exception, as tsc does: a legacy octal or
  leading-zero decimal literal (`055`, `09.5`) prints its value (`45`, `9.5`).
- **Synthesized nodes have empty spans** (`start == end`, set by transforms). Their literals
  print from cooked values; their span start is only a source-map position (`0..0` =
  unmapped). Never index the source with a synthesized node's span. In `Layout::Tsc` a
  synthesized string is double-quoted with non-ASCII escaped (`"é"`), like tsc's.
- **Round trip.** `parse(print(parse(x)))` has the same `shape` as `parse(x)` in both layouts,
  and printing is idempotent. Parentheses come from precedence and position (never from the
  source), so transformed trees print correctly.
- **Modifiers.** `Syntax::TypeScript` prints every modifier in the tree, misplaced ones too
  (`{ public a: 1 }`, `async class C`, `static const x`, `override` on a signature,
  `type T<public U>`), in canonical order (`const`/`in`/`out` last). `Syntax::JavaScript`
  follows tsc 7.0.2's emit (observed): TypeScript-only modifiers (accessibility, `readonly`,
  `abstract`, `declare`, `override`, `const`, `in`, `out`) are never printed, object literal
  properties print none, parameters no `export`; misplaced JavaScript modifiers stay
  (`async var x`, `static const x`). Type-only nodes always print all of theirs.
- **ASI-safe**: every statement ends in `;` or `}`; no line break between `return`/`throw`/
  `yield`/postfix operators and their operand. Compact layout drops the `;` right before a `}`
  (never an empty statement's) and keeps the last one of the output, writes keyword spaces only
  where tokens would merge (`return(a)`, `let{a}=b`, `return #x in y`), and prints a lone
  plain arrow parameter bare (`x=>x`). Pending `;`/space are resolved by the next token, and
  source-map segments recorded meanwhile wait for it.
- **Comments**: readable keeps statement-level comments; compact keeps legal and
  annotation comments; nothing inside `PrintHints::erased` is printed.
- **`Layout::Tsc`** aims at byte equality with tsc 7's JavaScript emit (record 0012,
  "Equivalence gate"). Every rule is observed tsc 7 output, never read from tsc's source;
  `tests/tsc_layout/` holds inputs and tsc 7's output for each rule. 4-space indent, no blank
  lines, and line breaks come from the source spans (`source_breaks`, `op_breaks`):
  - Function bodies (functions, arrows, methods, accessors, static blocks) written on one line
    stay on one line (`{ return 1; }`, empty `{ }`) unless a transform added a statement.
    Statement blocks (`if`, loops, labels, `try`, bare blocks) and class bodies always break;
    an empty one-line statement block is `{ }`, a multi-line empty block `{\n}`.
  - `else`, `catch` and `finally` start a new line. A non-block embedded statement (also `;`)
    goes on its own indented line; `do x(); while (y);` breaks before `while`. A case's lone
    statement written on the case line stays there.
  - Object and array literals keep a source break after `{`/`[` (then also before the close),
    between elements and before the close; elements sit one level in. Trailing commas and
    holes are kept (`[, 1, , 2,]`); empty literals print `{}`/`[]`. Patterns, arguments,
    parameters, declarator lists, imports and templates never break; binding patterns add
    no indentation (a class in a pattern's computed key keeps the outer level).
  - A case's lone block written on the next line stays on its own line (`case 1:\n
    {`). A class keeps its empty `;` members (`get x() { };` → `get x() { }\n    ;`).
  - Bigint literals: separators dropped, binary and octal in decimal, hex in lower case
    (`0b101n` → `5n`, `0xFFn` → `0xffn`). Decorators print as written (`@x().y`, `@new x`),
    with the comments between and after them on their own lines.
  - A source break before or after a binary or assignment operator, `?` or `:`, before `.`/
    `?.` and after `,` in a comma expression is kept, each adding one indentation level. A
    break before an operator is measured from the token in front of it (so a `)` of erased
    `as const` parentheses counts), and comments on the lines before a broken `.` stay there.
  - `PrintHints::strip_numeric_separators` (no `target`, or one below ES2021): a numeric
    literal with `_` prints by its value (`1_000` → `1000`).
  - Multi-line block comments are re-indented by the column shift (tabs to 4-column stops,
    never below column 0, trailing spaces trimmed); line comments keep trailing whitespace.
  - JSX: `<div />` without attributes, `<div id="x"/>` with. An expression container
    (attribute value or child, not a spread) indents its contents one level; a multi-line
    comment in an empty container is re-indented from the writer's indent (each line keeps
    its own indent past the source line's).
  - Synthesized layout comes from the transform (`PrintHints::single_line` / `multi_line`,
    sorted node ids): blocks and embedded statements tsc builds on one line (`get x() {
    return this.#x; }`, `static { _classThis = this; }`, `get: function () { return m.x; }`,
    `if (_metadata) ...;`) and arrays it builds one element per line (`__decorate([\n
    dec\n], C)`). Without a hint a synthesized block breaks and a synthesized array does not.
  - A synthesized arrow with a synthesized parameter prints `x => ...`; an `async` arrow's
    single parameter is always parenthesized (`async (x) => x`, also when written bare).
  - A tag and its template are separated by a space (`tag `x``), as tsc always does.
  - Names written with `\u` escapes print as written when they still spell the node's name
    (`\u0078`, `#x\u{78}`); a misspelt meta property prints as written (`import.metal`).
  - Decorators written after `export` print `export ` on its own line, then one decorator
    per line (`export \n@dec\nclass C {`); `@a export @b class` splits around `export`.
  - A source trailing comma is kept in named imports and exports (`{ a, b, }`), also when the
    specifiers after the last printed one were erased; a comment after a
    trailing comma in a multi-line literal trails it after a space (`x, // c`). A
    specifier's leading comments print through `expr_leading`, so one on its own source
    line goes on its own line after the `, ` (`export { a, \n// c\nb, }`), and its trailing
    ones before the `,` stay (`b /* c */, c`).
  - Comments after erased code on its last line are dropped with it (`declare function
    f(); // c`). A synthesized statement whose source-map start is not past the next original
    statement of its list stands in for an original one: the comments before that start lead
    it (`// c` before `import x from "y"` leads `const y_1 = require("y")`), and the last
    stand-in for an original takes the original's same-line comments
    (`const y_1 = require("y"); // c`, `})(E || (E = {})); // c`), skipping erased code
    after it; not at the end of a nested list (moved statements) and not over erased code
    alone. A span start of 0 is a real position only when the source starts with code
    (`code_at_zero`). Transform fixtures: `ts_r_transform/tests/tsc_layout/`; program
    fixtures: `ts_r_emit/tests/tsc_layout/`.
  - `/// <reference ...>` and `/// <amd-...>` directives at the top of the file survive the
    erased declaration they lead (printed with the next statement's comments), and come
    before the `export {};` of a module whose statements were all erased.
  - Imports a transform adds at the top (`react/jsx-runtime`, `tslib`) print before the
    file's detached comments and triple-slash directives.
  - Moved statements (`PrintHints::moved_comments`, `(statement, original span)` sorted by
    node): a field initializer moved into the constructor takes the field's leading, inner
    and same-line trailing comments (`moved_statement`); the linear pass hides them there
    (`comment_hidden`). Synthesized static blocks print on one line.
  - Comments between `if (...)` and an embedded statement lead the statement; comments before
    `else` and before the `)` of an `if` condition stay where they are.
  - A break after `=` is not kept for a moved `this.x = v`; an erased `<T>x` assertion's
    breaks are measured from its `<`.
  - The file's detached comments (up to the last one a blank line follows) print right after
    the prologue, before helpers and `Object.defineProperty(exports, ...)`, and survive when
    the first statement is erased.
  - Comments after `{` on its line: kept in statement blocks (also `{ /* c */ }`), `switch`
    and after `case x:`; dropped in function bodies (also `{ /* c */ }` and an empty
    multi-line body `{ // c\n}`), class bodies and multi-line objects. A leading block comment followed by code on its line stays there
    (`/* c */ f();`).
  - Comments inside statements (`expr_leading`/`expr_trailing`, only inside an original
    statement or a stand-in, see above): a node's leading comments are those in the trivia
    right before it (`/* c */ x`; `// c` then a line break; a comment the source starts on a
    new line starts one); its trailing ones follow it on its line, across erased code
    (`a /* c */, b`, `b /* c1 */ /* c2 */` for `b /* c1 */: T /* c2 */`), never at the end of
    the statement or class member (they go after its `;`). After a `,` they trail the comma
    (`x, // c`). After `(` of a parenthesized expression or empty arguments/parameters, `.`,
    `[`, a prefix operator or `new` they trail the token with no space after
    (`( /* c */x`, `f( /* c */)`), and so after `...` (`... /* c */x`) when on its line.
    A binary operator's own comments print around it (`/* c */ in /* d */`). Comments
    touching no node (`function /* c */ ()`,
    `) /* c */ {`) are dropped. Comments before the close of a multi-line literal print one
    level in. Array spread elements keep their leading comments. Trailing comments print
    after the outermost expression ending at that position (`Printer::expr_ends`), so they
    follow its source-map end.
  - Measured (`emit_oracle --emitter transpile --layout tsc`, js outputs byte-equal): see
    Testing.
- **Source maps**: `Readable`/`Compact`: one segment per token start, generated positions
  non-decreasing, exact duplicates dropped. Columns are UTF-16 code units on both sides, as
  the spec requires (source via `LineLookup`, generated counted while printing).
- **`Layout::Tsc` source maps** (`Printer::tsc_maps`) follow tsc 7's segments (observed in
  the `.sourcemap.txt` baselines and probes, never read from tsc's source):
  - The start and end of every original node tsc has a node for (`map_start` /
    `map_end`): statements, expressions (inside parentheses a precedence adds), names,
    parameters, declarators, the declaration list (`let` to the last declarator), patterns
    and elements, object members, property keys, class members, heritage clauses (mapped
    before the space in front of them), catch and case clauses, import clauses, named
    imports/exports (`{` to after `}`), `* as ns`, each modifier written in the source
    (`export`, `default`, `static`, `async`, ...). `constructor` has no name node.
  - Tokens with their own segments (start and end, `map_token`): a statement block's `{`
    (not a function body's), every block's `}` (a missing one at the block's end, its end
    one past it), a case block's `{` and `}`, `=>`, `?` and `:` of a conditional, a class's
    empty `;` member. Keywords, other punctuation and class/object braces have none.
  - Generated lines count U+2028, U+2029 and a lone `\r` in copied text as line breaks, as
    source lines do.
  - Comments copied from the source: start and end.
  - An end recorded at the start of a line is the end of the previous line (the line
    break comes after it). An elision maps its start and then its end, which is earlier.
  - Merging (`SourceMapBuilder::add_tsc`): a segment at the generated position of the
    previous one replaces it unless its source position is earlier (then both stay).
    `names` is always empty.
  - Synthesized nodes are mapped only through `PrintHints::map_ranges` (sorted by node id,
    looked up by `Printer::map_range_of`; a range also overrides an original node's span, e.g.
    a `(x as T)` stripped of its parentheses maps from `(`): `map_start`/`map_end` use the range, a synthesized
    block maps its `}` to the range's `}`, and a synthesized declaration list maps (from
    `var` to its last declarator) through its first declarator's range. Without a range a
    synthesized node has no segments. `Printer::map_muted` prints without segments (tsc
    leaves `constructor` unmapped); turning `tsc_maps` off instead would map the other
    layout's way.
  - A generator's `*` before an original name is mapped (start and end); a generator
    method's `*` too.
  - E6 additions (observed on real repositories, record 0012 E6 notes): `?.` maps its start
    and end; a tagged template maps the template's start and end and each `}` before a
    middle or tail; `new.target` maps at the name; an arrow's `=>` uses the body's range.
  - `NO_MAP` (`u32::MAX`) on either side of a range means no segment there. A synthesized
    statement whose range is `NO_MAP` on both sides (`Printer::unmapped_insert`: CommonJS
    preamble statements, a static field's `static { }` block) stands in for no original
    statement, so it takes no comments.
- No panics on any tree the parser or a transform can build; deep trees use the big-stack
  thread from `with_stack_for`.

## Testing

- `cargo test -p ts_r_codegen`: unit tests (VLQ, base64, literals) and `tests/print.rs`
  (round trips of tricky syntax, TS and TSX, every repo fixture; precedence and statement-
  start parentheses on synthesized trees; valid inputs that need none, like `for (v++ of x)`
  and `@x!`; misplaced modifiers per `Syntax`; comments per layout; compact output;
  source-map positions and data URLs).
- Corpus round trip (both layouts):
  `scripts/limit.sh -- cargo run --release -p ts_r_codegen --example roundtrip_corpus -- <dir>... [--show N]`.
  Last run: private-app 357/357; TypeScript `tests/cases` 17,744 ok, 0 mismatches,
  1,468 skipped (syntax errors; the parser reports more of tsc's since alpha.3).
- `cargo run -p ts_r_codegen --example print -- <file> [--compact]` prints a file back.
- `tests/tsc_layout.rs`: each `tests/tsc_layout/<name>.ts(x)` prints byte-for-byte as the
  `.js(x)` next to it under `Layout::Tsc`; the expected files are tsc 7 output at
  `target: esnext` with the `"use strict";` line removed. Regenerate them with tsc 7 under
  `scripts/limit.sh`, never by hand.
- tsc layout against the conformance baselines:
  `scripts/limit.sh -- cargo run --release -p ts_r_check --example emit_oracle -- --threads 4 --emitter transpile --layout tsc`.
  Last run (js outputs byte-equal): ALL 10,721 / 14,476 (74.1%, readable layout 40.1%);
  ES2022+ 1,426 / 2,159 (66.0%, readable 42.2%). Shape-equal-only buckets left: comments
  or tokens 494, line breaks and spacing 351.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_lexer` (identifier tables). Dev: `ts_r_parser`,
`ts_r_testing`. No external crates.

## Gaps

- Synthesized comments (`Ast::synthetic_comments`, e.g. `0 /* E.A */`) are printed only
  around expressions (`expr`, and a number before `.`), not statements; dropped without
  `comments` and in the compact layout.
- `Layout::Tsc` gaps: comments after `x!`, in template spans, inside import/export clauses
  (`import /*2*/ D /*3*/, { ... }`) and after an erased `as T` and its removed parentheses
  (`x /*a3*/ /*a4*/.y`); a stand-in moved into a constructor (`this[_a] = 0; // c`) does
  not take its field's comment; triple-slash directives before an erased CommonJS
  `export {}` are kept (tsc drops them); leading comments of some nodes (`catch ( /*c*/x)`)
  print `/*c*/ x`. Template literals copy the source's line breaks (tsc writes `\n` for
  `\r\n` in some tagged templates).
- `Layout::Tsc` source maps of synthesized code the transforms give no range: the TC39
  decorator transform (esDecorators-classDeclaration-sourceMap), `super(...arguments)` in a
  synthesized constructor. JSX has no tsc source-map segments yet (tsc maps every JSX tag,
  attribute and child). tsc's maps of some JSX children count generated lines differently
  (jsxFactoryIdentifierAsParameter and two more).
- No line-length based wrapping; compact output is one line per statement list.
- `names` only records identifiers whose printed name differs from their source text
  (renamed by a transform); `Layout::Tsc` records none, as tsc.
- Misplaced modifiers in JavaScript output differ from tsc where `ts_r_transform` rewrites the
  node first: it drops `async` from classes, class fields and parameters (tsc keeps
  `async class C`, `async x = 1`, `f(async a)`) and emits `var E` for `async enum E` (tsc:
  `async var E`). All are grammar errors.
