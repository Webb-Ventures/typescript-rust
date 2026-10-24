//! What a target forbids that lowering does not handle (`features.rs`): the syntax esbuild
//! refuses to lower, as build errors with its messages.
//!
//! Observed with esbuild 0.28.2 (`--target=es5`, `--supported:<feature>=false`):
//! `const`/`let`, classes, destructuring, default and rest parameters, `for-of`,
//! generators, async functions (below generators) and array or call spread fail with
//! "Transforming X to the configured target environment is not supported yet"; arrows and
//! template literals are lowered, which ts-r-build leaves as written (Node runs them, and
//! tsc 7 emits ES2015 at the lowest). A top-level `await` fails without
//! `top-level-await`. String import and export names (`arbitrary-module-namespace-names`)
//! depend on what the output keeps, so `string_names` checks them apart.

use ts_r_ast::{Ast, FnFlags, Modifiers, Node, NodeId, NodeKind, VarKind};
use ts_r_config::ScriptTarget;

use crate::features::{Feature, Features};

/// Whether a JavaScript file needs `ts_r_transform` to lower it to `target` (TypeScript
/// files always go through it).
pub fn js_needs_lowering(ast: &Ast, target: ScriptTarget) -> bool {
    if target >= ScriptTarget::ESNext {
        return false;
    }
    if target < ScriptTarget::ES2022 {
        return true;
    }
    ast.nodes.iter().any(|n| match n {
        Node::PropertyDef { modifiers, .. } => modifiers.contains(Modifiers::ACCESSOR),
        Node::VarDecl { kind, .. } => matches!(kind, VarKind::Using | VarKind::AwaitUsing),
        Node::Decorator { .. } => true,
        _ => false,
    })
}

/// esbuild's errors for syntax in the original tree `ast` that `unsupported` forbids and
/// lowering does not handle: `(offset, message)`, in source order.
pub fn errors(ast: &Ast, unsupported: Features) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    if unsupported.is_empty() {
        return out;
    }
    let not_yet = |what: &str| {
        format!("Transforming {what} to the configured target environment is not supported yet")
    };
    // (node, at the module's top level for `await`)
    let mut stack = vec![(ast.root, true)];
    while let Some((id, top)) = stack.pop() {
        let n = *ast.node(id);
        if ts_r_transform::is_type_kind(n.kind()) || n.modifiers().contains(Modifiers::DECLARE) {
            continue;
        }
        if matches!(
            n,
            Node::Interface { .. } | Node::TypeAlias { .. } | Node::FunctionDecl { body: None, .. }
        ) {
            continue;
        }
        let at = ast.span(id).start;
        let has = |f| unsupported.has(f);
        let msg = match n {
            Node::VarDecl {
                kind: VarKind::Const,
                ..
            } if has(Feature::ConstAndLet) => Some(not_yet("const")),
            Node::VarDecl {
                kind: VarKind::Let, ..
            } if has(Feature::ConstAndLet) => Some(not_yet("let")),
            Node::ClassDecl { .. } | Node::ClassExpr { .. } if has(Feature::Class) => {
                Some(not_yet("class syntax"))
            }
            Node::ObjectPattern { .. } | Node::ArrayPattern { .. }
                if has(Feature::Destructuring) =>
            {
                Some(not_yet("destructuring"))
            }
            Node::Param { init: Some(_), .. } if has(Feature::DefaultArgument) => {
                Some(not_yet("default arguments"))
            }
            Node::Param { rest: true, .. } if has(Feature::RestArgument) => {
                Some(not_yet("rest arguments"))
            }
            Node::ForOf {
                is_await: false, ..
            } if has(Feature::ForOf) => Some(not_yet("for-of loops")),
            Node::FunctionDecl { flags, .. }
            | Node::FunctionExpr { flags, .. }
            | Node::Arrow { flags, .. }
                if flags.contains(FnFlags::ASYNC)
                    && has(Feature::AsyncAwait)
                    && has(Feature::Generator) =>
            {
                Some(not_yet("async functions"))
            }
            Node::FunctionDecl { flags, .. } | Node::FunctionExpr { flags, .. }
                if flags.contains(FnFlags::GENERATOR) && has(Feature::Generator) =>
            {
                Some(not_yet("generator functions"))
            }
            Node::ArrayLit { elements } if has(Feature::ArraySpread) => ast
                .list(elements)
                .iter()
                .any(|&e| ast.kind(e) == NodeKind::Spread)
                .then(|| not_yet("array spread")),
            Node::Call { args, .. } | Node::New { args, .. } if has(Feature::ArraySpread) => ast
                .list(args)
                .iter()
                .any(|&e| ast.kind(e) == NodeKind::Spread)
                .then(|| not_yet("array spread")),
            Node::Await { .. } | Node::ForOf { is_await: true, .. }
                if top && has(Feature::TopLevelAwait) =>
            {
                Some("Top-level await is not available in the configured target environment".into())
            }
            Node::VarDecl {
                kind: VarKind::AwaitUsing,
                ..
            } if top && has(Feature::TopLevelAwait) => {
                Some("Top-level await is not available in the configured target environment".into())
            }
            _ => None,
        };
        if let Some(m) = msg {
            out.push((at, m));
        }
        let function = matches!(
            n,
            Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::Arrow { .. }
                | Node::MethodDef { .. }
                | Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
        );
        let top = top && !function;
        ast.for_each_child(id, |c| stack.push((c, top)));
    }
    out.sort_by_key(|&(at, _)| at);
    out
}

