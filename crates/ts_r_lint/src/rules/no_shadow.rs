//! `no-shadow`: a declaration that shadows one of an enclosing scope. Shared with
//! `@typescript-eslint/no-shadow` (which defaults `hoist` to `functions-and-types`, reports
//! enum members as `noEnumShadow` and narrows a few exemptions).

use super::prelude::*;
use crate::semantic::{DeclKind, ScopeKind, SymbolFlags, SymbolId};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-shadow",
        "Disallow variable declarations from shadowing variables declared in the outer scope",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoShadow>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hoist {
    All,
    Functions,
    Never,
    Types,
    FunctionsAndTypes,
}

#[derive(Debug, Default)]
pub(crate) struct NoShadow {
    builtin_globals: bool,
    /// `None`: the rule's default.
    hoist: Option<Hoist>,
    allow: Vec<String>,
    ignore_on_initialization: bool,
    ignore_type_value_shadow: bool,
    ignore_fn_type_param: bool,
}

impl FromOptions for NoShadow {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let hoist = match str_field(o, "hoist") {
            Some("all") => Some(Hoist::All),
            Some("functions") => Some(Hoist::Functions),
            Some("never") => Some(Hoist::Never),
            Some("types") => Some(Hoist::Types),
            Some("functions-and-types") => Some(Hoist::FunctionsAndTypes),
            _ => None,
        };
        let allow = o
            .get("allow")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Ok(NoShadow {
            builtin_globals: bool_field(o, "builtinGlobals", false),
            hoist,
            allow,
            ignore_on_initialization: bool_field(o, "ignoreOnInitialization", false),
            ignore_type_value_shadow: bool_field(o, "ignoreTypeValueShadow", true),
            ignore_fn_type_param: bool_field(o, "ignoreFunctionTypeParameterNameValueShadow", true),
        })
    }
}

/// The shadowed binding: a declared symbol, or a global without a declaration.
#[derive(Clone, Copy)]
enum Shadowed {
    Symbol(SymbolId),
    Global,
}

impl Rule for NoShadow {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ts = cx.rule_name().starts_with("@typescript-eslint/");
        let hoist = self.hoist.unwrap_or(if ts {
            Hoist::FunctionsAndTypes
        } else {
            Hoist::Functions
        });
        let sem = cx.semantic();
        let ast = cx.ast();
        let dts = [".d.ts", ".d.cts", ".d.mts"]
            .iter()
            .any(|e| cx.file.path.ends_with(e));
        let mut reports: Vec<(Span, String, &'static str)> = Vec::new();
        for (si, scope) in sem.scopes().iter().enumerate() {
            let Some(upper) = scope.parent else { continue };
            if scope.kind == ScopeKind::Global || in_global_augmentation(cx, si) {
                continue;
            }
            for &sym in &scope.symbols {
                let s = sem.symbol(sym);
                let Some(d0) = s.decls.first() else { continue };
                if matches!(d0.kind, DeclKind::Arguments | DeclKind::CommentGlobal) {
                    continue;
                }
                let name = ast.atom(s.name);
                if name == "this" || self.allow.iter().any(|a| a == name) {
                    continue;
                }
                if dts && s.decls.iter().any(|d| is_declare(ast, d.node, d.parent)) {
                    continue;
                }
                let shadowed = match sem.lookup(upper, name) {
                    Some(o) if !sem.symbol(o).flags.contains(SymbolFlags::COMMENT_GLOBAL) => {
                        Shadowed::Symbol(o)
                    }
                    Some(_) => Shadowed::Global,
                    None if sem.is_global(name) => Shadowed::Global,
                    None => continue,
                };
                if matches!(shadowed, Shadowed::Global) && !self.builtin_globals {
                    continue;
                }
                if self.type_value_shadow(cx, sym, shadowed, ts)
                    || self.fn_type_param_shadow(cx, sym, shadowed, ts)
                    || static_generic_shadow(cx, sym, shadowed, ts)
                    || external_merging(cx, si, sym, shadowed, ts)
                {
                    continue;
                }
                let Shadowed::Symbol(o) = shadowed else {
                    reports.push((
                        name_span(cx, d0.name),
                        format!("'{name}' is already a global variable."),
                        "noShadowGlobal",
                    ));
                    continue;
                };
                if on_initializer(cx, sym, o)
                    || (self.ignore_on_initialization && init_pattern(cx, si, o))
                    || (hoist != Hoist::All && in_tdz(cx, sym, o, hoist))
                {
                    continue;
                }
                let od = sem.symbol(o).decls[0];
                let lc = cx
                    .file
                    .lines
                    .line_col_utf16(cx.src(), cx.span(od.name).start);
                let (line, col) = (lc.line + 1, lc.col + 1);
                let is_enum = ts && sem.symbol(o).decls.iter().any(|d| d.kind == DeclKind::Enum);
                if is_enum {
                    reports.push((
                        name_span(cx, d0.name),
                        format!(
                            "Enum members are added to the enum scope, so references to '{name}' in enum member initializers resolve to this member instead of the declaration in the upper scope on line {line} column {col}."
                        ),
                        "noEnumShadow",
                    ));
                } else {
                    reports.push((
                        name_span(cx, d0.name),
                        format!(
                            "'{name}' is already declared in the upper scope on line {line} column {col}."
                        ),
                        "noShadow",
                    ));
                }
            }
        }
        for (span, msg, id) in reports {
            cx.report(span, msg).id(id);
        }
    }
}

