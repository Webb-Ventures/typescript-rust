//! `isolatedDeclarations` (TS9xxx): a declaration the emitter writes must have a type that
//! follows from its syntax alone. The rules are tsc 7.0.2's, as observed in its baselines:
//!
//! - Literals (numbers, strings, bigints, booleans, `null`, `undefined`, templates without
//!   substitutions; `-1`) are inferable. A template with substitutions is inferable where
//!   the type widens to `string` (`let`, mutable properties, returns), not where it would be
//!   a literal type (`const`, `readonly`, `as const`).
//! - `e as T` / `<T>e` (not `as const`) are inferable; `e as const` is when `e` is.
//! - Object literals are when every property value is (else TS9013 at the value), with
//!   no spread (TS9015), shorthand (TS9016) or computed name other than a string or number
//!   literal (TS9038); methods need inferable returns (TS9008); an accessor pair needs one
//!   annotation or a getter with an inferable return (TS9009).
//! - Array literals only under `as const` (TS9017), without spread elements (TS9018).
//! - Functions and arrows need annotated parameters (or inferable defaults, TS9011) and
//!   an annotated return type, or a single `return` of an inferable expression (TS9007).
//! - Class expressions never are (TS9022). Anything else (identifiers, calls, operators)
//!   is not: the declaration's own error (TS9010 variable, TS9012 property, TS9011
//!   parameter, TS9037 default export) at its name.
//!
//! Each error refuses the file's `.d.ts`, as tsc writes none for it.

use ts_r_ast::{FnFlags, MethodKind, Node, NodeId, UnaryOp};

use super::{Emitter, reuse, skip_parens};

/// An error at a node: `code` `None` means the declaration's own code.
#[derive(Clone, Copy, Debug)]
struct Issue {
    code: Option<u32>,
    at: NodeId,
}

fn message(code: u32) -> &'static str {
    match code {
        9007 => {
            "Function must have an explicit return type annotation with --isolatedDeclarations."
        }
        9008 => "Method must have an explicit return type annotation with --isolatedDeclarations.",
        9009 => {
            "At least one accessor must have an explicit type annotation with --isolatedDeclarations."
        }
        9010 => "Variable must have an explicit type annotation with --isolatedDeclarations.",
        9011 => "Parameter must have an explicit type annotation with --isolatedDeclarations.",
        9012 => "Property must have an explicit type annotation with --isolatedDeclarations.",
        9013 => "Expression type can't be inferred with --isolatedDeclarations.",
        9015 => {
            "Objects that contain spread assignments can't be inferred with --isolatedDeclarations."
        }
        9016 => {
            "Objects that contain shorthand properties can't be inferred with --isolatedDeclarations."
        }
        9017 => "Only const arrays can be inferred with --isolatedDeclarations.",
        9018 => "Arrays with spread elements can't inferred with --isolatedDeclarations.",
        9019 => "Binding elements can't be exported directly with --isolatedDeclarations.",
        9020 => {
            "Enum member initializers must be computable without references to external symbols with --isolatedDeclarations."
        }
        9021 => "Extends clause can't contain an expression with --isolatedDeclarations.",
        9022 => "Inference from class expressions is not supported with --isolatedDeclarations.",
        9023 => {
            "Assigning properties to functions without declaring them is not supported with --isolatedDeclarations. Add an explicit declaration for the properties assigned to this function."
        }
        9025 => {
            "Declaration emit for this parameter requires implicitly adding undefined to its type. This is not supported with --isolatedDeclarations."
        }
        9026 => {
            "Declaration emit for this file requires preserving this import for augmentations. This is not supported with --isolatedDeclarations."
        }
        9037 => "Default exports can't be inferred with --isolatedDeclarations.",
        9038 => {
            "Computed property names on class or object literals cannot be inferred with --isolatedDeclarations."
        }
        _ => "The declaration cannot be written with --isolatedDeclarations.",
    }
}

impl Emitter<'_, '_> {
    pub(super) fn iso_on(&self, n: NodeId) -> bool {
        self.opts.isolated_declarations && !self.is_synth(n)
    }

    /// Records a TS9xxx error at `at` (the file's `.d.ts` is refused).
    pub(super) fn iso_error(&mut self, code: u32, at: NodeId) {
        let span = self.ast.span(at);
        if !self.failed.iter().any(|f| f.0 == code && f.1 == span) {
            self.failed
                .push((code, span, message(code).to_owned(), None));
        }
    }

