//! The lowering walk: one pass over the transformed tree that applies every year's lowering
//! the target needs, newest year first at each node, as tsc's chain of per-year transformers
//! does.
//!
//! **Temporaries** (observed, tsc 7.0.2): every transformer hoists its temporaries into one
//! `var` statement per function body (or the module), inserted right after the directive
//! prologue, so the statement of the transformer that runs last comes first: ES2016's, then
//! ES2020's optional chains, ES2020's `??`, ES2021's, then the TypeScript transform's own.
//! An expression-bodied arrow that needs one becomes `{ var _a; return x; }` on one line;
//! a one-line body stays on one line. Names are given in print order, per function: `_a` to
//! `_z` without `_i` and `_n`, then `_0`, `_1`, ..., skipping every identifier of the file;
//! a nested function starts again at `_a`. Temporaries are created here as placeholder
//! names and named by [`Lower::name_temps`] once the tree is final.

use rustc_hash::{FxHashMap, FxHashSet};
use ts_r_ast::{Atom, FnFlags, Modifiers, Node, NodeId, NodeKind, NodeList, Span, VarKind};
use ts_r_config::ScriptTarget;

use crate::builder::{Build, replace};
use crate::transform::Tx;

/// The `var` statement a temporary is hoisted into, in print order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Group {
    Es2016 = 0,
    /// ES2017: `var`s of an async body that collide with a parameter.
    Es2017,
    /// ES2018 `for await` and object rest.
    Es2018,
    /// ES2020 optional chains.
    Chain,
    /// ES2020 `??`.
    Coalesce,
    Es2021,
    /// ES2022 class aliases, heritage and computed-key temporaries.
    Es2022,
}

const GROUPS: usize = 7;

#[derive(Default)]
struct Frame {
    groups: [Vec<Atom>; GROUPS],
    /// The parameter of the setters a private destructuring target becomes (one name per
    /// function, not hoisted).
    setter_param: Option<Atom>,
}

pub(crate) struct Lower<'t, 'a> {
    pub tx: &'t mut Tx<'a>,
    pub target: ScriptTarget,
    frames: Vec<Frame>,
    temps: u32,
    /// Index of the append-only source-map ranges, refreshed before copying a name.
    map_ranges: FxHashMap<NodeId, Span>,
    indexed_ranges: usize,
    /// The functions around the node being visited, innermost last.
    pub fns: Vec<FnCtx>,
    /// Bases of the unique-name placeholders (`\u{1}{n}`), by `n`.
    uniques: Vec<UniqueBase>,
    /// The `arguments` atom.
    pub arguments: Atom,
    /// Loops around the node being visited, in the current function.
    pub loops: u32,
    /// `args_shared` for code outside every function.
    pub top_args: Option<Atom>,
    /// Statements to insert after the statement being visited (a lowered class's statics).
    pub after: Vec<NodeId>,
    /// Temporaries whose names nested functions do not reuse (class temporaries).
    pub reserved: FxHashSet<Atom>,
    /// The TypeScript transform's temporaries moved into a lowering `var` statement.
    pub moved: FxHashSet<Atom>,
    /// The private names of the classes around the node being visited, innermost last.
    /// Receiver captures of private destructuring targets, collected while an assignment
    /// pattern is visited (`None` elsewhere: such targets are refused).
    pub priv_caps: Option<Vec<NodeId>>,
    /// Private names declared by the classes lowered so far.
    pub declared: FxHashSet<Atom>,
    /// Synthesized functions whose temporaries belong to the enclosing function.
    pub transparent: FxHashSet<NodeId>,
    /// The source has `...` (object rest or spread may need lowering).
    pub dots: bool,
    /// Cached template objects of tagged templates (module files), declared at the end.
    pub template_objects: Vec<Atom>,
    pub privates: Vec<super::private::PrivEnv>,
    /// Receiver chains already found to contain no optional link.
    pub ordinary_chains: FxHashSet<NodeId>,
}

/// What a unique name (`arguments_1`, `e_1`, `it_1_1`) is made from.
enum UniqueBase {
    Text(String),
    /// `base` itself when free, else `base_1`, ... (private storage `_C_x`).
    Exact(String),
    /// `base_1`, ... free in the file but not reserved: renamed async parameters and
    /// `args_1` repeat across functions (observed).
    Local(String),
    /// The final name of another unique name (`it_1` → `it_1_1`).
    Of(Atom),
}

/// A function being visited.
#[derive(Default)]
pub(crate) struct FnCtx {
    pub arrow: bool,
    /// An async function or arrow lowered to a generator (`await` → `yield`).
    pub lower_await: bool,
    /// An async generator lowered to a generator (`await x` → `yield __await(x)`).
    pub async_gen: bool,
    /// `arguments_1`, once the lowered body uses `arguments` (declared by this function).
    pub args: Option<Atom>,
    /// For a function that is not lowered: the `arguments_1` its lowered async arrows share.
    pub args_shared: Option<Atom>,
    /// `super` in a lowered async method: `_super.x` / `_superIndex(k)`.
    pub sup: Option<SuperUse>,
}

#[derive(Default)]
pub(crate) struct SuperUse {
    /// Names read as `super.x`, in first-use order.
    pub names: Vec<Atom>,
    /// `super[k]` is used.
    pub index: bool,
    /// Some `super` property is assigned (setters and the cached `_superIndex`).
    pub assign: bool,
}

impl Build for Lower<'_, '_> {
    fn ast_mut(&mut self) -> &mut ts_r_ast::Ast {
        self.tx.ast
    }
}

