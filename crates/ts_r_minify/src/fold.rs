//! Constant evaluation and dead-branch removal.
//!
//! [`fold`] removes branches whose test is a compile-time constant: `if`, `?:`, `&&`, `||`,
//! `??`, `while (false)` and the unreachable cases of `switch (constant)`. `var` declarations (and, in sloppy code, block functions that
//! Annex B binds as `var`) inside a removed branch are kept as `var x;` so hoisting is
//! unchanged. A selected operand that is called, tagged, deleted or `typeof`'d is wrapped as
//! `(0, x)` where it would otherwise mean something else (`(1 && o.m)()` must not pass `o`
//! as `this`). The bundler runs this on every module after `define` substitution, before
//! scanning imports, so a `require` in a dead `process.env.NODE_ENV` branch is never
//! followed.
//!
//! [`eval`] is the shared constant evaluator; [`crate::compress`] builds on it.

use ts_r_ast::{Ast, BinaryOp, Node, NodeId, NodeList, UnaryOp, VarKind};

use crate::build;
use crate::purity::Purity;
use crate::scope::{ScopeId, Scopes, SymbolKind};

/// A compile-time constant.
#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Const {
    pub fn truthy(&self) -> bool {
        match self {
            Const::Undefined | Const::Null => false,
            Const::Bool(b) => *b,
            Const::Num(n) => *n != 0.0 && !n.is_nan(),
            Const::Str(s) => !s.is_empty(),
        }
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Const::Undefined | Const::Null)
    }

    fn type_of(&self) -> &'static str {
        match self {
            Const::Undefined => "undefined",
            Const::Null => "object",
            Const::Bool(_) => "boolean",
            Const::Num(_) => "number",
            Const::Str(_) => "string",
        }
    }

    fn strict_eq(&self, other: &Const) -> bool {
        match (self, other) {
            (Const::Num(a), Const::Num(b)) => a == b,
            _ => self == other,
        }
    }

    fn loose_eq(&self, other: &Const) -> Option<bool> {
        match (self, other) {
            (a, b) if a.is_nullish() && b.is_nullish() => Some(true),
            (a, b) if a.is_nullish() != b.is_nullish() => Some(false),
            (a, b) if std::mem::discriminant(a) == std::mem::discriminant(b) => {
                Some(a.strict_eq(b))
            }
            _ => None,
        }
    }

    /// `ToString` for primitives (numbers use JavaScript formatting for common cases).
    pub fn to_js_string(&self) -> Option<String> {
        Some(match self {
            Const::Undefined => "undefined".into(),
            Const::Null => "null".into(),
            Const::Bool(b) => b.to_string(),
            Const::Num(n) => number_to_string(*n)?,
            Const::Str(s) => s.clone(),
        })
    }
}

/// JavaScript's `Number.prototype.toString()`.
pub fn number_to_string(n: f64) -> Option<String> {
    Some(ts_r_codegen::literals::js_number(n))
}

/// Evaluates a side-effect-free constant expression.
pub fn eval(ast: &Ast, id: NodeId, scopes: Option<&Scopes>) -> Option<Const> {
    eval_d(ast, id, scopes, 0)
}

fn is_global(scopes: Option<&Scopes>, id: NodeId) -> bool {
    scopes.is_none_or(|s| s.symbol_of(id).is_none())
}

