//! `unicorn/prefer-json-import`: in a module, a top-level (or exported) declaration
//! initialised with `JSON.parse(<read>)` of a `.json` file should be a JSON import.
//!
//! `<read>` is `readFileSync(path[, encoding])` from `fs`, or an awaited `readFile` from
//! `fs/promises` (or `fs.promises.readFile`), reached through an import binding (default,
//! namespace or named; `node:` prefixes allowed; type-only imports don't count) and a chain
//! of plain members. The encoding is absent, or statically `null`, `undefined` or
//! `utf8`/`utf-8` (any case), possibly as the only `encoding` property of an object. The path
//! is a static string ending in `.json`, or `new URL(<that>, import.meta.url)`; TS wrappers
//! are looked through everywhere.
//!
//! The suggestion (rewriting the declaration as `import x from '...' with {type: 'json'};`)
//! is offered only for a module-relative `./` or `../` URL path without `#`, `?` or `\`, in a
//! non-exported single-declarator `const` binding a plain name without a type annotation,
//! with no TS wrapper around the call and no comment inside the declaration.

use super::prelude::*;
use super::unicorn_static::{Static, static_value_control_flow};
use super::unicorn_util::{escape_string, skip_wrappers};
use crate::env::SourceType;
use crate::semantic::Semantic;
use ts_r_ast::{Ast, MetaPropKind};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-json-import",
        "Prefer JSON imports over reading and parsing JSON files.",
        NO_PRESET,
        Category::Style,
    )
    .suggestions(),
    create: create_default::<PreferJsonImport>,
};

#[derive(Debug, Default)]
struct PreferJsonImport;

/// A plain call (no `?.`, no spread among the first `max` arguments) with `min..=max`
/// arguments.
fn plain_call(ast: &Ast, id: NodeId, min: usize, max: usize) -> Option<(NodeId, &[NodeId])> {
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return None;
    };
    let args = ast.list(args);
    if args.len() < min || args.len() > max {
        return None;
    }
    if args
        .iter()
        .take(max)
        .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
    {
        return None;
    }
    Some((callee, args))
}

/// `read` is a file read of the right kind (see the module docs).
fn is_file_read(cx: &Ctx<'_, '_>, sem: &dyn Semantic, callee: NodeId, awaited: bool) -> bool {
    let ast = cx.ast();
    let mut props: Vec<&str> = Vec::new();
    let mut cur = skip_parens(ast, callee);
    for _ in 0..1000 {
        match *ast.node(cur) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } => {
                let Some(name) = ident_name(ast, prop) else {
                    return false;
                };
                if matches!(ast.node(prop), Node::PrivateName { .. }) {
                    return false;
                }
                props.push(name);
                cur = skip_parens(ast, obj);
            }
            _ => break,
        }
    }
    let Node::Ident { .. } = *ast.node(cur) else {
        return false;
    };
    let Some(sym) = sem
        .reference_at(cur)
        .and_then(|r| sem.reference(r).symbol())
    else {
        return false;
    };
    let Some(def) = sem.symbol(sym).decls.first().copied() else {
        return false;
    };
    if !matches!(
        def.kind,
        DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed
    ) {
        return false;
    }
    let Node::ImportDecl {
        type_only, source, ..
    } = *ast.node(def.parent)
    else {
        return false;
    };
    if type_only {
        return false;
    }
    let imported_name: Option<String> = match *ast.node(def.node) {
        Node::ImportSpecifier {
            type_only: true, ..
        } => return false,
        Node::ImportSpecifier {
            imported, local, ..
        } => {
            let n = imported.unwrap_or(local);
            match *ast.node(n) {
                Node::Ident { name } | Node::StringLit { value: name } => {
                    let s = ast.atom(name);
                    (s != "default").then(|| s.to_owned())
                }
                _ => return false,
            }
        }
        _ => None,
    };
    let Node::StringLit { value } = *ast.node(source) else {
        return false;
    };
    let module = ast.atom(value);
    let module = module.strip_prefix("node:").unwrap_or(module);
    let mut method = String::new();
    if let Some(n) = &imported_name {
        method.push_str(n);
    }
    for p in props.iter().rev() {
        if !method.is_empty() {
            method.push('.');
        }
        method.push_str(p);
    }
    if awaited {
        (module == "fs/promises" && method == "readFile")
            || (module == "fs" && method == "promises.readFile")
    } else {
        module == "fs" && method == "readFileSync"
    }
}