/// Placeholder names start with a NUL, which no identifier can contain.
const PLACEHOLDER: char = '\0';
/// Unique-name placeholders start with U+0001.
const UNIQUE: char = '\u{1}';

pub(crate) fn run(tx: &mut Tx) {
    let target = tx.opts.target;
    let arguments = tx.ast.intern("arguments");
    let mut l = Lower {
        tx,
        target,
        frames: vec![Frame::default()],
        temps: 0,
        map_ranges: FxHashMap::default(),
        indexed_ranges: 0,
        fns: Vec::new(),
        uniques: Vec::new(),
        arguments,
        loops: 0,
        top_args: None,
        after: Vec::new(),
        reserved: FxHashSet::default(),
        moved: FxHashSet::default(),
        privates: Vec::new(),
        priv_caps: None,
        transparent: FxHashSet::default(),
        declared: FxHashSet::default(),
        dots: false,
        template_objects: Vec::new(),
        ordinary_chains: FxHashSet::default(),
    };
    l.dots = l.tx.src.contains("...");
    l.adopt_using_names();
    l.rename_arguments_bindings();
    let root = l.tx.ast.root;
    let Node::Program { stmts } = l.tx.n(root) else {
        return;
    };
    let stmts = l.visit_stmts(stmts);
    if l.tx.src.contains('#') {
        l.blank_privates();
    }
    let frame = l.frames.pop().unwrap_or_default();
    let mut stmts = l.with_temps(stmts, frame);
    if !l.template_objects.is_empty() {
        let names = std::mem::take(&mut l.template_objects);
        let at = l.tx.src.len() as u32;
        let var = l.template_objects_var(&names, at);
        let mut all = l.tx.items(stmts);
        all.push(var);
        stmts = l.new_list(&all);
    }
    l.tx.set(root, Node::Program { stmts });
    if l.temps > 0 || !l.uniques.is_empty() || !l.tx.temp_atoms.is_empty() {
        l.name_temps();
    }
}

impl Lower<'_, '_> {
    /// True if the target is below `year`, so `year`'s syntax is lowered.
    #[inline]
    pub fn below(&self, year: ScriptTarget) -> bool {
        self.target < year
    }

    #[inline]
    pub fn n(&self, id: NodeId) -> Node {
        self.tx.n(id)
    }

    #[inline]
    pub fn at(&self, id: NodeId) -> u32 {
        self.tx.at(id)
    }

    /// A new temporary hoisted into the innermost function's `group` statement.
    /// The parameter name of a private destructuring target's setter in this function (a
    /// placeholder named in print order, after the function's hoisted temporaries).
    pub fn setter_param(&mut self) -> Atom {
        if let Some(a) = self.frames.last().and_then(|f| f.setter_param) {
            return a;
        }
        let a = self.tx.ast.intern(&format!("{PLACEHOLDER}{}", self.temps));
        self.temps += 1;
        if let Some(f) = self.frames.last_mut() {
            f.setter_param = Some(a);
        }
        a
    }

    pub fn temp(&mut self, group: Group) -> Atom {
        let a = self.tx.ast.intern(&format!("{PLACEHOLDER}{}", self.temps));
        self.temps += 1;
        if let Some(f) = self.frames.last_mut() {
            f.groups[group as usize].push(a);
        }
        a
    }

    /// A unique name `base_1`, `base_2`, ... (file-wide; named in print order).
    pub fn unique(&mut self, base: &str) -> Atom {
        self.unique_of(UniqueBase::Text(base.to_owned()))
    }

    /// A unique name `base` (or `base_1`, ... when taken).
    pub fn unique_exact(&mut self, base: &str) -> Atom {
        self.unique_of(UniqueBase::Exact(base.to_owned()))
    }

    /// A name `base_1`, `base_2`, ... not used in the file, but not reserved either.
    pub fn unique_local(&mut self, base: &str) -> Atom {
        self.unique_of(UniqueBase::Local(base.to_owned()))
    }

    /// The `using` lowering's names become unique-name placeholders, so they are numbered
    /// with this transform's (`e_1` of a `for await` before a `using`'s `e_2`, observed).
    fn adopt_using_names(&mut self) {
        let names = std::mem::take(&mut self.tx.using_names);
        if names.is_empty() {
            return;
        }
        let mut map: FxHashMap<Atom, Atom> = FxHashMap::default();
        for a in names {
            let text = self.tx.ast.atom(a).to_owned();
            let base = text
                .rsplit_once('_')
                .map_or(text.as_str(), |(b, _)| b)
                .to_owned();
            self.tx.taken.remove(&text);
            let u = self.unique(&base);
            map.insert(a, u);
        }
        for raw in 1..self.tx.ast.node_count() as u32 {
            let Some(id) = NodeId::from_raw(raw) else {
                continue;
            };
            if let Node::Ident { name } = self.n(id)
                && let Some(&u) = map.get(&name)
            {
                self.tx.set(id, Node::Ident { name: u });
            }
        }
    }

    /// The text a unique-name placeholder is made from (`it` for `it_1`), if `name` is one.
    pub fn unique_base(&self, name: Atom) -> Option<&str> {
        let t = self.tx.ast.atom(name);
        let n: usize = t.strip_prefix(UNIQUE)?.parse().ok()?;
        match self.uniques.get(n)? {
            UniqueBase::Text(s) | UniqueBase::Local(s) | UniqueBase::Exact(s) => Some(s),
            UniqueBase::Of(_) => None,
        }
    }

