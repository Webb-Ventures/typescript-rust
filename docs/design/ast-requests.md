# AST and parser requests

Requests from downstream crates to the front-end owners (ts_r_ast, ts_r_parser). Append
new entries at the end; do not rewrite other agents' entries. Each entry names the
requester, gives a minimal repro, what tsc 7.0.2 reports and what our parser produces
(`cargo run -p ts_r_binder --example astdump -- file.ts`).

## R1. Misplaced modifiers parse as identifiers (from: binder, M5)

**Resolved in 95fecd8.** Modifier keywords parse as modifiers in every position tsc 7
accepts them (statements, parameters, object/type/class members, type parameters), with no
parse error, and each is recorded with its span in `Ast::modifier_tokens`
(`modifiers_in`, `modifier_tokens_of`). Object-literal `Property`/`ShorthandProp` and
`MethodSignature` gained `modifiers`. See `crates/ts_r_parser/src/modifiers.rs`.

tsc parses a modifier keyword in a position where no modifier is allowed as a modifier
and reports a grammar error. Our parser treats it as an identifier and recovers, so the
binder sees an `Ident` named `public`/`static` and (correctly, for an identifier) reports
TS1212/TS1213/TS1214 strict-mode reserved words. This accounts for ~145 of the binder's
extra diagnostics in the conformance suite (classUpdateTests, class2, restParamModifier,
modifiersInObjectLiterals, propertyWrappedInTry, ...).

```ts
namespace Y { public class A {} }      // tsc: 1,15 TS1044, no syntax error
function g(static x) {}                // tsc: 1,12 TS1090, no syntax error
const o = { static m() {} };           // tsc: 1,13 TS1042 + TS1184, no syntax error
export static var v = 1;               // modifier, not an identifier
class C { static static foo() {} }     // repeated modifier
```

Ours: `ExprStmt(Ident "public")` + TS1005 `';' expected`; `Param(Ident "static")`,
`Param(x)` + TS1005; `ShorthandProp(static)` + `MethodDef(m)` + TS1005.

Request: parse these as modifiers on the following declaration/parameter/member (so the
modifier set records them) and leave the "modifier cannot appear here" errors to the
checker's grammar pass. A `Modifiers` bit plus the modifier's span is enough for the
binder.

## R2. `yield` in a non-generator strict function (from: binder, M5)

**Resolved in 9abe49e.** tsc 7 does this in any file, not just strict code: outside a
generator, `yield` starting an assignment expression is a `Yield` when a name, keyword,
private name or string/number/bigint literal follows on the same line; otherwise it stays an
identifier. Inside a generator, `yield` in operand position is TS1109, as in tsc.

```ts
"use strict";
function f() { yield 1; }              // tsc: 2,16 TS1163, no syntax error
class C { constructor() { yield foo } }
```

tsc parses a `YieldExpression` (and reports TS1163 from the checker) whenever the code is
strict, even outside a generator. Ours: `ExprStmt(Ident "yield")` then TS1005 `';'
expected`, and the binder reports TS1212 on the identifier. Request: in strict code
(module, class body, "use strict" prologue) parse `yield` as a yield expression; keep the
identifier reading only in sloppy non-generator code. (castOfYield, yieldInStrict* tests.)

## R3. Recovery differences that change binder output (from: binder, M5, low priority)

**Resolved in 16a6b7e** (with a small binder change: missing names are declared and print
as `(Missing)`). `import while = require(...)` needs nothing: tsc's `(Missing)` names in
reservedWords2 come from the other lines (`import * as while`, `function throw`,
`{while, return}`, `enum void`), which now match. interfaceNaming1's remaining extras are
TS1212 in a file with syntax errors, which tsc does not report there (a binder matter).

These produce different declarations after a syntax error, so duplicate-identifier
diagnostics differ. Syntax errors stop tsc before semantic checks, so these only matter
for conformance baselines, not for users:

