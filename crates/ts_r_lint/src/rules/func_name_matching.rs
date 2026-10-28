//! `func-name-matching`: a named function expression's name should (or, with `"never"`,
//! should not) match the variable, property or field it is assigned to.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "func-name-matching",
        "Require function names to match the name of the variable or property to which they are assigned",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<FuncNameMatching>,
};

#[derive(Debug)]
struct FuncNameMatching {
    never: bool,
    property_descriptor: bool,
    module_exports: bool,
}

impl FromOptions for FuncNameMatching {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = match opts.first() {
            Some(Value::Object(_)) => obj_opt(opts, 0),
            _ => obj_opt(opts, 1),
        };
        Ok(FuncNameMatching {
            never: opts.first().and_then(Value::as_str) == Some("never"),
            property_descriptor: bool_field(o, "considerPropertyDescriptor", false),
            module_exports: bool_field(o, "includeCommonJSModuleExports", false),
        })
    }
}

/// esutils' `isIdentifierES5` / `isIdentifierES6` (non-strict).
fn is_identifier(name: &str, es6: bool) -> bool {
    let mut cs = name.chars();
    let Some(first) = cs.next() else { return false };
    if !(first.is_alphabetic() || first == '$' || first == '_')
        || !cs.all(|c| c.is_alphanumeric() || matches!(c, '$' | '_' | '\u{200c}' | '\u{200d}'))
    {
        return false;
    }
    const RESERVED: &[&str] = &[
        "null",
        "true",
        "false",
        "if",
        "in",
        "do",
        "var",
        "for",
        "new",
        "try",
        "this",
        "else",
        "case",
        "void",
        "with",
        "enum",
        "while",
        "break",
        "catch",
        "throw",
        "const",
        "class",
        "super",
        "return",
        "typeof",
        "delete",
        "switch",
        "export",
        "import",
        "default",
        "finally",
        "extends",
        "function",
        "continue",
        "debugger",
        "instanceof",
    ];
    !(RESERVED.contains(&name) || (es6 && name == "yield"))
}

/// `obj.method(...)` with `obj` the identifier `object`.
fn is_call_of(cx: &Ctx<'_, '_>, n: Option<NodeId>, object: &str, method: &str) -> bool {
    let ast = cx.ast();
    let Some(n) = n else { return false };
    let Node::Call { callee, .. } = *cx.node(n) else {
        return false;
    };
    member_parts(ast, cx.src(), callee)
        .is_some_and(|(o, m)| m == method && ident_name(ast, skip_parens(ast, o)) == Some(object))
}

impl FuncNameMatching {
    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId, name: &str, func: &str, prop: bool) {
        if (name == func) != self.never {
            return;
        }
        let verb = if self.never { "should not" } else { "should" };
        let kind = if prop { "property" } else { "variable" };
        let id = match (self.never, prop) {
            (false, true) => "matchProperty",
            (false, false) => "matchVariable",
            (true, true) => "notMatchProperty",
            (true, false) => "notMatchVariable",
        };
        let span = cx.ast().span(node);
        cx.report(
            span,
            format!("Function name `{func}` {verb} match {kind} name `{name}`."),
        )
        .id(id);
    }
}

