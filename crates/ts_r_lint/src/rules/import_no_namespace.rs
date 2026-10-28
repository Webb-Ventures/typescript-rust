//! `import/no-namespace`: `import * as ns` specifiers, except sources matching an `ignore`
//! glob (a glob without `/` matches the last path segment). When every reference is a
//! `ns.x` or `ns["x"]` member access, the fix turns the namespace into named imports and
//! the accesses into the names, renaming to `ns_x` (then `ns_x_1`, ...) when `x` is already
//! taken in the scope of an access or the one around it.

use super::prelude::*;
use crate::config::glob::Glob;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "import/no-namespace",
        "Forbid namespace (a.k.a. \"wildcard\" `*`) imports.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<NoNamespace>,
};

#[derive(Debug)]
struct NoNamespace {
    ignore: Vec<Glob>,
}

impl FromOptions for NoNamespace {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoNamespace {
            ignore: str_list(obj_opt(opts, 0), "ignore")
                .iter()
                .map(|g| Glob::new(g))
                .collect(),
        })
    }
}

fn ignored(globs: &[Glob], value: &str) -> bool {
    globs.iter().any(|g| {
        if g.source.contains('/') {
            g.matches(value)
        } else {
            g.matches(value.rsplit('/').next().unwrap_or(value))
        }
    })
}

/// The property name of a member access whose object is `ident`.
fn member_name(ast: &Ast, src: &str, member: NodeId, ident: NodeId) -> Option<String> {
    match *ast.node(member) {
        Node::Member { obj, prop, .. } if skip_parens(ast, obj) == ident => {
            ast.name_of(prop).map(str::to_owned)
        }
        Node::Index { obj, index, .. } if skip_parens(ast, obj) == ident => {
            match *ast.node(index) {
                Node::StringLit { value } => Some(ast.atom(value).to_owned()),
                Node::NumberLit { .. } | Node::BoolLit { .. } | Node::NullLit {} => {
                    Some(cx_text(ast, src, index))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn cx_text(ast: &Ast, src: &str, id: NodeId) -> String {
    ast.span(id).text(src).to_owned()
}

impl Rule for NoNamespace {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportNamespaceSpecifier]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let src = cx.src();
        let Node::ImportNamespaceSpecifier { local } = *cx.node(node) else {
            return;
        };
        if let Some(decl) = cx.parent(node)
            && let Node::ImportDecl { source, .. } = *ast.node(decl)
            && let Some(v) = super::import_util::string_literal(ast, source)
            && ignored(&self.ignore, v)
        {
            return;
        }
        let span = cx.span(node);
        let sem = cx.semantic();
        let refs: Vec<NodeId> = sem
            .symbol_of_decl(local)
            .map(|s| {
                sem.symbol(s)
                    .refs
                    .iter()
                    .map(|&r| sem.reference(r).node)
                    .collect()
            })
            .unwrap_or_default();
        let mut uses = Vec::new();
        for &r in &refs {
            let member = parent_skip_parens(ast, cx.file.parents, r)
                .and_then(|m| member_name(ast, src, m, r).map(|n| (m, n)));
            match member {
                Some(m) => uses.push(m),
                None => {
                    uses.clear();
                    break;
                }
            }
        }
        if uses.is_empty() {
            cx.report(span, "Unexpected namespace import.");
            return;
        }
        let ns = ast.name_of(local).unwrap_or("");
        // Names taken around each access, per imported name (in first-use order).
        let mut names: Vec<(String, std::collections::BTreeSet<String>)> = Vec::new();
        for (member, name) in &uses {
            let scope = sem.enclosing_scope(*member);
            let mut taken = std::collections::BTreeSet::new();
            let sc = sem.scope(scope);
            let scopes = std::iter::once(scope).chain(sc.parent);
            for s in scopes {
                for &sym in &sem.scope(s).symbols {
                    taken.insert(sem.name(sym).to_owned());
                }
            }
            match names.iter_mut().find(|(n, _)| n == name) {
                Some((_, t)) => t.extend(taken),
                None => names.push((name.clone(), taken)),
            }
        }
        let locals: Vec<(String, String)> = names
            .iter()
            .map(|(name, taken)| {
                let local = if !taken.contains(name) {
                    name.clone()
                } else if !taken.contains(&format!("{ns}_{name}")) {
                    format!("{ns}_{name}")
                } else {
                    (1..)
                        .map(|i| format!("{ns}_{name}_{i}"))
                        .find(|c| !taken.contains(c))
                        .unwrap_or_default()
                };
                (name.clone(), local)
            })
            .collect();
        let specs: Vec<String> = locals
            .iter()
            .map(|(n, l)| {
                if n == l {
                    n.clone()
                } else {
                    format!("{n} as {l}")
                }
            })
            .collect();
        let mut fix = Fix::replace(span, format!("{{ {} }}", specs.join(", ")));
        for (member, name) in &uses {
            let local = locals
                .iter()
                .find(|(n, _)| n == name)
                .map_or("", |(_, l)| l.as_str());
            fix = fix.and(cx.span(*member), local.to_owned());
        }
        cx.report(span, "Unexpected namespace import.").fix(fix);
    }
}
