//! `@typescript-eslint/no-unused-private-class-members`: `#private` and `private` class
//! members (and `private` parameter properties) that are never read.
//!
//! One walk keeps a stack of `this` contexts: a class, its methods, field initializers and
//! static blocks bind `this` to the class; other functions bind nothing unless their `this`
//! parameter names an enclosing class. Reads count through `this.x`, `this.#x`, `#x in o`,
//! `ClassName.x` (statics), aliases `const self = this`, parameters typed as an enclosing
//! class (`thing: Foo`, `typeof Foo`), and destructuring `this`.

use rustc_hash::FxHashMap;
use ts_r_ast::visit::{Visitor, Walk, walk};

use super::prelude::*;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "@typescript-eslint/no-unused-private-class-members",
        "Disallow unused private class members",
        NO_PRESET,
        Category::Correctness,
    ),
    create: create_default::<NoUnusedPrivateClassMembers>,
};

#[derive(Debug, Default)]
struct NoUnusedPrivateClassMembers;

/// A member key: `(true, name)` for `#name`, `(false, name)` otherwise.
type Key = (bool, String);

struct Member {
    name_span: Span,
    name: String,
    reportable: bool,
    accessor: bool,
    reads: u32,
    writes: u32,
}

impl Member {
    fn new(name_span: Span, name: String, reportable: bool, accessor: bool) -> Member {
        Member {
            name_span,
            name,
            reportable,
            accessor,
            reads: 0,
            writes: 0,
        }
    }
}

struct Class<'a> {
    name: Option<&'a str>,
    instance: FxHashMap<Key, usize>,
    statics: FxHashMap<Key, usize>,
}

#[derive(Clone, Copy)]
struct Frame {
    this_ctx: Option<usize>,
    is_static: bool,
    /// Set for a class's own frame.
    class: Option<usize>,
}

struct Analyzer<'c, 'a, 'f> {
    cx: &'c Ctx<'a, 'f>,
    classes: Vec<Class<'a>>,
    members: Vec<Member>,
    frames: Vec<Frame>,
}

/// The key a literal or simple template names (`['a']`, `[1]`, `` [`a`] ``).
fn literal_key(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::NullLit {} => Some("null".to_owned()),
        Node::BoolLit { value } => Some(value.to_string()),
        Node::TemplateLit { parts } => {
            let [el] = ast.list(parts) else { return None };
            let raw = cx.span(*el).text(cx.src());
            Some(raw.trim_start_matches('`').trim_end_matches('`').to_owned())
        }
        _ => None,
    }
}

