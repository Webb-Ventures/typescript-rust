//! `unicorn/no-unreadable-array-destructuring`: no `[,, x]` destructuring or member targets in
//! array patterns.
//!
//! An array pattern (a binding `ArrayPattern`, or an array literal that is an assignment
//! target) with more consecutive holes than `maximumIgnoredElements` (default 1) is
//! reported; its span includes a type annotation written on it. When it is a declarator's
//! name with an initializer and holds exactly one element that is not a default, has no
//! type annotation, the initializer isn't a known non-indexed collection (nor a known
//! non-array for a rest element), and no comment outside the element would be lost, the
//! fix writes `const x = init[i]` (or `init.slice(i)` for `...x`), parenthesizing the
//! initializer as a member object when needed.
//!
//! A member expression whose nearest enclosing pattern (through TS wrappers, defaults and
//! rest elements; an object pattern stops the search) is an array pattern is reported too.

use super::prelude::*;
use super::unicorn_types::{is_known_non_array, is_known_non_indexed_collection};
use super::unicorn_util::{space_around_keyword, with_edits};
use super::unicorn_util_h5::{is_parenthesized, member_object_needs_parens, would_remove_comments};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unreadable-array-destructuring",
        "Disallow unreadable array destructuring.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoUnreadableArrayDestructuring>,
};

#[derive(Debug)]
struct NoUnreadableArrayDestructuring {
    maximum: u64,
}

impl FromOptions for NoUnreadableArrayDestructuring {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let maximum = obj_opt(opts, 0)
            .get("maximumIgnoredElements")
            .and_then(Value::as_u64)
            .unwrap_or(1);
        Ok(NoUnreadableArrayDestructuring { maximum })
    }
}

/// True when the array literal `id` is a destructuring target.
fn is_array_pattern(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    match cx.node(id) {
        Node::ArrayPattern { .. } => true,
        Node::ArrayLit { .. } => is_assignment_target(cx.ast(), cx.file.parents, id),
        _ => false,
    }
}

impl Rule for NoUnreadableArrayDestructuring {
    fn kinds(&self) -> &'static [NodeKind] {
        &[
            NodeKind::ArrayPattern,
            NodeKind::ArrayLit,
            NodeKind::Member,
            NodeKind::Index,
        ]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::ArrayPattern { elements } | Node::ArrayLit { elements } => {
                if is_array_pattern(cx, node) {
                    self.check_pattern(cx, node, elements);
                }
            }
            Node::Member { .. } | Node::Index { .. } => {
                if parent_pattern_is_array(cx, node) {
                    cx.report_node(
                        node,
                        "Do not assign destructured values to object properties.",
                    )
                    .id("property-assignment");
                }
            }
            _ => {}
        }
    }
}

impl NoUnreadableArrayDestructuring {
    fn check_pattern(&self, cx: &mut Ctx<'_, '_>, node: NodeId, elements: ts_r_ast::NodeList) {
        let ast = cx.ast();
        let elems = ast.list(elements);
        let mut run = 0u64;
        let mut max = 0u64;
        for &e in elems {
            if matches!(ast.node(e), Node::Hole {}) {
                run += 1;
                max = max.max(run);
            } else {
                run = 0;
            }
        }
        if max <= self.maximum {
            return;
        }
        let parent = cx.parent(node);
        let type_ann = parent.and_then(|p| match *cx.node(p) {
            Node::VarDeclarator { name, type_ann, .. }
            | Node::Param {
                pattern: name,
                type_ann,
                ..
            } if name == node => type_ann,
            _ => None,
        });
        let span = match type_ann {
            Some(t) => Span::new(cx.span(node).start, cx.span(t).end),
            None => cx.span(node),
        };
        let fix = self.fix(cx, node, elems, parent, type_ann.is_some());
        cx.report(
            span,
            "Array destructuring may not contain consecutive ignored values.",
        )
        .id("ignored-elements")
        .fix_opt(fix);
    }

    fn fix(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        elems: &[NodeId],
        parent: Option<NodeId>,
        typed: bool,
    ) -> Option<Fix> {
        let ast = cx.ast();
        let decl = parent?;
        let Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } = *cx.node(decl)
        else {
            return None;
        };
        if name != node || typed {
            return None;
        }
        let mut present = elems
            .iter()
            .enumerate()
            .filter(|(_, e)| !matches!(ast.node(**e), Node::Hole {}));
        let (index, &element) = present.next()?;
        if present.next().is_some() {
            return None;
        }
        let (variable, slice) = match *ast.node(element) {
            Node::AssignPattern { .. } => return None,
            Node::Rest { arg } => (arg, true),
            _ => (element, false),
        };
        if is_known_non_indexed_collection(cx, init)
            || (slice && is_known_non_array(cx, init))
            || would_remove_comments(cx, cx.span(node), &[cx.span(variable)])
        {
            return None;
        }
        let code = if slice {
            format!(".slice({index})")
        } else {
            format!("[{index}]")
        };
        let end = cx.span(decl).end;
        let mut fix = Fix::replace(cx.span(node), cx.text(variable));
        let array = skip_parens(ast, init);
        if !is_parenthesized(cx, array) && member_object_needs_parens(cx, array, array) {
            fix = fix
                .and(Span::at(cx.span(array).start), "(")
                .and(Span::at(end), format!("){code}"));
        } else {
            fix = fix.and(Span::at(end), code);
        }
        Some(with_edits(fix, space_around_keyword(cx, node)))
    }
}

/// unicorn's `getParentPattern(node)` is an array pattern.
fn parent_pattern_is_array(cx: &Ctx<'_, '_>, node: NodeId) -> bool {
    let mut cur = node;
    for _ in 0..10_000 {
        let Some(parent) = cx.parent(cur) else {
            return false;
        };
        match *cx.node(parent) {
            Node::Paren { .. } => {}
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. }
                if expr == cur => {}
            Node::Assign { left, .. } | Node::AssignPattern { left, .. } if left == cur => {}
            Node::Spread { arg } | Node::Rest { arg } if arg == cur => {}
            Node::ArrayLit { .. } | Node::ArrayPattern { .. } => {
                return is_array_pattern(cx, parent);
            }
            _ => return false,
        }
        cur = parent;
    }
    false
}