    fn iso_report(&mut self, issues: Vec<Issue>, own: u32, own_at: NodeId) {
        for i in issues {
            // An object literal's computed name stays as written (`{ [u]: number }`), so
            // the entity it names is printed too: its own declaration may need a type.
            if i.code == Some(9038)
                && let Node::ComputedKey { expr } = *self.ast.node(i.at)
                && is_entity_name_expr(&self.ast, expr)
            {
                let mut root = expr;
                while let Node::Member { obj, .. } = *self.ast.node(root) {
                    root = obj;
                }
                self.reference_ident(root, ts_r_binder::meaning::VALUE);
            }
            match i.code {
                Some(c) => self.iso_error(c, i.at),
                None => self.iso_error(own, own_at),
            }
        }
    }

    /// A variable declarator written without an annotation.
    pub(super) fn iso_variable(&mut self, d: NodeId, mutable: bool) {
        if !self.iso_on(d) {
            return;
        }
        match *self.ast.node(d) {
            Node::VarDeclarator {
                name,
                init: Some(i),
                ..
            } => {
                let issue = self.iso_expr(i, mutable, false);
                self.iso_report(issue, 9010, name);
            }
            Node::VarDeclarator { name, .. } => self.iso_error(9010, name),
            // A name bound by a destructuring pattern.
            _ => self.iso_error(9019, d),
        }
    }

    /// A class property written without an annotation.
    pub(super) fn iso_property(&mut self, key: NodeId, value: Option<NodeId>, readonly: bool) {
        if !self.iso_on(key) {
            return;
        }
        match value {
            Some(v) => {
                let issue = self.iso_expr(v, !readonly, false);
                self.iso_report(issue, 9012, key);
            }
            None => self.iso_error(9012, key),
        }
    }

    /// A computed member name of a class (only literals are inferable).
    pub(super) fn iso_class_key(&mut self, key: NodeId) {
        if self.iso_on(key)
            && let Some(i) = self.iso_key(key)
        {
            self.iso_error(9038, i.at);
        }
    }

    /// A parameter written without an annotation; `add_undefined` when its type gains
    /// `| undefined` (a default before a required parameter: TS9025 for an asserted type).
    pub(super) fn iso_param(&mut self, p: NodeId, add_undefined: bool) {
        if !self.iso_on(p) {
            return;
        }
        let mut issues = Vec::new();
        self.iso_param_issues(p, &mut issues);
        if !issues.is_empty() {
            self.iso_report(issues, 9011, p);
        } else if add_undefined
            && let Node::Param { init: Some(i), .. } = *self.ast.node(p)
            && reuse::is_reusable(&self.ast, self.src, i)
        {
            // `p = x as T` before a required parameter: `T | undefined` is not syntax.
            self.iso_error(9025, p);
        }
    }

    /// A function, method or getter written without a return annotation.
    pub(super) fn iso_return(&mut self, f: NodeId, code: u32, at: NodeId) {
        if !self.iso_on(f) {
            return;
        }
        if !self.iso_return_inferable(f) {
            self.iso_error(code, at);
        }
    }

    /// An accessor pair without annotations: the getter's `return` must be inferable.
    pub(super) fn iso_accessor(&mut self, get: Option<NodeId>, set: Option<NodeId>, at: NodeId) {
        // A computed name is its own error (TS9038).
        if !self.iso_on(at) || matches!(self.ast.node(at), Node::ComputedKey { .. }) {
            return;
        }
        let get_fn = get.and_then(|g| match *self.ast.node(g) {
            Node::MethodDef { value, .. } => Some(value),
            _ => None,
        });
        if get_fn.is_some_and(|f| self.iso_return_inferable(f)) {
            return;
        }
        // The setter's parameter, else the getter's name.
        let set_param = set.and_then(|s| match *self.ast.node(s) {
            Node::MethodDef { value, .. } => match *self.ast.node(value) {
                Node::FunctionExpr { params, .. } => self.ast.list(params).first().copied(),
                _ => None,
            },
            _ => None,
        });
        let at = match (get, set_param) {
            (_, Some(p)) => match *self.ast.node(p) {
                Node::Param { pattern, .. } => pattern,
                _ => p,
            },
            (Some(g), None) => match *self.ast.node(g) {
                Node::MethodDef { key, .. } => key,
                _ => g,
            },
            _ => at,
        };
        self.iso_error(9009, at);
    }

    /// A signature member without a return type (an implicit `any`).
    pub(super) fn iso_implicit_any(&mut self, m: NodeId) {
        if self.iso_on(m) {
            self.iso_error(9013, m);
        }
    }

    /// `export default <expression>` (not an identifier).
    pub(super) fn iso_default(&mut self, expr: NodeId) {
        if !self.iso_on(expr) {
            return;
        }
        let issue = self.iso_expr(expr, false, false);
        self.iso_report(issue, 9037, expr);
    }

