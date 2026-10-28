//! `prefer-destructuring`: `const {a} = o` over `const a = o.a`, and `const [a] = arr` over
//! `const a = arr[0]`. `@typescript-eslint/prefer-destructuring` shares it (`ts`: type-aware
//! index accesses, annotated declarations).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "prefer-destructuring",
        "Require destructuring from arrays and/or objects",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<PreferDestructuring>,
};

#[derive(Debug, Default)]
pub(crate) struct PreferDestructuring {
    /// `[VariableDeclarator, AssignmentExpression]` x `[array, object]`, as ESLint reads them.
    base: [[bool; 2]; 2],
    /// The same with typescript-eslint's defaults merged in.
    merged: [[bool; 2]; 2],
    renamed: bool,
    enforce_annotated: bool,
    pub(crate) ts: bool,
}

impl FromOptions for PreferDestructuring {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o1 = obj_opt(opts, 1);
        let mut r = PreferDestructuring {
            base: [[true; 2]; 2],
            merged: [[true; 2]; 2],
            renamed: bool_field(o1, "enforceForRenamedProperties", false),
            enforce_annotated: bool_field(o1, "enforceForDeclarationWithTypeAnnotation", false),
            ts: false,
        };
        if let Some(o) = opts.first().and_then(Value::as_object) {
            let kinds = ["array", "object"];
            let flat = o.contains_key("array") || o.contains_key("object");
            for (i, node) in ["VariableDeclarator", "AssignmentExpression"]
                .iter()
                .enumerate()
            {
                for (j, kind) in kinds.iter().enumerate() {
                    if flat {
                        let v = bool_field(o, kind, false);
                        r.base[i][j] = v;
                        r.merged[i][j] = v;
                    } else {
                        let n = o.get(*node).and_then(Value::as_object);
                        r.base[i][j] = n.is_some_and(|n| bool_field(n, kind, false));
                        r.merged[i][j] = n.is_none_or(|n| bool_field(n, kind, true));
                    }
                }
            }
        }
        Ok(r)
    }
}

/// `x[1]`: a computed access with an integer literal.
fn integer_index(ast: &ts_r_ast::Ast, right: NodeId) -> bool {
    let Node::Index { index, .. } = *ast.node(right) else {
        return false;
    };
    matches!(*ast.node(skip_parens(ast, index)), Node::NumberLit { value } if value.get().fract() == 0.0 && value.get().is_finite())
}

impl PreferDestructuring {
    /// `x` is any-typed or iterable (every union member).
    fn any_or_iterable(cx: &Ctx<'_, '_>, obj: NodeId) -> bool {
        let ti = cx.types();
        let Some(t) = ti.type_at(obj) else {
            return true;
        };
        if t == Ty::UNRESOLVED || is_any(ti, t) {
            return true;
        }
        let Some(iter) = ti.well_known_symbol("iterator") else {
            return true;
        };
        parts(ti, t)
            .into_iter()
            .all(|p| is_any(ti, p) || ti.property_of_type(ti.apparent_type(p), &iter).is_some())
    }

    fn check(&self, cx: &mut Ctx<'_, '_>, node: NodeId, left: NodeId, right: NodeId, decl: bool) {
        let ast = cx.ast();
        let src = cx.src();
        let ti = if decl { 0 } else { 1 };
        let right = skip_parens(ast, right);
        // `a?.b` is ESTree's `ChainExpression`, not a member access.
        if chain_has_optional(ast, right) {
            return;
        }
        let (obj, prop_name, computed_lit) = match *ast.node(right) {
            Node::Member { obj, prop, .. } => {
                if matches!(ast.node(prop), Node::PrivateName { .. }) {
                    return;
                }
                (obj, ident_name(ast, prop), None)
            }
            Node::Index { obj, index, .. } => {
                let i = skip_parens(ast, index);
                let lit = match *ast.node(i) {
                    Node::StringLit { value } => Some(ast.atom(value)),
                    _ => None,
                };
                (obj, None, lit)
            }
            _ => return,
        };
        if matches!(ast.node(skip_parens(ast, obj)), Node::Super {}) {
            return;
        }
        let annotation = match *ast.node(node) {
            Node::VarDeclarator { type_ann, .. } => type_ann.is_some(),
            _ => false,
        };
        let mut allow_fix = true;
        if self.ts {
            if annotation && !self.enforce_annotated {
                return;
            }
            allow_fix = !annotation && matches!(ast.node(left), Node::Ident { .. });
            if integer_index(ast, right) && !Self::any_or_iterable(cx, skip_parens(ast, obj)) {
                if self.renamed && self.merged[ti][1] {
                    cx.report_node(node, "Use object destructuring.")
                        .id("preferDestructuring");
                }
                return;
            }
        }
        if integer_index(ast, right) {
            if self.base[ti][0] {
                cx.report_node(node, "Use array destructuring.")
                    .id("preferDestructuring");
            }
            return;
        }
        if !self.base[ti][1] {
            return;
        }
        let left_name = match ast.node(left) {
            Node::Ident { .. } => ident_name(ast, left),
            _ => None,
        };
        let same = left_name.is_some_and(|l| Some(l) == prop_name || Some(l) == computed_lit);
        if !self.renamed && !same {
            return;
        }
        // `const a = o.a` -> `const {a} = o`.
        let fix = if allow_fix
            && decl
            && prop_name.is_some()
            && left_name.is_some()
            && left_name == prop_name
        {
            let obj = skip_parens(ast, obj);
            let span = cx.span(node);
            let ospan = cx.span(obj);
            let inside = |s: Span| ast.comments_in(s).len();
            if inside(span) > inside(ospan) {
                None
            } else {
                let mut text = ospan.text(src).to_owned();
                if precedence(ast, obj) < 1 {
                    text = format!("({text})");
                }
                Some(Fix::replace(
                    span,
                    format!("{{{}}} = {text}", prop_name.unwrap_or_default()),
                ))
            }
        } else {
            None
        };
        cx.report_node(node, "Use object destructuring.")
            .id("preferDestructuring")
            .fix_opt(fix);
    }
}

impl Rule for PreferDestructuring {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator, NodeKind::Assign]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } => {
                if cx.parent(node).is_some_and(|p| {
                    matches!(
                        cx.node(p),
                        Node::VarDecl {
                            kind: VarKind::Using | VarKind::AwaitUsing,
                            ..
                        }
                    )
                }) {
                    return;
                }
                self.check(cx, node, name, init, true);
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => self.check(cx, node, skip_parens(cx.ast(), left), right, false),
            _ => {}
        }
    }
}
