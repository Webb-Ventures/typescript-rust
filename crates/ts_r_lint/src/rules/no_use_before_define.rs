//! `no-use-before-define`: a reference that comes before its binding's declaration (or runs
//! during the binding's own initialization). Shared with `@typescript-eslint/no-use-before-define`,
//! which has its own rules for outer-scope bindings, type references and named exports.

use super::prelude::*;
use crate::semantic::{DeclKind, RefFlags, Reference, ScopeId, ScopeKind, Semantic, SymbolId};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-use-before-define",
        "Disallow the use of variables before they are defined",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create::<NoUseBeforeDefine>,
};

#[derive(Debug)]
pub(crate) struct NoUseBeforeDefine {
    functions: bool,
    classes: bool,
    variables: bool,
    allow_named_exports: bool,
    enums: bool,
    typedefs: bool,
    ignore_type_references: bool,
}

impl FromOptions for NoUseBeforeDefine {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut r = NoUseBeforeDefine {
            functions: true,
            classes: true,
            variables: true,
            allow_named_exports: false,
            enums: true,
            typedefs: true,
            ignore_type_references: true,
        };
        match opts.first() {
            Some(Value::String(s)) => r.functions = s != "nofunc",
            Some(Value::Object(o)) => {
                r.functions = bool_field(o, "functions", true);
                r.classes = bool_field(o, "classes", true);
                r.variables = bool_field(o, "variables", true);
                r.allow_named_exports = bool_field(o, "allowNamedExports", false);
                r.enums = bool_field(o, "enums", true);
                r.typedefs = bool_field(o, "typedefs", true);
                r.ignore_type_references = bool_field(o, "ignoreTypeReferences", true);
            }
            _ => {}
        }
        Ok(r)
    }
}

/// The nearest scope that receives `var`s.
fn var_scope(sem: &dyn Semantic, mut s: ScopeId) -> ScopeId {
    for _ in 0..100_000 {
        let sc = sem.scope(s);
        if sc.kind.is_var_scope() {
            return s;
        }
        match sc.parent {
            Some(p) => s = p,
            None => return s,
        }
    }
    s
}

fn in_range(cx: &Ctx<'_, '_>, n: Option<NodeId>, loc: u32) -> bool {
    n.is_some_and(|n| {
        let s = cx.span(n);
        s.start <= loc && loc <= s.end
    })
}

/// The declaration's own initializer (or a `for-in/of` right side, or a default value) runs
/// at `loc`.
fn in_initializer(cx: &Ctx<'_, '_>, name: NodeId, loc: u32) -> bool {
    let mut n = cx.parent(name);
    while let Some(id) = n {
        match *cx.node(id) {
            Node::VarDeclarator { init, .. } => {
                if in_range(cx, init, loc) {
                    return true;
                }
                if let Some(vd) = cx.parent(id)
                    && let Some(f) = cx.parent(vd)
                    && let Node::ForIn { right, .. } | Node::ForOf { right, .. } = *cx.node(f)
                {
                    return in_range(cx, Some(right), loc);
                }
                return false;
            }
            Node::AssignPattern { right, .. } => {
                if in_range(cx, Some(right), loc) {
                    return true;
                }
            }
            Node::Param { init, .. } | Node::BindingProperty { init, .. } => {
                if in_range(cx, init, loc) {
                    return true;
                }
            }
            Node::Arrow { .. }
            | Node::CatchClause { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. }
            | Node::ExportNamed { .. }
            | Node::FunctionDecl { .. }
            | Node::FunctionExpr { .. }
            | Node::ImportDecl { .. } => return false,
            _ => {}
        }
        n = cx.parent(id);
    }
    false
}