impl<'c, 'a, 'f> Analyzer<'c, 'a, 'f> {
    fn ast(&self) -> &'a ts_r_ast::Ast {
        self.cx.ast()
    }

    /// The key, written name and name span of a class member key.
    fn member_name(&self, key: NodeId) -> Option<(Key, String, Span)> {
        let ast = self.ast();
        match *ast.node(key) {
            Node::Ident { name } => {
                let n = ast.atom(name).to_owned();
                Some(((false, n.clone()), n, self.cx.span(key)))
            }
            Node::PrivateName { name } => {
                let n = ast.atom(name).to_owned();
                Some(((true, n.clone()), format!("#{n}"), self.cx.span(key)))
            }
            Node::ComputedKey { expr } => {
                let e = skip_parens(ast, expr);
                let n = literal_key(self.cx, e)?;
                Some(((false, n.clone()), n, self.cx.span(e)))
            }
            _ => {
                let n = literal_key(self.cx, key)?;
                Some(((false, n.clone()), n, self.cx.span(key)))
            }
        }
    }

    fn add_member(&mut self, class: usize, is_static: bool, key: Key, member: Member) {
        let i = self.members.len();
        self.members.push(member);
        let c = &mut self.classes[class];
        if is_static {
            c.statics.insert(key, i);
        } else {
            c.instance.insert(key, i);
        }
    }

    fn enter_class(&mut self, name: Option<NodeId>, members: ts_r_ast::NodeList) {
        let ast = self.ast();
        let idx = self.classes.len();
        self.classes.push(Class {
            name: name.and_then(|n| ident_name(ast, n)),
            instance: FxHashMap::default(),
            statics: FxHashMap::default(),
        });
        for &m in ast.list(members) {
            match *ast.node(m) {
                Node::MethodDef {
                    kind: MethodKind::Constructor,
                    value,
                    ..
                } => {
                    let Node::FunctionExpr { params, .. } = *ast.node(value) else {
                        continue;
                    };
                    for &p in ast.list(params) {
                        let Node::Param {
                            modifiers,
                            pattern,
                            type_ann,
                            ..
                        } = *ast.node(p)
                        else {
                            continue;
                        };
                        if !modifiers.intersects(
                            Modifiers::PUBLIC
                                | Modifiers::PRIVATE
                                | Modifiers::PROTECTED
                                | Modifiers::READONLY
                                | Modifiers::OVERRIDE,
                        ) {
                            continue;
                        }
                        let Some(n) = ident_name(ast, pattern) else {
                            continue;
                        };
                        let ps = self.cx.span(pattern);
                        let end = type_ann.map_or(ps.end, |t| self.cx.span(t).end.max(ps.end));
                        let member = Member::new(
                            Span::new(ps.start, end),
                            n.to_owned(),
                            modifiers.contains(Modifiers::PRIVATE),
                            false,
                        );
                        self.add_member(idx, false, (false, n.to_owned()), member);
                    }
                }
                Node::MethodDef {
                    modifiers,
                    key,
                    kind,
                    ..
                } => {
                    let accessor = matches!(kind, MethodKind::Get | MethodKind::Set);
                    self.add_class_member(idx, modifiers, key, accessor);
                }
                Node::PropertyDef { modifiers, key, .. } => {
                    self.add_class_member(
                        idx,
                        modifiers,
                        key,
                        modifiers.contains(Modifiers::ACCESSOR),
                    );
                }
                _ => {}
            }
        }
        self.frames.push(Frame {
            this_ctx: Some(idx),
            is_static: false,
            class: Some(idx),
        });
    }

    fn add_class_member(
        &mut self,
        class: usize,
        modifiers: Modifiers,
        key: NodeId,
        accessor: bool,
    ) {
        let Some((k, name, span)) = self.member_name(key) else {
            return;
        };
        let reportable = k.0 || modifiers.contains(Modifiers::PRIVATE);
        let member = Member::new(span, name, reportable, accessor);
        self.add_member(class, modifiers.contains(Modifiers::STATIC), k, member);
    }

    fn find_class(&self, name: &str) -> Option<usize> {
        self.frames
            .iter()
            .rev()
            .filter_map(|f| f.class)
            .find(|&c| self.classes[c].name == Some(name))
    }

    fn function_frame(&self, node: NodeId) -> Frame {
        let cx = self.cx;
        let ast = self.ast();
        let upper = self.frames.last().copied();
        let none = Frame {
            this_ctx: None,
            is_static: false,
            class: None,
        };
        if let Some(p) = parent_skip_parens(ast, cx.file.parents, node) {
            let member = match *ast.node(p) {
                Node::MethodDef {
                    modifiers, value, ..
                } if value == node => Some(modifiers),
                Node::PropertyDef {
                    modifiers,
                    value: Some(v),
                    ..
                } if skip_parens(ast, v) == node => Some(modifiers),
                _ => None,
            };
            if let Some(modifiers) = member
                && cx.parent(p).is_some_and(|c| {
                    matches!(ast.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                })
            {
                return Frame {
                    this_ctx: upper.and_then(|u| u.class),
                    is_static: modifiers.contains(Modifiers::STATIC),
                    class: None,
                };
            }
        }
        let params = match *ast.node(node) {
            Node::FunctionDecl { params, .. } | Node::FunctionExpr { params, .. } => {
                ast.list(params)
            }
            _ => &[],
        };
        if let Some(&first) = params.first()
            && let Node::Param {
                pattern,
                type_ann: Some(t),
                ..
            } = *ast.node(first)
            && ident_name(ast, pattern) == Some("this")
            && let Node::TypeRef { name, .. } = *ast.node(t)
            && let Some(n) = ident_name(ast, name)
            && let Some(c) = self.find_class(n)
        {
            return Frame {
                this_ctx: Some(c),
                is_static: false,
                class: None,
            };
        }
        none
    }

    /// Which class (and whether statics) the object of `obj.x` refers to.
    fn object_class(&self, obj: NodeId) -> Option<(usize, bool)> {
        let cx = self.cx;
        let ast = self.ast();
        let frame = self.frames.last().copied()?;
        let obj = skip_parens(ast, obj);
        match *ast.node(obj) {
            Node::This {} => Some((frame.this_ctx?, frame.is_static)),
            Node::Ident { .. } => {
                let name = ident_name(ast, obj)?;
                if let Some(c) = self.find_class(name) {
                    return Some((c, true));
                }
                let sem = cx.semantic();
                let sym = sem
                    .reference_at(obj)
                    .and_then(|r| sem.reference(r).symbol())?;
                let symbol = sem.symbol(sym);
                let first = symbol.decls.first()?;
                if let Node::VarDeclarator { init, .. } = *ast.node(first.node) {
                    if !init.is_some_and(|i| matches!(ast.node(skip_parens(ast, i)), Node::This {}))
                    {
                        return None;
                    }
                    if symbol.refs.iter().any(|&r| {
                        let r = sem.reference(r);
                        r.is_write() && !r.flags.contains(RefFlags::INIT)
                    }) {
                        return None;
                    }
                    return Some((frame.this_ctx?, frame.is_static));
                }
                let ty = match *ast.node(first.node) {
                    Node::Param {
                        pattern, type_ann, ..
                    } if pattern == first.name => type_ann,
                    Node::CatchClause { type_ann, .. } => type_ann,
                    _ => None,
                }?;
                match *ast.node(ty) {
                    Node::TypeRef { name, .. } if matches!(ast.node(name), Node::Ident { .. }) => {
                        Some((self.find_class(ident_name(ast, name)?)?, false))
                    }
                    Node::TypeQuery { expr_name, .. }
                        if matches!(ast.node(expr_name), Node::Ident { .. }) =>
                    {
                        Some((self.find_class(ident_name(ast, expr_name)?)?, true))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Counts a use of `target` (a member expression, or the `#x in o` expression).
    fn count(&mut self, target: NodeId, member: usize) {
        let cx = self.cx;
        let ast = self.ast();
        let parents = cx.file.parents;
        let gp = parent_skip_parens(ast, parents, target);
        let es = |n: NodeId| skip_parens(ast, n);
        let in_stmt = |n: NodeId| {
            parent_skip_parens(ast, parents, n)
                .is_some_and(|p| matches!(ast.node(p), Node::ExprStmt { .. }))
        };
        let write = gp.is_some_and(|g| match *ast.node(g) {
            Node::Assign { left, op, .. } if es(left) == target => {
                op == AssignOp::Assign || in_stmt(g)
            }
            Node::ForIn { left, .. } | Node::ForOf { left, .. } => es(left) == target,
            Node::AssignPattern { left, .. } => es(left) == target,
            Node::Update { .. } => in_stmt(g),
            Node::BindingProperty { value: Some(v), .. } => es(v) == target,
            Node::Rest { .. } | Node::ArrayPattern { .. } => true,
            Node::Property { value, .. } => {
                es(value) == target && write_root(ast, parents, target).is_some()
            }
            Node::Spread { .. } | Node::ArrayLit { .. } => {
                write_root(ast, parents, target).is_some()
            }
            _ => false,
        });
        let m = &mut self.members[member];
        if write {
            m.writes += 1;
        } else {
            m.reads += 1;
        }
    }

    fn destructure_this(&mut self, pattern: NodeId) {
        let ast = self.ast();
        let Some(frame) = self.frames.last().copied() else {
            return;
        };
        let Some(c) = frame.this_ctx else { return };
        let props = match *ast.node(pattern) {
            Node::ObjectPattern { props } | Node::ObjectLit { props } => ast.list(props),
            _ => return,
        };
        for &p in props {
            let key = match *ast.node(p) {
                Node::BindingProperty { key, .. } | Node::Property { key, .. } => key,
                Node::ShorthandProp { name, .. } => name,
                _ => continue,
            };
            let Some(name) =
                ident_name(ast, key).filter(|_| matches!(ast.node(key), Node::Ident { .. }))
            else {
                continue;
            };
            let class = &self.classes[c];
            let map = if frame.is_static {
                &class.statics
            } else {
                &class.instance
            };
            if let Some(&m) = map.get(&(false, name.to_owned())) {
                self.members[m].reads += 1;
            }
        }
    }

    fn is_this(&self, id: Option<NodeId>) -> bool {
        id.is_some_and(|i| matches!(self.ast().node(skip_parens(self.ast(), i)), Node::This {}))
    }
}

impl Visitor for Analyzer<'_, '_, '_> {
    fn enter(&mut self, ast: &ts_r_ast::Ast, id: NodeId) -> Walk {
        match *ast.node(id) {
            Node::ClassDecl { name, members, .. } | Node::ClassExpr { name, members, .. } => {
                self.enter_class(name, members)
            }
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => {
                let f = self.function_frame(id);
                self.frames.push(f);
            }
            Node::StaticBlock { .. } => {
                let class = self.frames.last().and_then(|f| f.class);
                self.frames.push(Frame {
                    this_ctx: class,
                    is_static: true,
                    class: None,
                });
            }
            Node::Member { obj, prop, .. } => {
                if let Node::Ident { name } = *ast.node(prop)
                    && let Some((c, st)) = self.object_class(obj)
                {
                    let class = &self.classes[c];
                    let map = if st { &class.statics } else { &class.instance };
                    if let Some(&m) = map.get(&(false, ast.atom(name).to_owned())) {
                        self.count(id, m);
                    }
                }
            }
            Node::Index { obj, index, .. } => {
                if let Some(k) = literal_key(self.cx, skip_parens(ast, index))
                    && let Some((c, st)) = self.object_class(obj)
                {
                    let class = &self.classes[c];
                    let map = if st { &class.statics } else { &class.instance };
                    if let Some(&m) = map.get(&(false, k)) {
                        self.count(id, m);
                    }
                }
            }
            Node::PrivateName { name } => {
                let parent = self.cx.parent(id);
                let is_key = parent.is_some_and(|p| {
                    matches!(*ast.node(p), Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } if key == id)
                });
                if let (Some(p), false) = (parent, is_key) {
                    let key = (true, ast.atom(name).to_owned());
                    let found = self.frames.iter().rev().find_map(|f| {
                        let c = &self.classes[f.this_ctx?];
                        c.instance
                            .get(&key)
                            .or_else(|| c.statics.get(&key))
                            .copied()
                    });
                    if let Some(m) = found {
                        self.count(p, m);
                    }
                }
            }
            Node::VarDeclarator { name, init, .. } if self.is_this(init) => {
                if matches!(ast.node(name), Node::ObjectPattern { .. }) {
                    self.destructure_this(name);
                }
            }
            Node::Assign { left, right, .. } if self.is_this(Some(right)) => {
                self.destructure_this(skip_parens(ast, left));
            }
            Node::AssignPattern { left, right } if self.is_this(Some(right)) => {
                if matches!(ast.node(left), Node::ObjectPattern { .. }) {
                    self.destructure_this(left);
                }
            }
            Node::Param { pattern, init, .. } if self.is_this(init) => {
                if matches!(ast.node(pattern), Node::ObjectPattern { .. }) {
                    self.destructure_this(pattern);
                }
            }
            Node::BindingProperty {
                value: Some(v),
                init,
                ..
            } if self.is_this(init) => {
                if matches!(ast.node(v), Node::ObjectPattern { .. }) {
                    self.destructure_this(v);
                }
            }
            _ => {}
        }
        Walk::Continue
    }

    fn leave(&mut self, ast: &ts_r_ast::Ast, id: NodeId) {
        if matches!(
            ast.node(id),
            Node::ClassDecl { .. }
                | Node::ClassExpr { .. }
                | Node::FunctionDecl { .. }
                | Node::FunctionExpr { .. }
                | Node::StaticBlock { .. }
        ) {
            self.frames.pop();
        }
    }
}

impl Rule for NoUnusedPrivateClassMembers {
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let mut a = Analyzer {
            cx: &*cx,
            classes: Vec::new(),
            members: Vec::new(),
            frames: vec![Frame {
                this_ctx: None,
                is_static: false,
                class: None,
            }],
        };
        walk(ast, ast.root, &mut a);
        // Members still tracked (a later declaration of a key replaces an earlier one).
        let mut live: Vec<usize> = a
            .classes
            .iter()
            .flat_map(|c| c.instance.values().chain(c.statics.values()).copied())
            .collect();
        live.sort_unstable();
        let reports: Vec<(Span, String)> = live
            .into_iter()
            .map(|i| &a.members[i])
            .filter(|m| m.reportable && !(m.reads > 0 || (m.writes > 0 && m.accessor)))
            .map(|m| (m.name_span, m.name.clone()))
            .collect();
        for (span, name) in reports {
            cx.report(
                span,
                format!("Private class member '{name}' is defined but never used."),
            )
            .id("unusedPrivateClassMember");
        }
    }
}
