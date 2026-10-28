//! `unicorn/prefer-import-meta-properties`: `import.meta.dirname` / `import.meta.filename`
//! instead of computing them from `import.meta.url`.
//!
//! Reported (each fixed to `import.meta.<name>` unless a comment is inside):
//! - `fileURLToPath(import.meta.url)` and `fileURLToPath(new URL(import.meta.url))` (the
//!   filename), and `path.dirname(...)` of one of those or of `import.meta.filename` (the
//!   dirname); a `const` initialised with a filename also reports `path.dirname(<it>)` at
//!   every read;
//! - `fileURLToPath(new URL(".", import.meta.url))` (also `"./"`): the dirname.
//!
//! `fileURLToPath` and `dirname` must come from `url`/`node:url` and `path`/`node:path`:
//! a named import, a member of a default or namespace import, of `process.getBuiltinModule
//! ("<module>")`, or of a `const` (or `const {fn}` / `{fn: alias}` destructuring) of one of
//! those. Calls are plain (no `?.`) with exactly one argument.

use super::prelude::*;
use super::unicorn_text::static_string_value;
use ts_r_ast::MetaPropKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/prefer-import-meta-properties",
        "Prefer `import.meta.{dirname,filename}` over legacy techniques for getting file paths.",
        NO_PRESET,
        Category::Style,
    )
    .fixable(),
    create: create_default::<PreferImportMetaProperties>,
};

#[derive(Debug, Default)]
struct PreferImportMetaProperties;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// The expression is the function itself.
    Property,
    /// The expression is the module object.
    Module,
}

struct Builtin {
    modules: [&'static str; 2],
    function: &'static str,
}

const URL_FILE_URL_TO_PATH: Builtin = Builtin {
    modules: ["url", "node:url"],
    function: "fileURLToPath",
};
const PATH_DIRNAME: Builtin = Builtin {
    modules: ["path", "node:path"],
    function: "dirname",
};

impl Builtin {
    fn is_module_literal(&self, cx: &Ctx<'_, '_>, id: NodeId) -> bool {
        static_string_value(cx.ast(), id).is_some_and(|v| self.modules.contains(&v.as_str()))
    }

    /// `id` is a call of this function with one argument: that argument.
    fn call_arg(&self, cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
        let ast = cx.ast();
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(id)
        else {
            return None;
        };
        let &[arg] = ast.list(args) else {
            return None;
        };
        if matches!(ast.node(arg), Node::Spread { .. }) {
            return None;
        }
        self.expr(cx, callee, Kind::Property, 0)
            .then(|| skip_parens(ast, arg))
    }

    fn expr(&self, cx: &Ctx<'_, '_>, id: NodeId, kind: Kind, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        let ast = cx.ast();
        let id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::Member {
                obj,
                optional: false,
                prop,
            } => {
                kind == Kind::Property
                    && matches!(ast.node(prop), Node::Ident { .. })
                    && ident_name(ast, prop) == Some(self.function)
                    && self.expr(cx, obj, Kind::Module, depth + 1)
            }
            Node::Member { .. } | Node::Index { .. } => false,
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } => {
                if kind != Kind::Module {
                    return false;
                }
                let Node::Member {
                    obj,
                    optional: false,
                    prop,
                } = *ast.node(skip_parens(ast, callee))
                else {
                    return false;
                };
                let &[arg] = ast.list(args) else {
                    return false;
                };
                matches!(ast.node(prop), Node::Ident { .. })
                    && ident_name(ast, prop) == Some("getBuiltinModule")
                    && matches!(ast.node(skip_parens(ast, obj)), Node::Ident { .. })
                    && ident_name(ast, skip_parens(ast, obj)) == Some("process")
                    && self.is_module_literal(cx, arg)
            }
            Node::Ident { .. } => self.ident(cx, id, kind, depth),
            _ => false,
        }
    }

    fn ident(&self, cx: &Ctx<'_, '_>, id: NodeId, kind: Kind, depth: u32) -> bool {
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(sym) = sem.reference_at(id).and_then(|r| sem.reference(r).symbol()) else {
            return false;
        };
        let [d] = sem.symbol(sym).decls[..] else {
            return false;
        };
        match d.kind {
            DeclKind::ImportDefault | DeclKind::ImportNamespace | DeclKind::ImportNamed => {
                let Node::ImportDecl { source, .. } = *ast.node(d.parent) else {
                    return false;
                };
                if !self.is_module_literal(cx, source) {
                    return false;
                }
                match (kind, *ast.node(d.node)) {
                    (
                        Kind::Module,
                        Node::ImportDefaultSpecifier { .. } | Node::ImportNamespaceSpecifier { .. },
                    ) => true,
                    (
                        Kind::Property,
                        Node::ImportSpecifier {
                            imported, local, ..
                        },
                    ) => {
                        let n = imported.unwrap_or(local);
                        matches!(ast.node(n), Node::Ident { .. })
                            && ident_name(ast, n) == Some(self.function)
                    }
                    _ => false,
                }
            }
            DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                self.pattern(cx, d.name, kind, depth + 1)
            }
            _ => false,
        }
    }

    /// The binding `node` (an identifier or object pattern) of a `const`.
    fn pattern(&self, cx: &Ctx<'_, '_>, node: NodeId, kind: Kind, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        let ast = cx.ast();
        let Some(parent) = cx.parent(node) else {
            return false;
        };
        match *ast.node(parent) {
            Node::VarDeclarator {
                name,
                init: Some(init),
                ..
            } if name == node => {
                cx.parent(parent).is_some_and(|d| {
                    matches!(
                        ast.node(d),
                        Node::VarDecl {
                            kind: VarKind::Const,
                            ..
                        }
                    )
                }) && self.expr(cx, init, kind, depth + 1)
            }
            Node::BindingProperty {
                key,
                value,
                init: None,
            } => {
                kind == Kind::Property
                    && value.unwrap_or(key) == node
                    && matches!(ast.node(key), Node::Ident { .. })
                    && ident_name(ast, key) == Some(self.function)
                    && cx.parent(parent).is_some_and(|p| {
                        matches!(ast.node(p), Node::ObjectPattern { .. })
                            && self.pattern(cx, p, Kind::Module, depth + 1)
                    })
            }
            _ => false,
        }
    }
}

fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    parent_skip_parens(cx.ast(), cx.file.parents, id)
}

/// `new URL(...)` with 1 or 2 non-spread arguments: the arguments.
fn new_url_args<'a>(cx: &Ctx<'_, 'a>, id: NodeId) -> Option<&'a [NodeId]> {
    let ast = cx.ast();
    let Node::New { callee, args, .. } = *ast.node(id) else {
        return None;
    };
    let args = ast.list(args);
    (matches!(ast.node(callee), Node::Ident { .. })
        && ident_name(ast, callee) == Some("URL")
        && (1..=2).contains(&args.len())
        && !args
            .iter()
            .any(|&a| matches!(ast.node(a), Node::Spread { .. })))
    .then_some(args)
}

/// `(node, is_dirname)` problems, in the order found.
type Problems = Vec<(NodeId, bool)>;

/// The problems a filename expression `node` leads to.
fn from_filename(cx: &Ctx<'_, '_>, node: NodeId, report_filename: bool, out: &mut Problems) {
    let ast = cx.ast();
    let Some(parent) = parent_np(cx, node) else {
        return;
    };
    if PATH_DIRNAME.call_arg(cx, parent) == Some(node) {
        out.push((parent, true));
        return;
    }
    if report_filename {
        out.push((node, false));
    }
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(parent)
    else {
        return;
    };
    if skip_parens(ast, init) != node
        || !matches!(ast.node(name), Node::Ident { .. })
        || !cx.parent(parent).is_some_and(|d| {
            matches!(
                ast.node(d),
                Node::VarDecl {
                    kind: VarKind::Const,
                    ..
                }
            )
        })
    {
        return;
    }
    let sem = cx.semantic();
    let Some(sym) = sem.symbol_of_decl(name) else {
        return;
    };
    for &r in &sem.symbol(sym).refs {
        let r = sem.reference(r);
        if !r.is_read() || r.is_write() {
            continue;
        }
        if let Some(p) = parent_np(cx, r.node)
            && PATH_DIRNAME.call_arg(cx, p) == Some(r.node)
        {
            out.push((p, true));
        }
    }
}

impl Rule for PreferImportMetaProperties {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::MetaProperty]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        if !matches!(
            ast.node(node),
            Node::MetaProperty {
                kind: MetaPropKind::ImportMeta
            }
        ) {
            return;
        }
        let Some(member) = parent_np(cx, node) else {
            return;
        };
        let Node::Member {
            optional: false,
            prop,
            ..
        } = *ast.node(member)
        else {
            return;
        };
        if !matches!(ast.node(prop), Node::Ident { .. }) {
            return;
        }
        let mut problems = Problems::new();
        match ident_name(ast, prop) {
            Some("url") => {
                let Some(parent) = parent_np(cx, member) else {
                    return;
                };
                if URL_FILE_URL_TO_PATH.call_arg(cx, parent) == Some(member) {
                    from_filename(cx, parent, true, &mut problems);
                } else if let Some(args) = new_url_args(cx, parent) {
                    let url_parent = parent_np(cx, parent);
                    let converted = url_parent
                        .filter(|&p| URL_FILE_URL_TO_PATH.call_arg(cx, p) == Some(parent));
                    if let Some(p) = converted {
                        if args.len() == 1 && skip_parens(ast, args[0]) == member {
                            from_filename(cx, p, true, &mut problems);
                        } else if args.len() == 2
                            && skip_parens(ast, args[1]) == member
                            && static_string_value(ast, args[0])
                                .is_some_and(|v| v == "." || v == "./")
                        {
                            problems.push((p, true));
                        }
                    }
                }
            }
            Some("filename") => from_filename(cx, member, false, &mut problems),
            _ => return,
        }
        for (node, dirname) in problems {
            let span = cx.span(node);
            let fix = ast.comments_in(span).is_empty().then(|| {
                Fix::replace(
                    span,
                    if dirname {
                        "import.meta.dirname"
                    } else {
                        "import.meta.filename"
                    },
                )
            });
            let (id, msg) = if dirname {
                ("error/calculate-dirname", "Do not construct dirname.")
            } else {
                (
                    "error/calculate-filename",
                    "Do not construct filename using `fileURLToPath()`.",
                )
            };
            cx.report(span, msg).id(id).fix_opt(fix);
        }
    }
}
