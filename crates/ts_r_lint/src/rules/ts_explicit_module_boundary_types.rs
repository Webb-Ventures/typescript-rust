//! `@typescript-eslint/explicit-module-boundary-types`: exported functions, and the public
//! methods of exported classes and objects, declare their return and parameter types.

use rustc_hash::FxHashSet;
use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;
use super::ts_explicit_function_return_type::{
    ReturnOpts, ancestor_has_return_type, class_method, es_parent, immediately_returns_function,
    typed_function_expression, valid_function_expression_return_type, valid_function_return_type,
};
use super::unicorn_static::static_value;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/explicit-module-boundary-types",
        "Require explicit return and argument types on exported functions' and classes' public class methods",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<ExplicitModuleBoundaryTypes>,
};

#[derive(Debug)]
struct ExplicitModuleBoundaryTypes {
    opts: ReturnOpts,
    allow_any: bool,
    allowed_names: Vec<String>,
    allow_overloads: bool,
}

impl FromOptions for ExplicitModuleBoundaryTypes {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(ExplicitModuleBoundaryTypes {
            opts: ReturnOpts {
                allow_direct_const: bool_field(
                    o,
                    "allowDirectConstAssertionInArrowFunctions",
                    true,
                ),
                allow_expressions: false,
                allow_higher_order: bool_field(o, "allowHigherOrderFunctions", true),
                allow_typed: bool_field(o, "allowTypedFunctionExpressions", true),
            },
            allow_any: bool_field(o, "allowArgumentsExplicitlyTypedAsAny", false),
            allowed_names: str_list(o, "allowedNames"),
            allow_overloads: bool_field(o, "allowOverloadFunctions", false),
        })
    }
}

/// Every function with a body, in source order, and the export nodes.
#[derive(Default)]
struct Collect {
    functions: Vec<NodeId>,
    exports: Vec<NodeId>,
}

impl Visitor for Collect {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match *ast.node(id) {
            Node::FunctionDecl {
                body: Some(_),
                modifiers,
                ..
            } => {
                self.functions.push(id);
                if modifiers.contains(Modifiers::EXPORT) || modifiers.contains(Modifiers::DEFAULT) {
                    self.exports.push(id);
                }
            }
            Node::FunctionExpr { body: Some(_), .. } | Node::Arrow { .. } => {
                self.functions.push(id)
            }
            Node::ClassDecl { modifiers, .. } | Node::VarDecl { modifiers, .. }
                if modifiers.contains(Modifiers::EXPORT)
                    || modifiers.contains(Modifiers::DEFAULT) =>
            {
                self.exports.push(id)
            }
            Node::ExportNamed { source: None, .. }
            | Node::ExportDefaultExpr { .. }
            | Node::ExportAssignment { .. } => self.exports.push(id),
            _ => {}
        }
        Walk::Continue
    }
}

struct Run<'r> {
    rule: &'r ExplicitModuleBoundaryTypes,
    visited: FxHashSet<NodeId>,
    checked: FxHashSet<NodeId>,
    work: Vec<NodeId>,
}

/// The name a class or object member's key gives statically.
fn key_value(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::Ident { .. } => ident_name(ast, key).map(str::to_owned),
        Node::PrivateName { name } => Some(ast.atom(name).to_owned()),
        Node::ComputedKey { expr } => static_value(cx, expr)?.to_js_string(),
        _ => static_value(cx, key)?.to_js_string(),
    }
}

fn member_key(cx: &Ctx<'_, '_>, m: NodeId) -> Option<NodeId> {
    match *cx.node(m) {
        Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => Some(key),
        _ => None,
    }
}

fn body_of(cx: &Ctx<'_, '_>, f: NodeId) -> Option<NodeId> {
    match *cx.node(f) {
        Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body,
        Node::Arrow { body, .. } => Some(body),
        _ => None,
    }
}