impl Rule for FuncNameMatching {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::VarDeclarator,
            NodeKind::Assign,
            NodeKind::Property,
            NodeKind::PropertyDef,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        // The function expression's own name.
        let fn_name = |v: NodeId| match *cx.node(skip_parens(ast, v)) {
            Node::FunctionExpr { name: Some(n), .. } => ident_name(ast, n),
            _ => None,
        };
        match *cx.node(node) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                if let (Some(f), true) =
                    (fn_name(init), matches!(cx.node(name), Node::Ident { .. }))
                {
                    let n = ident_name(ast, name).unwrap_or("");
                    self.check(cx, node, n, f, false);
                }
            }
            Node::Assign { left, right, .. } => {
                let Some(f) = fn_name(right) else { return };
                let left = skip_parens(ast, left);
                let (name, prop) = match *cx.node(left) {
                    Node::Ident { .. } => (ident_name(ast, left).map(Into::into), false),
                    Node::Member { obj, prop, .. } => {
                        let module_exports = ident_name(ast, skip_parens(ast, obj))
                            == Some("module")
                            && ident_name(ast, prop) == Some("exports");
                        if module_exports && !self.module_exports {
                            return;
                        }
                        (
                            member_name(ast, src, left).filter(|n| !n.starts_with('#')),
                            true,
                        )
                    }
                    Node::Index { obj, index, .. } => {
                        let index = skip_parens(ast, index);
                        if !matches!(
                            cx.node(index),
                            Node::StringLit { .. }
                                | Node::NumberLit { .. }
                                | Node::NullLit {}
                                | Node::BoolLit { .. }
                                | Node::RegexLit { .. }
                                | Node::BigIntLit { .. }
                        ) {
                            return;
                        }
                        let module_exports = ident_name(ast, skip_parens(ast, obj))
                            == Some("module")
                            && matches!(*cx.node(index), Node::StringLit { value } if ast.atom(value) == "exports");
                        if module_exports && !self.module_exports {
                            return;
                        }
                        (member_name(ast, src, left), true)
                    }
                    _ => return,
                };
                if let Some(name) = name.filter(|n| !n.is_empty() && is_identifier(n, false)) {
                    let name = name.into_owned();
                    self.check(cx, node, &name, f, prop);
                }
            }
            Node::Property { key, value, .. }
            | Node::PropertyDef {
                key,
                value: Some(value),
                ..
            } => {
                let Some(f) = fn_name(value) else { return };
                match *cx.node(key) {
                    Node::Ident { .. } => {
                        let name = ident_name(ast, key).unwrap_or("");
                        let descriptor = self.property_descriptor
                            && name == "value"
                            && matches!(cx.node(node), Node::Property { .. });
                        let obj = cx.parent(node);
                        let call = obj.and_then(|o| parent_skip_parens(ast, cx.file.parents, o));
                        if descriptor
                            && (is_call_of(cx, call, "Object", "defineProperty")
                                || is_call_of(cx, call, "Reflect", "defineProperty"))
                        {
                            let Some(Node::Call { args, .. }) = call.map(|c| *cx.node(c)) else {
                                return;
                            };
                            if let Some(&p) = ast.list(args).get(1)
                                && let Node::StringLit { value } = *cx.node(skip_parens(ast, p))
                            {
                                let p = ast.atom(value).to_owned();
                                self.check(cx, node, &p, f, true);
                            }
                            return;
                        }
                        // `Object.defineProperties(o, { name: { value: function f() {} } })`.
                        let outer = call.and_then(|c| cx.parent(c));
                        let outer_call =
                            outer.and_then(|o| parent_skip_parens(ast, cx.file.parents, o));
                        if descriptor
                            && (is_call_of(cx, outer_call, "Object", "defineProperties")
                                || is_call_of(cx, outer_call, "Object", "create"))
                        {
                            let Some(pp) = call else { return };
                            let (n, computed) = match *cx.node(pp) {
                                Node::Property { key, .. } => (
                                    ident_name(ast, key).unwrap_or("undefined"),
                                    matches!(cx.node(key), Node::ComputedKey { .. }),
                                ),
                                _ => ("undefined", false),
                            };
                            if !computed {
                                self.check(cx, node, n, f, true);
                            }
                            return;
                        }
                        self.check(cx, node, name, f, true);
                    }
                    Node::StringLit { value } => {
                        let n = ast.atom(value);
                        let es6 =
                            crate::globals::normalize_ecma_version(cx.env().ecma_version) >= 2015;
                        if is_identifier(n, es6) {
                            self.check(cx, node, n, f, true);
                        }
                    }
                    Node::ComputedKey { expr } => {
                        if let Node::StringLit { value } = *cx.node(skip_parens(ast, expr)) {
                            let n = ast.atom(value);
                            let es6 = crate::globals::normalize_ecma_version(cx.env().ecma_version)
                                >= 2015;
                            if is_identifier(n, es6) {
                                self.check(cx, node, n, f, true);
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}
