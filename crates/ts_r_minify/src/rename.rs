//! Scope-aware renaming.
//!
//! Both modes work scope by scope, outermost first. For a scope `S`, the *forbidden* names
//! are the final names of everything `S`'s subtree refers to from outside `S` (its
//! "through" set: outer symbols and globals) plus the caller's reserved names. A binding of
//! `S` may not take a forbidden name, or it would capture one of those references.
//!
//! - [`RenameMode::Mangle`] gives every binding the shortest free name, most-referenced
//!   bindings first, restarting the name sequence in every scope (siblings reuse names).
//! - [`RenameMode::Deconflict`] keeps names and only renames a binding whose name is
//!   forbidden (because the caller renamed an outer symbol, as the bundler does when it
//!   hoists modules into one scope); the new name is unique in the whole file.
//!
//! Symbols in scopes that contain a direct `eval` keep their names. Callers fix names of
//! chosen symbols up front with [`RenameOptions::fixed`] (the bundler fixes module-scope
//! symbols, which are shared across modules).

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Ast, Atom, Node, NodeId, Span};

use crate::names::{is_reserved, nth_name};
use crate::scope::{ScopeId, ScopeKind, Scopes, SymbolId, Through};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameMode {
    Mangle,
    Deconflict,
}

pub struct RenameOptions<'a> {
    pub mode: RenameMode,
    /// Final names chosen by the caller; never changed here.
    pub fixed: &'a FxHashMap<SymbolId, String>,
    /// Also rename module-scope symbols that aren't fixed (only safe when the program's
    /// top level is not global, e.g. an ES module or a function-wrapped module).
    pub include_module_scope: bool,
    /// Names no binding may take (globals and synthesized references the caller adds).
    pub reserved: &'a dyn Fn(&str) -> bool,
    /// Further names module-scope bindings may not take (the bundler's other top-level
    /// names, when a module shares the file's top level but names its own bindings).
    pub module_reserved: &'a dyn Fn(&str) -> bool,
}

impl std::fmt::Debug for RenameOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenameOptions")
            .field("mode", &self.mode)
            .field("include_module_scope", &self.include_module_scope)
            .finish()
    }
}

/// Computes new names for symbols (fixed names included in the result).
pub fn compute(
    ast: &Ast,
    scopes: &Scopes,
    opts: &RenameOptions<'_>,
) -> FxHashMap<SymbolId, String> {
    let through = scopes.through();
    let mut out: FxHashMap<SymbolId, String> = FxHashMap::default();
    for (&s, n) in opts.fixed {
        out.insert(s, n.clone());
    }
    // Names used anywhere in the file (for unique names in deconflict mode).
    let mut taken: FxHashSet<String> = FxHashSet::default();
    if opts.mode == RenameMode::Deconflict {
        for s in &scopes.symbols {
            taken.insert(ast.atom(s.name).to_owned());
        }
        for &g in scopes.globals.keys() {
            taken.insert(ast.atom(g).to_owned());
        }
        for n in opts.fixed.values() {
            taken.insert(n.clone());
        }
    }
    let name_of = |out: &FxHashMap<SymbolId, String>, s: SymbolId| -> String {
        match out.get(&s) {
            Some(n) => n.clone(),
            None => ast.atom(scopes.symbol(s).name).to_owned(),
        }
    };
    let mut forbidden: FxHashSet<String> = FxHashSet::default();
    let mut used: FxHashSet<String> = FxHashSet::default();
    for (si, scope) in scopes.scopes.iter().enumerate() {
        if scope.symbols.is_empty() {
            continue;
        }
        let is_module = si == 0;
        if is_module && !opts.include_module_scope && opts.mode == RenameMode::Mangle {
            continue;
        }
        let frozen = scope.contains_eval || (scopes.has_with && scope.kind != ScopeKind::Module);
        forbidden.clear();
        for t in &through[si] {
            match *t {
                Through::Symbol(s) => {
                    forbidden.insert(name_of(&out, s));
                }
                Through::Global(a) => {
                    forbidden.insert(ast.atom(a).to_owned());
                }
            }
        }
        match opts.mode {
            RenameMode::Deconflict => {
                for &sym in &scope.symbols {
                    if out.contains_key(&sym) || frozen {
                        continue;
                    }
                    if is_module && !opts.include_module_scope {
                        continue;
                    }
                    let name = ast.atom(scopes.symbol(sym).name);
                    let blocked = |n: &str| {
                        forbidden.contains(n)
                            || (opts.reserved)(n)
                            || is_module && (opts.module_reserved)(n)
                    };
                    if !blocked(name) {
                        continue;
                    }
                    let mut i = 2u32;
                    let fresh = loop {
                        let cand = format!("{name}{i}");
                        if !taken.contains(&cand) && !blocked(&cand) {
                            break cand;
                        }
                        i += 1;
                    };
                    taken.insert(fresh.clone());
                    out.insert(sym, fresh);
                }
            }
            RenameMode::Mangle => {
                used.clear();
                let mut todo: Vec<SymbolId> = Vec::with_capacity(scope.symbols.len());
                for &sym in &scope.symbols {
                    if out.contains_key(&sym) || frozen {
                        used.insert(name_of(&out, sym));
                    } else {
                        todo.push(sym);
                    }
                }
                // Most-used first; ties keep declaration order (stable output).
                todo.sort_by_key(|&s| {
                    let sy = scopes.symbol(s);
                    (std::cmp::Reverse(sy.refs + sy.decls), s.0)
                });
                let mut cursor = 0usize;
                for sym in todo {
                    let name = loop {
                        let cand = nth_name(cursor);
                        cursor += 1;
                        if is_reserved(&cand)
                            || forbidden.contains(&cand)
                            || used.contains(&cand)
                            || (opts.reserved)(&cand)
                            || is_module && (opts.module_reserved)(&cand)
                        {
                            continue;
                        }
                        break cand;
                    };
                    used.insert(name.clone());
                    out.insert(sym, name);
                }
            }
        }
    }
    // Drop identity renames.
    out.retain(|&s, n| ast.atom(scopes.symbol(s).name) != n.as_str());
    out
}

