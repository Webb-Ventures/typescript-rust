# ts_r_minify

JavaScript minifier: scope analysis, constant folding and dead-branch removal, safe
compression, and scope-aware renaming (mangling or deconflicting). Used per module by
`ts_r_bundler` and on single files by `ts-r-build --minify` without `--bundle`.
Root rules: [../../AGENTS.md](../../AGENTS.md). Design:
[../../docs/design/0008-bundler.md](../../docs/design/0008-bundler.md); AST:
[../../docs/design/0001-ast.md](../../docs/design/0001-ast.md).

Written from scratch. terser's, esbuild's, swc's and oxc's docs and observed output are the
oracles; their source may be read to understand a rule, never ported (root AGENTS.md).

## API in one breath

```rust
let out = ts_r_minify::minify(path, src, &MinifyOptions { whitespace, identifiers, syntax, module }, None);
// or on a tree you own (the bundler):
ts_r_minify::minify_ast(&mut ast, src, &opts, strict);
let scopes = Scopes::analyze(&ast, strict);   // symbols, references, globals, through-sets
fold::fold(&mut ast, src, &scopes);            // returns true when the tree changed
compress::compress(&mut ast, src, &scopes, &CompressOptions::default());
let names = rename::compute(&ast, &scopes, &RenameOptions { mode, fixed, include_module_scope, reserved, module_reserved });
rename::apply(&mut ast, &scopes, &names);
```

Files: `scope.rs` (scopes, symbols, references, hoisting, `eval`/`with` detection),
`purity.rs` (side-effect analysis, `/*#__PURE__*/`), `fold.rs` (constant folding,
dead branches, the handler of an empty `try`), `compress.rs` (size rewrites), `rename.rs` (mangle/deconflict), `private.rs`
(`#private` member names, per class; untouched when a direct `eval` exists), `names.rs`
(short-name sequence, reserved words, sanitising), `build.rs` (synthesized node helpers).

## Invariants

- **Never changes behaviour** of code that does not use `eval`, `with` or `Function.prototype.toString`.
  Scopes containing direct `eval` or `with` keep every name in reach.
- **Stable mangling**: names are assigned scope by scope, most-referenced binding first (ties
  by declaration order), restarting the sequence in every scope. The same input gives the
  same output, and the bundler fixes module-scope names, so an edit in one module does not
  rename the others.
- Globals and names passed in `reserved` are never produced as new names.
- **Speed over bytes where V8 cares**: top-level `const` is never rewritten to `let` (V8
  constant-folds module and script `const` bindings, not `let`; generated validators ran
  35% slower). Nested never-written `const` becomes `let`.
- Re-run `Scopes::analyze` after any pass that changes the tree (fold reports changes).
- Synthesized nodes have empty spans; `build::replace` copies the replaced node's span so
  source maps keep pointing at the original code.
- No panics on any tree the parser can build.

## Testing

`cargo test -p ts_r_minify`: unit tests per pass (folding, compression, mangle, deconflict,
scope edge cases). End-to-end minified bundles run under Node in `ts_r_bundler`'s
`tests/bundle.rs`.

## Dependencies

`ts_r_span`, `ts_r_ast`, `ts_r_parser`, `ts_r_codegen`. External: `rustc-hash` (fast
hash maps keyed by symbol and node ids; never iterated for output order). Dev: `ts_r_testing`.

## Gaps

- No property mangling, no function inlining, no cross-statement constant propagation.
- Compression is conservative: no `if`-return merging across nested blocks, no loop rewrites.
