//! `unicorn/no-non-function-verb-prefix` (type-aware): a binding whose name is a verb
//! (`get`, `set`, `unset`, `delete`, `add`, `remove`, `destroy`, `create`, or the `verbs`
//! option, longest first) followed by an ASCII capital must hold something callable.
//! Variables (not `declare`d), parameters, value imports and import-equals, non-const
//! non-declared enums and class fields with a plain or private name (not abstract or
//! `declare`) are checked, except inside a `declare module`/`namespace` and names matching
//! `ignore`. A type is callable when it has call or construct signatures or is the default
//! library's `Function`/`CallableFunction`/`NewableFunction`; a union is judged on its
//! non-nullish members (all callable), an intersection on any member, type parameters and
//! instantiable types on their constraints. `any`, `unknown`, error types, `null`,
//! `undefined` and `never` are unknown and never reported. Object-pattern shorthands are
//! skipped. Without type information the rule reports nothing, like the plugin without a
//! program; it is not flagged `typed` so that it still runs (silently) there.

use super::prelude::*;
use super::unicorn_names::{RegexOpt, regex_list};
use crate::types::{TypeFlags, TypeInfo};
use ts_r_ast::{Ast, Modifiers};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-non-function-verb-prefix",
        "Disallow non-function values with function-style verb prefixes.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoNonFunctionVerbPrefix>,
};

const DEFAULT_VERBS: [&str; 8] = [
    "get", "set", "unset", "delete", "add", "remove", "destroy", "create",
];

#[derive(Debug)]
struct NoNonFunctionVerbPrefix {
    verbs: Vec<String>,
    ignore: Vec<RegexOpt>,
}