/// Applies `renames` to every declaring and referencing identifier. Shorthand forms whose
/// identifier changes are expanded (`{ a }` becomes `{ a: b }`, `export { a }` becomes
/// `export { b as a }`, `import { a }` becomes `import { a as b }`).
pub fn apply(ast: &mut Ast, scopes: &Scopes, renames: &FxHashMap<SymbolId, String>) {
    if renames.is_empty() {
        return;
    }
    let atoms: FxHashMap<SymbolId, Atom> =
        renames.iter().map(|(&s, n)| (s, ast.intern(n))).collect();
    let renamed =
        |id: NodeId| -> bool { scopes.symbol_of(id).is_some_and(|s| atoms.contains_key(&s)) };
    // 1. Expand shorthands while the identifiers still have their old names.
    let count = ast.nodes.len();
    for i in 1..count {
        let Some(id) = NodeId::from_raw(i as u32) else {
            continue;
        };
        match *ast.node(id) {
            Node::ShorthandProp {
                modifiers,
                name,
                init,
            } if renamed(name) => {
                let key = copy_ident(ast, name);
                let value = match init {
                    Some(init) => ast.add(
                        Node::Assign {
                            left: name,
                            op: ts_r_ast::AssignOp::Assign,
                            right: init,
                        },
                        Span::new(ast.span(name).start, ast.span(name).start),
                    ),
                    None => name,
                };
                *ast.node_mut(id) = Node::Property {
                    modifiers,
                    key,
                    value,
                };
            }
            Node::BindingProperty {
                key,
                value: None,
                init,
            } if renamed(key) => {
                let new_key = copy_ident(ast, key);
                *ast.node_mut(id) = Node::BindingProperty {
                    key: new_key,
                    value: Some(key),
                    init,
                };
            }
            Node::ExportSpecifier {
                type_only,
                local,
                exported: None,
            } if renamed(local) => {
                let exported = copy_ident(ast, local);
                *ast.node_mut(id) = Node::ExportSpecifier {
                    type_only,
                    local,
                    exported: Some(exported),
                };
            }
            Node::ImportSpecifier {
                type_only,
                imported: None,
                local,
            } if renamed(local) => {
                let imported = copy_ident(ast, local);
                *ast.node_mut(id) = Node::ImportSpecifier {
                    type_only,
                    imported: Some(imported),
                    local,
                };
            }
            _ => {}
        }
    }
    // 2. Rename identifiers.
    for i in 1..count {
        let Some(id) = NodeId::from_raw(i as u32) else {
            continue;
        };
        if let Node::Ident { .. } = ast.node(id)
            && let Some(s) = scopes.symbol_of(id)
            && let Some(&a) = atoms.get(&s)
        {
            *ast.node_mut(id) = Node::Ident { name: a };
        }
    }
}

