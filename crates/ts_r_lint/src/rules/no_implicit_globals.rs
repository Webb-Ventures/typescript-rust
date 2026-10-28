//! `no-implicit-globals`: declarations in a script's global scope, assignments to
//! undeclared variables in sloppy code, and writes to read-only globals.

use super::prelude::*;
use super::this_binding::is_strict;
use crate::env::{GlobalAccess, SourceType};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "no-implicit-globals",
        "Disallow declarations in the global scope",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create::<NoImplicitGlobals>,
};

#[derive(Debug)]
struct NoImplicitGlobals {
    lexical_bindings: bool,
}

impl FromOptions for NoImplicitGlobals {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        Ok(NoImplicitGlobals {
            lexical_bindings: bool_field(obj_opt(opts, 0), "lexicalBindings", false),
        })
    }
}

/// The assignment (or `for-in/of`) a written identifier belongs to.
fn assignment_of(cx: &Ctx<'_, '_>, id: NodeId) -> NodeId {
    let ast = cx.ast();
    ancestors_inclusive(cx.file.parents, id)
        .skip(1)
        .find(|&a| match *cx.node(a) {
            // A default inside a destructuring pattern is not an assignment.
            Node::Assign { .. } => !is_assignment_target(ast, cx.file.parents, a),
            Node::ForIn { .. } | Node::ForOf { .. } => true,
            _ => false,
        })
        .unwrap_or(id)
}

impl Rule for NoImplicitGlobals {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let env = cx.env();
        let access = |name: &str| -> Option<GlobalAccess> {
            match cx
                .file
                .comment_globals
                .iter()
                .rev()
                .find(|g| !g.off && g.name == name)
            {
                Some(g) if g.writable => Some(GlobalAccess::Writable),
                Some(_) => Some(GlobalAccess::Readonly),
                None => env.globals.get(name),
            }
        };
        let mut out: Vec<(Span, &'static str, String)> = Vec::new();
        let global = sem.global_scope();
        let script = env.source_type == SourceType::Script && !env.global_return;
        for s in sem.symbols() {
            if s.scope != global {
                continue;
            }
            let name = ast.atom(s.name);
            let acc = access(name);
            if acc == Some(GlobalAccess::Writable) || cx.file.exported.iter().any(|e| e == name) {
                continue;
            }
            let readonly = acc == Some(GlobalAccess::Readonly);
            for d in s.decls.iter().filter(|_| script) {
                let kind = match d.kind {
                    DeclKind::Function => "function".to_owned(),
                    DeclKind::Var => "'var'".to_owned(),
                    DeclKind::Class if self.lexical_bindings => "class".to_owned(),
                    DeclKind::Let if self.lexical_bindings => "'let'".to_owned(),
                    DeclKind::Const if self.lexical_bindings => "'const'".to_owned(),
                    _ => continue,
                };
                let span = ast.span(d.node);
                if readonly {
                    out.push((
                        span,
                        "redeclarationOfReadonlyGlobal",
                        "Unexpected redeclaration of read-only global variable.".into(),
                    ));
                } else if matches!(d.kind, DeclKind::Function | DeclKind::Var) {
                    out.push((span, "globalNonLexicalBinding", format!("Unexpected {kind} declaration in the global scope, wrap in an IIFE for a local variable, assign as global property for a global variable.")));
                } else {
                    out.push((span, "globalLexicalBinding", format!("Unexpected {kind} declaration in the global scope, wrap in a block or in an IIFE.")));
                }
            }
        }
        for r in sem.references() {
            if !r.flags.contains(RefFlags::WRITE)
                || r.flags.intersects(
                    RefFlags::READ | RefFlags::COMPOUND | RefFlags::INIT | RefFlags::IMPLICIT,
                )
            {
                continue;
            }
            let name = ast.atom(r.name);
            let readonly_global = match r.resolution {
                Resolution::Global => access(name) == Some(GlobalAccess::Readonly),
                Resolution::Symbol(s) => {
                    let s = sem.symbol(s);
                    s.flags.contains(SymbolFlags::COMMENT_GLOBAL)
                        && s.decls.iter().all(|d| d.kind == DeclKind::CommentGlobal)
                        && access(name) == Some(GlobalAccess::Readonly)
                }
                Resolution::Unresolved => false,
            };
            if readonly_global && !cx.file.exported.iter().any(|e| e == name) {
                let span = ast.span(assignment_of(cx, r.node));
                out.push((
                    span,
                    "assignmentToReadonlyGlobal",
                    "Unexpected assignment to read-only global variable.".into(),
                ));
            } else if r.resolution == Resolution::Unresolved
                && access(name).is_none()
                && !is_strict(cx, r.node)
            {
                let span = ast.span(assignment_of(cx, r.node));
                out.push((
                    span,
                    "globalVariableLeak",
                    "Global variable leak, declare the variable if it is intended to be local."
                        .into(),
                ));
            }
        }
        out.sort_by_key(|(s, ..)| s.start);
        for (span, id, msg) in out {
            cx.report(span, msg).id(id);
        }
    }
}