    /// True if `name` is a temporary placeholder (`_a`, named later).
    pub fn is_temp(&self, name: Atom) -> bool {
        self.tx.ast.atom(name).starts_with(PLACEHOLDER)
    }

    /// A unique name made from the final name of the unique name `of` (`it_1_1`).
    pub fn unique_from(&mut self, of: Atom) -> Atom {
        self.unique_of(UniqueBase::Of(of))
    }

    fn unique_of(&mut self, base: UniqueBase) -> Atom {
        let a = self
            .tx
            .ast
            .intern(&format!("{UNIQUE}{}", self.uniques.len()));
        self.uniques.push(base);
        a
    }

    /// The innermost function's `group` temporaries so far.
    pub fn frame_group(&self, group: Group) -> Vec<Atom> {
        self.frames
            .last()
            .map(|f| f.groups[group as usize].clone())
            .unwrap_or_default()
    }

    pub fn frame_group_mut(&mut self, group: Group) -> &mut Vec<Atom> {
        if self.frames.is_empty() {
            self.frames.push(Frame::default());
        }
        let last = self.frames.len() - 1;
        &mut self.frames[last].groups[group as usize]
    }

    /// Hoists `name` into the innermost function's `group` statement.
    pub fn hoist(&mut self, name: Atom, group: Group) {
        if let Some(f) = self.frames.last_mut() {
            f.groups[group as usize].push(name);
        }
    }

    /// A new temporary that is not hoisted (a `catch` binding, a loop variable).
    pub fn local_temp(&mut self) -> Atom {
        let a = self.tx.ast.intern(&format!("{PLACEHOLDER}{}", self.temps));
        self.temps += 1;
        a
    }

