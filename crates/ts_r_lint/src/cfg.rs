//! Control-flow graphs for function bodies, used by the code-path rules (`no-unreachable`,
//! `constructor-super`, `getter-return`, ...).
//!
//! A graph is a list of basic blocks. Each block holds *events* in evaluation order:
//! statements when they start, and expressions after their operands (post-order). Nested
//! functions and class bodies get their own graphs; only the parts of a class evaluated at
//! definition time (heritage, computed keys) are part of the enclosing graph.
//!
//! Modelled flow: branches (`if`, `?:`, `&&`/`||`/`??` and their assignments), loops (a
//! literal truthy test never exits), `switch` fall-through, labels, `break`/`continue`,
//! `return`, `throw`, and `try`/`catch`/`finally` (an expression that may throw inside a
//! `try` gets an edge to the handler; `finally` is built twice, for normal and abrupt
//! completion).

use ts_r_ast::{AssignOp, Ast, BinaryOp, Node, NodeId, NodeKind};

pub type BlockId = u32;

#[derive(Debug, Default, Clone)]
pub struct Block {
    pub events: Vec<NodeId>,
    pub succ: Vec<BlockId>,
}

#[derive(Debug)]
pub struct Cfg {
    pub blocks: Vec<Block>,
    pub entry: BlockId,
    /// Reached by falling off the end of the body.
    pub exit: BlockId,
    /// Reached by `return`.
    pub ret: BlockId,
    /// Switch cases with the block at the end of their statements (for fall-through).
    pub case_ends: Vec<(NodeId, BlockId)>,
    /// `try` statements with the block at the end of their `try` block (normal completion).
    pub try_ends: Vec<(NodeId, BlockId)>,
    reachable: Vec<bool>,
}

/// How [`Cfg::function_with`] and friends build the graph (the code-path rules use the
/// defaults; the plugin rules that model `process.exit()` set the rest).
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// Calls that end the program (sorted), as in [`Cfg::function_with_exits`].
    pub exits: &'a [NodeId],
    /// Identifiers (sorted) whose read cannot throw.
    pub safe_reads: &'a [NodeId],
    /// When set, the implicit throws inside a `try` are exactly after these nodes (sorted),
    /// instead of after every call, member access, identifier read, ...
    pub throws: Option<&'a [NodeId]>,
    /// ESLint's code-path model: the end of a `try` block also flows to its `catch`.
    pub try_end_to_catch: bool,
    /// Literal tests are folded even without exits, loops with a falsy literal test never
    /// run their body or loop again, and literals include TypeScript-wrapped ones and
    /// template literals without substitutions.
    pub fold_literals: bool,
    /// `continue` statements (sorted) that end their path instead of looping.
    pub dead_continues: &'a [NodeId],
    /// A `continue` in the copy of a `finally` built for abrupt completion ends its path
    /// when it targets a loop outside the `finally`.
    pub abrupt_finally_continue_dead: bool,
    /// With exits, static field initializers run with the class definition too, in
    /// member order with the static blocks.
    pub static_fields: bool,
}

impl Cfg {
    /// The graph of a function's parameters and body (`FunctionDecl`, `FunctionExpr`,
    /// `Arrow`).
    pub fn function(ast: &Ast, f: NodeId) -> Cfg {
        Cfg::function_with_exits(ast, f, &[])
    }

    /// [`Cfg::function`], where each call in `exits` (sorted) ends the program like
    /// `process.exit()`: its arguments are evaluated, then control goes nowhere (not even
    /// to a `catch` or `finally`). With exits, the static blocks of classes defined in the
    /// function are part of its graph too.
    pub fn function_with_exits(ast: &Ast, f: NodeId, exits: &[NodeId]) -> Cfg {
        Cfg::function_with(
            ast,
            f,
            &Options {
                exits,
                ..Options::default()
            },
        )
    }

    /// The graph of a function, built with `opts`.
    pub fn function_with(ast: &Ast, f: NodeId, opts: &Options<'_>) -> Cfg {
        let mut b = Builder::new(ast);
        b.opts(opts);
        match *ast.node(f) {
            Node::FunctionDecl { params, body, .. } | Node::FunctionExpr { params, body, .. } => {
                for &p in ast.list(params) {
                    b.expr(p);
                }
                if let Some(body) = body {
                    b.stmt(body);
                }
            }
            Node::Arrow { params, body, .. } => {
                for &p in ast.list(params) {
                    b.expr(p);
                }
                if ast.node(body).is_statement() {
                    b.stmt(body);
                } else {
                    b.expr(body);
                    let ret = b.ret;
                    b.edge(b.cur, ret);
                    b.cur = b.new_block();
                }
            }
            _ => {}
        }
        b.finish()
    }

