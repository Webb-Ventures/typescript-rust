//! The per-file context every rule runs in: the checker worker for the file (already run
//! over it, so node types are cached), the file's AST and level, program-wide facts, and the
//! output list. Also the helpers rules share: walking, enclosing functions and the
//! "assignment sites" (an expression and the type its position requires).

use rustc_hash::FxHashSet;
use ts_r_ast::{Ast, FnFlags, Node, NodeId, ParentMap};
use ts_r_checker::checker::{Ck, Loc};
use ts_r_checker::program::ProgramFiles;
use ts_r_diagnostics::FileId;
use ts_r_span::{LineIndex, Span};
use ts_r_types::{TypeFlags, TypeId};

use crate::codes::SoundMessage;
use crate::config::{Level, SoundConfig};
use crate::pragma::Raw;

/// Facts about the whole program, computed once before the per-file passes.
#[derive(Debug)]
pub struct Info<'a> {
    pub program: &'a ProgramFiles,
    pub config: &'a SoundConfig,
    /// Level of every file, by `FileId`.
    pub levels: Vec<Level>,
    /// Ambient module names whose values are `loose` (shorthand `declare module "x";` and
    /// packages configured `loose`).
    pub loose_modules: FxHashSet<String>,
}

impl Info<'_> {
    pub fn level(&self, f: FileId) -> Level {
        self.levels.get(f.0 as usize).copied().unwrap_or_default()
    }

    /// Declared in a file held to the sound rules (`sound` or `contracts`).
    pub fn is_sound_file(&self, f: FileId) -> bool {
        self.level(f) >= Level::Sound
    }

    /// Declared in any file at `strict` or above (part of the "sound set" for S-01: its
    /// written `any` is already an error there).
    pub fn in_sound_set(&self, f: FileId) -> bool {
        self.level(f) >= Level::Strict
    }

    pub fn is_lib(&self, f: FileId) -> bool {
        self.program
            .files
            .get(f.0 as usize)
            .is_some_and(|s| s.is_lib)
    }
}

/// A position that requires a type of an expression.
#[derive(Clone, Copy, Debug)]
pub struct Site {
    /// The type the position requires.
    pub target: TypeId,
    /// Where tsc would report an assignability error.
    pub report: NodeId,
    pub kind: SiteKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SiteKind {
    Variable,
    Assignment,
    Argument { call: NodeId, index: usize },
    Return,
    Field,
}

pub struct Ctx<'r, 'c, 'a> {
    pub ck: &'r mut Ck<'c, 'a>,
    pub info: &'r Info<'r>,
    pub file: FileId,
    pub level: Level,
    pub ast: &'a Ast,
    pub text: &'a str,
    pub parents: &'a ParentMap,
    pub lines: &'a LineIndex,
    /// Every node of the file in source (pre-)order.
    pub nodes: Vec<NodeId>,
    pub out: Vec<Raw>,
}

impl<'r, 'c, 'a> Ctx<'r, 'c, 'a> {
    pub fn new(ck: &'r mut Ck<'c, 'a>, info: &'r Info<'r>, file: FileId) -> Ctx<'r, 'c, 'a> {
        let sf: &'a ts_r_checker::SourceFile = ck.c.file(file);
        let ast = &sf.ast;
        let mut nodes = Vec::with_capacity(ast.node_count());
        let mut stack = vec![ast.root];
        let mut kids = Vec::new();
        while let Some(n) = stack.pop() {
            nodes.push(n);
            kids.clear();
            ast.for_each_child(n, |c| kids.push(c));
            stack.extend(kids.iter().rev());
        }
        Ctx {
            ck,
            info,
            file,
            level: info.level(file),
            ast,
            text: &sf.text,
            parents: sf.parents(),
            lines: sf.lines(),
            nodes,
            out: Vec::new(),
        }
    }

    #[inline]
    pub fn loc(&self, n: NodeId) -> Loc {
        Loc::new(self.file, n)
    }

    #[inline]
    pub fn node(&self, n: NodeId) -> Node {
        *self.ast.node(n)
    }

    #[inline]
    pub fn span(&self, n: NodeId) -> Span {
        self.ast.span(n)
    }

    pub fn src(&self, n: NodeId) -> &'a str {
        self.ast.span(n).text(self.text)
    }

