//! `sort-imports`: import declarations sorted by member syntax and then by their first local
//! name, and the named members of each declaration sorted.

use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "sort-imports",
        "Enforce sorted `import` declarations within modules",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<SortImports>,
};

#[derive(Debug)]
struct SortImports {
    ignore_case: bool,
    ignore_declaration_sort: bool,
    ignore_member_sort: bool,
    order: Vec<String>,
    allow_separated_groups: bool,
}

impl FromOptions for SortImports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        let mut order = str_list(o, "memberSyntaxSortOrder");
        if !o.contains_key("memberSyntaxSortOrder") {
            order = ["none", "all", "multiple", "single"]
                .map(String::from)
                .to_vec();
        }
        Ok(SortImports {
            ignore_case: bool_field(o, "ignoreCase", false),
            ignore_declaration_sort: bool_field(o, "ignoreDeclarationSort", false),
            ignore_member_sort: bool_field(o, "ignoreMemberSort", false),
            order,
            allow_separated_groups: bool_field(o, "allowSeparatedGroups", false),
        })
    }
}

#[derive(Default)]
struct Collect {
    decls: Vec<NodeId>,
}

impl Visitor for Collect {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match ast.node(id) {
            Node::ImportDecl { .. } => {
                self.decls.push(id);
                Walk::SkipChildren
            }
            Node::Program { .. } | Node::Module { .. } | Node::ModuleBlock { .. } => Walk::Continue,
            _ => Walk::SkipChildren,
        }
    }
}

fn local_name(ast: &ts_r_ast::Ast, spec: NodeId) -> &str {
    let local = match *ast.node(spec) {
        Node::ImportDefaultSpecifier { local }
        | Node::ImportNamespaceSpecifier { local }
        | Node::ImportSpecifier { local, .. } => local,
        _ => return "",
    };
    ident_name(ast, local).unwrap_or_default()
}

impl SortImports {
    fn key(&self, s: &str) -> String {
        if self.ignore_case {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    }

    fn syntax(ast: &ts_r_ast::Ast, specs: &[NodeId]) -> &'static str {
        match specs {
            [] => "none",
            [s, ..] if matches!(ast.node(*s), Node::ImportNamespaceSpecifier { .. }) => "all",
            [_] => "single",
            _ => "multiple",
        }
    }

    fn check_members(&self, cx: &mut Ctx<'_, '_>, decl: NodeId, specs: &[NodeId]) {
        let ast = cx.ast();
        let named: Vec<NodeId> = specs
            .iter()
            .copied()
            .filter(|&s| matches!(ast.node(s), Node::ImportSpecifier { .. }))
            .collect();
        let keys: Vec<String> = named
            .iter()
            .map(|&s| self.key(local_name(ast, s)))
            .collect();
        let Some(i) = (1..keys.len()).find(|&i| keys[i - 1] > keys[i]) else {
            return;
        };
        let src = cx.src();
        let first = cx.span(named[0]);
        let last = cx.span(named[named.len() - 1]);
        // Any comment between the braces blocks the fix.
        let decl_span = cx.span(decl);
        let open = src[decl_span.start as usize..first.start as usize]
            .rfind('{')
            .map_or(first.start, |p| decl_span.start + p as u32);
        let close = src[last.end as usize..decl_span.end as usize]
            .find('}')
            .map_or(last.end, |p| last.end + p as u32);
        let fix = if ast.comments_in(Span::new(open, close)).is_empty() {
            let mut order: Vec<usize> = (0..named.len()).collect();
            order.sort_by(|&a, &b| keys[a].cmp(&keys[b]));
            let mut text = String::new();
            for (k, &j) in order.iter().enumerate() {
                text.push_str(cx.span(named[j]).text(src));
                if k + 1 < named.len() {
                    let gap = Span::new(cx.span(named[k]).end, cx.span(named[k + 1]).start);
                    text.push_str(gap.text(src));
                }
            }
            Some(Fix::replace(Span::new(first.start, last.end), text))
        } else {
            None
        };
        let name = local_name(ast, named[i]);
        let msg =
            format!("Member '{name}' of the import declaration should be sorted alphabetically.");
        cx.report_node(named[i], msg)
            .id("sortMembersAlphabetically")
            .fix_opt(fix);
    }
}

impl Rule for SortImports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut c = Collect::default();
        walk(ast, ast.root, &mut c);
        let lines = cx.file.lines;
        let mut previous: Option<NodeId> = None;
        for decl in c.decls {
            let Node::ImportDecl { specifiers, .. } = *ast.node(decl) else {
                continue;
            };
            let specs = ast.list(specifiers);
            if !self.ignore_declaration_sort {
                if let Some(p) = previous
                    && self.allow_separated_groups
                    && lines.line_of(cx.span(decl).start) > lines.line_of(cx.span(p).end) + 1
                {
                    previous = None;
                }
                if let Some(p) = previous {
                    let Node::ImportDecl {
                        specifiers: prev_specs,
                        ..
                    } = *ast.node(p)
                    else {
                        continue;
                    };
                    let prev_specs = ast.list(prev_specs);
                    let index = |s: &str| self.order.iter().position(|o| o == s);
                    let cur_syntax = Self::syntax(ast, specs);
                    let prev_syntax = Self::syntax(ast, prev_specs);
                    let (ci, pi) = (index(cur_syntax), index(prev_syntax));
                    if ci != pi {
                        // `indexOf` is -1 for a syntax missing from the option.
                        let rank = |i: Option<usize>| i.map_or(-1, |i| i as i64);
                        if rank(ci) < rank(pi) {
                            let name = |i: Option<usize>| {
                                i.map_or("undefined", |i| self.order[i].as_str())
                            };
                            let msg = format!(
                                "Expected '{}' syntax before '{}' syntax.",
                                name(ci),
                                name(pi)
                            );
                            cx.report_node(decl, msg).id("unexpectedSyntaxOrder");
                        }
                    } else if let (Some(&c0), Some(&p0)) = (specs.first(), prev_specs.first()) {
                        let cur = self.key(local_name(ast, c0));
                        let prev = self.key(local_name(ast, p0));
                        if !cur.is_empty() && !prev.is_empty() && cur < prev {
                            cx.report_node(decl, "Imports should be sorted alphabetically.")
                                .id("sortImportsAlphabetically");
                        }
                    }
                }
                previous = Some(decl);
            }
            if !self.ignore_member_sort {
                self.check_members(cx, decl, specs);
            }
        }
    }
}