    /// The graph of a statement list (`Program`, `StaticBlock`).
    pub fn statements(ast: &Ast, stmts: &[NodeId]) -> Cfg {
        Cfg::statements_with_exits(ast, stmts, &[], &[])
    }

    /// [`Cfg::statements`] with program-ending calls, as in [`Cfg::function_with_exits`]. A
    /// `break` or `continue` whose target is outside `stmts` goes nowhere, like an exit.
    /// The identifiers in `safe_reads` (sorted) are known to be defined: reading them
    /// cannot throw.
    pub fn statements_with_exits(
        ast: &Ast,
        stmts: &[NodeId],
        exits: &[NodeId],
        safe_reads: &[NodeId],
    ) -> Cfg {
        Cfg::statements_with(
            ast,
            stmts,
            &Options {
                exits,
                safe_reads,
                ..Options::default()
            },
        )
    }

    /// The graph of a statement list, built with `opts`.
    pub fn statements_with(ast: &Ast, stmts: &[NodeId], opts: &Options<'_>) -> Cfg {
        let mut b = Builder::new(ast);
        b.opts(opts);
        for &s in stmts {
            b.stmt(s);
        }
        b.finish()
    }

    /// The graph of one expression evaluated on its own (a class field initializer), built
    /// with `opts`.
    pub fn expression_with(ast: &Ast, expr: NodeId, opts: &Options<'_>) -> Cfg {
        let mut b = Builder::new(ast);
        b.opts(opts);
        b.expr(expr);
        b.finish()
    }

    pub fn is_reachable(&self, b: BlockId) -> bool {
        self.reachable[b as usize]
    }

    /// Reachable blocks containing each event (an event can appear more than once).
    pub fn event_reachability(&self) -> rustc_hash::FxHashMap<NodeId, bool> {
        let mut m: rustc_hash::FxHashMap<NodeId, bool> = rustc_hash::FxHashMap::default();
        for (i, b) in self.blocks.iter().enumerate() {
            for &e in &b.events {
                let r = self.reachable[i];
                m.entry(e).and_modify(|v| *v |= r).or_insert(r);
            }
        }
        m
    }

    /// Forward data flow: the state at the entry of every reachable block (`None` for
    /// unreachable ones). `transfer` maps a state across one event.
    pub fn forward<S: Clone + PartialEq>(
        &self,
        init: S,
        join: impl Fn(&S, &S) -> S,
        transfer: impl Fn(&S, NodeId) -> S,
    ) -> Vec<Option<S>> {
        let n = self.blocks.len();
        let mut entry: Vec<Option<S>> = vec![None; n];
        entry[self.entry as usize] = Some(init);
        let mut work = vec![self.entry];
        let mut queued = vec![false; n];
        queued[self.entry as usize] = true;
        while let Some(b) = work.pop() {
            queued[b as usize] = false;
            let Some(mut s) = entry[b as usize].clone() else {
                continue;
            };
            for &e in &self.blocks[b as usize].events {
                s = transfer(&s, e);
            }
            for &t in &self.blocks[b as usize].succ {
                let new = match &entry[t as usize] {
                    None => s.clone(),
                    Some(old) => join(old, &s),
                };
                if entry[t as usize].as_ref() != Some(&new) {
                    entry[t as usize] = Some(new);
                    if !queued[t as usize] {
                        queued[t as usize] = true;
                        work.push(t);
                    }
                }
            }
        }
        entry
    }
}

struct Jump<'a> {
    labels: Vec<&'a str>,
    brk: BlockId,
    cont: Option<BlockId>,
    /// Unlabeled `break` targets loops and switches only.
    breakable: bool,
    depth: usize,
}

struct Finally {
    abrupt: BlockId,
    routed: Vec<(BlockId, usize)>,
}

struct Builder<'a> {
    ast: &'a Ast,
    blocks: Vec<Block>,
    cur: BlockId,
    exit: BlockId,
    ret: BlockId,
    throw: BlockId,
    jumps: Vec<Jump<'a>>,
    handlers: Vec<(BlockId, usize)>,
    finally: Vec<Finally>,
    labels: Vec<&'a str>,
    case_ends: Vec<(NodeId, BlockId)>,
    /// Block count past which `finally` bodies are no longer built twice (each copy doubles
    /// the nested ones).
    block_budget: usize,
    /// Calls that end the program (sorted).
    exits: &'a [NodeId],
    safe_reads: &'a [NodeId],
    throws: Option<&'a [NodeId]>,
    try_end_to_catch: bool,
    fold_literals: bool,
    dead_continues: &'a [NodeId],
    abrupt_finally_continue_dead: bool,
    static_fields: bool,
    /// While building the abrupt copy of a `finally`: the jump-target depth at its start.
    abrupt_floor: Option<usize>,
    try_ends: Vec<(NodeId, BlockId)>,
}

