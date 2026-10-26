//! `unicorn/prefer-path2d`: a reusable `Path2D` over rebuilding the same canvas path in
//! every repeated draw.
//!
//! One walk over the file collects, per function (and the top level), the calls on a 2D
//! canvas context: builders (`moveTo`, `lineTo`, `arc`, `rect`, `closePath`, ...),
//! current-path consumers (`stroke()`, `fill()`/`clip()` with no argument or a fill rule,
//! `isPointInPath(x, y[, rule])`, `isPointInStroke(x, y)`) and `beginPath()`, which closes
//! the receiver's open group. The receiver is a context when its local annotation says so
//! (`CanvasRenderingContext2D`/`OffscreenCanvasRenderingContext2D` through local aliases,
//! interfaces' `extends`, type parameter constraints, unions of contexts and nullish types,
//! intersections with one), when it is a `getContext("2d")` call, a binding initialized with
//! one, or an unknown binding named `context`/`ctx`. Calls are grouped by receiver (same
//! binding and same reference) and by innermost loop.
//!
//! A function is repeated when it is the first argument of the global
//! `requestAnimationFrame`/`setInterval` (also on `window`/`self`/`globalThis`), is a
//! function declaration or `const` arrow/function that such a call receives by name, that
//! is called inside a loop, or that a repeated function calls (transitively). In a repeated
//! function every call counts; elsewhere only calls inside a loop. A group with two
//! builders and a consumer is reported on its first builder's method name.

use super::prelude::*;
use super::unicorn_text::is_same_reference;
use crate::semantic::{DeclKind, SymbolId};
use ts_r_ast::{Ast, KeywordTypeKind, TypeOperatorKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-path2d",
        "Prefer `Path2D` for repeatedly drawn canvas paths.",
        NO_PRESET,
        Category::Style,
    ),
    create: create_default::<PreferPath2d>,
};

#[derive(Debug, Default)]
struct PreferPath2d;

const BUILDERS: &[&str] = &[
    "moveTo",
    "lineTo",
    "bezierCurveTo",
    "quadraticCurveTo",
    "arc",
    "arcTo",
    "ellipse",
    "rect",
    "roundRect",
    "closePath",
];
const CONTEXT_TYPES: &[&str] = &[
    "CanvasRenderingContext2D",
    "OffscreenCanvasRenderingContext2D",
];
const MAX_DEPTH: u32 = 64;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Canvas,
    NonCanvas,
    Nullish,
    Unknown,
}

fn union(kinds: &[Kind]) -> Kind {
    let real: Vec<Kind> = kinds
        .iter()
        .copied()
        .filter(|&k| k != Kind::Nullish)
        .collect();
    if !real.is_empty() && real.iter().all(|&k| k == Kind::Canvas) {
        return Kind::Canvas;
    }
    if !kinds.contains(&Kind::Unknown) && kinds.contains(&Kind::NonCanvas) {
        return Kind::NonCanvas;
    }
    Kind::Unknown
}

fn intersection(kinds: &[Kind]) -> Kind {
    if kinds.contains(&Kind::Canvas) {
        Kind::Canvas
    } else if kinds.iter().all(|&k| k == Kind::NonCanvas) {
        Kind::NonCanvas
    } else {
        Kind::Unknown
    }
}

struct Types<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    types: Vec<SymbolId>,
    vars: Vec<SymbolId>,
}