fn eval_d(ast: &Ast, id: NodeId, scopes: Option<&Scopes>, d: u32) -> Option<Const> {
    if d > 64 {
        return None;
    }
    let d = d + 1;
    Some(match *ast.node(id) {
        Node::NullLit {} => Const::Null,
        Node::BoolLit { value } => Const::Bool(value),
        Node::NumberLit { value } => Const::Num(value.get()),
        Node::StringLit { value } => Const::Str(ast.atom(value).to_owned()),
        Node::Ident { name } if is_global(scopes, id) => match ast.atom(name) {
            "undefined" => Const::Undefined,
            "NaN" => Const::Num(f64::NAN),
            "Infinity" => Const::Num(f64::INFINITY),
            _ => return None,
        },
        Node::TemplateLit { parts } => {
            let mut s = String::new();
            for &p in ast.list(parts) {
                match *ast.node(p) {
                    Node::TemplateElement {
                        cooked,
                        invalid_escape: false,
                    } => s.push_str(ast.atom(cooked)),
                    Node::TemplateElement { .. } => return None,
                    _ => s.push_str(&eval_d(ast, p, scopes, d)?.to_js_string()?),
                }
            }
            Const::Str(s)
        }
        Node::Paren { expr } => return eval_d(ast, expr, scopes, d),
        Node::Unary { op, arg } => {
            if op == UnaryOp::TypeOf
                && let Node::Ident { name } = *ast.node(arg)
                && !is_global(scopes, arg)
            {
                // `typeof localFn` isn't known without types; give up.
                let _ = name;
                return None;
            }
            let v = eval_d(ast, arg, scopes, d)?;
            match op {
                UnaryOp::Not => Const::Bool(!v.truthy()),
                UnaryOp::Void => Const::Undefined,
                UnaryOp::TypeOf => Const::Str(v.type_of().to_owned()),
                UnaryOp::Minus => match v {
                    Const::Num(n) => Const::Num(-n),
                    _ => return None,
                },
                UnaryOp::Plus => match v {
                    Const::Num(n) => Const::Num(n),
                    Const::Bool(b) => Const::Num(if b { 1.0 } else { 0.0 }),
                    Const::Null => Const::Num(0.0),
                    _ => return None,
                },
                _ => return None,
            }
        }
        Node::Binary { left, op, right } => {
            match op {
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce => {
                    let l = eval_d(ast, left, scopes, d)?;
                    let pick_left = match op {
                        BinaryOp::LogicalAnd => !l.truthy(),
                        BinaryOp::LogicalOr => l.truthy(),
                        _ => !l.is_nullish(),
                    };
                    return if pick_left {
                        Some(l)
                    } else {
                        eval_d(ast, right, scopes, d)
                    };
                }
                _ => {}
            }
            let l = eval_d(ast, left, scopes, d)?;
            let r = eval_d(ast, right, scopes, d)?;
            match op {
                BinaryOp::StrictEq => Const::Bool(l.strict_eq(&r)),
                BinaryOp::StrictNotEq => Const::Bool(!l.strict_eq(&r)),
                BinaryOp::Eq => Const::Bool(l.loose_eq(&r)?),
                BinaryOp::NotEq => Const::Bool(!l.loose_eq(&r)?),
                BinaryOp::Add => match (&l, &r) {
                    (Const::Num(a), Const::Num(b)) => Const::Num(a + b),
                    (Const::Str(_), _) | (_, Const::Str(_)) => {
                        Const::Str(format!("{}{}", l.to_js_string()?, r.to_js_string()?))
                    }
                    _ => return None,
                },
                BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => match (&l, &r) {
                    (Const::Num(a), Const::Num(b)) => Const::Num(match op {
                        BinaryOp::Sub => a - b,
                        BinaryOp::Mul => a * b,
                        BinaryOp::Div => a / b,
                        _ => a % b,
                    }),
                    _ => return None,
                },
                BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => match (&l, &r) {
                    (Const::Num(a), Const::Num(b)) => Const::Bool(match op {
                        BinaryOp::Lt => a < b,
                        BinaryOp::LtEq => a <= b,
                        BinaryOp::Gt => a > b,
                        _ => a >= b,
                    }),
                    (Const::Str(a), Const::Str(b)) if a.is_ascii() && b.is_ascii() => {
                        Const::Bool(match op {
                            BinaryOp::Lt => a < b,
                            BinaryOp::LtEq => a <= b,
                            BinaryOp::Gt => a > b,
                            _ => a >= b,
                        })
                    }
                    _ => return None,
                },
                _ => return None,
            }
        }
        Node::Conditional { test, cons, alt } => {
            let t = eval_d(ast, test, scopes, d)?;
            return eval_d(ast, if t.truthy() { cons } else { alt }, scopes, d);
        }
        _ => return None,
    })
}