    /// Visits `id` and lowers what the target needs, in place.
    pub fn visit(&mut self, id: NodeId) {
        let n = self.n(id);
        match n {
            Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. } => {
                self.function(id, None)
            }
            Node::ClassDecl { .. } | Node::ClassExpr { .. }
                if self.below(ScriptTarget::ES2022) && self.lowers_class(id) =>
            {
                self.class(id, None, false)
            }
            Node::VarDeclarator { .. }
            | Node::Assign { .. }
            | Node::Property { .. }
            | Node::Param { .. }
            | Node::AssignPattern { .. }
                if self.below(ScriptTarget::ES2022) && self.named_class(id).is_some() =>
            {
                if let Some((c, hint)) = self.named_class(id) {
                    let in_var = matches!(n, Node::VarDeclarator { .. });
                    self.class(c, Some(hint), in_var);
                }
            }
            Node::MethodDef {
                key, value, kind, ..
            } => {
                // `async` on a constructor or accessor (an error) is dropped (observed).
                if self.below(ScriptTarget::ES2017)
                    && kind != ts_r_ast::MethodKind::Method
                    && let Node::FunctionExpr {
                        modifiers,
                        flags,
                        name,
                        type_params,
                        params,
                        return_type,
                        body,
                    } = self.n(value)
                    && flags.contains(FnFlags::ASYNC)
                {
                    let mut flags = flags;
                    flags.remove(FnFlags::ASYNC);
                    self.tx.set(
                        value,
                        Node::FunctionExpr {
                            modifiers,
                            flags,
                            name,
                            type_params,
                            params,
                            return_type,
                            body,
                        },
                    );
                }
                self.visit_key(key);
                self.function(value, Some(id));
            }
            Node::ExprStmt { expr } if self.is_private_update_stmt(expr) => {
                let e = self.skip_parens(expr);
                self.private_use(e, true);
            }
            _ if self.is_private_use(id) => self.private_use(id, false),
            Node::Ident { name } if name == self.arguments => self.arguments_ref(id),
            Node::ShorthandProp { name, .. } if self.tx.atom_of(name) == Some(self.arguments) => {
                self.arguments_shorthand(id)
            }
            Node::Member { obj, .. } | Node::Index { obj, .. }
                if self.n(obj) == Node::Super {} && self.super_ctx().is_some() =>
            {
                self.super_access(id)
            }
            Node::Call { callee, .. } if self.is_super_access(callee) => self.super_call(id),
            Node::Await { .. } if self.below(ScriptTarget::ES2018) && self.in_async_gen() => {
                self.await_in_async_gen(id)
            }
            Node::Await { .. } if self.below(ScriptTarget::ES2017) && !self.fns.is_empty() => {
                self.await_to_yield(id)
            }
            Node::Yield { .. } if self.below(ScriptTarget::ES2018) && self.in_async_gen() => {
                self.yield_in_async_gen(id)
            }
            Node::Return { .. } if self.below(ScriptTarget::ES2018) && self.in_async_gen() => {
                self.return_in_async_gen(id)
            }
            Node::ForOf { is_await: true, .. } if self.below(ScriptTarget::ES2018) => {
                self.for_await(id, None)
            }
            Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                ..
            } if self.below(ScriptTarget::ES2018)
                && matches!(self.n(left), Node::ObjectLit { .. } | Node::ArrayLit { .. })
                && self.has_object_rest(left) =>
            {
                self.rest_assign(id, true)
            }
            Node::ExprStmt { expr }
                if self.below(ScriptTarget::ES2018) && self.is_rest_assign(expr) =>
            {
                let inner = self.skip_parens(expr);
                self.rest_assign(inner, false)
            }
            Node::VarDecl { decls, .. }
                if self.below(ScriptTarget::ES2018) && self.decls_have_rest(decls) =>
            {
                self.children(id);
                self.rest_var_decl(id);
            }
            Node::CatchClause { param: Some(p), .. }
                if self.below(ScriptTarget::ES2018) && self.has_object_rest(p) =>
            {
                self.rest_catch(id)
            }
            Node::ForOf {
                is_await: false,
                left,
                ..
            }
            | Node::ForIn { left, .. }
                if self.below(ScriptTarget::ES2018) && self.left_has_rest(left) =>
            {
                self.rest_for(id)
            }
            Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } if matches!(self.n(left), Node::ObjectLit { .. } | Node::ArrayLit { .. }) => {
                // Receivers of private targets are captured first: `(_a = this, [...] = v)`.
                let outer = self.priv_caps.replace(Vec::new());
                self.visit_target(left);
                self.visit(right);
                let caps = std::mem::replace(&mut self.priv_caps, outer).unwrap_or_default();
                if !caps.is_empty() {
                    let at = self.at(id);
                    let assign = crate::builder::clone_node(self.tx.ast, id);
                    let mut items = caps;
                    items.push(assign);
                    let seq = self.seq(&items, at);
                    self.put(id, seq);
                }
            }
            Node::ForOf {
                left, right, body, ..
            }
            | Node::ForIn { left, right, body }
                if matches!(self.n(left), Node::ObjectLit { .. } | Node::ArrayLit { .. }) =>
            {
                self.visit_target(left);
                self.visit(right);
                self.loops += 1;
                self.visit(body);
                self.loops -= 1;
            }
            Node::TaggedTemplate { quasi, .. }
                if self.below(ScriptTarget::ES2018) && self.has_invalid_escape(quasi) =>
            {
                self.tagged_template(id)
            }
            Node::ObjectLit { props }
                if self.below(ScriptTarget::ES2018)
                    && (self.has_spread(props) || self.holds_rest_assign(props)) =>
            {
                self.object_spread(id)
            }
            Node::Member { .. } | Node::Index { .. } | Node::Call { .. }
                if self.below(ScriptTarget::ES2020) && self.chain_has_optional(id) =>
            {
                self.optional_chain(id)
            }
            Node::Call {
                callee,
                optional: false,
                args,
                ..
            } if self.below(ScriptTarget::ES2020) && self.paren_chain_callee(callee) => {
                self.paren_chain_call(id, callee, args)
            }
            Node::Unary {
                op: ts_r_ast::UnaryOp::Delete,
                arg,
            } if self.below(ScriptTarget::ES2020)
                && self.chain_has_optional(self.skip_parens(arg)) =>
            {
                self.optional_delete(id, arg)
            }
            Node::Binary {
                op: ts_r_ast::BinaryOp::Coalesce,
                ..
            } if self.below(ScriptTarget::ES2020) => self.coalesce(id),
            Node::Binary {
                op: ts_r_ast::BinaryOp::Exp,
                ..
            } if self.below(ScriptTarget::ES2016) => self.exponent(id),
            Node::Assign {
                op:
                    ts_r_ast::AssignOp::OrAssign
                    | ts_r_ast::AssignOp::AndAssign
                    | ts_r_ast::AssignOp::CoalesceAssign,
                ..
            } if self.below(ScriptTarget::ES2021) => self.logical_assign(id),
            Node::Assign {
                op: ts_r_ast::AssignOp::ExpAssign,
                ..
            } if self.below(ScriptTarget::ES2016) => self.exponent_assign(id),
            Node::CatchClause { param: None, .. } if self.below(ScriptTarget::ES2019) => {
                self.optional_catch(id)
            }
            Node::NumberLit { .. } | Node::BigIntLit { .. } if self.below(ScriptTarget::ES2021) => {
                self.numeric_separators(id)
            }
            _ => self.children(id),
        }
    }

    /// Visits the statements of a list, inserting what lowering puts after each one.
    pub fn visit_stmts(&mut self, list: NodeList) -> NodeList {
        let items = self.tx.items(list);
        let mut out: Vec<NodeId> = Vec::with_capacity(items.len());
        let mut changed = false;
        // Inserted statements wait for the CommonJS `exports.C = C;` after a class.
        let mut pending: Vec<NodeId> = Vec::new();
        for &s in &items {
            if !pending.is_empty() && !self.is_export_copy(s) {
                out.append(&mut pending);
            }
            let mark = self.after.len();
            self.visit(s);
            out.push(s);
            if self.after.len() > mark {
                changed = true;
                pending.extend(self.after.drain(mark..));
            }
        }
        out.append(&mut pending);
        if changed { self.new_list(&out) } else { list }
    }

    /// `exports.x = x;`
    fn is_export_copy(&self, s: NodeId) -> bool {
        let Node::ExprStmt { expr } = self.n(s) else {
            return false;
        };
        let Node::Assign {
            op: ts_r_ast::AssignOp::Assign,
            left,
            right,
        } = self.n(expr)
        else {
            return false;
        };
        // `exports.default = ...` comes after the statics (observed).
        matches!(self.n(left), Node::Member { obj, prop, .. }
            if self.tx.atom_of(obj).is_some_and(|a| self.tx.ast.atom(a) == "exports")
                && self.tx.atom_of(prop).is_some_and(|a| self.tx.ast.atom(a) != "default"))
            && matches!(self.n(right), Node::Ident { .. })
    }

    /// Visits the children of `id` (not property names or labels, which are not references).
    pub fn children(&mut self, id: NodeId) {
        match self.n(id) {
            Node::Block { stmts } => {
                let stmts = self.visit_stmts(stmts);
                self.tx.set(id, Node::Block { stmts });
            }
            Node::ModuleBlock { stmts } => {
                let stmts = self.visit_stmts(stmts);
                self.tx.set(id, Node::ModuleBlock { stmts });
            }
            Node::SwitchCase { test, body } => {
                if let Some(t) = test {
                    self.visit(t);
                }
                let body = self.visit_stmts(body);
                self.tx.set(id, Node::SwitchCase { test, body });
            }
            Node::Member { obj, .. } => self.visit(obj),
            Node::Property { key, value, .. } => {
                self.visit_key(key);
                self.visit(value);
            }
            Node::BindingProperty { key, value, init } => {
                self.visit_key(key);
                if let Some(v) = value {
                    self.visit(v);
                }
                if let Some(i) = init {
                    self.visit(i);
                }
            }
            Node::PropertyDef { key, value, .. } => {
                self.visit_key(key);
                if let Some(v) = value {
                    self.visit(v);
                }
            }
            Node::Labeled { body, .. }
                if self.below(ScriptTarget::ES2018)
                    && matches!(self.n(body), Node::ForOf { is_await: true, .. }) =>
            {
                self.for_await(id, Some(id))
            }
            Node::Labeled { body, .. } => self.visit(body),
            Node::Break { .. } | Node::Continue { .. } => {}
            Node::For { .. }
            | Node::ForIn { .. }
            | Node::ForOf { .. }
            | Node::While { .. }
            | Node::DoWhile { .. } => {
                self.loops += 1;
                let kids = self.tx.ast.children(id);
                for c in kids {
                    self.visit(c);
                }
                self.loops -= 1;
            }
            _ => {
                let kids = self.tx.ast.children(id);
                for c in kids {
                    self.visit(c);
                }
            }
        }
    }

    /// Visits a destructuring assignment target (object and array literals are patterns
    /// there, so their spreads stay).
    pub fn visit_target(&mut self, id: NodeId) {
        match self.n(id) {
            Node::ObjectLit { props } => {
                for p in self.tx.items(props) {
                    match self.n(p) {
                        Node::Property { key, value, .. } => {
                            self.visit_key(key);
                            self.visit_target(value);
                        }
                        Node::Spread { arg } => self.visit_target(arg),
                        _ => self.visit(p),
                    }
                }
            }
            Node::ArrayLit { elements } => {
                for e in self.tx.items(elements) {
                    match self.n(e) {
                        Node::Spread { arg } => self.visit_target(arg),
                        _ => self.visit_target(e),
                    }
                }
            }
            Node::Assign {
                op: ts_r_ast::AssignOp::Assign,
                left,
                right,
            } => {
                self.visit_target(left);
                self.visit(right);
            }
            Node::Paren { expr } => self.visit_target(expr),
            // A private member as a destructuring target: a setter (`for` heads are refused).
            Node::Member { obj, prop, .. } if self.tx.kind(prop) == NodeKind::PrivateName => {
                if self.priv_caps.is_some() {
                    self.private_target(id);
                } else {
                    self.visit(obj)
                }
            }
            _ => self.visit(id),
        }
    }

    /// Visits a property name: only a computed one holds references.
    pub fn visit_key(&mut self, key: NodeId) {
        if let Node::ComputedKey { .. } = self.n(key) {
            self.children(key);
        }
    }

    pub fn visit_list(&mut self, list: NodeList) {
        for c in self.tx.items(list) {
            self.visit(c);
        }
    }

    // ---- functions and hoisting ----

    /// A function, arrow or method value (`method` is its `MethodDef`): its body is visited
    /// with its own temporaries, then async functions and async generators are lowered.
    fn function(&mut self, id: NodeId, method: Option<NodeId>) {
        let (params, body, flags, arrow) = match self.n(id) {
            Node::FunctionDecl {
                params,
                body,
                flags,
                ..
            }
            | Node::FunctionExpr {
                params,
                body,
                flags,
                ..
            } => (params, body, flags, false),
            Node::Arrow {
                params,
                body,
                flags,
                ..
            } => (params, Some(body), flags, true),
            _ => return,
        };
        let Some(body) = body else {
            self.visit_list(params);
            return;
        };
        // `async` on a constructor or accessor is an error tsc leaves alone.
        let is_async = flags.contains(FnFlags::ASYNC)
            && method.is_none_or(|m| {
                matches!(
                    self.n(m),
                    Node::MethodDef {
                        kind: ts_r_ast::MethodKind::Method,
                        ..
                    }
                )
            });
        let is_gen = flags.contains(FnFlags::GENERATOR);
        let lower_await = is_async && !is_gen && self.below(ScriptTarget::ES2017);
        let async_gen = is_async && is_gen && self.below(ScriptTarget::ES2018);
        let sup = if method.is_some() && (lower_await || async_gen) {
            Some(SuperUse {
                assign: self.assigns_super(body),
                ..SuperUse::default()
            })
        } else if !arrow && self.below(ScriptTarget::ES2017) && self.super_in_async_arrow(body) {
            // A function that is not lowered but holds a lowered async arrow using `super`
            // declares `_super` itself, naming every `super.x` it reads (observed).
            Some(SuperUse {
                assign: self.assigns_super(body),
                names: self.super_names(body),
                ..SuperUse::default()
            })
        } else {
            None
        };
        self.fns.push(FnCtx {
            arrow,
            lower_await,
            async_gen,
            args: None,
            args_shared: None,
            sup,
        });
        self.frames.push(Frame::default());
        let loops = std::mem::take(&mut self.loops);
        self.visit_list(params);
        let params_need_temps = self
            .frames
            .last()
            .is_some_and(|f| f.groups.iter().any(|g| !g.is_empty()));
        self.visit(body);
        self.loops = loops;
        if params_need_temps {
            // Temporaries cannot live in the parameter scope: initializers and patterns
            // move into the body (observed).
            let pre = self.move_param_inits(params);
            if !pre.is_empty() {
                self.prepend_body(id, pre, true);
            }
        }
        if self.below(ScriptTarget::ES2018) {
            let pre = self.rest_params(params);
            if !pre.is_empty() {
                self.prepend_body(id, pre, false);
            }
        }
        let body = match self.n(id) {
            Node::Arrow { body, .. } => body,
            _ => body,
        };
        if lower_await {
            self.hoist_colliding_vars(params, body);
        }
        let frame = self.frames.pop().unwrap_or_default();
        self.add_temps(id, body, frame);
        let ctx = self.fns.pop().unwrap_or_default();
        if lower_await {
            self.lower_async(id, method, ctx);
        } else if async_gen {
            self.lower_async_gen(id, method, ctx);
        } else if let Some(sup) = &ctx.sup {
            let at = self.at(id);
            let decls = self.super_decls(sup, at);
            self.prepend_body(id, decls, false);
        }
    }

    /// Declares `frame`'s temporaries in the body of function `id`.
    fn add_temps(&mut self, id: NodeId, body: NodeId, frame: Frame) {
        if frame.groups.iter().all(Vec::is_empty) {
            return;
        }
        match self.n(body) {
            Node::Block { stmts } => {
                let one_line = self.one_line_body(body);
                let stmts = self.with_temps(stmts, frame);
                self.tx.set(body, Node::Block { stmts });
                if one_line {
                    self.tx.mark_single_line(body);
                }
            }
            _ => {
                // `x => e` becomes `x => { var _a; return e; }`, on one line (observed, also
                // for a body on its own line).
                let at = self.at(body);
                let moved = crate::builder::clone_node(self.tx.ast, body);
                let ret = self.ret(Some(moved), at);
                let list = self.new_list(&[ret]);
                let stmts = self.with_temps(list, frame);
                let block = self.mk(Node::Block { stmts }, at);
                self.tx.mark_single_line(block);
                if let Node::Arrow {
                    flags,
                    type_params,
                    params,
                    return_type,
                    ..
                } = self.n(id)
                {
                    let mut flags = flags;
                    flags.remove(FnFlags::EXPRESSION_BODY);
                    self.tx.set(
                        id,
                        Node::Arrow {
                            flags,
                            type_params,
                            params,
                            return_type,
                            body: block,
                        },
                    );
                }
            }
        }
    }

    /// A parsed function body written on one line with no synthesized statements (tsc keeps
    /// it on one line after adding its `var`).
    pub fn one_line_body(&self, body: NodeId) -> bool {
        let s = self.tx.span(body);
        if s.start >= s.end {
            return false;
        }
        let Node::Block { stmts } = self.n(body) else {
            return false;
        };
        // A directive prologue makes the rewritten body multi-line (observed).
        if self.tx.ast.list(stmts).iter().any(|&st| {
            let sp = self.tx.span(st);
            sp.start >= sp.end || matches!(self.n(st), Node::Directive { .. })
        }) {
            return false;
        }
        self.tx
            .src
            .get(s.start as usize..s.end as usize)
            .is_some_and(|t| !t.bytes().any(|b| b == b'\n' || b == b'\r'))
    }

    /// `stmts` with `frame`'s `var` statements after the directive prologue.
    fn with_temps(&mut self, stmts: NodeList, frame: Frame) -> NodeList {
        if frame.groups.iter().all(Vec::is_empty) {
            return stmts;
        }
        let items = self.tx.items(stmts);
        let pro = items
            .iter()
            .take_while(|&&s| self.tx.kind(s) == NodeKind::Directive)
            .count();
        let mut out = Vec::with_capacity(items.len() + GROUPS);
        out.extend_from_slice(&items[..pro]);
        for group in frame.groups {
            if group.is_empty() {
                continue;
            }
            let decls: Vec<NodeId> = group
                .into_iter()
                .map(|t| {
                    let name = self.ident_atom(t, 0);
                    self.mk(
                        Node::VarDeclarator {
                            name,
                            definite: false,
                            type_ann: None,
                            init: None,
                        },
                        0,
                    )
                })
                .collect();
            let decls = self.new_list(&decls);
            let v = self.mk(
                Node::VarDecl {
                    modifiers: Modifiers::NONE,
                    kind: VarKind::Var,
                    decls,
                },
                0,
            );
            out.push(v);
        }
        let mut dropped = false;
        for &s in &items[pro..] {
            if !dropped && !self.moved.is_empty() && self.is_ts_temps(s) {
                dropped = true;
                let Node::VarDecl {
                    modifiers,
                    kind,
                    decls,
                } = self.n(s)
                else {
                    continue;
                };
                let keep: Vec<NodeId> = self
                    .tx
                    .items(decls)
                    .into_iter()
                    .filter(|&d| {
                        !matches!(self.n(d), Node::VarDeclarator { name, .. }
                            if self.tx.atom_of(name).is_some_and(|a| self.moved.contains(&a)))
                    })
                    .collect();
                if keep.is_empty() {
                    continue;
                }
                let decls = self.new_list(&keep);
                self.tx.set(
                    s,
                    Node::VarDecl {
                        modifiers,
                        kind,
                        decls,
                    },
                );
            }
            out.push(s);
        }
        self.new_list(&out)
    }

    /// The TypeScript transform's `var` of temporaries.
    fn is_ts_temps(&self, s: NodeId) -> bool {
        let Node::VarDecl {
            kind: VarKind::Var,
            decls,
            ..
        } = self.n(s)
        else {
            return false;
        };
        let ds = self.tx.ast.list(decls);
        !ds.is_empty()
            && ds.iter().all(|&d| {
                matches!(self.n(d), Node::VarDeclarator { name, init: None, .. }
                    if self.tx.atom_of(name).is_some_and(|a| self.tx.temp_atoms.contains(&a)))
            })
    }

    // ---- naming ----

    /// Gives every placeholder temporary its name, in print order (see the module docs).
    fn name_temps(&mut self) {
        let mut names: FxHashMap<Atom, Atom> = FxHashMap::default();
        // The TypeScript transform's temporaries are renamed with ours, in print order.
        let adopted: rustc_hash::FxHashSet<Atom> = self.tx.temp_atoms.iter().copied().collect();
        for &a in &adopted {
            let t = self.tx.ast.atom(a).to_owned();
            self.tx.taken.remove(&t);
        }
        let mut counters: Vec<usize> = vec![0];
        // Names of reserved temporaries per function: nested functions skip them.
        let mut reserved: Vec<Vec<Atom>> = vec![Vec::new()];
        let root = self.tx.ast.root;
        let mut stack: Vec<(NodeId, bool)> = vec![(root, false)];
        while let Some((id, exit)) = stack.pop() {
            if exit {
                counters.pop();
                reserved.pop();
                continue;
            }
            let n = self.n(id);
            match n {
                // Unique names are numbered afterwards, scope by scope.
                Node::Ident { name } if self.tx.ast.atom(name).starts_with(UNIQUE) => continue,
                Node::Ident { name } => {
                    if !self.tx.ast.atom(name).starts_with(PLACEHOLDER) && !adopted.contains(&name)
                    {
                        continue;
                    }
                    let given = match names.get(&name) {
                        Some(&g) => g,
                        None => {
                            let counter = counters.last().copied().unwrap_or(0);
                            let (g, next) = self.next_name(counter, &reserved);
                            if let Some(c) = counters.last_mut() {
                                *c = next;
                            }
                            if self.reserved.contains(&name)
                                && let Some(r) = reserved.last_mut()
                            {
                                r.push(g);
                            }
                            names.insert(name, g);
                            g
                        }
                    };
                    self.tx.set(id, Node::Ident { name: given });
                    continue;
                }
                Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                    if !self.transparent.contains(&id) =>
                {
                    counters.push(0);
                    reserved.push(Vec::new());
                    stack.push((id, true));
                }
                _ => {}
            }
            let kids = self.tx.ast.children(id);
            for &c in kids.iter().rev() {
                stack.push((c, false));
            }
        }
        if !self.uniques.is_empty() {
            self.name_uniques(&mut names);
        }
    }

    /// Numbers the unique names (`e_1`, `env_1`, ...): a function's own names first, in
    /// order, then those of the functions nested in it, each the same way (observed: a
    /// hoisted `function f` printed first still numbers after the module's own `env_1`; a
    /// `for await`'s `e_1` in the function's leading `var` before a `using` block's `e_2`).
    fn name_uniques(&mut self, names: &mut FxHashMap<Atom, Atom>) {
        let mut scopes = vec![self.tx.ast.root];
        let mut nested = Vec::new();
        let mut stack = Vec::new();
        while let Some(scope) = scopes.pop() {
            nested.clear();
            stack.push(scope);
            while let Some(id) = stack.pop() {
                match self.n(id) {
                    Node::FunctionDecl { .. } | Node::FunctionExpr { .. } | Node::Arrow { .. }
                        if id != scope =>
                    {
                        nested.push(id);
                        continue;
                    }
                    Node::Ident { name } if self.tx.ast.atom(name).starts_with(UNIQUE) => {
                        let given = self.unique_name(name, names);
                        self.tx.set(id, Node::Ident { name: given });
                        continue;
                    }
                    _ => {}
                }
                let kids = self.tx.ast.children(id);
                stack.extend(kids.iter().rev());
            }
            scopes.extend(nested.iter().rev());
        }
    }

    /// The final name of the unique-name placeholder `name`.
    fn unique_name(&mut self, name: Atom, names: &mut FxHashMap<Atom, Atom>) -> Atom {
        if let Some(&g) = names.get(&name) {
            return g;
        }
        let n: usize = self.tx.ast.atom(name)[1..].parse().unwrap_or(0);
        if let Some(UniqueBase::Local(base)) = self.uniques.get(n) {
            let base = base.clone();
            let mut i = 1;
            let g = loop {
                let cand = format!("{base}_{i}");
                let atom = self.tx.ast.intern(&cand);
                if !self.tx.sc.names.contains(&atom) && !self.tx.taken.contains(&cand) {
                    break atom;
                }
                i += 1;
            };
            names.insert(name, g);
            return g;
        }
        if let Some(UniqueBase::Exact(base)) = self.uniques.get(n) {
            let base = base.clone();
            let g = self.tx.fresh(&base, false);
            names.insert(name, g);
            return g;
        }
        let base = match self.uniques.get(n) {
            Some(UniqueBase::Text(s)) | Some(UniqueBase::Exact(s)) => s.clone(),
            Some(UniqueBase::Local(s)) => s.clone(),
            Some(&UniqueBase::Of(of)) => {
                let g = self.unique_name(of, names);
                self.tx.ast.atom(g).to_owned()
            }
            None => "_".to_owned(),
        };
        let g = self.tx.fresh(&base, true);
        names.insert(name, g);
        g
    }

    /// The first free temporary name at or after sequence position `i`, and the position
    /// after it.
    fn next_name(&mut self, mut i: usize, reserved: &[Vec<Atom>]) -> (Atom, usize) {
        loop {
            let cand = if i < 26 {
                let c = (b'a' + i as u8) as char;
                if c == 'i' || c == 'n' {
                    i += 1;
                    continue;
                }
                format!("_{c}")
            } else {
                format!("_{}", i - 26)
            };
            i += 1;
            let atom = self.tx.ast.intern(&cand);
            if !self.tx.sc.names.contains(&atom)
                && !self.tx.taken.contains(&cand)
                && !reserved.iter().any(|r| r.contains(&atom))
            {
                return (atom, i);
            }
        }
    }

    // ---- small helpers ----

    /// An expression tsc copies instead of capturing in a temporary: identifiers, `this`,
    /// `super`, `null`, booleans, numbers, strings and templates without substitutions (not
    /// bigints or regular expressions; observed).
    pub fn is_simple(&self, id: NodeId) -> bool {
        if self.tx.instantiated.contains(&id) {
            return false;
        }
        matches!(
            self.n(id),
            Node::Ident { .. }
                | Node::This {}
                | Node::Super {}
                | Node::NullLit {}
                | Node::BoolLit { .. }
                | Node::NumberLit { .. }
                | Node::StringLit { .. }
        ) || matches!(self.n(id), Node::TemplateLit { parts } if self.tx.ast.list(parts).len() == 1)
    }

    /// `id` without enclosing parentheses.
    pub fn skip_parens(&self, mut id: NodeId) -> NodeId {
        while let Node::Paren { expr } = self.n(id) {
            id = expr;
        }
        id
    }

    /// A copy of the simple expression `id` (a fresh node; identifiers synthesized).
    pub fn copy(&mut self, id: NodeId) -> NodeId {
        match self.n(id) {
            Node::Ident { name } => {
                let at = self.at(id);
                let c = self.ident_atom(name, at);
                // Source maps (tsc's): each copy of a name maps to the name (a copy of a
                // copy too).
                let span = self.tx.span(id);
                let range = if span.start < span.end {
                    Some(span)
                } else {
                    self.map_ranges
                        .extend(self.tx.map_ranges[self.indexed_ranges..].iter().copied());
                    self.indexed_ranges = self.tx.map_ranges.len();
                    self.map_ranges.get(&id).copied()
                };
                if let Some(r) = range {
                    self.tx.map_range(c, r);
                }
                c
            }
            _ => crate::builder::clone_node(self.tx.ast, id),
        }
    }

    /// `t = e` with a new temporary in `group` (parenthesized by the printer where needed),
    /// and a reference to `t`.
    pub fn capture(&mut self, e: NodeId, group: Group) -> (NodeId, NodeId) {
        let at = self.at(e);
        let t = self.temp(group);
        let target = self.ident_atom(t, at);
        let a = self.assign(target, e, at);
        let r = self.ident_atom(t, at);
        (a, r)
    }

    /// `e` itself and a copy when simple, else `(t = e)` and `t`.
    pub fn capture_unless_simple(&mut self, e: NodeId, group: Group) -> (NodeId, NodeId) {
        if self.is_simple(e) {
            let c = self.copy(e);
            (e, c)
        } else {
            self.capture(e, group)
        }
    }

    /// Replaces the node in slot `id` by `with`.
    pub fn put(&mut self, id: NodeId, with: NodeId) {
        replace(self.tx.ast, id, with);
    }
}