impl Types<'_, '_, '_> {
    /// The kind a type reference named by `name` (an `Ident`, or anything else) means.
    fn reference(&mut self, name: NodeId, scope: crate::semantic::ScopeId, depth: u32) -> Kind {
        let ast = self.cx.ast();
        if matches!(ast.node(name), Node::QualifiedName { .. }) {
            return Kind::NonCanvas;
        }
        let Some(text) =
            ident_name(ast, name).filter(|_| matches!(ast.node(name), Node::Ident { .. }))
        else {
            return Kind::NonCanvas;
        };
        let sem = self.cx.semantic();
        let Some(sym) = sem.lookup(scope, text) else {
            return if CONTEXT_TYPES.contains(&text) {
                Kind::Canvas
            } else {
                Kind::NonCanvas
            };
        };
        let symbol = sem.symbol(sym);
        let Some(decl) = symbol.decls.first() else {
            return if CONTEXT_TYPES.contains(&text) {
                Kind::Canvas
            } else {
                Kind::NonCanvas
            };
        };
        if self.types.contains(&sym) || depth > MAX_DEPTH {
            return Kind::Unknown;
        }
        self.types.push(sym);
        let def_scope = symbol.scope;
        let kind = match (decl.kind, *ast.node(decl.node)) {
            (DeclKind::TypeAlias, Node::TypeAlias { ty, .. }) => {
                self.annotation(ty, def_scope, depth + 1)
            }
            (DeclKind::Interface, Node::Interface { extends, .. }) => {
                let mut kind = Kind::NonCanvas;
                for &h in ast.list(extends) {
                    let expr = match *ast.node(h) {
                        Node::ExprWithTypeArgs { expr, .. } => expr,
                        _ => h,
                    };
                    if self.reference(expr, def_scope, depth + 1) == Kind::Canvas {
                        kind = Kind::Canvas;
                    }
                }
                kind
            }
            (
                DeclKind::TypeParam,
                Node::TypeParam {
                    constraint: Some(c),
                    ..
                },
            ) => self.annotation(c, def_scope, depth + 1),
            _ => Kind::NonCanvas,
        };
        self.types.retain(|&s| s != sym);
        kind
    }

    fn annotation(&mut self, ty: NodeId, scope: crate::semantic::ScopeId, depth: u32) -> Kind {
        let ast = self.cx.ast();
        if depth > MAX_DEPTH {
            return Kind::Unknown;
        }
        match *ast.node(ty) {
            Node::ParenType { ty } => self.annotation(ty, scope, depth + 1),
            Node::TypeOperator { op, ty } => {
                if op == TypeOperatorKind::Readonly {
                    self.annotation(ty, scope, depth + 1)
                } else {
                    Kind::Unknown
                }
            }
            Node::TypeRef { name, .. } => self.reference(name, scope, depth + 1),
            Node::UnionType { types } => {
                let kinds: Vec<Kind> = ast
                    .list(types)
                    .iter()
                    .map(|&t| self.annotation(t, scope, depth + 1))
                    .collect();
                union(&kinds)
            }
            Node::IntersectionType { types } => {
                let kinds: Vec<Kind> = ast
                    .list(types)
                    .iter()
                    .map(|&t| self.annotation(t, scope, depth + 1))
                    .collect();
                intersection(&kinds)
            }
            Node::KeywordType { kind } => match kind {
                KeywordTypeKind::Null | KeywordTypeKind::Undefined => Kind::Nullish,
                KeywordTypeKind::BigInt
                | KeywordTypeKind::Boolean
                | KeywordTypeKind::Never
                | KeywordTypeKind::Number
                | KeywordTypeKind::String
                | KeywordTypeKind::Symbol
                | KeywordTypeKind::Void
                | KeywordTypeKind::Any
                | KeywordTypeKind::Unknown => Kind::NonCanvas,
                _ => Kind::Unknown,
            },
            Node::StringLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::TypeLiteral { .. }
            | Node::FunctionType { .. }
            | Node::ConstructorType { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. } => Kind::NonCanvas,
            _ => Kind::Unknown,
        }
    }

    fn variable(&mut self, ident: NodeId, depth: u32) -> Kind {
        let cx = self.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(sym) = sem.symbol_of_ident(ident) else {
            return Kind::Unknown;
        };
        let symbol = sem.symbol(sym);
        if self.vars.contains(&sym) || symbol.decls.len() != 1 || depth > MAX_DEPTH {
            return Kind::Unknown;
        }
        let decl = symbol.decls[0];
        let (type_ann, init) = match *ast.node(decl.node) {
            Node::Param {
                pattern, type_ann, ..
            } if pattern == decl.name => (type_ann, None),
            Node::VarDeclarator {
                name,
                type_ann,
                init,
                ..
            } if name == decl.name => (type_ann, init),
            _ => (None, None),
        };
        self.vars.push(sym);
        let variable_decl = matches!(
            decl.kind,
            DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
        );
        let kind = if let Some(t) = type_ann {
            match self.annotation(t, sem.enclosing_scope(decl.name), depth + 1) {
                Kind::Unknown => Kind::NonCanvas,
                k => k,
            }
        } else if variable_decl
            && decl.kind != DeclKind::Const
            && symbol.refs.iter().any(|&r| {
                let r = sem.reference(r);
                r.is_write() && r.node != decl.name
            })
        {
            Kind::NonCanvas
        } else if variable_decl && let Some(init) = init {
            self.context(init, depth + 1)
        } else {
            Kind::Unknown
        };
        self.vars.retain(|&s| s != sym);
        kind
    }

    fn context(&mut self, id: NodeId, depth: u32) -> Kind {
        let cx = self.cx;
        let ast = cx.ast();
        if depth > MAX_DEPTH {
            return Kind::Unknown;
        }
        let syntax = match *ast.node(id) {
            Node::Ident { .. } => match self.variable(id, depth + 1) {
                Kind::Unknown => {
                    if matches!(ident_name(ast, id), Some("context" | "ctx")) {
                        Kind::Canvas
                    } else {
                        Kind::Unknown
                    }
                }
                k => k,
            },
            Node::Satisfies { expr, ty }
            | Node::As { expr, ty }
            | Node::TypeAssertion { expr, ty } => {
                match self.annotation(ty, cx.semantic().enclosing_scope(id), depth + 1) {
                    Kind::Unknown => self.context(expr, depth + 1),
                    k => k,
                }
            }
            Node::NonNull { expr } | Node::Paren { expr } => self.context(expr, depth + 1),
            Node::ObjectLit { .. }
            | Node::ArrayLit { .. }
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::New { .. } => Kind::NonCanvas,
            _ => Kind::Unknown,
        };
        if syntax != Kind::Unknown {
            return syntax;
        }
        match get_context_arg(ast, id) {
            Some(Some(true)) => Kind::Canvas,
            Some(Some(false)) => Kind::NonCanvas,
            _ => Kind::Unknown,
        }
    }
}