- reservedWords2: `import while = require("dfdf");` tsc reports 2,8 TS1109, 2,14 TS1005,
  2,31 TS1005 and then declares several `(Missing)` names that it reports as TS2300/TS2567.
  Ours parses an `ImportDecl` with a Missing source and then a `while` statement, so no
  Missing declarations exist.
- interfaceNaming1, parametersSyntaxErrorNoCrash1-3, reachabilityChecksNoCrash1: tsc
  creates `(Missing)`-named parameters/declarations that collide. Ours drops them.
- class2, castOfYield: see R1/R2.

Request: where recovery has to invent a declaration name, emit a `Missing` name node
instead of dropping the declaration, so the binder can declare it under `(Missing)`.

## R4. `extends A<T>` followed by a newline parses as an instantiation expression (from: transform/codegen, M1)

**Resolved in 69ef430.** A heritage type whose expression
came back as an `Instantiation` is unwrapped to `ExprWithTypeArgs { expr, type_args }`
(the dropped node was the last one allocated, so nothing is orphaned). As observed in tsc 7:
`A<T>\n.x` is still `Member(Instantiation)` with TS1477, now reported at the type arguments
(it was at the `.`), and `A<T>\n(1)` is still a call. The heritage list also recovers like
tsc's (`extends A x {}` is `',' expected` and the list goes on). Round trip over
`tests/cases`: 45 → 38 mismatches; all seven files listed below now match.

```ts
class A<T> {}
class B<T> extends A<T>
{
}
```

tsc 7: `HeritageClause` → `ExpressionWithTypeArguments { expression: A, typeArguments: [T] }`,
the same as when `{` is on the same line. Ours: `ExprWithTypeArgs { expr: Instantiation(A,
<T>), type_args: None }` (span 33..37 above): the newline after `>` makes the parser take the
instantiation-expression path. Output is the same text, but the tree differs, so codegen's
round trip reports a shape change (7 of the 31 round-trip mismatches over
`tests/cases`: baseTypeOrderChecking, complicatedGenericRecursiveBaseClassReference,
indexedAccessRelation, infiniteExpandingTypeThroughInheritanceInstantiation,
instanceSubtypeCheck1, specializeVarArgs1, infiniteExpansionThroughInstantiation2).

Request: in a heritage clause, parse `Expr<TypeArgs>` as `ExprWithTypeArgs { expr, type_args }`
regardless of what follows the `>`.

## R5. Invalid syntax accepted without an error (from: transform/codegen, M1, low priority)

**Resolved in c04fa6e.** Each construct now gets tsc 7's code at tsc's position, checked by
running tsc 7.0.2 on each (`invalid_syntax_reported_like_tsc` in the parser tests):

- `a ?? b || c`: TS5076 at the inner operation. `??` now binds like `||` (left to right), so
  the tree is tsc's `(a ?? b) || c` (and `a ?? (b && c)`).
- `for (async of x)`: TS1106 at `async` (not with `for await` or `(async)`).
- `new a()?.b()` is valid in tsc; the round-trip file was `new a?.b()`, which is TS1209 at
  `?.` with the callee text in the message. The tree stays `(new a)?.b()`, as tsc's.
- `@a?.b class C {}`: TS1497 at the expression, for anything but `(e)` or an identifier /
  `.name` / `!` chain called at most once (also `a()()`, `a().b`, `(a).b`, `this.x`).
- `await using [a] = null`: only left-hand-side expressions take `=`, so this is
  `await using[a]` then TS1005 `';' expected` at `=` (also `-x = 1`, `a + b = c`, ...).
- `++(a as any)++`: a prefix update's operand is a left-hand-side expression, so this is
  `++(a as any)`, TS1005 at the second `++` and TS1109 at `;` (`++await x` is TS1109).

Also from the same round-trip list: TS17006/TS17007 (`-a ** b`, `<T>a ** b`), TS18007 (`{a, b}`
in JSX), and `new tag<T> `x` (args)` now parses as `New(TaggedTemplate(...))` like tsc.

