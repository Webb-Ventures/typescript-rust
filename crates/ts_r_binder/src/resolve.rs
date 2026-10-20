//! Resolution of identifier references against the file's own scopes, after the walk (so
//! hoisted and later declarations are visible).

use crate::binder::Binder;
use crate::symbols::{SymbolFacts, SymbolFlags, SymbolId};
use crate::{FreeRef, Refs, ScopeKind, meaning};

/// The meanings a symbol provides.
pub(crate) fn symbol_meaning(flags: SymbolFlags) -> u8 {
    if flags.intersects(SymbolFlags::ALIAS) {
        return meaning::ALL;
    }
    let mut m = 0;
    if flags.intersects(SymbolFlags::VALUE) {
        m |= meaning::VALUE;
    }
    if flags.intersects(SymbolFlags::TYPE) {
        m |= meaning::TYPE;
    }
    if flags.intersects(SymbolFlags::NAMESPACE) {
        m |= meaning::NAMESPACE;
    }
    m
}

/// A function-scope symbol that parameters cannot see: anything but the parameters, type
/// parameters and the body's hoisted functions and namespaces (observed with tsc 7.0.2:
/// `function f(a = x) { var x }` reads the outer `x`; a body function or namespace is found
/// and the checker reports TS2373).
fn is_body_local(flags: SymbolFlags) -> bool {
    !flags.intersects(
        SymbolFlags::PARAMETER
            | SymbolFlags::TYPE_PARAMETER
            | SymbolFlags::FUNCTION
            | SymbolFlags::MODULE,
    )
}

pub(crate) fn resolve_refs(b: &mut Binder<'_>) -> Refs {
    let pending = b.take_refs();
    let mut resolved = Vec::with_capacity(pending.len());
    let mut free = Vec::new();
    let mut assigned: Vec<SymbolId> = Vec::new();
    for r in pending {
        let mut scope = Some(r.scope);
        let mut in_params = r.in_params;
        let mut found = None;
        // Bounded by the number of scopes: each step moves to a strictly earlier scope.
        while let Some(s) = scope {
            let sc = b.scopes()[s.0 as usize];
            if let Some(sym) = b.lookup_table(sc.locals, r.name)
                && symbol_meaning(b.symbol_ref(sym).flags) & r.meaning != 0
                && !(in_params
                    && sc.kind == ScopeKind::Function
                    && is_body_local(b.symbol_ref(sym).flags))
            {
                found = Some(sym);
                break;
            }
            if matches!(
                sc.kind,
                ScopeKind::File | ScopeKind::Module | ScopeKind::Enum
            ) && let Some(container) = sc.symbol
            {
                let exports = b.symbol_ref(container).exports;
                if let Some(sym) = b.lookup_table(exports, r.name) {
                    let s = b.symbol_ref(sym);
                    if !s.facts.intersects(SymbolFacts::EXPORT_SPECIFIER)
                        && symbol_meaning(s.flags) & r.meaning != 0
                    {
                        found = Some(sym);
                        break;
                    }
                    // A name declared in `declare global { }` (any meaning) is the merged
                    // global inside the block: the module's own locals of that name are
                    // shadowed (observed: `var Iterator` in lib.es2025.iterator's global
                    // block makes its `Iterator` types the global interface, not the
                    // file-local class). Leave the reference free for the global lookup.
                    if exports == b.global_augmentations_table() {
                        break;
                    }
                }
            }
            in_params = b.scope_in_params(s);
            scope = sc.parent.filter(|p| p.0 < s.0);
        }
        match found {
            Some(sym) => {
                resolved.push((r.node, sym));
                if r.write {
                    assigned.push(sym);
                }
            }
            None => free.push(FreeRef {
                node: r.node,
                name: r.name,
                meaning: r.meaning,
            }),
        }
    }
    for s in assigned {
        b.mark_assigned(s);
    }
    resolved.sort_unstable_by_key(|&(n, _)| n);
    resolved.dedup_by_key(|&mut (n, _)| n);
    free.sort_by_key(|f| f.node);
    free.dedup_by_key(|f| f.node);
    Refs { resolved, free }
}