/// Truthiness of an expression whose value may not be fully known: object literals,
/// functions and classes are truthy. `None` when unknown or when evaluating it has side
/// effects.
pub fn truthiness(ast: &Ast, id: NodeId, scopes: Option<&Scopes>) -> Option<bool> {
    if let Some(c) = eval(ast, id, scopes) {
        return Some(c.truthy());
    }
    match *ast.node(id) {
        Node::Paren { expr } => truthiness(ast, expr, scopes),
        Node::ObjectLit { .. }
        | Node::ArrayLit { .. }
        | Node::FunctionExpr { .. }
        | Node::Arrow { .. }
        | Node::RegexLit { .. } => {
            let p = Purity {
                ast,
                src: "",
                scopes,
                annotations: false,
            };
            p.expr(id).then_some(true)
        }
        _ => None,
    }
}

/// Removes constant branches everywhere in the tree (see the module docs). `src` is used to
/// check purity annotations. Returns true when something changed (scope analysis of the
/// old tree is then stale).
pub fn fold(ast: &mut Ast, src: &str, scopes: &Scopes) -> bool {
    let root = ast.root;
    let mut f = Folder {
        src,
        scopes,
        depth: 0,
        changed: false,
        scope: ScopeId::MODULE,
    };
    f.visit(ast, root, Use::Value);
    f.changed
}

struct Folder<'a> {
    src: &'a str,
    scopes: &'a Scopes,
    depth: u32,
    changed: bool,
    /// The innermost scope around the node being folded.
    scope: ScopeId,
}

/// How the parent uses an expression, where replacing a folded `?:` / `&&` / `||` / `??`
/// by the operand it selects could change meaning: as a call target or tag a member
/// expression would bind `this` (and `eval` would become a direct eval), `delete` would
/// delete the reference, and `typeof` of an unbound name would stop throwing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Use {
    Value,
    Callee,
    Delete,
    TypeOf,
}

const MAX_DEPTH: u32 = 10_000;

