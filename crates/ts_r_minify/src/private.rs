//! Short names for private class members (`#field` to `#a`).
//!
//! A `#name` resolves to the nearest enclosing class that declares it; a class's heritage
//! (`extends ...`), decorators and name are outside its private scope. Every class names its
//! own private members, most-used first, avoiding the names its enclosing classes chose (an
//! inner declaration would shadow theirs); sibling classes reuse names. Direct `eval` can
//! name private members, so nothing is renamed when the program contains one.

use rustc_hash::FxHashMap;
use ts_r_ast::{Ast, Atom, Node, NodeId, NodeList};

use crate::names::nth_name;

struct Class {
    parent: Option<usize>,
    /// Declared names with their reference counts (declarations included), in order.
    declared: Vec<(Atom, u32)>,
    renamed: FxHashMap<Atom, Atom>,
}

enum Step {
    Visit(NodeId),
    Enter(usize),
    Leave,
}

/// Renames every private name in `ast`. `has_direct_eval`: the program contains a direct
/// `eval` (then nothing changes).
pub fn mangle(ast: &mut Ast, has_direct_eval: bool) {
    if has_direct_eval {
        return;
    }
    let mut classes: Vec<Class> = Vec::new();
    let mut refs: Vec<(NodeId, usize)> = Vec::new();
    let mut ctx: Vec<usize> = Vec::new();
    let mut stack = vec![Step::Visit(ast.root)];
    let mut kids: Vec<NodeId> = Vec::new();
    while let Some(step) = stack.pop() {
        let id = match step {
            Step::Enter(ci) => {
                ctx.push(ci);
                continue;
            }
            Step::Leave => {
                ctx.pop();
                continue;
            }
            Step::Visit(id) => id,
        };
        match *ast.node(id) {
            Node::ClassDecl {
                decorators,
                name,
                heritage,
                members,
                ..
            }
            | Node::ClassExpr {
                decorators,
                name,
                heritage,
                members,
                ..
            } => {
                let ci = classes.len();
                classes.push(Class {
                    parent: ctx.last().copied(),
                    declared: declared_names(ast, members),
                    renamed: FxHashMap::default(),
                });
                // Members inside the class's private scope; the rest outside it.
                stack.push(Step::Leave);
                stack.extend(ast.list(members).iter().rev().map(|&m| Step::Visit(m)));
                stack.push(Step::Enter(ci));
                stack.extend(ast.list(heritage).iter().map(|&h| Step::Visit(h)));
                stack.extend(name.map(Step::Visit));
                stack.extend(ast.list(decorators).iter().map(|&d| Step::Visit(d)));
            }
            Node::PrivateName { name } => {
                for &ci in ctx.iter().rev() {
                    if let Some(slot) = classes[ci].declared.iter_mut().find(|d| d.0 == name) {
                        slot.1 += 1;
                        refs.push((id, ci));
                        break;
                    }
                }
            }
            _ => {
                kids.clear();
                ast.for_each_child(id, |c| kids.push(c));
                stack.extend(kids.iter().rev().map(|&k| Step::Visit(k)));
            }
        }
    }
    // Parents come before their nested classes.
    for ci in 0..classes.len() {
        let mut outer: Vec<Atom> = Vec::new();
        let mut p = classes[ci].parent;
        while let Some(pi) = p {
            outer.extend(classes[pi].renamed.values().copied());
            p = classes[pi].parent;
        }
        let mut order: Vec<(Atom, u32)> = classes[ci].declared.clone();
        order.sort_by_key(|d| std::cmp::Reverse(d.1));
        let mut cursor = 0usize;
        let mut renamed = FxHashMap::default();
        for (old, _) in order {
            let new = loop {
                let cand = ast.intern(&nth_name(cursor));
                cursor += 1;
                if !outer.contains(&cand) {
                    break cand;
                }
            };
            renamed.insert(old, new);
        }
        classes[ci].renamed = renamed;
    }
    for (id, ci) in refs {
        if let Node::PrivateName { name } = *ast.node(id)
            && let Some(&new) = classes[ci].renamed.get(&name)
        {
            *ast.node_mut(id) = Node::PrivateName { name: new };
        }
    }
}

/// Private names the class body declares (fields, methods, accessors), once each.
fn declared_names(ast: &Ast, members: NodeList) -> Vec<(Atom, u32)> {
    let mut out: Vec<(Atom, u32)> = Vec::new();
    for &m in ast.list(members) {
        let key = match *ast.node(m) {
            Node::PropertyDef { key, .. } | Node::MethodDef { key, .. } => key,
            _ => continue,
        };
        if let Node::PrivateName { name } = *ast.node(key)
            && !out.iter().any(|d| d.0 == name)
        {
            out.push((name, 0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ts_r_ast::SourceKind;
    use ts_r_codegen::{CodegenOptions, Layout};

    fn run(src: &str) -> String {
        let mut ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        super::mangle(&mut ast, false);
        let o = CodegenOptions {
            layout: Layout::Compact,
            ..CodegenOptions::default()
        };
        ts_r_codegen::print(&ast, src, &o).code
    }

    #[test]
    fn renames_by_class_and_scope() {
        assert_eq!(
            run(
                "class A { #long = 1; #other; get #acc() { return this.#long; } set #acc(v) { this.#other = v; } has(o) { return #long in o; } }"
            ),
            "class A{#e=1;#t;get #n(){return this.#e}set #n(v){this.#t=v}has(o){return #e in o}}"
        );
        // The inner class avoids `#e` (the outer `#outer`), which it reads.
        assert_eq!(
            run(
                "class A { #outer = 1; m() { return class B { #inner = 2; f(a) { return a.#outer + this.#inner; } }; } }"
            ),
            "class A{#e=1;m(){return class B{#t=2;f(a){return a.#e+this.#t}}}}"
        );
        // Heritage is outside the class: `#x` there is the outer class's.
        assert_eq!(
            run(
                "class A { #x = 1; m() { return class B extends (this.#x, Object) { #y; #x; }; } }"
            ),
            "class A{#e=1;m(){return class B extends(this.#e,Object){#t;#n}}}"
        );
        // Siblings reuse names.
        assert_eq!(
            run("class A { #first; } class B { #second; }"),
            "class A{#e}class B{#e}"
        );
    }
}