fn static_string(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(skip_parens(ast, id)) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[quasi] => match *ast.node(quasi) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// For `<x>.getContext(arg, ...)`: `Some(Some(is "2d"))`, `Some(None)` without arguments.
fn get_context_arg(ast: &Ast, id: NodeId) -> Option<Option<bool>> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let Node::Member {
        optional: false,
        prop,
        ..
    } = *ast.node(callee)
    else {
        return None;
    };
    if ident_name(ast, prop) != Some("getContext") {
        return None;
    }
    let first = ast.list(args).first().copied();
    Some(first.map(|a| static_string(ast, a).as_deref() == Some("2d")))
}

#[derive(Clone, Copy)]
struct PathCall {
    prop: NodeId,
    receiver: NodeId,
    receiver_var: Option<SymbolId>,
    builder: bool,
    consumer: bool,
    boundary: bool,
    region: Option<u32>,
}

#[derive(Default)]
struct Info {
    calls: Vec<(Option<SymbolId>, bool)>,
    path_calls: Vec<PathCall>,
    repeated: bool,
    variables: Vec<SymbolId>,
}

fn is_function(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
    )
}

fn is_loop(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(id),
        Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. }
    )
}

fn has_write_after_init(cx: &Ctx<'_, '_>, sym: SymbolId) -> bool {
    let sem = cx.semantic();
    let symbol = sem.symbol(sym);
    symbol.refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write() && !symbol.decls.iter().any(|d| d.name == r.node)
    })
}

fn is_global(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    cx.semantic().symbol_of_ident(ident).is_none()
}

fn is_scheduler_call(cx: &Ctx<'_, '_>, call: NodeId) -> bool {
    let ast = cx.ast();
    let Node::Call { callee, .. } = *ast.node(call) else {
        return false;
    };
    let methods = |n: Option<&str>| matches!(n, Some("requestAnimationFrame" | "setInterval"));
    match *ast.node(callee) {
        Node::Ident { .. } => methods(ident_name(ast, callee)) && is_global(cx, callee),
        Node::Member {
            obj,
            optional: false,
            prop,
        } => {
            matches!(ast.node(obj), Node::Ident { .. })
                && matches!(ident_name(ast, obj), Some("globalThis" | "self" | "window"))
                && methods(ident_name(ast, prop))
                && is_global(cx, obj)
        }
        _ => false,
    }
}

