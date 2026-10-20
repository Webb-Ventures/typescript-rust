//! `no-unused-vars`: disallow unused variables. Also hosts the implementation of
//! `@typescript-eslint/no-unused-vars` (`ts: true`).

use super::prelude::*;
use crate::regex::{self, Regex};
use crate::semantic::{Decl, Reference, Semantic, Symbol};
use rustc_hash::FxHashMap;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-unused-vars",
        "Disallow unused variables",
        ESLINT_REC,
        Category::Correctness,
    )
    .suggestions(),
    create: create::<NoUnusedVars>,
};

pub(crate) fn create_ts(opts: &[Value]) -> Result<Box<dyn Rule>, String> {
    let mut r = NoUnusedVars::from_options(opts)?;
    r.ts = true;
    Ok(Box::new(r))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Args {
    #[default]
    AfterUsed,
    All,
    None,
}

#[derive(Debug, Default)]
struct NoUnusedVars {
    ts: bool,
    vars_local: bool,
    args: Args,
    caught_none: bool,
    vars_pat: Option<String>,
    args_pat: Option<String>,
    caught_pat: Option<String>,
    array_pat: Option<String>,
    ignore_rest_siblings: bool,
    ignore_class_static: bool,
    report_used_ignore: bool,
    ignore_using: bool,
    fix_imports: bool,
}

impl FromOptions for NoUnusedVars {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut r = NoUnusedVars::default();
        match opts.first() {
            Some(Value::String(s)) => r.vars_local = s == "local",
            Some(Value::Object(o)) => {
                r.vars_local = str_field(o, "vars") == Some("local");
                r.args = match str_field(o, "args") {
                    Some("all") => Args::All,
                    Some("none") => Args::None,
                    _ => Args::AfterUsed,
                };
                r.caught_none = str_field(o, "caughtErrors") == Some("none");
                r.vars_pat = str_field(o, "varsIgnorePattern").map(str::to_owned);
                r.args_pat = str_field(o, "argsIgnorePattern").map(str::to_owned);
                r.caught_pat = str_field(o, "caughtErrorsIgnorePattern").map(str::to_owned);
                r.array_pat = str_field(o, "destructuredArrayIgnorePattern").map(str::to_owned);
                r.ignore_rest_siblings = bool_field(o, "ignoreRestSiblings", false);
                r.ignore_class_static = bool_field(o, "ignoreClassWithStaticInitBlock", false);
                r.report_used_ignore = bool_field(o, "reportUsedIgnorePattern", false);
                r.ignore_using = bool_field(o, "ignoreUsingDeclarations", false);
                r.fix_imports = match o.get("enableAutofixRemoval") {
                    Some(Value::Object(e)) => bool_field(e, "imports", false),
                    _ => false,
                };
            }
            _ => {}
        }
        Ok(r)
    }
}

/// Compiled ignore patterns (borrowing the option strings).
struct Pats<'a> {
    vars: Option<Regex<'a>>,
    args: Option<Regex<'a>>,
    caught: Option<Regex<'a>>,
    array: Option<Regex<'a>>,
}

