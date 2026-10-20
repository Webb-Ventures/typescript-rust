# ts_r_parser

Recursive-descent parser with precedence climbing for all of TypeScript, TSX, JavaScript, JSX
and `.d.ts`, including ASI and error recovery. It produces a `ts_r_ast::Ast` and tsc-compatible
syntax diagnostics. Root rules: [../../AGENTS.md](../../AGENTS.md). AST design:
[../../docs/design/0001-ast.md](../../docs/design/0001-ast.md).

## API in one breath

```rust
let r = ts_r_parser::parse(src, SourceKind::Tsx); // or parse_path("a.d.ts", src)
r.ast;          // ts_r_ast::Ast (re-exported as ts_r_parser::ts_r_ast); root is a Program
r.diagnostics;  // Vec<Diagnostic>, sorted by position
```

Diagnostics are deduplicated as tsc does: scanner and parser errors form one stream, and a
parse error at the position of the error just before it (scanner or parser) is dropped. So
a stray `\` is TS1127 (scanner) only, not also TS1128. Scanner errors are always kept, so a
position can carry a parser and a scanner error (`'}' expected` and TS1160 at end of file).

`ts_r_parser::dump(src, kind)` returns the debug dump. `ts_r_parser::parse_json(src)` parses
a `.json` source file as tsc does: a program with one expression statement holding the value,
with tsc's JSON errors on it (TS1327 single-quoted or unquoted key or string, TS1328 a value
that is not a JSON literal, object or array, TS1136 a property that is not `"key": value`).

TypeScript syntax in `.js`/`.jsx` files, as tsc and typescript-eslint parse them (request R8):

```rust
let opts = ParseOptions { ts_in_js: TsInJs::Report }; // or Allow (no TS8xxx), Reject (default)
let r = ts_r_parser::parse_with(src, SourceKind::Js, opts);
```

Files:
- `stmt.rs`: statements, declarations, import/export.
- `expr.rs`: expressions, arrows, literals, templates, binding patterns.
- `class.rs`: classes, parameters, decorators.
- `types.rs`: types and type members.
- `modifiers.rs`: modifier keywords in every position (`ModCtx`), and the statement-level
  lookaheads for modifiers that lead to a declaration or are stray.
- `jsx.rs`: JSX.
- `lists.rs`: list contexts (`List`) and tsc's list recovery: element starts, terminators,
  the per-list error, `abort_list_or_skip`, and `parse_delimited` for `,`-separated lists.
- `semicolon.rs`: tsc's errors where a statement or class property ends without `;` (TS1434,
  TS1435 spelling suggestions, TS1437/TS1438, TS1441/TS1442).
- `js_ts.rs`: the TS8xxx pass for TypeScript syntax in a JavaScript file (`TsInJs::Report`).
- `jsdoc.rs`: `attach_jsdoc(&mut ast, src) -> Vec<Diagnostic>`, run by the checker on
  JavaScript files before binding. It desugars JSDoc into TypeScript syntax on the parsed
  tree: `@type` (variable/property/parameter annotation; a parenthesized cast becomes a
  `TypeAssertion`), `@param` (nested `o.a` builds a type literal, `[x]`/`T=` optional,
  `...T` is `T[]`), `@returns`, `@this` (a `this` parameter), `@template`, `@typedef`/`@callback`
  (type aliases inserted in the enclosing statement list), `@import` (a type-only import),
  `@satisfies`, `@extends`/`@implements`, and the accessibility/`@readonly`/`@override`
  modifiers. Types are parsed by the normal type parser (with `Parser::jsdoc` set) over a
  masked copy of the source (`/**` and leading `*` blanked), so spans are source offsets.
  The returned diagnostics are tsc's JSDoc syntax errors (`'}' expected`, ...; one per
  position, except `'from' expected` and `String literal expected` together), plus the tag checks tsc's checker makes that need only the comment
  (`check_param_names`: TS8024 a `@param` naming no parameter, TS8029 when the function
  uses `arguments`, TS8032 a nested name without its parent tag; TS8023 an `@augments`
  whose rightmost name differs from the `extends` clause's). Tag names
  may use `\u` escapes; a type that stops early (`{function(number)}`) still types the name
  after its `}`; a comment before `return f` applies to the returned function, one before
  `a = b = function` to the innermost value. Parameter names may follow stray `*`s and be
  backquoted; `??T` is two `?` prefixes. A parameter JSDoc makes optional (`{T=}`, `[a]`)
  followed by a required one is TS1016, and on a setter TS1051. A comment after a token
  on its own line (`f(); /** @type {T} */ let x`) documents only a parameter, type
  parameter, function expression, arrow, parenthesized expression or variable declarator
  starting right after it; otherwise it is ignored entirely (`@typedef` included), as in
  tsc 7. `@template in out T` takes variance modifiers, `const` and the other modifier
  keywords (the checker reports the misplaced ones; the type parameter's span starts at the
  first modifier);
  `@callback` reads `@this`. `@enum` types are not read (tsc 7 reports nothing in them).
  `@template {C}` with no name after the braces is a JSDoc TS1069 at the braces' end (at
  the `}` when `*/` follows with no space; at the next line's first character, the margin
  `*`, when the comment goes on past that line) and declares nothing.
  `@public`/`@private`/`@protected` on a `#name` member is a JSDoc TS18010 at each such tag.
  `@typedef Name` without braces or `@property` children takes its first `@type` child
  as its type; each later `@type` child is a JSDoc TS8033 just after its `}`.
  `@implements` without a type is a JSDoc TS1003 after the tag name (as `@augments`); a
  `@property` named `#x` is TS1003 at the `#` and declares nothing.
  `@this` on an arrow function is a JSDoc TS2730 at the tag name (and is not applied).
  `@readonly` on a method, accessor or constructor is a JSDoc TS1024 at the tag; on a
  constructor `@template` is TS1092 at the tag and `@returns {T}` TS1093 at `T`.
  A `@typedef {T}` without a name (none after the type, or the type stops early) is still
  an alias, named `(Missing)` at the next token after the type (the type's end at the
  comment's end), so script files declaring two report TS2300; tsc 7 reports
  `Identifier expected.` one character before that point as a syntax error of the file,
  checked or not (`attach_jsdoc_with_syntax`'s second list, which the checker adds to the
  file's parse errors), and again at it as a JSDoc error. The last `@import` of a comment
  is parsed with the source cut at the `*/`: a missing specifier there is `Expression
  expected.` at the end of the last token (after `'from' expected.` at the `*/` when a
  clause was read); a wrong token before it gets `'from' expected.` and `String literal
  expected.`; `@import x = require("m")` is not an import (both errors at `=`); errors
  after the specifier (`with` without `{`) are reported and keep the import.
  A `@typedef`'s children are the `@property`/`@prop`/`@template`/`@type` tags right after
  it; a later `@param`/`@returns` documents the host. A typedef of a type other than
  `Object`/`object` (or an array of it) has no children: a `@type` after it is the host's. A tag type may be a type predicate
  (`{x is T}`; the checker reports TS1228 outside return types). `@type` on a shorthand
  property is recorded on it.
- `grammar.rs`: statement grammar errors tsc's checker reports, deferred (jump targets,
  `return`, ambient statements, labelled declarations).

## Invariants

- JSDoc type syntax (`*`, `?T`, `T?`, `!T`, `T!`, `Array.<T>`) is accepted while
  `Parser::jsdoc` is set and, as tsc 7 does, in TypeScript source types too, where `*`,
  `?T`, `T?`, `!T` and `T!` are recorded in `Ast::jsdoc_syntax` for the checker's
  TS17019/TS17020/TS8020 (no parse error). Top-level `...T` and `T=` are JSDoc-only. In a
  tuple, `T?` is an optional element (not recorded, except under `...`; `[name: T?]` is not
  recorded either). A `?` followed by a token that starts a type (`|`, `&`) is a conditional
  type's `?`, never postfix `T?` (`S extends T ? | A : B`). `attach_jsdoc` only appends nodes and rewrites fields of existing
  nodes; every tag loop consumes the comment text or stops, and malformed JSDoc never panics
  (`pathological_comments_terminate`).
  Nested parameter/property paths are grouped before synthesis and built from the leaves
  up, sharing each prefix type. Nested type synthesis has a source-sized work budget
  (TS99003 on exhaustion); qualified names become member expressions iteratively.

- **tsc 7 is the spec.** What parses, and which error code is reported at which position, is
  checked by running `npx -y -p typescript@7 tsc --noEmit` on a tiny file. Never read
  TypeScript's source. `first_error_matches_tsc` in `src/tests.rs` pins observed codes and
  positions; add a row when you fix one.
- **Never panics, always terminates, always returns a tree.**
  - Missing pieces become zero-width `Missing` nodes.
  - Every list loop must consume a token or break. Use the `before = tok().start` progress
    check, as the existing loops do.
  - Every recursive entry point goes through `enter()`/`leave()`: statements, unary, LHS,
    `new`, types, JSX. Nesting is bounded by `MAX_DEPTH` and a native stack budget
    (`STACK_BUDGET`); past either, TS99001 is reported.
  - Two backstops catch loop bugs:
    - `MAX_STALL` consecutive `enter()` calls at one token.
    - More than `NODES_PER_BYTE` × source length (+1024) nodes.
    Either reports TS99003 and `halt()`s: skip to EOF, after which `aborting()` is true so
    every loop exits.
  - Regression tests: `pathological_inputs_terminate` (repeated garbage units, lone `#`,
    unterminated constructs, 20 KB each in every source kind) and
    `deep_nesting_is_bounded`. Add a unit there for every hang or blow-up you fix.
- **Linear time.**
  - Arrow-function speculation is memoised in `failed_arrows`, so `((((a))))` is not
    exponential.
  - Type-argument speculation is memoised in `failed_type_args`, so `a < b < c < ...` is not
    quadratic.
  - The modifier-chain lookahead is capped at `MAX_MODIFIER_CHAIN`, so `declare declare ...`
    is not quadratic.
  - Other speculation (`try_parse`, `lookahead`) is bounded by short-circuiting:
    `aborting()` stops loops once a speculative parse has failed.
- **Speculation is transactional.**
  - `checkpoint`/`rewind` restore the lexer, the AST arena, diagnostics, scratch, context and
    the depth flag.
  - `try_parse` keeps a result only if no error was reported. The exception is
    `grammar_error_at`, which reports a grammar error (e.g. TS1013) without failing the
    attempt, so `(...a,) => a` still parses as an arrow.
- **Context lives in `Ctx` bits** (`AWAIT`, `YIELD`, `DISALLOW_IN`, `AMBIENT`,
  `DISALLOW_CONDITIONAL_TYPES`, `IN_CONDITIONAL_TRUE`, `DECORATOR`, `TYPE`, ...). Set them with
  `with_ctx(add, remove, f)`. Function bodies, parameters and brackets clear the bits that do
  not cross them; see `NESTED_CLEAR` and `parse_params`.
- **Misplaced modifiers are modifiers.** Where tsc 7 parses a modifier keyword as a
  modifier but rejects it in the checker (`public class` in a namespace, `static` on a
  parameter or object member, `export` on a class member or a parameter), so do we, with no
  parse error;
  every one is recorded in `Ast::modifier_tokens`. Which keywords count, and which tokens
  may follow, depend on `ModCtx`; see the rules at the top of `modifiers.rs`. A statement
  modifier followed by a name that starts no declaration (`public x = 1`, `export foo;`) is
  TS1128 at the keyword, which is skipped, as in tsc.
- **A private name in an optional chain** (`a?.#x`, `a?.b.#x`, `a?.f().#x`; parentheses
  end the chain) is TS18030 at the name, a syntax error as in tsc 7, so unchecked
  JavaScript reports it too (`check_private_name_in_chain`).
- **`yield` outside a generator** starting an assignment expression is a yield expression
  when a name, keyword, private name or string/number/bigint literal follows on the same
  line (`function f() { yield 1 }`), in any file, as in tsc 7; the checker reports TS1163.
  Otherwise it is an identifier. Inside a generator, `yield` in operand position
  (`1 + yield x`, `<T> yield x`) is TS1109 and is left for the next expression.
- **Recovery keeps tsc's missing names.** Where tsc's recovery creates a declaration whose
  name is missing, so do we, with a `Missing` name node (the binder declares it as
  `(Missing)`, so duplicates match tsc): `function throw() {}` and `enum void {}` (the
  reserved word starts the next statement), a keyword or literal key without `:` in an
  object binding pattern (`{ return 'a' }`), a type-start token in a parameter list
  (`f(a!: T)`, `f(a<T>)`: a nameless parameter, the token skipped) and a reserved word as a
  parameter name (`f(null)`). Parameter and object-pattern lists go on after a missing
  `,` where tsc's do; a statement keyword (`f(enum)`) ends a parameter list.
- **Lists recover as tsc's do** (`lists.rs`, observed in tsc 7). `in_list(l, f)` marks the
  list as active while `f` parses it. A token that is neither an element start nor a
  terminator of the innermost list is reported with the list's error (TS1128 for statements,
  TS1068 for class members, TS1130 for switch clauses, ...). The list then ends without
  consuming the token if any active list can use it, as an element or a terminator
  (`class C { constructor() { static f = 3; } }` ends the block at `static`). Otherwise the
  token is skipped. A block without `{` is empty, and a stray `catch`/`finally` starts a
  `try` statement with `'try' expected`. In a `,`-separated list (`parse_delimited`) a
  missing `,` between elements is `',' expected` and the list goes on (`f(a:b)` is one
  error, at the `:`). In an object literal a same-line `;` then stands for the `,`.
  Arguments, array and object literals and `var` declarators use it so far.