fn function_variables(cx: &Ctx<'_, '_>, function: NodeId) -> Vec<SymbolId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut out = Vec::new();
    if let Node::FunctionDecl {
        name: Some(name), ..
    } = *ast.node(function)
        && let Some(sym) = sem.symbol_of_decl(name)
        && !has_write_after_init(cx, sym)
    {
        out.push(sym);
    }
    if let Some(p) = cx.parent(function)
        && let Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } = *ast.node(p)
        && init == function
        && matches!(ast.node(name), Node::Ident { .. })
        && cx.parent(p).is_some_and(|d| {
            matches!(
                ast.node(d),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            )
        })
        && let Some(sym) = sem.symbol_of_decl(name)
    {
        out.push(sym);
    }
    out
}

fn path_call(cx: &Ctx<'_, '_>, call: NodeId, region: Option<u32>) -> Option<PathCall> {
    let ast = cx.ast();
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(call)
    else {
        return None;
    };
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(callee)
    else {
        return None;
    };
    let method = ident_name(ast, prop)?;
    let args = ast.list(args);
    let fill_rule = |a: NodeId| {
        matches!(
            static_string(ast, a).as_deref(),
            Some("nonzero" | "evenodd")
        )
    };
    let builder = BUILDERS.contains(&method);
    let consumer = match method {
        "stroke" => args.is_empty(),
        "fill" | "clip" => args.is_empty() || (args.len() == 1 && fill_rule(args[0])),
        "isPointInStroke" => args.len() == 2,
        "isPointInPath" => args.len() == 2 || (args.len() == 3 && fill_rule(args[2])),
        _ => false,
    };
    let boundary = method == "beginPath";
    if !builder && !consumer && !boundary {
        return None;
    }
    let mut types = Types {
        cx,
        types: Vec::new(),
        vars: Vec::new(),
    };
    if types.context(obj, 0) != Kind::Canvas {
        return None;
    }
    let mut root = obj;
    for _ in 0..10_000 {
        match *ast.node(root) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => root = obj,
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr }
            | Node::Paren { expr } => root = expr,
            _ => break,
        }
    }
    let receiver_var = matches!(ast.node(root), Node::Ident { .. })
        .then(|| cx.semantic().symbol_of_ident(root))
        .flatten();
    Some(PathCall {
        prop,
        receiver: obj,
        receiver_var,
        builder,
        consumer,
        boundary,
        region,
    })
}

struct Group {
    first: PathCall,
    closed: bool,
    builders: Vec<PathCall>,
    consumers: Vec<PathCall>,
}

fn same_receiver(cx: &Ctx<'_, '_>, group: &PathCall, call: &PathCall) -> bool {
    if (group.receiver_var.is_some() || call.receiver_var.is_some())
        && group.receiver_var != call.receiver_var
    {
        return false;
    }
    group.region == call.region && is_same_reference(cx, group.receiver, call.receiver)
}

fn groups(cx: &Ctx<'_, '_>, calls: &[PathCall]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for call in calls {
        let found = groups
            .iter()
            .rposition(|g| !g.closed && same_receiver(cx, &g.first, call));
        if call.boundary {
            if let Some(i) = found {
                groups[i].closed = true;
            }
            continue;
        }
        let i = match found {
            Some(i) => i,
            None => {
                groups.push(Group {
                    first: *call,
                    closed: false,
                    builders: Vec::new(),
                    consumers: Vec::new(),
                });
                groups.len() - 1
            }
        };
        if call.builder {
            groups[i].builders.push(*call);
        }
        if call.consumer {
            groups[i].consumers.push(*call);
        }
    }
    groups
}

enum Step {
    Enter(NodeId),
    Exit(NodeId),
}

