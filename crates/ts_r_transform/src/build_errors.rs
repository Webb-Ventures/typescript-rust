//! Which parser errors fail a build (ts-r-build, ts-r-run, ts-r-dev).
//!
//! The parser reports what tsc reports before type checking, including tsc's checker grammar
//! errors ([`ParseResult::grammar`]). A build tool follows esbuild instead (observed with
//! esbuild 0.28.2): it fails on syntax errors and on the grammar errors that are ECMAScript
//! early errors (`const x;`, two `default` clauses), and accepts TypeScript-only grammar
//! rules (`declare function f() {}`, `module M {}`) and sloppy-mode leniencies
//! (`for (var x = 1 in o)`, `if (x) ;`, `l: var x`).

use ts_r_ast::{Ast, Modifiers, Node, NodeId, UnaryOp, VarKind};
use ts_r_diagnostics::Diagnostic;
use ts_r_parser::ParseResult;

/// tsc grammar errors (checker-reported) that esbuild also fails on.
const REJECTED_GRAMMAR: &[u32] = &[
    1005,  // `'{' expected`: accessor without a body (esbuild rejects it in object literals)
    1009,  // trailing comma in a declaration list or heritage clause
    1025,  // trailing comma in an index signature
    1091,  // `for (var x, y in o)`
    1098,  // `class C<> {}`
    1099,  // `f<>()`
    1104,  // `continue` outside an iteration statement
    1105,  // `break` outside an iteration or switch statement
    1107,  // jump target across a function boundary
    1113,  // two `default` clauses
    1115,  // `continue` to a non-iteration label
    1116,  // `break` to an unknown label
    1120,  // modifiers on `export =`
    1123,  // empty declaration list
    1141,  // `import(T)` type with a non-string argument
    1155,  // `const x;`, `using x;`
    1156,  // `if (x) let y = 1;`
    1188,  // `for (var x, y of a)`
    1190,  // `for (var x = 1 of a)`
    1197,  // `catch (e = 1)`
    1211,  // `class {}` as a statement
    1248,  // `const` class member
    1317,  // rest parameter property
    1325,  // `import(...a)`
    1358,  // tagged template in an optional chain
    1359,  // `await` as a name where it is an operator
    1492,  // `using {a} = x`
    5076,  // `a && b ?? c`: `??` mixed with `&&` / `||` without parentheses (a JS syntax error)
    1189,  // `for (x = 1 in o)`: accepted for a sloppy-mode `var x` (`for_in_var_heads`)
    18016, // private name outside a class
];

/// A tsc parser error that esbuild accepts: `a?.#x` (TS18030).
const ACCEPTED_SYNTAX: &[u32] = &[18030];

/// The error diagnostics of `parsed` that a build fails on, in source order.
pub fn build_errors(parsed: &ParseResult) -> Vec<&Diagnostic> {
    parse_build_errors(&parsed.diagnostics, &parsed.grammar, &parsed.ast)
}

/// [`build_errors`] over the parts of a [`ParseResult`]: `diagnostics` and `grammar` as in
/// the parse result, `ast` its tree.
pub fn parse_build_errors<'a>(
    diagnostics: &'a [Diagnostic],
    grammar: &[Diagnostic],
    ast: &Ast,
) -> Vec<&'a Diagnostic> {
    let mut out = Vec::new();
    let mut heads: Option<Vec<(u32, bool)>> = None;
    for d in diagnostics {
        if !d.is_error() || ACCEPTED_SYNTAX.contains(&d.code) {
            continue;
        }
        if grammar.contains(d) {
            if !REJECTED_GRAMMAR.contains(&d.code) {
                continue;
            }
            if d.code == 1189 {
                let heads = heads.get_or_insert_with(|| for_in_var_heads(ast));
                if heads
                    .iter()
                    .any(|&(start, strict)| start == d.span.start && !strict)
                {
                    continue;
                }
            }
        }
        out.push(d);
    }
    out
}

/// Whether the file has ES module syntax (`import`, `export`): it is strict code.
pub fn has_module_syntax(ast: &Ast) -> bool {
    match *ast.node(ast.root) {
        Node::Program { stmts } => ast.list(stmts).iter().any(|&s| {
            matches!(
                ast.node(s),
                Node::ImportDecl { .. }
                    | Node::ExportNamed { .. }
                    | Node::ExportAll { .. }
                    | Node::ExportDefaultExpr { .. }
            ) || ast.node(s).modifiers().contains(Modifiers::EXPORT)
        }),
        _ => false,
    }
}