- **Some grammar errors wait for a clean file.** tsc's checker reports TS1123 (`var;`),
  TS1009 (`var a,;`) and TS1248 (`const` on a class member) only when the file has no
  syntax error. The parser records them with `deferred_grammar_error_at`, and `finish`
  keeps them only if no other parse error (grammar errors from `grammar_error_at` aside)
  and no scanner error was reported. So do (`method_grammar`, the first that applies):
  TS1094 accessor type parameters, TS1054 `get` parameters, TS1095 `set` return type,
  TS1049/1053/1051/1052 the `set` parameter (a `this` parameter does not count), and
  TS1092/TS1093 constructor type parameters (just after the `<`) / annotation; also
  TS1098/TS1099 empty `<>` lists, TS1492 `using {a} = x`, and an accessor without a body,
  which is `'{' expected` at its last character (`;` included) and has no body.
  `constructor<T>()` is a constructor. The kept ones are also listed in
  `ParseResult::grammar`, so a consumer that wants syntax errors only (the linter) can drop
  them (`ParseResult::syntax_errors`).
  Statement grammar errors come from `grammar.rs`, checked as each statement is parsed
  (no extra tree walk: a walk cost 28% of parse time): jump targets
  (TS1104/1105/1107/1115/1116, from a `Jump` scope stack pushed around loop, switch and
  labelled bodies, function bodies and static blocks), `return` outside a function
  (TS1108), the first non-declaration statement of an ambient block (TS1036), a label on
  a `var`/`let`/`const` (TS1344), declarator initializers (TS1155, TS1182, and
  TS1091/TS1188/TS1189/TS1190 in `for...in`/`for...of` heads), parameter lists (one of
  TS1014/1047/1048/1015/1016 per list, TS1317 in constructors), index signatures (one of
  TS1096/1017/1018/1019/1020, TS1268 for a keyword key type, then TS1021 only when the key
  type is surely valid: other key types are the checker's), class heritage (one of
  TS1172/1173/1174/1175), declarations without modifiers as `if`/loop bodies, through
  labels (TS1156), `import(T)` types (TS1141), and ambient rules: initializers (TS1039;
  TS1254 for a `const` or `readonly` without a type whose initializer is not a literal, a
  literal enum reference or similar), bodies (TS1183, also accessor bodies in types) and
  generators (TS1221). A decorated `declare` field skips TS1039 (tsc reports the decorator
  instead). Each statement list is checked once it is parsed (`check_statement_list`, by
  `Container`: file, namespace, external module, block): module elements in a block or
  under a label (TS1235/1234/1231/1233/1258/1232, TS1473/1474 in JavaScript), quoted
  module names outside ambient contexts (TS1035), and in a namespace TS1319/1194/1147.
  Also: `export <modifiers> export =` (TS1120), `module N` (TS1540 per name), `if (x);`
  (TS1313), `for await` outside an async function (TS1103; TS18038 in a static block,
  told apart by `Jump::StaticBlock`), `import(x,)` (TS1009), generator overloads (TS1222),
  definite assignment `!` (TS1255/1263/1264), abstract accessor bodies (TS1318) and index
  signature trailing commas (TS1025). Nothing under a `with` body is checked.
