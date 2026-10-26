//! `unicorn/no-using-resource-escape`: a `return` or export whose value is, contains or
//! captures a `using` / `await using` binding of the same owner (the enclosing function, or
//! the program). The value is followed through TS wrappers, array elements, object
//! property values and methods, both branches of `?:` (a resource that is the test can't
//! reach the alternate: it is truthy), logical operands (never the left of `&&`) and the
//! last operand of a sequence. A function captures the resources its body reads at
//! runtime (not in `typeof` types, JSX namespaced names, or keys of type-only members);
//! a name stands for the function of an unreassigned function declaration (a single one)
//! or of a `const` initialized with a function. Exports: `export =`, `export default`,
//! exported `const`s and functions, and value specifiers of local `export {}`.

use super::prelude::*;
use super::unicorn_conditions::{is_function_node, unwrap_ts_and_parens};
use crate::semantic::SymbolId;
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-using-resource-escape",
        "Disallow returning or exporting resources declared with `using`, including through capturing functions.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUsingResourceEscape>,
};

#[derive(Debug, Default)]
struct NoUsingResourceEscape;

/// The enclosing function (or the program) of `id`.
fn owner(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let mut cur = id;
    for _ in 0..1_000_000 {
        let Some(p) = cx.parent(cur) else {
            return cur;
        };
        if is_function_node(cx.node(p)) || matches!(cx.node(p), Node::Program { .. }) {
            return p;
        }
        cur = p;
    }
    cur
}

/// `findValueVariable`: the innermost binding of the name with a value declaration.
fn value_variable(cx: &Ctx<'_, '_>, ident: NodeId) -> Option<SymbolId> {
    let sem = cx.semantic();
    let name = ident_name(cx.ast(), ident)?;
    let mut scope = Some(sem.enclosing_scope(ident));
    for _ in 0..100_000 {
        let s = sem.lookup(scope?, name)?;
        let sym = sem.symbol(s);
        if sym.decls.iter().any(|d| {
            !matches!(
                d.kind,
                DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam
            )
        }) {
            return Some(s);
        }
        scope = sem.scope(sym.scope).parent;
    }
    None
}

fn is_variable_decl(kind: DeclKind) -> bool {
    matches!(
        kind,
        DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
    )
}

fn is_owned_resource(cx: &Ctx<'_, '_>, sym: SymbolId, owner_node: NodeId) -> bool {
    let sem = cx.semantic();
    let mut defs = sem
        .symbol(sym)
        .decls
        .iter()
        .filter(|d| is_variable_decl(d.kind));
    match (defs.next(), defs.next()) {
        (Some(d), None) => d.kind == DeclKind::Using && owner(cx, d.node) == owner_node,
        _ => false,
    }
}

fn referenced_function(cx: &Ctx<'_, '_>, sym: SymbolId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut defs = sem
        .symbol(sym)
        .decls
        .iter()
        .filter(|d| is_variable_decl(d.kind) || d.kind == DeclKind::Function);
    let (Some(d), None) = (defs.next(), defs.next()) else {
        return None;
    };
    if d.kind == DeclKind::Function {
        return Some(d.node);
    }
    if d.kind != DeclKind::Const {
        return None;
    }
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(d.node)
    else {
        return None;
    };
    if !matches!(ast.node(name), Node::Ident { .. }) {
        return None;
    }
    let init = unwrap_ts_and_parens(ast, init);
    is_function_node(ast.node(init)).then_some(init)
}

/// A member that exists only in types, so its computed key is never evaluated.
fn is_type_only_member(node: &Node) -> bool {
    match node {
        Node::MethodSignature { .. } | Node::PropertySignature { .. } => true,
        Node::MethodDef {
            decorators,
            modifiers,
            ..
        } => decorators.is_empty() && modifiers.contains(Modifiers::ABSTRACT),
        Node::PropertyDef {
            decorators,
            modifiers,
            ..
        } => {
            decorators.is_empty()
                && (modifiers.contains(Modifiers::ABSTRACT)
                    || modifiers.contains(Modifiers::DECLARE))
        }
        _ => false,
    }
}