/// Visits every node with whether it is in code made strict by the file itself: a
/// `"use strict"` prologue or a class body (not module syntax: see [`has_module_syntax`]).
fn walk_strict(ast: &Ast, mut f: impl FnMut(NodeId, &Node, bool)) {
    let mut stack: Vec<(NodeId, bool)> = vec![(ast.root, false)];
    while let Some((id, strict)) = stack.pop() {
        let node = ast.node(id);
        let mut strict = strict || matches!(node, Node::ClassDecl { .. } | Node::ClassExpr { .. });
        if !strict {
            ast.for_each_child(id, |c| {
                if let Node::Directive { value } = *ast.node(c)
                    && ast.atom(value) == "use strict"
                {
                    strict = true;
                }
            });
        }
        f(id, node, strict);
        ast.for_each_child(id, |c| stack.push((c, strict)));
    }
}

/// The declarator start of every `for (var x = ... in o)` head with a plain name, and
/// whether it is in strict code (an ES module, a `"use strict"` prologue, a class body).
fn for_in_var_heads(ast: &Ast) -> Vec<(u32, bool)> {
    let module = has_module_syntax(ast);
    let mut heads = Vec::new();
    walk_strict(ast, |_, node, strict| {
        if let Node::ForIn { left, .. } = *node
            && let Node::VarDecl {
                kind: VarKind::Var,
                decls,
                ..
            } = *ast.node(left)
            && let [decl] = *ast.list(decls)
            && let Node::VarDeclarator { name, .. } = *ast.node(decl)
            && matches!(ast.node(name), Node::Ident { .. })
        {
            heads.push((ast.span(decl).start, strict || module));
        }
    });
    heads
}

/// Sloppy-mode-only syntax esbuild rejects in strict code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SloppyOnly {
    /// A `with` statement.
    With,
    /// `delete x` of a bare (possibly parenthesized) name.
    DeleteName,
}

impl SloppyOnly {
    /// esbuild's wording: "With statements" / "Delete of a bare identifier".
    pub fn what(self) -> &'static str {
        match self {
            SloppyOnly::With => "With statements",
            SloppyOnly::DeleteName => "Delete of a bare identifier",
        }
    }
}