- **Import calls take an argument list.** `import(...)` parses its arguments like a call:
  the first is the specifier, the second the options, later ones are parsed and dropped
  (tsc's TS1450 for them, and for `import()`, is a checker message). A spread argument is
  TS1325 and `import(x,)` TS1009, both deferred. `import` followed by neither `(` nor `.`
  in an expression is `Expression expected` without consuming it, so the import
  declaration after it parses. A named import or export list ends at the first token
  that is not a name, string or `,`.
- **Arrow heads are kept where tsc keeps them.** A parameter list that can only be an
  arrow's (`(): T`, `(x: T`, `(...`), or one followed by `{` on its line, is an arrow
  with `'=>' expected` when the `=>` is missing (`var a = () { };`). An arrow body that
  starts a statement but no expression (`() => var k = 10; }`) is a block missing its
  `{`, read up to the `}`. A function without a body is `'{' expected` where one is
  needed (nothing consumed) and TS1144 `'{' or ';' expected` where it may be omitted;
  after either error the function gets an empty zero-width `Block` body, as in tsc (no
  TS7010/TS2391 for a missing implementation).
- **A missing `;` after a lone name is about the name** (`semicolon.rs`, observed in tsc 7):
  `a b` is TS1434 at `a`, `asynd function f() {}` TS1435 "Did you mean 'async'?",
  `interface {` TS1438, and a typed class property followed by a token is TS1442 (TS1441 at
  `(`). `interface`/`type`/`namespace`/`module` before a token that is no name are about
  that token: TS2427/TS2457/TS2819 "... name cannot be 'void'" (`interface void {}`).
  Anything else is `';' expected` at the next token.
- **A reserved word in type position is a type name** (`x: break`, `a: super()`): no syntax
  error, the checker reports TS2304, as in tsc.
- **A reserved word where a name is needed is TS1359** "Identifier expected. 'void' is a
  reserved word that cannot be used here." (`enum void {}`, `function function() {}`,
  `{ while: while }`), not TS1003. After `class` there is no name error: the class has no
  name (deferred TS1211) and the reserved word is `'{' expected`.
