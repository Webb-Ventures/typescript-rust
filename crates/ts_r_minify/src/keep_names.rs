//! `--keep-names`: functions and classes keep their `name` when renaming changed the binding
//! they were named after (esbuild's `keepNames`, from its documentation and observed output).
//!
//! Runs right after [`crate::rename::apply`], with the scopes the renames were computed from:
//! a binding is renamed when its identifier's text differs from its symbol's name. Then:
//!
//! - a function or class declaration is followed by `helper(f, "f")` in its statement list;
//! - a named function or class expression becomes `helper(function f() {}, "f")`;
//! - an anonymous function, arrow or class that takes its name from a renamed binding
//!   (`var f = () => {}`, `f = function () {}`, a parameter default) is wrapped the same way.
//!
//! `helper(target, value)` defines `target.name` as `value` (configurable, not writable) and
//! returns `target`. A class with a static member called `name` keeps its own. Whether names
//! changed decides, not whether a binding is used: unused code is left to tree shaking.

use rustc_hash::FxHashMap;
use ts_r_ast::{AssignOp, Ast, Modifiers, Node, NodeId, NodeList};

use crate::build;
use crate::scope::Scopes;

/// What [`apply`] did.
#[derive(Debug, Default)]
pub struct KeptNames {
    /// Statements to print right after each program-level statement (only filled when
    /// `program` was false: the caller assembles the program).
    pub after: FxHashMap<NodeId, Vec<NodeId>>,
    /// Whether any `helper(...)` call was added.
    pub used: bool,
}

/// How [`apply`] treats the program.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeepNamesOptions {
    /// Also insert into the program's own statement list (else return them in `after`).
    pub program: bool,
    /// Program-level class declarations always get a call (the bundler turns them into
    /// `var C = class _C {}`, whose name would be `_C`).
    pub top_classes: bool,
}

/// Adds the `helper(...)` calls. See the module docs.
pub fn apply(ast: &mut Ast, scopes: &Scopes, helper: &str, opts: KeepNamesOptions) -> KeptNames {
    let mut out = KeptNames::default();
    let top: Vec<NodeId> = build::program_stmts(ast);
    let top_set: rustc_hash::FxHashSet<NodeId> = top.iter().copied().collect();
    // The original name of a binding identifier whose text renaming changed.
    let original = |ast: &Ast, id: NodeId| -> Option<String> {
        let Node::Ident { name } = *ast.node(id) else {
            return None;
        };
        let sym = scopes.symbol_of(id)?;
        let orig = scopes.symbol(sym).name;
        (orig != name).then(|| ast.atom(orig).to_owned())
    };
    let count = ast.nodes.len() as u32;
    let mut wraps: Vec<(NodeId, String)> = Vec::new();
    let mut follow: Vec<(NodeId, NodeId, String)> = Vec::new();
    for raw in 1..count {
        let Some(id) = NodeId::from_raw(raw) else {
            continue;
        };
        match *ast.node(id) {
            Node::FunctionDecl {
                name: Some(n),
                body: Some(_),
                ..
            } => {
                if let Some(o) = original(ast, n) {
                    follow.push((id, n, o));
                }
            }
            Node::ClassDecl {
                name: Some(n),
                members,
                modifiers,
                ..
            } if !modifiers.contains(Modifiers::DECLARE) && !static_name(ast, members) => {
                let o = original(ast, n).or_else(|| {
                    (opts.top_classes && top_set.contains(&id))
                        .then(|| ast.name_of(n).map(str::to_owned))
                        .flatten()
                });
                if let Some(o) = o {
                    follow.push((id, n, o));
                }
            }
            Node::FunctionExpr { name: Some(n), .. } => {
                if let Some(o) = original(ast, n) {
                    wraps.push((id, o));
                }
            }
            Node::ClassExpr {
                name: Some(n),
                members,
                ..
            } if !static_name(ast, members) => {
                if let Some(o) = original(ast, n) {
                    wraps.push((id, o));
                }
            }
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            }
            | Node::Param {
                pattern: name,
                init: Some(init),
                ..
            }
            | Node::Assign {
                left: name,
                op: AssignOp::Assign,
                right: init,
            } => {
                if anonymous(ast, init)
                    && let Some(o) = original(ast, name)
                {
                    wraps.push((init, o));
                }
            }
            _ => {}
        }
    }
    for (slot, name) in wraps {
        let at = ast.span(slot).start;
        let target = build::move_out(ast, slot);
        let value = build::string(ast, &name, at);
        let call = build::call_name(ast, helper, &[target, value], at);
        build::replace(ast, slot, call);
        out.used = true;
    }
    let mut after: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    for (stmt, binding, name) in follow {
        let at = ast.span(stmt).start;
        let target = match ast.name_of(binding) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        let target = build::ident(ast, &target, at);
        let value = build::string(ast, &name, at);
        let call = build::call_name(ast, helper, &[target, value], at);
        let s = build::expr_stmt(ast, call, at);
        after.entry(stmt).or_default().push(s);
        out.used = true;
    }
    if after.is_empty() {
        return out;
    }
    // Statement lists: each declaration's calls go right after it.
    let lists_end = ast.nodes.len() as u32;
    for raw in 1..lists_end {
        let Some(id) = NodeId::from_raw(raw) else {
            continue;
        };
        let list = match *ast.node(id) {
            Node::Block { stmts } | Node::ModuleBlock { stmts } => stmts,
            Node::StaticBlock { body } | Node::SwitchCase { body, .. } => body,
            _ => continue,
        };
        if let Some(new) = with_followers(ast, list, &after) {
            match ast.node_mut(id) {
                Node::Block { stmts } | Node::ModuleBlock { stmts } => *stmts = new,
                Node::StaticBlock { body } | Node::SwitchCase { body, .. } => *body = new,
                _ => {}
            }
        }
    }
    if opts.program {
        let mut stmts = Vec::with_capacity(top.len());
        for s in &top {
            stmts.push(*s);
            if let Some(f) = after.get(s) {
                stmts.extend(f);
            }
        }
        build::set_program(ast, &stmts);
    } else {
        after.retain(|s, _| top_set.contains(s));
        out.after = after;
    }
    out
}

