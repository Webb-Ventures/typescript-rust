//! `one-var`: one declaration statement per scope (`always`), one per declarator (`never`),
//! or no adjacent statements of the same kind (`consecutive`), with per-kind and
//! initialized/uninitialized settings.

use super::prelude::*;
use rustc_hash::FxHashMap;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "one-var",
        "Enforce variables to be declared either together or separately in functions",
        NO_PRESET,
        Category::Style,
    ),
    create: create::<OneVar>,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Always,
    Never,
    Consecutive,
}

#[derive(Debug, Clone, Copy, Default)]
struct KindOpt {
    uninit: Option<Mode>,
    init: Option<Mode>,
}

#[derive(Debug)]
pub(crate) struct OneVar {
    /// Indexed by `kind_index`.
    kinds: [KindOpt; 5],
    separate_requires: bool,
}

fn mode(v: Option<&Value>) -> Option<Mode> {
    match v?.as_str()? {
        "always" => Some(Mode::Always),
        "never" => Some(Mode::Never),
        "consecutive" => Some(Mode::Consecutive),
        _ => None,
    }
}

impl FromOptions for OneVar {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let mut kinds = [KindOpt::default(); 5];
        let mut separate_requires = false;
        match opts.first() {
            Some(Value::Object(o)) => {
                separate_requires = bool_field(o, "separateRequires", false);
                for (i, k) in ["var", "let", "const", "using", "awaitUsing"]
                    .iter()
                    .enumerate()
                {
                    let m = mode(o.get(*k));
                    kinds[i] = KindOpt { uninit: m, init: m };
                    if o.contains_key("uninitialized") {
                        kinds[i].uninit = mode(o.get("uninitialized"));
                    }
                    if o.contains_key("initialized") {
                        kinds[i].init = mode(o.get("initialized"));
                    }
                }
            }
            v => {
                let m = mode(v).or(Some(Mode::Always));
                kinds = [KindOpt { uninit: m, init: m }; 5];
            }
        }
        Ok(OneVar {
            kinds,
            separate_requires,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct State {
    init: bool,
    uninit: bool,
    required: bool,
}

fn kind_index(k: VarKind) -> usize {
    match k {
        VarKind::Var => 0,
        VarKind::Let => 1,
        VarKind::Const => 2,
        VarKind::Using => 3,
        VarKind::AwaitUsing => 4,
    }
}

fn kind_text(k: VarKind) -> &'static str {
    ["var", "let", "const", "using", "await using"][kind_index(k)]
}

fn is_require(cx: &Ctx<'_, '_>, d: NodeId) -> bool {
    let ast = cx.ast();
    matches!(*cx.node(d), Node::VarDeclarator { init: Some(i), .. }
        if matches!(*cx.node(skip_parens(ast, i)), Node::Call { callee, .. }
            if ident_name(ast, skip_parens(ast, callee)) == Some("require")))
}

fn has_init(cx: &Ctx<'_, '_>, d: NodeId) -> bool {
    matches!(*cx.node(d), Node::VarDeclarator { init: Some(_), .. })
}

/// The statement list holding `n`: ESTree's `body` array (Program, block, static block,
/// namespace body), or `None`.
fn body_of<'a>(cx: &Ctx<'a, '_>, p: NodeId) -> Option<&'a [NodeId]> {
    let ast = cx.ast();
    match *cx.node(p) {
        Node::Program { stmts } | Node::Block { stmts } | Node::ModuleBlock { stmts } => {
            Some(ast.list(stmts))
        }
        Node::StaticBlock { body } => Some(ast.list(body)),
        _ => None,
    }
}

/// ESTree's start of a declaration: after `export`.
fn decl_start(cx: &Ctx<'_, '_>, node: NodeId) -> u32 {
    let span = cx.ast().span(node);
    let src = cx.src();
    let mut at = span.start as usize;
    if let Node::VarDecl { modifiers, .. } = *cx.node(node)
        && modifiers.contains(Modifiers::EXPORT)
        && src.get(at..).is_some_and(|r| r.starts_with("export"))
    {
        at = skip_trivia_forward(src, at + "export".len());
    }
    at as u32
}

impl OneVar {
    /// `hasOnlyOneStatement`: records this statement in its scope and says whether it is the
    /// first one the `always` settings allow.
    fn first_in_scope(
        &self,
        cx: &Ctx<'_, '_>,
        o: KindOpt,
        decls: &[NodeId],
        st: &mut State,
    ) -> bool {
        let uninit = decls.iter().any(|&d| !has_init(cx, d));
        let init = decls.iter().any(|&d| has_init(cx, d));
        let requires = decls.iter().any(|&d| is_require(cx, d));
        let always = |m: Option<Mode>| m == Some(Mode::Always);
        if always(o.uninit) && always(o.init) && (st.uninit || st.init) && !requires {
            return false;
        }
        if uninit && always(o.uninit) && st.uninit {
            return false;
        }
        if init && always(o.init) && st.init && !requires {
            return false;
        }
        if st.required && requires {
            return false;
        }
        for &d in decls {
            if !has_init(cx, d) {
                st.uninit |= always(o.uninit);
            } else if always(o.init) {
                if self.separate_requires && is_require(cx, d) {
                    st.required = true;
                } else {
                    st.init = true;
                }
            }
        }
        true
    }

    /// Joins the statement into the previous one of the same kind.
    fn join_fix(&self, cx: &Ctx<'_, '_>, node: NodeId, kind: VarKind) -> Option<Fix> {
        let ast = cx.ast();
        let p = cx.parent(node)?;
        let body = body_of(cx, p)?;
        let i = body.iter().position(|&s| s == node)?;
        let prev = *body.get(i.checked_sub(1)?)?;
        if !matches!(*cx.node(prev), Node::VarDecl { kind: k, modifiers, .. } if k == kind && !modifiers.contains(Modifiers::EXPORT))
        {
            return None;
        }
        let src = cx.src();
        let start = ast.span(node).start;
        let before = skip_trivia_backward(ast, src, start as usize) as u32;
        let mut fix = if src.as_bytes().get(before as usize - 1) == Some(&b';') {
            Fix::replace(Span::new(before - 1, before), ",")
        } else {
            Fix::insert(before, ",")
        };
        let kw_end = if kind == VarKind::AwaitUsing {
            let using = skip_trivia_forward(src, start as usize + "await".len());
            fix = fix.and(Span::new(start, start + "await".len() as u32), "");
            fix = fix.and(
                Span::new(using as u32, using as u32 + "using".len() as u32),
                "",
            );
            return Some(fix);
        } else {
            start + kind_text(kind).len() as u32
        };
        Some(fix.and(Span::new(start, kw_end), ""))
    }

    /// Splits the statement into one per declarator.
    fn split_fix(
        &self,
        cx: &Ctx<'_, '_>,
        node: NodeId,
        kind: VarKind,
        decls: &[NodeId],
    ) -> Option<Fix> {
        let ast = cx.ast();
        let src = cx.src();
        let p = cx.parent(node)?;
        if !matches!(
            cx.node(p),
            Node::Program { .. }
                | Node::Block { .. }
                | Node::StaticBlock { .. }
                | Node::SwitchCase { .. }
        ) {
            return None;
        }
        let export = match *cx.node(node) {
            Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT) => "export ",
            _ => "",
        };
        let kw = kind_text(kind);
        let mut fix: Option<Fix> = None;
        for &d in decls {
            let comma = skip_trivia_forward(src, ast.span(d).end as usize);
            if src.as_bytes().get(comma) != Some(&b',') {
                continue;
            }
            let after = comma + 1;
            let next = skip_trivia_forward(src, after);
            let comment_next = ast
                .comments
                .iter()
                .any(|c| c.span.start as usize >= after && (c.span.start as usize) < next);
            let (span, text) = if next == after {
                (
                    Span::new(comma as u32, after as u32),
                    format!("; {export}{kw} "),
                )
            } else if comment_next || has_newline(src, after as u32, next as u32) {
                let between = src.get(after..next).unwrap_or("");
                (
                    Span::new(comma as u32, next as u32),
                    format!(";{between}{export}{kw} "),
                )
            } else {
                (
                    Span::new(comma as u32, after as u32),
                    format!("; {export}{kw}"),
                )
            };
            fix = Some(match fix {
                None => Fix::replace(span, text),
                Some(f) => f.and(span, text),
            });
        }
        fix
    }
}

impl Rule for OneVar {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut decls_in_order: Vec<NodeId> = (1..=ast.node_count() as u32)
            .filter_map(NodeId::from_raw)
            .filter(|&id| matches!(cx.node(id), Node::VarDecl { .. }))
            .collect();
        decls_in_order.sort_by_key(|&id| ast.span(id).start);
        let mut states: FxHashMap<(NodeId, usize), State> = FxHashMap::default();
        for node in decls_in_order {
            let Node::VarDecl { kind, decls, .. } = *cx.node(node) else {
                continue;
            };
            let ki = kind_index(kind);
            let o = self.kinds[ki];
            let decls = ast.list(decls);
            let kw = kind_text(kind);
            let start = decl_start(cx, node);
            let span = Span::new(start, ast.span(node).end);
            let always = |m: Option<Mode>| m == Some(Mode::Always);
            let n_init = decls.iter().filter(|&&d| has_init(cx, d)).count();
            let n_uninit = decls.len() - n_init;
            let requires = decls.iter().filter(|&&d| is_require(cx, d)).count();
            let mut out: Vec<(&'static str, String, Option<Fix>)> = Vec::new();
            if always(o.init) && self.separate_requires && requires > 0 && requires < decls.len() {
                out.push((
                    "splitRequires",
                    "Split requires to be separated into a single block.".into(),
                    None,
                ));
            }
            let parent = cx.parent(node);
            let exported = matches!(*cx.node(node), Node::VarDecl { modifiers, .. } if modifiers.contains(Modifiers::EXPORT));
            // The previous statement in the same list (none for an exported declaration).
            let prev = parent
                .filter(|_| !exported)
                .and_then(|p| body_of(cx, p))
                .and_then(|b| {
                    let i = b.iter().position(|&s| s == node)?;
                    b.get(i.checked_sub(1)?).copied()
                });
            if let Some(prev) = prev
                && let Node::VarDecl {
                    kind: pk,
                    decls: pd,
                    modifiers: pm,
                } = *cx.node(prev)
                && pk == kind
                && !pm.contains(Modifiers::EXPORT)
            {
                let pd = ast.list(pd);
                let all = decls.iter().chain(pd);
                let any_req = all.clone().any(|&d| is_require(cx, d));
                let every_req = all.clone().all(|&d| is_require(cx, d));
                if !any_req || every_req {
                    let p_init = pd.iter().filter(|&&d| has_init(cx, d)).count();
                    let p_uninit = pd.len() - p_init;
                    let c = Some(Mode::Consecutive);
                    let id = if o.init == c && o.uninit == c {
                        Some((
                            "combine",
                            format!("Combine this with the previous '{kw}' statement."),
                        ))
                    } else if o.init == c && n_init > 0 && p_init > 0 {
                        Some((
                            "combineInitialized",
                            format!(
                                "Combine this with the previous '{kw}' statement with initialized variables."
                            ),
                        ))
                    } else if o.uninit == c && n_uninit > 0 && p_uninit > 0 {
                        Some((
                            "combineUninitialized",
                            format!(
                                "Combine this with the previous '{kw}' statement with uninitialized variables."
                            ),
                        ))
                    } else {
                        None
                    };
                    if let Some((id, msg)) = id {
                        out.push((id, msg, self.join_fix(cx, node, kind)));
                    }
                }
            }
            // The scope `var` (function) or the other kinds (block) belong to.
            let container = ancestors_inclusive(cx.file.parents, node)
                .skip(1)
                .find(|&a| match cx.node(a) {
                    Node::Program { .. }
                    | Node::FunctionDecl { .. }
                    | Node::FunctionExpr { .. }
                    | Node::Arrow { .. }
                    | Node::StaticBlock { .. } => true,
                    Node::Block { .. }
                    | Node::For { .. }
                    | Node::ForIn { .. }
                    | Node::ForOf { .. }
                    | Node::Switch { .. } => kind != VarKind::Var,
                    _ => false,
                })
                .unwrap_or(ast.root);
            let st = states.entry((container, ki)).or_default();
            if !self.first_in_scope(cx, o, decls, st) {
                if always(o.init) && always(o.uninit) {
                    out.push((
                        "combine",
                        format!("Combine this with the previous '{kw}' statement."),
                        self.join_fix(cx, node, kind),
                    ));
                } else {
                    if always(o.init) && n_init > 0 {
                        out.push(("combineInitialized", format!("Combine this with the previous '{kw}' statement with initialized variables."), self.join_fix(cx, node, kind)));
                    }
                    let for_in_head = parent.is_some_and(|p| matches!(*cx.node(p), Node::ForIn { left, .. } | Node::ForOf { left, .. } if left == node));
                    if always(o.uninit) && n_uninit > 0 && !for_in_head {
                        out.push(("combineUninitialized", format!("Combine this with the previous '{kw}' statement with uninitialized variables."), self.join_fix(cx, node, kind)));
                    }
                }
            }
            let for_init = parent.is_some_and(
                |p| matches!(*cx.node(p), Node::For { init: Some(i), .. } if i == node),
            );
            if !for_init && decls.len() > 1 {
                let nv = Some(Mode::Never);
                let id = if o.init == nv && o.uninit == nv {
                    Some((
                        "split",
                        format!("Split '{kw}' declarations into multiple statements."),
                    ))
                } else if o.init == nv && n_init > 0 {
                    Some((
                        "splitInitialized",
                        format!("Split initialized '{kw}' declarations into multiple statements."),
                    ))
                } else if o.uninit == nv && n_uninit > 0 {
                    Some((
                        "splitUninitialized",
                        format!(
                            "Split uninitialized '{kw}' declarations into multiple statements."
                        ),
                    ))
                } else {
                    None
                };
                if let Some((id, msg)) = id {
                    out.push((id, msg, self.split_fix(cx, node, kind, decls)));
                }
            }
            for (id, msg, fix) in out {
                cx.report(span, msg).id(id).fix_opt(fix);
            }
        }
    }
}