/// The offsets of the BigInt literals in values of `ast` (not in types or `declare`d code), in
/// source order. Below `bigint` esbuild warns at each one that it may crash at run time, and
/// still builds.
pub fn bigint_literals(ast: &Ast) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![ast.root];
    while let Some(id) = stack.pop() {
        let n = *ast.node(id);
        if ts_r_transform::is_type_kind(n.kind()) || n.modifiers().contains(Modifiers::DECLARE) {
            continue;
        }
        if matches!(
            n,
            Node::Interface { .. } | Node::TypeAlias { .. } | Node::FunctionDecl { body: None, .. }
        ) {
            continue;
        }
        if let Node::BigIntLit { .. } = n {
            out.push(ast.span(id).start);
        }
        ast.for_each_child(id, |c| stack.push(c));
    }
    out.sort_unstable();
    out
}

/// esbuild's errors for string module names (`import { "a b" as x }`) that would reach the
/// output with `arbitrary-module-namespace-names` unsupported: a string that is not an
/// identifier, as an import name from a module `kept` says stays an import (all of them
/// without bundling; external ones when bundling), and as an export name when `exports`
/// (the module's exports are the ESM output's: every file without bundling, the entries and
/// what they `export *` when bundling). CommonJS and IIFE output have none: their names are
/// property keys. `(offset, message)`, in source order.
pub fn string_names(ast: &Ast, exports: bool, kept: &dyn Fn(&str) -> bool) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let check = |out: &mut Vec<(u32, String)>, id: Option<NodeId>, what: &str| {
        if let Some(id) = id
            && let Some(s) = string_name(ast, id)
            && !ts_r_minify::names::is_identifier(&s)
        {
            out.push((ast.span(id).start, string_name_error(&s, what)));
        }
    };
    for s in ts_r_minify::build::program_stmts(ast) {
        match *ast.node(s) {
            Node::ImportDecl {
                type_only: false,
                specifiers,
                source,
                ..
            } if string_name(ast, source).is_some_and(|p| kept(&p)) => {
                for &sp in ast.list(specifiers) {
                    if let Node::ImportSpecifier {
                        type_only: false,
                        imported,
                        ..
                    } = *ast.node(sp)
                    {
                        check(&mut out, imported, "import");
                    }
                }
            }
            Node::ExportNamed {
                type_only: false,
                specifiers,
                source,
                ..
            } => {
                let from_kept = source
                    .and_then(|p| string_name(ast, p))
                    .is_some_and(|p| kept(&p));
                for &sp in ast.list(specifiers) {
                    if let Node::ExportSpecifier {
                        type_only: false,
                        local,
                        exported,
                    } = *ast.node(sp)
                    {
                        if from_kept {
                            check(&mut out, Some(local), "import");
                        }
                        if exports {
                            check(&mut out, exported.or(source.map(|_| local)), "export");
                        }
                    }
                }
            }
            Node::ExportAll {
                type_only: false,
                exported,
                ..
            } if exports => check(&mut out, exported, "export"),
            _ => {}
        }
    }
    out.sort_by_key(|&(at, _)| at);
    out
}

fn string_name(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        _ => None,
    }
}

fn string_name_error(name: &str, what: &str) -> String {
    format!(
        "Using the string {name:?} as an {what} name is not supported in the configured target environment"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;

    fn errs(src: &str, features: &[&str]) -> Vec<String> {
        let parsed = ts_r_parser::parse(src, SourceKind::Ts);
        let mut u = Features::NONE;
        for f in features {
            u.insert(Feature::parse(f).unwrap());
        }
        errors(&parsed.ast, u).into_iter().map(|(_, m)| m).collect()
    }

    #[test]
    fn es5_level_syntax() {
        let es5 = Features::newer_than(2014);
        let parsed = ts_r_parser::parse(
            "const a = 1; class C {} function f(x = 1, ...r) {} var [y] = z; for (var q of z);",
            SourceKind::Ts,
        );
        let m: Vec<String> = errors(&parsed.ast, es5)
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        assert_eq!(m.len(), 6, "{m:?}");
        assert!(m[0].contains("const"));
        assert!(m[1].contains("class syntax"));
        // Arrows and template literals are allowed (esbuild lowers them).
        let parsed = ts_r_parser::parse("var f = () => `a${b}`;", SourceKind::Ts);
        assert!(errors(&parsed.ast, es5).is_empty());
        // Types are not syntax.
        let parsed = ts_r_parser::parse("declare const x: number; type T = 1;", SourceKind::Ts);
        assert!(errors(&parsed.ast, es5).is_empty());
    }

    #[test]
    fn module_level_features() {
        assert_eq!(
            errs(
                "await 1; async function f() { await 2 }",
                &["top-level-await"]
            )
            .len(),
            1
        );
        assert!(errs("await 1;", &[]).is_empty());
        let names = |src: &str, exports: bool| -> Vec<String> {
            let parsed = ts_r_parser::parse(src, SourceKind::Ts);
            string_names(&parsed.ast, exports, &|p| p == "ext")
                .into_iter()
                .map(|(_, m)| m)
                .collect()
        };
        assert_eq!(
            names("export { x as \"a b\", x as \"ok\" }; var x;", true),
            [
                "Using the string \"a b\" as an export name is not supported in the configured target environment"
            ]
        );
        assert!(names("export { x as \"a b\" }; var x;", false).is_empty());
        // Only imports that stay imports.
        assert_eq!(
            names("import { \"x y\" as z } from \"ext\";", false).len(),
            1
        );
        assert!(names("import { \"x y\" as z } from \"./in\";", true).is_empty());
        // `export { "a" } from` names both an import and an export.
        let m = names(
            "export { \"a b\" } from \"ext\"; export * as \"c d\" from \"./in\";",
            true,
        );
        assert_eq!(m.len(), 3, "{m:?}");
        assert!(m[0].contains("import name") && m[1].contains("export name"));
    }
}