/// A static block or the initializer of a static field.
fn is_static_init_scope(cx: &Ctx<'_, '_>, sem: &dyn Semantic, s: ScopeId) -> bool {
    let sc = sem.scope(s);
    match sc.kind {
        ScopeKind::StaticBlock => true,
        ScopeKind::ClassFieldInit => cx.parent(sc.node).is_some_and(|p| {
            matches!(cx.node(p), Node::PropertyDef { modifiers, .. } if modifiers.contains(Modifiers::STATIC))
        }),
        _ => false,
    }
}

/// The reference runs later than the code around it (inside a function), except for
/// static class initializers, which run as the class is defined.
fn separate_context(cx: &Ctx<'_, '_>, r: &Reference, sym: SymbolId) -> bool {
    let sem = cx.semantic();
    let target = var_scope(sem, sem.symbol(sym).scope);
    let mut s = r.scope;
    for _ in 0..100_000 {
        let vs = var_scope(sem, s);
        if vs == target {
            return false;
        }
        if !is_static_init_scope(cx, sem, vs) {
            return true;
        }
        match sem.scope(vs).parent {
            Some(p) => s = p,
            None => return true,
        }
    }
    true
}

/// The reference sits inside a decorator of the class it names.
fn in_class_decorator(cx: &Ctx<'_, '_>, sym: SymbolId, r: &Reference) -> bool {
    let d = cx.semantic().symbol(sym).decls[0];
    if !matches!(d.kind, DeclKind::Class | DeclKind::ClassExprName) {
        return false;
    }
    let decorators = match *cx.node(d.node) {
        Node::ClassDecl { decorators, .. } | Node::ClassExpr { decorators, .. } => decorators,
        _ => return false,
    };
    let rs = cx.span(r.node);
    cx.ast().list(decorators).iter().any(|&dec| {
        let ds = cx.span(dec);
        rs.start >= ds.start && rs.end <= ds.end
    })
}

/// `identifier` is the name of a type reference (`let a: A`).
fn is_type_ref_name(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    cx.parent(ident)
        .is_some_and(|p| matches!(cx.node(p), Node::TypeRef { name, .. } if *name == ident))
}

fn is_named_export(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    cx.parent(ident).is_some_and(
        |p| matches!(cx.node(p), Node::ExportSpecifier { local, .. } if *local == ident),
    )
}

/// The scope belongs to a function type or signature.
fn is_function_type_scope(cx: &Ctx<'_, '_>, s: ScopeId) -> bool {
    let sc = cx.semantic().scope(s);
    sc.kind == ScopeKind::Type
        && matches!(
            cx.node(sc.node),
            Node::FunctionType { .. }
                | Node::ConstructorType { .. }
                | Node::CallSignature { .. }
                | Node::ConstructSignature { .. }
                | Node::MethodSignature { .. }
        )
}

impl NoUseBeforeDefine {
    fn check_core(&self, cx: &Ctx<'_, '_>, r: &Reference) -> bool {
        let sem = cx.semantic();
        if r.flags.contains(RefFlags::INIT) || r.flags.contains(RefFlags::IMPLICIT) {
            return false;
        }
        if self.allow_named_exports && is_named_export(cx, r.node) {
            return false;
        }
        let Some(sym) = r.symbol() else { return false };
        let Some(d) = sem.symbol(sym).decls.first().copied() else {
            return false;
        };
        if matches!(d.kind, DeclKind::Arguments | DeclKind::CommentGlobal) {
            return false;
        }
        let var_like = matches!(
            d.kind,
            DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
        );
        if !self.functions && matches!(d.kind, DeclKind::Function | DeclKind::FunctionExprName) {
            return false;
        }
        if ((!self.variables && var_like)
            || (!self.classes && matches!(d.kind, DeclKind::Class | DeclKind::ClassExprName)))
            && separate_context(cx, r, sym)
        {
            return false;
        }
        if !self.enums && d.kind == DeclKind::Enum {
            return false;
        }
        if !self.typedefs
            && matches!(
                d.kind,
                DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam
            )
        {
            return false;
        }
        let type_ref = is_type_ref_name(cx, r.node);
        if self.ignore_type_references && (r.flags.contains(RefFlags::TYPEOF_TYPE) || type_ref) {
            return false;
        }
        if in_class_decorator(cx, sym, r) {
            return false;
        }
        let def_end = super::no_shadow::name_span(cx, d.name).end;
        let rs = cx.span(r.node);
        if rs.end < def_end {
            return true;
        }
        if type_ref || separate_context(cx, r, sym) {
            return false;
        }
        if d.kind == DeclKind::Class || d.kind == DeclKind::ClassExprName {
            let cs = cx.span(d.node);
            if !(cs.start <= rs.end && rs.end <= cs.end) {
                return false;
            }
            // Not inside a static initializer of that class.
            let members = match *cx.node(d.node) {
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
                _ => return true,
            };
            return !cx.ast().list(members).iter().any(|&m| match *cx.node(m) {
                Node::StaticBlock { .. } => in_range(cx, Some(m), rs.end),
                Node::PropertyDef {
                    modifiers, value, ..
                } => modifiers.contains(Modifiers::STATIC) && in_range(cx, value, rs.end),
                _ => false,
            });
        }
        in_initializer(cx, d.name, rs.end)
    }

