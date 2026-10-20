//! Expando functions: properties assigned to a function (`f.x = 1`, `f["y"] = 2`) are
//! written as a namespace merged with it, after the function (after its first overload).
//! The layout is tsc 7.0.2's, as observed in its baselines:
//!
//! - one `var name: T;` per assignment (a property assigned twice is written twice), in
//!   source order, typed by the property's type;
//! - an assigned class, function, enum, namespace or import (`f.a = C`) is re-exported:
//!   `export { C as a };`;
//! - a name that is a reserved word or names a value in scope (`f.undefined`, `f.top`)
//!   is written as a generated local (`_a`, `_b`, ... skipping `_i` and `_n`, then `_0`,
//!   `_1`, ...) exported under the name: `var _a: T; export { _a as null };`. With such
//!   export declarations the other `var`s are `export var` (all but the first generated
//!   local, which tsc writes without `export`);
//! - names that are not identifiers (`f[77]`, `f["a b"]`) are dropped;
//! - a `const` initialized by an arrow or function expression with expando properties is
//!   written as a function declaration (`declare function f(): T;`) and the namespace,
//!   unless a property name is not an identifier (then it stays a `const` whose type lists
//!   the properties).

use ts_r_ast::{Modifiers, Node, NodeId, VarKind};
use ts_r_binder::{SymbolFlags, meaning};
use ts_r_checker::Loc;
use ts_r_types::SymId;

use super::{Emitter, Scope, skip_parens};

/// One written member of an expando namespace.
enum Member {
    /// `var name: T;`.
    Var { name: String, prop: SymId },
    /// `export { local as name };`: of a declaration `target`, or (`None`) of the
    /// generated local `var` before it.
    Reexport {
        local: String,
        name: String,
        target: Option<SymId>,
    },
}

/// Reserved words that cannot name a `var` (ES keywords and strict-mode reserved words).
fn is_reserved(name: &str) -> bool {
    matches!(
        name,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "enum"
            | "export"
            | "extends"
            | "false"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "new"
            | "null"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "true"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
    )
}

impl Emitter<'_, '_> {
    /// The expando properties of function symbol `sym` that are written, with whether a
    /// property name is not an identifier (such a property is dropped).
    fn expando_members_of(&mut self, sym: SymId) -> (Vec<(String, SymId, Option<Loc>)>, bool) {
        let props = self.ck.expando_props(sym);
        let mut out: Vec<(u32, String, SymId, Option<Loc>)> = Vec::new();
        let mut other_names = false;
        for (n, p) in props {
            let name = self.ck.store().str(n).to_owned();
            if !ts_r_checker::typenode::is_identifier_text(&name) {
                other_names = true;
                continue;
            }
            let decls: Vec<Loc> = self
                .c
                .decls(p)
                .into_iter()
                .filter(|d| d.file == self.file)
                .collect();
            if decls.is_empty() {
                out.push((u32::MAX, name, p, None));
                continue;
            }
            for d in decls {
                let start = self.ast.span(d.node).start;
                out.push((start, name.clone(), p, Some(d)));
            }
        }
        out.sort_by_key(|m| m.0);
        (
            out.into_iter().map(|(_, n, p, d)| (n, p, d)).collect(),
            other_names,
        )
    }

    /// The symbol an assignment `f.x = C` re-exports: `C` names a class, function, enum,
    /// namespace or import.
    fn reexported_value(&self, d: Loc) -> Option<SymId> {
        let p = self.parents.parent(d.node)?;
        let Node::Assign { left, right, .. } = *self.ast.node(p) else {
            return None;
        };
        if left != d.node {
            return None;
        }
        let right = skip_parens(&self.ast, right);
        if !matches!(self.ast.node(right), Node::Ident { .. }) {
            return None;
        }
        let sym = self.c.resolve_ident(self.loc(right), meaning::VALUE)?;
        let f = self.c.sym_flags(sym);
        f.intersects(
            SymbolFlags::ALIAS
                | SymbolFlags::CLASS
                | SymbolFlags::FUNCTION
                | SymbolFlags::ENUM
                | SymbolFlags::VALUE_MODULE
                | SymbolFlags::VARIABLE,
        )
        .then_some(sym)
    }

    /// A generated local name (`_a`, ... `_z` without `_i` and `_n`, then `_0`, `_1`, ...)
    /// that the file does not use.
    fn expando_temp_name(&mut self) -> String {
        const LETTERS: &[u8] = b"abcdefghjklmopqrstuvwxyz";
        loop {
            let i = self.temp_index as usize;
            self.temp_index += 1;
            let name = match LETTERS.get(i) {
                Some(&c) => format!("_{}", c as char),
                None => format!("_{}", i - LETTERS.len()),
            };
            let used = self.c.file(self.file).ast.atoms.find(&name).is_some();
            if !used && self.generated.insert(name.clone()) {
                return name;
            }
        }
    }