fn is_supported_encoding(cx: &Ctx<'_, '_>, arg: Option<NodeId>) -> bool {
    let ast = cx.ast();
    let Some(arg) = arg else {
        return true;
    };
    let mut node = skip_wrappers(ast, arg);
    if let Node::ObjectLit { props } = *ast.node(node) {
        let &[prop] = ast.list(props) else {
            return false;
        };
        match *ast.node(prop) {
            Node::Property { key, value, .. } => {
                let key_ok = match *ast.node(key) {
                    Node::Ident { name } | Node::StringLit { value: name } => {
                        ast.atom(name) == "encoding"
                    }
                    _ => false,
                };
                if !key_ok {
                    return false;
                }
                node = value;
            }
            Node::ShorthandProp {
                name, init: None, ..
            } if ident_name(ast, name) == Some("encoding") => node = name,
            _ => return false,
        }
    }
    match static_value_control_flow(cx, node) {
        Some(Static::Null | Static::Undefined) => true,
        Some(Static::Str(s)) => {
            let l = s.to_ascii_lowercase();
            l == "utf8" || l == "utf-8"
        }
        _ => false,
    }
}

/// The JSON path: `(value, module_relative)`.
fn json_path(cx: &Ctx<'_, '_>, arg: NodeId) -> Option<(String, bool)> {
    let ast = cx.ast();
    let mut node = skip_wrappers(ast, arg);
    let mut module_relative = false;
    if let Node::New { callee, args, .. } = *ast.node(node)
        && ident_name(ast, callee) == Some("URL")
        && matches!(ast.node(callee), Node::Ident { .. })
        && let &[path, base] = ast.list(args)
        && !matches!(ast.node(path), Node::Spread { .. })
        && !matches!(ast.node(base), Node::Spread { .. })
    {
        let base = skip_parens(ast, base);
        let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(base)
        else {
            return None;
        };
        if ident_name(ast, prop) != Some("url")
            || !matches!(ast.node(prop), Node::Ident { .. })
            || !matches!(
                ast.node(skip_parens(ast, obj)),
                Node::MetaProperty {
                    kind: MetaPropKind::ImportMeta
                }
            )
        {
            return None;
        }
        node = path;
        module_relative = true;
    }
    match static_value_control_flow(cx, node)? {
        Static::Str(s) if s.ends_with(".json") => Some((s, module_relative)),
        _ => None,
    }
}

impl Rule for PreferJsonImport {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::VarDeclarator]
    }
    fn enter(&self, declarator: NodeId, cx: &mut Ctx<'_, '_>) {
        if cx.env().source_type != SourceType::Module {
            return;
        }
        let ast = cx.ast();
        let Node::VarDeclarator {
            name,
            type_ann,
            init: Some(init),
            ..
        } = *ast.node(declarator)
        else {
            return;
        };
        let Some(declaration) = cx.parent(declarator) else {
            return;
        };
        let Node::VarDecl {
            modifiers,
            kind,
            decls,
        } = *ast.node(declaration)
        else {
            return;
        };
        if !cx
            .parent(declaration)
            .is_some_and(|p| matches!(ast.node(p), Node::Program { .. }))
        {
            return;
        }
        let exported = modifiers.contains(Modifiers::EXPORT);
        let node = skip_wrappers(ast, init);
        // `JSON.parse(x)`.
        let Some((callee, args)) = plain_call(ast, node, 1, 1) else {
            return;
        };
        let Node::Member {
            obj,
            optional: false,
            prop,
        } = *ast.node(skip_parens(ast, callee))
        else {
            return;
        };
        if ident_name(ast, prop) != Some("parse")
            || !matches!(ast.node(prop), Node::Ident { .. })
            || ident_name(ast, skip_parens(ast, obj)) != Some("JSON")
            || !matches!(ast.node(skip_parens(ast, obj)), Node::Ident { .. })
        {
            return;
        }
        let mut read = skip_wrappers(ast, args[0]);
        let awaited = if let Node::Await { arg } = *ast.node(read) {
            read = skip_wrappers(ast, arg);
            true
        } else {
            false
        };
        let Some((read_callee, read_args)) = plain_call(ast, read, 1, 2) else {
            return;
        };
        let sem = cx.semantic();
        if !is_file_read(cx, sem, read_callee, awaited)
            || !is_supported_encoding(cx, read_args.get(1).copied())
        {
            return;
        }
        let Some((path, module_relative)) = json_path(cx, read_args[0]) else {
            return;
        };
        let suggest = module_relative
            && (path.starts_with("./") || path.starts_with("../"))
            && !path.contains(['#', '?', '\\'])
            && !exported
            && kind == VarKind::Const
            && ast.list(decls).len() == 1
            && matches!(ast.node(name), Node::Ident { .. })
            && type_ann.is_none()
            && skip_parens(ast, init) == node
            && ast.comments_in(cx.span(declaration)).is_empty();
        let decl_span = cx.span(declaration);
        let report = cx
            .report_node(
                node,
                "Prefer a JSON import over reading and parsing a JSON file.",
            )
            .id("prefer-json-import");
        if suggest {
            let binding = ident_name(ast, name).unwrap_or_default();
            report.suggest(
                "prefer-json-import/suggestion",
                "Replace the declaration with a JSON import (changes loading time and caches the data).",
                Fix::replace(
                    decl_span,
                    format!(
                        "import {binding} from {} with {{type: 'json'}};",
                        escape_string(&path, None)
                    ),
                ),
            );
        }
    }
}
