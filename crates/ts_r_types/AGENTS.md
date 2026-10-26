# ts_r_types

The interned, thread-safe type store shared by every checker worker: `TypeId`s, type data,
type lists, object types, signatures, type parameters, mappers, alias info and relation cache
storage. It knows nothing about the AST, the binder or symbols beyond the opaque `SymId`.
Root rules: [../../AGENTS.md](../../AGENTS.md). Design record:
[../../docs/design/0004-types-and-checker.md](../../docs/design/0004-types-and-checker.md).

## API in one breath

```rust
let store = TypeStore::new();                       // intrinsics have fixed ids (TypeId::STRING ...)
store.string_literal("a", false); store.number_literal(1.0, false);
store.union_sorted(&ids, alias); store.members_of_union(u); store.intersection_sorted(..);
store.get(t); store.flags(t); store.kind(t); store.object(o); store.sig(s); store.list_items(l);
store.reference(..); store.tuple_target(..); store.anonymous(..); store.mapper(..);
store.members(o); store.set_members(o, ResolvedMembers{..});   // write-once
store.relation_cache(RelationKind::Assignable);    // shared by all threads
```

Files: `store.rs` (all type data and interning), `flags.rs` (`TypeFlags`, `ObjectFlags`,
`SigFlags`, `TypeTraits`), `chunk.rs` (`ChunkVec`, the lock-free append-only vector behind
every table), `tests.rs`.

## Invariants

- **Hash-consing is content-based** for structural types (literals, unions, intersections,
  references, tuples, indexed access, `keyof`, template literals); declaration-based types are
  keyed by symbol and mapper. References are keyed by (target, arguments, alias): a reference
  named by a type alias (`reference_aliased`) is a distinct type from the plain one, whose
  alias is `AliasId::NONE`. Mapped types are keyed by (declaration node, mapper, alias),
  deferred conditional types by (root, mapper, alias) with one `ConditionalRoot` per
  conditional node. Union member order is by content key, never by id, so printing is
  deterministic.
- **Special mappers.** `MapperId::PERMISSIVE` and `MapperId::RESTRICTIVE` are reserved ids
  (`is_special`), never stored mapper lists; the checker interprets them (type parameters to
  the wildcard, or to their `restrictive_type_param` copy, which is unique per parameter).
- **Write-once publication.** Lazily computed facts (resolved members, base types, variances)
  go into `OnceLock` slots; a second writer's value is dropped, so workers must compute the
  same answer (the checker never publishes speculative results).
- **No panics on user input.** `ChunkVec::push` panics only past 2^26 entries (the memory cap
  is hit long before); `get` panics only for ids the store never handed out (a checker bug).
- **Named error types.** `named_error(name)` interns an any-flagged intrinsic per name
  (`Key::NamedError`, trait `NAMED_ERROR`, which `combined_traits` does not propagate);
  `is_error(t)` is true for it and for `TypeId::ERROR`, `error_name(t)` gives the text.
- **Union origins.** `union_with_origin(members, origin)` stores the items a union was
  written with as a symbol-less alias (`AliasInfo { sym: SymId::NONE, args: origin }`), so
  the origin is part of the interned key and visible as soon as the id is; `union_origin(t)`
  reads it and `alias_of` hides it.
- **Forward references.** `forward(sym)` interns one `TypeKind::Forward` per non-generic type
  alias (`Key::Forward`), at the alias symbol's level; `link_forward(f, t)` points it at the
  alias's type once (ignored when `t` lives in a higher level). `get`/`flags`/`kind`/`traits`
  follow a linked forward, so a linked forward reads as its target everywhere; an unlinked
  one reads as `UNRESOLVED`. This is the only way a type graph can be cyclic
  (`type J = string | J[]` is `string | Array<F>` with `F -> J`): walkers over type
  arguments must stop on traits or keep a visited set. `forward_node((file, node))`
  interns one per type node (`Key::ForwardNode`, at the node's file level) for an element
  whose union names the alias (`type V = (string | V)[]`); it carries no alias.
  `forward_alias(t)` gives an alias forward's symbol (printers print the alias name there;
  a node forward prints as its target), `is_forward(t)` tells any raw forward id from its
  target, and `unforward(t)` gives a
  linked forward's target id (identity comparisons need it: `F -> object` is not
  `TypeId::OBJECT`).
- No relation, printing or member resolution logic here: that needs declarations and lives in
  `ts_r_checker` (`relate.rs`, `print.rs`, `members.rs`).

## Levels (design record 0005, `ts_r_history`)

A store can be a *child* of another (`TypeStore::child(&parent, file_start)`, at most
`MAX_LEVELS` = 4 levels): lib files, dependencies, stable project files, hot files. Lower
levels are shared read-only-ish (they keep filling lazily) by every program and project built
on them.

- **Every id carries its level** in its top two bits (`LEVEL_SHIFT`; `level_of`, `index_of`,
  `make_id`): types, objects, signatures, type parameters, lists, mappers, aliases,
  conditional roots and data, template data. `u32::MAX` sentinels and the special mappers
  (`mapper_level`) count as level 0.
- **Anything created goes to the level of its highest part** (`key_level`, `members_level`,
  `signature_level`). Declarations count by file (`file_level`, from each level's
  `file_start`); `(u32::MAX, id)` declaration keys count by the id's level (`decl_level`).
  Getting this wrong puts a type above its parts: a lower level then cannot keep it and
  recomputes it on every use (found once: 10x slower checks). Strings, number literals and
  tuple shapes always live in the root.
- **A lower level never holds a value from a higher one.** Write-once slots check the
  level: signature return/predicate and type parameter constraint/default return the value
  unpublished, `set_regular` drops it, resolved members go to a shadow table in the higher
  level. Each such miss is counted in `PUBLISH_MISSES` (should stay 0 on real projects;
  `ts_r_history`'s `compare` example prints it).
- **Relation caches** are per level (`RelationCacheRef` routes by the higher of the two ids).
- `ChunkVec` is segmented (geometric segments allocated on first use), so an empty store
  costs a few hundred bytes: a store per level per edit is cheap.

## Dependencies

`rustc-hash` only. Dev: `ts_r_testing` (memory cap).

## Tests and harnesses

`cargo test -p ts_r_types`: `src/tests.rs` (interning, references and tuples, write-once
members, parallel interning from 4 threads, relation cache, mappers, mapped, conditional and
template literal interning) and the `chunk.rs` unit tests. Everything else is exercised through
`ts_r_checker`'s tests and conformance harness.