    /// `class X extends <expression>`.
    pub(super) fn iso_heritage(&mut self, expr: NodeId) {
        if self.iso_on(expr) {
            self.iso_error(9021, expr);
        }
    }

    /// Enum member initializers may only refer to earlier constant members of the same
    /// enum (by name or qualified by the enum's own name).
    pub(super) fn iso_enum(&mut self, enum_name: NodeId, members: &[NodeId]) {
        if !self.iso_on(enum_name) {
            return;
        }
        let own = self.raw(enum_name).to_owned();
        let mut ok_members: Vec<String> = Vec::new();
        for &m in members {
            let Node::EnumMember { name, init } = *self.ast.node(m) else {
                continue;
            };
            let mname = self.raw(name).trim_matches(['"', '\'']).to_owned();
            let bad = init.is_some_and(|i| !self.enum_init_ok(i, &own, &ok_members, 0));
            if bad {
                self.iso_error(9020, name);
            } else {
                ok_members.push(mname);
            }
        }
    }

    fn enum_init_ok(&self, e: NodeId, own: &str, ok: &[String], depth: u32) -> bool {
        if depth > 64 {
            return false;
        }
        match *self.ast.node(e) {
            Node::Ident { .. } => ok.iter().any(|m| m == self.raw(e)),
            Node::Member { obj, prop, .. } => {
                matches!(self.ast.node(obj), Node::Ident { .. })
                    && self.raw(obj) == own
                    && ok.iter().any(|m| m == self.raw(prop))
            }
            Node::Index { obj, index, .. } => {
                matches!(self.ast.node(obj), Node::Ident { .. })
                    && self.raw(obj) == own
                    && matches!(self.ast.node(index), Node::StringLit { .. })
                    && ok
                        .iter()
                        .any(|m| m == self.raw(index).trim_matches(['"', '\'']))
            }
            // Not a constant: written without a value, nothing to compute.
            Node::Call { .. } | Node::New { .. } => true,
            _ => {
                let mut fine = true;
                self.ast.for_each_child(e, |c| {
                    if fine && !self.enum_init_ok(c, own, ok, depth + 1) {
                        fine = false;
                    }
                });
                fine
            }
        }
    }

    /// `f.x = ...` after a function declaration that is written (TS9023).
    pub(super) fn iso_expandos(&mut self, stmts: &[NodeId], written: &[NodeId]) {
        if !self.opts.isolated_declarations {
            return;
        }
        // Each property is reported once, at its first assignment (its declaration).
        let mut seen: Vec<(&str, String)> = Vec::new();
        for &s in stmts {
            let Node::ExprStmt { expr } = *self.ast.node(s) else {
                continue;
            };
            let Node::Assign { left, .. } = *self.ast.node(expr) else {
                continue;
            };
            // The property: a name, a literal key, or a key that is an entity name
            // (`f[k.x]`); other element accesses declare nothing.
            let (obj, prop) = match *self.ast.node(left) {
                Node::Member { obj, prop, .. } => (obj, self.raw(prop).to_owned()),
                Node::Index { obj, index, .. } => match *self.ast.node(index) {
                    Node::StringLit { value } => (obj, self.ast.atom(value).to_owned()),
                    Node::NumberLit { .. } => (obj, self.raw(index).to_owned()),
                    _ if is_entity_name_expr(&self.ast, index) => {
                        (obj, format!("[{}]", self.raw(index)))
                    }
                    _ => continue,
                },
                _ => continue,
            };
            if !matches!(self.ast.node(obj), Node::Ident { .. }) {
                continue;
            }
            let name = self.raw(obj);
            if seen.iter().any(|(o, p)| *o == name && *p == prop) {
                continue;
            }
            seen.push((name, prop));
            let target = stmts.iter().copied().find(|&d| match *self.ast.node(d) {
                Node::FunctionDecl { name: Some(n), .. } => self.raw(n) == name,
                Node::VarDecl { decls, .. } => self.ast.list(decls).iter().any(|&v| {
                    matches!(*self.ast.node(v), Node::VarDeclarator { name: n, init: Some(i), .. }
                        if self.raw(n) == name
                            && matches!(self.ast.node(skip_parens(&self.ast, i)),
                                Node::Arrow { .. } | Node::FunctionExpr { .. }))
                }),
                _ => false,
            });
            if let Some(t) = target
                && written.contains(&t)
            {
                self.iso_error(9023, left);
            }
        }
    }

    // ---- inferability -----------------------------------------------------------------

