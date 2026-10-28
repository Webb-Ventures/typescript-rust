//! `unicorn/consistent-class-member-order`: class members follow a group order (default:
//! static fields, static blocks, static methods, private fields, public fields, the
//! constructor, private methods, public methods; `order` replaces it). Fields include
//! `accessor` and abstract fields; methods include getters, setters and abstract methods;
//! `private` means a `#name` or the `private` keyword. Members with no group (index
//! signatures) are skipped. Only the first member whose group comes before the highest group
//! seen so far is reported, naming both members (`static method \`x\``, `constructor`).
//!
//! The suggestion rewrites the whole body in group order (stable within a group), members
//! separated by a blank line, each with its leading indentation. It is offered only when the
//! body has no comments, every member starts its line, no member is grouped-less, computed
//! or decorated, and the static fields and static blocks keep their relative order (they run
//! in declaration order).

use super::prelude::*;
use super::unicorn_ast::{linebreak, would_remove_comments};
use super::unicorn_static::static_value;
use ts_r_span::Span;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-class-member-order",
        "Enforce consistent class member order.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create::<ConsistentClassMemberOrder>,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Group {
    StaticField,
    StaticBlock,
    StaticMethod,
    PrivateField,
    PublicField,
    Constructor,
    PrivateMethod,
    PublicMethod,
}

const GROUPS: [(Group, &str); 8] = [
    (Group::StaticField, "static-field"),
    (Group::StaticBlock, "static-block"),
    (Group::StaticMethod, "static-method"),
    (Group::PrivateField, "private-field"),
    (Group::PublicField, "public-field"),
    (Group::Constructor, "constructor"),
    (Group::PrivateMethod, "private-method"),
    (Group::PublicMethod, "public-method"),
];

impl Group {
    fn label(self) -> &'static str {
        match self {
            Group::StaticField => "static field",
            Group::StaticBlock => "static block",
            Group::StaticMethod => "static method",
            Group::PrivateField => "private field",
            Group::PublicField => "public field",
            Group::Constructor => "constructor",
            Group::PrivateMethod => "private method",
            Group::PublicMethod => "public method",
        }
    }
}

#[derive(Debug)]
struct ConsistentClassMemberOrder {
    /// The rank of each group, indexed like [`GROUPS`].
    rank: [usize; 8],
}

impl FromOptions for ConsistentClassMemberOrder {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut rank = [0, 1, 2, 3, 4, 5, 6, 7];
        let order = str_list(obj_opt(opts, 0), "order");
        if order.len() == GROUPS.len() {
            for (i, (_, name)) in GROUPS.iter().enumerate() {
                if let Some(at) = order.iter().position(|o| o == name) {
                    rank[i] = at;
                }
            }
        }
        Ok(ConsistentClassMemberOrder { rank })
    }
}

impl ConsistentClassMemberOrder {
    fn rank(&self, g: Group) -> usize {
        let i = GROUPS.iter().position(|&(x, _)| x == g).unwrap_or(0);
        self.rank[i]
    }
}

fn group(cx: &Ctx<'_, '_>, member: NodeId) -> Option<Group> {
    let ast = cx.ast();
    match *cx.node(member) {
        Node::StaticBlock { .. } => Some(Group::StaticBlock),
        Node::MethodDef {
            kind: MethodKind::Constructor,
            ..
        } => Some(Group::Constructor),
        Node::PropertyDef { modifiers, key, .. } => {
            Some(if modifiers.contains(Modifiers::STATIC) {
                Group::StaticField
            } else if is_private(ast, modifiers, key) {
                Group::PrivateField
            } else {
                Group::PublicField
            })
        }
        Node::MethodDef { modifiers, key, .. } => Some(if modifiers.contains(Modifiers::STATIC) {
            Group::StaticMethod
        } else if is_private(ast, modifiers, key) {
            Group::PrivateMethod
        } else {
            Group::PublicMethod
        }),
        _ => None,
    }
}

fn is_private(ast: &ts_r_ast::Ast, modifiers: Modifiers, key: NodeId) -> bool {
    modifiers.contains(Modifiers::PRIVATE) || matches!(ast.node(key), Node::PrivateName { .. })
}

fn key_of(cx: &Ctx<'_, '_>, member: NodeId) -> Option<NodeId> {
    match *cx.node(member) {
        Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => Some(key),
        _ => None,
    }
}