impl NoShadow {
    /// A type and a value of the same name (`ignoreTypeValueShadow`).
    fn type_value_shadow(
        &self,
        cx: &Ctx<'_, '_>,
        sym: SymbolId,
        shadowed: Shadowed,
        ts: bool,
    ) -> bool {
        if !self.ignore_type_value_shadow {
            return false;
        }
        let sem = cx.semantic();
        let shadowed_value = match shadowed {
            Shadowed::Global => true,
            Shadowed::Symbol(o) => !is_type_import(cx, o, ts) && sem.symbol(o).is_value(),
        };
        sem.symbol(sym).is_value() != shadowed_value
    }

    /// A parameter of a function type or signature
    /// (`ignoreFunctionTypeParameterNameValueShadow`).
    fn fn_type_param_shadow(
        &self,
        cx: &Ctx<'_, '_>,
        sym: SymbolId,
        shadowed: Shadowed,
        ts: bool,
    ) -> bool {
        if !self.ignore_fn_type_param {
            return false;
        }
        let sem = cx.semantic();
        if ts
            && let Shadowed::Symbol(o) = shadowed
            && !sem.symbol(o).is_value()
        {
            return false;
        }
        let s = sem.symbol(sym);
        let is_sig = |d: &crate::semantic::Decl| {
            d.kind == DeclKind::Param
                && cx.parent(d.node).is_some_and(|f| {
                    matches!(
                        cx.node(f),
                        Node::FunctionType { .. }
                            | Node::ConstructorType { .. }
                            | Node::CallSignature { .. }
                            | Node::ConstructSignature { .. }
                            | Node::MethodSignature { .. }
                            | Node::FunctionDecl { body: None, .. }
                            | Node::FunctionExpr { body: None, .. }
                    )
                })
        };
        if ts {
            s.decls.iter().all(is_sig)
        } else {
            s.decls.iter().any(is_sig)
        }
    }
}

/// The first declaration of `o` is a type-only import (core: any `type` specifier of its
/// import declaration).
fn is_type_import(cx: &Ctx<'_, '_>, o: SymbolId, ts: bool) -> bool {
    let sem = cx.semantic();
    let d = sem.symbol(o).decls[0];
    if !matches!(
        d.kind,
        DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed
    ) {
        return false;
    }
    let Node::ImportDecl {
        type_only,
        specifiers,
        ..
    } = *cx.node(d.parent)
    else {
        return false;
    };
    if type_only {
        return true;
    }
    let spec_type = |s: NodeId| {
        matches!(
            cx.node(s),
            Node::ImportSpecifier {
                type_only: true,
                ..
            }
        )
    };
    if ts {
        spec_type(d.node)
    } else {
        cx.ast().list(specifiers).iter().any(|&s| spec_type(s))
    }
}

/// A `declare` declaration (for `.d.ts` files).
fn is_declare(ast: &ts_r_ast::Ast, node: NodeId, parent: NodeId) -> bool {
    let m = match *ast.node(parent) {
        Node::VarDecl { modifiers, .. } => modifiers,
        _ => match *ast.node(node) {
            Node::ClassDecl { modifiers, .. }
            | Node::Enum { modifiers, .. }
            | Node::Module { modifiers, .. } => modifiers,
            _ => return false,
        },
    };
    m.contains(Modifiers::DECLARE)
}

