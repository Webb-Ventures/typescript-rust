//! Static detection of CommonJS export names: what a dev server needs to offer named imports
//! (`import { useState } from "react"`) from a CommonJS module served as ESM.
//!
//! It is a syntactic approximation in the spirit of Node's documented CommonJS named-export
//! detection: names come from assignments anywhere in the file (including inside IIFEs and
//! `if` branches) and are never evaluated. Recognized forms:
//!
//! - `exports.a = …`, `exports["a"] = …`, `module.exports.a = …`
//! - `Object.defineProperty(exports, "a", …)` (also on `module.exports`)
//! - `module.exports = { a, b: …, c() {}, ...require("./x") }`
//! - re-exports, followed by the caller: `module.exports = require("./x")`,
//!   `__exportStar(require("./x"), exports)`, `__export(require("./x"))`
//!
//! Shadowed `exports`/`module` bindings are not tracked (like Node's detector, a false name
//! only yields an `undefined` export).

use rustc_hash::FxHashSet;
use ts_r_ast::{AssignOp, Ast, Node, NodeId, SourceKind};

/// Export names found in one CommonJS file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CjsExports {
    /// In first-seen order, without duplicates.
    pub names: Vec<String>,
    /// Specifiers whose exports are re-exported, in source order.
    pub reexports: Vec<String>,
}

impl CjsExports {
    fn add(&mut self, seen: &mut FxHashSet<String>, name: &str) {
        if seen.insert(name.to_owned()) {
            self.names.push(name.to_owned());
        }
    }
}

/// Finds the export names and re-exports of a CommonJS file.
pub fn analyze(src: &str) -> CjsExports {
    let parsed = ts_r_parser::parse(src, SourceKind::Js);
    analyze_ast(&parsed.ast)
}

/// [`analyze`] on a parsed tree.
pub fn analyze_ast(ast: &Ast) -> CjsExports {
    let mut out = CjsExports::default();
    let mut seen = FxHashSet::default();
    for node in &ast.nodes {
        match *node {
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => {
                if let Some(name) = exports_member(ast, left) {
                    out.add(&mut seen, &name);
                } else if is_module_exports(ast, left) {
                    let right = unparen(ast, right);
                    if let Some(spec) = require_of(ast, right) {
                        out.reexports.push(spec);
                    } else if let Node::ObjectLit { props } = *ast.node(right) {
                        for &p in ast.list(props) {
                            match *ast.node(p) {
                                Node::Property { key, .. } | Node::MethodDef { key, .. } => {
                                    if let Some(k) = key_name(ast, key) {
                                        out.add(&mut seen, &k);
                                    }
                                }
                                Node::ShorthandProp { name, .. } => {
                                    if let Some(k) = key_name(ast, name) {
                                        out.add(&mut seen, &k);
                                    }
                                }
                                Node::Spread { arg } => {
                                    if let Some(spec) = require_of(ast, unparen(ast, arg)) {
                                        out.reexports.push(spec);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Node::Call { callee, args, .. } => {
                let args = ast.list(args);
                let callee = unparen(ast, callee);
                if is_member(ast, callee, "Object", "defineProperty")
                    && args.len() >= 2
                    && is_exports_object(ast, args[0])
                    && let Node::StringLit { value } = *ast.node(args[1])
                {
                    out.add(&mut seen, ast.atom(value));
                    continue;
                }
                let fname = match *ast.node(callee) {
                    Node::Ident { name } => Some(ast.atom(name)),
                    Node::Member { prop, .. } => match *ast.node(prop) {
                        Node::Ident { name } => Some(ast.atom(name)),
                        _ => None,
                    },
                    _ => None,
                };
                if matches!(
                    fname,
                    Some("__exportStar" | "__export" | "_exportStar" | "_export_star")
                ) && let Some(&first) = args.first()
                    && let Some(spec) = require_of(ast, unparen(ast, first))
                {
                    out.reexports.push(spec);
                }
            }
            _ => {}
        }
    }
    out
}

fn unparen(ast: &Ast, mut id: NodeId) -> NodeId {
    while let Node::Paren { expr } = *ast.node(id) {
        id = expr;
    }
    id
}

fn ident_is(ast: &Ast, id: NodeId, want: &str) -> bool {
    matches!(*ast.node(id), Node::Ident { name } if ast.atom(name) == want)
}

fn is_member(ast: &Ast, id: NodeId, obj: &str, prop: &str) -> bool {
    matches!(*ast.node(id), Node::Member { obj: o, prop: p, .. }
        if ident_is(ast, unparen(ast, o), obj) && ident_is(ast, p, prop))
}

fn is_module_exports(ast: &Ast, id: NodeId) -> bool {
    is_member(ast, unparen(ast, id), "module", "exports")
}

/// `exports` or `module.exports`.
fn is_exports_object(ast: &Ast, id: NodeId) -> bool {
    let id = unparen(ast, id);
    ident_is(ast, id, "exports") || is_module_exports(ast, id)
}

/// `exports.a` / `exports["a"]` / `module.exports.a` → `a`.
fn exports_member(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(unparen(ast, id)) {
        Node::Member { obj, prop, .. } if is_exports_object(ast, obj) => match *ast.node(prop) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            _ => None,
        },
        Node::Index { obj, index, .. } if is_exports_object(ast, obj) => {
            match *ast.node(unparen(ast, index)) {
                Node::StringLit { value } => Some(ast.atom(value).to_owned()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// `require("x")` → `x`.
fn require_of(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::Call { callee, args, .. } if ident_is(ast, unparen(ast, callee), "require") => {
            match ast.list(args) {
                [a] => match *ast.node(*a) {
                    Node::StringLit { value } => Some(ast.atom(value).to_owned()),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

fn key_name(ast: &Ast, key: NodeId) -> Option<String> {
    match *ast.node(key) {
        Node::Ident { name } | Node::StringLit { value: name } => Some(ast.atom(name).to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_names_and_reexports() {
        let e = analyze(
            "'use strict';\n\
             if (process.env.NODE_ENV === 'production') { module.exports = require('./prod.js'); }\n\
             else { module.exports = require('./dev.js'); }\n",
        );
        assert!(e.names.is_empty());
        assert_eq!(e.reexports, ["./prod.js", "./dev.js"]);

        let e = analyze(
            "(function () { exports.useState = function () {}; exports['use-x'] = 1;\n\
             module.exports.version = '19'; exports.useState = null; })();\n\
             Object.defineProperty(exports, '__esModule', { value: true });\n\
             __exportStar(require('./more'), exports);\n",
        );
        assert_eq!(e.names, ["useState", "use-x", "version", "__esModule"]);
        assert_eq!(e.reexports, ["./more"]);

        let e = analyze("module.exports = { a, b: 1, c() {}, 'd': 2, ...require('./e') };");
        assert_eq!(e.names, ["a", "b", "c", "d"]);
        assert_eq!(e.reexports, ["./e"]);
    }
}
