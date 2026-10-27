//! `no-magic-numbers`: numeric literals outside constant declarations. Shared with
//! `@typescript-eslint/no-magic-numbers`, which decides enums, literal types, indexed-access
//! types and readonly fields itself before deferring to the core rule.

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-magic-numbers",
        "Disallow magic numbers",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoMagicNumbers>,
};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Num {
    F(f64),
    /// `None` when out of range: it then matches nothing in `ignore`.
    Big(Option<i128>),
}

#[derive(Debug, Default)]
pub(crate) struct NoMagicNumbers {
    detect_objects: bool,
    enforce_const: bool,
    ignore: Vec<Num>,
    ignore_array_indexes: bool,
    ignore_default_values: bool,
    ignore_class_field_initial_values: bool,
    ignore_enums: bool,
    ignore_numeric_literal_types: bool,
    ignore_readonly_class_properties: bool,
    ignore_type_indexes: bool,
}

impl FromOptions for NoMagicNumbers {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let b = |k| bool_field(o, k, false);
        let ignore = o
            .get("ignore")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| match v {
                        Value::Number(n) => n.as_f64().map(Num::F),
                        Value::String(s) => Some(Num::Big(
                            s.strip_suffix('n').and_then(|d| d.parse::<i128>().ok()),
                        )),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(NoMagicNumbers {
            detect_objects: b("detectObjects"),
            enforce_const: b("enforceConst"),
            ignore,
            ignore_array_indexes: b("ignoreArrayIndexes"),
            ignore_default_values: b("ignoreDefaultValues"),
            ignore_class_field_initial_values: b("ignoreClassFieldInitialValues"),
            ignore_enums: b("ignoreEnums"),
            ignore_numeric_literal_types: b("ignoreNumericLiteralTypes"),
            ignore_readonly_class_properties: b("ignoreReadonlyClassProperties"),
            ignore_type_indexes: b("ignoreTypeIndexes"),
        })
    }
}

/// The ESTree parent: parentheses are not nodes there.
fn up(cx: &Ctx<'_, '_>, n: NodeId) -> Option<NodeId> {
    let mut p = cx.parent(n)?;
    for _ in 0..256 {
        if !matches!(cx.node(p), Node::Paren { .. } | Node::ParenType { .. }) {
            break;
        }
        p = cx.parent(p)?;
    }
    Some(p)
}

fn is_unary_sign(cx: &Ctx<'_, '_>, n: Option<NodeId>, minus_only: bool) -> bool {
    n.is_some_and(|n| {
        matches!(
            *cx.node(n),
            Node::Unary {
                op: UnaryOp::Minus,
                ..
            }
        ) || (!minus_only
            && matches!(
                *cx.node(n),
                Node::Unary {
                    op: UnaryOp::Plus,
                    ..
                }
            ))
    })
}

/// `n` (a literal or signed literal) is a literal type (`TSLiteralType` in ESTree).
fn in_type(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let Some(p) = up(cx, n) else { return false };
    crate::cfg::is_type_kind(cx.ast().kind(p))
        || match *cx.node(p) {
            Node::TypeAlias { ty, .. } | Node::As { ty, .. } | Node::Satisfies { ty, .. } => {
                ty == n
            }
            Node::TypeAssertion { ty, .. } => ty == n,
            Node::VarDeclarator { type_ann, .. }
            | Node::Param { type_ann, .. }
            | Node::PropertyDef { type_ann, .. } => type_ann == Some(n),
            Node::FunctionDecl { return_type, .. }
            | Node::FunctionExpr { return_type, .. }
            | Node::Arrow { return_type, .. } => return_type == Some(n),
            _ => false,
        }
}

/// Climbs from the literal type `n` through unions (and intersections) and returns the node
/// above them.
fn above_unions(cx: &Ctx<'_, '_>, n: NodeId, intersections: bool) -> Option<NodeId> {
    let mut a = n;
    for _ in 0..256 {
        let p = up(cx, a)?;
        match cx.node(p) {
            Node::UnionType { .. } => a = p,
            Node::IntersectionType { .. } if intersections => a = p,
            _ => return Some(p),
        }
    }
    None
}