fn member_key(node: &Node) -> Option<NodeId> {
    match *node {
        Node::MethodSignature { key, .. }
        | Node::PropertySignature { key, .. }
        | Node::MethodDef { key, .. }
        | Node::PropertyDef { key, .. } => Some(key),
        _ => None,
    }
}

fn is_non_runtime_reference(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let mut child = ident;
    for _ in 0..1_000_000 {
        let Some(parent) = cx.parent(child) else {
            return false;
        };
        let node = cx.node(parent);
        if is_function_node(node) {
            return false;
        }
        if matches!(
            node,
            Node::TypeQuery { .. } | Node::JsxNamespacedName { .. }
        ) {
            return true;
        }
        if is_type_only_member(node) && member_key(node) == Some(child) {
            return true;
        }
        child = parent;
    }
    false
}

fn captured_resources(
    cx: &Ctx<'_, '_>,
    using: &[SymbolId],
    function: NodeId,
    owner_node: NodeId,
    out: &mut Vec<SymbolId>,
) {
    let ast = cx.ast();
    let sem = cx.semantic();
    if let Node::FunctionDecl {
        name: Some(name), ..
    } = *ast.node(function)
        && let Some(sym) = sem.symbol_of_decl(name)
        && sem
            .symbol(sym)
            .refs
            .iter()
            .any(|&r| sem.reference(r).is_write())
    {
        return;
    }
    let span = cx.span(function);
    for &sym in using {
        if out.contains(&sym) || !is_owned_resource(cx, sym, owner_node) {
            continue;
        }
        let captured = sem.symbol(sym).refs.iter().any(|&r| {
            let r = sem.reference(r);
            let s = cx.span(r.node);
            s.start >= span.start
                && s.end <= span.end
                && !r.is_type()
                && !is_non_runtime_reference(cx, r.node)
        });
        if captured {
            out.push(sym);
        }
    }
}

fn escaping(
    cx: &Ctx<'_, '_>,
    using: &[SymbolId],
    id: NodeId,
    owner_node: NodeId,
    out: &mut Vec<SymbolId>,
    depth: u32,
) {
    if depth > 200 {
        return;
    }
    let ast = cx.ast();
    let mut id = unwrap_ts_and_parens(ast, id);
    if matches!(ast.node(id), Node::Ident { .. }) {
        let Some(sym) = value_variable(cx, id) else {
            return;
        };
        if is_owned_resource(cx, sym, owner_node) {
            if !out.contains(&sym) {
                out.push(sym);
            }
            return;
        }
        let Some(f) = referenced_function(cx, sym) else {
            return;
        };
        id = f;
    }
    if is_function_node(ast.node(id)) {
        captured_resources(cx, using, id, owner_node, out);
        return;
    }
    match *ast.node(id) {
        Node::ArrayLit { elements } => {
            for &e in ast.list(elements) {
                if !matches!(ast.node(e), Node::Hole {} | Node::Spread { .. }) {
                    escaping(cx, using, e, owner_node, out, depth + 1);
                }
            }
        }
        Node::ObjectLit { props } => {
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::Property { value, .. } | Node::MethodDef { value, .. } => {
                        escaping(cx, using, value, owner_node, out, depth + 1)
                    }
                    Node::ShorthandProp { name, .. } => {
                        escaping(cx, using, name, owner_node, out, depth + 1)
                    }
                    _ => {}
                }
            }
        }
        Node::Conditional { test, cons, alt } => {
            escaping(cx, using, cons, owner_node, out, depth + 1);
            let test = unwrap_ts_and_parens(ast, test);
            let test_var = matches!(ast.node(test), Node::Ident { .. })
                .then(|| value_variable(cx, test))
                .flatten();
            let mut alternate = Vec::new();
            escaping(cx, using, alt, owner_node, &mut alternate, depth + 1);
            for s in alternate {
                if Some(s) != test_var && !out.contains(&s) {
                    out.push(s);
                }
            }
        }
        Node::Binary {
            left,
            op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce),
            right,
        } => {
            if op != BinaryOp::LogicalAnd {
                escaping(cx, using, left, owner_node, out, depth + 1);
            }
            escaping(cx, using, right, owner_node, out, depth + 1);
        }
        Node::Seq { exprs } => {
            if let Some(&last) = ast.list(exprs).last() {
                escaping(cx, using, last, owner_node, out, depth + 1);
            }
        }
        _ => {}
    }
}