tsc reports TS5076, TS1106, TS1497 and TS18007 from its grammar check (next to semantic
errors), not as syntax errors; we report them from the parser as requested, without failing
speculation. Round trip over `tests/cases`: 18060 ok / 38 mismatches / 1114 skipped →
18061 / 18 / 1133. The 18 left are codegen's: 14 misplaced modifiers that are not printed
(`async class`, `public` in an object literal, `const` on a class member, `override` on a
signature, `public T`; see R1) and 4 valid inputs it parenthesises (`for (v++ of x)`,
`for (a = 1 in b)`, `for await (async of x)`, `@x!`). Transform baseline oracle: 1612 / 164
→ 1611 / 159 (6 more files skipped because they now have errors; one of them matched), the
binder conformance numbers are unchanged, and private-app still parses with 0 errors.

The parser accepts these without a diagnostic; tsc reports a syntax error. Codegen has to add
parentheses to print valid JavaScript, which changes the tree (the remaining round-trip
mismatches besides R4):

```ts
a ?? b || c;               // tsc: TS5076 '||' and '??' operations cannot be mixed without parentheses
for (async of x) {}        // tsc: TS1106 The left-hand side of a 'for...of' statement may not be 'async'
new a()?.b();              // tsc: TS1209 Invalid optional chain from new expression
@a?.b class C {}           // decorator expression must be a member chain / call / parenthesized
await using [a] = null;    // tsc: `await using` + `[` is an element access, then `= null` is TS1005;
                           // ours: Assign(Await(Index(using, a)), null), no error
++(a as any)++;            // invalid update operand (incrementAndDecrement, *InvalidOperations)
```

Request: report these (any error code tsc uses is fine) so downstream tools see the file as
invalid. The transform and codegen don't depend on the shape of the recovered tree.

## R6. Generated child remapping and a synthesized comment node (from: transform, M1)

**Part 1 resolved in 7e73d49.** `define_nodes!` generates
`Node::map_children(&self, ast: &mut Ast, f: &mut F) -> Node` with
`F: FnMut(&mut Ast, NodeId) -> NodeId` (plus `Ast::map_children(id, f)`). `f` gets the arena
so it can allocate (a deep clone is `ast.map_children(id, &mut deep_clone)`); children are
visited in source order and a list is copied with `add_list` only when an item changes.
`ts_r_transform/src/rewrite.rs` is gone. Every transform test passes and the numbers are
unchanged: baseline oracle 1611 / 159, esDecorators 113 / 27, fixtures 30 / 1,
private-app 339 / 339.

**Part 2 resolved in 61090cd**, additively, as proposed: no node variant. `Ast` has a side
table `synthetic_comments: Vec<SyntheticComment { node, text: Atom, trailing }>` sorted by
node id (`add_synthetic_comment`, `synthetic_comments_of`). The parser never fills it, so
it is not part of checkpoints. Codegen prints a node's comments around the expression
(inside any parentheses it adds), drops them without `comments` and in the compact layout
(tsc's `removeComments` drops them too), and lets a trailing comment separate a number from
`.` (`0 /* E.A */.toString()`). The const enum inliner attaches tsc's comment: the access as
written, with `*/` escaped as `*_/` (`5 /* E["B"] */`, `0 /* E . A */`); it also inlines
`` E[`A`] `` now. The output for a test file of these cases is byte-identical to tsc 7.0.2's.
Comments on nodes that are later deep-cloned are not copied (no current transform needs it).

1. **`map_children`.** `ts_r_transform` deep-clones subtrees (namespace and enum IIFEs, class
   field moves). That needs "this node with every child id mapped through `f`", for all 146
   variants. Today `crates/ts_r_transform/src/rewrite.rs` is a hand-written table that must be
   kept in sync with `node.rs`. Proposal: have `define_nodes!` also generate
   `Node::map_children(&self, ast: &mut Ast, f: &mut impl FnMut(NodeId) -> NodeId) -> Node`
   (remapping `NodeList`s by copying them through `ast.add_list`), next to `for_each_child`.
2. **Synthesized comments.** tsc prints inlined const-enum values as `0 /* E.A */` and keeps a
   few comments on synthesized nodes. Transforms can't attach text to a new node. Proposal: a
   side table `Ast::synthetic_comments: Vec<(NodeId, Atom, bool /* trailing */)>` (sorted by
   node id) that codegen prints around the node. Low priority: oracles compare AST shapes, so
   this is cosmetic.

## R7. `function f(export e)` is rejected; tsc accepts it (from: codegen, M1, low priority)

**Resolved in the M4 lint-gaps commit "export as a parameter modifier".** `export` is a
parameter modifier (in functions, constructors, arrows and function types) when a parameter
start follows, except `{`, `as`, `default`, and `type` not followed by one (observed in tsc
7); otherwise it is a reserved word used as a name, TS1359 (now also reported for `f(null)`,
as tsc does), and the list goes on. Codegen round trip on `tests/cases`: one more file parses.

tsc parses `export` as a (misplaced) parameter modifier and reports it later; our parser stops at
`export` and recovers, so the tree differs. Parse it as a modifier on the parameter, as with the
other misplaced modifiers in R1.

## R8. TypeScript syntax in `.js` files (from: lint, M4)

**Resolved in f187cde** (M4 lint gaps). Opt-in and additive:
`ts_r_parser::parse_with(src, kind, ParseOptions { ts_in_js })` with
`TsInJs::{Reject (default), Report, Allow}`; `parse` is unchanged. No AST changes. See
`crates/ts_r_parser/src/js_ts.rs`.

tsc parses `.js` files with its TypeScript grammar (JSX on, but no type arguments on
expressions) and reports each TypeScript construct with a TS8xxx error; typescript-eslint
does the same without treating those as parse errors. Our `SourceKind::Js` grammar rejects
TypeScript syntax, so ts-r-lint parsed `.js` files as `.ts` when the TypeScript parser was
configured, which got two rule-oracle cases wrong:

```js
new Array<Foo>(1, 2, 3);   // tsc: no error; `(new Array < Foo) > (1, 2, 3)`; ours (as .ts): a call with type args
<div/> as any;             // tsc: 1,11 TS8016 only; ours (as .ts): "'>' expected" (no JSX)
let x: number = 1;         // tsc: 1,8 TS8010 "Type annotations can only be used in TypeScript files."
```

`Report` gives tsc's TS8002–TS8017 and TS8037 at tsc's positions (pinned by
`ts_in_js_matches_tsc`, 64 inputs); `Allow` gives the same tree without them (ts-r-lint uses
it with the TypeScript parser). The checker can use `Report` for `allowJs`/`checkJs` files.

## R9. JSON syntax errors differ from tsc's (from: checker bench parity, M5, low priority)

`parse_json` parses a `.json` file (`resolveJsonModule`) as one TypeScript expression
followed by statements, so malformed JSON gets TypeScript statement errors. tsc 7.0.2
(observed, `--pretty false`):

```text
{ "a": 1, "b": , }          bad.json(1,15): TS1328 Property value can only be string literal, numeric literal, 'true', 'false', 'null', object literal or array literal.
                            bad.json(1,15): TS1109 Expression expected.        (ours: TS1109 only)
{ "a": 1 } { "b": 2 }       bad2.json(1,12): TS1012 Unexpected token.          (ours: 1,17 TS1005 ';' expected)
[1, 2<EOF>                  bad3.json(2,1): TS1005 ']' expected.               (ours: TS1005 ',' expected)
```

A JSON-specific mode (value grammar only, TS1328 for non-JSON values, TS1012 for anything
after the value) would match. The checker types JSON modules from the parsed value and
needs no AST change.