    /// Errors of a parameter without annotation (none when it has one).
    fn iso_param_issues(&self, p: NodeId, out: &mut Vec<Issue>) {
        let Node::Param {
            pattern,
            type_ann,
            init,
            ..
        } = *self.ast.node(p)
        else {
            return;
        };
        if type_ann.is_some() {
            return;
        }
        if matches!(self.ast.node(pattern), Node::Ident { .. }) && self.raw(pattern) == "this" {
            return;
        }
        match init {
            None => out.push(Issue {
                code: Some(9011),
                at: pattern,
            }),
            Some(i) => {
                for is in self.iso_expr(i, true, false) {
                    out.push(match is.code {
                        Some(_) => is,
                        None => Issue {
                            code: Some(9011),
                            at: i,
                        },
                    });
                }
            }
        }
    }

    /// Whether the return type of function-like `f` is annotated or follows from a
    /// single inferable `return`.
    fn iso_return_inferable(&self, f: NodeId) -> bool {
        let (flags, return_type) = match *self.ast.node(f) {
            Node::Arrow {
                flags, return_type, ..
            }
            | Node::FunctionExpr {
                flags, return_type, ..
            }
            | Node::FunctionDecl {
                flags, return_type, ..
            } => (flags, return_type),
            _ => return true,
        };
        if return_type.is_some() {
            return true;
        }
        if flags.intersects(FnFlags::ASYNC) || flags.intersects(FnFlags::GENERATOR) {
            return false;
        }
        match reuse::single_return(&self.ast, f) {
            Some(r) => self.iso_expr(r, true, false).is_empty(),
            None => false,
        }
    }

    /// Why the type of expression `e` does not follow from its syntax (empty when it
    /// does). `mutable`: the type widens (no literal types); `in_const`: under `as const`.
    fn iso_expr(&self, e: NodeId, mutable: bool, in_const: bool) -> Vec<Issue> {
        let mut out = Vec::new();
        self.iso_expr_into(e, mutable, in_const, 0, &mut out);
        out
    }

    /// The errors of `e` nested in an object or array literal: its own failure is TS9013
    /// at `at`.
    fn iso_nested(
        &self,
        e: NodeId,
        at: NodeId,
        mutable: bool,
        in_const: bool,
        depth: u32,
        out: &mut Vec<Issue>,
    ) {
        let mut inner = Vec::new();
        self.iso_expr_into(e, mutable, in_const, depth, &mut inner);
        for i in inner {
            out.push(match i.code {
                Some(_) => i,
                None => Issue {
                    code: Some(9013),
                    at,
                },
            });
        }
    }