    fn check_ts(&self, cx: &Ctx<'_, '_>, r: &Reference) -> bool {
        let sem = cx.semantic();
        if r.flags.contains(RefFlags::INIT) || r.flags.contains(RefFlags::IMPLICIT) {
            return false;
        }
        let sym = r.symbol();
        let decl = sym
            .and_then(|s| sem.symbol(s).decls.first().copied())
            .filter(|d| !matches!(d.kind, DeclKind::Arguments | DeclKind::CommentGlobal));
        let rs = cx.span(r.node);
        let defined_before = |d: crate::semantic::Decl, s: SymbolId| {
            super::no_shadow::name_span(cx, d.name).end <= rs.end
                && !(!r.is_type()
                    && sem.symbol(s).scope == r.scope
                    && in_initializer(cx, d.name, rs.end))
        };
        if !self.allow_named_exports && is_named_export(cx, r.node) {
            return match (sym, decl) {
                (Some(s), Some(d)) => !defined_before(d, s),
                _ => true,
            };
        }
        let (Some(s), Some(d)) = (sym, decl) else {
            return false;
        };
        if defined_before(d, s) {
            return false;
        }
        let outer = var_scope(sem, sem.symbol(s).scope) != var_scope(sem, r.scope);
        let forbidden = if self.ignore_type_references
            && (r.is_type()
                || r.flags.contains(RefFlags::TYPEOF_TYPE)
                || is_named_export(cx, r.node))
        {
            false
        } else if matches!(d.kind, DeclKind::Function | DeclKind::FunctionExprName) {
            self.functions
        } else if matches!(d.kind, DeclKind::Class | DeclKind::ClassExprName) && outer {
            self.classes
        } else if matches!(
            d.kind,
            DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
        ) && outer
        {
            self.variables
        } else if d.kind == DeclKind::Enum && outer {
            self.enums
        } else if matches!(
            d.kind,
            DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam
        ) {
            self.typedefs
        } else {
            true
        };
        forbidden && !in_class_decorator(cx, s, r) && !is_function_type_scope(cx, r.scope)
    }
}

impl Rule for NoUseBeforeDefine {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ts = cx.rule_name().starts_with("@typescript-eslint/");
        let sem = cx.semantic();
        let ast = cx.ast();
        let hits: Vec<NodeId> = sem
            .references()
            .iter()
            .filter(|r| {
                if ts {
                    self.check_ts(cx, r)
                } else {
                    self.check_core(cx, r)
                }
            })
            .map(|r| r.node)
            .collect();
        for n in hits {
            let name = ident_name(ast, n).unwrap_or_default();
            cx.report_node(n, format!("'{name}' was used before it was defined."))
                .id(if ts {
                    "noUseBeforeDefine"
                } else {
                    "usedBeforeDefined"
                });
        }
    }
}