- **`yield` and `await` as names.** In a generator, `yield` as a binding, parameter or
  function name is an identifier with no parse error (the binder reports TS1212-TS1214, as
  tsc 7 does). `await` as a name in an async context, and an async function expression
  named `await`, is a deferred TS1359 (tsc reports it from the checker, so it does not stop
  semantic checking). A function expression's `yield` name ignores the enclosing
  generator.
- **Decorators after modifiers are TS1436** at the `@` (`public @dec m() {}`); the
  decorators and any further modifiers still belong to the member. A same-line `@` after a
  property (`prop @dec` + newline + `m() {}`) is TS1436 too and decorates the next member.
- **Smaller recoveries observed in tsc 7:**
  - The name after `.` is missing at the end of the file and before a name on the next line
    that has another name after it on its line (`B.` + newline + `namespace B {}`): TS1003
    just after the `.`, and the next line starts a statement (expressions and type names).
  - Modifiers before a token that starts no member (`public {};`): TS1146 just after them,
    then a nameless property whose `;` is missing.
  - `f(...)`: TS1003, not TS1138. An arrow head that can only be one (`()`, `(...`, `(x:`)
    is parsed for real, errors kept; others (`(readonly as T)`) stay speculative.
  - `interface I implements A`: TS1176 at `implements`. `default function () {}` at
    statement level: `'export' expected` at `default`, then the declaration.
  - Deferred (checker) grammar errors: TS1009 for `extends C, {`, TS1029 for
    `@dec default class {}`, TS1246/TS1247 for an interface / type literal property
    initializer (at the initializer), TS1197 at a catch variable's initializer, TS1113 for
    the first extra `default` of a switch, TS18016 for a private name as an object literal
    or type member key outside a class (not for `a.#x`, which the checker handles).
  - `await`/`yield` in type position are type names, even where they are operators.