/// A literal test that is always truthy (`while (true)`).
fn const_true(ast: &Ast, test: NodeId) -> bool {
    match *ast.node(crate::ast_util::skip_parens(ast, test)) {
        Node::BoolLit { value } => value,
        Node::NumberLit { value } => {
            let v = value.get();
            v != 0.0 && !v.is_nan()
        }
        Node::StringLit { value } => !ast.atom(value).is_empty(),
        Node::RegexLit { .. } => true,
        _ => false,
    }
}

pub(crate) fn is_type_kind(k: NodeKind) -> bool {
    use NodeKind::*;
    matches!(
        k,
        KeywordType
            | TypeRef
            | ThisType
            | TemplateLitType
            | ArrayType
            | TupleType
            | NamedTupleMember
            | OptionalType
            | RestType
            | UnionType
            | IntersectionType
            | FunctionType
            | ConstructorType
            | TypeLiteral
            | PropertySignature
            | MethodSignature
            | CallSignature
            | ConstructSignature
            | TypeQuery
            | TypeOperator
            | IndexedAccessType
            | ConditionalType
            | InferType
            | MappedType
            | ParenType
            | TypePredicate
            | ImportType
            | TypeParams
            | TypeParam
            | TypeArgs
            | Decorator
    )
}

/// Expressions that may throw (and so reach a `catch`).
fn may_throw(k: NodeKind) -> bool {
    use NodeKind::*;
    matches!(
        k,
        Ident | Call | New | Member | Index | Yield | Await | TaggedTemplate | ImportCall | Super
    )
}

impl<'a> Builder<'a> {
    fn new(ast: &'a Ast) -> Builder<'a> {
        let mut b = Builder {
            ast,
            blocks: Vec::new(),
            cur: 0,
            exit: 0,
            ret: 0,
            throw: 0,
            jumps: Vec::new(),
            handlers: Vec::new(),
            finally: Vec::new(),
            labels: Vec::new(),
            case_ends: Vec::new(),
            block_budget: 4 * ast.nodes.len() + 4096,
            exits: &[],
            safe_reads: &[],
            throws: None,
            try_end_to_catch: false,
            fold_literals: false,
            dead_continues: &[],
            abrupt_finally_continue_dead: false,
            static_fields: false,
            abrupt_floor: None,
            try_ends: Vec::new(),
        };
        b.cur = b.new_block();
        b.exit = b.new_block();
        b.ret = b.new_block();
        b.throw = b.new_block();
        b
    }

    fn opts(&mut self, o: &Options<'a>) {
        self.exits = o.exits;
        self.safe_reads = o.safe_reads;
        self.throws = o.throws;
        self.try_end_to_catch = o.try_end_to_catch;
        self.fold_literals = o.fold_literals;
        self.dead_continues = o.dead_continues;
        self.abrupt_finally_continue_dead = o.abrupt_finally_continue_dead;
        self.static_fields = o.static_fields;
    }

    /// With an explicit throw list, an implicit throw after `id` when it is listed.
    fn listed_throw(&mut self, id: NodeId) {
        if self.throws.is_some_and(|t| t.binary_search(&id).is_ok()) {
            self.maybe_throw();
        }
    }

    fn finish(mut self) -> Cfg {
        let (cur, exit) = (self.cur, self.exit);
        self.edge(cur, exit);
        let mut reachable = vec![false; self.blocks.len()];
        let mut stack = vec![0];
        reachable[0] = true;
        while let Some(b) = stack.pop() {
            for &t in &self.blocks[b as usize].succ {
                if !reachable[t as usize] {
                    reachable[t as usize] = true;
                    stack.push(t);
                }
            }
        }
        Cfg {
            blocks: self.blocks,
            entry: 0,
            exit: self.exit,
            ret: self.ret,
            case_ends: self.case_ends,
            try_ends: self.try_ends,
            reachable,
        }
    }

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(Block::default());
        (self.blocks.len() - 1) as BlockId
    }

    fn edge(&mut self, from: BlockId, to: BlockId) {
        let s = &mut self.blocks[from as usize].succ;
        if !s.contains(&to) {
            s.push(to);
        }
    }

    fn event(&mut self, id: NodeId) {
        let c = self.cur;
        self.blocks[c as usize].events.push(id);
    }

