//! `no-param-reassign`: no assignment to a function parameter (with `props`, nor to its
//! properties).

use super::prelude::*;
use crate::regex;
use crate::semantic::{DeclKind, RefFlags};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-param-reassign",
        "Disallow reassigning function parameters",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<NoParamReassign>,
};

#[derive(Debug, Default)]
struct NoParamReassign {
    props: bool,
    ignore: Vec<String>,
    ignore_regex: Vec<String>,
}

impl FromOptions for NoParamReassign {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let strings = |k: &str| -> Vec<String> {
            o.get(k)
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        Ok(NoParamReassign {
            props: bool_field(o, "props", false),
            ignore: strings("ignorePropertyModificationsFor"),
            ignore_regex: strings("ignorePropertyModificationsForRegex"),
        })
    }
}

/// The reference is the object of a member that is assigned, updated, deleted or the target
/// of a `for-in/of` (`a.b = 1`, `a.b++`, `delete a.b`, `for (a.b of c)`).
fn is_modifying_prop(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let mut node = ident;
    let mut parent = cx.parent(node);
    while let Some(p) = parent {
        let pn = cx.node(p);
        let for_in_of = matches!(pn, Node::ForIn { .. } | Node::ForOf { .. });
        if (pn.is_statement() && !for_in_of)
            || matches!(
                pn,
                Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::Program { .. }
            )
        {
            return false;
        }
        match *pn {
            Node::Assign { left, .. } => return left == node,
            Node::Update { .. } => return true,
            Node::Unary {
                op: UnaryOp::Delete,
                ..
            } => return true,
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => return left == node,
            Node::Call { callee, .. } if callee != node => return false,
            Node::Member { prop, .. } | Node::Index { index: prop, .. } if prop == node => {
                return false;
            }
            Node::Property { key, .. } | Node::BindingProperty { key, .. } if key == node => {
                return false;
            }
            Node::Conditional { test, .. } if test == node => return false,
            _ => {}
        }
        node = p;
        parent = cx.parent(p);
    }
    false
}

impl Rule for NoParamReassign {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let sem = cx.semantic();
        let ast = cx.ast();
        let mut hits: Vec<(NodeId, bool)> = Vec::new();
        for s in sem.symbols() {
            if s.decls.first().is_none_or(|d| d.kind != DeclKind::Param) {
                continue;
            }
            let name = ast.atom(s.name);
            for &r in &s.refs {
                let r = sem.reference(r);
                if r.flags.contains(RefFlags::INIT) || r.flags.contains(RefFlags::IMPLICIT) {
                    continue;
                }
                if r.is_write() {
                    hits.push((r.node, false));
                } else if self.props
                    && is_modifying_prop(cx, r.node)
                    && !self.ignore.iter().any(|i| i == name)
                    && !self
                        .ignore_regex
                        .iter()
                        .any(|p| regex::compile(p).is_some_and(|re| re.is_match_flags(name, "u")))
                {
                    hits.push((r.node, true));
                }
            }
        }
        for (n, prop) in hits {
            let name = ident_name(ast, n).unwrap_or_default();
            if prop {
                cx.report_node(
                    n,
                    format!("Assignment to property of function parameter '{name}'."),
                )
                .id("assignmentToFunctionParamProp");
            } else {
                cx.report_node(n, format!("Assignment to function parameter '{name}'."))
                    .id("assignmentToFunctionParam");
            }
        }
    }
}