impl Run<'_> {
    fn allowed_name(&self, cx: &Ctx<'_, '_>, n: Option<NodeId>) -> bool {
        let Some(n) = n else { return false };
        let names = &self.rule.allowed_names;
        if names.is_empty() {
            return false;
        }
        let ast = cx.ast();
        let name = match *cx.node(n) {
            Node::VarDeclarator { name, .. } => ident_name(ast, name).map(str::to_owned),
            Node::FunctionDecl { name, .. } => {
                name.and_then(|n| ident_name(ast, n)).map(str::to_owned)
            }
            Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => key_value(cx, key),
            _ => None,
        };
        name.is_some_and(|n| names.contains(&n))
    }

    /// `node` (a `FunctionDecl` or a class `MethodDef`) has bodyless overload signatures.
    fn has_overloads(&self, cx: &Ctx<'_, '_>, node: NodeId) -> bool {
        let ast = cx.ast();
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        if let Node::FunctionDecl {
            modifiers, name, ..
        } = *cx.node(node)
        {
            let Some(list) = statement_list(ast, parent) else {
                return false;
            };
            let name = name.and_then(|n| ident_name(ast, n));
            let default = modifiers.contains(Modifiers::DEFAULT);
            let export = modifiers.contains(Modifiers::EXPORT);
            return list.iter().any(|&s| match *ast.node(s) {
                Node::FunctionDecl {
                    body: None,
                    modifiers: m,
                    name: n,
                    ..
                } => {
                    if default {
                        m.contains(Modifiers::DEFAULT)
                    } else {
                        (!export || m.contains(Modifiers::EXPORT))
                            && n.and_then(|n| ident_name(ast, n)) == name
                    }
                }
                _ => false,
            });
        }
        let members = match *cx.node(parent) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
            _ => return false,
        };
        let key = member_key(cx, node).and_then(|k| key_value(cx, k));
        members.iter().any(|&m| {
            matches!(*cx.node(m), Node::MethodDef { value, .. } if body_of(cx, value).is_none())
                && member_key(cx, m).and_then(|k| key_value(cx, k)) == key
        })
    }

    fn check_params(&self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        let ast = cx.ast();
        let src = cx.src();
        let params = match *cx.node(f) {
            Node::FunctionDecl { params, .. }
            | Node::FunctionExpr { params, .. }
            | Node::Arrow { params, .. } => ast.list(params),
            _ => return,
        };
        for &p in params {
            let Node::Param {
                rest,
                pattern,
                type_ann,
                init,
                ..
            } = *cx.node(p)
            else {
                continue;
            };
            if init.is_some() {
                continue;
            }
            let pspan = cx.span(pattern);
            let mut start = pspan.start;
            if rest
                && let Some(i) = src
                    .get(cx.span(p).start as usize..pspan.start as usize)
                    .and_then(|s| s.rfind("..."))
            {
                start = cx.span(p).start + i as u32;
            }
            let end = type_ann.map_or(pspan.end, |t| cx.span(t).end.max(pspan.end));
            let (named, label) = match *cx.node(pattern) {
                Node::Ident { .. } => (ident_name(ast, pattern), None),
                Node::ObjectPattern { .. } if !rest => (None, Some("Object pattern")),
                Node::ArrayPattern { .. } if !rest => (None, Some("Array pattern")),
                _ if rest => (None, Some("Rest")),
                _ => continue,
            };
            let any = type_ann.is_some_and(|t| {
                matches!(
                    cx.node(t),
                    Node::KeywordType {
                        kind: KeywordTypeKind::Any
                    }
                )
            });
            let (msg, id) = match (type_ann.is_none(), named, label) {
                (true, Some(n), _) => {
                    (format!("Argument '{n}' should be typed."), "missingArgType")
                }
                (true, None, Some(l)) => (
                    format!("{l} argument should be typed."),
                    "missingArgTypeUnnamed",
                ),
                (false, Some(n), _) if any && !self.rule.allow_any => (
                    format!("Argument '{n}' should be typed with a non-any type."),
                    "anyTypedArg",
                ),
                (false, None, Some(l)) if any && !self.rule.allow_any => (
                    format!("{l} argument should be typed with a non-any type."),
                    "anyTypedArgUnnamed",
                ),
                _ => continue,
            };
            cx.report(Span::new(start, end), msg).id(id);
        }
    }

    fn report_return(&self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        let head = member_function_head_span(cx.ast(), cx.src(), cx.file.parents, f);
        cx.report(head, "Missing return type on function.")
            .id("missingReturnType");
    }

    fn check_expression(&mut self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        if !self.checked.insert(f) {
            return;
        }
        let o = &self.rule.opts;
        let parent = es_parent(cx, f);
        let method_parent = cx
            .parent(f)
            .filter(|&p| matches!(*cx.node(p), Node::MethodDef { value, .. } if value == f));
        // An object literal's non-method property doesn't count for `allowedNames`.
        let named_parent = match parent.map(|p| cx.node(p)) {
            Some(Node::Property { .. }) => None,
            _ => method_parent.or(parent),
        };
        if self.allowed_name(cx, named_parent)
            || typed_function_expression(cx, f, o)
            || ancestor_has_return_type(cx, f)
        {
            return;
        }
        if self.rule.allow_overloads
            && let Some(m) = class_method(cx, f)
            && self.has_overloads(cx, m)
        {
            return;
        }
        if !valid_function_expression_return_type(cx, f, o) && !valid_function_return_type(cx, f, o)
        {
            self.report_return(cx, f);
        }
        self.check_params(cx, f);
    }

    fn check_declaration(&mut self, cx: &mut Ctx<'_, '_>, f: NodeId) {
        if !self.checked.insert(f) {
            return;
        }
        let o = &self.rule.opts;
        if self.allowed_name(cx, Some(f)) || ancestor_has_return_type(cx, f) {
            return;
        }
        if self.rule.allow_overloads && self.has_overloads(cx, f) {
            return;
        }
        if !valid_function_return_type(cx, f, o) {
            self.report_return(cx, f);
        }
        self.check_params(cx, f);
    }

    /// A bodyless class method (an overload or abstract method).
    fn check_empty_body(&mut self, cx: &mut Ctx<'_, '_>, f: NodeId, kind: MethodKind) {
        let return_type = match *cx.node(f) {
            Node::FunctionExpr { return_type, .. } => return_type,
            _ => None,
        };
        if !matches!(kind, MethodKind::Constructor | MethodKind::Set) && return_type.is_none() {
            let mut span = cx.span(f);
            // ESTree's node starts at the parameters (or type parameters).
            if let Node::FunctionExpr {
                type_params,
                params,
                ..
            } = *cx.node(f)
            {
                let ast = cx.ast();
                let from = type_params
                    .map(|t| cx.span(t).start)
                    .or_else(|| ast.list(params).first().map(|&p| cx.span(p).start))
                    .unwrap_or(span.start);
                if let Some(i) = cx
                    .src()
                    .get(span.start as usize..from as usize)
                    .and_then(|s| {
                        if type_params.is_some() {
                            s.rfind('<')
                        } else {
                            s.rfind('(')
                        }
                    })
                {
                    span = Span::new(span.start + i as u32, span.end);
                }
            }
            cx.report(span, "Missing return type on function.")
                .id("missingReturnType");
        }
        self.check_params(cx, f);
    }

    fn follow(&mut self, cx: &Ctx<'_, '_>, ident: NodeId) {
        let sem = cx.semantic();
        let Some(r) = sem.reference_at(ident) else {
            return;
        };
        let reference = sem.reference(r);
        let Some(sym) = reference.symbol() else {
            return;
        };
        let symbol = sem.symbol(sym);
        if symbol.scope != reference.scope {
            return;
        }
        for d in &symbol.decls {
            match d.kind {
                DeclKind::CatchParam
                | DeclKind::Param
                | DeclKind::ImportDefault
                | DeclKind::ImportNamespace
                | DeclKind::ImportNamed
                | DeclKind::ImportEquals
                | DeclKind::CommentGlobal => {}
                _ => self.work.push(d.node),
            }
        }
        for &r in &symbol.refs {
            let r = sem.reference(r);
            if r.is_write()
                && !r.flags.contains(RefFlags::INIT)
                && let Some(v) = r.write_value
            {
                self.work.push(v);
            }
        }
    }

    fn drain(&mut self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        while let Some(n) = self.work.pop() {
            let n = skip_parens(ast, n);
            if !self.visited.insert(n) {
                continue;
            }
            match *cx.node(n) {
                Node::Arrow { .. } | Node::FunctionExpr { body: Some(_), .. } => {
                    self.check_expression(cx, n)
                }
                Node::FunctionDecl { body: Some(_), .. } => self.check_declaration(cx, n),
                Node::ArrayLit { elements } => self.work.extend(ast.list(elements).iter().rev()),
                Node::PropertyDef {
                    modifiers,
                    key,
                    value,
                    ..
                } => {
                    if !modifiers.contains(Modifiers::PRIVATE)
                        && !modifiers.contains(Modifiers::ABSTRACT)
                        && !matches!(cx.node(key), Node::PrivateName { .. })
                        && let Some(v) = value
                    {
                        self.work.push(v);
                    }
                }
                Node::MethodDef {
                    modifiers,
                    key,
                    value,
                    kind,
                    ..
                } => {
                    if modifiers.contains(Modifiers::PRIVATE)
                        || matches!(cx.node(key), Node::PrivateName { .. })
                    {
                        continue;
                    }
                    if body_of(cx, value).is_none() {
                        if self.visited.insert(value) {
                            self.check_empty_body(cx, value, kind);
                        }
                    } else {
                        self.work.push(value);
                    }
                }
                Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                    self.work.extend(ast.list(members).iter().rev())
                }
                Node::Ident { .. } => self.follow(cx, n),
                Node::ObjectLit { props } => self.work.extend(ast.list(props).iter().rev()),
                Node::Property { value, .. } => self.work.push(value),
                Node::ShorthandProp { name, .. } => self.work.push(name),
                Node::VarDecl { decls, .. } => self.work.extend(ast.list(decls).iter().rev()),
                Node::VarDeclarator { init: Some(i), .. } => self.work.push(i),
                _ => {}
            }
        }
    }

    /// `f` is returned (directly, through every level) by a function this rule checked.
    fn exported_higher_order(&self, cx: &Ctx<'_, '_>, f: NodeId) -> bool {
        let mut cur = es_parent(cx, f);
        while let Some(c) = cur {
            let c = if matches!(cx.node(c), Node::Return { .. }) {
                match cx.parent(c).and_then(|b| cx.parent(b)) {
                    Some(g) => g,
                    None => return false,
                }
            } else {
                c
            };
            if !matches!(
                cx.node(c),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
            ) || !immediately_returns_function(cx, c)
            {
                return false;
            }
            if self.checked.contains(&c) {
                return true;
            }
            cur = es_parent(cx, c);
        }
        false
    }
}

impl Rule for ExplicitModuleBoundaryTypes {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut c = Collect::default();
        walk(ast, ast.root, &mut c);
        let mut run = Run {
            rule: self,
            visited: FxHashSet::default(),
            checked: FxHashSet::default(),
            work: Vec::new(),
        };
        for &e in &c.exports {
            match *cx.node(e) {
                Node::ExportNamed { specifiers, .. } => {
                    for &s in ast.list(specifiers) {
                        if let Node::ExportSpecifier { local, .. } = *cx.node(s)
                            && matches!(cx.node(local), Node::Ident { .. })
                        {
                            run.follow(cx, local);
                        }
                    }
                }
                Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => {
                    run.work.push(expr)
                }
                _ => run.work.push(e),
            }
            run.drain(cx);
        }
        for &f in &c.functions {
            if run.exported_higher_order(cx, f) {
                run.work.push(f);
                run.drain(cx);
            }
        }
    }
}
