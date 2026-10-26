//! `unicorn/no-error-property-assignment`: writing `name`, `stack` or `cause` (and `errors`
//! on an `AggregateError`) on a value known to be a built-in error: `Object.assign(err,
//! {name})` with the global `Object`, `err.name = x`, `err.stack++`. The error is a call or
//! `new` of a global built-in error constructor (through TS wrappers), or a variable known
//! to hold one.
//!
//! What a variable holds is tracked in source order per frame (the program, each block,
//! static block and `switch` case): a plain-name declarator or an unconditional `=`
//! statement at frame level records the constructor; any other write (conditional,
//! nested in an expression, compound, destructuring, a loop target, `++`) makes it unknown.
//! A write inside a bare nested block carries out to the enclosing frames; one inside a
//! conditional block or another function stays local and makes the outer knowledge
//! unknown (a write from a nested function keeps the outer value when it stores another
//! error).

use super::prelude::*;
use super::unicorn_conditions::{idents_in, is_function_node, unwrap_ts_and_parens};
use super::unicorn_static::static_value;
use super::unicorn_text::has_possible_side_effect;
use crate::semantic::{ScopeKind, SymbolId};
use ts_r_ast::Ast;
use ts_r_ast::visit::{Visitor, Walk, walk};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-error-property-assignment",
        "Disallow assigning to built-in error properties.",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoErrorPropertyAssignment>,
};

#[derive(Debug, Default)]
struct NoErrorPropertyAssignment;

const BUILTIN_ERRORS: &[&str] = &[
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
    "SuppressedError",
];

fn disallowed(constructor: &str, property: &str) -> bool {
    matches!(property, "name" | "stack" | "cause")
        || (constructor == "AggregateError" && property == "errors")
}

struct Frame {
    node: NodeId,
    known: Vec<(SymbolId, Option<&'static str>)>,
    function_boundary: bool,
    transparent: bool,
}

impl Frame {
    fn get(&self, sym: SymbolId) -> Option<Option<&'static str>> {
        self.known.iter().find(|(s, _)| *s == sym).map(|&(_, c)| c)
    }
    fn set(&mut self, sym: SymbolId, value: Option<&'static str>) {
        match self.known.iter_mut().find(|(s, _)| *s == sym) {
            Some(entry) => entry.1 = value,
            None => self.known.push((sym, value)),
        }
    }
}

fn is_frame_node(node: &Node) -> bool {
    matches!(
        node,
        Node::Program { .. }
            | Node::Block { .. }
            | Node::StaticBlock { .. }
            | Node::SwitchCase { .. }
    )
}

struct Walker<'c, 'a, 'b> {
    cx: &'c Ctx<'a, 'b>,
    ast: &'c Ast,
    frames: Vec<Frame>,
    reports: Vec<(Span, String)>,
}