/// Scope `si` is inside `declare global {}`.
fn in_global_augmentation(cx: &Ctx<'_, '_>, si: usize) -> bool {
    let sem = cx.semantic();
    let mut s = Some(crate::semantic::ScopeId(si as u32));
    while let Some(id) = s {
        let sc = sem.scope(id);
        if sc.kind == ScopeKind::TsModule
            && matches!(
                cx.node(sc.node),
                Node::Module {
                    kind: ModuleKind::Global,
                    ..
                }
            )
        {
            return true;
        }
        s = sc.parent;
    }
    false
}

/// The `TypeParam` declaring `sym`, and the node its `TypeParams` belong to.
fn type_param_owner(cx: &Ctx<'_, '_>, sym: SymbolId) -> Option<NodeId> {
    let d = cx.semantic().symbol(sym).decls[0];
    if d.kind != DeclKind::TypeParam {
        return None;
    }
    let tps = cx.parent(d.node)?;
    matches!(cx.node(tps), Node::TypeParams { .. }).then_some(())?;
    cx.parent(tps)
}

/// A static method's type parameter (ts: shadowing its class's type parameter).
fn static_generic_shadow(cx: &Ctx<'_, '_>, sym: SymbolId, shadowed: Shadowed, ts: bool) -> bool {
    let Some(f) = type_param_owner(cx, sym) else {
        return false;
    };
    let is_static = matches!(cx.node(f), Node::FunctionExpr { .. })
        && cx.parent(f).is_some_and(|m| {
            matches!(cx.node(m), Node::MethodDef { modifiers, .. } if modifiers.contains(Modifiers::STATIC))
        });
    if !is_static {
        return false;
    }
    if !ts {
        return true;
    }
    let Shadowed::Symbol(o) = shadowed else {
        return false;
    };
    type_param_owner(cx, o)
        .is_some_and(|c| matches!(cx.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. }))
}

/// A type import merged into by `declare module "<its source>" { interface X {} }`.
fn external_merging(
    cx: &Ctx<'_, '_>,
    si: usize,
    sym: SymbolId,
    shadowed: Shadowed,
    ts: bool,
) -> bool {
    let Shadowed::Symbol(o) = shadowed else {
        return false;
    };
    if !is_type_import(cx, o, ts) {
        return false;
    }
    let sem = cx.semantic();
    let Node::ImportDecl { source, .. } = *cx.node(sem.symbol(o).decls[0].parent) else {
        return false;
    };
    let Node::StringLit { value } = *cx.node(source) else {
        return false;
    };
    let src = cx.ast().atom(value);
    let d = sem.symbol(sym).decls[0];
    let module = if ts {
        if !matches!(d.kind, DeclKind::Interface | DeclKind::TypeAlias) {
            return false;
        }
        let sc = sem.scope(crate::semantic::ScopeId(si as u32));
        (sc.kind == ScopeKind::TsModule).then_some(sc.node)
    } else {
        let mut n = cx.parent(d.name);
        let mut found = None;
        while let Some(p) = n {
            if matches!(cx.node(p), Node::Module { .. }) {
                found = Some(p);
                break;
            }
            n = cx.parent(p);
        }
        found
    };
    let Some(m) = module else { return false };
    let Node::Module { name, .. } = *cx.node(m) else {
        return false;
    };
    match *cx.node(name) {
        Node::StringLit { value } => cx.ast().atom(value) == src,
        Node::Ident { .. } => !ts && ident_name(cx.ast(), name) == Some(src),
        _ => false,
    }
}

/// The initializer that `name` (a declaration's identifier) gets: a declarator's `init`, a
/// default value.
fn initializer_of(cx: &Ctx<'_, '_>, name: NodeId) -> Option<NodeId> {
    match *cx.node(cx.parent(name)?) {
        Node::VarDeclarator { init, .. } => init,
        Node::AssignPattern { left, right } if left == name => Some(right),
        Node::Param { pattern, init, .. } if pattern == name => init,
        Node::BindingProperty { init, .. } => init,
        _ => None,
    }
}

