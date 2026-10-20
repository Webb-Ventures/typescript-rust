# ts_r_decls

Per-file declaration index: for each export (or script global, ambient module, augmentation,
`declare global` contribution) its symbol flags, a canonical BLAKE3 hash of its signature,
whether the signature is fully explicit, and its dependencies; plus the file's module
requests, `export *` specifiers and an API hash. Stored in the shared ts-r cache. Root
rules: [../../AGENTS.md](../../AGENTS.md). Design:
[../../docs/design/0005-history.md](../../docs/design/0005-history.md) ("Units") and
[../../docs/design/0003-binder.md](../../docs/design/0003-binder.md).

## API in one breath

```rust
let idx = ts_r_decls::index(&ast, &bound);          // DeclIndex
idx.entries; idx.entry(Section::Export, "f"); idx.is_explicit(); idx.api_hash;
let bytes = idx.encode(); DeclIndex::decode(&bytes); // versioned, None on damage
let cache = ts_r_decls::open_cache()?;               // namespace "decls", FORMAT_VERSION
let key = ts_r_decls::cache_key(src, kind, &bind_options);
let idx = ts_r_decls::get_or_compute(&cache, &key, || index(&ast, &bound));
```

## Hashing rules

The hash walks the AST generically (`Node::dump_fields`: node kind name, field names,
scalar payloads, children in order). It never sees spans, so whitespace, comments and
moves do not change it. Skipped: function bodies when a return type is written,
initializers of typed variables/parameters/properties (an `init` marker keeps optionality),
constructor and setter bodies, decorators, the type and value of private members
(`private`, `#x`), and non-declaration statements inside namespace bodies. When a type
must be inferred (no return type, untyped initializer, `export default <expr>`), the
inferring parts are hashed and the entry is `explicit: false`.
Detached JSDoc `@type` annotations are also hashed with their host, including references
in those types; annotations already present as child fields use the ordinary tree walk.

References: a referenced non-exported local is folded into the entry (closure, each symbol
once), and so is the local target of an export alias (`export { Local }`); imports become
`Dep::Import { specifier, name }`; names the file does not declare and other script globals
become `Dep::Global`. Exported symbols referenced by another export are not folded (they
have their own entry): they become `Dep::Sibling(export name)` (folded only when the export
table does not list the symbol under its own id). References count wherever the hashed
parts name a symbol: type names, and every value name in a hashed initializer or body
(hashed only when inferred: an untyped initializer `new Counter(1)` depends on `Counter`).
Aliases hash whether they are type-only (`export type { x }` changes the entry).

## Invariants

- Hashes contain no positions, absolute paths, ids or timestamps; equal surfaces give equal
  hashes in any file at any offset.
- **Bump `FORMAT_VERSION` whenever the encoding or a hashing rule changes** (the cache key
  and the payload header both carry it).
- `decode` never panics; truncated, trailing or mis-versioned bytes are `None`.
- Recursion is bounded by `MAX_DEPTH` (an over-deep subtree hashes a marker and marks the
  entry non-explicit); local closure terminates through the `seen` set.

## Dependencies

`ts_r_ast`, `ts_r_binder` (symbols, aliases, resolved references), `ts_r_cache` (storage),
`blake3`. Dev: `ts_r_parser`, `ts_r_testing`.

## Tests and harnesses

- `cargo test -p ts_r_decls`: `tests/index.rs` (what does and does not change hashes,
  explicitness, deps, encoding damage, cache round trip in a temp dir).
- Corpus: `cargo build --release -j 4 -p ts_r_decls --example corpus` then
  `scripts/limit.sh --mem-mb 4096 --time-s 600 -- target/release/examples/corpus ../private-app`
  (decodes every encoded index as a check). private-app: 5910 files, 0 panics,
  56.9k entries, 95% explicit, 6.3 MB encoded; index ≈ 1 s CPU total (the generic
  `dump_fields` walk allocates; a typed walker is the obvious speed-up), peak RSS 53 MB.