/// Each `with` statement and `delete name` in the file: its start, which, and whether the
/// file itself makes it strict code (a `"use strict"` prologue or a class body around it).
/// Whether module syntax, the output format or tsconfig's `alwaysStrict` make it an error
/// is the caller's decision. Cheap when the file has neither.
pub fn sloppy_only_syntax(ast: &Ast) -> Vec<(u32, SloppyOnly, bool)> {
    let delete_name = |arg: NodeId| {
        let mut arg = arg;
        while let Node::Paren { expr } = *ast.node(arg) {
            arg = expr;
        }
        matches!(ast.node(arg), Node::Ident { .. })
    };
    let any = ast.nodes.iter().any(|n| match *n {
        Node::With { .. } => true,
        Node::Unary {
            op: UnaryOp::Delete,
            arg,
        } => delete_name(arg),
        _ => false,
    });
    let mut out = Vec::new();
    if !any {
        return out;
    }
    walk_strict(ast, |id, node, strict| {
        let kind = match *node {
            Node::With { .. } => SloppyOnly::With,
            Node::Unary {
                op: UnaryOp::Delete,
                arg,
            } if delete_name(arg) => SloppyOnly::DeleteName,
            _ => return,
        };
        let at = match *node {
            Node::Unary { arg, .. } => ast.span(arg).start,
            _ => ast.span(id).start,
        };
        out.push((at, kind, strict));
    });
    out.sort_by_key(|&(at, ..)| at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;

    fn fails(src: &str, kind: SourceKind) -> bool {
        let parsed = ts_r_parser::parse(src, kind);
        !build_errors(&parsed).is_empty()
    }

    #[test]
    fn esbuild_accepts_typescript_grammar_and_sloppy_mode() {
        for (src, kind) in [
            ("if (x) ;", SourceKind::Js),
            (
                "class C { #x; static f(o) { return o?.#x } }",
                SourceKind::Js,
            ),
            (
                "class C { #m() {} static f(o) { return o?.#m() } }",
                SourceKind::Js,
            ),
            ("for (var x = 1 in o);", SourceKind::Js),
            ("for (var x = 1 in o);", SourceKind::Ts),
            ("if (t) for (var a = b in {});", SourceKind::Js),
            ("declare function f() { return 1 }", SourceKind::Ts),
            ("declare namespace N { function f() {} }", SourceKind::Ts),
            ("declare let x = 1;", SourceKind::Ts),
            ("declare function* g(): any;", SourceKind::Ts),
            ("module M { export const x = 1 }", SourceKind::Ts),
            (
                "abstract class C { abstract get x() { return 1 } }",
                SourceKind::Ts,
            ),
            ("let {a};", SourceKind::Js),
            ("l: var x = 1;", SourceKind::Js),
            ("return;", SourceKind::Js),
        ] {
            assert!(!fails(src, kind), "{src}");
        }
    }

    #[test]
    fn esbuild_rejects_early_errors() {
        for (src, kind) in [
            ("const x;", SourceKind::Js),
            ("const x: number;", SourceKind::Ts),
            ("switch (1) { default: break; default: }", SourceKind::Js),
            ("if (1) let x = 1;", SourceKind::Js),
            ("try {} catch (e = 1) {}", SourceKind::Js),
            ("for (let x = 1 in o);", SourceKind::Js),
            ("for (var [x] = 1 in o);", SourceKind::Js),
            ("for (var x = 1 of o);", SourceKind::Js),
            ("'use strict'; for (var x = 1 in o);", SourceKind::Js),
            ("export {}; for (var x = 1 in o);", SourceKind::Js),
            (
                "function f() { 'use strict'; for (var x = 1 in o); }",
                SourceKind::Js,
            ),
            (
                "class C { static { for (var x = 1 in o); } }",
                SourceKind::Js,
            ),
            ("class C<> {}", SourceKind::Ts),
            ("for (var x, y in o);", SourceKind::Js),
            ("let x = 1 +;", SourceKind::Js),
            ("f((undefined && foo ?? {}));", SourceKind::Ts),
            ("a ?? b || c;", SourceKind::Js),
            ("a || b ?? c;", SourceKind::Js),
        ] {
            assert!(fails(src, kind), "{src}");
        }
    }

    #[test]
    fn optional_chain_tagged_templates_fail_a_build() {
        for src in ["a?.b`x`;", "a?.[b]`x`;", "a?.()`x`;"] {
            for kind in [SourceKind::Js, SourceKind::Ts] {
                let parsed = ts_r_parser::parse(src, kind);
                assert!(parsed.grammar.iter().any(|d| d.code == 1358), "{src}");
                assert!(
                    build_errors(&parsed).iter().any(|d| d.code == 1358),
                    "{src}"
                );
            }
        }
        assert!(!fails("(a?.b)`x`;", SourceKind::Ts));
        let out = crate::transpile(
            "a.ts",
            "const a = { b: () => 'ok' }; a?.b`x`;",
            &crate::TransformOptions::default(),
            &ts_r_codegen::CodegenOptions::default(),
        );
        assert!(out.build_errors.iter().any(|d| d.code == 1358));
    }

    #[test]
    fn invalid_jump_targets_fail_a_build() {
        for (src, code) in [
            ("continue;", 1104),
            ("break;", 1105),
            ("outer: while (true) { (() => { break outer; })() }", 1107),
            ("label: { continue label; }", 1115),
            ("while (true) { break missing; }", 1116),
        ] {
            for kind in [SourceKind::Js, SourceKind::Ts] {
                let parsed = ts_r_parser::parse(src, kind);
                assert!(
                    build_errors(&parsed).iter().any(|d| d.code == code),
                    "{src}: expected TS{code}"
                );
            }
        }
        assert!(!fails(
            "outer: while (true) { if (x) continue outer; break outer; }",
            SourceKind::Js
        ));
    }

    #[test]
    fn finds_sloppy_only_syntax() {
        let src = "with (a) b;\ndelete (x);\ndelete x.y;\ndelete (1 ? z : z);\n\
                   function f() { 'use strict'; delete y }\nclass C { m() { with (a) b } }\n";
        let ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let found: Vec<_> = sloppy_only_syntax(&ast)
            .into_iter()
            .map(|(at, k, strict)| (&src[at as usize..at as usize + 1], k, strict))
            .collect();
        assert_eq!(
            found,
            [
                ("w", SloppyOnly::With, false),
                ("(", SloppyOnly::DeleteName, false),
                ("y", SloppyOnly::DeleteName, true),
                ("w", SloppyOnly::With, true),
            ]
        );
        let ast = ts_r_parser::parse("delete x.y; with (a) {}", SourceKind::Js).ast;
        assert!(!has_module_syntax(&ast));
        assert_eq!(sloppy_only_syntax(&ast).len(), 1);
    }
}