impl Folder<'_> {
    fn visit(&mut self, ast: &mut Ast, id: NodeId, use_: Use) {
        if self.depth > MAX_DEPTH {
            return;
        }
        self.depth += 1;
        let outer = self.scope;
        if let Some(s) = self.scopes.scope_of_node(id) {
            self.scope = s;
        }
        let kids = ast.children(id);
        for k in kids {
            let kid_use = match *ast.node(id) {
                Node::Call { callee, .. } if callee == k => Use::Callee,
                Node::TaggedTemplate { tag, .. } if tag == k => Use::Callee,
                Node::Unary {
                    op: UnaryOp::Delete,
                    ..
                } => Use::Delete,
                Node::Unary {
                    op: UnaryOp::TypeOf,
                    ..
                } => Use::TypeOf,
                Node::Paren { .. } => use_,
                _ => Use::Value,
            };
            self.visit(ast, k, kid_use);
        }
        self.depth -= 1;
        self.fold_node(ast, id, use_);
        self.scope = outer;
    }

    fn pure(&self, ast: &Ast, id: NodeId) -> bool {
        Purity {
            ast,
            src: self.src,
            scopes: Some(self.scopes),
            annotations: true,
        }
        .expr(id)
    }

    fn known(&self, ast: &Ast, test: NodeId) -> Option<bool> {
        let t = truthiness(ast, test, Some(self.scopes))?;
        self.pure(ast, test).then_some(t)
    }

    fn fold_node(&mut self, ast: &mut Ast, id: NodeId, use_: Use) {
        match *ast.node(id) {
            Node::If { test, cons, alt } => {
                let Some(t) = self.known(ast, test) else {
                    return;
                };
                let (keep, drop) = if t {
                    (Some(cons), alt)
                } else {
                    (alt, Some(cons))
                };
                let mut hoisted = Vec::new();
                if let Some(d) = drop {
                    self.hoisted_names(ast, d, &mut hoisted);
                }
                let at = ast.span(id).start;
                let kept = keep.unwrap_or_else(|| build::empty(ast, at));
                let new = if hoisted.is_empty() {
                    kept
                } else {
                    let decls: Vec<_> = hoisted.into_iter().map(|n| (n, None)).collect();
                    let v = build::var_decl_many(ast, VarKind::Var, &decls, at);
                    build::block(ast, &[v, kept], at)
                };
                build::replace(ast, id, new);
                self.changed = true;
            }
            Node::While { test, body } => {
                if self.known(ast, test) == Some(false) {
                    let mut hoisted = Vec::new();
                    self.hoisted_names(ast, body, &mut hoisted);
                    let at = ast.span(id).start;
                    let new = if hoisted.is_empty() {
                        build::empty(ast, at)
                    } else {
                        let decls: Vec<_> = hoisted.into_iter().map(|n| (n, None)).collect();
                        build::var_decl_many(ast, VarKind::Var, &decls, at)
                    };
                    build::replace(ast, id, new);
                    self.changed = true;
                }
            }
            // `try {} catch { C } finally { F }`: the handler can never run.
            Node::Try {
                block,
                handler: Some(handler),
                finalizer,
            } if matches!(*ast.node(block), Node::Block { stmts } if stmts.is_empty()) => {
                let mut hoisted = Vec::new();
                self.hoisted_names(ast, handler, &mut hoisted);
                let at = ast.span(id).start;
                let mut stmts = Vec::new();
                if !hoisted.is_empty() {
                    let decls: Vec<_> = hoisted.into_iter().map(|n| (n, None)).collect();
                    stmts.push(build::var_decl_many(ast, VarKind::Var, &decls, at));
                }
                stmts.extend(finalizer);
                let new = match stmts[..] {
                    [] => build::empty(ast, at),
                    [one] => one,
                    _ => build::block(ast, &stmts, at),
                };
                build::replace(ast, id, new);
                self.changed = true;
            }
            Node::Switch { disc, cases } => self.fold_switch(ast, disc, cases),
            Node::Conditional { test, cons, alt } => {
                if let Some(t) = self.known(ast, test) {
                    let keep = self.as_value(ast, if t { cons } else { alt }, use_);
                    build::replace(ast, id, keep);
                    self.changed = true;
                }
            }
            Node::Binary { left, op, right } => {
                let keep = match op {
                    BinaryOp::LogicalAnd => match self.known(ast, left) {
                        Some(true) => right,
                        Some(false) => left,
                        None => return,
                    },
                    BinaryOp::LogicalOr => match self.known(ast, left) {
                        Some(true) => left,
                        Some(false) => right,
                        None => return,
                    },
                    BinaryOp::Coalesce => {
                        let Some(c) = eval(ast, left, Some(self.scopes)) else {
                            return;
                        };
                        if c.is_nullish() { right } else { left }
                    }
                    _ => return,
                };
                let keep = self.as_value(ast, keep, use_);
                build::replace(ast, id, keep);
                self.changed = true;
            }
            _ => {}
        }
    }

    /// `switch (constant)`: empties the bodies of cases that can neither be jumped to nor be
    /// fallen into (their `var`s stay declared), so a `require()` there is never resolved,
    /// as esbuild does. Case tests are kept: they may still be evaluated. A case is a jump
    /// target when it is a non-constant test before the first matching case, the first
    /// matching case itself, or `default` when no case matches for sure. A switch whose dead
    /// bodies hold a lexical declaration (other cases could see it) is left alone.
    fn fold_switch(&mut self, ast: &mut Ast, disc: NodeId, cases: NodeList) {
        if !self.pure(ast, disc) {
            return;
        }
        let Some(value) = eval(ast, disc, Some(self.scopes)) else {
            return;
        };
        let cases = ast.list(cases).to_vec();
        // Per case: `None` for `default`, else whether it matches (`None`: unknown).
        let mut tests = Vec::with_capacity(cases.len());
        for &c in &cases {
            let Node::SwitchCase { test, .. } = *ast.node(c) else {
                return;
            };
            tests.push(test.map(|t| {
                self.pure(ast, t)
                    .then(|| eval(ast, t, Some(self.scopes)))
                    .flatten()
                    .map(|v| v.strict_eq(&value))
            }));
        }
        let first_match = tests.iter().position(|t| *t == Some(Some(true)));
        let mut dead = Vec::new();
        let mut falls_in = false;
        for (i, &c) in cases.iter().enumerate() {
            let target = match tests[i] {
                None => first_match.is_none(),
                Some(t) => match first_match {
                    Some(m) => i == m || (i < m && t.is_none()),
                    None => t != Some(false),
                },
            };
            let Node::SwitchCase { body, .. } = *ast.node(c) else {
                return;
            };
            let body = ast.list(body);
            if target || falls_in {
                falls_in = !body.last().is_some_and(|&s| {
                    matches!(
                        ast.node(s),
                        Node::Break { .. }
                            | Node::Continue { .. }
                            | Node::Return { .. }
                            | Node::Throw { .. }
                    )
                });
                continue;
            }
            if body.is_empty() {
                continue;
            }
            let lexical = body.iter().any(|&s| match *ast.node(s) {
                Node::VarDecl { kind, .. } => kind != VarKind::Var,
                Node::FunctionDecl { .. } | Node::ClassDecl { .. } => true,
                _ => false,
            });
            if lexical {
                return;
            }
            dead.push(c);
        }
        for c in dead {
            let Node::SwitchCase { test, body } = *ast.node(c) else {
                continue;
            };
            let mut hoisted = Vec::new();
            for s in ast.list(body).to_vec() {
                self.hoisted_names(ast, s, &mut hoisted);
            }
            let at = ast.span(c).start;
            let stmts = if hoisted.is_empty() {
                Vec::new()
            } else {
                let decls: Vec<_> = hoisted.into_iter().map(|n| (n, None)).collect();
                vec![build::var_decl_many(ast, VarKind::Var, &decls, at)]
            };
            let body = ast.add_list(&stmts);
            *ast.node_mut(c) = Node::SwitchCase { test, body };
            self.changed = true;
        }
    }

    /// `kept`, or `(0, kept)` when it replaces an expression used as `use_` and would mean
    /// something else there (see [`Use`]).
    fn as_value(&self, ast: &mut Ast, kept: NodeId, use_: Use) -> NodeId {
        let mut inner = kept;
        while let Node::Paren { expr } = *ast.node(inner) {
            inner = expr;
        }
        let wrap = match (use_, *ast.node(inner)) {
            (Use::Callee | Use::Delete, Node::Member { .. } | Node::Index { .. }) => true,
            (Use::Callee, Node::Ident { name }) => ast.atom(name) == "eval",
            (Use::Delete | Use::TypeOf, Node::Ident { .. }) => true,
            _ => false,
        };
        if !wrap {
            return kept;
        }
        let at = ast.span(kept).start;
        let zero = build::number(ast, 0.0, at);
        build::seq(ast, &[zero, kept], at)
    }

    /// The names a removed statement must keep declared as `var`: its `var` declarations
    /// and, in sloppy code, the block functions that Annex B also binds as `var` (only when
    /// that `var` would not clash with a lexical declaration, where Annex B doesn't apply).
    fn hoisted_names(&self, ast: &Ast, dropped: NodeId, out: &mut Vec<NodeId>) {
        let strict = self.scopes.scope(self.scope).strict;
        collect_var_names(ast, dropped, strict, out);
        if strict {
            return;
        }
        out.retain(|&n| {
            let Node::Ident { name } = *ast.node(n) else {
                return true;
            };
            let mut scope = self.scope;
            loop {
                let s = self.scopes.scope(scope);
                if let Some(sym) = self.scopes.lookup_in(scope, name)
                    && matches!(
                        self.scopes.symbol(sym).kind,
                        SymbolKind::Let
                            | SymbolKind::Const
                            | SymbolKind::Using
                            | SymbolKind::Class
                            | SymbolKind::Import
                    )
                {
                    return false;
                }
                match s.parent {
                    Some(p) if !s.kind.is_var_target() => scope = p,
                    _ => return true,
                }
            }
        });
    }
}