impl Lower<'_, '_> {
    /// Each evaluation of a private class in a loop owns its storage and class aliases.
    pub(super) fn scoped_private_class(&mut self, id: NodeId, hint: Option<Atom>) {
        let Some(mut p) = crate::class::parts(self.tx, id) else {
            return;
        };
        let at = self.at(id);
        let decl = !p.is_expr;
        let modifiers = p.modifiers;
        let name = p.name;
        let class = crate::builder::clone_node(self.tx.ast, id);
        p.is_expr = true;
        p.modifiers.remove(Modifiers::EXPORT | Modifiers::DEFAULT);
        crate::class::set_parts(self.tx, class, p);
        self.frames.push(Frame::default());
        let loops = std::mem::take(&mut self.loops);
        self.class_inner(class, hint, false);
        self.loops = loops;
        let frame = self.frames.pop().unwrap_or_default();
        let ret = self.ret(Some(class), at);
        let body = self.block(&[ret], at);
        let arrow = self.mk(
            Node::Arrow {
                flags: FnFlags::NONE,
                type_params: None,
                params: NodeList::EMPTY,
                return_type: None,
                body,
            },
            at,
        );
        self.add_temps(arrow, body, frame);
        let callee = self.paren(arrow, at);
        let call = self.call(callee, &[], at);
        let replacement = if decl {
            let Some(name) = name else {
                return;
            };
            let name = self.copy(name);
            self.var(VarKind::Let, modifiers, name, Some(call), at)
        } else {
            call
        };
        self.put(id, replacement);
    }
}