    /// Jumps from the current block to `target`, passing through enclosing `finally`
    /// blocks deeper than `depth`.
    fn jump(&mut self, target: BlockId, depth: usize) {
        if self.finally.len() > depth {
            let f = self.finally.last_mut().expect("finally frame");
            if !f.routed.contains(&(target, depth)) {
                f.routed.push((target, depth));
            }
            let abrupt = f.abrupt;
            self.edge(self.cur, abrupt);
        } else {
            self.edge(self.cur, target);
        }
    }

    fn throw_target(&self) -> (BlockId, usize) {
        self.handlers.last().copied().unwrap_or((self.throw, 0))
    }

    /// Before an expression that may throw inside a `try`: an edge to the handler.
    fn maybe_throw(&mut self) {
        if self.handlers.is_empty() && self.finally.is_empty() {
            return;
        }
        let (t, d) = self.throw_target();
        self.jump(t, d);
        let next = self.new_block();
        self.edge(self.cur, next);
        self.cur = next;
    }

    fn expr_opt(&mut self, id: Option<NodeId>) {
        if let Some(id) = id {
            self.expr(id);
        }
    }

    /// Evaluates `first`, then `second` only on some paths.
    fn short_circuit(&mut self, first: NodeId, second: NodeId, rhs_runs: Option<bool>) {
        self.expr(first);
        let from = self.cur;
        let rhs = self.new_block();
        if rhs_runs != Some(false) {
            self.edge(from, rhs);
        }
        self.cur = rhs;
        self.expr(second);
        let after = self.new_block();
        self.edge(self.cur, after);
        if rhs_runs != Some(true) {
            self.edge(from, after);
        }
        self.cur = after;
    }

    /// With program-ending calls (the plugin's exit analysis), the truthiness of a literal
    /// test, which decides the branch. Plain code-path graphs never fold.
    fn literal_truth(&self, test: NodeId) -> Option<bool> {
        if self.exits.is_empty() && !self.fold_literals {
            return None;
        }
        let mut id = test;
        loop {
            match *self.ast.node(id) {
                Node::Paren { expr } => id = expr,
                Node::As { expr, .. }
                | Node::Satisfies { expr, .. }
                | Node::NonNull { expr }
                | Node::TypeAssertion { expr, .. }
                    if self.fold_literals =>
                {
                    id = expr
                }
                _ => break,
            }
        }
        match *self.ast.node(id) {
            Node::BoolLit { value } => Some(value),
            Node::NullLit {} => Some(false),
            Node::NumberLit { value } => Some(value.get() != 0.0 && !value.get().is_nan()),
            Node::StringLit { value } => Some(!self.ast.atom(value).is_empty()),
            Node::TemplateLit { parts } if self.fold_literals => {
                let parts = self.ast.list(parts);
                match parts {
                    [only] => match *self.ast.node(*only) {
                        Node::TemplateElement { cooked, .. } => {
                            Some(!self.ast.atom(cooked).is_empty())
                        }
                        _ => None,
                    },
                    [] => Some(false),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// A loop test known to be falsy (only with `fold_literals`).
    fn loop_never_runs(&self, test: NodeId) -> bool {
        self.fold_literals && self.literal_truth(test) == Some(false)
    }

    /// A default value, evaluated only on some paths.
    fn optional(&mut self, e: Option<NodeId>) {
        let Some(e) = e else { return };
        let from = self.cur;
        let b = self.new_block();
        self.edge(from, b);
        self.cur = b;
        self.expr(e);
        let after = self.new_block();
        self.edge(self.cur, after);
        self.edge(from, after);
        self.cur = after;
    }

    /// An assignment target: each binding is written after its default.
    fn target(&mut self, id: NodeId) {
        let ast = self.ast;
        match *ast.node(id) {
            Node::ObjectLit { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::Property { key, value, .. } => {
                            if let Node::ComputedKey { expr } = *ast.node(key) {
                                self.expr(expr);
                            }
                            self.target(value);
                        }
                        Node::ShorthandProp { name, init, .. } => {
                            self.optional(init);
                            self.event(name);
                        }
                        Node::Spread { arg } => self.target(arg),
                        _ => {
                            self.expr(p);
                            continue;
                        }
                    }
                    self.event(p);
                }
            }
            Node::ArrayLit { elements } => {
                for &e in ast.list(elements) {
                    self.target(e);
                }
            }
            Node::Spread { arg } | Node::Paren { expr: arg } | Node::NonNull { expr: arg } => {
                self.target(arg)
            }
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. } => self.target(expr),
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } => {
                self.optional(Some(right));
                self.target(left);
            }
            Node::Ident { .. } | Node::Hole {} => {}
            _ => {
                self.expr(id);
                return;
            }
        }
        self.listed_throw(id);
        self.event(id);
    }