impl Rule for PreferPath2d {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        // infos[0] is the top level.
        let mut infos: Vec<Info> = vec![Info::default()];
        let mut info_stack: Vec<usize> = vec![0];
        let mut loops: Vec<u32> = Vec::new();
        let mut saved_loops: Vec<Vec<u32>> = Vec::new();
        let mut next_region = 0u32;
        let mut stack = vec![Step::Enter(ast.root)];
        let mut children = Vec::new();
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter(id) => {
                    if is_function(ast, id) {
                        let repeated = cx.parent(id).is_some_and(|p| {
                            matches!(*ast.node(p), Node::Call { args, .. } if ast.list(args).first() == Some(&id))
                                && is_scheduler_call(cx, p)
                        });
                        infos.push(Info {
                            variables: function_variables(cx, id),
                            repeated,
                            ..Info::default()
                        });
                        info_stack.push(infos.len() - 1);
                        saved_loops.push(std::mem::take(&mut loops));
                    } else if is_loop(ast, id) {
                        loops.push(next_region);
                        next_region += 1;
                    } else if let Node::Call { callee, args, .. } = *ast.node(id) {
                        let current = *info_stack.last().unwrap_or(&0);
                        let scheduler = is_scheduler_call(cx, id);
                        let sem = cx.semantic();
                        if matches!(ast.node(callee), Node::Ident { .. })
                            && let Some(var) = sem.symbol_of_ident(callee)
                        {
                            infos[current]
                                .calls
                                .push((Some(var), !loops.is_empty() || scheduler));
                        }
                        if scheduler
                            && let Some(&cb) = ast.list(args).first()
                            && matches!(ast.node(cb), Node::Ident { .. })
                        {
                            infos[current].calls.push((sem.symbol_of_ident(cb), true));
                        }
                        if let Some(pc) = path_call(cx, id, loops.last().copied()) {
                            infos[current].path_calls.push(pc);
                        }
                    }
                    stack.push(Step::Exit(id));
                    children.clear();
                    ast.for_each_child(id, |c| children.push(c));
                    for &c in children.iter().rev() {
                        stack.push(Step::Enter(c));
                    }
                }
                Step::Exit(id) => {
                    if is_function(ast, id) {
                        info_stack.pop();
                        loops = saved_loops.pop().unwrap_or_default();
                    } else if is_loop(ast, id) {
                        loops.pop();
                    }
                }
            }
        }
        // Which infos are repeated.
        let info_of = |var: Option<SymbolId>| -> Option<usize> {
            let var = var?;
            (1..infos.len())
                .rev()
                .find(|&i| infos[i].variables.contains(&var))
        };
        let mut repeated = vec![false; infos.len()];
        let mut queue = Vec::new();
        let mark = |i: Option<usize>, repeated: &mut Vec<bool>, queue: &mut Vec<usize>| {
            if let Some(i) = i
                && !repeated[i]
            {
                repeated[i] = true;
                queue.push(i);
            }
        };
        for (i, info) in infos.iter().enumerate().skip(1) {
            if info.repeated {
                mark(Some(i), &mut repeated, &mut queue);
            }
        }
        for info in &infos {
            for &(var, rep) in &info.calls {
                if rep {
                    mark(info_of(var), &mut repeated, &mut queue);
                }
            }
        }
        let mut q = 0;
        while q < queue.len() {
            let i = queue[q];
            q += 1;
            for &(var, _) in &infos[i].calls {
                mark(info_of(var), &mut repeated, &mut queue);
            }
        }
        let mut reports = Vec::new();
        for (i, info) in infos.iter().enumerate() {
            for group in groups(cx, &info.path_calls) {
                let relevant = |calls: &[PathCall]| -> Vec<PathCall> {
                    calls
                        .iter()
                        .copied()
                        .filter(|c| repeated[i] || c.region.is_some())
                        .collect()
                };
                let builders = relevant(&group.builders);
                let consumers = relevant(&group.consumers);
                if builders.len() >= 2 && !consumers.is_empty() {
                    reports.push(cx.span(builders[0].prop));
                }
            }
        }
        reports.sort_by_key(|s| (s.start, s.end));
        for span in reports {
            cx.report(
                span,
                "Prefer reusing a `Path2D` object instead of rebuilding the canvas path in repeated draws.",
            )
            .id("prefer-path2d");
        }
    }
}