/// A copy of an identifier node (same name and span) that no symbol points at.
fn copy_ident(ast: &mut Ast, id: NodeId) -> NodeId {
    let n = *ast.node(id);
    let span = ast.span(id);
    ast.add(n, span)
}

/// The first `n`-th-name not rejected by `taken` (helper for callers naming module-level
/// symbols themselves, like the bundler).
pub fn next_free_name(cursor: &mut usize, taken: &dyn Fn(&str) -> bool) -> String {
    loop {
        let cand = nth_name(*cursor);
        *cursor += 1;
        if !is_reserved(&cand) && !taken(&cand) {
            return cand;
        }
    }
}

/// Scope of a symbol's declaration (convenience for callers).
pub fn scope_of(scopes: &Scopes, s: SymbolId) -> ScopeId {
    scopes.symbol(s).scope
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;
    use ts_r_codegen::{CodegenOptions, Layout};

    fn run(src: &str, mode: RenameMode, module: bool, fixed: &[(&str, &str)]) -> String {
        let mut ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let scopes = Scopes::analyze(&ast, true);
        let mut f = FxHashMap::default();
        for (from, to) in fixed {
            let s = scopes
                .module_symbols()
                .iter()
                .copied()
                .find(|&s| ast.atom(scopes.symbol(s).name) == *from)
                .unwrap();
            f.insert(s, (*to).to_owned());
        }
        let r = compute(
            &ast,
            &scopes,
            &RenameOptions {
                mode,
                fixed: &f,
                include_module_scope: module,
                reserved: &|_| false,
                module_reserved: &|_| false,
            },
        );
        apply(&mut ast, &scopes, &r);
        let o = CodegenOptions {
            layout: Layout::Compact,
            ..CodegenOptions::default()
        };
        ts_r_codegen::print(&ast, src, &o).code
    }

    #[test]
    fn mangles_locals_not_globals() {
        assert_eq!(
            run(
                "function outer(longName, other) { let inner = longName + other; return inner + window; }",
                RenameMode::Mangle,
                false,
                &[]
            ),
            "function outer(e,t){let n=e+t;return n+window}"
        );
    }

    #[test]
    fn avoids_capturing_outer_names() {
        // `e` is the mangled module-level name; the function's binding must not take it.
        let out = run(
            "let topLevel = 1; function f(param) { return param + topLevel; }",
            RenameMode::Mangle,
            true,
            &[],
        );
        assert_eq!(out, "let e=1;function t(t){return t+e}");
    }

    #[test]
    fn deconflict_renames_only_captures() {
        let out = run(
            "import { a } from 'x'; function f() { let b = 1; return a + b; }",
            RenameMode::Deconflict,
            false,
            &[("a", "b")],
        );
        assert_eq!(
            out,
            "import{a as b}from'x';function f(){let b2=1;return b+b2}"
        );
    }

    #[test]
    fn shorthands_expand() {
        let out = run(
            "function f(value) { const { x } = value; return { value, x }; }",
            RenameMode::Mangle,
            false,
            &[],
        );
        assert_eq!(out, "function f(e){const{x:t}=e;return{value:e,x:t}}");
    }

    #[test]
    fn block_let_avoids_names_of_vars_declared_in_the_block() {
        // `{ let e; var e; }` is a redeclaration: the unused var must stay distinct.
        let out = run(
            "function f(t) { if (t) { let a = 1, b = 2; var unused = a, other = b; g(a, b); } }",
            RenameMode::Mangle,
            false,
            &[],
        );
        assert!(
            ts_r_parser::parse(&out, SourceKind::Js)
                .diagnostics
                .is_empty(),
            "{out}"
        );
        let block = &out[out.find("{let ").expect(&out)..];
        let lets: Vec<&str> = block[5..block.find(';').unwrap()]
            .split(',')
            .map(|d| d.split('=').next().unwrap())
            .collect();
        let vars = &block[block.find("var ").unwrap() + 4..];
        let vars: Vec<&str> = vars[..vars.find(';').unwrap()]
            .split(',')
            .map(|d| d.split('=').next().unwrap())
            .collect();
        assert!(lets.iter().all(|l| !vars.contains(l)), "{out}");
    }

    #[test]
    fn eval_freezes_names() {
        let out = run(
            "function f(keep) { eval('keep'); return function g(y) { return y; }; }",
            RenameMode::Mangle,
            false,
            &[],
        );
        assert!(out.contains("f(keep)"), "{out}");
        assert!(out.contains("(e){return e}"), "{out}");
    }
}