    /// Source text of `n`, shortened to one line of at most 60 characters.
    pub fn short(&self, n: NodeId) -> String {
        let s = self.src(n);
        let line = s.lines().next().unwrap_or("");
        if line.chars().count() > 60 || line.len() < s.len() {
            let mut t: String = line.chars().take(57).collect();
            t.push_str("...");
            t
        } else {
            line.to_owned()
        }
    }

    /// One-based line of a position.
    pub fn line(&self, pos: u32) -> u32 {
        self.lines.line_of(pos) + 1
    }

    pub fn parent(&self, n: NodeId) -> Option<NodeId> {
        self.parents.parent(n)
    }

    /// The parent of `n` after skipping enclosing parentheses, and the child of that parent
    /// on the path to `n`.
    pub fn parent_skip_parens(&self, n: NodeId) -> Option<(NodeId, NodeId)> {
        let mut child = n;
        let mut p = self.parent(n)?;
        while matches!(self.ast.node(p), Node::Paren { .. }) {
            child = p;
            p = self.parent(p)?;
        }
        Some((p, child))
    }

    pub fn skip_parens(&self, n: NodeId) -> NodeId {
        ts_r_checker::nodes::skip_parens(self.ast, n)
    }

    pub fn report(&mut self, msg: SoundMessage, span: Span, args: &[&str]) {
        if !self.level.has_rule(msg.rule()) || !self.info.config.rule_on(msg.rule()) {
            return;
        }
        self.out.push(Raw::new(msg, span, args));
    }

    pub fn has_rule(&self, rule: u8) -> bool {
        self.level.has_rule(rule) && self.info.config.rule_on(rule)
    }

    /// The (cached) type of expression `n`.
    pub fn ty(&mut self, n: NodeId) -> TypeId {
        let l = self.loc(n);
        self.ck.quietly(|ck| ck.check_expr(l))
    }

    pub fn type_node(&mut self, n: NodeId) -> TypeId {
        let l = self.loc(n);
        self.ck.quietly(|ck| ck.type_from_node(l))
    }

    pub fn type_str(&mut self, t: TypeId) -> String {
        self.ck.quietly(|ck| ck.type_to_string(t))
    }

    pub fn flags(&self, t: TypeId) -> TypeFlags {
        self.ck.c.store.flags(t)
    }

    /// The checker's error type, including the named error types of unresolved references.
    pub fn is_error(&self, t: TypeId) -> bool {
        self.ck.c.store.is_error(t)
    }

    pub fn is_assignable(&mut self, s: TypeId, t: TypeId) -> bool {
        self.ck.quietly(|ck| ck.is_assignable(s, t))
    }

    /// The innermost function-like node (function, method value, arrow) containing `n`.
    pub fn enclosing_fn(&self, n: NodeId) -> Option<NodeId> {
        let mut cur = self.parent(n);
        while let Some(p) = cur {
            if matches!(
                self.ast.node(p),
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
            ) {
                return Some(p);
            }
            cur = self.parent(p);
        }
        None
    }

    /// The type a `return` in function `f` must produce: the declared return type, awaited
    /// in async functions. `None` without an annotation, for generators and predicates.
    pub fn return_target(&mut self, f: NodeId) -> Option<TypeId> {
        let parts = ts_r_checker::nodes::fn_parts(self.ast, f)?;
        if parts.flags.contains(FnFlags::GENERATOR) {
            return None;
        }
        let rt = parts.return_type?;
        if matches!(self.ast.node(rt), Node::TypePredicate { .. }) {
            return None;
        }
        let t = self.type_node(rt);
        if parts.flags.contains(FnFlags::ASYNC) {
            Some(self.ck.quietly(|ck| ck.awaited_type(t)))
        } else {
            Some(t)
        }
    }