fn name(cx: &Ctx<'_, '_>, member: NodeId, group: Group) -> Option<String> {
    if matches!(group, Group::StaticBlock | Group::Constructor) {
        return None;
    }
    let ast = cx.ast();
    let key = key_of(cx, member)?;
    match *ast.node(key) {
        Node::PrivateName { name } => Some(format!("#{}", ast.atom(name))),
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::ComputedKey { expr } => static_value(cx, expr)?.to_js_string(),
        _ => static_value(cx, key)?.to_js_string(),
    }
}

fn describe(cx: &Ctx<'_, '_>, member: NodeId, group: Group) -> String {
    match name(cx, member, group) {
        Some(n) => format!("{} `{n}`", group.label()),
        None => group.label().to_owned(),
    }
}

impl Rule for ConsistentClassMemberOrder {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ClassDecl, NodeKind::ClassExpr]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let members = match *cx.node(node) {
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => ast.list(members),
            _ => return,
        };
        let mut highest: Option<(usize, NodeId, Group)> = None;
        for &m in members {
            let Some(g) = group(cx, m) else {
                continue;
            };
            let r = self.rank(g);
            if let Some((hr, hm, hg)) = highest
                && r < hr
            {
                let message = format!(
                    "Expected {} to come before {}.",
                    describe(cx, m, g),
                    describe(cx, hm, hg)
                );
                let fix = self.suggestion(cx, node, members);
                let report = cx
                    .report_node(m, message)
                    .id("consistent-class-member-order");
                if let Some(fix) = fix {
                    report.suggest(
                        "consistent-class-member-order-suggestion",
                        "Reorder class members by group.",
                        fix,
                    );
                }
                return;
            }
            highest = Some((r, m, g));
        }
    }
}

impl ConsistentClassMemberOrder {
    fn suggestion(&self, cx: &Ctx<'_, '_>, class: NodeId, members: &[NodeId]) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let (&first, &last) = (members.first()?, members.last()?);
        let class_end = cx.span(class).end;
        // The body runs from the `{` before the first member to the class's end.
        let open = src[..cx.span(first).start as usize].rfind('{')? as u32;
        if would_remove_comments(ast, Span::new(open, class_end), &[]) {
            return None;
        }
        let lines = &cx.file.lines;
        let line_start = |m: NodeId| {
            let start = cx.span(m).start;
            lines.line_start(lines.line_of(start)).unwrap_or(start)
        };
        let lead = |m: NodeId| &src[line_start(m) as usize..cx.span(m).start as usize];
        if members
            .iter()
            .any(|&m| !lead(m).chars().all(char::is_whitespace))
        {
            return None;
        }
        let mut items = Vec::with_capacity(members.len());
        for (i, &m) in members.iter().enumerate() {
            let g = group(cx, m)?;
            let decorated = match *ast.node(m) {
                Node::PropertyDef { decorators, .. } | Node::MethodDef { decorators, .. } => {
                    !ast.list(decorators).is_empty()
                }
                _ => false,
            };
            let computed =
                key_of(cx, m).is_some_and(|k| matches!(ast.node(k), Node::ComputedKey { .. }));
            if decorated || computed {
                return None;
            }
            items.push((self.rank(g), i, m, g));
        }
        let mut sorted: Vec<_> = items.clone();
        sorted.sort_by_key(|&(r, i, _, _)| (r, i));
        let evaluated = |list: &[(usize, usize, NodeId, Group)]| -> Vec<usize> {
            list.iter()
                .filter(|(_, _, _, g)| matches!(g, Group::StaticField | Group::StaticBlock))
                .map(|&(_, i, _, _)| i)
                .collect()
        };
        if evaluated(&items) != evaluated(&sorted) {
            return None;
        }
        let text_start = |m: NodeId| {
            if lead(m).is_empty() {
                cx.span(m).start
            } else {
                line_start(m)
            }
        };
        let lb = linebreak(src);
        let sep = format!("{lb}{lb}");
        let text: Vec<&str> = sorted
            .iter()
            .map(|&(_, _, m, _)| &src[text_start(m) as usize..cx.span(m).end as usize])
            .collect();
        Some(Fix::replace(
            Span::new(text_start(first), cx.span(last).end),
            text.join(&sep),
        ))
    }
}