/// Collects the identifiers declared with `var` (and, in sloppy mode, block-level function
/// names) inside `id`, without entering nested functions.
pub fn collect_var_names(ast: &Ast, id: NodeId, strict: bool, out: &mut Vec<NodeId>) {
    let mut stack = vec![id];
    while let Some(n) = stack.pop() {
        match *ast.node(n) {
            Node::VarDecl {
                kind: VarKind::Var,
                decls,
                ..
            } => {
                for &d in ast.list(decls) {
                    if let Node::VarDeclarator { name, .. } = *ast.node(d) {
                        binding_idents(ast, name, out);
                    }
                }
            }
            Node::FunctionDecl { name, .. } => {
                if !strict && let Some(name) = name {
                    out.push(name);
                }
            }
            Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::ClassDecl { .. }
            | Node::ClassExpr { .. } => {}
            _ => {
                let kids = ast.children(n);
                stack.extend(kids.into_iter().rev());
            }
        }
    }
}

/// The identifiers a binding pattern declares.
pub fn binding_idents(ast: &Ast, id: NodeId, out: &mut Vec<NodeId>) {
    match *ast.node(id) {
        Node::Ident { .. } => out.push(id),
        Node::ObjectPattern { props } => {
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::BindingProperty { key, value, .. } => {
                        binding_idents(ast, value.unwrap_or(key), out)
                    }
                    Node::Rest { arg } => binding_idents(ast, arg, out),
                    _ => {}
                }
            }
        }
        Node::ArrayPattern { elements } => {
            for &e in ast.list(elements) {
                binding_idents(ast, e, out);
            }
        }
        Node::AssignPattern { left, .. } => binding_idents(ast, left, out),
        Node::Rest { arg } => binding_idents(ast, arg, out),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;
    use ts_r_codegen::CodegenOptions;

    fn run(src: &str) -> String {
        let mut ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let scopes = Scopes::analyze(&ast, false);
        fold(&mut ast, src, &scopes);
        let o = CodegenOptions {
            layout: ts_r_codegen::Layout::Compact,
            ..CodegenOptions::default()
        };
        ts_r_codegen::print(&ast, src, &o).code
    }

    #[test]
    fn removes_dead_branches() {
        assert_eq!(
            run(r#"if ("production" === "production") { a(); } else { b(); var x = 1; }"#),
            "{var x;{a()}}"
        );
        assert_eq!(run("if (!1) a(); b();"), ";b();");
        assert_eq!(run("x = 0 ? a : b;"), "x=b;");
        assert_eq!(run("x = 'dev' !== 'production' && y;"), "x=y;");
        assert_eq!(run("x = null ?? z;"), "x=z;");
        assert_eq!(run("while (false) { var q; }"), "var q;");
        assert_eq!(run("if (f()) a();"), "if(f())a();");
        assert_eq!(
            run("if (typeof x === 'undefined') a();"),
            "if(typeof x==='undefined')a();"
        );
        assert_eq!(run("if (typeof 1 === 'number') a();"), "a();");
        assert_eq!(run("try {} catch { a(); } finally { b(); }"), "{b()}");
        assert_eq!(run("try {} catch (e) { var v = e; }"), "var v;");
        assert_eq!(run("try {} catch { a() }"), ";");
        assert_eq!(run("try { f() } catch { a() }"), "try{f()}catch{a()}");
    }

    #[test]
    fn empties_unreachable_switch_cases() {
        assert_eq!(
            run("switch (1) { case 0: a(); break; case 1: b(); break; case 2: c(); var v; }"),
            "switch(1){case 0:case 1:b();break;case 2:var v}"
        );
        // Fall-through keeps the next case; a non-constant case before the match is a target.
        assert_eq!(
            run(
                "switch ('b') { case 'a': a(); case 'b': b(); case 'c': c(); break; case 'd': d() }"
            ),
            "switch('b'){case'a':case'b':b();case'c':c();break;case'd':}"
        );
        assert_eq!(
            run("switch (1) { case x: a(); break; case 1: b(); break; case x: c(); default: d() }"),
            "switch(1){case x:a();break;case 1:b();break;case x:default:}"
        );
        // `default` is the target when nothing matches.
        assert_eq!(
            run("switch (0) { case 1: a(); break; default: b() }"),
            "switch(0){case 1:default:b()}"
        );
        assert_eq!(run("switch (x) { case 0: a() }"), "switch(x){case 0:a()}");
        // A lexical declaration in a dead case is visible to the others.
        assert_eq!(
            run("switch (1) { case 0: let q; case 1: q = 1 }"),
            "switch(1){case 0:let q;case 1:q=1}"
        );
    }

    #[test]
    fn evaluates() {
        let ast = ts_r_parser::parse("`a${1 + 2}b` + 'c'", SourceKind::Js).ast;
        let e = match *ast.node(crate::build::program_stmts(&ast)[0]) {
            Node::ExprStmt { expr } => expr,
            _ => unreachable!(),
        };
        assert_eq!(eval(&ast, e, None), Some(Const::Str("a3bc".into())));
        assert_eq!(number_to_string(0.1), Some("0.1".into()));
        assert_eq!(number_to_string(1e21), Some("1e+21".into()));
        assert_eq!(number_to_string(-5.0), Some("-5".into()));
    }

    #[test]
    fn folded_operands_keep_their_call_semantics() {
        // A selected member expression must not become a method call, `eval` must not
        // become a direct eval, and `delete` / `typeof` must keep operating on a value.
        assert_eq!(run("(true ? o.m : null)();"), "(0,o.m)();");
        assert_eq!(run("(false || o[k])?.();"), "(0,o[k])?.();");
        assert_eq!(run("(1 && o.t)`x`;"), "(0,o.t)`x`;");
        assert_eq!(run("(null ?? (o.m))();"), "(0,(o.m))();");
        assert_eq!(run("(true ? eval : 0)('x');"), "(0,eval)('x');");
        // Compact output writes a keyword's space only where the next token would merge
        // with it (d7be5933), as esbuild's minifier does.
        assert_eq!(run("delete (true ? o.p : 0);"), "delete(0,o.p);");
        assert_eq!(run("typeof (1 && x);"), "typeof(0,x);");
        // Elsewhere, and for plain callees, nothing changes.
        assert_eq!(run("y = (true ? o.m : null);"), "y=(o.m);");
        assert_eq!(run("(true ? f : null)();"), "(f)();");
        assert_eq!(run("(true ? o.m : null).call(o);"), "(o.m).call(o);");
    }

    fn run_in(src: &str, strict: bool) -> String {
        let mut ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let scopes = Scopes::analyze(&ast, strict);
        fold(&mut ast, src, &scopes);
        let o = CodegenOptions {
            layout: ts_r_codegen::Layout::Compact,
            ..CodegenOptions::default()
        };
        ts_r_codegen::print(&ast, src, &o).code
    }

    #[test]
    fn removed_block_functions_follow_annex_b() {
        // Sloppy code: a removed block function stays a `var` binding...
        assert_eq!(run("if (false) { function g() {} }"), "{var g;;}");
        // ...unless a lexical declaration of the name blocks Annex B (a `var` would be a
        // syntax error).
        assert_eq!(
            run("let f = 1; if (false) { function f() {} }"),
            "let f=1;;"
        );
        assert_eq!(
            run("if (false) { function f() {} } const f = 1;"),
            ";const f=1;"
        );
        assert_eq!(
            run("function h() { { class f {} if (false) { function f() {} } } }"),
            "function h(){{class f{};}}"
        );
        // Strict code has no Annex B: only real `var`s are kept.
        for (src, strict) in [
            (
                "'use strict'; let f = 1; if (false) { function f() {} var v; }",
                false,
            ),
            ("let f = 1; if (false) { function f() {} var v; }", true),
        ] {
            let out = run_in(src, strict);
            assert!(out.contains("var v;") && !out.contains("var f"), "{out}");
        }
        assert_eq!(
            run("function h() { 'use strict'; if (false) { function g() {} } }"),
            "function h(){'use strict';;}"
        );
    }
}