impl FromOptions for NoNonFunctionVerbPrefix {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut verbs: Vec<String> = match o.get("verbs").and_then(Value::as_array) {
            Some(list) => list
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            None => DEFAULT_VERBS.iter().map(|v| (*v).to_owned()).collect(),
        };
        verbs.sort_by_key(|v| std::cmp::Reverse(v.encode_utf16().count()));
        Ok(NoNonFunctionVerbPrefix {
            verbs,
            ignore: regex_list(o.get("ignore"), "ignore")?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Callability {
    Callable,
    NonCallable,
    Unknown,
}

const MAX_DEPTH: u32 = 64;

fn callability(ti: &dyn TypeInfo, t: Ty, depth: u32) -> Callability {
    use Callability::*;
    if depth > MAX_DEPTH || t == Ty::UNRESOLVED || ti.is_error(t) {
        return Unknown;
    }
    let f = ti.flags(t);
    if f.intersects(
        TypeFlags::ANY
            | TypeFlags::UNKNOWN
            | TypeFlags::NULL
            | TypeFlags::UNDEFINED
            | TypeFlags::NEVER,
    ) {
        return Unknown;
    }
    if f.contains(TypeFlags::TYPE_PARAMETER) {
        let c = ti.base_constraint(t);
        return if c == t || c == Ty::UNRESOLVED {
            Unknown
        } else {
            callability(ti, c, depth + 1)
        };
    }
    if f.contains(TypeFlags::UNION) {
        let results: Vec<Callability> = ti
            .union_members(t)
            .into_iter()
            .filter(|&m| {
                !ti.flags(m)
                    .intersects(TypeFlags::NULL | TypeFlags::UNDEFINED)
            })
            .map(|m| callability(ti, m, depth + 1))
            .collect();
        if results.is_empty() || results.contains(&Unknown) {
            return Unknown;
        }
        return if results.iter().all(|&r| r == Callable) {
            Callable
        } else {
            NonCallable
        };
    }
    if !ti.signatures(t, false).is_empty()
        || !ti.signatures(t, true).is_empty()
        || ti
            .type_symbol(t)
            .or_else(|| ti.alias_symbol(t))
            .is_some_and(|s| {
                matches!(
                    ti.symbol_name(s).as_str(),
                    "CallableFunction" | "Function" | "NewableFunction"
                ) && ti.symbol_decls(s).iter().any(|d| d.in_lib)
            })
    {
        return Callable;
    }
    if f.contains(TypeFlags::INTERSECTION) {
        let results: Vec<Callability> = ti
            .intersection_members(t)
            .into_iter()
            .map(|m| callability(ti, m, depth + 1))
            .collect();
        if results.contains(&Callable) {
            return Callable;
        }
        return if results.contains(&Unknown) {
            Unknown
        } else {
            NonCallable
        };
    }
    if f.intersects(TypeFlags::INSTANTIABLE) {
        let c = ti.base_constraint(t);
        if c != t && c != Ty::UNRESOLVED {
            return callability(ti, c, depth + 1);
        }
    }
    NonCallable
}

/// The identifiers a binding pattern declares (object shorthands left out).
fn binding_idents(ast: &Ast, id: NodeId, out: &mut Vec<(NodeId, NodeId)>, depth: u32) {
    if depth > MAX_DEPTH {
        return;
    }
    match *ast.node(id) {
        Node::Ident { .. } => out.push((id, id)),
        Node::Rest { arg } => binding_idents(ast, arg, out, depth + 1),
        Node::AssignPattern { left, .. } => binding_idents(ast, left, out, depth + 1),
        Node::Param { pattern, .. } => binding_idents(ast, pattern, out, depth + 1),
        Node::ArrayPattern { elements } => {
            for &e in ast.list(elements) {
                binding_idents(ast, e, out, depth + 1);
            }
        }
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::BindingProperty { value: Some(v), .. } => {
                        binding_idents(ast, v, out, depth + 1);
                    }
                    Node::Rest { .. } => binding_idents(ast, p, out, depth + 1),
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn in_declared_module(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let mut cur = id;
    for _ in 0..100_000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        if let Node::Module { modifiers, .. } = *cx.node(p)
            && modifiers.contains(Modifiers::DECLARE)
        {
            return true;
        }
        cur = p;
    }
    false
}

impl NoNonFunctionVerbPrefix {
    fn verb_of<'s>(&'s self, name: &str) -> Option<&'s str> {
        self.verbs
            .iter()
            .find(|v| {
                name.starts_with(v.as_str())
                    && name
                        .as_bytes()
                        .get(v.len())
                        .is_some_and(u8::is_ascii_uppercase)
            })
            .map(String::as_str)
    }

    /// Reports `ident` when the type at `type_node` (the identifier, or a class field) is
    /// not callable.
    fn check(&self, cx: &mut Ctx<'_, '_>, ident: NodeId, type_node: NodeId) {
        let ast = cx.ast();
        let name = match *ast.node(ident) {
            Node::Ident { name } => ast.atom(name),
            Node::PrivateName { name } => ast.atom(name),
            _ => return,
        };
        let Some(verb) = self.verb_of(name) else {
            return;
        };
        if in_declared_module(cx, ident) || self.ignore.iter().any(|p| p.test(name)) {
            return;
        }
        let ti = cx.types();
        let Some(t) = ti.type_at(type_node) else {
            return;
        };
        if callability(ti, t, 0) != Callability::NonCallable {
            return;
        }
        cx.report(
            cx.span(ident),
            format!("`{name}` starts with `{verb}`, so it should be a function."),
        )
        .id("no-non-function-verb-prefix");
    }
}

impl Rule for NoNonFunctionVerbPrefix {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::VarDeclarator,
            NodeKind::ImportDefaultSpecifier,
            NodeKind::ImportNamespaceSpecifier,
            NodeKind::ImportSpecifier,
            NodeKind::ImportEquals,
            NodeKind::Enum,
            NodeKind::FunctionDecl,
            NodeKind::FunctionExpr,
            NodeKind::Arrow,
            NodeKind::PropertyDef,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        if !cx.types().is_available() {
            return;
        }
        let ast = cx.ast();
        let mut idents = Vec::new();
        match *cx.node(node) {
            Node::VarDeclarator { name, .. } => {
                if cx.parent(node).is_some_and(|p| {
                    matches!(*ast.node(p), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::DECLARE))
                }) {
                    return;
                }
                binding_idents(ast, name, &mut idents, 0);
            }
            Node::ImportDefaultSpecifier { local } | Node::ImportNamespaceSpecifier { local } => {
                if !import_decl_type_only(cx, node) {
                    idents.push((local, local));
                }
            }
            Node::ImportSpecifier {
                type_only, local, ..
            } => {
                if !type_only && !import_decl_type_only(cx, node) {
                    idents.push((local, local));
                }
            }
            Node::ImportEquals {
                type_only: false,
                name,
                ..
            } => idents.push((name, name)),
            Node::Enum {
                modifiers, name, ..
            } => {
                if !modifiers.intersects(Modifiers::CONST | Modifiers::DECLARE) {
                    idents.push((name, name));
                }
            }
            Node::FunctionDecl { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::Arrow { params, .. } => {
                for &p in ast.list(params) {
                    binding_idents(ast, p, &mut idents, 0);
                }
            }
            Node::PropertyDef { modifiers, key, .. } => {
                if modifiers.intersects(Modifiers::ABSTRACT | Modifiers::DECLARE) {
                    return;
                }
                let class_declared = cx.parent(node).is_some_and(|c| {
                    matches!(
                        *ast.node(c),
                        Node::ClassDecl { modifiers, .. } | Node::ClassExpr { modifiers, .. }
                            if modifiers.contains(Modifiers::DECLARE)
                    )
                });
                if !class_declared
                    && matches!(ast.node(key), Node::Ident { .. } | Node::PrivateName { .. })
                {
                    idents.push((key, node));
                }
            }
            _ => {}
        }
        for (id, type_node) in idents {
            self.check(cx, id, type_node);
        }
    }
}

fn import_decl_type_only(cx: &Ctx<'_, '_>, spec: NodeId) -> bool {
    cx.parent(spec).is_some_and(|d| {
        matches!(
            *cx.node(d),
            Node::ImportDecl {
                type_only: true,
                ..
            }
        )
    })
}