    fn class(
        &mut self,
        decorators: ts_r_ast::NodeList,
        heritage: ts_r_ast::NodeList,
        members: ts_r_ast::NodeList,
    ) {
        let ast = self.ast;
        self.decorators(decorators);
        for &h in ast.list(heritage) {
            if let Node::HeritageClause { types, .. } = *ast.node(h) {
                for &t in ast.list(types) {
                    if let Node::ExprWithTypeArgs { expr, .. } = *ast.node(t) {
                        self.expr(expr);
                    }
                }
            }
        }
        for &m in ast.list(members) {
            let (decorators, key) = match *ast.node(m) {
                Node::PropertyDef {
                    decorators, key, ..
                }
                | Node::MethodDef {
                    decorators, key, ..
                } => (decorators, key),
                _ => continue,
            };
            // Decorator expressions run in the class's code path, before the key.
            self.decorators(decorators);
            if let Node::ComputedKey { expr } = *ast.node(key) {
                self.expr(expr);
            }
        }
        // With exit calls, static blocks run with the class definition, in order (the
        // exit-aware graph needs to see a `process.exit()` there).
        if !self.exits.is_empty() {
            for &m in ast.list(members) {
                match *ast.node(m) {
                    Node::StaticBlock { body } => {
                        for &s in ast.list(body) {
                            self.stmt(s);
                        }
                    }
                    Node::PropertyDef {
                        modifiers,
                        value: Some(v),
                        ..
                    } if self.static_fields && modifiers.contains(ts_r_ast::Modifiers::STATIC) => {
                        self.expr(v)
                    }
                    _ => {}
                }
            }
        }
    }

    fn decorators(&mut self, decorators: ts_r_ast::NodeList) {
        for &d in self.ast.list(decorators) {
            if let Node::Decorator { expr } = *self.ast.node(d) {
                self.expr(expr);
            }
        }
    }