/// `list` with the follow-up statements inserted, or `None` when it has none.
fn with_followers(
    ast: &mut Ast,
    list: NodeList,
    after: &FxHashMap<NodeId, Vec<NodeId>>,
) -> Option<NodeList> {
    let items = ast.list(list);
    if !items.iter().any(|s| after.contains_key(s)) {
        return None;
    }
    let mut stmts = Vec::with_capacity(items.len() + 1);
    for s in items {
        stmts.push(*s);
        if let Some(f) = after.get(s) {
            stmts.extend(f);
        }
    }
    Some(ast.add_list(&stmts))
}

/// An anonymous function, arrow or class expression (its name comes from the binding).
fn anonymous(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::FunctionExpr { name: None, .. } | Node::Arrow { .. } => true,
        Node::ClassExpr {
            name: None,
            members,
            ..
        } => !static_name(ast, members),
        _ => false,
    }
}

/// The class has a static member called `name` (it defines its own).
fn static_name(ast: &Ast, members: NodeList) -> bool {
    ast.list(members).iter().any(|&m| match *ast.node(m) {
        Node::PropertyDef { modifiers, key, .. } | Node::MethodDef { modifiers, key, .. } => {
            modifiers.contains(Modifiers::STATIC) && ast.name_of(key) == Some("name")
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rename::{self, RenameMode, RenameOptions};
    use ts_r_ast::SourceKind;

    fn run(src: &str) -> String {
        let mut ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let scopes = Scopes::analyze(&ast, true);
        let fixed = FxHashMap::default();
        let names = rename::compute(
            &ast,
            &scopes,
            &RenameOptions {
                mode: RenameMode::Mangle,
                fixed: &fixed,
                include_module_scope: true,
                reserved: &|n| n == "__name",
                module_reserved: &|_| false,
            },
        );
        rename::apply(&mut ast, &scopes, &names);
        apply(
            &mut ast,
            &scopes,
            "__name",
            KeepNamesOptions {
                program: true,
                top_classes: false,
            },
        );
        ts_r_codegen::print(&ast, src, &ts_r_codegen::CodegenOptions::default()).code
    }

    #[test]
    fn renamed_functions_and_classes_keep_their_names() {
        let out = run(
            "function foo() {}\nclass Bar {}\nlet baz = () => {}\nlet q = function named() {}\n{ function inner() {} }\nfunction g(cb = function () {}) { return cb }\nclass N { static name = 1 }\nexport { foo, Bar, baz, q, g, N }\n",
        );
        assert!(out.contains("__name(e, \"foo\")"), "{out}");
        assert!(out.contains("\"Bar\")"), "{out}");
        assert!(out.contains("__name(() => {}, \"baz\")"), "{out}");
        assert!(out.contains("\"named\")"), "{out}");
        assert!(out.contains("\"inner\")"), "{out}");
        assert!(out.contains("\"cb\")"), "{out}");
        assert!(!out.contains("\"N\")"), "{out}");
    }

    #[test]
    fn unrenamed_names_are_left_alone() {
        let mut ast = ts_r_parser::parse("function foo() {}\n", SourceKind::Js).ast;
        let scopes = Scopes::analyze(&ast, true);
        let kept = apply(&mut ast, &scopes, "__name", KeepNamesOptions::default());
        assert!(!kept.used && kept.after.is_empty());
    }
}