    /// Writes the namespace of function `s`'s expando properties (`sym` is the function's
    /// symbol, `name` its name). `prefix` is written before `namespace`.
    pub(super) fn expando_namespace(&mut self, s: NodeId, sym: SymId, name: &str, prefix: &str) {
        if self.opts.isolated_declarations {
            return;
        }
        let (props, _) = self.expando_members_of(sym);
        if props.is_empty() {
            return;
        }
        let mut members = Vec::new();
        for (pname, prop, decl) in props {
            if let Some(target) = decl.and_then(|d| self.reexported_value(d)) {
                let local = self.c.sym_name(target).to_owned();
                members.push(Member::Reexport {
                    local,
                    name: pname,
                    target: Some(target),
                });
                continue;
            }
            let shadows = is_reserved(&pname)
                || self
                    .ck
                    .resolve_name_at(self.loc(s), &pname, meaning::VALUE)
                    .is_some();
            if shadows {
                let local = self.expando_temp_name();
                members.push(Member::Var {
                    name: local.clone(),
                    prop,
                });
                members.push(Member::Reexport {
                    local,
                    name: pname,
                    target: None,
                });
            } else {
                members.push(Member::Var { name: pname, prop });
            }
        }
        let has_exports = members.iter().any(|m| matches!(m, Member::Reexport { .. }));
        self.w(prefix);
        self.w("namespace ");
        self.w(name);
        self.w(" {");
        self.nl();
        self.indent += 1;
        let mut first_local = true;
        for (i, m) in members.iter().enumerate() {
            match m {
                Member::Var { name: n, prop } => {
                    let generated = matches!(members.get(i + 1),
                        Some(Member::Reexport { local, target: None, .. }) if local == n);
                    let export = has_exports && !(generated && first_local);
                    if generated {
                        first_local = false;
                    }
                    if export {
                        self.w("export ");
                    }
                    self.w("var ");
                    self.w(n);
                    self.w(": ");
                    let t = self.ck.type_of_symbol(*prop);
                    self.inferred_type(t, s);
                    self.w(";");
                    self.nl();
                }
                Member::Reexport {
                    local,
                    name: n,
                    target,
                } => {
                    if let Some(t) = *target {
                        self.reference_symbol(t);
                    }
                    self.w("export { ");
                    self.w(local);
                    if local != n {
                        self.w(" as ");
                        self.w(n);
                    }
                    self.w(" };");
                    self.nl();
                }
            }
        }
        self.indent -= 1;
        self.w("}");
        self.nl();
    }

    /// The function written for `s` when it is a `const` statement whose only declarator is
    /// initialized by an arrow or function expression with expando properties that are all
    /// identifiers: `(declarator name, function node, symbol)`.
    pub(super) fn expando_const(&mut self, s: NodeId) -> Option<(NodeId, NodeId, SymId)> {
        let Node::VarDecl { kind, decls, .. } = *self.ast.node(s) else {
            return None;
        };
        if kind != VarKind::Const || self.opts.isolated_declarations {
            return None;
        }
        let &[d] = self.ast.list(decls) else {
            return None;
        };
        let Node::VarDeclarator {
            name,
            type_ann: None,
            init: Some(init),
            ..
        } = *self.ast.node(d)
        else {
            return None;
        };
        if !matches!(self.ast.node(name), Node::Ident { .. }) {
            return None;
        }
        let f = skip_parens(&self.ast, init);
        if !matches!(
            self.ast.node(f),
            Node::Arrow { .. } | Node::FunctionExpr { .. }
        ) {
            return None;
        }
        let sym = self
            .c
            .sym_of_decl(self.loc(d))
            .or_else(|| self.c.sym_of_decl(self.loc(name)))?;
        let (props, other_names) = self.expando_members_of(sym);
        (!props.is_empty() && !other_names).then_some((name, f, sym))
    }

    /// `function f<T>(params): R;` for an arrow or function expression `f` named `name`.
    pub(super) fn function_from_expr(&mut self, name: &str, f: NodeId) {
        let (type_params, params, return_type) = match *self.ast.node(f) {
            Node::Arrow {
                type_params,
                params,
                return_type,
                ..
            }
            | Node::FunctionExpr {
                type_params,
                params,
                return_type,
                ..
            } => (type_params, params, return_type),
            _ => return,
        };
        self.w("function ");
        // A new name node (observed: unmapped).
        self.w(name);
        self.type_params(type_params);
        self.params(params, f);
        self.w(": ");
        match return_type {
            Some(r) => self.type_node(r),
            None => self.inferred_return(f),
        }
        self.w(";");
        self.nl();
    }

    /// The symbol of function declaration `s` when it has expando properties to write.
    pub(super) fn expando_function(&mut self, s: NodeId) -> Option<SymId> {
        if self.opts.isolated_declarations {
            return None;
        }
        let Node::FunctionDecl { name: Some(_), .. } = *self.ast.node(s) else {
            return None;
        };
        let sym = self.c.sym_of_decl(self.loc(s))?;
        let (props, _) = self.expando_members_of(sym);
        (!props.is_empty()).then_some(sym)
    }

    /// The prefix of an expando namespace for declaration statement `s` in `scope`.
    pub(super) fn expando_prefix(&self, s: NodeId, scope: Scope, keep_export: bool) -> String {
        let mods = self.ast.node(s).modifiers();
        let mut p = String::new();
        if mods.contains(Modifiers::EXPORT)
            && !mods.contains(Modifiers::DEFAULT)
            && (scope.top() || keep_export)
        {
            p.push_str("export ");
        }
        if scope.top() {
            p.push_str("declare ");
        }
        p
    }
}