    /// The site `e` sits in, if its position requires a type.
    pub fn site_of(&mut self, e: NodeId) -> Option<Site> {
        let (p, child) = self.parent_skip_parens(e)?;
        match self.node(p) {
            Node::VarDeclarator {
                name,
                type_ann: Some(t),
                init: Some(i),
                ..
            } if i == child => Some(Site {
                target: self.type_node(t),
                report: name,
                kind: SiteKind::Variable,
            }),
            Node::Assign {
                left,
                op: ts_r_ast::AssignOp::Assign,
                right,
            } if right == child => {
                if matches!(
                    self.node(left),
                    Node::ObjectPattern { .. }
                        | Node::ArrayPattern { .. }
                        | Node::ObjectLit { .. }
                        | Node::ArrayLit { .. }
                ) {
                    return None;
                }
                let l = self.loc(left);
                let target = self.ck.quietly(|ck| ck.assignment_target_type(l));
                Some(Site {
                    target,
                    report: left,
                    kind: SiteKind::Assignment,
                })
            }
            Node::Call { args, .. } | Node::New { args, .. } => {
                let list = self.ast.list(args);
                let index = list.iter().position(|&a| a == child)?;
                if list[..=index]
                    .iter()
                    .any(|&a| matches!(self.ast.node(a), Node::Spread { .. }))
                {
                    return None;
                }
                let l = self.loc(p);
                let sig = self.ck.resolved_signature(l)?;
                let target = self.ck.quietly(|ck| ck.param_type_at(sig, index))?;
                Some(Site {
                    target,
                    report: child,
                    kind: SiteKind::Argument { call: p, index },
                })
            }
            Node::Return { arg: Some(a) } if a == child => {
                let f = self.enclosing_fn(p)?;
                let target = self.return_target(f)?;
                Some(Site {
                    target,
                    report: child,
                    kind: SiteKind::Return,
                })
            }
            Node::Arrow { flags, body, .. }
                if body == child && flags.contains(FnFlags::EXPRESSION_BODY) =>
            {
                let target = self.return_target(p)?;
                Some(Site {
                    target,
                    report: child,
                    kind: SiteKind::Return,
                })
            }
            Node::PropertyDef {
                key,
                type_ann: Some(t),
                value: Some(v),
                ..
            } if v == child => Some(Site {
                target: self.type_node(t),
                report: key,
                kind: SiteKind::Field,
            }),
            _ => None,
        }
    }

    /// The declaration of a signature (following instantiations to the original).
    pub fn sig_decl(&self, mut s: ts_r_types::SigId) -> Option<Loc> {
        let store = self.ck.store();
        for _ in 0..16 {
            let sig = store.sig(s);
            if let Some((f, n)) = sig.decl {
                let node = self.ck.node_id(n)?;
                return Some(Loc::new(FileId(f), node));
            }
            s = sig.target?.0;
        }
        None
    }

    /// Every expression of the file in a position that requires a type, with its site.
    pub fn sites(&mut self) -> Vec<(NodeId, Site)> {
        let mut cands = Vec::new();
        for &n in &self.nodes {
            match *self.ast.node(n) {
                Node::VarDeclarator {
                    type_ann: Some(_),
                    init: Some(i),
                    ..
                } => cands.push(i),
                Node::Assign {
                    op: ts_r_ast::AssignOp::Assign,
                    right,
                    ..
                } => cands.push(right),
                Node::Call { args, .. } | Node::New { args, .. } => {
                    cands.extend(self.ast.list(args).iter().copied());
                }
                Node::Return { arg: Some(a) } => cands.push(a),
                Node::Arrow { flags, body, .. } if flags.contains(FnFlags::EXPRESSION_BODY) => {
                    cands.push(body);
                }
                Node::PropertyDef {
                    type_ann: Some(_),
                    value: Some(v),
                    ..
                } => cands.push(v),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for e in cands {
            if let Some(s) = self.site_of(e) {
                out.push((e, s));
            }
        }
        out
    }

    /// The text of a property key (`a`, `"a"`, `#a`).
    pub fn key_text(&self, key: NodeId) -> Option<String> {
        ts_r_checker::nodes::key_text(self.ast, key)
    }
}