/// `const a = function a() {}` (or a class, possibly behind `||`, `&&` or `? :`).
fn on_initializer(cx: &Ctx<'_, '_>, sym: SymbolId, o: SymbolId) -> bool {
    let sem = cx.semantic();
    let d = sem.symbol(sym).decls[0];
    let expr_name = match d.kind {
        DeclKind::FunctionExprName => matches!(cx.node(d.node), Node::FunctionExpr { .. }),
        DeclKind::ClassExprName => matches!(cx.node(d.node), Node::ClassExpr { .. }),
        _ => false,
    };
    if !expr_name {
        return false;
    }
    let Some(init) = initializer_of(cx, sem.symbol(o).decls[0].name) else {
        return false;
    };
    let mut n = d.node;
    for _ in 0..10_000 {
        if n == init {
            return true;
        }
        let Some(p) = cx.parent(n) else { return false };
        let up = match *cx.node(p) {
            Node::Paren { .. } => true,
            Node::Binary { op, .. } => {
                matches!(
                    op,
                    BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce
                )
            }
            Node::Conditional { test, .. } => test != n,
            _ => false,
        };
        if !up {
            return false;
        }
        n = p;
    }
    false
}

/// `ignoreOnInitialization`: the shadowing function runs inside the shadowed binding's own
/// initializer (`const a = f(a => a)`).
fn init_pattern(cx: &Ctx<'_, '_>, si: usize, o: SymbolId) -> bool {
    let sem = cx.semantic();
    let mut vs = crate::semantic::ScopeId(si as u32);
    for _ in 0..10_000 {
        let sc = sem.scope(vs);
        if sc.kind.is_var_scope() {
            break;
        }
        match sc.parent {
            Some(p) => vs = p,
            None => return false,
        }
    }
    let vscope = sem.scope(vs);
    if !matches!(
        cx.node(vscope.node),
        Node::Arrow { .. } | Node::FunctionExpr { .. }
    ) {
        return false;
    }
    let mut outer = vscope.parent;
    if let Some(u) = outer
        && sem.scope(u).kind == ScopeKind::FunctionExprName
    {
        outer = sem.scope(u).parent;
    }
    if outer != Some(sem.symbol(o).scope) {
        return false;
    }
    let mut call = cx.parent(vscope.node);
    while let Some(c) = call {
        if matches!(cx.node(c), Node::Call { .. }) {
            break;
        }
        call = cx.parent(c);
    }
    let Some(call) = call else { return false };
    let loc = cx.span(call).end;
    let within = |n: Option<NodeId>| {
        n.is_some_and(|n| {
            let s = cx.span(n);
            s.start <= loc && loc <= s.end
        })
    };
    let mut n = Some(sem.symbol(o).decls[0].name);
    while let Some(id) = n {
        match *cx.node(id) {
            Node::VarDeclarator { init, .. } => {
                if within(init) {
                    return true;
                }
                if let Some(vd) = cx.parent(id)
                    && let Some(f) = cx.parent(vd)
                    && let Node::ForIn { right, .. } | Node::ForOf { right, .. } = *cx.node(f)
                {
                    return within(Some(right));
                }
                return false;
            }
            Node::AssignPattern { right, .. } => {
                if within(Some(right)) {
                    return true;
                }
            }
            Node::Param { init, .. } | Node::BindingProperty { init, .. } => {
                if within(init) {
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

/// The shadowing declaration comes before the shadowed one, which isn't hoisted.
fn in_tdz(cx: &Ctx<'_, '_>, sym: SymbolId, o: SymbolId, hoist: Hoist) -> bool {
    let sem = cx.semantic();
    let inner = cx.span(sem.symbol(sym).decls[0].name);
    let od = sem.symbol(o).decls[0];
    if inner.end >= cx.span(od.name).start {
        return false;
    }
    let is_fn = matches!(cx.node(od.node), Node::FunctionDecl { body: Some(_), .. });
    let is_type = matches!(
        cx.node(od.node),
        Node::Interface { .. } | Node::TypeAlias { .. }
    );
    match hoist {
        Hoist::Types => !is_type,
        Hoist::FunctionsAndTypes => !is_fn && !is_type,
        Hoist::Functions => !is_fn,
        Hoist::Never | Hoist::All => true,
    }
}

/// ESTree's identifier range: a parameter's or declarator's name with its type annotation.
pub(crate) fn name_span(cx: &Ctx<'_, '_>, name: NodeId) -> Span {
    let span = cx.span(name);
    let ann = cx.parent(name).and_then(|p| match *cx.node(p) {
        Node::Param {
            pattern, type_ann, ..
        } if pattern == name => type_ann,
        Node::VarDeclarator {
            name: n, type_ann, ..
        } if n == name => type_ann,
        _ => None,
    });
    ann.map_or(span, |t| Span::new(span.start, cx.span(t).end))
}