    fn expr(&mut self, id: NodeId) {
        let ast = self.ast;
        if !self.exits.is_empty()
            && self.exits.binary_search(&id).is_ok()
            && let Node::Call { args, .. } = *ast.node(id)
        {
            for &a in ast.list(args) {
                self.expr(a);
            }
            self.event(id);
            self.dead();
            return;
        }
        match *ast.node(id) {
            Node::Binary {
                left,
                op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce),
                right,
            } => {
                let truth = self.literal_truth(left);
                let rhs_runs = match op {
                    BinaryOp::LogicalAnd => truth,
                    BinaryOp::LogicalOr => truth.map(|t| !t),
                    _ => truth.map(|_| {
                        let mut l = left;
                        while let Node::Paren { expr } = *ast.node(l) {
                            l = expr;
                        }
                        matches!(ast.node(l), Node::NullLit {})
                    }),
                };
                self.short_circuit(left, right, rhs_runs);
            }
            Node::Assign {
                left,
                op: AssignOp::AndAssign | AssignOp::OrAssign | AssignOp::CoalesceAssign,
                right,
            } => self.short_circuit(left, right, None),
            Node::Assign { left, right, .. } if matches!(ast.node(left), Node::Ident { .. }) => {
                self.expr(right);
                self.event(left);
            }
            Node::Assign {
                left,
                op: AssignOp::Assign,
                right,
            } if matches!(
                ast.node(left),
                Node::ObjectLit { .. } | Node::ArrayLit { .. }
            ) =>
            {
                self.expr(right);
                self.target(left);
            }
            Node::ObjectPattern { props } => {
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::BindingProperty { key, value, init } => {
                            match value {
                                Some(v) => {
                                    if let Node::ComputedKey { expr } = *ast.node(key) {
                                        self.expr(expr);
                                    }
                                    self.optional(init);
                                    self.expr(v);
                                }
                                None => {
                                    self.optional(init);
                                    self.expr(key);
                                }
                            }
                            self.event(p);
                        }
                        _ => self.expr(p),
                    }
                }
            }
            Node::AssignPattern { left, right } => {
                self.optional(Some(right));
                self.expr(left);
            }
            Node::Conditional { test, cons, alt } => {
                self.expr(test);
                let truth = self.literal_truth(test);
                let from = self.cur;
                let (c, a, after) = (self.new_block(), self.new_block(), self.new_block());
                if truth != Some(false) {
                    self.edge(from, c);
                }
                if truth != Some(true) {
                    self.edge(from, a);
                }
                self.cur = c;
                self.expr(cons);
                self.edge(self.cur, after);
                self.cur = a;
                self.expr(alt);
                self.edge(self.cur, after);
                self.cur = after;
            }
            Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::FunctionDecl { .. } => {}
            Node::ClassExpr {
                decorators,
                heritage,
                members,
                ..
            }
            | Node::ClassDecl {
                decorators,
                heritage,
                members,
                ..
            } => self.class(decorators, heritage, members),
            Node::Call { callee, args, .. } => {
                self.expr(callee);
                for &a in ast.list(args) {
                    self.expr(a);
                }
                if self.throws.is_none() {
                    self.maybe_throw();
                }
            }
            Node::New { callee, args, .. } => {
                self.expr(callee);
                for &a in ast.list(args) {
                    self.expr(a);
                }
                if self.throws.is_none() {
                    self.maybe_throw();
                }
            }
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::NonNull { expr }
            | Node::Instantiation { expr, .. } => self.expr(expr),
            Node::Param { pattern, init, .. } => {
                self.optional(init);
                self.expr(pattern);
            }
            Node::VarDeclarator { name, init, .. } => {
                self.expr_opt(init);
                self.expr(name);
            }
            _ => {
                let mut kids = Vec::new();
                ast.for_each_child(id, |c| kids.push(c));
                for c in kids {
                    if !is_type_kind(ast.kind(c)) {
                        self.expr(c);
                    }
                }
                if self.throws.is_none()
                    && may_throw(ast.kind(id))
                    && self.safe_reads.binary_search(&id).is_err()
                {
                    self.maybe_throw();
                }
                // `generator.return()` resumes a `yield` as a return through `finally`.
                if matches!(ast.node(id), Node::Yield { .. }) && !self.finally.is_empty() {
                    let ret = self.ret;
                    self.jump(ret, 0);
                    let next = self.new_block();
                    self.edge(self.cur, next);
                    self.cur = next;
                }
            }
        }
        self.listed_throw(id);
        self.event(id);
    }

    fn take_labels(&mut self) -> Vec<&'a str> {
        std::mem::take(&mut self.labels)
    }

    fn push_jump(
        &mut self,
        labels: Vec<&'a str>,
        brk: BlockId,
        cont: Option<BlockId>,
        breakable: bool,
    ) {
        let depth = self.finally.len();
        self.jumps.push(Jump {
            labels,
            brk,
            cont,
            breakable,
            depth,
        });
    }

    fn label_name(&self, label: Option<NodeId>) -> Option<&'a str> {
        label.and_then(|l| crate::ast_util::ident_name(self.ast, l))
    }

    /// Ends the current block abruptly; following code starts an unreachable block.
    fn dead(&mut self) {
        self.cur = self.new_block();
    }

    fn stmt(&mut self, id: NodeId) {
        let ast = self.ast;
        // Labels apply only to the statement directly labeled.
        let labels = if matches!(ast.node(id), Node::Labeled { .. }) {
            Vec::new()
        } else {
            self.take_labels()
        };
        self.event(id);
        match *ast.node(id) {
            Node::Block { stmts } | Node::ModuleBlock { stmts } => {
                let after = (!labels.is_empty()).then(|| self.new_block());
                if let Some(a) = after {
                    self.push_jump(labels, a, None, false);
                }
                for &s in ast.list(stmts) {
                    self.stmt(s);
                }
                if let Some(a) = after {
                    self.jumps.pop();
                    self.edge(self.cur, a);
                    self.cur = a;
                }
            }
            Node::Labeled { label, body } => {
                let name = crate::ast_util::ident_name(ast, label).unwrap_or("");
                let mut ls = self.take_labels();
                ls.push(name);
                let is_loop_or_block = matches!(
                    ast.node(body),
                    Node::For { .. }
                        | Node::ForIn { .. }
                        | Node::ForOf { .. }
                        | Node::While { .. }
                        | Node::DoWhile { .. }
                        | Node::Switch { .. }
                        | Node::Block { .. }
                        | Node::Labeled { .. }
                );
                if is_loop_or_block {
                    self.labels = ls;
                    self.stmt(body);
                } else {
                    let after = self.new_block();
                    self.push_jump(ls, after, None, false);
                    self.stmt(body);
                    self.jumps.pop();
                    self.edge(self.cur, after);
                    self.cur = after;
                }
            }
            Node::ExprStmt { expr } => self.expr(expr),
            Node::VarDecl { decls, .. } => {
                for &d in ast.list(decls) {
                    self.expr(d);
                }
            }
            Node::If { test, cons, alt } => {
                self.expr(test);
                let truth = self.literal_truth(test);
                let from = self.cur;
                let then_b = self.new_block();
                if truth != Some(false) {
                    self.edge(from, then_b);
                }
                self.cur = then_b;
                self.stmt(cons);
                let then_end = self.cur;
                let after = self.new_block();
                self.edge(then_end, after);
                match alt {
                    Some(a) => {
                        let else_b = self.new_block();
                        if truth != Some(true) {
                            self.edge(from, else_b);
                        }
                        self.cur = else_b;
                        self.stmt(a);
                        self.edge(self.cur, after);
                    }
                    None if truth == Some(true) => {}
                    None => self.edge(from, after),
                }
                self.cur = after;
            }
            Node::While { test, body } => {
                let head = self.new_block();
                self.edge(self.cur, head);
                self.cur = head;
                self.expr(test);
                let test_end = self.cur;
                let (body_b, after) = (self.new_block(), self.new_block());
                if !self.loop_never_runs(test) {
                    self.edge(test_end, body_b);
                }
                if !const_true(ast, test) {
                    self.edge(test_end, after);
                }
                self.push_jump(labels, after, Some(head), true);
                self.cur = body_b;
                self.stmt(body);
                self.edge(self.cur, head);
                self.jumps.pop();
                self.cur = after;
            }
            Node::DoWhile { body, test } => {
                let (body_b, test_b, after) =
                    (self.new_block(), self.new_block(), self.new_block());
                self.edge(self.cur, body_b);
                self.push_jump(labels, after, Some(test_b), true);
                self.cur = body_b;
                self.stmt(body);
                self.edge(self.cur, test_b);
                self.jumps.pop();
                self.cur = test_b;
                self.expr(test);
                if !self.loop_never_runs(test) {
                    self.edge(self.cur, body_b);
                }
                if !const_true(ast, test) {
                    self.edge(self.cur, after);
                }
                self.cur = after;
            }
            Node::For {
                init,
                test,
                update,
                body,
            } => {
                if let Some(i) = init {
                    if matches!(ast.node(i), Node::VarDecl { .. }) {
                        self.stmt(i);
                    } else {
                        self.expr(i);
                    }
                }
                let head = self.new_block();
                self.edge(self.cur, head);
                self.cur = head;
                self.expr_opt(test);
                let test_end = self.cur;
                let (body_b, upd, after) = (self.new_block(), self.new_block(), self.new_block());
                if !test.is_some_and(|t| self.loop_never_runs(t)) {
                    self.edge(test_end, body_b);
                }
                if test.is_some_and(|t| !const_true(ast, t)) {
                    self.edge(test_end, after);
                }
                self.push_jump(labels, after, Some(upd), true);
                self.cur = body_b;
                self.stmt(body);
                self.edge(self.cur, upd);
                self.jumps.pop();
                self.cur = upd;
                self.expr_opt(update);
                self.edge(self.cur, head);
                self.cur = after;
            }
            Node::ForIn { left, right, body }
            | Node::ForOf {
                left, right, body, ..
            } => {
                self.expr(right);
                let head = self.new_block();
                self.edge(self.cur, head);
                let (body_b, after) = (self.new_block(), self.new_block());
                self.edge(head, body_b);
                self.edge(head, after);
                self.push_jump(labels, after, Some(head), true);
                self.cur = body_b;
                if matches!(ast.node(left), Node::VarDecl { .. }) {
                    self.stmt(left);
                } else {
                    self.target(left);
                }
                self.stmt(body);
                self.edge(self.cur, head);
                self.jumps.pop();
                self.cur = after;
            }
            Node::Switch { disc, cases } => {
                self.expr(disc);
                let after = self.new_block();
                self.push_jump(labels, after, None, true);
                let mut test_b = self.cur;
                let mut prev_end: Option<BlockId> = None;
                let mut default_entry = None;
                for &c in ast.list(cases) {
                    let Node::SwitchCase { test, body } = *ast.node(c) else {
                        continue;
                    };
                    let body_b = self.new_block();
                    match test {
                        Some(t) => {
                            self.cur = test_b;
                            self.expr(t);
                            self.edge(self.cur, body_b);
                            let next = self.new_block();
                            self.edge(self.cur, next);
                            test_b = next;
                        }
                        None => default_entry = Some(body_b),
                    }
                    if let Some(pe) = prev_end {
                        self.edge(pe, body_b);
                    }
                    self.cur = body_b;
                    self.event(c);
                    for &s in ast.list(body) {
                        self.stmt(s);
                    }
                    self.case_ends.push((c, self.cur));
                    prev_end = Some(self.cur);
                }
                self.edge(test_b, default_entry.unwrap_or(after));
                if let Some(pe) = prev_end {
                    self.edge(pe, after);
                }
                self.jumps.pop();
                self.cur = after;
            }
            Node::Return { arg } => {
                self.expr_opt(arg);
                let ret = self.ret;
                self.jump(ret, 0);
                self.dead();
            }
            Node::Throw { arg } => {
                self.expr(arg);
                let (t, d) = self.throw_target();
                self.jump(t, d);
                self.dead();
            }
            Node::Break { label } | Node::Continue { label } => {
                let is_break = matches!(ast.node(id), Node::Break { .. });
                let name = self.label_name(label);
                let target = self.jumps.iter().enumerate().rev().find_map(|(index, j)| {
                    let ok = match name {
                        Some(n) => j.labels.contains(&n),
                        None => {
                            if is_break {
                                j.breakable
                            } else {
                                j.cont.is_some()
                            }
                        }
                    };
                    if !ok {
                        return None;
                    }
                    let t = if is_break { Some(j.brk) } else { j.cont };
                    t.map(|t| (t, j.depth, index))
                });
                // A `continue` that does not loop again (plugin model) still runs the
                // `finally` blocks on its way, then goes nowhere.
                let no_loop = !is_break
                    && (self.dead_continues.binary_search(&id).is_ok()
                        || target.is_some_and(|(_, _, index)| {
                            self.abrupt_finally_continue_dead
                                && self.abrupt_floor.is_some_and(|floor| index < floor)
                        }));
                if let Some((t, d, _)) = target {
                    let t = if no_loop { self.new_block() } else { t };
                    self.jump(t, d);
                }
                self.dead();
            }
            Node::Try {
                block,
                handler,
                finalizer,
            } => {
                let after = self.new_block();
                let catch_entry = handler.map(|_| self.new_block());
                if finalizer.is_some() {
                    let abrupt = self.new_block();
                    self.finally.push(Finally {
                        abrupt,
                        routed: Vec::new(),
                    });
                }
                if let Some(ce) = catch_entry {
                    let depth = self.finally.len();
                    self.handlers.push((ce, depth));
                }
                self.stmt(block);
                self.try_ends.push((id, self.cur));
                let mut ends = vec![self.cur];
                if let (Some(h), Some(ce)) = (handler, catch_entry) {
                    self.handlers.pop();
                    if self.try_end_to_catch {
                        let c = self.cur;
                        self.edge(c, ce);
                    }
                    self.cur = ce;
                    self.event(h);
                    if let Node::CatchClause { param, body, .. } = *ast.node(h) {
                        self.expr_opt(param);
                        self.stmt(body);
                    }
                    ends.push(self.cur);
                }
                match finalizer {
                    Some(fin) => {
                        let frame = self.finally.pop().expect("finally frame");
                        let normal = self.new_block();
                        for e in ends {
                            self.edge(e, normal);
                        }
                        self.cur = normal;
                        self.stmt(fin);
                        self.edge(self.cur, after);
                        if !frame.routed.is_empty() {
                            let end = if self.blocks.len() > self.block_budget {
                                // Over budget: abrupt completion shares the normal copy (less
                                // precise, but the graph stays linear in the source).
                                self.edge(frame.abrupt, normal);
                                self.cur
                            } else {
                                self.cur = frame.abrupt;
                                let floor = self.abrupt_floor.replace(self.jumps.len());
                                self.stmt(fin);
                                self.abrupt_floor = floor;
                                self.cur
                            };
                            for (t, d) in frame.routed {
                                self.cur = end;
                                self.jump(t, d);
                            }
                        }
                    }
                    None => {
                        for e in ends {
                            self.edge(e, after);
                        }
                    }
                }
                self.cur = after;
            }
            Node::With { obj, body } => {
                self.expr(obj);
                self.stmt(body);
            }
            Node::ClassDecl {
                decorators,
                heritage,
                members,
                ..
            } => self.class(decorators, heritage, members),
            Node::ExportDefaultExpr { expr } | Node::ExportAssignment { expr } => self.expr(expr),
            Node::Enum { members, .. } => {
                for &m in ast.list(members) {
                    if let Node::EnumMember { init: Some(i), .. } = *ast.node(m) {
                        self.expr(i);
                    }
                }
            }
            Node::Module { body: Some(b), .. } => self.stmt(b),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cfg;
    use ts_r_ast::{Node, SourceKind};

    /// `T(n) = try { throw 0; } finally { T(n-1) }`: building every `finally` twice would
    /// make 2^25 copies of the innermost one.
    #[test]
    fn nested_finally_stays_linear() {
        let mut body = "x();".to_owned();
        for _ in 0..25 {
            body = format!("try {{ throw 0; }} finally {{ {body} }}");
        }
        let src = format!("function f() {{ {body} }}");
        let parsed = ts_r_parser::parse(&src, SourceKind::Ts);
        let Node::Program { stmts } = *parsed.ast.node(parsed.ast.root) else {
            panic!("program");
        };
        let cfg = Cfg::function(&parsed.ast, parsed.ast.list(stmts)[0]);
        assert!(cfg.blocks.len() < 100_000, "{} blocks", cfg.blocks.len());
    }
}