impl Walker<'_, '_, '_> {
    fn variable(&self, ident: NodeId) -> Option<SymbolId> {
        matches!(self.ast.node(ident), Node::Ident { .. })
            .then(|| self.cx.semantic().symbol_of_ident(ident))
            .flatten()
    }

    /// The frame node of the scope that declares `sym`.
    fn frame_node_of(&self, sym: SymbolId) -> NodeId {
        let sem = self.cx.semantic();
        let scope = sem.scope(sem.symbol(sym).scope);
        let node = scope.node;
        if scope.kind == ScopeKind::Function {
            let body = match *self.ast.node(node) {
                Node::FunctionDecl { body: Some(b), .. }
                | Node::FunctionExpr { body: Some(b), .. } => Some(b),
                Node::Arrow { body, .. } => Some(body),
                _ => None,
            };
            if let Some(b) = body
                && matches!(self.ast.node(b), Node::Block { .. })
            {
                return b;
            }
        }
        node
    }

    fn can_propagate_to(&self, index: usize) -> bool {
        self.frames[index + 1..].iter().all(|f| f.transparent)
    }

    fn local_update_frame(&mut self) -> &mut Frame {
        let i = self
            .frames
            .iter()
            .rposition(|f| !f.transparent)
            .unwrap_or(0);
        &mut self.frames[i]
    }

    fn update_existing(&mut self, sym: SymbolId, value: Option<&'static str>) -> bool {
        let mut crosses_function = false;
        for i in (0..self.frames.len()).rev() {
            if self.frames[i].get(sym).is_some() {
                if self.can_propagate_to(i) {
                    self.frames[i].set(sym, value);
                    return true;
                }
                if !crosses_function || value.is_none() {
                    self.frames[i].set(sym, None);
                }
                self.local_update_frame().set(sym, value);
                return true;
            }
            if self.frames[i].function_boundary {
                crosses_function = true;
            }
        }
        false
    }

    fn set_from_declaration(&mut self, sym: SymbolId, value: Option<&'static str>) {
        if self.update_existing(sym, value) {
            return;
        }
        let frame_node = self.frame_node_of(sym);
        let current = self.frames.len() - 1;
        let target = self
            .frames
            .iter()
            .rposition(|f| f.node == frame_node)
            .unwrap_or(current);
        if target != current && !self.can_propagate_to(target) {
            self.local_update_frame().set(sym, value);
            return;
        }
        self.frames[target].set(sym, value);
    }

    fn set_from_assignment(&mut self, sym: SymbolId, value: Option<&'static str>) {
        if self.update_existing(sym, value) {
            return;
        }
        let frame_node = self.frame_node_of(sym);
        if let Some(i) = self.frames.iter().rposition(|f| f.node == frame_node)
            && self.can_propagate_to(i)
        {
            self.frames[i].set(sym, value);
            return;
        }
        self.local_update_frame().set(sym, value);
    }

    fn known(&self, sym: SymbolId) -> Option<&'static str> {
        self.frames.iter().rev().find_map(|f| f.get(sym)).flatten()
    }

    fn error_constructor(&self, id: NodeId) -> Option<&'static str> {
        let ast = self.ast;
        let id = unwrap_ts_and_parens(ast, id);
        let callee = match *ast.node(id) {
            Node::Call {
                callee,
                optional: false,
                ..
            }
            | Node::New { callee, .. } => callee,
            _ => return None,
        };
        let name =
            ident_name(ast, callee).filter(|_| matches!(ast.node(callee), Node::Ident { .. }))?;
        let name = BUILTIN_ERRORS.iter().find(|&&n| n == name)?;
        self.cx.is_global_ref(callee, name).then_some(*name)
    }

    fn known_error(&self, id: NodeId) -> Option<&'static str> {
        let id = unwrap_ts_and_parens(self.ast, id);
        self.error_constructor(id)
            .or_else(|| self.variable(id).and_then(|s| self.known(s)))
    }

    fn static_key(&self, key: NodeId) -> Option<String> {
        let ast = self.ast;
        if has_possible_side_effect(self.cx, key) || !idents_in(ast, key).is_empty() {
            return None;
        }
        static_value(self.cx, key)?.to_js_string()
    }

    /// `getPropertyName` of a member access.
    fn member_property(&self, id: NodeId) -> Option<(NodeId, String)> {
        let ast = self.ast;
        match *ast.node(id) {
            Node::Member {
                prop,
                optional: false,
                ..
            } => matches!(ast.node(prop), Node::Ident { .. })
                .then(|| ident_name(ast, prop).map(|n| (prop, n.to_owned())))
                .flatten(),
            Node::Index {
                index,
                optional: false,
                ..
            } => self.static_key(index).map(|k| (index, k)),
            _ => None,
        }
    }

    fn object_key(&self, key: NodeId) -> Option<(NodeId, String)> {
        let ast = self.ast;
        match *ast.node(key) {
            Node::Ident { .. } => ident_name(ast, key).map(|n| (key, n.to_owned())),
            Node::StringLit { value } => Some((key, ast.atom(value).to_owned())),
            Node::NumberLit { value } => Some((key, number_to_string(value.get()))),
            Node::ComputedKey { expr } => self.static_key(expr).map(|k| (expr, k)),
            _ => None,
        }
    }

    fn direct_assignment(&mut self, target: NodeId) {
        let ast = self.ast;
        let target = skip_parens(ast, target);
        let obj = match *ast.node(target) {
            Node::Member { obj, .. } | Node::Index { obj, .. } => obj,
            _ => return,
        };
        let Some(constructor) = self.known_error(obj) else {
            return;
        };
        if let Some((prop, name)) = self.member_property(target)
            && disallowed(constructor, &name)
        {
            self.report(prop, &name);
        }
    }

    fn object_assign(&mut self, call: NodeId) {
        let ast = self.ast;
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(call)
        else {
            return;
        };
        let Node::Member {
            obj,
            prop,
            optional: false,
        } = *ast.node(skip_parens(ast, callee))
        else {
            return;
        };
        let args = ast.list(args);
        if args.len() < 2
            || !matches!(ast.node(obj), Node::Ident { .. })
            || ident_name(ast, obj) != Some("Object")
            || ident_name(ast, prop) != Some("assign")
            || !self.cx.is_global_ref(obj, "Object")
            || matches!(ast.node(args[0]), Node::Spread { .. })
        {
            return;
        }
        let Some(constructor) = self.known_error(args[0]) else {
            return;
        };
        for &source in &args[1..] {
            let Node::ObjectLit { props } = *ast.node(skip_parens(ast, source)) else {
                continue;
            };
            for &p in ast.list(props) {
                let key = match *ast.node(p) {
                    Node::Property { key, .. } | Node::MethodDef { key, .. } => key,
                    Node::ShorthandProp { name, .. } => name,
                    _ => continue,
                };
                if let Some((node, name)) = self.object_key(key)
                    && disallowed(constructor, &name)
                {
                    self.report(node, &name);
                }
            }
        }
    }

    fn report(&mut self, node: NodeId, property: &str) {
        self.reports.push((
            self.cx.span(node),
            format!("Do not assign to `{property}` on a built-in error."),
        ));
    }

    /// Every name an assignment target writes (patterns, defaults, rests, TS wrappers).
    fn clear_targets(&mut self, target: NodeId, depth: u32) {
        if depth > 200 {
            return;
        }
        let ast = self.ast;
        let target = unwrap_ts_and_parens(ast, target);
        match *ast.node(target) {
            Node::Ident { .. } => {
                if let Some(s) = self.variable(target) {
                    self.set_from_assignment(s, None);
                }
            }
            Node::AssignPattern { left, .. } | Node::Assign { left, .. } => {
                self.clear_targets(left, depth + 1)
            }
            Node::Rest { arg } | Node::Spread { arg } => self.clear_targets(arg, depth + 1),
            Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
                for &e in ast.list(elements) {
                    self.clear_targets(e, depth + 1);
                }
            }
            Node::ObjectPattern { props } | Node::ObjectLit { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::Property { value, .. } => self.clear_targets(value, depth + 1),
                        Node::BindingProperty { key, value, .. } => {
                            self.clear_targets(value.unwrap_or(key), depth + 1)
                        }
                        Node::ShorthandProp { name, .. } => self.clear_targets(name, depth + 1),
                        Node::Rest { arg } | Node::Spread { arg } => {
                            self.clear_targets(arg, depth + 1)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn es_parent(&self, id: NodeId) -> Option<NodeId> {
        parent_skip_parens(self.ast, self.cx.file.parents, id)
    }

    fn direct_child_of_frame(&self, id: NodeId) -> bool {
        let ast = self.ast;
        let Some(parent) = self.cx.parent(id) else {
            return false;
        };
        if is_frame_node(ast.node(parent)) {
            return true;
        }
        if !matches!(ast.node(parent), Node::VarDecl { .. }) {
            return false;
        }
        self.cx.parent(parent).is_some_and(|g| {
            is_frame_node(ast.node(g))
                || matches!(*ast.node(g), Node::For { init: Some(i), .. } if i == parent)
        })
    }

    fn unconditional(&self, assign: NodeId) -> bool {
        let ast = self.ast;
        let Some(parent) = self.es_parent(assign) else {
            return false;
        };
        match *ast.node(parent) {
            Node::For { init: Some(i), .. } => skip_parens(ast, i) == assign,
            Node::ExprStmt { .. } => self.direct_child_of_frame(parent),
            _ => false,
        }
    }

    fn assignment(&mut self, node: NodeId, left: NodeId, op: AssignOp, right: NodeId) {
        self.direct_assignment(left);
        let ast = self.ast;
        let target = unwrap_ts_and_parens(ast, left);
        if !matches!(ast.node(target), Node::Ident { .. }) {
            self.clear_targets(target, 0);
            return;
        }
        let Some(sym) = self.variable(target) else {
            return;
        };
        let value = if op == AssignOp::Assign && self.unconditional(node) {
            self.error_constructor(right)
        } else {
            None
        };
        self.set_from_assignment(sym, value);
    }

    fn declarator(&mut self, node: NodeId, name: NodeId, init: Option<NodeId>) {
        if !matches!(self.ast.node(name), Node::Ident { .. }) || !self.direct_child_of_frame(node) {
            return;
        }
        let value = init.and_then(|i| self.error_constructor(i));
        if value.is_none() && init.is_none() {
            return;
        }
        if let Some(sym) = self.cx.semantic().symbol_of_decl(name) {
            self.set_from_declaration(sym, value);
        }
    }
}

impl Visitor for Walker<'_, '_, '_> {
    fn enter(&mut self, ast: &Ast, id: NodeId) -> Walk {
        match *ast.node(id) {
            Node::Block { .. } | Node::StaticBlock { .. } | Node::SwitchCase { .. } => {
                let parent = self.cx.parent(id);
                let parent_node = parent.map(|p| ast.node(p));
                let block = matches!(ast.node(id), Node::Block { .. });
                let function_boundary = block && parent_node.is_some_and(is_function_node);
                let transparent =
                    block && !function_boundary && parent_node.is_some_and(is_frame_node);
                self.frames.push(Frame {
                    node: id,
                    known: Vec::new(),
                    function_boundary,
                    transparent,
                });
            }
            Node::VarDeclarator { name, init, .. } => self.declarator(id, name, init),
            Node::Assign { left, op, right } => self.assignment(id, left, op, right),
            Node::Update { arg, .. } => {
                self.direct_assignment(arg);
                self.clear_targets(arg, 0);
            }
            Node::Call { .. } => self.object_assign(id),
            _ => {}
        }
        Walk::Continue
    }

    fn leave(&mut self, ast: &Ast, id: NodeId) {
        if matches!(
            ast.node(id),
            Node::Block { .. } | Node::StaticBlock { .. } | Node::SwitchCase { .. }
        ) && self.frames.len() > 1
        {
            self.frames.pop();
            return;
        }
        let Some(parent) = self.cx.parent(id) else {
            return;
        };
        let left = match *ast.node(parent) {
            Node::ForIn { left, right, .. } | Node::ForOf { left, right, .. } if right == id => {
                left
            }
            _ => return,
        };
        let target = match *ast.node(left) {
            Node::VarDecl { decls, .. } => match ast.list(decls).first() {
                Some(&d) => match *ast.node(d) {
                    Node::VarDeclarator { name, .. } => name,
                    _ => return,
                },
                None => return,
            },
            _ => left,
        };
        self.clear_targets(target, 0);
    }
}

impl Rule for NoErrorPropertyAssignment {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let root = cx.ast().root;
        let reports = {
            let mut w = Walker {
                cx,
                ast: cx.ast(),
                frames: vec![Frame {
                    node: root,
                    known: Vec::new(),
                    function_boundary: false,
                    transparent: false,
                }],
                reports: Vec::new(),
            };
            walk(cx.ast(), root, &mut w);
            w.reports
        };
        for (span, msg) in reports {
            cx.report(span, msg).id("no-error-property-assignment");
        }
    }
}