fn exported_values(ast: &Ast, id: NodeId) -> Vec<NodeId> {
    match *ast.node(id) {
        Node::ExportAssignment { expr } | Node::ExportDefaultExpr { expr } => vec![expr],
        Node::ExportNamed {
            type_only: false,
            specifiers,
            source: None,
            ..
        } => ast
            .list(specifiers)
            .iter()
            .filter_map(|&s| match *ast.node(s) {
                Node::ExportSpecifier {
                    type_only: false,
                    local,
                    ..
                } => Some(local),
                _ => None,
            })
            .collect(),
        Node::VarDecl {
            modifiers,
            kind: VarKind::Const,
            decls,
        } if modifiers.contains(Modifiers::EXPORT) && !modifiers.contains(Modifiers::DECLARE) => {
            ast.list(decls)
                .iter()
                .filter_map(|&d| match *ast.node(d) {
                    Node::VarDeclarator { name, init, .. }
                        if matches!(ast.node(name), Node::Ident { .. }) =>
                    {
                        init
                    }
                    _ => None,
                })
                .collect()
        }
        Node::FunctionDecl { modifiers, .. }
            if modifiers.contains(Modifiers::EXPORT) || modifiers.contains(Modifiers::DEFAULT) =>
        {
            vec![id]
        }
        _ => Vec::new(),
    }
}

fn check(cx: &mut Ctx<'_, '_>, using: &[SymbolId], node: NodeId, values: &[NodeId], action: &str) {
    let owner_node = owner(cx, node);
    let mut resources = Vec::new();
    for &v in values {
        escaping(cx, using, v, owner_node, &mut resources, 0);
    }
    let span = cx.span(node);
    for sym in resources {
        let name = cx.semantic().name(sym).to_owned();
        cx.report(
            span,
            format!(
                "Do not {action} resource `{name}` or a value that contains or captures it. The resource is disposed when its owning scope exits."
            ),
        )
        .id("no-using-resource-escape");
    }
}

struct Collect {
    nodes: Vec<NodeId>,
}

impl Visitor for Collect {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
        if matches!(
            ast.node(id),
            Node::Return { .. }
                | Node::ExportAssignment { .. }
                | Node::ExportDefaultExpr { .. }
                | Node::ExportNamed { .. }
                | Node::VarDecl { .. }
                | Node::FunctionDecl { .. }
        ) {
            self.nodes.push(id);
        }
        Walk::Continue
    }
}

impl Rule for NoUsingResourceEscape {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        // Only files with a `using` binding can have an escape; most have none.
        let using: Vec<SymbolId> = cx
            .semantic()
            .symbols()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.decls.iter().any(|d| d.kind == DeclKind::Using))
            .map(|(i, _)| SymbolId(i as u32))
            .collect();
        if using.is_empty() {
            return;
        }
        let ast = cx.ast();
        let mut c = Collect { nodes: Vec::new() };
        walk(ast, ast.root, &mut c);
        for node in c.nodes {
            if let Node::Return { arg } = *ast.node(node) {
                if let Some(arg) = arg {
                    check(cx, &using, node, &[arg], "return");
                }
                continue;
            }
            let values = exported_values(ast, node);
            if !values.is_empty() {
                check(cx, &using, node, &values, "export");
            }
        }
    }
}