fn is_readonly_field_value(cx: &Ctx<'_, '_>, parent: Option<NodeId>, n: NodeId) -> bool {
    parent.is_some_and(|p| {
        matches!(*cx.node(p), Node::PropertyDef { modifiers, value: Some(v), .. }
            if v == n && modifiers.contains(Modifiers::READONLY))
    })
}

impl NoMagicNumbers {
    fn is_numeric_literal_type(&self, cx: &Ctx<'_, '_>, full: NodeId) -> bool {
        in_type(cx, full)
            && above_unions(cx, full, false)
                .is_some_and(|a| matches!(cx.node(a), Node::TypeAlias { .. }))
    }
    fn is_type_index(&self, cx: &Ctx<'_, '_>, full: NodeId) -> bool {
        in_type(cx, full)
            && above_unions(cx, full, true)
                .is_some_and(|a| matches!(cx.node(a), Node::IndexedAccessType { .. }))
    }
}

impl Rule for NoMagicNumbers {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::NumberLit, NodeKind::BigIntLit]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let value = match *cx.node(node) {
            Node::NumberLit { value } => Num::F(value.get()),
            Node::BigIntLit { value } => {
                let t = ast.atom(value).replace('_', "");
                let (radix, digits) = match t.get(..2) {
                    Some("0x" | "0X") => (16, &t[2..]),
                    Some("0o" | "0O") => (8, &t[2..]),
                    Some("0b" | "0B") => (2, &t[2..]),
                    _ => (10, &t[..]),
                };
                Num::Big(i128::from_str_radix(digits, radix).ok())
            }
            _ => return,
        };
        let neg = |v: Num| match v {
            Num::F(f) => Num::F(-f),
            Num::Big(b) => Num::Big(b.and_then(i128::checked_neg)),
        };
        let lit_parent = up(cx, node);
        let sp = ast.span(node);
        let raw_lit = src.get(sp.start as usize..sp.end as usize).unwrap_or("");
        let ts = cx.rule_name().starts_with("@typescript-eslint/");
        let report = |cx: &mut Ctx<'_, '_>, n: NodeId, raw: String| {
            let span = cx.ast().span(n);
            cx.report(span, format!("No magic number: {raw}."))
                .id("noMagic");
        };
        if ts {
            let minus = is_unary_sign(cx, lit_parent, true);
            let v = if minus { neg(value) } else { value };
            let signed = if is_unary_sign(cx, lit_parent, false) {
                lit_parent.and_then(|p| up(cx, p))
            } else {
                lit_parent
            };
            let type_node = lit_parent.filter(|_| minus).unwrap_or(node);
            // The literal with its sign, if any.
            let signed_lit = lit_parent.filter(|_| signed != lit_parent).unwrap_or(node);
            let allowed = if self.ignore.contains(&v) {
                Some(true)
            } else if signed.is_some_and(|p| matches!(cx.node(p), Node::EnumMember { .. })) {
                Some(self.ignore_enums)
            } else if in_type(cx, type_node)
                && up(cx, type_node).is_some_and(|p| {
                    matches!(cx.node(p), Node::TypeAlias { .. })
                        || matches!(cx.node(p), Node::UnionType { .. })
                            && up(cx, p)
                                .is_some_and(|g| matches!(cx.node(g), Node::TypeAlias { .. }))
                })
            {
                Some(self.ignore_numeric_literal_types)
            } else if self.is_type_index(cx, signed_lit) {
                Some(self.ignore_type_indexes)
            } else if is_readonly_field_value(cx, signed, signed_lit) {
                Some(self.ignore_readonly_class_properties)
            } else {
                None
            };
            match allowed {
                Some(true) => return,
                Some(false) => {
                    let (n, raw) = match lit_parent {
                        Some(p) if minus => (p, format!("-{raw_lit}")),
                        _ => (node, raw_lit.to_string()),
                    };
                    return report(cx, n, raw);
                }
                None => {}
            }
        }
        let (full, value, raw) = match lit_parent {
            Some(p) if is_unary_sign(cx, Some(p), false) => {
                let minus = is_unary_sign(cx, Some(p), true);
                let sign = if minus { "-" } else { "+" };
                (
                    p,
                    if minus { neg(value) } else { value },
                    format!("{sign}{raw_lit}"),
                )
            }
            _ => (node, value, raw_lit.to_string()),
        };
        let parent = if full == node {
            lit_parent
        } else {
            up(cx, full)
        };
        let Some(parent) = parent else { return };
        let typed = in_type(cx, full);
        let pnode = *cx.node(parent);
        let is_default = !typed
            && match pnode {
                Node::AssignPattern { right, .. } => right == full,
                Node::Param { init, .. }
                | Node::ShorthandProp { init, .. }
                | Node::BindingProperty { init, .. } => init == Some(full),
                // `[a = 1] = x`: a default in a destructuring assignment.
                Node::Assign {
                    op: AssignOp::Assign,
                    right,
                    ..
                } => right == full && is_assignment_target(ast, cx.file.parents, parent),
                _ => false,
            };
        let field_value =
            !typed && matches!(pnode, Node::PropertyDef { value: Some(v), .. } if v == full);
        let is_radix = match pnode {
            Node::Call { callee, args, .. } if ast.list(args).get(1) == Some(&full) => {
                let callee = skip_parens(ast, callee);
                match *cx.node(callee) {
                    Node::Ident { .. } => ident_name(ast, callee) == Some("parseInt"),
                    Node::Member { obj, prop, .. } => {
                        let obj = skip_parens(ast, obj);
                        matches!(cx.node(obj), Node::Ident { .. })
                            && ident_name(ast, obj) == Some("Number")
                            && ident_name(ast, prop) == Some("parseInt")
                    }
                    Node::Index { obj, index, .. } => {
                        let obj = skip_parens(ast, obj);
                        matches!(cx.node(obj), Node::Ident { .. })
                            && ident_name(ast, obj) == Some("Number")
                            && static_value_string(ast, src, index).as_deref() == Some("parseInt")
                    }
                    _ => false,
                }
            }
            _ => false,
        };
        let is_index = matches!(pnode, Node::Index { index, .. } if index == full)
            && match value {
                Num::F(f) => f.fract() == 0.0 && (0.0..4_294_967_295.0).contains(&f),
                Num::Big(b) => b.is_some_and(|b| (0..4_294_967_295).contains(&b)),
            };
        let enum_member = !typed && matches!(pnode, Node::EnumMember { .. });
        let jsx = !typed
            && matches!(
                pnode,
                Node::JsxExprContainer { .. }
                    | Node::JsxSpreadAttr { .. }
                    | Node::JsxSpreadChild { .. }
            );
        if self.ignore.contains(&value)
            || (self.ignore_default_values && is_default)
            || (self.ignore_class_field_initial_values && field_value)
            || (self.ignore_enums && enum_member)
            || (self.ignore_numeric_literal_types && self.is_numeric_literal_type(cx, full))
            || (self.ignore_type_indexes && self.is_type_index(cx, full))
            || (self.ignore_readonly_class_properties
                && !typed
                && is_readonly_field_value(cx, Some(parent), full))
            || is_radix
            || jsx
            || (self.ignore_array_indexes && is_index)
        {
            return;
        }
        if !typed && let Node::VarDeclarator { .. } = pnode {
            if self.enforce_const
                && !cx.parent(parent).is_some_and(|d| {
                    matches!(
                        *cx.node(d),
                        Node::VarDecl {
                            kind: VarKind::Const,
                            ..
                        }
                    )
                })
            {
                let span = ast.span(full);
                cx.report(span, "Number constants declarations must use 'const'.")
                    .id("useConst");
            }
            return;
        }
        let ok_parent = !typed
            && !self.detect_objects
            && match pnode {
                Node::Property { .. } | Node::ObjectLit { .. } => true,
                Node::Assign { left, .. } => !matches!(cx.node(left), Node::Ident { .. }),
                _ => false,
            };
        if !ok_parent {
            report(cx, full, raw);
        }
    }
}