fn matches(p: &Option<Regex<'_>>, name: &str) -> bool {
    p.as_ref().is_some_and(|r| r.is_match(name, false))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefKind {
    Var,
    Param,
    Catch,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Usage {
    Used,
    OnlyType,
    Unused,
}

/// One problem found for a symbol.
struct Found {
    sym: SymbolId,
    span: Span,
    message: String,
    id: &'static str,
}

fn var_scope(sem: &dyn Semantic, mut s: ScopeId) -> ScopeId {
    loop {
        let sc = sem.scope(s);
        if sc.kind.is_var_scope() {
            return s;
        }
        match sc.parent {
            Some(p) => s = p,
            None => return s,
        }
    }
}

fn inside(cx: &Ctx<'_, '_>, inner: NodeId, outer: NodeId) -> bool {
    let (a, b) = (cx.span(inner), cx.span(outer));
    a.start >= b.start && a.end <= b.end
}

/// The parent of `id`, skipping parentheses.
fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut p = cx.parent(id)?;
    while matches!(cx.node(p), Node::Paren { .. }) {
        p = cx.parent(p)?;
    }
    Some(p)
}

/// The result of `node` is discarded (expression statement, non-final sequence item).
fn is_unused_expr(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let ast = cx.ast();
    let mut n = node;
    loop {
        let Some(p) = parent_np(cx, n) else {
            return false;
        };
        match *ast.node(p) {
            Node::ExprStmt { .. } => return true,
            Node::Seq { exprs } => {
                let last = ast.list(exprs).last().map(|&l| skip_parens(ast, l));
                if last != Some(n) {
                    return true;
                }
                n = p;
            }
            _ => return false,
        }
    }
}

fn is_loop(n: &Node) -> bool {
    matches!(
        n,
        Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
    )
}

fn is_in_loop(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    for a in ancestors_inclusive(cx.file.parents, id) {
        let n = cx.node(a);
        if is_function(n.kind()) {
            return false;
        }
        if is_loop(n) {
            return true;
        }
    }
    false
}

fn upper_function(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    ancestors_inclusive(cx.file.parents, id)
        .skip(1)
        .find(|&a| is_function(cx.node(a).kind()))
}

fn is_statement_like(n: &Node) -> bool {
    n.is_statement()
        || matches!(
            n,
            Node::FunctionDecl { .. } | Node::ClassDecl { .. } | Node::VarDecl { .. }
        )
}

/// A function inside `rhs` whose value may be stored somewhere other than the variable
/// being assigned (so a read inside it is a real use).
fn is_storable_function(cx: &Ctx<'_, '_>, func: NodeId, rhs: NodeId) -> bool {
    let ast = cx.ast();
    let mut node = func;
    let mut parent = parent_np(cx, func);
    while let Some(p) = parent {
        if !inside(cx, p, rhs) {
            break;
        }
        match *ast.node(p) {
            Node::Seq { exprs } => {
                if ast.list(exprs).last().map(|&l| skip_parens(ast, l)) != Some(node) {
                    return false;
                }
            }
            Node::Call { callee, .. } | Node::New { callee, .. } => {
                return skip_parens(ast, callee) != node;
            }
            Node::Assign { .. } | Node::TaggedTemplate { .. } | Node::Yield { .. } => return true,
            ref n if is_statement_like(n) => return true,
            _ => {}
        }
        node = p;
        parent = parent_np(cx, p);
    }
    false
}

fn inside_storable_function(cx: &Ctx<'_, '_>, id: NodeId, rhs: NodeId) -> bool {
    upper_function(cx, id).is_some_and(|f| inside(cx, f, rhs) && is_storable_function(cx, f, rhs))
}

/// `a += 1`, `a++` as statements, or a read inside the right side of `a = ...` (all only
/// feed the variable itself).
fn is_read_for_itself(cx: &Ctx<'_, '_>, r: &Reference, rhs: Option<NodeId>) -> bool {
    if !r.is_read() {
        return false;
    }
    let ast = cx.ast();
    let id = r.node;
    if let Some(p) = cx.parent(id) {
        match *ast.node(p) {
            Node::Assign { left, op, .. }
                if left == id
                    && !matches!(
                        op,
                        AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign
                    )
                    && is_unused_expr(cx, p) =>
            {
                return true;
            }
            Node::Update { .. } if is_unused_expr(cx, p) => return true,
            _ => {}
        }
    }
    rhs.is_some_and(|rhs| inside(cx, id, rhs) && !inside_storable_function(cx, id, rhs))
}

fn next_rhs(
    cx: &Ctx<'_, '_>,
    sem: &dyn Semantic,
    sym: &Symbol,
    r: &Reference,
    prev: Option<NodeId>,
) -> Option<NodeId> {
    let ast = cx.ast();
    let id = r.node;
    if let Some(prev) = prev
        && inside(cx, id, prev)
    {
        return Some(prev);
    }
    let later = var_scope(sem, r.scope) != var_scope(sem, sym.scope) || is_in_loop(cx, id);
    if let Some(p) = cx.parent(id)
        && let Node::Assign { left, right, .. } = *ast.node(p)
        && left == id
        && !later
        && is_unused_expr(cx, p)
    {
        return Some(skip_parens(ast, right));
    }
    None
}

/// `for (x in y) return;`-style loops count as a use of `x`.
fn is_for_in_of_ref(cx: &Ctx<'_, '_>, r: &Reference) -> bool {
    let ast = cx.ast();
    let Some(mut target) = cx.parent(r.node) else {
        return false;
    };
    if matches!(ast.node(target), Node::VarDeclarator { .. }) {
        match cx.parent(target).and_then(|d| cx.parent(d)) {
            Some(t) => target = t,
            None => return false,
        }
    }
    let body = match *ast.node(target) {
        Node::ForIn { body, .. } | Node::ForOf { body, .. } => body,
        _ => return false,
    };
    let first = match *ast.node(body) {
        Node::Block { stmts } => match ast.list(stmts).first() {
            Some(&s) => s,
            None => return false,
        },
        _ => body,
    };
    matches!(ast.node(first), Node::Return { .. })
}

fn is_type_ref(r: &Reference) -> bool {
    r.flags.intersects(RefFlags::TYPE | RefFlags::TYPEOF_TYPE)
}

fn is_pure_value(k: DeclKind) -> bool {
    matches!(
        k,
        DeclKind::Var
            | DeclKind::Let
            | DeclKind::Const
            | DeclKind::Using
            | DeclKind::Function
            | DeclKind::Param
            | DeclKind::CatchParam
    )
}

fn is_pure_type(k: DeclKind) -> bool {
    matches!(k, DeclKind::TypeAlias | DeclKind::Interface)
}

/// The declaration statement carries `export`.
fn decl_exported(cx: &Ctx<'_, '_>, d: &Decl) -> bool {
    let m = match *cx.node(d.parent) {
        Node::VarDecl { modifiers, .. }
        | Node::FunctionDecl { modifiers, .. }
        | Node::ClassDecl { modifiers, .. }
        | Node::Interface { modifiers, .. }
        | Node::TypeAlias { modifiers, .. }
        | Node::Enum { modifiers, .. }
        | Node::Module { modifiers, .. }
        | Node::ImportEquals { modifiers, .. } => modifiers,
        _ => match *cx.node(d.node) {
            Node::FunctionDecl { modifiers, .. }
            | Node::ClassDecl { modifiers, .. }
            | Node::Interface { modifiers, .. }
            | Node::TypeAlias { modifiers, .. }
            | Node::Enum { modifiers, .. }
            | Node::Module { modifiers, .. } => modifiers,
            _ => return false,
        },
    };
    m.contains(Modifiers::EXPORT)
}

/// `export {}`, `export =`, `export default x`, `export * from` in a statement list.
fn has_export_statement(ast: &ts_r_ast::Ast, stmts: &[NodeId]) -> bool {
    stmts.iter().any(|&s| {
        matches!(
            ast.node(s),
            Node::ExportNamed { .. }
                | Node::ExportAll { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportAssignment { .. }
        )
    })
}

fn is_ambient_module(cx: &Ctx<'_, '_>, m: NodeId) -> bool {
    if cx.ast().source_kind == ts_r_ast::SourceKind::Dts {
        return true;
    }
    ancestors_inclusive(cx.file.parents, m).any(|a| match *cx.node(a) {
        Node::Module {
            modifiers, kind, ..
        } => modifiers.contains(Modifiers::DECLARE) || kind == ModuleKind::Global,
        _ => false,
    })
}

/// Declarations in an ambient block without explicit exports are implicitly exported.
fn implicitly_exported(cx: &Ctx<'_, '_>, sem: &dyn Semantic, scope: ScopeId) -> bool {
    let ast = cx.ast();
    let node = sem.scope(scope).node;
    match *ast.node(node) {
        Node::Program { stmts } => {
            ast.source_kind == ts_r_ast::SourceKind::Dts
                && !has_export_statement(ast, ast.list(stmts))
        }
        Node::Module { body: Some(b), .. } => {
            is_ambient_module(cx, node)
                && statement_list(ast, b).is_none_or(|s| !has_export_statement(ast, s))
        }
        _ => false,
    }
}

impl NoUnusedVars {
    fn def_kind(d: &Decl) -> DefKind {
        match d.kind {
            DeclKind::Param => DefKind::Param,
            DeclKind::CatchParam => DefKind::Catch,
            _ => DefKind::Var,
        }
    }

    /// Nodes whose inner references don't count as uses (recursion, self-typed
    /// declarations).
    /// The flag marks type-only declarations, where only type references are "self".
    fn self_nodes(&self, cx: &Ctx<'_, '_>, s: &Symbol) -> Vec<(NodeId, bool)> {
        let ast = cx.ast();
        let mut out = Vec::new();
        for d in &s.decls {
            match d.kind {
                DeclKind::Function => out.push((d.node, false)),
                DeclKind::Var | DeclKind::Let | DeclKind::Const => {
                    if let Node::VarDeclarator {
                        name,
                        init: Some(init),
                        ..
                    } = *ast.node(d.node)
                        && name == d.name
                    {
                        let init = skip_parens(ast, init);
                        if matches!(
                            ast.node(init),
                            Node::FunctionExpr { .. } | Node::Arrow { .. }
                        ) {
                            out.push((init, false));
                        }
                    }
                }
                DeclKind::Interface | DeclKind::TypeAlias if self.ts => out.push((d.node, true)),
                DeclKind::Enum | DeclKind::Namespace if self.ts => out.push((d.node, false)),
                _ => {}
            }
        }
        out
    }

    fn usage(&self, cx: &Ctx<'_, '_>, sem: &dyn Semantic, s: &Symbol) -> Usage {
        let self_nodes = self.self_nodes(cx, s);
        let mut value_used = false;
        let mut type_used = false;
        let mut rhs = None;
        for &rid in &s.refs {
            let r = sem.reference(rid);
            if is_for_in_of_ref(cx, r) {
                return Usage::Used;
            }
            let for_itself = is_read_for_itself(cx, r, rhs);
            rhs = next_rhs(cx, sem, s, r, rhs);
            if !r.is_read() || for_itself {
                continue;
            }
            if self_nodes
                .iter()
                .any(|&(n, ty)| inside(cx, r.node, n) && (!ty || r.flags.contains(RefFlags::TYPE)))
            {
                continue;
            }
            if is_type_ref(r) {
                type_used = true;
            } else {
                value_used = true;
            }
            if value_used {
                break;
            }
        }
        let value_only = self.ts
            && s.decls.iter().any(|d| is_pure_value(d.kind))
            && !s
                .decls
                .iter()
                .any(|d| !is_pure_value(d.kind) && !is_pure_type(d.kind));
        if value_used || (type_used && !value_only) {
            Usage::Used
        } else if type_used {
            Usage::OnlyType
        } else {
            Usage::Unused
        }
    }

    fn exported(&self, cx: &Ctx<'_, '_>, sem: &dyn Semantic, s: &Symbol) -> bool {
        let mixed = s.decls.iter().any(|d| is_pure_value(d.kind))
            && s.decls.iter().any(|d| is_pure_type(d.kind));
        if !mixed {
            return s.flags.contains(SymbolFlags::EXPORTED);
        }
        s.decls
            .iter()
            .any(|d| is_pure_value(d.kind) && decl_exported(cx, d))
            || s.refs
                .iter()
                .any(|&r| sem.reference(r).flags.contains(RefFlags::EXPORT))
    }

    fn has_rest_sibling(cx: &Ctx<'_, '_>, sem: &dyn Semantic, s: &Symbol) -> bool {
        let ast = cx.ast();
        let last_is = |list: ts_r_ast::NodeList, rest: fn(&Node) -> bool| {
            ast.list(list).last().is_some_and(|&l| rest(ast.node(l)))
        };
        let decl_side = s.decls.iter().any(|d| {
            cx.parent(d.name).is_some_and(|p| {
                matches!(ast.node(p), Node::BindingProperty { .. })
                    && cx.parent(p).is_some_and(|op| match *ast.node(op) {
                        Node::ObjectPattern { props } => {
                            last_is(props, |n| matches!(n, Node::Rest { .. }))
                        }
                        _ => false,
                    })
            })
        });
        decl_side
            || s.refs.iter().any(|&rid| {
                let r = sem.reference(rid);
                cx.parent(r.node).is_some_and(|p| {
                    matches!(
                        ast.node(p),
                        Node::ShorthandProp { .. } | Node::Property { .. }
                    ) && cx.parent(p).is_some_and(|op| match *ast.node(op) {
                        Node::ObjectLit { props } => {
                            last_is(props, |n| matches!(n, Node::Spread { .. }))
                        }
                        _ => false,
                    })
                })
            })
    }

    /// The parameter is followed by a parameter that is used (`args: "after-used"`).
    fn has_used_after(cx: &Ctx<'_, '_>, sem: &dyn Semantic, d: &Decl) -> bool {
        let ast = cx.ast();
        let Some(f) = cx.parent(d.node) else {
            return false;
        };
        let params = match *ast.node(f) {
            Node::FunctionDecl { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::Arrow { params, .. } => params,
            _ => return false,
        };
        let mut idents = Vec::new();
        for &p in ast.list(params) {
            binding_idents(ast, p, &mut idents);
        }
        let Some(pos) = idents.iter().position(|&i| i == d.name) else {
            return false;
        };
        idents[pos + 1..].iter().any(|&i| {
            sem.symbol_of_decl(i).is_some_and(|s| {
                let s = sem.symbol(s);
                s.flags.contains(SymbolFlags::PARAM_PROPERTY)
                    || s.refs.iter().any(|&r| !is_type_ref(sem.reference(r)))
            })
        })
    }

    fn is_setter_param(cx: &Ctx<'_, '_>, d: &Decl) -> bool {
        cx.parent(d.node)
            .and_then(|f| cx.parent(f))
            .is_some_and(|m| {
                matches!(
                    cx.node(m),
                    Node::MethodDef {
                        kind: MethodKind::Set,
                        ..
                    }
                )
            })
    }

    fn check(
        &self,
        cx: &Ctx<'_, '_>,
        sem: &dyn Semantic,
        pats: &Pats<'_>,
        ambient: &mut FxHashMap<ScopeId, bool>,
        sid: SymbolId,
        out: &mut Vec<Found>,
    ) {
        let ast = cx.ast();
        let s = sem.symbol(sid);
        let Some(d) = s.decls.first() else { return };
        if s.flags.intersects(
            SymbolFlags::FN_EXPR_NAME
                | SymbolFlags::CLASS_EXPR_NAME
                | SymbolFlags::ARGUMENTS
                | SymbolFlags::ENUM_MEMBER
                | SymbolFlags::GLOBAL_AUGMENT,
        ) {
            return;
        }
        // Core `no-unused-vars` on TypeScript code sees signature parameters and parameter
        // properties as ordinary (unused) parameters; the TypeScript variant does not.
        if self.ts
            && s.flags
                .intersects(SymbolFlags::SIGNATURE_PARAM | SymbolFlags::PARAM_PROPERTY)
        {
            return;
        }
        let scope = sem.scope(s.scope);
        if self.vars_local
            && (scope.kind == ScopeKind::Global || (self.ts && scope.kind == ScopeKind::Module))
        {
            return;
        }
        // `namespace a.b {}` declares `a` only as a container.
        if d.kind == DeclKind::Namespace
            && matches!(*cx.node(d.node), Node::Module { name, .. } if matches!(cx.node(name), Node::QualifiedName { .. }))
        {
            return;
        }
        let name = sem.name(sid).to_owned();
        if (scope.kind == ScopeKind::Global || (self.ts && scope.kind == ScopeKind::Module))
            && cx.file.exported.contains(&name)
        {
            return;
        }
        if d.kind == DeclKind::TypeParam && matches!(cx.node(d.parent), Node::MappedType { .. }) {
            return;
        }
        if self.ts
            && *ambient
                .entry(s.scope)
                .or_insert_with(|| implicitly_exported(cx, sem, s.scope))
        {
            return;
        }
        let is_comment_global = d.kind == DeclKind::CommentGlobal;
        let decl_span = if is_comment_global {
            s.comment_span.unwrap_or_else(|| cx.span(d.name))
        } else {
            cx.span(d.name)
        };
        let kind = Self::def_kind(d);
        let def_in_array = !is_comment_global
            && cx
                .parent(d.name)
                .is_some_and(|p| matches!(ast.node(p), Node::ArrayPattern { .. }));
        let array_ref = s.refs.iter().map(|&r| sem.reference(r)).find(|r| {
            r.flags.contains(RefFlags::DESTRUCTURE)
                && cx
                    .parent(r.node)
                    .is_some_and(|p| matches!(ast.node(p), Node::ArrayLit { .. }))
        });
        let used_ignored = |what: &str, pat: &Option<String>, span: Span, out: &mut Vec<Found>| {
            if self.report_used_ignore && self.usage(cx, sem, s) == Usage::Used {
                out.push(Found {
                    sym: sid,
                    span,
                    message: format!(
                        "'{name}' is marked as ignored but is used. Used {what} must not match /{}/u.",
                        pat.as_deref().unwrap_or("")
                    ),
                    id: "usedIgnoredVar",
                });
            }
        };
        if (def_in_array || array_ref.is_some()) && matches(&pats.array, &name) {
            let span = match array_ref {
                Some(r) if self.ts && !def_in_array => cx.span(r.node),
                _ => decl_span,
            };
            used_ignored(
                "elements of array destructuring",
                &self.array_pat,
                span,
                out,
            );
            return;
        }
        if d.kind == DeclKind::Class
            && self.ignore_class_static
            && let Node::ClassDecl { members, .. } = *ast.node(d.node)
            && ast
                .list(members)
                .iter()
                .any(|&m| matches!(ast.node(m), Node::StaticBlock { .. }))
        {
            return;
        }
        match kind {
            DefKind::Catch => {
                if self.caught_none {
                    return;
                }
                if matches(&pats.caught, &name) {
                    used_ignored("caught errors", &self.caught_pat, decl_span, out);
                    return;
                }
            }
            DefKind::Param => {
                if Self::is_setter_param(cx, d) || self.args == Args::None {
                    return;
                }
                if matches(&pats.args, &name) {
                    used_ignored("args", &self.args_pat, decl_span, out);
                    return;
                }
                let simple = matches!(
                    *ast.node(d.node),
                    Node::Param { pattern, init: None, rest: false, .. } if pattern == d.name
                );
                if self.args == Args::AfterUsed && simple && Self::has_used_after(cx, sem, d) {
                    return;
                }
            }
            DefKind::Var => {
                if matches(&pats.vars, &name) {
                    used_ignored("vars", &self.vars_pat, decl_span, out);
                    return;
                }
            }
        }
        if self.ignore_using && d.kind == DeclKind::Using {
            return;
        }
        let usage = self.usage(cx, sem, s);
        if usage == Usage::Used
            || self.exported(cx, sem, s)
            || (self.ignore_rest_siblings && Self::has_rest_sibling(cx, sem, s))
        {
            return;
        }
        let own_scope = var_scope(sem, s.scope);
        let mut assigned = false;
        let mut span = decl_span;
        for &rid in &s.refs {
            let r = sem.reference(rid);
            if r.is_write() {
                assigned = true;
                if var_scope(sem, r.scope) == own_scope {
                    span = cx.span(r.node);
                }
            }
        }
        let action = if assigned {
            "assigned a value"
        } else {
            "defined"
        };
        let additional = if self.array_pat.is_some() && def_in_array {
            format!(
                ". Allowed unused elements of array destructuring must match /{}/u",
                self.array_pat.as_deref().unwrap_or("")
            )
        } else {
            let (what, pat) = match kind {
                DefKind::Catch => ("caught errors", &self.caught_pat),
                DefKind::Param => ("args", &self.args_pat),
                DefKind::Var => ("vars", &self.vars_pat),
            };
            match pat {
                Some(p) => format!(". Allowed unused {what} must match /{p}/u"),
                None => String::new(),
            }
        };
        let (message, id) = if usage == Usage::OnlyType {
            (
                format!("'{name}' is {action} but only used as a type{additional}."),
                "usedOnlyAsType",
            )
        } else {
            (
                format!("'{name}' is {action} but never used{additional}."),
                "unusedVar",
            )
        };
        out.push(Found {
            sym: sid,
            span,
            message,
            id,
        });
    }
}

// ---- removal suggestions ----

fn list_of(ast: &ts_r_ast::Ast, l: ts_r_ast::NodeList) -> Vec<NodeId> {
    ast.list(l).to_vec()
}

/// Removes `item` from a comma-separated list: `, item` after a previous item, else
/// `item,` before the next one.
fn remove_list_item(cx: &Ctx<'_, '_>, items: &[NodeId], item: NodeId) -> Option<Fix> {
    let idx = items.iter().position(|&i| i == item)?;
    let span = cx.span(item);
    if idx > 0 {
        let prev = cx.span(items[idx - 1]);
        return Some(Fix::delete(Span::new(prev.end, span.end)));
    }
    let src = cx.src();
    let comma = skip_trivia_forward(src, span.end as usize);
    if src.as_bytes().get(comma) == Some(&b',') {
        Some(Fix::delete(Span::new(span.start, comma as u32 + 1)))
    } else {
        Some(Fix::delete(span))
    }
}

/// Statements that end with a semicolon (or rely on ASI).
fn needs_semicolon(n: &Node) -> bool {
    matches!(
        n,
        Node::ExprStmt { .. }
            | Node::VarDecl { .. }
            | Node::Return { .. }
            | Node::Throw { .. }
            | Node::Break { .. }
            | Node::Continue { .. }
            | Node::Debugger {}
            | Node::DoWhile { .. }
            | Node::ImportDecl { .. }
            | Node::ImportEquals { .. }
            | Node::ExportNamed { .. }
            | Node::ExportAll { .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. }
            | Node::TypeAlias { .. }
            | Node::Directive { .. }
    )
}

fn remove_statement(cx: &Ctx<'_, '_>, stmt: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let p = cx.parent(stmt)?;
    let Some(list) = statement_list(ast, p) else {
        // The body of `if`, a loop, `with`, ...: keep an empty statement.
        return Some(Fix::replace(cx.span(stmt), ";"));
    };
    let idx = list.iter().position(|&s| s == stmt)?;
    if idx > 0 {
        let prev = list[idx - 1];
        if needs_semicolon(ast.node(prev)) && !cx.text(prev).trim_end().ends_with(';') {
            return None;
        }
    }
    // Removing it would turn a following string statement into a directive.
    let prologue = matches!(ast.node(p), Node::Program { .. })
        || (matches!(ast.node(p), Node::Block { .. })
            && cx.parent(p).is_some_and(|f| is_function(cx.node(f).kind())));
    if prologue
        && list[..idx]
            .iter()
            .all(|&s| matches!(ast.node(s), Node::Directive { .. }))
        && list.get(idx + 1).is_some_and(|&n| {
            matches!(*ast.node(n), Node::ExprStmt { expr } if matches!(ast.node(skip_parens(ast, expr)), Node::StringLit { .. }))
        })
    {
        return None;
    }
    Some(Fix::delete(cx.span(stmt)))
}

fn remove_declarator(cx: &Ctx<'_, '_>, decl: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let vd = cx.parent(decl)?;
    let Node::VarDecl { decls, .. } = *ast.node(vd) else {
        return None;
    };
    if cx.parent(vd).is_some_and(|p| match *ast.node(p) {
        Node::For { init, .. } => init == Some(vd),
        Node::ForIn { left, .. } | Node::ForOf { left, .. } => left == vd,
        _ => false,
    }) {
        return None;
    }
    let decls = list_of(ast, decls);
    if decls.len() == 1 {
        return remove_statement(cx, vd);
    }
    // Dropping the last declarator of a statement without `;` could join it with the next.
    if decls.last() == Some(&decl)
        && !cx.text(vd).trim_end().ends_with(';')
        && skip_trivia_forward(cx.src(), cx.span(vd).end as usize) < cx.src().len()
    {
        return None;
    }
    remove_list_item(cx, &decls, decl)
}

fn remove_param(cx: &Ctx<'_, '_>, param: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let f = cx.parent(param)?;
    let params = match *ast.node(f) {
        Node::FunctionDecl { params, .. }
        | Node::FunctionExpr { params, .. }
        | Node::Arrow { params, .. } => list_of(ast, params),
        _ => return None,
    };
    if params.len() == 1 {
        let span = cx.span(param);
        let after = skip_trivia_forward(cx.src(), span.end as usize);
        if matches!(ast.node(f), Node::Arrow { .. })
            && cx.src().as_bytes().get(after) != Some(&b')')
        {
            return Some(Fix::replace(span, "()"));
        }
        return Some(Fix::delete(span));
    }
    remove_list_item(cx, &params, param)
}

/// Removes the binding `ident` from its declaration, collapsing patterns that become empty.
fn remove_binding(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<Fix> {
    let ast = cx.ast();
    let mut target = ident;
    loop {
        let p = cx.parent(target)?;
        match *ast.node(p) {
            Node::VarDeclarator { name, .. } if name == target => return remove_declarator(cx, p),
            Node::Param { pattern, .. } if pattern == target => return remove_param(cx, p),
            Node::AssignPattern { left, .. } if left == target => target = p,
            Node::Rest { .. } => target = p,
            Node::BindingProperty { .. } => {
                let op = cx.parent(p)?;
                let Node::ObjectPattern { props } = *ast.node(op) else {
                    return None;
                };
                let props = list_of(ast, props);
                if props.len() == 1 {
                    target = op;
                } else {
                    return remove_list_item(cx, &props, p);
                }
            }
            Node::ObjectPattern { props } => {
                let props = list_of(ast, props);
                if props.len() == 1 {
                    target = p;
                } else {
                    return remove_list_item(cx, &props, target);
                }
            }
            Node::ArrayPattern { elements } => {
                let els = list_of(ast, elements);
                let live = els
                    .iter()
                    .filter(|&&e| !matches!(ast.node(e), Node::Hole {}))
                    .count();
                if live <= 1 {
                    target = p;
                    continue;
                }
                let idx = els.iter().position(|&e| e == target)?;
                let span = cx.span(target);
                if idx + 1 == els.len() && idx > 0 {
                    let prev = cx.span(els[idx - 1]);
                    return Some(Fix::delete(Span::new(prev.end, span.end)));
                }
                return Some(Fix::delete(span));
            }
            _ => return None,
        }
    }
}

/// Removes one import specifier (or the whole import when it is the only binding).
fn remove_import_specifier(cx: &Ctx<'_, '_>, spec: NodeId, line_aware: bool) -> Option<Fix> {
    let ast = cx.ast();
    let decl = cx.parent(spec)?;
    let Node::ImportDecl {
        specifiers, source, ..
    } = *ast.node(decl)
    else {
        return None;
    };
    let specs = list_of(ast, specifiers);
    let is_named = |n: NodeId| matches!(ast.node(n), Node::ImportSpecifier { .. });
    if specs.len() == 1 {
        if is_named(spec) {
            return if line_aware {
                Some(remove_whole_line(cx, decl))
            } else {
                remove_statement(cx, decl)
            };
        }
        return Some(Fix::delete(Span::new(
            cx.span(spec).start,
            cx.span(source).start,
        )));
    }
    if is_named(spec) {
        let named: Vec<NodeId> = specs.iter().copied().filter(|&s| is_named(s)).collect();
        if named.len() == 1 {
            // `import a, { b } from` -> `import a from`
            let idx = specs.iter().position(|&s| s == spec)?;
            let prev = cx.span(*specs.get(idx.checked_sub(1)?)?);
            let close = find_char(cx.src(), cx.span(spec).end, b'}')?;
            return Some(Fix::delete(Span::new(prev.end, close + 1)));
        }
        return remove_list_item(cx, &named, spec);
    }
    remove_list_item(cx, &specs, spec)
}

/// Deletes a statement, and its whole line when nothing else is on it.
fn remove_whole_line(cx: &Ctx<'_, '_>, stmt: NodeId) -> Fix {
    let src = cx.src();
    let span = cx.span(stmt);
    let line_start = src[..span.start as usize].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[span.end as usize..]
        .find('\n')
        .map_or(src.len(), |i| span.end as usize + i);
    let before = &src[line_start..span.start as usize];
    let after = &src[span.end as usize..line_end];
    if before.trim().is_empty() && after.trim().is_empty() {
        Fix::delete(Span::new(
            line_start as u32,
            (line_end + 1).min(src.len()) as u32,
        ))
    } else {
        Fix::delete(span)
    }
}

impl NoUnusedVars {
    fn js_suggestion(cx: &Ctx<'_, '_>, sem: &dyn Semantic, s: &Symbol) -> Option<Fix> {
        if s.refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(RefFlags::INIT)
        }) {
            return None;
        }
        let d = s.decls.first()?;
        match d.kind {
            DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed => {
                remove_import_specifier(cx, d.node, false)
            }
            DeclKind::Function | DeclKind::Class => remove_statement(cx, d.node),
            DeclKind::Var
            | DeclKind::Let
            | DeclKind::Const
            | DeclKind::Using
            | DeclKind::Param
            | DeclKind::CatchParam => remove_binding(cx, d.name),
            _ => None,
        }
    }
}

impl Rule for NoUnusedVars {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let pats = Pats {
            vars: self.vars_pat.as_deref().and_then(regex::compile),
            args: self.args_pat.as_deref().and_then(regex::compile),
            caught: self.caught_pat.as_deref().and_then(regex::compile),
            array: self.array_pat.as_deref().and_then(regex::compile),
        };
        let mut found = Vec::new();
        let mut ambient = FxHashMap::default();
        for i in 0..sem.symbols().len() {
            self.check(cx, sem, &pats, &mut ambient, SymbolId(i as u32), &mut found);
        }
        // Unused import bindings per declaration, for "remove the whole import".
        let ast = cx.ast();
        let mut unused_imports: FxHashMap<NodeId, usize> = FxHashMap::default();
        for f in &found {
            if f.id == "unusedVar"
                && let Some(d) = sem.symbol(f.sym).decls.first()
                && matches!(
                    d.kind,
                    DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed
                )
            {
                *unused_imports.entry(d.parent).or_default() += 1;
            }
        }
        found.sort_by_key(|f| (f.span.start, f.span.end));
        for f in found {
            let s = sem.symbol(f.sym);
            let name = sem.name(f.sym).to_owned();
            let fix = if f.id != "unusedVar" {
                None
            } else if !self.ts {
                Self::js_suggestion(cx, sem, s).map(|fix| {
                    (
                        "removeVar",
                        format!("Remove unused variable '{name}'."),
                        fix,
                    )
                })
            } else {
                s.decls.first().and_then(|d| match d.kind {
                    DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed => {
                        let Node::ImportDecl { specifiers, .. } = *ast.node(d.parent) else {
                            return None;
                        };
                        if unused_imports.get(&d.parent).copied()
                            == Some(ast.list(specifiers).len())
                        {
                            Some((
                                "removeUnusedImportDeclaration",
                                "Remove unused import declaration.".to_owned(),
                                remove_whole_line(cx, d.parent),
                            ))
                        } else {
                            remove_import_specifier(cx, d.node, true).map(|fix| {
                                (
                                    "removeUnusedVar",
                                    format!("Remove unused variable \"{name}\"."),
                                    fix,
                                )
                            })
                        }
                    }
                    DeclKind::ImportEquals => Some((
                        "removeUnusedImportDeclaration",
                        "Remove unused import declaration.".to_owned(),
                        remove_whole_line(cx, d.node),
                    )),
                    _ => None,
                })
            };
            let r = cx.report(f.span, f.message).id(f.id);
            match fix {
                Some((_, _, fix)) if self.ts && self.fix_imports => {
                    let _ = r.fix(fix);
                }
                Some((id, desc, fix)) => {
                    let _ = r.suggest(id, desc, fix);
                }
                None => {}
            }
        }
    }
}
