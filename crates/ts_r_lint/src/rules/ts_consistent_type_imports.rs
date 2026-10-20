//! `@typescript-eslint/consistent-type-imports`: enforce `import type` for imports only
//! used as types (or forbid it with `prefer: "no-type-imports"`).

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/consistent-type-imports",
        "Enforce consistent usage of type imports",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create::<ConsistentTypeImports>,
};

#[derive(Debug)]
struct ConsistentTypeImports {
    prefer_type: bool,
    no_annotations: bool,
    inline: bool,
}

impl FromOptions for ConsistentTypeImports {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(ConsistentTypeImports {
            prefer_type: o.get("prefer").and_then(Value::as_str) != Some("no-type-imports"),
            no_annotations: bool_field(o, "disallowTypeAnnotations", true),
            inline: o.get("fixStyle").and_then(Value::as_str) == Some("inline-type-imports"),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Use {
    Type,
    Value,
    Unused,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Default,
    Namespace,
    Named,
}

struct Spec {
    node: NodeId,
    local: NodeId,
    kind: Kind,
    inline_type: bool,
    usage: Use,
}

const TYPE_OVER_VALUE: &str =
    "All imports in the declaration are only used as types. Use `import type`.";

/// How the binding introduced by `local` is used.
fn usage(cx: &Ctx<'_, '_>, local: NodeId) -> Use {
    let sem = cx.semantic();
    let ast = cx.ast();
    let Some(sym) = sem.symbol_of_decl(local) else {
        return Use::Unused;
    };
    let mut any = false;
    for &r in &sem.symbol(sym).refs {
        let r = sem.reference(r);
        any = true;
        if r.flags.intersects(RefFlags::TYPE | RefFlags::TYPEOF_TYPE) {
            continue;
        }
        if r.flags.contains(RefFlags::EXPORT) && !r.flags.contains(RefFlags::IMPLICIT) {
            let spec = cx.parent(r.node);
            let type_export = spec.is_some_and(|s| match *ast.node(s) {
                Node::ExportSpecifier { type_only, .. } => {
                    type_only
                        || cx.parent(s).is_some_and(|d| {
                            matches!(
                                ast.node(d),
                                Node::ExportNamed {
                                    type_only: true,
                                    ..
                                }
                            )
                        })
                }
                _ => false,
            });
            if type_export {
                continue;
            }
        }
        return Use::Value;
    }
    if any { Use::Type } else { Use::Unused }
}

/// `"A"`, `"A" and "B"`, `"A", "B" and "C"`.
fn name_list(names: &[&str]) -> String {
    let q: Vec<String> = names.iter().map(|n| format!("\"{n}\"")).collect();
    match q.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Position of the first byte of `ch` at or after `from`, skipping comments.
fn find_token(src: &str, from: u32, ch: u8) -> Option<u32> {
    crate::ast_util::find_char(src, from, ch)
}

/// End of whitespace (not comments) starting at `pos`.
fn skip_ws(src: &str, pos: u32) -> u32 {
    let rest = &src[pos as usize..];
    pos + (rest.len() - rest.trim_start().len()) as u32
}

impl Rule for ConsistentTypeImports {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::ImportDecl, NodeKind::ImportType]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        match *cx.node(node) {
            Node::ImportType { is_typeof, .. } => {
                if self.no_annotations {
                    // `typeof import("x").A` is reported from `import`.
                    let span = cx.span(node);
                    let start = if is_typeof {
                        skip_trivia_forward(cx.src(), span.start as usize + 6) as u32
                    } else {
                        span.start
                    };
                    cx.report(
                        Span::new(start, span.end),
                        "`import()` type annotations are forbidden.",
                    )
                    .id("noImportTypeAnnotations");
                }
            }
            Node::ImportDecl {
                type_only,
                specifiers,
                source,
                ..
            } => {
                if !self.prefer_type {
                    self.no_type_imports(cx, node, type_only, specifiers);
                } else if !type_only {
                    self.type_imports(cx, node, specifiers, source);
                }
            }
            _ => {}
        }
    }
}

impl ConsistentTypeImports {
    fn no_type_imports(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        type_only: bool,
        specifiers: ts_r_ast::NodeList,
    ) {
        let src = cx.src();
        const MSG: &str = "Use an `import` instead of an `import type`.";
        if type_only {
            let start = cx.span(node).start;
            let Some(kw) = src[start as usize + 6..].find("type") else {
                return;
            };
            let kw = start + 6 + kw as u32;
            let end = skip_ws(src, kw + 4);
            cx.report_node(node, MSG)
                .id("avoidImportType")
                .fix(Fix::delete(Span::new(kw, end)));
            return;
        }
        let ast = cx.ast();
        for &s in ast.list(specifiers) {
            if let Node::ImportSpecifier {
                type_only: true, ..
            } = *ast.node(s)
            {
                let sp = cx.span(s);
                let end = skip_ws(src, sp.start + 4);
                cx.report(sp, MSG)
                    .id("avoidImportType")
                    .fix(Fix::delete(Span::new(sp.start, end)));
            }
        }
    }

    fn type_imports(
        &self,
        cx: &mut Ctx<'_, '_>,
        node: NodeId,
        specifiers: ts_r_ast::NodeList,
        source: NodeId,
    ) {
        let ast = cx.ast();
        if ast.list(specifiers).is_empty() {
            return;
        }
        if cx.env().decorator_metadata
            && ast
                .nodes
                .iter()
                .any(|n| matches!(n, Node::Decorator { .. }))
        {
            return;
        }
        let specs: Vec<Spec> = ast
            .list(specifiers)
            .iter()
            .filter_map(|&s| {
                let (kind, local, inline_type) = match *ast.node(s) {
                    Node::ImportDefaultSpecifier { local } => (Kind::Default, local, false),
                    Node::ImportNamespaceSpecifier { local } => (Kind::Namespace, local, false),
                    Node::ImportSpecifier {
                        type_only, local, ..
                    } => (Kind::Named, local, type_only),
                    _ => return None,
                };
                let usage = if inline_type {
                    Use::Unused
                } else {
                    usage(cx, local)
                };
                Some(Spec {
                    node: s,
                    local,
                    kind,
                    inline_type,
                    usage,
                })
            })
            .collect();
        let types: Vec<&Spec> = specs.iter().filter(|s| s.usage == Use::Type).collect();
        if types.is_empty() {
            return;
        }
        let all = specs.iter().all(|s| s.usage == Use::Type || s.inline_type);
        let fix = if all {
            self.fix_all(cx, node, &specs, source)
        } else {
            self.fix_some(cx, node, &specs, source)
        };
        if all {
            cx.report_node(node, TYPE_OVER_VALUE)
                .id("typeOverValue")
                .fix_opt(fix);
        } else {
            let names: Vec<&str> = types.iter().map(|s| cx.text(s.local)).collect();
            cx.report_node(
                node,
                format!("Imports {} are only used as type.", name_list(&names)),
            )
            .id("someImportsAreOnlyTypes")
            .fix_opt(fix);
        }
    }

    /// Every specifier is type-only.
    fn fix_all(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        specs: &[Spec],
        source: NodeId,
    ) -> Option<Fix> {
        let src = cx.src();
        let start = cx.span(node).start;
        let from = cx.text(source);
        let default = specs.iter().find(|s| s.kind == Kind::Default);
        let named: Vec<&Spec> = specs.iter().filter(|s| s.kind == Kind::Named).collect();
        let ns = specs.iter().find(|s| s.kind == Kind::Namespace);
        if specs.iter().any(|s| s.inline_type) {
            return None;
        }
        match default {
            Some(d)
                if ns.is_some()
                    || find_token(src, cx.span(d.node).end, b',')
                        .is_some_and(|c| c < cx.span(source).start) =>
            {
                // `import A, { B }` / `import A, * as B`: split the default off.
                let (insert, clause_end) = if let Some(ns) = ns {
                    (
                        format!("import type {} from {from};\n", cx.text(ns.node)),
                        cx.span(ns.node).end,
                    )
                } else {
                    let open = find_token(src, cx.span(d.node).end, b'{')?;
                    let close = find_token(src, open, b'}')?;
                    let insert = if named.is_empty() {
                        String::new()
                    } else if self.inline {
                        let parts: Vec<String> = named
                            .iter()
                            .map(|s| format!("type {}", cx.text(s.node)))
                            .collect();
                        format!("import {{{}}} from {from};\n", parts.join(", "))
                    } else {
                        format!(
                            "import type {} from {from};\n",
                            &src[open as usize..close as usize + 1]
                        )
                    };
                    (insert, close + 1)
                };
                Some(Fix::insert(start, insert).and(
                    Span::new(start, clause_end),
                    format!("import type {}", cx.text(d.node)),
                ))
            }
            _ if self.inline && !named.is_empty() => {
                let mut fix: Option<Fix> = None;
                for s in &named {
                    let at = cx.span(s.node).start;
                    fix = Some(match fix {
                        None => Fix::insert(at, "type "),
                        Some(f) => f.and(Span::new(at, at), "type "),
                    });
                }
                fix
            }
            _ => Some(Fix::insert(start + 6, " type")),
        }
    }

    /// Some specifiers are type-only: move them to their own `import type`.
    fn fix_some(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        specs: &[Spec],
        source: NodeId,
    ) -> Option<Fix> {
        let src = cx.src();
        let start = cx.span(node).start;
        let from = cx.text(source);
        let default = specs.iter().find(|s| s.kind == Kind::Default);
        let ns = specs.iter().find(|s| s.kind == Kind::Namespace);
        let named: Vec<&Spec> = specs.iter().filter(|s| s.kind == Kind::Named).collect();
        let named_types: Vec<usize> = (0..named.len())
            .filter(|&i| named[i].usage == Use::Type)
            .collect();
        let mut edits: Vec<(Span, String)> = Vec::new();
        let mut prefix = String::new();
        if !named_types.is_empty() {
            if self.inline {
                for &i in &named_types {
                    let at = cx.span(named[i].node).start;
                    edits.push((Span::new(at, at), "type ".to_owned()));
                }
            } else {
                let open = find_token(
                    src,
                    default.map_or(start + 6, |d| cx.span(d.node).end),
                    b'{',
                )?;
                let close = find_token(src, cx.span(named[named.len() - 1].node).end, b'}')?;
                // Each specifier with the trivia before it (and up to `}` for the last one).
                let piece = |i: usize| -> &str {
                    let begin = if i == 0 {
                        open + 1
                    } else {
                        find_token(src, cx.span(named[i - 1].node).end, b',').map_or(0, |c| c + 1)
                    };
                    let sp = cx.span(named[i].node);
                    let after = skip_trivia_forward(src, sp.end as usize) as u32;
                    let end = if after == close { close } else { sp.end };
                    &src[begin as usize..end as usize]
                };
                let joined: Vec<&str> = named_types.iter().map(|&i| piece(i)).collect();
                let joined = joined.join(",");
                match existing_type_import(cx, node, source) {
                    Some((close_at, comma)) => {
                        let sep = if comma { "" } else { "," };
                        edits.push((Span::new(close_at, close_at), format!("{sep}{joined}")));
                    }
                    None => prefix.push_str(&format!("import type {{{joined}}} from {from};\n")),
                }
                if named_types.len() == named.len() {
                    // Drop the whole `{ ... }` (only a default remains).
                    let d = default?;
                    edits.push((Span::new(cx.span(d.node).end, close + 1), String::new()));
                } else {
                    let n = named.len();
                    let mut tail = n;
                    while tail > 0 && named[tail - 1].usage == Use::Type {
                        tail -= 1;
                    }
                    if tail < n {
                        let comma = find_token(src, cx.span(named[tail - 1].node).end, b',')?;
                        let mut end = cx.span(named[n - 1].node).end;
                        let after = skip_trivia_forward(src, end as usize) as u32;
                        if src.as_bytes()[after as usize] == b',' {
                            end = after + 1;
                        }
                        edits.push((Span::new(comma, end), String::new()));
                    }
                    for &i in named_types.iter().filter(|&&i| i < tail) {
                        let sp = cx.span(named[i].node);
                        let comma = find_token(src, sp.end, b',')?;
                        let end = skip_trivia_forward(src, comma as usize + 1) as u32;
                        edits.push((Span::new(sp.start, end), String::new()));
                    }
                }
            }
        }
        if let Some(d) = default.filter(|d| d.usage == Use::Type) {
            let comma = find_token(src, cx.span(d.node).end, b',')?;
            prefix.push_str(&format!(
                "import type {} from {from};\n",
                src[cx.span(d.node).start as usize..comma as usize].trim_end()
            ));
            edits.push((
                Span::new(cx.span(d.node).start, skip_ws(src, comma + 1)),
                String::new(),
            ));
        }
        if let Some(n) = ns.filter(|n| n.usage == Use::Type) {
            prefix.push_str(&format!("import type {} from {from};\n", cx.text(n.node)));
            let d = default?;
            edits.push((
                Span::new(cx.span(d.node).end, cx.span(n.node).end),
                String::new(),
            ));
        }
        if !prefix.is_empty() {
            edits.push((Span::new(start, start), prefix));
        }
        let mut it = edits.into_iter();
        let (sp, text) = it.next()?;
        let mut fix = Fix::replace(sp, text);
        for (sp, text) in it {
            fix = fix.and(sp, text);
        }
        Some(fix)
    }
}

/// The `}` of another `import type { ... } from <same source>` in the file, and whether its
/// last specifier has a trailing comma.
fn existing_type_import(cx: &Ctx<'_, '_>, node: NodeId, source: NodeId) -> Option<(u32, bool)> {
    let ast = cx.ast();
    let src = cx.src();
    let parent = cx.parent(node)?;
    let stmts = crate::ast_util::statement_list(ast, parent)?;
    let want = static_value_string(ast, src, source)?;
    stmts.iter().find_map(|&s| match *ast.node(s) {
        Node::ImportDecl {
            type_only: true,
            specifiers,
            source: other,
            ..
        } if s != node && static_value_string(ast, src, other).as_deref() == Some(&*want) => {
            let list = ast.list(specifiers);
            let last = *list.last()?;
            if !matches!(ast.node(last), Node::ImportSpecifier { .. }) {
                return None;
            }
            let end = cx.span(last).end;
            let comma = src.as_bytes().get(skip_trivia_forward(src, end as usize)) == Some(&b',');
            Some((find_token(src, end, b'}')?, comma))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use crate::config::{ConfigFile, ConfigKind, ResolveOptions, objects_from_eslint_json};
    use crate::engine::lint_and_fix;
    use crate::registry::Registry;
    use crate::types::{FileKey, NoTypeInfo};

    fn fix(code: &str) -> Option<String> {
        let cfg = serde_json::json!([{
            "files": ["**/*.ts"],
            "languageOptions": { "parser": "typescript-eslint/parser" },
            "rules": { "@typescript-eslint/consistent-type-imports": "error" },
        }]);
        let objs = objects_from_eslint_json(&cfg, Registry::global()).unwrap();
        let cfg = ConfigFile::new(ConfigKind::Eslint, None, "/p".into(), objs);
        let fc = cfg.file_config("a.ts", &ResolveOptions::new()).unwrap();
        lint_and_fix("/p/a.ts", code, &fc, &NoTypeInfo, FileKey(0)).output
    }

    #[test]
    fn extends_type_import_with_trailing_comma() {
        // Outputs observed from typescript-eslint.
        assert_eq!(
            fix("import type { A, } from 'x'; import { B, C } from 'x'; type T = B; C();")
                .as_deref(),
            Some("import type { A,  B} from 'x'; import { C } from 'x'; type T = B; C();")
        );
        assert_eq!(
            fix("import type { A } from 'x'; import { B, C } from 'x'; type T = B; C();")
                .as_deref(),
            Some("import type { A , B} from 'x'; import { C } from 'x'; type T = B; C();")
        );
    }
}