- **Heritage types take their type arguments across a line break.** `extends A<T>` followed by
  a newline and `{` is `ExprWithTypeArgs { expr: A, type_args: <T> }`, as on one line; the
  instantiation expression the member tail builds there is unwrapped (and dropped from the
  arena). `A<T>\n.x` stays `Member(Instantiation)` with TS1477 at the type arguments, and
  `A<T>\n(1)` a call, as in tsc. A heritage list recovers like tsc's: a token that is not `,`
  or a terminator is `',' expected` and the list goes on (`extends A x {}`).
- **Operands and assignment targets are tsc's** (all observed in tsc 7):
  - `??` has the precedence of `||`, left to right: `a ?? b || c` is `(a ?? b) || c`,
    `a ?? b && c` is `a ?? (b && c)`.
  - Only a left-hand-side expression takes `=`/`op=` (`is_lhs_expression`): `await x = 1`,
    `-x = 1`, `a + b = c`, `x as T = 1` leave the `=` for the statement (`';' expected`).
  - A prefix `++`/`--` operand is a left-hand-side expression with no postfix operator:
    `++a++` is `++a` then `';' expected`; `++-a` and `++await x` (in an async function)
    are TS1109.
  - `new tag<T> `x` (args)` is `New(TaggedTemplate(tag, <T>, `x`), args)`.
  - `await x` inside a function that is not async is an await expression when a name,
    keyword or literal follows on the same line (tsc's recovery; the checker reports
    TS1308, which the editor's "Add async modifier" fix needs). `await (x)` and `await`
    before a line break stay an identifier there.
  - Import attributes (`with { ... }`, and the `{ with: { ... } }` of an `import()` type)
    are a delimited list (`List::ImportAttributes`, TS1478 for a bad entry). An `import()`
    type whose second argument is not an object with `with`/`assert` gets empty
    attributes, which declaration emit writes as `{ with: {} }` (tsc's recovered node).
- **Invalid syntax gets tsc's code** even where the tree stays as parsed: TS1209 (`new a?.b()`),
  TS17006/TS17007 (`-a ** b`, `<T>a ** b`). tsc reports these as syntax errors. TS5076
  (`??` mixed with `||`/`&&`), TS1106 (`for (async of x)`), TS1497 (a decorator that is not an
  identifier/member chain, optionally called once, or parenthesised) and TS18007 (`{a, b}`
  in JSX) come from tsc's grammar check, which reports them next to semantic errors. We
  report them here with `grammar_error_at` (they do not fail speculation) so tools treat the
  file as invalid, as transform and codegen asked (request R5). TS1164 (a computed enum
  member name other than a string or number literal) is the same, reported with
  `checker_error_at`, which is not merged with a parse error at the same position
  (parserComputedPropertyName30 has both). Each of these codes is in the checker's
  `CHECKER_GRAMMAR_CODES`, so it does not stop semantic checking.
- Generic-call and arrow ambiguities (`a < b > (c)`, `<T>(x) => x` in TSX, an arrow in the true
  branch of `?:`) are resolved by speculation with tsc's follow-token rules
  (`can_follow_type_args`, the conditional-true rule). Change them only with a tsc-verified
  test.
- **TypeScript syntax in JavaScript files is opt-in** (`parse_with` + `TsInJs`); `parse` on
  `SourceKind::Js`/`Jsx` stays JavaScript-only. With `Report`/`Allow` the file parses with
  the TSX grammar except that calls, `new`, tagged templates, instantiation expressions and
  JSX tags take no type arguments (`Parser::js_ts`), as in tsc. `Report` then walks the tree
  once (`js_ts.rs`, explicit stack) for tsc's TS8xxx errors; the rules observed in tsc 7 are
  listed at the top of that file and pinned by `ts_in_js_matches_tsc`.
- Span rules:
  - A node's span runs from its first token to the end of its last consumed token.
  - A node that consumed nothing is zero-width at the end of the previous token, the same
    as `Missing`.
- Output is deterministic. `failed_arrows` and `failed_type_args` are `HashSet`s used only
  for membership, never iterated.

## Testing

- `cargo test -p ts_r_parser`:
  - Unit tests per construct in `src/tests.rs`: valid snippets must produce no diagnostics,
    error snippets check codes and positions.
  - Robustness tests: garbage, truncation and deep nesting.
- Snapshots: `tests/fixtures/*` are rendered (dump, comments, diagnostics) and compared with
  `tests/snapshots/<name>.txt`. Regenerate them with
  `UPDATE_SNAPSHOTS=1 cargo test -p ts_r_parser --test fixtures` and review the diff.
  Fixtures without "error" in the name must have no diagnostics.
- Corpus: `scripts/limit.sh -- cargo run -p ts_r_parser --example parse_corpus --release --
  <dir> [--errors N] [--quiet]`.
  - It parses every `.ts/.tsx/.mts/.cts/.js/.mjs/.cjs/.jsx` file (symlinks are not followed)
    and reports files with errors, nodes, files/s and MB/s.
  - Like every harness that runs real-world or generated input, it calls
    `ts_r_testing::cap_memory!()`. So do `examples/dump.rs`, `tests/fixtures.rs` and the unit
    tests.
  - Baseline on private-app including `node_modules`: 12,711 files and 214.6 MB with 0
    files reporting errors.
    - About 300 MB/s single-threaded on an Apple M-series machine with the memory-capping
      allocator.
    - About 330–350 MB/s without that allocator.
- Fuzzing: bounded only. Use a fixed iteration count, inputs at most 200 KB, a memory cap,
  and run it under `scripts/limit.sh`. See the resource limits in the root AGENTS.md. The
  M1 mutation runs (about 86k mutated corpus files) found the lone-`#` loop, now a
  regression test.
- Ad hoc: `cargo run -p ts_r_parser --example dump -- -e 'let x = <T,>(a: T) => a' --kind tsx`
  or `-- path/to/file.ts`.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_lexer` and `ts_r_diagnostics` only; no third-party crates.
Dev: `ts_r_testing` (memory cap for tests and examples).

## Gaps

- **Only some checker-level grammar errors are reported** (the deferred ones listed under
  "Some grammar errors wait for a clean file", and TS5076, TS1106, TS1497, TS1164, TS18007). Not
  yet, for example:
  - Modifier order and applicability (the parser records every modifier keyword and its
    span in `Ast::modifier_tokens` for the checker's pass).
  - Strict-mode-only restrictions beyond reserved words.
- Recovery on badly broken input is close to tsc, but follow-on errors can differ in count
  and position. The first error usually matches.
- Regex pattern syntax is not validated (see `ts_r_lexer`).
- No incremental re-parse; whole-file only.
- `parse` runs on one thread. The corpus tool is single-threaded so its numbers are
  per-core throughput.