    fn iso_expr_into(
        &self,
        e: NodeId,
        mutable: bool,
        in_const: bool,
        depth: u32,
        out: &mut Vec<Issue>,
    ) {
        let own = Issue { code: None, at: e };
        if depth > 64 {
            out.push(own);
            return;
        }
        let e = skip_parens(&self.ast, e);
        match *self.ast.node(e) {
            Node::NumberLit { .. }
            | Node::StringLit { .. }
            | Node::BigIntLit { .. }
            | Node::BoolLit { .. }
            | Node::NullLit {} => {}
            Node::Ident { .. } if self.raw(e) == "undefined" => {}
            Node::TemplateLit { parts } => {
                if self.ast.list(parts).len() > 1 && (!mutable || in_const) {
                    out.push(own);
                }
            }
            Node::Unary {
                op: UnaryOp::Minus | UnaryOp::Plus,
                arg,
            } if matches!(
                self.ast.node(arg),
                Node::NumberLit { .. } | Node::BigIntLit { .. }
            ) => {}
            Node::As { expr, .. } | Node::TypeAssertion { expr, .. } => {
                if !reuse::is_reusable(&self.ast, self.src, e) {
                    self.iso_expr_into(expr, mutable, true, depth + 1, out);
                }
            }
            Node::Satisfies { expr, .. } => {
                self.iso_expr_into(expr, mutable, in_const, depth + 1, out)
            }
            Node::ClassExpr { .. } => out.push(Issue {
                code: Some(9022),
                at: e,
            }),
            Node::Arrow { params, .. } | Node::FunctionExpr { params, .. } => {
                for &p in self.ast.list(params) {
                    self.iso_param_issues(p, out);
                }
                if !self.iso_return_inferable(e) {
                    out.push(Issue {
                        code: Some(9007),
                        at: e,
                    });
                }
            }
            Node::ArrayLit { elements } => {
                if !in_const {
                    out.push(Issue {
                        code: Some(9017),
                        at: e,
                    });
                    return;
                }
                for &el in self.ast.list(elements) {
                    match *self.ast.node(el) {
                        Node::Spread { .. } => out.push(Issue {
                            code: Some(9018),
                            at: el,
                        }),
                        Node::Hole {} => {}
                        _ => self.iso_nested(el, el, mutable, in_const, depth + 1, out),
                    }
                }
            }
            Node::ObjectLit { props } => {
                let props = self.ast.list(props).to_vec();
                for &m in &props {
                    match *self.ast.node(m) {
                        Node::Spread { .. } => out.push(Issue {
                            code: Some(9015),
                            at: m,
                        }),
                        Node::ShorthandProp { .. } => out.push(Issue {
                            code: Some(9016),
                            at: m,
                        }),
                        Node::Property { key, value, .. } => {
                            out.extend(self.iso_key(key));
                            let at = skip_parens(&self.ast, value);
                            self.iso_nested(value, at, !in_const, in_const, depth + 1, out);
                        }
                        Node::MethodDef {
                            kind, key, value, ..
                        } => {
                            out.extend(self.iso_key(key));
                            match kind {
                                MethodKind::Get | MethodKind::Set => {
                                    out.extend(self.iso_object_accessor(m, &props));
                                }
                                _ => {
                                    if let Node::FunctionExpr { params, .. } = *self.ast.node(value)
                                    {
                                        for &p in self.ast.list(params) {
                                            self.iso_param_issues(p, out);
                                        }
                                    }
                                    if !self.iso_return_inferable(value) {
                                        out.push(Issue {
                                            code: Some(9008),
                                            at: key,
                                        });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => out.push(own),
        }
    }

    /// A computed name other than a primitive literal (TS9038): a string, number, boolean or
    /// substitution-free template literal, or a signed number (`[-1]`).
    fn iso_key(&self, key: NodeId) -> Option<Issue> {
        let Node::ComputedKey { expr } = *self.ast.node(key) else {
            return None;
        };
        let e = skip_parens(&self.ast, expr);
        let literal = match *self.ast.node(e) {
            Node::StringLit { .. } | Node::NumberLit { .. } | Node::BoolLit { .. } => true,
            Node::TemplateLit { parts } => self.ast.list(parts).len() == 1,
            Node::Unary {
                op: UnaryOp::Minus | UnaryOp::Plus,
                arg,
            } => matches!(self.ast.node(arg), Node::NumberLit { .. }),
            _ => false,
        };
        if literal {
            return None;
        }
        Some(Issue {
            code: Some(9038),
            at: key,
        })
    }

    /// An object literal accessor: its pair needs an annotation or an inferable getter.
    fn iso_object_accessor(&self, m: NodeId, props: &[NodeId]) -> Option<Issue> {
        let Node::MethodDef { key, .. } = *self.ast.node(m) else {
            return None;
        };
        let name = self.raw(key);
        let pair: Vec<NodeId> = props
            .iter()
            .copied()
            .filter(|&o| {
                matches!(*self.ast.node(o), Node::MethodDef { kind: MethodKind::Get | MethodKind::Set, key: k, .. }
                    if self.raw(k) == name)
            })
            .collect();
        let mut get = None;
        let mut set_param = None;
        for &o in &pair {
            let Node::MethodDef { kind, value, .. } = *self.ast.node(o) else {
                continue;
            };
            let Node::FunctionExpr {
                params,
                return_type,
                ..
            } = *self.ast.node(value)
            else {
                continue;
            };
            match kind {
                MethodKind::Get => {
                    if return_type.is_some() {
                        return None;
                    }
                    get = Some(value);
                }
                _ => {
                    if let Some(&p) = self.ast.list(params).first() {
                        if matches!(
                            *self.ast.node(p),
                            Node::Param {
                                type_ann: Some(_),
                                ..
                            }
                        ) {
                            return None;
                        }
                        set_param = Some(p);
                    }
                }
            }
        }
        if get.is_some_and(|g| self.iso_return_inferable(g)) {
            return None;
        }
        // Reported once, at the first accessor of the pair.
        if pair.first() != Some(&m) {
            return None;
        }
        let at = match set_param.map(|p| *self.ast.node(p)) {
            Some(Node::Param { pattern, .. }) => pattern,
            _ => key,
        };
        Some(Issue {
            code: Some(9009),
            at,
        })
    }
}

/// An identifier or a property access chain of identifiers (`a`, `a.b.c`).
fn is_entity_name_expr(ast: &ts_r_ast::Ast, n: NodeId) -> bool {
    let mut cur = n;
    for _ in 0..256 {
        match *ast.node(cur) {
            Node::Ident { .. } => return true,
            Node::Member { obj, .. } => cur = obj,
            _ => return false,
        }
    }
    false
}
