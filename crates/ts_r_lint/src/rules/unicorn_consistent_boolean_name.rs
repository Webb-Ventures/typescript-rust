//! `unicorn/consistent-boolean-name`: variables, parameters and functions whose value is a
//! boolean (or that return one) start with a boolean prefix (`is`, `has`, `can`, ...), and a
//! name with such a prefix holds a boolean. A prefix counts when the name (leading `_`s
//! dropped) starts with it followed by an uppercase letter, a digit or `_`, or with its
//! uppercase form and `_`. A React hook binding (`useX` holding a function) is checked on
//! `x`.
//!
//! Every binding is classified as boolean, non-boolean or unknown from its declaration and
//! its writes: literals and operators, calls of known predicates (`Array.isArray`,
//! `Boolean`, `.has()` on a `Map`, `.test()` on a regex, ...), `const` bindings and functions
//! they name, the returned expression of a function, and TypeScript annotations (aliases,
//! interfaces with call signatures, generics with their arguments, `Promise<T>` returns).
//! A write that is not a plain or logical assignment makes a binding non-boolean, any
//! unknown part makes it unknown. A boolean binding without a prefix is reported with a
//! rename suggestion per prefix (fixed with the first prefix for a local, non-exported
//! variable that does not hold a function); a prefixed binding that is non-boolean is
//! reported without a fix. With `checkMethods`/`checkFields`, object and class members,
//! parameter properties and interface or type-literal signatures are checked the same way
//! (no suggestions); methods, getters and signatures count once per owner and name, merged
//! interfaces sharing an owner, and overload signatures decide over the implementation.
//! In JavaScript, a JSDoc comment right before a binding, parameter or function supplies
//! the same facts: `@type`, `@param` and `@returns` types are parsed (unions, intersections,
//! nullable and optional markers, function types, generics) and classified the same way;
//! a type the parser cannot read makes the binding unknown. Type information from a checker
//! is not used, and the `wrappers` option is not read.

use super::prelude::*;
use super::unicorn_names::{RegexOpt, regex_list};
use super::unicorn_static::{
    Static, evaluated_children, is_global_ident, static_value_no_side_effects,
};
use super::unicorn_util_h4::{available_name, estree_ident_span, rename_symbol_fix};
use crate::semantic::Decl;
use ts_r_ast::CommentKind;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/consistent-boolean-name",
        "Enforce consistent naming for boolean values.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<ConsistentBooleanName>,
};

const DEFAULT_PREFIXES: &[&str] = &[
    "is", "are", "has", "have", "can", "should", "was", "were", "does", "did", "will", "requires",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Always,
    Prohibit,
    Never,
}

fn mode(o: &serde_json::Map<String, Value>, key: &str, default: Mode) -> Mode {
    match o.get(key).and_then(Value::as_str) {
        Some("always") => Mode::Always,
        Some("prohibit") => Mode::Prohibit,
        Some("never") => Mode::Never,
        _ => default,
    }
}

#[derive(Debug)]
struct ConsistentBooleanName {
    check_variables: Mode,
    check_arguments: Mode,
    check_functions: Mode,
    check_methods: Mode,
    check_fields: Mode,
    prefixes: Vec<String>,
    ignore: Vec<RegexOpt>,
}

impl FromOptions for ConsistentBooleanName {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        if o.contains_key("checkProperties") {
            return Err(
                "`checkProperties` was removed. Use `checkMethods` and `checkFields` instead."
                    .into(),
            );
        }
        let mut entries: Vec<(String, bool)> = DEFAULT_PREFIXES
            .iter()
            .map(|p| ((*p).to_owned(), true))
            .collect();
        if let Some(m) = o.get("prefixes").and_then(Value::as_object) {
            for (k, v) in m {
                let on = v.as_bool().unwrap_or(!v.is_null());
                match entries.iter_mut().find(|(n, _)| n == k) {
                    Some(e) => e.1 = on,
                    None => entries.push((k.clone(), on)),
                }
            }
        }
        Ok(ConsistentBooleanName {
            check_variables: mode(o, "checkVariables", Mode::Always),
            check_arguments: mode(o, "checkArguments", Mode::Always),
            check_functions: mode(o, "checkFunctions", Mode::Always),
            check_methods: mode(o, "checkMethods", Mode::Never),
            check_fields: mode(o, "checkFields", Mode::Never),
            prefixes: entries
                .into_iter()
                .filter(|(_, on)| *on)
                .map(|(n, _)| n)
                .collect(),
            ignore: regex_list(o.get("ignore"), "ignore")?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    Bool,
    Non,
    Unk,
}

fn combine(states: &[St]) -> St {
    if states.is_empty() || states.contains(&St::Unk) {
        St::Unk
    } else if states.iter().all(|&s| s == St::Bool) {
        St::Bool
    } else {
        St::Non
    }
}

fn combine_with_writes(states: &[St]) -> St {
    if states.contains(&St::Non) {
        St::Non
    } else {
        combine(states)
    }
}

fn strip_underscores(name: &str) -> &str {
    name.trim_start_matches('_')
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_lowercase().chain(c).collect())
        .unwrap_or_default()
}

fn is_screaming(name: &str) -> bool {
    let n = strip_underscores(name);
    let mut c = n.chars();
    c.next().is_some_and(|f| f.is_ascii_uppercase())
        && c.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn boolean_prefix<'p>(name: &str, prefixes: &'p [String]) -> Option<&'p str> {
    let name = strip_underscores(name);
    prefixes
        .iter()
        .find(|p| {
            name.starts_with(&format!("{}_", p.to_uppercase()))
                || (name.starts_with(p.as_str())
                    && name[p.len()..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        })
        .map(String::as_str)
}

fn replacement_name(name: &str, prefix: &str) -> String {
    let rest = strip_underscores(name);
    let lead = &name[..name.len() - rest.len()];
    if rest == rest.to_uppercase() {
        format!("{lead}{}_{rest}", prefix.to_uppercase())
    } else {
        format!("{lead}{prefix}{}", upper_first(rest))
    }
}

fn hook_replacement_name(name: &str, checked: &str, prefix: &str) -> String {
    if name == checked {
        return replacement_name(name, prefix);
    }
    let hook = if is_screaming(checked) {
        replacement_name(checked, prefix)
    } else {
        format!("{prefix}{}", upper_first(checked))
    };
    format!("use{}", upper_first(&hook))
}

fn is_react_hook_name(name: &str) -> bool {
    name == "use"
        || name
            .strip_prefix("use")
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// A binding's declaration as the plugin supports it.
#[derive(Clone, Copy)]
enum Def {
    /// `name` is the declarator's own identifier.
    Var {
        declarator: NodeId,
        decl: NodeId,
    },
    /// `func` is the owner of the parameter list (a function or a signature).
    Param {
        param: NodeId,
        func: Option<NodeId>,
    },
    Func {
        func: NodeId,
    },
}

/// Type parameter bindings: a persistent list of `name -> (type, env)` frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Env(u32);
const EMPTY: Env = Env(0);

struct Frame {
    name: String,
    value: Option<(NodeId, Env)>,
    parent: Env,
}

#[derive(Clone, Copy)]
struct Ts {
    fn_bool: bool,
    nullish: bool,
    env: Env,
}

const TS: Ts = Ts {
    fn_bool: true,
    nullish: true,
    env: EMPTY,
};

impl Ts {
    fn fb(self, fn_bool: bool) -> Ts {
        Ts { fn_bool, ..self }
    }
    fn nl(self, nullish: bool) -> Ts {
        Ts { nullish, ..self }
    }
    fn at(self, env: Env) -> Ts {
        Ts { env, ..self }
    }
}

const MAX_DEPTH: u32 = 120;

struct An<'c, 'x, 'f> {
    cx: &'c Ctx<'x, 'f>,
    frames: Vec<Frame>,
    visiting_types: Vec<NodeId>,
    visiting_syms: Vec<SymbolId>,
    depth: u32,
    /// The file has a `/**` comment.
    jsdoc: bool,
}

fn unwrap_type(ast: &ts_r_ast::Ast, mut id: NodeId) -> NodeId {
    for _ in 0..64 {
        match *ast.node(id) {
            Node::ParenType { ty } => id = ty,
            _ => break,
        }
    }
    id
}

fn fn_flags(ast: &ts_r_ast::Ast, id: NodeId) -> Option<FnFlags> {
    match *ast.node(id) {
        Node::FunctionDecl { flags, .. }
        | Node::FunctionExpr { flags, .. }
        | Node::Arrow { flags, .. } => Some(flags),
        _ => None,
    }
}

fn fn_return_type(ast: &ts_r_ast::Ast, id: NodeId) -> Option<NodeId> {
    match *ast.node(id) {
        Node::FunctionDecl { return_type, .. }
        | Node::FunctionExpr { return_type, .. }
        | Node::Arrow { return_type, .. } => return_type,
        _ => None,
    }
}

fn is_nullish_type(ast: &ts_r_ast::Ast, id: NodeId) -> bool {
    matches!(
        *ast.node(id),
        Node::KeywordType {
            kind: KeywordTypeKind::Null | KeywordTypeKind::Undefined
        } | Node::NullLit {}
    )
}

impl<'c, 'x, 'f> An<'c, 'x, 'f> {
    fn new(cx: &'c Ctx<'x, 'f>) -> Self {
        An {
            cx,
            frames: vec![Frame {
                name: String::new(),
                value: None,
                parent: EMPTY,
            }],
            visiting_types: Vec::new(),
            visiting_syms: Vec::new(),
            depth: 0,
            jsdoc: cx.ast().comments.iter().any(|c| {
                c.kind == CommentKind::Block && cx.src()[c.span.start as usize..].starts_with("/**")
            }),
        }
    }

    fn ast(&self) -> &'x ts_r_ast::Ast {
        self.cx.ast()
    }

    fn enter(&mut self) -> bool {
        if self.depth >= MAX_DEPTH {
            return false;
        }
        self.depth += 1;
        true
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // ---- bindings ----

    fn sym_of_ref(&self, ident: NodeId) -> Option<SymbolId> {
        let sem = self.cx.semantic();
        sem.reference_at(ident)
            .and_then(|r| sem.reference(r).symbol())
            .or_else(|| sem.symbol_of_decl(ident))
    }

    fn function_defs(&self, sym: SymbolId) -> Option<Vec<NodeId>> {
        let s = self.cx.semantic().symbol(sym);
        if s.decls.len() <= 1 || s.decls.iter().any(|d| d.kind != DeclKind::Function) {
            return None;
        }
        let ast = self.ast();
        let overloads: Vec<NodeId> = s
            .decls
            .iter()
            .filter(|d| matches!(*ast.node(d.node), Node::FunctionDecl { body: None, .. }))
            .map(|d| d.node)
            .collect();
        Some(if overloads.is_empty() {
            s.decls.iter().map(|d| d.node).collect()
        } else {
            overloads
        })
    }

    fn has_generator_impl(&self, sym: SymbolId) -> bool {
        let ast = self.ast();
        self.cx
            .semantic()
            .symbol(sym)
            .decls
            .iter()
            .any(|d| fn_flags(ast, d.node).is_some_and(|f| f.contains(FnFlags::GENERATOR)))
    }

    fn is_setter_param(&self, param: NodeId) -> bool {
        let ast = self.ast();
        let Some(f) = self.cx.parent(param) else {
            return false;
        };
        let Some(m) = self.cx.parent(f) else {
            return false;
        };
        matches!(
            *ast.node(m),
            Node::MethodDef {
                kind: MethodKind::Set,
                ..
            }
        ) || matches!(
            *ast.node(f),
            Node::MethodSignature {
                kind: MethodKind::Set,
                ..
            }
        )
    }

    fn supported_def(&self, sym: SymbolId) -> Option<Def> {
        let s = self.cx.semantic().symbol(sym);
        let [d] = s.decls[..] else {
            return None;
        };
        let ast = self.ast();
        if !matches!(*ast.node(d.name), Node::Ident { .. }) {
            return None;
        }
        match d.kind {
            DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                match *ast.node(d.node) {
                    Node::VarDeclarator { name, .. } if name == d.name => Some(Def::Var {
                        declarator: d.node,
                        decl: d.parent,
                    }),
                    _ => None,
                }
            }
            DeclKind::Param => {
                let Node::Param { pattern, .. } = *ast.node(d.node) else {
                    return None;
                };
                if pattern != d.name || self.is_setter_param(d.node) {
                    return None;
                }
                let func = self
                    .cx
                    .parent(d.node)
                    .filter(|&f| fn_flags(ast, f).is_some());
                Some(Def::Param {
                    param: d.node,
                    func,
                })
            }
            DeclKind::Function | DeclKind::FunctionExprName => Some(Def::Func { func: d.node }),
            _ => None,
        }
    }

    fn decl(&self, sym: SymbolId) -> Decl {
        self.cx.semantic().symbol(sym).decls[0]
    }

    fn type_ann_of_name(&self, sym: SymbolId) -> Option<NodeId> {
        let d = self.decl(sym);
        match *self.ast().node(d.node) {
            Node::VarDeclarator { type_ann, .. } | Node::Param { type_ann, .. } => type_ann,
            _ => None,
        }
    }

    fn has_write_after_init(&self, sym: SymbolId) -> bool {
        let sem = self.cx.semantic();
        sem.symbol(sym).refs.iter().any(|&r| {
            let f = sem.reference(r).flags;
            f.contains(RefFlags::WRITE) && !f.contains(RefFlags::INIT)
        })
    }

    fn has_any_write(&self, sym: SymbolId) -> bool {
        let sem = self.cx.semantic();
        sem.symbol(sym)
            .refs
            .iter()
            .any(|&r| sem.reference(r).is_write())
    }

    fn simple_const_init(&self, sym: SymbolId) -> Option<Option<NodeId>> {
        let Def::Var { declarator, decl } = self.supported_def(sym)? else {
            return None;
        };
        let ast = self.ast();
        if !matches!(
            *ast.node(decl),
            Node::VarDecl {
                kind: VarKind::Const,
                ..
            }
        ) {
            return None;
        }
        match *ast.node(declarator) {
            Node::VarDeclarator { init, .. } => Some(init),
            _ => None,
        }
    }

    fn var_init(&self, declarator: NodeId) -> Option<NodeId> {
        match *self.ast().node(declarator) {
            Node::VarDeclarator { init, .. } => init,
            _ => None,
        }
    }

    // ---- type-level lookups ----

    fn type_ref_name(&self, ty: NodeId) -> Option<&'x str> {
        let ast = self.ast();
        match *ast.node(ty) {
            Node::TypeRef { name, .. } => match *ast.node(name) {
                Node::Ident { name } => Some(ast.atom(name)),
                _ => None,
            },
            _ => None,
        }
    }

    fn type_args(&self, ty: NodeId) -> Vec<NodeId> {
        let ast = self.ast();
        let args = match *ast.node(ty) {
            Node::TypeRef { type_args, .. } | Node::ExprWithTypeArgs { type_args, .. } => type_args,
            _ => None,
        };
        match args.map(|a| ast.node(a)) {
            Some(&Node::TypeArgs { args }) => ast.list(args).to_vec(),
            _ => Vec::new(),
        }
    }

    /// The type declarations (`type`, `interface`, type parameter) `name` resolves to at `at`.
    fn type_defs_named(&self, name: &str, at: NodeId) -> Vec<NodeId> {
        let sem = self.cx.semantic();
        let scope = sem.enclosing_scope(at);
        let Some(sym) = sem.lookup(scope, name) else {
            return Vec::new();
        };
        sem.symbol(sym)
            .decls
            .iter()
            .filter(|d| {
                matches!(
                    d.kind,
                    DeclKind::TypeAlias | DeclKind::Interface | DeclKind::TypeParam
                )
            })
            .map(|d| d.node)
            .collect()
    }

    fn type_defs(&self, ty: NodeId) -> Vec<NodeId> {
        match self.type_ref_name(ty) {
            Some(n) => self.type_defs_named(n, ty),
            None => Vec::new(),
        }
    }

    fn interface_defs(&self, ty: NodeId) -> Vec<NodeId> {
        let ast = self.ast();
        self.type_defs(ty)
            .into_iter()
            .filter(|&d| matches!(ast.node(d), Node::Interface { .. }))
            .collect()
    }

    fn type_params_of(&self, def: NodeId) -> Vec<NodeId> {
        let ast = self.ast();
        let tp = match *ast.node(def) {
            Node::TypeAlias { type_params, .. } | Node::Interface { type_params, .. } => {
                type_params
            }
            _ => None,
        };
        match tp.map(|t| ast.node(t)) {
            Some(&Node::TypeParams { params }) => ast.list(params).to_vec(),
            _ => Vec::new(),
        }
    }

    fn missing_type_args(&self, ty: Option<NodeId>) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        let ty = unwrap_type(self.ast(), ty);
        if !matches!(self.ast().node(ty), Node::TypeRef { .. }) {
            return false;
        }
        let n = self.type_args(ty).len();
        let ast = self.ast();
        self.type_defs(ty).into_iter().any(|d| {
            self.type_params_of(d).iter().enumerate().any(|(i, &p)| {
                i >= n && matches!(*ast.node(p), Node::TypeParam { default: None, .. })
            })
        })
    }

    fn lookup_env(&self, mut env: Env, name: &str) -> Option<(NodeId, Env)> {
        for _ in 0..10_000 {
            if env == EMPTY {
                return None;
            }
            let f = &self.frames[env.0 as usize];
            if f.name == name {
                return f.value;
            }
            env = f.parent;
        }
        None
    }

    fn bind(&mut self, parent: Env, name: String, value: Option<(NodeId, Env)>) -> Env {
        if self.frames.len() > 100_000 {
            return parent;
        }
        self.frames.push(Frame {
            name,
            value,
            parent,
        });
        Env(self.frames.len() as u32 - 1)
    }

    /// The environment of `def`'s body when referenced with `args` from `env`.
    fn instantiate(&mut self, def: NodeId, args: &[NodeId], env: Env) -> Env {
        let ast = self.ast();
        let mut out = env;
        for (i, p) in self.type_params_of(def).into_iter().enumerate() {
            let Node::TypeParam { name, default, .. } = *ast.node(p) else {
                continue;
            };
            let name = self.cx.text(name).to_owned();
            let value = match (args.get(i), default) {
                (Some(&a), _) => Some((a, env)),
                (None, Some(d)) => Some((d, out)),
                (None, None) => None,
            };
            out = self.bind(out, name, value);
        }
        out
    }

    /// What a type parameter reference stands for.
    fn resolve_param(&self, ty: NodeId, env: Env) -> Option<(NodeId, Env)> {
        let mut cur = self.lookup_env(env, self.type_ref_name(ty)?)?;
        for _ in 0..32 {
            let Some(name) = self.type_ref_name(cur.0) else {
                break;
            };
            match self.lookup_env(cur.1, name) {
                Some(next) if next.0 != cur.0 => cur = next,
                _ => break,
            }
        }
        Some(cur)
    }

    fn has_unresolved_param_ref(&self, ty: Option<NodeId>, env: Env, check_root: bool) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        let ast = self.ast();
        let mut stack = vec![(ty, check_root)];
        let mut steps = 0;
        while let Some((n, check)) = stack.pop() {
            steps += 1;
            if steps > 10_000 {
                return false;
            }
            if check
                && self.type_ref_name(n).is_some()
                && self.resolve_param(n, env).is_none()
                && self
                    .type_defs(n)
                    .iter()
                    .any(|&d| matches!(ast.node(d), Node::TypeParam { .. }))
            {
                return true;
            }
            ast.for_each_child(n, |c| stack.push((c, true)));
        }
        false
    }

    fn is_global_promise_ref(&self, ty: NodeId) -> bool {
        matches!(self.type_ref_name(ty), Some("Promise" | "PromiseLike"))
            && self.type_args(ty).len() == 1
            && self.type_defs(ty).is_empty()
    }

    fn is_global_promise_ann(&self, ty: Option<NodeId>) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        let ast = self.ast();
        let ty = unwrap_type(ast, ty);
        if let Node::UnionType { types } = *ast.node(ty) {
            let rest: Vec<NodeId> = ast
                .list(types)
                .iter()
                .copied()
                .filter(|&t| !is_nullish_type(ast, t))
                .collect();
            return !rest.is_empty() && rest.iter().all(|&t| self.is_global_promise_ann(Some(t)));
        }
        self.is_global_promise_ref(ty)
    }

    fn call_sigs(&self, members: &[NodeId]) -> Vec<Option<NodeId>> {
        let ast = self.ast();
        members
            .iter()
            .filter_map(|&m| match *ast.node(m) {
                Node::CallSignature { return_type, .. } => Some(return_type),
                _ => None,
            })
            .collect()
    }

    fn interface_parts(&self, iface: NodeId) -> (Vec<NodeId>, Vec<NodeId>) {
        let ast = self.ast();
        match *ast.node(iface) {
            Node::Interface { body, extends, .. } => {
                (ast.list(body).to_vec(), ast.list(extends).to_vec())
            }
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn heritage_defs(&self, h: NodeId) -> Vec<NodeId> {
        let ast = self.ast();
        let Node::ExprWithTypeArgs { expr, .. } = *ast.node(h) else {
            return Vec::new();
        };
        match *ast.node(expr) {
            Node::Ident { name } => self.type_defs_named(ast.atom(name), h),
            _ => Vec::new(),
        }
    }

    // ---- syntactic type predicates (no type arguments) ----

    fn ref_alias_or_iface(&mut self, ty: NodeId) -> Option<NodeId> {
        let d = *self.type_defs(ty).first()?;
        if self.visiting_types.contains(&d) {
            return None;
        }
        Some(d)
    }

    fn is_boolean_type_ann(&mut self, ty: Option<NodeId>) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        if !self.enter() {
            return false;
        }
        let ast = self.ast();
        let ty = unwrap_type(ast, ty);
        let r = match *ast.node(ty) {
            Node::KeywordType {
                kind: KeywordTypeKind::Boolean,
            }
            | Node::BoolLit { .. } => true,
            Node::TypeRef { .. } => match self.ref_alias_or_iface(ty) {
                Some(d) => match *ast.node(d) {
                    Node::TypeAlias { ty: body, .. } => {
                        self.visiting_types.push(d);
                        let r = self.is_boolean_type_ann(Some(body));
                        self.visiting_types.pop();
                        r
                    }
                    _ => false,
                },
                None => false,
            },
            Node::TypePredicate { asserts, .. } => !asserts,
            Node::UnionType { types } => ast
                .list(types)
                .to_vec()
                .into_iter()
                .all(|t| self.is_boolean_type_ann(Some(t))),
            _ => false,
        };
        self.leave();
        r
    }

    fn is_function_type_ann(&mut self, ty: Option<NodeId>) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        if !self.enter() {
            return false;
        }
        let ast = self.ast();
        let ty = unwrap_type(ast, ty);
        let r = match *ast.node(ty) {
            Node::FunctionType { .. } => true,
            Node::TypeLiteral { members } => !self.call_sigs(ast.list(members)).is_empty(),
            Node::TypeRef { .. } => match self.ref_alias_or_iface(ty) {
                Some(d) => match *ast.node(d) {
                    Node::TypeAlias { ty: body, .. } => {
                        self.visiting_types.push(d);
                        let r = self.is_function_type_ann(Some(body));
                        self.visiting_types.pop();
                        r
                    }
                    Node::Interface { body, .. } => !self.call_sigs(ast.list(body)).is_empty(),
                    _ => false,
                },
                None => false,
            },
            Node::UnionType { types } => {
                let rest: Vec<NodeId> = ast
                    .list(types)
                    .iter()
                    .copied()
                    .filter(|&t| !is_nullish_type(ast, t))
                    .collect();
                !rest.is_empty() && rest.into_iter().all(|t| self.is_function_type_ann(Some(t)))
            }
            _ => false,
        };
        self.leave();
        r
    }

    fn is_boolean_fn_type_ann(&mut self, ty: Option<NodeId>) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        if !self.enter() {
            return false;
        }
        let ast = self.ast();
        let ty = unwrap_type(ast, ty);
        let sigs_bool = |this: &mut Self, members: &[NodeId]| {
            let sigs = this.call_sigs(members);
            !sigs.is_empty() && sigs.into_iter().all(|r| this.is_boolean_type_ann(r))
        };
        let r = match *ast.node(ty) {
            Node::FunctionType { return_type, .. } => self.is_boolean_type_ann(Some(return_type)),
            Node::TypeLiteral { members } => sigs_bool(self, ast.list(members)),
            Node::TypeRef { .. } => match self.ref_alias_or_iface(ty) {
                Some(d) => match *ast.node(d) {
                    Node::TypeAlias { ty: body, .. } => {
                        self.visiting_types.push(d);
                        let r = self.is_boolean_fn_type_ann(Some(body));
                        self.visiting_types.pop();
                        r
                    }
                    Node::Interface { body, .. } => sigs_bool(self, ast.list(body)),
                    _ => false,
                },
                None => false,
            },
            Node::UnionType { types } => ast
                .list(types)
                .to_vec()
                .into_iter()
                .all(|t| self.is_boolean_fn_type_ann(Some(t))),
            _ => false,
        };
        self.leave();
        r
    }

    // ---- type annotation states (with type arguments) ----

    fn callable(&mut self, ty: Option<NodeId>, ts: Ts) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        if self.missing_type_args(Some(ty)) || !self.enter() {
            return false;
        }
        let r = self.callable_inner(ty, ts);
        self.leave();
        r
    }

    fn callable_inner(&mut self, ty: NodeId, ts: Ts) -> bool {
        if let Some((t, env)) = self.resolve_param(ty, ts.env) {
            return self.callable(Some(t), ts.at(env));
        }
        if self.is_function_type_ann(Some(ty)) {
            return true;
        }
        let ast = self.ast();
        match *ast.node(ty) {
            Node::ParenType { ty } => self.callable(Some(ty), ts),
            Node::UnionType { types } => {
                let rest: Vec<NodeId> = ast
                    .list(types)
                    .iter()
                    .copied()
                    .filter(|&t| !is_nullish_type(ast, t))
                    .collect();
                !rest.is_empty() && rest.into_iter().all(|t| self.callable(Some(t), ts))
            }
            Node::IntersectionType { types } => ast
                .list(types)
                .to_vec()
                .into_iter()
                .any(|t| self.callable(Some(t), ts)),
            Node::TypeRef { .. } => {
                if self.visiting_types.contains(&ty) {
                    return false;
                }
                let args = self.type_args(ty);
                self.visiting_types.push(ty);
                let mut r = false;
                for d in self.type_defs(ty) {
                    let env = self.instantiate(d, &args, ts.env);
                    let hit = match *ast.node(d) {
                        Node::Interface { .. } => !self
                            .interface_sig_states(d, ts.at(env), Ret::Unknown, &mut Vec::new())
                            .is_empty(),
                        Node::TypeAlias { ty: body, .. } => self.callable(Some(body), ts.at(env)),
                        _ => false,
                    };
                    if hit {
                        r = true;
                        break;
                    }
                }
                self.visiting_types.pop();
                r
            }
            _ => false,
        }
    }

    fn ret_state(&mut self, how: Ret, ret: Option<NodeId>, ts: Ts) -> St {
        match how {
            Ret::Unknown => St::Unk,
            Ret::Plain => self.type_state(ret, ts.fb(false)),
            Ret::Promised => self.promised_state(ret, ts.fb(false)),
        }
    }

    fn interface_sig_states(
        &mut self,
        iface: NodeId,
        ts: Ts,
        how: Ret,
        seen: &mut Vec<NodeId>,
    ) -> Vec<St> {
        if seen.contains(&iface) || !self.enter() {
            return Vec::new();
        }
        seen.push(iface);
        let ast = self.ast();
        let (body, extends) = self.interface_parts(iface);
        let sigs = self.call_sigs(&body);
        let mut out: Vec<St> = if ts.fn_bool {
            sigs.into_iter()
                .map(|r| self.ret_state(how, r, ts))
                .collect()
        } else if !sigs.is_empty() {
            vec![St::Non]
        } else {
            Vec::new()
        };
        for h in extends {
            let args = self.type_args(h);
            for d in self.heritage_defs(h) {
                let env = self.instantiate(d, &args, ts.env);
                match *ast.node(d) {
                    Node::Interface { .. } => {
                        let s = self.interface_sig_states(d, ts.at(env), how, seen);
                        out.extend(s);
                    }
                    Node::TypeAlias { ty: body, .. } => {
                        for (r, renv) in self.call_sig_returns(Some(body), ts.at(env)) {
                            let s = self.ret_state(how, Some(r), ts.at(renv));
                            out.push(s);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.leave();
        out
    }

    fn call_sig_returns(&mut self, ty: Option<NodeId>, ts: Ts) -> Vec<(NodeId, Env)> {
        let Some(ty) = ty else {
            return Vec::new();
        };
        if self.missing_type_args(Some(ty)) || !self.enter() {
            return Vec::new();
        }
        let ast = self.ast();
        let resolve = |this: &Self, r: NodeId| this.resolve_param(r, ts.env).unwrap_or((r, ts.env));
        let out = match *ast.node(ty) {
            Node::FunctionType { return_type, .. } => vec![resolve(self, return_type)],
            Node::TypeLiteral { members } => self
                .call_sigs(ast.list(members))
                .into_iter()
                .flatten()
                .map(|r| resolve(self, r))
                .collect(),
            Node::IntersectionType { types } => {
                let mut out = Vec::new();
                for t in ast.list(types).to_vec() {
                    out.extend(self.call_sig_returns(Some(t), ts));
                }
                out
            }
            Node::Interface { .. } => {
                let (body, extends) = self.interface_parts(ty);
                let mut out: Vec<(NodeId, Env)> = self
                    .call_sigs(&body)
                    .into_iter()
                    .flatten()
                    .map(|r| resolve(self, r))
                    .collect();
                for h in extends {
                    let args = self.type_args(h);
                    for d in self.heritage_defs(h) {
                        let env = self.instantiate(d, &args, ts.env);
                        out.extend(self.def_call_sig_returns(d, ts.at(env)));
                    }
                }
                out
            }
            Node::TypeRef { .. } if !self.visiting_types.contains(&ty) => {
                let args = self.type_args(ty);
                self.visiting_types.push(ty);
                let mut out = Vec::new();
                for d in self.type_defs(ty) {
                    let env = self.instantiate(d, &args, ts.env);
                    out.extend(self.def_call_sig_returns(d, ts.at(env)));
                }
                self.visiting_types.pop();
                out
            }
            _ => Vec::new(),
        };
        self.leave();
        out
    }

    fn def_call_sig_returns(&mut self, d: NodeId, ts: Ts) -> Vec<(NodeId, Env)> {
        match *self.ast().node(d) {
            Node::Interface { .. } => self.call_sig_returns(Some(d), ts),
            Node::TypeAlias { ty, .. } => self.call_sig_returns(Some(ty), ts),
            _ => Vec::new(),
        }
    }

    fn members_state(&mut self, members: &[NodeId], ts: Ts, promised: bool) -> St {
        let sigs = self.call_sigs(members);
        if !sigs.is_empty() {
            if !ts.fn_bool {
                return St::Non;
            }
            let how = if promised { Ret::Promised } else { Ret::Plain };
            let states: Vec<St> = sigs
                .into_iter()
                .map(|r| self.ret_state(how, r, ts))
                .collect();
            return combine(&states);
        }
        if members.is_empty() { St::Unk } else { St::Non }
    }

    /// The plugin's `getTypeAnnotationBooleanState`.
    fn type_state(&mut self, ty: Option<NodeId>, ts: Ts) -> St {
        let Some(ty) = ty else {
            return St::Unk;
        };
        if self.missing_type_args(Some(ty)) || !self.enter() {
            return St::Unk;
        }
        let r = self.type_state_inner(ty, ts);
        self.leave();
        r
    }

    fn type_state_inner(&mut self, ty: NodeId, ts: Ts) -> St {
        let ast = self.ast();
        if let Node::ParenType { ty } = *ast.node(ty) {
            return self.type_state(Some(ty), ts);
        }
        if let Some((t, env)) = self.resolve_param(ty, ts.env) {
            return self.type_state(Some(t), ts.at(env));
        }
        match *ast.node(ty) {
            Node::FunctionType { return_type, .. } => {
                if ts.fn_bool {
                    self.type_state(Some(return_type), ts.fb(false))
                } else {
                    St::Non
                }
            }
            Node::UnionType { types } => {
                let types = ast.list(types).to_vec();
                let states: Vec<St> = types
                    .iter()
                    .map(|&t| self.type_state(Some(t), ts))
                    .collect();
                let rest: Vec<St> = states
                    .iter()
                    .zip(&types)
                    .filter(|&(_, &t)| !is_nullish_type(ast, t))
                    .map(|(&s, _)| s)
                    .collect();
                if !ts.nullish && combine(&rest) == St::Non {
                    return St::Non;
                }
                combine(if ts.nullish { &rest } else { &states })
            }
            Node::IntersectionType { types } => {
                let callables: Vec<NodeId> = ast
                    .list(types)
                    .to_vec()
                    .into_iter()
                    .filter(|&t| self.callable(Some(t), ts))
                    .collect();
                if callables.is_empty() || !ts.fn_bool {
                    return St::Non;
                }
                let states: Vec<St> = callables
                    .into_iter()
                    .map(|t| self.type_state(Some(t), ts))
                    .collect();
                combine(&states)
            }
            Node::TypeRef { .. } => self.type_ref_state(ty, ts),
            Node::TypeLiteral { members } => self.members_state(ast.list(members), ts, false),
            Node::KeywordType {
                kind:
                    KeywordTypeKind::Null
                    | KeywordTypeKind::Undefined
                    | KeywordTypeKind::Any
                    | KeywordTypeKind::Never
                    | KeywordTypeKind::Unknown,
            }
            | Node::NullLit {}
            | Node::ConditionalType { .. } => St::Unk,
            Node::KeywordType {
                kind: KeywordTypeKind::Boolean,
            }
            | Node::BoolLit { .. } => St::Bool,
            Node::TypePredicate { asserts, .. } => {
                if asserts {
                    St::Non
                } else {
                    St::Bool
                }
            }
            _ => St::Non,
        }
    }

    fn type_ref_state(&mut self, ty: NodeId, ts: Ts) -> St {
        if self.is_global_promise_ref(ty) {
            return St::Non;
        }
        if self.visiting_types.contains(&ty) {
            return St::Unk;
        }
        self.visiting_types.push(ty);
        let ast = self.ast();
        let args = self.type_args(ty);
        let ifaces = self.interface_defs(ty);
        let r = if !ifaces.is_empty() {
            let mut states = Vec::new();
            for &d in &ifaces {
                let env = self.instantiate(d, &args, ts.env);
                let s = self.interface_sig_states(d, ts.at(env), Ret::Plain, &mut Vec::new());
                states.extend(s);
            }
            if !states.is_empty() {
                combine(&states)
            } else {
                let members: Vec<NodeId> = ifaces
                    .iter()
                    .flat_map(|&d| self.interface_parts(d).0)
                    .collect();
                self.members_state(&members, ts, false)
            }
        } else {
            match self.type_defs(ty).first().map(|&d| (d, ast.node(d))) {
                Some((d, &Node::TypeAlias { ty: body, .. })) => {
                    let env = self.instantiate(d, &args, ts.env);
                    self.direct_type_state(Some(body), ts.at(env))
                }
                _ => St::Unk,
            }
        };
        self.visiting_types.pop();
        r
    }

    /// The plugin's `getPromisedTypeAnnotationBooleanState`.
    fn promised_state(&mut self, ty: Option<NodeId>, ts: Ts) -> St {
        let Some(ty) = ty else {
            return St::Unk;
        };
        if self.missing_type_args(Some(ty)) || !self.enter() {
            return St::Unk;
        }
        let r = self.promised_state_inner(ty, ts);
        self.leave();
        r
    }

    fn promised_state_inner(&mut self, ty: NodeId, ts: Ts) -> St {
        let ast = self.ast();
        match *ast.node(ty) {
            Node::ParenType { ty } => self.promised_state(Some(ty), ts),
            Node::FunctionType { return_type, .. } => {
                if !ts.fn_bool {
                    St::Non
                } else if self.callable(Some(return_type), ts) {
                    St::Unk
                } else {
                    self.promised_state(Some(return_type), ts.fb(false))
                }
            }
            Node::UnionType { types } => {
                let states: Vec<St> = ast
                    .list(types)
                    .to_vec()
                    .into_iter()
                    .filter(|&t| !is_nullish_type(ast, t) || !ts.nullish)
                    .map(|t| self.promised_state(Some(t), ts))
                    .collect();
                combine(&states)
            }
            Node::IntersectionType { types } => {
                let types = ast.list(types).to_vec();
                let callables: Vec<NodeId> = types
                    .iter()
                    .copied()
                    .filter(|&t| self.callable(Some(t), ts))
                    .collect();
                if !callables.is_empty() {
                    if !ts.fn_bool {
                        return St::Non;
                    }
                    let s: Vec<St> = callables
                        .into_iter()
                        .map(|t| self.promised_state(Some(t), ts))
                        .collect();
                    return combine(&s);
                }
                let promised: Vec<NodeId> = types
                    .into_iter()
                    .filter(|&t| self.is_promised(Some(t), ts))
                    .collect();
                if promised.is_empty() {
                    return St::Unk;
                }
                let s: Vec<St> = promised
                    .into_iter()
                    .map(|t| self.promised_state(Some(t), ts))
                    .collect();
                combine(&s)
            }
            Node::TypeLiteral { members } => self.members_state(ast.list(members), ts, true),
            Node::TypeRef { .. } => self.promised_ref_state(ty, ts),
            _ => self.type_state(Some(ty), ts.fb(false)),
        }
    }

    fn promised_ref_state(&mut self, ty: NodeId, ts: Ts) -> St {
        if let Some((t, env)) = self.resolve_param(ty, ts.env) {
            return self.promised_state(Some(t), ts.at(env));
        }
        let args = self.type_args(ty);
        if self.is_global_promise_ref(ty) {
            return self.type_state(args.first().copied(), ts.fb(false));
        }
        if self.type_ref_name(ty).is_none() || self.visiting_types.contains(&ty) {
            return St::Unk;
        }
        self.visiting_types.push(ty);
        let ast = self.ast();
        let ifaces = self.interface_defs(ty);
        let r = if !ifaces.is_empty() {
            let mut states = Vec::new();
            for &d in &ifaces {
                let env = self.instantiate(d, &args, ts.env);
                let s = self.interface_sig_states(d, ts.at(env), Ret::Promised, &mut Vec::new());
                states.extend(s);
            }
            combine(&states)
        } else {
            match self.type_defs(ty).first().map(|&d| (d, ast.node(d))) {
                Some((d, &Node::TypeAlias { ty: body, .. })) => {
                    let env = self.instantiate(d, &args, ts.env);
                    self.promised_state(Some(body), ts.at(env))
                }
                _ => St::Unk,
            }
        };
        self.visiting_types.pop();
        r
    }

    fn is_promised(&mut self, ty: Option<NodeId>, ts: Ts) -> bool {
        let Some(ty) = ty else {
            return false;
        };
        if self.missing_type_args(Some(ty)) || !self.enter() {
            return false;
        }
        let r = self.is_promised_inner(ty, ts);
        self.leave();
        r
    }

    fn is_promised_inner(&mut self, ty: NodeId, ts: Ts) -> bool {
        if let Some((t, env)) = self.resolve_param(ty, ts.env) {
            return self.is_promised(Some(t), ts.at(env));
        }
        let ast = self.ast();
        match *ast.node(ty) {
            Node::ParenType { ty } => self.is_promised(Some(ty), ts),
            Node::FunctionType { return_type, .. } => {
                !self.callable(Some(return_type), ts) && self.is_promised(Some(return_type), ts)
            }
            Node::IntersectionType { types } => {
                let types = ast.list(types).to_vec();
                let callables: Vec<NodeId> = types
                    .iter()
                    .copied()
                    .filter(|&t| self.callable(Some(t), ts))
                    .collect();
                if !callables.is_empty() {
                    callables.into_iter().all(|t| self.is_promised(Some(t), ts))
                } else {
                    types.into_iter().any(|t| self.is_promised(Some(t), ts))
                }
            }
            Node::TypeLiteral { members } => {
                let sigs = self.call_sigs(ast.list(members));
                !sigs.is_empty() && sigs.into_iter().all(|r| self.is_promised(r, ts))
            }
            Node::UnionType { types } => {
                let rest: Vec<NodeId> = ast
                    .list(types)
                    .iter()
                    .copied()
                    .filter(|&t| !is_nullish_type(ast, t))
                    .collect();
                !rest.is_empty() && rest.into_iter().all(|t| self.is_promised(Some(t), ts))
            }
            Node::TypeRef { .. } => {
                if self.is_global_promise_ref(ty) {
                    return true;
                }
                let defs = self.type_defs(ty);
                if defs.is_empty() || self.visiting_types.contains(&ty) {
                    return false;
                }
                let args = self.type_args(ty);
                self.visiting_types.push(ty);
                let mut states = Vec::new();
                for d in defs {
                    let env = self.instantiate(d, &args, ts.env);
                    let s = match *ast.node(d) {
                        Node::TypeAlias { ty: body, .. } => {
                            Some(self.is_promised(Some(body), ts.at(env)))
                        }
                        Node::Interface { .. } => self.promised_interface(d, ts.at(env)),
                        _ => Some(false),
                    };
                    states.push(s);
                }
                self.visiting_types.pop();
                states.contains(&Some(true)) && !states.contains(&Some(false))
            }
            _ => false,
        }
    }

    fn promised_interface(&mut self, iface: NodeId, ts: Ts) -> Option<bool> {
        if self.visiting_types.contains(&iface) {
            return None;
        }
        self.visiting_types.push(iface);
        let ast = self.ast();
        let (body, extends) = self.interface_parts(iface);
        let mut has_sig = false;
        let mut result = None;
        'outer: {
            for r in self.call_sigs(&body) {
                has_sig = true;
                if !self.is_promised(r, ts) {
                    result = Some(false);
                    break 'outer;
                }
            }
            for h in extends {
                let args = self.type_args(h);
                for d in self.heritage_defs(h) {
                    let env = self.instantiate(d, &args, ts.env);
                    match *ast.node(d) {
                        Node::Interface { .. } => match self.promised_interface(d, ts.at(env)) {
                            Some(false) => {
                                result = Some(false);
                                break 'outer;
                            }
                            Some(true) => has_sig = true,
                            None => {}
                        },
                        Node::TypeAlias { ty: b, .. } => {
                            let rets = self.call_sig_returns(Some(b), ts.at(env));
                            if !rets.is_empty() {
                                has_sig = true;
                                if rets
                                    .into_iter()
                                    .any(|(r, e)| !self.is_promised(Some(r), ts.at(e)))
                                {
                                    result = Some(false);
                                    break 'outer;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            if has_sig {
                result = Some(true);
            }
        }
        self.visiting_types.pop();
        result
    }

    /// The plugin's `getFunctionTypeAnnotationBooleanState`.
    fn fn_type_ann_state(&mut self, ty: Option<NodeId>, ts: Ts) -> St {
        if !ts.fn_bool || !self.callable(ty, ts) {
            return St::Unk;
        }
        self.promised_state(ty, ts.nl(false))
    }

    /// The plugin's `getDirectTypeAnnotationBooleanState`.
    fn direct_type_state(&mut self, ty: Option<NodeId>, ts: Ts) -> St {
        if self.missing_type_args(ty) {
            return St::Unk;
        }
        if self.is_global_promise_ann(ty) {
            return St::Non;
        }
        let s = self.fn_type_ann_state(ty, ts);
        if s != St::Unk {
            return s;
        }
        let callable = self.callable(ty, ts);
        if callable && self.promised_state(ty, ts.nl(true)) == St::Bool {
            return St::Unk;
        }
        if callable && self.is_promised(ty, ts) {
            return St::Unk;
        }
        self.type_state(ty, ts)
    }

    fn promised_return_state(&mut self, ty: Option<NodeId>) -> St {
        if self.has_unresolved_param_ref(ty, EMPTY, false) {
            return St::Unk;
        }
        if self.callable(ty, TS) {
            return St::Non;
        }
        let s = self.promised_state(ty, TS.fb(false).nl(false));
        if s != St::Unk {
            return s;
        }
        if self.promised_state(ty, TS.fb(false)) == St::Non {
            St::Non
        } else {
            St::Unk
        }
    }

    fn is_boolean_fn_like_type_ann(&mut self, ty: Option<NodeId>) -> bool {
        self.is_boolean_fn_type_ann(ty)
            || self.fn_type_ann_state(ty, TS) == St::Bool
            || (self.callable(ty, TS) && self.direct_type_state(ty, TS.nl(false)) == St::Bool)
    }

    fn is_boolean_type_annotated(&mut self, ty: NodeId) -> bool {
        if self.missing_type_args(Some(ty)) {
            return false;
        }
        self.is_boolean_type_ann(Some(ty))
            || self.is_boolean_fn_like_type_ann(Some(ty))
            || (!self.is_function_type_ann(Some(ty))
                && self.direct_type_state(Some(ty), TS.nl(false)) == St::Bool)
    }

    // ---- functions ----

    fn return_expression(&self, func: NodeId) -> Option<NodeId> {
        let ast = self.ast();
        let body = match *ast.node(func) {
            Node::Arrow { flags, body, .. } => {
                if flags.contains(FnFlags::EXPRESSION_BODY) {
                    return Some(body);
                }
                body
            }
            Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body?,
            _ => return None,
        };
        let Node::Block { stmts } = *ast.node(body) else {
            return None;
        };
        match ast.list(stmts) {
            [s] => match *ast.node(*s) {
                Node::Return { arg } => arg,
                _ => None,
            },
            _ => None,
        }
    }

    fn fn_body(&self, func: NodeId) -> Option<NodeId> {
        match *self.ast().node(func) {
            Node::Arrow { body, .. } => Some(body),
            Node::FunctionDecl { body, .. } | Node::FunctionExpr { body, .. } => body,
            _ => None,
        }
    }

    fn fn_body_state(&mut self, func: NodeId) -> St {
        let Some(body) = self.fn_body(func) else {
            return St::Unk;
        };
        if let Some(e) = self.return_expression(func) {
            return self.expr_state(Some(e), false);
        }
        let ast = self.ast();
        match *ast.node(body) {
            Node::Block { stmts } => match ast.list(stmts) {
                [] => St::Non,
                [s] if matches!(ast.node(*s), Node::Return { .. }) => St::Non,
                _ => St::Unk,
            },
            _ => St::Unk,
        }
    }

    /// The plugin's `getFunctionBooleanState`.
    fn fn_state(&mut self, func: NodeId) -> St {
        let ast = self.ast();
        let flags = fn_flags(ast, func).unwrap_or_default();
        if flags.contains(FnFlags::GENERATOR) {
            return St::Non;
        }
        let is_async = flags.contains(FnFlags::ASYNC);
        if let Some(a) = self.doc_fn_return(func) {
            return if is_async {
                St::Unk
            } else {
                jd_state(&a, true, false)
            };
        }
        let rt = fn_return_type(ast, func);
        let s = self.promised_return_state(rt);
        if s != St::Unk {
            return s;
        }
        if self.has_unresolved_param_ref(rt, EMPTY, false) {
            return St::Unk;
        }
        let direct = if is_async {
            St::Unk
        } else {
            self.type_state(rt, TS.fb(false))
        };
        if direct == St::Bool {
            return St::Bool;
        }
        if self.promised_state(rt, TS.fb(false)) == St::Bool {
            return St::Unk;
        }
        if direct != St::Unk {
            return direct;
        }
        self.fn_body_state(func)
    }

    fn is_boolean_function(&mut self, func: NodeId) -> bool {
        let ast = self.ast();
        let flags = fn_flags(ast, func).unwrap_or_default();
        if flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR) {
            return false;
        }
        if let Some(a) = self.doc_return(func) {
            return jd_state(&a, false, false) == St::Bool;
        }
        if self.is_boolean_type_ann(fn_return_type(ast, func)) {
            return true;
        }
        if self.fn_body(func).is_none() {
            return false;
        }
        let e = self.return_expression(func);
        self.is_boolean_expr(e)
    }

    fn has_boolean_return_type(&mut self, func: NodeId) -> bool {
        let rt = fn_return_type(self.ast(), func);
        self.promised_return_state(rt) == St::Bool
    }

    /// The plugin's `isBooleanFunctionValue`.
    fn is_boolean_fn_value(&mut self, func: NodeId) -> bool {
        let ast = self.ast();
        let flags = fn_flags(ast, func).unwrap_or_default();
        if flags.contains(FnFlags::GENERATOR) {
            return false;
        }
        if let Some(a) = self.doc_fn_return(func) {
            return !flags.contains(FnFlags::ASYNC) && jd_state(&a, false, false) == St::Bool;
        }
        if self.has_boolean_return_type(func) {
            return true;
        }
        if flags.contains(FnFlags::ASYNC) {
            return false;
        }
        let rt = fn_return_type(ast, func);
        if rt.is_some() && self.type_state(rt, TS.fb(false)) != St::Bool {
            return false;
        }
        self.direct_type_state(rt, TS.fb(false).nl(false)) == St::Bool
            || self.is_boolean_function(func)
    }

    fn function_value(&self, mut id: NodeId) -> Option<NodeId> {
        let ast = self.ast();
        for _ in 0..64 {
            match *ast.node(id) {
                Node::NonNull { expr } | Node::Satisfies { expr, .. } | Node::Paren { expr } => {
                    id = expr
                }
                _ => break,
            }
        }
        fn_flags(ast, id).map(|_| id)
    }

    fn unwrap_ts(&self, mut id: NodeId) -> NodeId {
        let ast = self.ast();
        for _ in 0..64 {
            match *ast.node(id) {
                Node::As { expr, .. }
                | Node::Satisfies { expr, .. }
                | Node::NonNull { expr }
                | Node::TypeAssertion { expr, .. }
                | Node::Paren { expr } => id = expr,
                _ => break,
            }
        }
        id
    }

    fn ident_sym(&self, id: NodeId) -> Option<SymbolId> {
        let ast = self.ast();
        let id = skip_parens(ast, id);
        if !matches!(ast.node(id), Node::Ident { .. }) {
            return None;
        }
        self.sym_of_ref(id)
    }

    fn stable_function_name(&self, sym: SymbolId) -> bool {
        let sem = self.cx.semantic();
        let s = sem.symbol(sym);
        let scope = sem.scope(s.scope).kind;
        scope != ScopeKind::Global
            && s.decls
                .iter()
                .all(|d| matches!(d.kind, DeclKind::Function | DeclKind::FunctionExprName))
    }

    /// The plugin's `isBooleanFunctionReference`.
    fn is_boolean_fn_ref(&mut self, id: Option<NodeId>) -> bool {
        let Some(sym) = id.and_then(|i| self.ident_sym(i)) else {
            return false;
        };
        if self.visiting_syms.contains(&sym) || !self.enter() {
            return false;
        }
        self.visiting_syms.push(sym);
        let r = self.is_boolean_fn_ref_inner(sym);
        self.visiting_syms.pop();
        self.leave();
        r
    }

    fn is_boolean_fn_ref_inner(&mut self, sym: SymbolId) -> bool {
        let ast = self.ast();
        let decls = self.cx.semantic().symbol(sym).decls.clone();
        if decls.len() > 1 && self.stable_function_name(sym) {
            if self.has_any_write(sym) {
                return false;
            }
            let overloads: Vec<NodeId> = decls
                .iter()
                .filter(|d| matches!(*ast.node(d.node), Node::FunctionDecl { body: None, .. }))
                .map(|d| d.node)
                .collect();
            let fns = if overloads.is_empty() {
                decls.iter().map(|d| d.node).collect()
            } else {
                overloads
            };
            return fns.into_iter().all(|f| self.is_boolean_function(f));
        }
        if decls.len() != 1 {
            return false;
        }
        let d = decls[0];
        if let Some(a) = self.doc_type(d.name) {
            let init = match *ast.node(d.node) {
                Node::VarDeclarator { init, .. } => init,
                Node::Param { init, .. } => init,
                _ => None,
            };
            let value = init.map(|i| self.unwrap_ts(i));
            let vflags = value.and_then(|v| fn_flags(ast, v)).unwrap_or_default();
            let vrt = value.and_then(|v| fn_return_type(ast, v));
            let is_param = matches!(ast.node(d.node), Node::Param { .. });
            if vflags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR)
                || (is_param && (vrt.is_some() || self.has_native_assertion(init)))
                || self.has_write_after_init(sym)
            {
                return false;
            }
            return match vrt {
                Some(rt) => self.is_boolean_type_ann(Some(rt)),
                None => {
                    !a.optional
                        && jd_callable_return(&a.ty).is_some_and(|r| {
                            let r = Ann {
                                ty: r,
                                optional: false,
                            };
                            jd_state(&r, false, false) == St::Bool
                        })
                }
            };
        }
        if let Some(ta) = self.type_ann_of_name(sym) {
            return self.is_boolean_fn_type_ann(Some(ta));
        }
        let func = if self.stable_function_name(sym) {
            (!self.has_any_write(sym)).then_some(d.node)
        } else if let Node::VarDeclarator { name, init, .. } = *ast.node(d.node) {
            if name == d.name && !self.has_write_after_init(sym) {
                init.and_then(|i| self.function_value(i))
            } else {
                None
            }
        } else {
            None
        };
        let Some(func) = func else {
            return false;
        };
        let rt = fn_return_type(ast, func);
        (rt.is_none() || self.is_boolean_type_ann(rt)) && self.is_boolean_function(func)
    }

    /// The plugin's `isBooleanReturningFunctionReference`.
    fn is_boolean_returning_fn_ref(&mut self, id: Option<NodeId>) -> bool {
        let Some(sym) = id.and_then(|i| self.ident_sym(i)) else {
            return false;
        };
        if self.has_write_after_init(sym) {
            return false;
        }
        let ast = self.ast();
        if let Some(fns) = self.function_defs(sym) {
            return !self.has_generator_impl(sym)
                && fns.into_iter().all(|f| self.has_boolean_return_type(f));
        }
        let decls = self.cx.semantic().symbol(sym).decls.clone();
        let [d] = decls[..] else {
            return false;
        };
        if let Some(ta) = self.type_ann_of_name(sym) {
            return self.fn_type_ann_state(Some(ta), TS) == St::Bool;
        }
        match d.kind {
            DeclKind::Function | DeclKind::FunctionExprName => {
                !fn_flags(ast, d.node).is_some_and(|f| f.contains(FnFlags::GENERATOR))
                    && self.has_boolean_return_type(d.node)
            }
            DeclKind::Const => match *ast.node(d.node) {
                Node::VarDeclarator {
                    init: Some(init), ..
                } => {
                    let init = skip_parens(ast, init);
                    fn_flags(ast, init).is_some_and(|f| !f.contains(FnFlags::GENERATOR))
                        && self.has_boolean_return_type(init)
                }
                _ => false,
            },
            _ => false,
        }
    }

    // ---- expressions ----

    /// Which of `names` the identifier `id` is, when it refers to the global.
    fn is_global(&self, id: NodeId, names: &[&'static str]) -> Option<&'static str> {
        let ast = self.ast();
        let id = skip_parens(ast, id);
        let Node::Ident { name } = *ast.node(id) else {
            return None;
        };
        let n = ast.atom(name);
        let hit = *names.iter().find(|&&x| x == n)?;
        is_global_ident(self.cx, id, n).then_some(hit)
    }

    fn property_name(&self, member: NodeId) -> Option<String> {
        let ast = self.ast();
        match *ast.node(member) {
            Node::Member { prop, .. } => match *ast.node(prop) {
                Node::Ident { name } => Some(ast.atom(name).to_owned()),
                _ => None,
            },
            Node::Index { index, .. } => match static_value_no_side_effects(self.cx, index)? {
                Static::Str(s) => Some(s),
                _ => None,
            },
            _ => None,
        }
    }

    fn member_object(&self, member: NodeId) -> Option<(NodeId, bool)> {
        match *self.ast().node(member) {
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                Some((obj, optional))
            }
            _ => None,
        }
    }

    fn annotation_kind(&self, ty: NodeId) -> Option<Kind> {
        let ast = self.ast();
        let ty = unwrap_type(ast, ty);
        match *ast.node(ty) {
            Node::KeywordType {
                kind: KeywordTypeKind::String,
            }
            | Node::StringLit { .. }
            | Node::TemplateLitType { .. } => Some(Kind::String),
            Node::ArrayType { .. } | Node::TupleType { .. } => Some(Kind::Array),
            Node::TypeOperator { ty, .. } => match self.annotation_kind(ty) {
                Some(Kind::Array) => Some(Kind::Array),
                _ => None,
            },
            Node::TypeRef { .. } => {
                if !self.type_defs(ty).is_empty() {
                    return None;
                }
                match self.type_ref_name(ty)? {
                    "Array" | "ReadonlyArray" => Some(Kind::Array),
                    n if TYPED_ARRAYS.contains(&n) => Some(Kind::Array),
                    "Map" | "ReadonlyMap" | "WeakMap" | "URLSearchParams" | "Headers"
                    | "FormData" => Some(Kind::Keyed),
                    "RegExp" => Some(Kind::RegExp),
                    "Set" | "ReadonlySet" => Some(Kind::Set),
                    "WeakSet" => Some(Kind::WeakSet),
                    "String" => Some(Kind::String),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn expression_kind(&mut self, id: NodeId) -> Option<Kind> {
        if !self.enter() {
            return None;
        }
        let r = self.expression_kind_inner(id);
        self.leave();
        r
    }

    fn expression_kind_inner(&mut self, id: NodeId) -> Option<Kind> {
        let ast = self.ast();
        let id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::ArrayLit { .. } => Some(Kind::Array),
            Node::StringLit { .. } | Node::TemplateLit { .. } => Some(Kind::String),
            Node::RegexLit { .. } => Some(Kind::RegExp),
            Node::New { callee, .. } if self.is_global(callee, TYPED_ARRAYS).is_some() => {
                Some(Kind::Array)
            }
            Node::Call {
                callee,
                optional: false,
                ..
            } => {
                let callee = skip_parens(ast, callee);
                if self.is_global(callee, &["String"]).is_some() {
                    return Some(Kind::String);
                }
                if matches!(ast.node(callee), Node::Ident { .. }) {
                    return self.call_return_kind(callee);
                }
                let (obj, optional) = self.member_object(callee)?;
                let method = self.property_name(callee)?;
                if optional {
                    return None;
                }
                if self.is_global(obj, &["String"]).is_some() {
                    return matches!(method.as_str(), "fromCharCode" | "fromCodePoint" | "raw")
                        .then_some(Kind::String);
                }
                let obj_kind = self.expression_kind(obj);
                ((STRING_RETURNING.contains(&method.as_str()) && obj_kind == Some(Kind::String))
                    || (method == "join" && obj_kind == Some(Kind::Array)))
                .then_some(Kind::String)
            }
            Node::New { callee, .. } => {
                match self.is_global(
                    callee,
                    &[
                        "RegExp",
                        "Map",
                        "WeakMap",
                        "URLSearchParams",
                        "Headers",
                        "FormData",
                        "Set",
                        "WeakSet",
                        "Array",
                        "String",
                    ],
                )? {
                    "RegExp" => Some(Kind::RegExp),
                    "Set" => Some(Kind::Set),
                    "WeakSet" => Some(Kind::WeakSet),
                    "Array" => Some(Kind::Array),
                    "String" => None,
                    _ => Some(Kind::Keyed),
                }
            }
            Node::Ident { .. } => {
                let sym = self.sym_of_ref(id)?;
                if let Some(ta) = self.type_ann_of_name(sym) {
                    return self.annotation_kind(ta);
                }
                if let Some(a) = self.doc_type(id) {
                    return jd_kind(&a.ty);
                }
                if self.visiting_syms.contains(&sym) {
                    return None;
                }
                let init = self.simple_const_init(sym)??;
                self.visiting_syms.push(sym);
                let r = self.expression_kind(init);
                self.visiting_syms.pop();
                r
            }
            Node::NonNull { expr } | Node::Satisfies { expr, .. } => self.expression_kind(expr),
            Node::Seq { exprs } => {
                let last = *ast.list(exprs).last()?;
                self.expression_kind(last)
            }
            Node::Assign {
                op: AssignOp::Assign,
                right,
                ..
            } => self.expression_kind(right),
            Node::TaggedTemplate { tag, .. } => {
                let tag = skip_parens(ast, tag);
                let (obj, optional) = self.member_object(tag)?;
                (!optional
                    && self.is_global(obj, &["String"]).is_some()
                    && self.property_name(tag).as_deref() == Some("raw"))
                .then_some(Kind::String)
            }
            Node::As { ty, .. } | Node::TypeAssertion { ty, .. } => self.annotation_kind(ty),
            // Both branches have to agree.
            Node::Conditional { cons, alt, .. } => {
                let a = self.expression_kind(cons)?;
                (self.expression_kind(alt)? == a).then_some(a)
            }
            Node::Binary {
                left,
                op: BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce,
                right,
            } => {
                let a = self.expression_kind(left)?;
                (self.expression_kind(right)? == a).then_some(a)
            }
            _ => None,
        }
    }

    /// The kind a call of the function `callee` returns, from its return annotation.
    fn call_return_kind(&mut self, callee: NodeId) -> Option<Kind> {
        let sym = self.sym_of_ref(callee)?;
        if self.has_write_after_init(sym) {
            return None;
        }
        let ast = self.ast();
        let decls = &self.cx.semantic().symbol(sym).decls;
        let [d] = decls[..] else {
            return None;
        };
        let func = match *ast.node(d.node) {
            Node::FunctionDecl { .. } => d.node,
            Node::VarDeclarator {
                init: Some(init), ..
            } => self.function_value(init)?,
            _ => return None,
        };
        if let Some(rt) = fn_return_type(ast, func) {
            return self.annotation_kind(rt);
        }
        self.doc_return(func).and_then(|a| jd_kind(&a.ty))
    }

    fn is_mutable_keyed(&mut self, id: NodeId) -> bool {
        let ast = self.ast();
        let id = skip_parens(ast, id);
        match *ast.node(id) {
            Node::New { callee, .. } => self
                .is_global(callee, &["Map", "Set", "WeakMap", "WeakSet"])
                .is_some(),
            Node::Ident { .. } => {
                let Some(sym) = self.sym_of_ref(id) else {
                    return false;
                };
                if let Some(a) = self.doc_type(id) {
                    let names = jd_names(&a.ty);
                    return !names.is_empty()
                        && names.iter().all(|n| {
                            matches!(n.as_deref(), Some("Map" | "Set" | "WeakMap" | "WeakSet"))
                        });
                }
                if let Some(ta) = self.type_ann_of_name(sym) {
                    let ta = unwrap_type(ast, ta);
                    return self.type_defs(ta).is_empty()
                        && matches!(
                            self.type_ref_name(ta),
                            Some("Map" | "Set" | "WeakMap" | "WeakSet")
                        );
                }
                if self.visiting_syms.contains(&sym) {
                    return false;
                }
                let Some(Some(init)) = self.simple_const_init(sym) else {
                    return false;
                };
                self.visiting_syms.push(sym);
                let r = self.is_mutable_keyed(init);
                self.visiting_syms.pop();
                r
            }
            _ => false,
        }
    }

    fn is_boolean_call(&mut self, call: NodeId) -> bool {
        let ast = self.ast();
        let Node::Call { callee, .. } = *ast.node(call) else {
            return false;
        };
        let callee = skip_parens(ast, callee);
        if let Some((obj, _)) = self.member_object(callee) {
            let method = self.property_name(callee);
            if let Some(g) = self.is_global(obj, STATIC_NAMES) {
                let methods = static_methods(g);
                if method.as_deref().is_some_and(|m| methods.contains(&m)) {
                    return true;
                }
            }
            let Some(method) = method else {
                return false;
            };
            if method == "delete" {
                return self.is_mutable_keyed(obj);
            }
            return self
                .expression_kind(obj)
                .is_some_and(|k| k.methods().contains(&method.as_str()));
        }
        if !matches!(ast.node(callee), Node::Ident { .. }) {
            return false;
        }
        if self
            .is_global(callee, &["Boolean", "isFinite", "isNaN"])
            .is_some()
        {
            return true;
        }
        self.is_boolean_fn_ref(Some(callee))
    }

    fn is_boolean_static(&self, id: NodeId) -> bool {
        let ast = self.ast();
        match *ast.node(id) {
            Node::BoolLit { .. } => true,
            Node::NullLit {}
            | Node::NumberLit { .. }
            | Node::StringLit { .. }
            | Node::BigIntLit { .. }
            | Node::RegexLit { .. }
            | Node::ArrayLit { .. }
            | Node::Arrow { .. }
            | Node::ClassExpr { .. }
            | Node::FunctionExpr { .. }
            | Node::New { .. }
            | Node::ObjectLit { .. }
            | Node::TaggedTemplate { .. }
            | Node::TemplateLit { .. } => false,
            _ => {
                matches!(
                    static_value_no_side_effects(self.cx, id),
                    Some(Static::Bool(_))
                ) && !self.doc_ref(id)
            }
        }
    }

    fn is_boolean_identifier(&mut self, id: NodeId) -> bool {
        let Some(sym) = self.sym_of_ref(id) else {
            return false;
        };
        let decls = &self.cx.semantic().symbol(sym).decls;
        if decls.len() != 1 || self.visiting_syms.contains(&sym) {
            return false;
        }
        let d = decls[0];
        let ast = self.ast();
        let (ta, optional) = match *ast.node(d.node) {
            Node::VarDeclarator { type_ann, name, .. } if name == d.name => (type_ann, false),
            Node::Param {
                type_ann,
                optional,
                pattern,
                ..
            } if pattern == d.name => (type_ann, optional),
            _ => (None, false),
        };
        if let Some(ta) = ta {
            return !optional && self.is_boolean_type_ann(Some(ta));
        }
        let Some(Some(init)) = self.simple_const_init(sym) else {
            return false;
        };
        self.visiting_syms.push(sym);
        let r = self.is_boolean_expr(Some(init));
        self.visiting_syms.pop();
        r
    }

    /// The plugin's `isBooleanExpression`.
    fn is_boolean_expr(&mut self, id: Option<NodeId>) -> bool {
        let Some(id) = id else {
            return false;
        };
        if !self.enter() {
            return false;
        }
        let ast = self.ast();
        let id = skip_parens(ast, id);
        if matches!(ast.node(id), Node::Ident { .. })
            && let Some(a) = self.doc_type(id)
        {
            self.leave();
            return jd_state(&a, false, false) == St::Bool;
        }
        let r = !is_chain(ast, id) && (self.is_boolean_static(id) || self.is_boolean_by_syntax(id));
        self.leave();
        r
    }

    fn is_boolean_by_syntax(&mut self, id: NodeId) -> bool {
        let ast = self.ast();
        match *ast.node(id) {
            Node::Paren { expr } | Node::Await { arg: expr } | Node::NonNull { expr } => {
                self.is_boolean_expr(Some(expr))
            }
            Node::Ident { .. } => self.is_boolean_identifier(id),
            Node::Unary { op, .. } => matches!(op, UnaryOp::Not | UnaryOp::Delete),
            Node::Binary { left, op, right } => match op {
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce => {
                    self.is_boolean_expr(Some(left)) && self.is_boolean_expr(Some(right))
                }
                BinaryOp::Gt
                | BinaryOp::GtEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Eq
                | BinaryOp::StrictEq
                | BinaryOp::NotEq
                | BinaryOp::StrictNotEq
                | BinaryOp::In
                | BinaryOp::InstanceOf => true,
                _ => false,
            },
            Node::Conditional { cons, alt, .. } => {
                self.is_boolean_expr(Some(cons)) && self.is_boolean_expr(Some(alt))
            }
            Node::As { expr, ty } | Node::TypeAssertion { expr, ty } => {
                self.is_boolean_type_ann(Some(ty))
                    || (matches!(
                        *ast.node(ty),
                        Node::KeywordType {
                            kind: KeywordTypeKind::Any | KeywordTypeKind::Unknown
                        }
                    ) && self.is_boolean_expr(Some(expr)))
            }
            Node::Satisfies { expr, ty } => {
                self.is_boolean_type_ann(Some(ty)) || self.is_boolean_expr(Some(expr))
            }
            Node::Call { .. } => self.is_boolean_call(id),
            Node::Member { .. } | Node::Index { .. } => {
                let Some((obj, optional)) = self.member_object(id) else {
                    return false;
                };
                if optional {
                    return false;
                }
                let prop = self.property_name(id);
                self.expression_kind(obj) == Some(Kind::RegExp)
                    && prop.is_some_and(|p| REGEXP_PROPERTIES.contains(&p.as_str()))
            }
            Node::Seq { exprs } => {
                let last = ast.list(exprs).last().copied();
                self.is_boolean_expr(last)
            }
            Node::Assign {
                op: AssignOp::Assign,
                right,
                ..
            } => self.is_boolean_expr(Some(right)),
            _ => false,
        }
    }

    fn potentially_mutable(&self, id: Option<NodeId>) -> bool {
        let Some(id) = id else {
            return false;
        };
        let ast = self.ast();
        let id = skip_parens(ast, id);
        let single_var_init = |sym: SymbolId| -> Option<NodeId> {
            match self.cx.semantic().symbol(sym).decls[..] {
                [d] if matches!(d.kind, DeclKind::Var | DeclKind::Let | DeclKind::Const) => {
                    self.var_init(d.node)
                }
                _ => None,
            }
        };
        match *ast.node(id) {
            Node::Ident { .. } => {
                let Some(init) = self.sym_of_ref(id).and_then(single_var_init) else {
                    return false;
                };
                let init = skip_parens(ast, init);
                matches!(ast.node(init), Node::Member { .. } | Node::Index { .. })
                    && self.potentially_mutable(Some(init))
            }
            Node::Member { obj, .. } | Node::Index { obj, .. } => {
                let obj = skip_parens(ast, obj);
                if !matches!(ast.node(obj), Node::Ident { .. }) {
                    return false;
                }
                self.sym_of_ref(obj)
                    .and_then(single_var_init)
                    .is_some_and(|i| {
                        matches!(ast.node(skip_parens(ast, i)), Node::ObjectLit { .. })
                    })
            }
            _ => false,
        }
    }

    /// The plugin's `getExpressionBooleanState`.
    fn expr_state(&mut self, id: Option<NodeId>, fvb: bool) -> St {
        let Some(id) = id else {
            return St::Unk;
        };
        if !self.enter() {
            return St::Unk;
        }
        let r = self.expr_state_inner(id, fvb);
        self.leave();
        r
    }

    fn expr_state_inner(&mut self, id: NodeId, fvb: bool) -> St {
        let ast = self.ast();
        let id = skip_parens(ast, id);
        if fn_flags(ast, id).is_some() {
            return if fvb { self.fn_state(id) } else { St::Non };
        }
        if is_chain(ast, id) {
            return St::Unk;
        }
        let doc_ref = self.doc_ref(id);
        if doc_ref
            && matches!(ast.node(id), Node::Ident { .. })
            && let Some(sym) = self.sym_of_ref(id)
            && (self.supported_def(sym).is_some() || self.function_defs(sym).is_some())
        {
            return self.var_state(sym, fvb);
        }
        if let Node::As { ty, .. } | Node::Satisfies { ty, .. } | Node::TypeAssertion { ty, .. } =
            *ast.node(id)
        {
            let s = self.direct_type_state(Some(ty), TS.fb(fvb));
            if s != St::Unk {
                return s;
            }
        }
        if !self.potentially_mutable(Some(id)) && self.is_boolean_expr(Some(id)) {
            return St::Bool;
        }
        if !matches!(ast.node(id), Node::Ident { .. }) && !doc_ref {
            match static_value_no_side_effects(self.cx, id) {
                None | Some(Static::Undefined | Static::Null) => {}
                Some(Static::Bool(_)) => return St::Bool,
                Some(_) => return St::Non,
            }
        }
        match *ast.node(id) {
            Node::ArrayLit { .. }
            | Node::Binary { .. }
            | Node::ObjectLit { .. }
            | Node::ClassExpr { .. }
            | Node::New { .. }
            | Node::TemplateLit { .. }
            | Node::Unary { .. }
            | Node::Update { .. } => St::Non,
            Node::Ident { .. } => match self.sym_of_ref(id) {
                Some(sym)
                    if self.supported_def(sym).is_some() || self.function_defs(sym).is_some() =>
                {
                    self.var_state(sym, fvb)
                }
                _ => St::Unk,
            },
            Node::Await { arg: expr }
            | Node::NonNull { expr }
            | Node::As { expr, .. }
            | Node::TypeAssertion { expr, .. }
            | Node::Satisfies { expr, .. } => self.expr_state(Some(expr), fvb),
            Node::Assign { op, right, .. } => {
                if op == AssignOp::Assign {
                    self.expr_state(Some(right), fvb)
                } else {
                    St::Unk
                }
            }
            Node::Seq { exprs } => {
                let last = ast.list(exprs).last().copied();
                self.expr_state(last, fvb)
            }
            Node::Conditional { cons, alt, .. } => {
                let a = self.expr_state(Some(cons), fvb);
                let b = self.expr_state(Some(alt), fvb);
                combine(&[a, b])
            }
            _ => St::Unk,
        }
    }

    // ---- variables ----

    fn param_default(&self, param: NodeId) -> Option<NodeId> {
        match *self.ast().node(param) {
            Node::Param {
                init, rest: false, ..
            } => init,
            _ => None,
        }
    }

    fn def_state(&mut self, sym: SymbolId, def: Def, fvb: bool) -> St {
        if let Some(a) = self.doc_value_type(self.decl(sym).name) {
            return jd_state(&a, true, fvb);
        }
        let ta = self.type_ann_of_name(sym);
        let s = self.direct_type_state(ta, TS.fb(fvb));
        if s != St::Unk {
            return s;
        }
        let ast = self.ast();
        match def {
            Def::Param { param, func } => {
                if func.is_none() {
                    return St::Unk;
                }
                let d = self.param_default(param);
                self.expr_state(d, fvb)
            }
            Def::Var { declarator, decl } => {
                if self.cx.parent(decl).is_some_and(
                    |p| matches!(*ast.node(p), Node::ForIn { left, .. } if left == decl),
                ) {
                    return St::Non;
                }
                let init = self.var_init(declarator);
                self.expr_state(init, fvb)
            }
            Def::Func { func } => {
                if fvb {
                    self.fn_state(func)
                } else {
                    St::Non
                }
            }
        }
    }

    fn write_states(&mut self, sym: SymbolId, fvb: bool) -> Vec<St> {
        let sem = self.cx.semantic();
        let ast = self.ast();
        let refs = sem.symbol(sym).refs.clone();
        let mut out = Vec::new();
        for r in refs {
            let rf = *sem.reference(r);
            if !rf.is_write() || (rf.flags.contains(RefFlags::INIT) && rf.write_value.is_none()) {
                continue;
            }
            let parent = self.cx.parent(rf.node).map(|p| ast.node(p));
            let s = match parent {
                Some(&Node::ForIn { left, .. }) if left == rf.node => St::Non,
                Some(Node::Update { .. }) => St::Non,
                Some(&Node::Assign { left, op, right }) if left == rf.node => match op {
                    AssignOp::Assign
                    | AssignOp::AndAssign
                    | AssignOp::OrAssign
                    | AssignOp::CoalesceAssign => self.expr_state(Some(right), fvb),
                    _ => St::Non,
                },
                _ => St::Unk,
            };
            out.push(s);
        }
        out
    }

    /// The plugin's `getVariableBooleanState`.
    fn var_state(&mut self, sym: SymbolId, fvb: bool) -> St {
        if self.visiting_syms.contains(&sym) || !self.enter() {
            return St::Unk;
        }
        self.visiting_syms.push(sym);
        let r = self.var_state_inner(sym, fvb);
        self.visiting_syms.pop();
        self.leave();
        r
    }

    fn var_state_inner(&mut self, sym: SymbolId, fvb: bool) -> St {
        let fns = self.function_defs(sym);
        let def = self.supported_def(sym);
        let mut result = match (&fns, def) {
            (None, None) => return St::Unk,
            (Some(_), _) if !fvb => St::Non,
            (Some(fns), _) => {
                if self.has_generator_impl(sym) {
                    St::Non
                } else {
                    let fns = fns.clone();
                    let states: Vec<St> = fns.into_iter().map(|f| self.fn_state(f)).collect();
                    combine(&states)
                }
            }
            (None, Some(def)) => self.def_state(sym, def, fvb),
        };
        let writes = self.write_states(sym, fvb);
        if !writes.is_empty() {
            let mut all = vec![result];
            all.extend(writes);
            result = combine_with_writes(&all);
        }
        if result == St::Unk && fvb && self.is_boolean_variable(sym) {
            result = St::Bool;
        }
        result
    }

    /// The plugin's `isBooleanValue` for a variable's initializer.
    fn is_boolean_value(&mut self, init: Option<NodeId>) -> bool {
        let Some(init) = init else {
            return false;
        };
        if let Some(f) = self.function_value(init) {
            return self.is_boolean_fn_value(f);
        }
        ((self.is_boolean_fn_ref(Some(init)) || self.is_boolean_returning_fn_ref(Some(init)))
            && self.expr_state(Some(init), true) == St::Bool)
            || self.is_boolean_expr(Some(init))
    }

    /// The plugin's `isBooleanVariable`.
    fn is_boolean_variable(&mut self, sym: SymbolId) -> bool {
        if let Some(fns) = self.function_defs(sym) {
            return !self.has_generator_impl(sym)
                && fns.into_iter().all(|f| self.is_boolean_fn_value(f));
        }
        let Some(def) = self.supported_def(sym) else {
            return false;
        };
        if let Some(ta) = self.type_ann_of_name(sym) {
            return self.is_boolean_type_annotated(ta);
        }
        let name = self.decl(sym).name;
        if let Some(a) = self.doc_value_type(name) {
            let a = Ann {
                optional: false,
                ..a
            };
            return jd_state(&a, false, true) == St::Bool;
        }
        match def {
            Def::Param { param, func } => {
                func.is_some() && {
                    let d = self.param_default(param);
                    d.is_some() && self.is_boolean_expr(d)
                }
            }
            Def::Var { declarator, .. } => {
                if let Some(a) = self.doc_type(name) {
                    if jd_state(&a, false, false) == St::Bool {
                        return true;
                    }
                } else if let Some(Some(init)) = self.simple_const_init(sym)
                    && !self.visiting_syms.contains(&sym)
                {
                    self.visiting_syms.push(sym);
                    let b =
                        !self.potentially_mutable(Some(init)) && self.is_boolean_expr(Some(init));
                    self.visiting_syms.pop();
                    if b {
                        return true;
                    }
                }
                let init = self.var_init(declarator);
                if self.potentially_mutable(init) {
                    return false;
                }
                self.is_boolean_value(init)
            }
            Def::Func { func } => {
                fn_flags(self.ast(), func).is_some() && self.is_boolean_fn_value(func)
            }
        }
    }

    fn is_react_hook_binding(&mut self, sym: SymbolId) -> bool {
        let name = self.cx.semantic().name(sym);
        if name == "use" || !is_react_hook_name(name) {
            return false;
        }
        if self.function_defs(sym).is_some() {
            return true;
        }
        match self.supported_def(sym) {
            Some(Def::Func { .. }) => true,
            Some(Def::Var { declarator, .. }) => {
                let init = self.var_init(declarator);
                init.and_then(|i| self.function_value(i)).is_some()
                    || self.is_boolean_fn_ref(init)
                    || self.is_boolean_returning_fn_ref(init)
                    || {
                        let ta = self.type_ann_of_name(sym);
                        self.callable(ta, TS)
                    }
                    || {
                        let name = self.decl(sym).name;
                        self.doc_type(name).is_some_and(|a| jd_is_function(&a))
                    }
            }
            _ => false,
        }
    }

    fn name_for_prefix_check(&mut self, sym: SymbolId) -> String {
        let name = self.cx.semantic().name(sym).to_owned();
        if !self.is_react_hook_binding(sym) {
            return name;
        }
        let hook = &name[3..];
        if is_screaming(hook) {
            hook.to_owned()
        } else {
            lower_first(hook)
        }
    }

    /// `const xRef = useRef(boolean)` (or `React.useRef`), never reassigned.
    fn is_boolean_react_ref(&mut self, sym: SymbolId) -> bool {
        let Some(Def::Var { declarator, .. }) = self.supported_def(sym) else {
            return false;
        };
        let name = strip_underscores(self.cx.semantic().name(sym));
        if self.has_write_after_init(sym) || !(name.ends_with("Ref") || name.ends_with("Reference"))
        {
            return false;
        }
        let ast = self.ast();
        let Some(init) = self.var_init(declarator).map(|i| skip_parens(ast, i)) else {
            return false;
        };
        let Node::Call {
            callee,
            optional: false,
            args,
            ..
        } = *ast.node(init)
        else {
            return false;
        };
        let is_use_ref = match *ast.node(callee) {
            Node::Ident { name } => ast.atom(name) == "useRef",
            Node::Member {
                obj,
                prop,
                optional: false,
            } => {
                matches!(*ast.node(obj), Node::Ident { name } if ast.atom(name) == "React")
                    && matches!(*ast.node(prop), Node::Ident { name } if ast.atom(name) == "useRef")
            }
            _ => false,
        };
        let arg = ast.list(args).first().copied();
        is_use_ref && self.expr_state(arg, false) == St::Bool
    }

    /// `callee` is Vue's own `ref`/`computed`: a global, an import from `vue` under the
    /// same name, or an ambient declaration.
    fn is_vue_function(&self, callee: NodeId) -> bool {
        let ast = self.ast();
        let Some(sym) = self.sym_of_ref(callee) else {
            return true;
        };
        let name = self.cx.text(callee);
        self.cx.semantic().symbol(sym).decls.iter().all(|d| {
            let from_vue = d.kind == DeclKind::ImportNamed
                && matches!(*ast.node(d.node), Node::ImportSpecifier { imported, .. }
                    if imported.is_none_or(|i| self.cx.text(i) == name))
                && matches!(*ast.node(d.parent), Node::ImportDecl { .. })
                && self.import_source(d.parent).as_deref() == Some("vue");
            from_vue || self.in_declare_context(d.node)
        })
    }

    fn import_source(&self, decl: NodeId) -> Option<String> {
        let ast = self.ast();
        let mut found = None;
        ast.for_each_child(decl, |c| {
            if found.is_none()
                && let Node::StringLit { value } = *ast.node(c)
            {
                found = Some(ast.atom(value).to_owned());
            }
        });
        found
    }

    /// `const x = ref(boolean)` or `computed(() => boolean)` from Vue, never reassigned.
    fn is_boolean_vue_ref(&mut self, sym: SymbolId) -> bool {
        let Some(Def::Var { declarator, .. }) = self.supported_def(sym) else {
            return false;
        };
        let ast = self.ast();
        let Some(init) = self.var_init(declarator).map(|i| skip_parens(ast, i)) else {
            return false;
        };
        let Node::Call { callee, args, .. } = *ast.node(init) else {
            return false;
        };
        if !matches!(ast.node(callee), Node::Ident { .. })
            || !self.is_vue_function(callee)
            || self.has_write_after_init(sym)
        {
            return false;
        }
        let arg = ast.list(args).first().copied();
        match self.cx.text(callee) {
            "ref" => self.expr_state(arg, false) == St::Bool,
            "computed" => {
                let Some(a) = arg else {
                    return false;
                };
                let mut g = a;
                for _ in 0..64 {
                    match *ast.node(g) {
                        Node::Paren { expr }
                        | Node::NonNull { expr }
                        | Node::As { expr, .. }
                        | Node::Satisfies { expr, .. }
                        | Node::TypeAssertion { expr, .. } => g = expr,
                        _ => break,
                    }
                }
                let plain_fn = fn_flags(ast, g)
                    .is_some_and(|f| !f.intersects(FnFlags::ASYNC | FnFlags::GENERATOR));
                (plain_fn || self.is_boolean_fn_ref(Some(g)))
                    && (if plain_fn {
                        self.fn_state(g) == St::Bool
                    } else {
                        true
                    })
                    && self.expr_state(Some(a), true) == St::Bool
            }
            _ => false,
        }
    }

    // ---- members ----

    fn param_property_sym(&self, param: NodeId) -> Option<SymbolId> {
        let Node::Param { pattern, .. } = *self.ast().node(param) else {
            return None;
        };
        self.cx.semantic().symbol_of_decl(pattern)
    }

    fn getter_state(&mut self, member: NodeId) -> St {
        let ast = self.ast();
        match *ast.node(member) {
            Node::MethodSignature { return_type, .. } => {
                self.direct_type_state(return_type, TS.fb(false).nl(false))
            }
            Node::MethodDef { value, .. } => {
                if let Some(a) = self.doc_return(value) {
                    return jd_state(&a, true, false);
                }
                let rt = fn_return_type(ast, value);
                let s = self.type_state(rt, TS.fb(false));
                if s != St::Unk {
                    return s;
                }
                self.fn_body_state(value)
            }
            _ => St::Unk,
        }
    }

    fn is_getter(&self, member: NodeId) -> bool {
        matches!(
            *self.ast().node(member),
            Node::MethodDef {
                kind: MethodKind::Get,
                ..
            } | Node::MethodSignature {
                kind: MethodKind::Get,
                ..
            }
        )
    }

    fn method_signature_state(&mut self, rt: Option<NodeId>) -> St {
        let s = self.promised_return_state(rt);
        if s == St::Unk
            && !self.has_unresolved_param_ref(rt, EMPTY, false)
            && self.promised_state(rt, TS.fb(false)) != St::Bool
            && !self.is_promised(rt, TS)
        {
            self.type_state(rt, TS.fb(false).nl(false))
        } else {
            s
        }
    }

    /// The plugin's `getExplicitPropertyBooleanState`.
    fn explicit_member_state(&mut self, member: NodeId) -> St {
        let ast = self.ast();
        if self.is_getter(member) {
            return self.getter_state(member);
        }
        if matches!(
            ast.node(member),
            Node::Property { .. } | Node::ShorthandProp { .. } | Node::PropertyDef { .. }
        ) && let Some(a) = self.doc_value_type(member)
        {
            return jd_state(&a, true, true);
        }
        match *ast.node(member) {
            Node::Param { .. } => match self.param_property_sym(member) {
                Some(sym) => self.var_state(sym, true),
                None => St::Unk,
            },
            Node::ShorthandProp { name, .. } => match self.sym_of_ref(name) {
                Some(sym) => self.var_state(sym, true),
                None => St::Unk,
            },
            Node::Property { value, .. } => self.expr_state(Some(value), true),
            Node::PropertyDef {
                type_ann, value, ..
            } => match type_ann {
                Some(t) => self.direct_type_state(Some(t), TS),
                None => self.expr_state(value, true),
            },
            Node::MethodDef { kind, value, .. } => match kind {
                MethodKind::Constructor | MethodKind::Set => St::Unk,
                _ => self.fn_state(value),
            },
            Node::PropertySignature { type_ann, .. } => {
                self.direct_type_state(type_ann, TS.nl(false))
            }
            Node::MethodSignature { return_type, .. } => self.method_signature_state(return_type),
            _ => St::Unk,
        }
    }

    /// The plugin's `isBooleanProperty`.
    fn is_boolean_member(&mut self, member: NodeId) -> bool {
        let ast = self.ast();
        if self.is_getter(member) {
            return self.getter_state(member) == St::Bool;
        }
        match *ast.node(member) {
            Node::Param { .. } => self
                .param_property_sym(member)
                .is_some_and(|sym| self.is_boolean_variable(sym)),
            Node::ShorthandProp { name, .. } => match self.doc_value_type(member) {
                Some(a) => {
                    jd_state(
                        &Ann {
                            optional: false,
                            ..a
                        },
                        false,
                        true,
                    ) == St::Bool
                }
                None => self.is_boolean_value(Some(name)),
            },
            Node::Property { value, .. } => match self.doc_value_type(member) {
                Some(a) => {
                    jd_state(
                        &Ann {
                            optional: false,
                            ..a
                        },
                        false,
                        true,
                    ) == St::Bool
                }
                None => self.is_boolean_value(Some(value)),
            },
            Node::MethodDef { kind, value, .. } => {
                !matches!(kind, MethodKind::Constructor | MethodKind::Set)
                    && self.is_boolean_fn_value(value)
            }
            Node::PropertyDef {
                type_ann, value, ..
            } => match type_ann {
                Some(t) => self.is_boolean_type_annotated(t),
                None => match self.doc_value_type(member) {
                    Some(a) => {
                        jd_state(
                            &Ann {
                                optional: false,
                                ..a
                            },
                            false,
                            true,
                        ) == St::Bool
                    }
                    None => self.is_boolean_value(value),
                },
            },
            Node::PropertySignature { type_ann, .. } => {
                type_ann.is_some_and(|t| self.is_boolean_type_annotated(t))
            }
            Node::MethodSignature { return_type, .. } => {
                self.direct_type_state(return_type, TS.fb(false).nl(false)) == St::Bool
                    || self.promised_return_state(return_type) == St::Bool
            }
            _ => false,
        }
    }

    fn member_state(&mut self, member: NodeId) -> St {
        let s = self.explicit_member_state(member);
        if s == St::Unk && self.is_boolean_member(member) {
            St::Bool
        } else {
            s
        }
    }

    fn in_declare_context(&self, node: NodeId) -> bool {
        let ast = self.ast();
        let mut cur = Some(node);
        for _ in 0..10_000 {
            let Some(n) = cur else {
                return false;
            };
            let declare = match *ast.node(n) {
                Node::VarDecl { modifiers, .. }
                | Node::FunctionDecl { modifiers, .. }
                | Node::ClassDecl { modifiers, .. }
                | Node::Module { modifiers, .. }
                | Node::Enum { modifiers, .. } => modifiers.contains(Modifiers::DECLARE),
                _ => false,
            };
            if declare {
                return true;
            }
            cur = self.cx.parent(n);
        }
        false
    }

    /// Identifiers of the binding that are exported declarations, `export { x }` or
    /// `export default x`, as (exported, any export).
    fn export_state(&self, sym: SymbolId) -> (bool, bool) {
        let sem = self.cx.semantic();
        let ast = self.ast();
        let s = sem.symbol(sym);
        let mut exported = false;
        let mut any = false;
        for d in &s.decls {
            let m = match *ast.node(d.node) {
                Node::VarDeclarator { .. } => match *ast.node(d.parent) {
                    Node::VarDecl { modifiers, .. } => modifiers,
                    _ => Modifiers::default(),
                },
                Node::FunctionDecl { modifiers, .. } => modifiers,
                _ => Modifiers::default(),
            };
            if m.contains(Modifiers::EXPORT) {
                any = true;
                if !m.contains(Modifiers::DEFAULT) {
                    exported = true;
                }
            }
        }
        for &r in &s.refs {
            let n = sem.reference(r).node;
            match self.cx.parent(n).map(|p| ast.node(p)) {
                Some(&Node::ExportSpecifier {
                    local, exported: e, ..
                }) if local == n => {
                    any = true;
                    if e.is_none() {
                        exported = true;
                    }
                }
                Some(Node::ExportDefaultExpr { .. }) => any = true,
                _ => {}
            }
        }
        (exported, any)
    }

    fn is_autofixable(&mut self, sym: SymbolId) -> bool {
        let sem = self.cx.semantic();
        if sem.scope(sem.symbol(sym).scope).kind == ScopeKind::Global {
            return false;
        }
        let Some(Def::Var { declarator, .. }) = self.supported_def(sym) else {
            return false;
        };
        let init = self.var_init(declarator);
        if self.in_declare_context(declarator)
            || init.and_then(|i| self.function_value(i)).is_some()
            || self.export_state(sym).1
        {
            return false;
        }
        if let Some(a) = self.doc_type(self.decl(sym).name)
            && jd_state(&a, false, true) == St::Bool
            && jd_state(&a, false, false) != St::Bool
        {
            return false;
        }
        let ta = self.type_ann_of_name(sym);
        !self.is_boolean_fn_like_type_ann(ta)
            && !self.is_boolean_fn_ref(init)
            && !self.is_boolean_returning_fn_ref(init)
    }
}

/// An ESTree `ChainExpression`: a call or member chain with an optional link.
fn is_chain(ast: &ts_r_ast::Ast, mut id: NodeId) -> bool {
    for _ in 0..10_000 {
        match *ast.node(id) {
            Node::Call {
                callee, optional, ..
            } => {
                if optional {
                    return true;
                }
                id = callee;
            }
            Node::Member { obj, optional, .. } | Node::Index { obj, optional, .. } => {
                if optional {
                    return true;
                }
                id = obj;
            }
            Node::NonNull { expr } => id = expr,
            _ => return false,
        }
    }
    false
}

const TYPED_ARRAYS: &[&str] = &[
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "BigInt64Array",
    "BigUint64Array",
];

const STRING_RETURNING: &[&str] = &[
    "charAt",
    "concat",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "slice",
    "substring",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "toLowerCase",
    "toString",
    "toUpperCase",
    "toWellFormed",
    "trim",
    "trimEnd",
    "trimStart",
    "valueOf",
];

#[derive(Clone, Copy)]
enum Ret {
    Unknown,
    Plain,
    Promised,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Array,
    Keyed,
    RegExp,
    Set,
    String,
    WeakSet,
}

impl Kind {
    fn methods(self) -> &'static [&'static str] {
        match self {
            Kind::Array => &["every", "includes", "some"],
            Kind::Keyed | Kind::WeakSet => &["has"],
            Kind::RegExp => &["test"],
            Kind::Set => &["has", "isDisjointFrom", "isSubsetOf", "isSupersetOf"],
            Kind::String => &["endsWith", "includes", "isWellFormed", "startsWith"],
        }
    }
}

const STATIC_NAMES: &[&str] = &[
    "Array",
    "ArrayBuffer",
    "Atomics",
    "Error",
    "Number",
    "Object",
    "Reflect",
    "URL",
    "RegExp",
    "Map",
    "WeakMap",
    "URLSearchParams",
    "Headers",
    "FormData",
    "Set",
    "WeakSet",
    "String",
    "Boolean",
    "isFinite",
    "isNaN",
];

fn static_methods(global: &str) -> &'static [&'static str] {
    match global {
        "Array" => &["isArray"],
        "ArrayBuffer" => &["isView"],
        "Atomics" => &["isLockFree"],
        "Error" => &["isError"],
        "Number" => &["isFinite", "isInteger", "isNaN", "isSafeInteger"],
        "Object" => &["hasOwn", "is", "isExtensible", "isFrozen", "isSealed"],
        "Reflect" => &[
            "defineProperty",
            "deleteProperty",
            "has",
            "isExtensible",
            "preventExtensions",
            "set",
            "setPrototypeOf",
        ],
        "URL" => &["canParse"],
        _ => &[],
    }
}

const REGEXP_PROPERTIES: &[&str] = &[
    "dotAll",
    "global",
    "hasIndices",
    "ignoreCase",
    "multiline",
    "sticky",
    "unicode",
    "unicodeSets",
];

struct Problem {
    span: Span,
    message: String,
    id: &'static str,
    fix: Option<Fix>,
    suggestions: Vec<(String, Fix)>,
}

/// Members reported once: (owner, static/private key, name), whether the prefix problem
/// was reported, and the no-prefix problem with each member's state and overload flag.
struct Group {
    key: (String, String, String),
    reported: bool,
    problem: Option<Problem>,
    states: Vec<(St, bool)>,
}

/// A class, object or type member to check.
struct Member {
    node: NodeId,
    key: NodeId,
    mode: Mode,
    /// Methods, getters and signatures are reported once per owner and name.
    dedup: Option<(String, String)>,
}

fn key_name(cx: &Ctx<'_, '_>, key: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::PrivateName { .. } => Some(cx.text(key).trim_start_matches('#').to_owned()),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { .. } => static_value_no_side_effects(cx, key)?.to_js_string(),
        Node::ComputedKey { expr } => match *ast.node(skip_parens(ast, expr)) {
            Node::StringLit { value } => Some(ast.atom(value).to_owned()),
            _ => static_value_no_side_effects(cx, expr)?.to_js_string(),
        },
        _ => None,
    }
}

fn collect_members(this: &ConsistentBooleanName, cx: &Ctx<'_, '_>) -> Vec<Member> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let mut out = Vec::new();
    let member_key = |m: Modifiers, key: NodeId| {
        format!(
            "{}:{}",
            if m.contains(Modifiers::STATIC) {
                "static"
            } else {
                "instance"
            },
            if matches!(ast.node(key), Node::PrivateName { .. }) {
                "private"
            } else {
                "public"
            }
        )
    };
    for id in (1..ast.node_count() as u32).filter_map(NodeId::from_raw) {
        if ast.is_jsdoc(id) {
            continue;
        }
        match *ast.node(id) {
            Node::ObjectLit { props } => {
                if is_assignment_pattern(cx, id) {
                    continue;
                }
                for &p in ast.list(props) {
                    match *ast.node(p) {
                        Node::Property { key, .. } => out.push(Member {
                            node: p,
                            key,
                            mode: this.check_fields,
                            dedup: None,
                        }),
                        Node::ShorthandProp { name, .. } => out.push(Member {
                            node: p,
                            key: name,
                            mode: this.check_fields,
                            dedup: None,
                        }),
                        Node::MethodDef { key, modifiers, .. } => out.push(Member {
                            node: p,
                            key,
                            mode: this.check_methods,
                            dedup: Some((format!("n{}", id.raw()), member_key(modifiers, key))),
                        }),
                        _ => {}
                    }
                }
            }
            Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => {
                for &m in ast.list(members) {
                    match *ast.node(m) {
                        Node::MethodDef { key, modifiers, .. } => out.push(Member {
                            node: m,
                            key,
                            mode: this.check_methods,
                            dedup: Some((format!("n{}", id.raw()), member_key(modifiers, key))),
                        }),
                        Node::PropertyDef { key, .. } => out.push(Member {
                            node: m,
                            key,
                            mode: this.check_fields,
                            dedup: None,
                        }),
                        _ => {}
                    }
                }
            }
            Node::Param { pattern, .. } => {
                let is_prop = sem
                    .symbol_of_decl(pattern)
                    .is_some_and(|s| sem.symbol(s).flags.contains(SymbolFlags::PARAM_PROPERTY));
                if is_prop && matches!(ast.node(pattern), Node::Ident { .. }) {
                    out.push(Member {
                        node: id,
                        key: pattern,
                        mode: this.check_fields,
                        dedup: None,
                    });
                }
            }
            Node::Interface {
                name,
                body,
                modifiers,
                ..
            } => {
                let (owner, qualified) = interface_owner(cx, id, name, modifiers);
                for &m in ast.list(body) {
                    signature_member(this, ast, m, &owner, &qualified, &mut out);
                }
            }
            Node::TypeLiteral { members } => {
                for &m in ast.list(members) {
                    signature_member(this, ast, m, &format!("n{}", id.raw()), "", &mut out);
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|m| cx.span(m.node).start);
    out
}

fn signature_member(
    this: &ConsistentBooleanName,
    ast: &ts_r_ast::Ast,
    m: NodeId,
    owner: &str,
    qualified: &str,
    out: &mut Vec<Member>,
) {
    match *ast.node(m) {
        Node::PropertySignature { key, .. } => out.push(Member {
            node: m,
            key,
            mode: this.check_fields,
            dedup: Some((owner.to_owned(), format!("{qualified}:instance:public"))),
        }),
        Node::MethodSignature { key, .. } => out.push(Member {
            node: m,
            key,
            mode: this.check_methods,
            dedup: Some((owner.to_owned(), format!("{qualified}:instance:public"))),
        }),
        _ => {}
    }
}

/// Which members of merged interfaces are one: interfaces of the same name in the same
/// namespace path (or, outside an ambient context, the same unexported block).
fn interface_owner(
    cx: &Ctx<'_, '_>,
    iface: NodeId,
    name: NodeId,
    modifiers: Modifiers,
) -> (String, String) {
    let ast = cx.ast();
    let iname = cx.text(name).to_owned();
    let mut names: Vec<String> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    let mut ambient = false;
    let mut external = false;
    let mut first_block = None;
    let mut cur = cx.parent(iface);
    for _ in 0..1000 {
        let Some(n) = cur else {
            break;
        };
        if let Node::Module {
            modifiers: m,
            kind,
            name: mname,
            ..
        } = *ast.node(n)
        {
            let literal = matches!(ast.node(mname), Node::StringLit { .. });
            let global = kind == ModuleKind::Global;
            ambient |= m.contains(Modifiers::DECLARE) || global || literal;
            external |= literal || global;
            let prefix = if global {
                "global"
            } else if literal {
                "module"
            } else {
                "namespace"
            };
            let text = match *ast.node(mname) {
                Node::StringLit { value } => ast.atom(value).to_owned(),
                _ => cx.text(mname).to_owned(),
            };
            let ns: Vec<String> = text.split('.').map(|t| t.trim().to_owned()).collect();
            for (i, t) in ns.iter().enumerate() {
                names.insert(i, t.clone());
                parts.insert(i, format!("{prefix}:{t}"));
            }
        } else if first_block.is_none() && matches!(ast.node(n), Node::ModuleBlock { .. }) {
            first_block = Some(n);
        }
        cur = cx.parent(n);
    }
    let exported = modifiers.contains(Modifiers::EXPORT);
    if !names.is_empty() && !ambient && !exported {
        let block = first_block.unwrap_or(iface);
        return (format!("n{}", block.raw()), iname);
    }
    let mut qualified = names.clone();
    qualified.push(iname.clone());
    let qualified = qualified.join(".");
    if external || !names.is_empty() {
        return (format!("p{}", parts.join("/")), qualified);
    }
    let sem = cx.semantic();
    let owner = sem
        .symbol_of_decl(name)
        .map_or(format!("n{}", iface.raw()), |s| format!("s{}", s.0));
    (owner, qualified)
}

/// An object literal that is (part of) an assignment target, an ESTree `ObjectPattern`.
fn is_assignment_pattern(cx: &Ctx<'_, '_>, obj: NodeId) -> bool {
    let ast = cx.ast();
    let mut cur = obj;
    for _ in 0..1000 {
        let Some(p) = cx.parent(cur) else {
            return false;
        };
        match *ast.node(p) {
            Node::Assign { left, .. } => return left == cur,
            Node::ForOf { left, .. } | Node::ForIn { left, .. } => return left == cur,
            Node::Property { value, .. } if value == cur => {}
            Node::ShorthandProp { .. } | Node::Spread { .. } | Node::ArrayLit { .. } => {}
            Node::ObjectLit { .. } | Node::Paren { .. } => {}
            Node::AssignPattern { left, .. } if left == cur => {}
            _ => return false,
        }
        cur = p;
    }
    false
}

impl ConsistentBooleanName {
    fn check_members(&self, an: &mut An<'_, '_, '_>, out: &mut Vec<Problem>) {
        let cx = an.cx;
        let ast = cx.ast();
        let list = self.prefix_list();
        let mut groups: Vec<Group> = Vec::new();
        for m in collect_members(self, cx) {
            if m.mode == Mode::Never {
                continue;
            }
            let Some(name) = key_name(cx, m.key) else {
                continue;
            };
            if self.ignore.iter().any(|r| r.test(&name)) {
                continue;
            }
            if matches!(
                *ast.node(m.node),
                Node::MethodDef {
                    kind: MethodKind::Set,
                    ..
                } | Node::MethodSignature {
                    kind: MethodKind::Set,
                    ..
                }
            ) {
                continue;
            }
            let span = match *ast.node(m.key) {
                _ if matches!(ast.node(m.node), Node::Param { .. }) => estree_ident_span(cx, m.key),
                Node::ComputedKey { expr } => cx.span(expr),
                _ => cx.span(m.key),
            };
            let group = m.dedup.as_ref().map(|(owner, key)| {
                let k = (owner.clone(), key.clone(), name.clone());
                match groups.iter().position(|g| g.key == k) {
                    Some(i) => i,
                    None => {
                        groups.push(Group {
                            key: k,
                            reported: false,
                            problem: None,
                            states: Vec::new(),
                        });
                        groups.len() - 1
                    }
                }
            });
            if let Some(prefix) = boolean_prefix(&name, &self.prefixes) {
                if an.member_state(m.node) == St::Non {
                    if let Some(g) = group {
                        if groups[g].reported {
                            continue;
                        }
                        groups[g].reported = true;
                    }
                    out.push(Problem {
                        span,
                        message: format!(
                            "`{name}` starts with `{prefix}`, so it should be boolean."
                        ),
                        id: "non-boolean-prefix",
                        fix: None,
                        suggestions: Vec::new(),
                    });
                }
                continue;
            }
            if m.mode != Mode::Always {
                continue;
            }
            let problem = || Problem {
                span,
                message: format!("Boolean name `{name}` should start with {list}."),
                id: "consistent-boolean-name",
                fix: None,
                suggestions: Vec::new(),
            };
            if let Some(g) = group {
                let state = an.member_state(m.node);
                let overload = matches!(*ast.node(m.node), Node::MethodDef { value, .. }
                    if matches!(*ast.node(value), Node::FunctionExpr { body: None, .. }));
                groups[g].states.push((state, overload));
                if groups[g].problem.is_none() {
                    groups[g].problem = Some(problem());
                }
                continue;
            }
            if !an.is_boolean_member(m.node) || an.explicit_member_state(m.node) == St::Non {
                continue;
            }
            out.push(problem());
        }
        for Group {
            problem, states, ..
        } in groups
        {
            let has_overload = states.iter().any(|&(_, o)| o);
            let kept: Vec<St> = states
                .iter()
                .filter(|&&(_, o)| !has_overload || o)
                .map(|&(s, _)| s)
                .collect();
            if let Some(p) = problem
                && combine(&kept) == St::Bool
            {
                out.push(p);
            }
        }
    }

    fn prefix_list(&self) -> String {
        self.prefixes
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn mode_of(&self, an: &An<'_, '_, '_>, sym: SymbolId) -> Option<Mode> {
        if an.function_defs(sym).is_some() {
            return Some(self.check_functions);
        }
        Some(match an.supported_def(sym)? {
            Def::Var { .. } => self.check_variables,
            Def::Param { .. } => self.check_arguments,
            Def::Func { .. } => self.check_functions,
        })
    }

    fn check(&self, an: &mut An<'_, '_, '_>, sym: SymbolId) -> Option<Problem> {
        let cx = an.cx;
        let sem = cx.semantic();
        let s = sem.symbol(sym);
        let name = sem.name(sym).to_owned();
        if s.decls.is_empty()
            || s.flags
                .intersects(SymbolFlags::ARGUMENTS | SymbolFlags::COMMENT_GLOBAL)
            || self.ignore.iter().any(|r| r.test(&name))
        {
            return None;
        }
        let mode = self.mode_of(an, sym)?;
        if mode == Mode::Never {
            return None;
        }
        let checked = an.name_for_prefix_check(sym);
        let ident = an.decl(sym).name;
        let span = estree_ident_span(cx, ident);
        if let Some(prefix) = boolean_prefix(&checked, &self.prefixes) {
            if an.var_state(sym, true) == St::Non
                && !an.is_boolean_react_ref(sym)
                && !an.is_boolean_vue_ref(sym)
            {
                return Some(Problem {
                    span,
                    message: format!(
                        "`{checked}` starts with `{prefix}`, so it should be boolean."
                    ),
                    id: "non-boolean-prefix",
                    fix: None,
                    suggestions: Vec::new(),
                });
            }
            return None;
        }
        if mode != Mode::Always
            || !an.is_boolean_variable(sym)
            || an.var_state(sym, true) == St::Non
        {
            return None;
        }
        let list = self.prefix_list();
        let mut problem = Problem {
            span,
            message: format!("Boolean name `{checked}` should start with {list}."),
            id: "consistent-boolean-name",
            fix: None,
            suggestions: Vec::new(),
        };
        let param_property = s.flags.contains(SymbolFlags::PARAM_PROPERTY);
        let jsx = s
            .refs
            .iter()
            .any(|&r| sem.reference(r).flags.intersects(RefFlags::JSX));
        if an.export_state(sym).0 || jsx || param_property {
            return Some(problem);
        }
        let mut scopes: Vec<ScopeId> = s.refs.iter().map(|&r| sem.reference(r).scope).collect();
        scopes.push(s.scope);
        let name_range = match an.supported_def(sym) {
            Some(Def::Param { .. }) => an.param_annotation(ident).and_then(|a| a.name_range),
            _ => None,
        };
        for p in &self.prefixes {
            let wanted = hook_replacement_name(&name, &checked, p);
            let Some(n) = available_name(cx, &wanted, &scopes, &|_| true) else {
                continue;
            };
            let mut fix = rename_symbol_fix(cx, sym, &n);
            if let Some(r) = name_range {
                fix.edits.push(crate::diagnostic::Edit {
                    span: r,
                    text: n.clone(),
                });
                fix.edits.sort_by_key(|e| (e.span.start, e.span.end));
            }
            problem.suggestions.push((n, fix));
        }
        let first = hook_replacement_name(&name, &checked, &self.prefixes[0]);
        if an.is_autofixable(sym)
            && let Some((_, f)) = problem.suggestions.iter().find(|(n, _)| *n == first)
        {
            problem.fix = Some(f.clone());
        }
        Some(problem)
    }
}

impl Rule for ConsistentBooleanName {
    fn kinds(&self) -> &'static [NodeKind] {
        &[]
    }
    fn has_run_file(&self) -> bool {
        true
    }
    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        if self.prefixes.is_empty() {
            return;
        }
        let problems: Vec<Problem> = {
            let mut an = An::new(cx);
            let n = cx.semantic().symbols().len();
            let mut out: Vec<Problem> = (0..n)
                .filter_map(|i| self.check(&mut an, SymbolId(i as u32)))
                .collect();
            if self.check_methods != Mode::Never || self.check_fields != Mode::Never {
                self.check_members(&mut an, &mut out);
            }
            out
        };
        for p in problems {
            let mut r = cx.report(p.span, p.message).id(p.id).fix_opt(p.fix);
            for (n, f) in p.suggestions {
                r = r.suggest("rename", format!("Rename to `{n}`."), f);
            }
        }
    }
}

// ---- JSDoc types, as the plugin reads them ----

/// A JSDoc type reduced to what the rule needs.
#[derive(Clone, Debug, PartialEq)]
enum Jd {
    Unknown,
    NonPrimitive,
    Nullish,
    Name(String),
    Union(Vec<Jd>),
    Inter(Vec<Jd>),
    Nullable(Box<Jd>),
    Optional(Box<Jd>),
    Func(Box<Jd>),
}

#[derive(Clone, Debug)]
struct Ann {
    ty: Jd,
    optional: bool,
}

#[derive(Clone, Debug)]
struct Tag {
    name: String,
    ty: Option<Jd>,
    param: Option<String>,
    optional: bool,
    name_range: Option<Span>,
}

fn is_id_start(c: char) -> bool {
    c == '$' || c == '_' || c.is_alphabetic()
}

fn is_id_continue(c: char) -> bool {
    c == '$' || c == '_' || c == '\u{200c}' || c == '\u{200d}' || c.is_alphanumeric()
}

fn is_identifier(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(is_id_start) && c.all(is_id_continue)
}

fn primitive_jd(name: &str) -> Option<Jd> {
    let n = match name {
        "Boolean" => "boolean",
        "String" => "string",
        "Number" => "number",
        "Void" => "void",
        "Null" => "null",
        "Undefined" => "undefined",
        n => n,
    };
    Some(match n {
        "any" | "unknown" | "never" => Jd::Unknown,
        "null" | "undefined" => Jd::Nullish,
        "true" | "false" | "boolean" => Jd::Name("boolean".into()),
        "string" | "number" | "bigint" | "symbol" | "void" => Jd::Name(n.into()),
        _ => return None,
    })
}

const UNSUPPORTED_NAMES: &[&str] = &[
    "import", "typeof", "extends", "new", "keyof", "infer", "asserts", "is", "readonly",
];
const RESERVED_NAMES: &[&str] = &["import", "typeof", "extends", "new"];

fn jd_tokens(text: &str) -> Option<(Vec<String>, Vec<usize>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut breaks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if chars[start..i]
                .iter()
                .any(|&c| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
            {
                breaks.push(tokens.len());
            }
            continue;
        }
        let start = i;
        if c == '=' && chars.get(i + 1) == Some(&'>') {
            i += 2;
        } else if c == '.' && chars.get(i + 1) == Some(&'.') && chars.get(i + 2) == Some(&'.') {
            i += 3;
        } else if is_id_start(c) {
            i += 1;
            while i < chars.len() && is_id_continue(chars[i]) {
                i += 1;
            }
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '_'))
            {
                i += 1;
            }
        } else if c == '"' || c == '\'' {
            i += 1;
            let mut closed = false;
            while i < chars.len() && !matches!(chars[i], '\n' | '\r') {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == c {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return None;
            }
        } else {
            i += 1;
        }
        tokens.push(chars[start..i.min(chars.len())].iter().collect());
    }
    Some((tokens, breaks))
}

fn is_numeric_token(t: &str) -> bool {
    let b = t.as_bytes();
    let starts = b.first().is_some_and(u8::is_ascii_digit)
        || (b.first() == Some(&b'.') && b.get(1).is_some_and(u8::is_ascii_digit));
    let lower = t.to_ascii_lowercase();
    starts
        && (!t.ends_with('n')
            || lower.starts_with("0b")
            || lower.starts_with("0o")
            || lower.starts_with("0x")
            || !(lower.contains('.') || lower.contains('e')))
}

/// A JSDoc type that is a function (nullish members aside), as the hook check reads it.
fn jd_is_function(a: &Ann) -> bool {
    fn class(t: &Jd) -> Class {
        let union = |cs: Vec<Class>| {
            let cs: Vec<Class> = cs.into_iter().filter(|&c| c != Class::Nullish).collect();
            if cs.is_empty() || cs.iter().all(|&c| c == Class::NonTarget) {
                Class::NonTarget
            } else if cs.iter().all(|&c| c == Class::Target) {
                Class::Target
            } else {
                Class::Unknown
            }
        };
        match t {
            Jd::Func(_) => Class::Target,
            Jd::Nullable(e) | Jd::Optional(e) => union(vec![class(e), Class::Nullish]),
            Jd::Union(_) => union(jd_members(t).into_iter().map(class).collect()),
            Jd::Inter(items) => {
                let cs: Vec<Class> = items
                    .iter()
                    .map(|i| match class(i) {
                        Class::Nullish => Class::NonTarget,
                        c => c,
                    })
                    .collect();
                if cs.contains(&Class::Unknown) {
                    Class::Unknown
                } else if cs.contains(&Class::Target) {
                    Class::Target
                } else {
                    Class::NonTarget
                }
            }
            Jd::Nullish => Class::Nullish,
            Jd::Name(n) => {
                if matches!(
                    n.as_str(),
                    "boolean" | "string" | "number" | "bigint" | "symbol" | "void"
                ) {
                    Class::NonTarget
                } else {
                    Class::Unknown
                }
            }
            Jd::NonPrimitive => Class::NonTarget,
            Jd::Unknown => Class::Unknown,
        }
    }
    class(&a.ty) == Class::Target
}

struct JdParser {
    tokens: Vec<String>,
    breaks: Vec<usize>,
    pos: usize,
    depth: u32,
}

type JdResult = Result<Jd, ()>;

impl JdParser {
    fn peek(&self, k: usize) -> Option<&str> {
        self.tokens.get(self.pos + k).map(String::as_str)
    }
    fn eat(&mut self, t: &str) -> bool {
        if self.peek(0) == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, t: &str) -> Result<(), ()> {
        if self.eat(t) { Ok(()) } else { Err(()) }
    }
    fn guard(&mut self) -> Result<(), ()> {
        self.depth += 1;
        if self.depth > 64 { Err(()) } else { Ok(()) }
    }

    fn optional(&mut self) -> JdResult {
        let t = self.union()?;
        Ok(if self.eat("=") {
            Jd::Optional(Box::new(t))
        } else {
            t
        })
    }

    fn union(&mut self) -> JdResult {
        self.guard()?;
        self.eat("|");
        let mut items = vec![self.intersection()?];
        while self.eat("|") {
            items.push(self.intersection()?);
        }
        self.depth -= 1;
        Ok(if items.len() == 1 {
            items.pop().unwrap_or(Jd::Unknown)
        } else {
            Jd::Union(items)
        })
    }

    fn intersection(&mut self) -> JdResult {
        self.eat("&");
        let mut items = vec![self.postfix()?];
        while self.eat("&") {
            items.push(self.postfix()?);
        }
        Ok(if items.len() == 1 {
            items.pop().unwrap_or(Jd::Unknown)
        } else {
            Jd::Inter(items)
        })
    }

    fn postfix(&mut self) -> JdResult {
        self.guard()?;
        let mut t = if self.eat("?") {
            if matches!(self.peek(0), Some("?" | "!")) {
                return Err(());
            }
            match self.peek(0) {
                None | Some(")" | ">" | "]" | "}" | "," | ";" | "|" | "&") => Jd::Unknown,
                _ => Jd::Nullable(Box::new(self.postfix()?)),
            }
        } else if self.eat("!") {
            if self.peek(0) == Some("!") {
                return Err(());
            }
            self.postfix()?
        } else if self.eat("...") {
            self.postfix()?;
            Jd::NonPrimitive
        } else if self.eat("readonly") {
            let t = self.postfix()?;
            if !matches!(&t, Jd::Name(n) if n == "Array" || n == "ReadonlyArray") {
                return Err(());
            }
            t
        } else {
            self.primary()?
        };
        while self.peek(0) == Some("[") && self.peek(1) == Some("]") {
            self.pos += 2;
            t = Jd::Name("Array".into());
        }
        self.depth -= 1;
        if self.eat("?") {
            return Ok(Jd::Nullable(Box::new(t)));
        }
        self.eat("!");
        Ok(t)
    }

    fn list(
        &mut self,
        open: &str,
        close: &str,
        member: fn(&mut Self) -> Result<(), ()>,
    ) -> Result<(), ()> {
        self.expect(open)?;
        if self.eat(close) {
            return Ok(());
        }
        for _ in 0..10_000 {
            member(self)?;
            if self.eat(close) {
                return Ok(());
            }
            self.expect(",")?;
            if self.eat(close) {
                return Ok(());
            }
        }
        Err(())
    }

    fn arrow_param(&mut self) -> Result<(), ()> {
        self.eat("...");
        match self.peek(0) {
            Some(t) if is_identifier(t) && !RESERVED_NAMES.contains(&t) => self.pos += 1,
            _ => return Err(()),
        }
        self.eat("?");
        if self.eat(":") {
            self.union()?;
        }
        Ok(())
    }

    fn function_param(&mut self) -> Result<(), ()> {
        if self.eat("this") {
            self.expect(":")?;
        }
        self.optional().map(|_| ())
    }

    fn tuple_member(&mut self) -> Result<(), ()> {
        let name = self.pos + usize::from(self.peek(0) == Some("..."));
        let colon = name
            + if self.tokens.get(name + 1).map(String::as_str) == Some("?") {
                2
            } else {
                1
            };
        if self.tokens.get(name).is_some_and(|t| is_identifier(t))
            && self.tokens.get(colon).map(String::as_str) == Some(":")
        {
            self.pos = colon + 1;
        }
        self.optional().map(|_| ())
    }

    fn matching_paren(&self) -> Option<usize> {
        let mut depth = 0i32;
        for (i, t) in self.tokens.iter().enumerate().skip(self.pos) {
            match t.as_str() {
                "(" | "[" | "{" | "<" => depth += 1,
                ")" | "]" | "}" | ">" => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn record(&mut self) -> JdResult {
        self.expect("{")?;
        if self.eat("}") {
            return Ok(Jd::Unknown);
        }
        let mut sigs = Vec::new();
        for _ in 0..10_000 {
            if self.eat("}") {
                return Ok(match sigs.len() {
                    0 => Jd::NonPrimitive,
                    1 => sigs.pop().unwrap_or(Jd::Unknown),
                    _ => Jd::Inter(sigs),
                });
            }
            if self.peek(0) == Some("readonly")
                && !matches!(self.peek(1), Some(":" | "?" | "(" | "," | ";" | "}"))
            {
                self.pos += 1;
            }
            if self.peek(0) == Some("(") {
                self.list("(", ")", Self::arrow_param)?;
                self.expect(":")?;
                sigs.push(Jd::Func(Box::new(self.union()?)));
            } else if self.eat("[") {
                if !self.peek(0).is_some_and(is_identifier) {
                    return Err(());
                }
                self.pos += 1;
                self.expect(":")?;
                self.union()?;
                self.expect("]")?;
                self.expect(":")?;
                self.union()?;
            } else {
                let key = self.peek(0).unwrap_or("");
                let ok = is_identifier(key)
                    || key.starts_with('"')
                    || key.starts_with('\'')
                    || is_numeric_token(key);
                if !ok {
                    return Err(());
                }
                self.pos += 1;
                self.eat("?");
                if self.peek(0) == Some("(") {
                    self.list("(", ")", Self::arrow_param)?;
                }
                if self.eat(":") {
                    self.union()?;
                }
            }
            if !self.eat(",")
                && !self.eat(";")
                && self.peek(0) != Some("}")
                && !self.breaks.contains(&self.pos)
            {
                return Err(());
            }
        }
        Err(())
    }

    fn primary(&mut self) -> JdResult {
        let tok = self.peek(0).unwrap_or("").to_owned();
        match tok.as_str() {
            "(" => {
                if self
                    .matching_paren()
                    .and_then(|i| self.tokens.get(i + 1))
                    .map(String::as_str)
                    == Some("=>")
                {
                    self.list("(", ")", Self::arrow_param)?;
                    self.expect("=>")?;
                    return Ok(Jd::Func(Box::new(self.union()?)));
                }
                self.pos += 1;
                let t = self.optional()?;
                self.expect(")")?;
                Ok(t)
            }
            "[" => {
                self.list("[", "]", Self::tuple_member)?;
                Ok(Jd::Name("Array".into()))
            }
            "{" => self.record(),
            "function" if self.peek(1) == Some("(") => {
                self.pos += 1;
                self.list("(", ")", Self::function_param)?;
                Ok(Jd::Func(Box::new(if self.eat(":") {
                    self.union()?
                } else {
                    Jd::Unknown
                })))
            }
            "*" => {
                self.pos += 1;
                Ok(Jd::Unknown)
            }
            t if t.starts_with('"') || t.starts_with('\'') => {
                self.pos += 1;
                Ok(Jd::Name("string".into()))
            }
            t if t == "-"
                || t.starts_with(|c: char| c.is_ascii_digit())
                || t.starts_with('.') && t.len() > 1 =>
            {
                self.eat("-");
                let lit = self.peek(0).unwrap_or("").to_owned();
                if !is_numeric_token(&lit) {
                    return Err(());
                }
                self.pos += 1;
                Ok(Jd::Name(
                    if lit.ends_with('n') {
                        "bigint"
                    } else {
                        "number"
                    }
                    .into(),
                ))
            }
            _ => self.named(),
        }
    }

    fn named(&mut self) -> JdResult {
        let tok = self.peek(0).unwrap_or("").to_owned();
        if !is_identifier(&tok) || UNSUPPORTED_NAMES.contains(&tok.as_str()) {
            return Err(());
        }
        self.pos += 1;
        let mut name = tok;
        while self.eat(".") {
            if self.peek(0) == Some("<") {
                break;
            }
            match self.peek(0) {
                Some(t) if is_identifier(t) => {
                    name.push('.');
                    name.push_str(t);
                    self.pos += 1;
                }
                _ => return Err(()),
            }
        }
        if self.peek(0) == Some("<") {
            if self.peek(1) == Some(">") || primitive_jd(&name).is_some() {
                return Err(());
            }
            self.list("<", ">", |p| p.union().map(|_| ()))?;
            if name == "Object" {
                return Ok(Jd::NonPrimitive);
            }
        }
        Ok(primitive_jd(&name).unwrap_or(Jd::Name(name)))
    }
}

fn parse_jd(text: &str) -> Jd {
    let trimmed = text.trim();
    if let Some(p) = primitive_jd(trimmed) {
        return p;
    }
    if is_identifier(trimmed) {
        return if UNSUPPORTED_NAMES.contains(&trimmed) {
            Jd::Unknown
        } else {
            Jd::Name(trimmed.to_owned())
        };
    }
    let Some((tokens, breaks)) = jd_tokens(text) else {
        return Jd::Unknown;
    };
    let mut stack: Vec<&str> = Vec::new();
    for t in &tokens {
        match t.as_str() {
            "(" | "[" | "{" | "<" => stack.push(t),
            ")" | "]" | "}" | ">" => {
                let open = stack.pop();
                let want = match t.as_str() {
                    ")" => "(",
                    "]" => "[",
                    "}" => "{",
                    _ => "<",
                };
                if open != Some(want) {
                    return Jd::Unknown;
                }
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Jd::Unknown;
    }
    let n = tokens.len();
    let mut p = JdParser {
        tokens,
        breaks,
        pos: 0,
        depth: 0,
    };
    match p.optional() {
        Ok(t) if p.pos == n => t,
        _ => Jd::Unknown,
    }
}

fn jd_members(t: &Jd) -> Vec<&Jd> {
    match t {
        Jd::Union(items) => items
            .iter()
            .flat_map(|i| {
                if matches!(i, Jd::Union(_)) {
                    jd_members(i)
                } else {
                    vec![i]
                }
            })
            .collect(),
        Jd::Inter(items) => items
            .iter()
            .flat_map(|i| {
                if matches!(i, Jd::Inter(_)) {
                    jd_members(i)
                } else {
                    vec![i]
                }
            })
            .collect(),
        _ => vec![t],
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Target,
    NonTarget,
    Nullish,
    Unknown,
}

fn jd_class(t: &Jd, targets: &[&str], non_targets: &[&str], mixed_target: bool) -> Class {
    let norm = |c: Class| {
        if c == Class::Nullish {
            Class::NonTarget
        } else {
            c
        }
    };
    let union = |cs: Vec<Class>| {
        let cs: Vec<Class> = cs.into_iter().map(norm).collect();
        if cs.is_empty() {
            Class::NonTarget
        } else if if mixed_target {
            cs.contains(&Class::Target)
        } else {
            cs.iter().all(|&c| c == Class::Target)
        } {
            Class::Target
        } else if cs.iter().all(|&c| c == Class::NonTarget) {
            Class::NonTarget
        } else {
            Class::Unknown
        }
    };
    let rec = |x: &Jd| jd_class(x, targets, non_targets, mixed_target);
    match t {
        Jd::Nullable(e) | Jd::Optional(e) => union(vec![rec(e), Class::Nullish]),
        Jd::Union(_) => union(jd_members(t).into_iter().map(rec).collect()),
        Jd::Inter(items) => {
            let cs: Vec<Class> = items.iter().map(|i| norm(rec(i))).collect();
            if cs.contains(&Class::Unknown) {
                Class::Unknown
            } else if cs.contains(&Class::Target) {
                Class::Target
            } else {
                Class::NonTarget
            }
        }
        Jd::Nullish => Class::Nullish,
        Jd::Name(n) => {
            if targets.contains(&n.as_str()) {
                Class::Target
            } else if non_targets.contains(&n.as_str())
                || matches!(
                    n.as_str(),
                    "boolean" | "string" | "number" | "bigint" | "symbol" | "void"
                )
            {
                Class::NonTarget
            } else {
                Class::Unknown
            }
        }
        Jd::NonPrimitive | Jd::Func(_) => Class::NonTarget,
        Jd::Unknown => Class::Unknown,
    }
}

fn ann_class(a: &Ann, targets: &[&str], non_targets: &[&str], mixed_target: bool) -> Class {
    let c = jd_class(&a.ty, targets, non_targets, mixed_target);
    if a.optional {
        jd_class(
            &Jd::Union(vec![a.ty.clone(), Jd::Nullish]),
            targets,
            non_targets,
            mixed_target,
        )
    } else {
        c
    }
}

const NON_BOOLEAN_NAMES: &[&str] = &["Array", "ReadonlyArray", "Promise", "PromiseLike"];

fn jd_type_state(t: &Jd, nullish: bool, fn_bool: bool) -> St {
    match t {
        Jd::Union(_) => {
            let states: Vec<St> = jd_members(t)
                .into_iter()
                .filter(|m| !nullish || **m != Jd::Nullish)
                .map(|m| jd_type_state(m, nullish, fn_bool))
                .collect();
            combine(&states)
        }
        Jd::Inter(_) => {
            let states: Vec<St> = jd_members(t)
                .into_iter()
                .filter(|m| **m != Jd::NonPrimitive)
                .map(|m| jd_type_state(m, nullish, fn_bool))
                .collect();
            if states.is_empty() {
                St::Non
            } else {
                combine(&states)
            }
        }
        Jd::Nullable(e) | Jd::Optional(e) => {
            if nullish {
                jd_type_state(e, nullish, fn_bool)
            } else {
                St::Unk
            }
        }
        Jd::Func(r) => {
            if fn_bool {
                jd_type_state(r, nullish, false)
            } else {
                St::Non
            }
        }
        _ => match jd_class(t, &["boolean"], NON_BOOLEAN_NAMES, false) {
            Class::Target => St::Bool,
            Class::NonTarget => St::Non,
            _ => St::Unk,
        },
    }
}

/// The plugin's `getJSDocumentBooleanState`.
fn jd_state(a: &Ann, nullish: bool, fn_bool: bool) -> St {
    if a.optional && !nullish {
        St::Unk
    } else {
        jd_type_state(&a.ty, nullish, fn_bool)
    }
}

/// The names of a JSDoc type's union members (`None` for a member that is not a name).
fn jd_names(t: &Jd) -> Vec<Option<String>> {
    match t {
        Jd::Union(_) => jd_members(t)
            .into_iter()
            .map(|m| match m {
                Jd::Name(n) => Some(n.clone()),
                _ => None,
            })
            .collect(),
        Jd::Name(n) => vec![Some(n.clone())],
        _ => vec![None],
    }
}

fn name_kind(n: &str) -> Option<Kind> {
    Some(match n {
        "string" => Kind::String,
        "Array" | "ReadonlyArray" => Kind::Array,
        n if TYPED_ARRAYS.contains(&n) => Kind::Array,
        "Map" | "ReadonlyMap" | "WeakMap" | "URLSearchParams" | "Headers" | "FormData" => {
            Kind::Keyed
        }
        "RegExp" => Kind::RegExp,
        "Set" | "ReadonlySet" => Kind::Set,
        "WeakSet" => Kind::WeakSet,
        _ => return None,
    })
}

/// The expression kind a JSDoc type names (every union member the same kind).
fn jd_kind(t: &Jd) -> Option<Kind> {
    let kinds: Option<Vec<Kind>> = jd_names(t)
        .into_iter()
        .map(|n| n.as_deref().and_then(name_kind))
        .collect();
    let kinds = kinds?;
    let first = *kinds.first()?;
    kinds.iter().all(|&k| k == first).then_some(first)
}

fn jd_callable_return(t: &Jd) -> Option<Jd> {
    match t {
        Jd::Func(r) => Some((**r).clone()),
        Jd::Union(items) => {
            let rets: Option<Vec<Jd>> = items.iter().map(jd_callable_return).collect();
            rets.filter(|r| !r.is_empty()).map(Jd::Union)
        }
        Jd::Inter(_) => {
            let rets: Option<Vec<Jd>> = jd_members(t)
                .into_iter()
                .filter(|m| **m != Jd::NonPrimitive)
                .map(jd_callable_return)
                .collect();
            rets.filter(|r| !r.is_empty()).map(Jd::Union)
        }
        _ => None,
    }
}

fn skip_hspace(t: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && t[i] != b'\n' && t[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn skip_template_space(t: &[u8], mut i: usize, end: usize) -> usize {
    while i < end {
        i = skip_hspace(t, i, end);
        if i >= end || t[i] != b'\n' {
            return i;
        }
        i = skip_hspace(t, i + 1, end);
        if i < end && t[i] == b'*' {
            i += 1;
        }
    }
    i
}

fn delimited_end(t: &[u8], start: usize, end: usize, open: u8) -> Option<usize> {
    let close = match open {
        b'{' => b'}',
        b'[' => b']',
        _ => b')',
    };
    let mut depth = 0;
    let mut quote = None;
    let mut i = start;
    while i < end {
        let c = t[i];
        if let Some(q) = quote {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if matches!(c, b'"' | b'\'' | b'`') {
            quote = Some(c);
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

fn tag_type(text: &str, start: usize, end: usize) -> Option<(Jd, usize)> {
    let t = text.as_bytes();
    if t.get(start) != Some(&b'{') {
        return None;
    }
    let e = delimited_end(t, start, end, b'{')?;
    let inner = text.get(start + 1..e - 1)?;
    let mut cleaned = String::new();
    for (i, line) in inner.split('\n').enumerate() {
        if i > 0 {
            cleaned.push('\n');
            let l = line.trim_start_matches([' ', '\t', '\r']);
            cleaned.push_str(l.strip_prefix('*').unwrap_or(l));
        } else {
            cleaned.push_str(line.strip_suffix('\r').unwrap_or(line));
        }
    }
    Some((parse_jd(&cleaned), e))
}

/// The supported tags of a `/** */` comment whose text (after `/*`) starts at `offset`.
fn comment_tags(text: &str, offset: u32) -> Vec<Tag> {
    let t = text.as_bytes();
    let mut starts = Vec::new();
    let mut line = 0;
    while line <= text.len() {
        let line_end = text[line..].find('\n').map_or(text.len(), |i| line + i);
        let mut i = skip_hspace(t, line, line_end);
        if i < line_end && t[i] == b'*' {
            i = skip_hspace(t, i + 1, line_end);
        }
        if i < line_end && t[i] == b'@' {
            let name_start = i + 1;
            let mut j = name_start;
            while j < line_end {
                let ch = text[j..].chars().next().unwrap_or(' ');
                if ch.is_alphanumeric() || (j > name_start && ch == '-') {
                    j += ch.len_utf8();
                } else {
                    break;
                }
            }
            if j < line_end && t[j] == b'!' {
                j += 1;
            }
            let next_ok = j >= line_end || t[j].is_ascii_whitespace() || t[j] == b'{';
            if j > name_start && next_ok {
                let name = text[name_start..j].trim_end_matches('!').to_lowercase();
                starts.push((name, j, line));
            }
        }
        line = line_end + 1;
    }
    if starts
        .iter()
        .any(|(n, ..)| matches!(n.as_str(), "typedef" | "callback" | "overload"))
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (k, (name, content, _)) in starts.iter().enumerate() {
        let end = starts.get(k + 1).map_or(text.len(), |s| s.2);
        if let Some(tag) = parse_tag(text, name, *content, end, offset) {
            out.push(tag);
        }
    }
    out
}

fn parse_tag(text: &str, name: &str, content: usize, end: usize, offset: u32) -> Option<Tag> {
    let is_param = matches!(name, "param" | "arg" | "argument");
    if !is_param && !matches!(name, "type" | "return" | "returns") {
        return None;
    }
    let t = text.as_bytes();
    let bare = Tag {
        name: name.to_owned(),
        ty: None,
        param: None,
        optional: false,
        name_range: None,
    };
    let mut start = skip_template_space(t, content, end);
    let mut ty = None;
    if t.get(start) == Some(&b'{') {
        let Some((parsed, e)) = tag_type(text, start, end) else {
            return Some(bare);
        };
        ty = Some(parsed);
        start = skip_template_space(t, e, end);
    } else if !is_param {
        return Some(bare);
    }
    if !is_param {
        return Some(Tag { ty, ..bare });
    }
    let optional = t.get(start) == Some(&b'[');
    let name_end = if optional {
        delimited_end(t, start, end, b'[')?
    } else {
        end
    };
    if name_end < end && !t[name_end].is_ascii_whitespace() {
        return None;
    }
    if optional {
        start += 1;
    }
    let rest = text.get(start..name_end)?;
    let mut chars = rest.char_indices();
    let len = match chars.next() {
        Some((_, c)) if is_id_start(c) => chars
            .find(|&(_, c)| !is_id_continue(c))
            .map_or(rest.len(), |(i, _)| i),
        _ => return None,
    };
    let pname = rest[..len].to_owned();
    let after = start + len;
    let next = skip_template_space(t, after, name_end);
    if optional {
        if !matches!(t.get(next), Some(b']' | b'=')) {
            return None;
        }
    } else if after < end && !t[after].is_ascii_whitespace() {
        return None;
    }
    let name_range = Some(Span::new(offset + start as u32, offset + after as u32));
    if ty.is_none() {
        let ts = skip_template_space(t, if optional { name_end } else { after }, end);
        ty = tag_type(text, ts, end).map(|(j, _)| j);
    }
    Some(Tag {
        ty,
        param: Some(pname),
        optional,
        name_range,
        ..bare
    })
}

/// An applicable tag's type (if it has one), optionality and parameter-name range.
#[derive(Clone, Debug)]
struct RawAnn {
    ty: Option<Jd>,
    optional: bool,
    name_range: Option<Span>,
}

impl RawAnn {
    fn typed(self) -> Option<Ann> {
        Some(Ann {
            ty: self.ty?,
            optional: self.optional,
        })
    }
}

/// The one tag matching `pred` (none when several do).
fn single_annotation(tags: &[Tag], pred: impl Fn(&Tag) -> bool) -> Option<RawAnn> {
    let mut it = tags.iter().filter(|t| pred(t));
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(RawAnn {
        ty: first.ty.clone(),
        optional: first.optional,
        name_range: first.name_range,
    })
}

impl<'c, 'x, 'f> An<'c, 'x, 'f> {
    /// The tags of the comment right before `pos` (`Some(vec![])` for a comment that is not
    /// JSDoc), `None` without a comment.
    fn tags_before(&self, pos: u32) -> Option<Vec<Tag>> {
        let ast = self.ast();
        let src = self.cx.src();
        let p = src[..pos as usize].trim_end().len() as u32;
        let i = ast.comments.partition_point(|c| c.span.end < p);
        let c = ast.comments.get(i).filter(|c| c.span.end == p)?;
        let text = &src[c.span.start as usize..c.span.end as usize];
        if c.kind != CommentKind::Block || !text.starts_with("/**") || text.len() < 4 {
            return Some(Vec::new());
        }
        Some(comment_tags(&text[2..text.len() - 2], c.span.start + 2))
    }

    fn doc_tags(&self, owners: &[NodeId]) -> Vec<Tag> {
        if !self.jsdoc {
            return Vec::new();
        }
        owners
            .iter()
            .find_map(|&o| self.tags_before(self.cx.span(o).start))
            .unwrap_or_default()
    }

    /// A function's documentation owners: TypeScript wrappers, then the declarator and its
    /// statement, the assignment and its statement, or the property it is the value of.
    fn function_owners(&self, func: NodeId) -> Vec<NodeId> {
        let ast = self.ast();
        let mut owners = vec![func];
        let mut node = func;
        for _ in 0..64 {
            let Some(p) = self.cx.parent(node) else {
                break;
            };
            match *ast.node(p) {
                Node::As { expr, .. }
                | Node::Satisfies { expr, .. }
                | Node::NonNull { expr }
                | Node::TypeAssertion { expr, .. }
                    if expr == node =>
                {
                    owners.push(p);
                    node = p;
                }
                Node::Paren { .. } => node = p,
                _ => break,
            }
        }
        let Some(parent) = self.cx.parent(node) else {
            return owners;
        };
        match *ast.node(parent) {
            Node::VarDeclarator { init: Some(i), .. } if i == node => {
                owners.push(parent);
                if let Some(d) = self.cx.parent(parent)
                    && matches!(*ast.node(d), Node::VarDecl { decls, .. } if ast.list(decls).len() == 1)
                {
                    owners.push(d);
                }
            }
            Node::Assign {
                op: AssignOp::Assign,
                right,
                ..
            } if right == node => {
                if let Some(stmt) = self.cx.parent(parent)
                    && matches!(ast.node(stmt), Node::ExprStmt { .. })
                {
                    owners.push(parent);
                    owners.push(stmt);
                }
            }
            Node::Property { value, .. } if value == node => owners.push(parent),
            Node::PropertyDef { value: Some(v), .. } if v == node => owners.push(parent),
            Node::MethodDef { value, .. } if value == node => owners.push(parent),
            Node::ExportDefaultExpr { .. } => owners.push(parent),
            _ => {}
        }
        owners
    }

    fn has_native_assertion(&self, id: Option<NodeId>) -> bool {
        let ast = self.ast();
        let Some(mut id) = id else {
            return false;
        };
        for _ in 0..64 {
            match *ast.node(id) {
                Node::As { .. } | Node::TypeAssertion { .. } => return true,
                Node::Satisfies { expr, .. } | Node::NonNull { expr } | Node::Paren { expr } => {
                    id = expr
                }
                _ => return false,
            }
        }
        false
    }

    /// The plugin's `getParameterAnnotation` for the parameter named by `ident`.
    fn param_annotation(&self, ident: NodeId) -> Option<RawAnn> {
        if !self.jsdoc {
            return None;
        }
        let ast = self.ast();
        let param = self.cx.parent(ident)?;
        let Node::Param {
            pattern, optional, ..
        } = *ast.node(param)
        else {
            return None;
        };
        if pattern != ident {
            return None;
        }
        let func = self
            .cx
            .parent(param)
            .filter(|&f| fn_flags(ast, f).is_some())?;
        let Node::Ident { name } = *ast.node(ident) else {
            return None;
        };
        let name = ast.atom(name);
        let inline = single_annotation(&self.doc_tags(&[ident, param]), |t| t.name == "type");
        let named = single_annotation(&self.doc_tags(&self.function_owners(func)), |t| {
            matches!(t.name.as_str(), "param" | "arg" | "argument")
                && t.param.as_deref() == Some(name)
        });
        let ann = match &inline {
            Some(i) if i.ty.is_some() => RawAnn {
                name_range: named.as_ref().and_then(|n| n.name_range),
                ..i.clone()
            },
            _ => named.clone()?,
        };
        let is_opt = |a: &Option<RawAnn>| {
            a.as_ref()
                .is_some_and(|a| a.optional || matches!(a.ty, Some(Jd::Optional(_))))
        };
        let optional = optional || is_opt(&inline) || is_opt(&named);
        Some(RawAnn {
            ty: ann.ty.map(|t| match t {
                Jd::Optional(e) => *e,
                t => t,
            }),
            optional,
            name_range: ann.name_range,
        })
    }

    /// The plugin's `getJSDocumentTypeAnnotation` for a binding, reference or property.
    fn doc_type(&self, node: NodeId) -> Option<Ann> {
        if !self.jsdoc {
            return None;
        }
        let ast = self.ast();
        let owners = match *ast.node(node) {
            Node::Property { value, .. } => {
                if self.has_native_assertion(Some(value)) {
                    return None;
                }
                vec![node]
            }
            Node::ShorthandProp { .. } => vec![node],
            Node::PropertyDef {
                type_ann, value, ..
            } => {
                if type_ann.is_some() || self.has_native_assertion(value) {
                    return None;
                }
                vec![node]
            }
            Node::Ident { .. } => {
                let parent = self.cx.parent(node)?;
                match *ast.node(parent) {
                    Node::VarDeclarator {
                        name,
                        type_ann,
                        init,
                        ..
                    } if name == node => {
                        if type_ann.is_some() || self.has_native_assertion(init) {
                            return None;
                        }
                        let mut o = vec![node, parent];
                        if let Some(d) = self.cx.parent(parent)
                            && matches!(*ast.node(d), Node::VarDecl { decls, .. } if ast.list(decls).len() == 1)
                        {
                            o.push(d);
                        }
                        o
                    }
                    Node::Param {
                        pattern, type_ann, ..
                    } if pattern == node => {
                        if type_ann.is_some() {
                            return None;
                        }
                        return self.param_annotation(node).and_then(RawAnn::typed);
                    }
                    _ => {
                        let sym = self.sym_of_ref(node)?;
                        let decls = &self.cx.semantic().symbol(sym).decls;
                        let [d] = decls[..] else {
                            return None;
                        };
                        if d.name == node || !matches!(ast.node(d.name), Node::Ident { .. }) {
                            return None;
                        }
                        return self.doc_type(d.name);
                    }
                }
            }
            _ => return None,
        };
        single_annotation(&self.doc_tags(&owners), |t| t.name == "type").and_then(RawAnn::typed)
    }

    /// The plugin's `getJSDocumentValueTypeAnnotation`.
    fn doc_value_type(&self, node: NodeId) -> Option<Ann> {
        let ann = self.doc_type(node)?;
        let ast = self.ast();
        let value = match *ast.node(node) {
            Node::Ident { .. } => match self.cx.parent(node).map(|p| ast.node(p)) {
                Some(&Node::VarDeclarator { name, init, .. }) if name == node => init,
                _ => None,
            },
            Node::Property { value, .. } => Some(value),
            Node::PropertyDef { value, .. } => value,
            _ => None,
        };
        let func = value.and_then(|v| self.function_value(v));
        let drop = func.is_some_and(|f| {
            fn_flags(ast, f).is_some_and(|fl| fl.contains(FnFlags::GENERATOR))
                || fn_return_type(ast, f).is_some()
        }) && jd_callable_return(&ann.ty).is_some();
        (!drop).then_some(ann)
    }

    /// The plugin's `getJSDocumentReturnType`.
    fn doc_return(&self, func: NodeId) -> Option<Ann> {
        if !self.jsdoc || fn_return_type(self.ast(), func).is_some() {
            return None;
        }
        let ast = self.ast();
        let owners = self.function_owners(func);
        if owners.iter().any(|&o| match *ast.node(o) {
            Node::VarDeclarator { type_ann, .. } | Node::PropertyDef { type_ann, .. } => {
                type_ann.is_some()
            }
            _ => false,
        }) {
            return None;
        }
        let getter = self.cx.parent(func).is_some_and(|p| {
            matches!(*ast.node(p), Node::MethodDef { kind: MethodKind::Get, value, .. } if value == func)
        });
        single_annotation(&self.doc_tags(&owners), |t| {
            matches!(t.name.as_str(), "return" | "returns") || (getter && t.name == "type")
        })
        .and_then(RawAnn::typed)
    }

    /// The plugin's `getJSDocumentFunctionReturnType`: not when it documents a promise.
    fn doc_fn_return(&self, func: NodeId) -> Option<Ann> {
        self.doc_return(func)
            .filter(|a| ann_class(a, &["Promise", "PromiseLike"], &[], true) != Class::Target)
    }

    /// The plugin's `hasJSDocumentTypeReference`.
    fn has_doc_type_ref(&self, id: NodeId, seen: &mut Vec<SymbolId>) -> bool {
        if !self.jsdoc || seen.len() > 64 {
            return false;
        }
        let ast = self.ast();
        let mut id = id;
        for _ in 0..64 {
            match *ast.node(id) {
                Node::As { expr, .. }
                | Node::Satisfies { expr, .. }
                | Node::NonNull { expr }
                | Node::TypeAssertion { expr, .. }
                | Node::Paren { expr } => id = expr,
                _ => break,
            }
        }
        match *ast.node(id) {
            Node::Ident { .. } => {
                if self.doc_type(id).is_some() {
                    return true;
                }
                let Some(sym) = self.sym_of_ref(id) else {
                    return false;
                };
                let decls = &self.cx.semantic().symbol(sym).decls;
                let [d] = decls[..] else {
                    return false;
                };
                let Node::VarDeclarator {
                    name,
                    init: Some(init),
                    ..
                } = *ast.node(d.node)
                else {
                    return false;
                };
                if name != d.name || seen.contains(&sym) {
                    return false;
                }
                seen.push(sym);
                self.has_doc_type_ref(init, seen)
            }
            Node::Arrow { .. }
            | Node::FunctionExpr { .. }
            | Node::FunctionDecl { .. }
            | Node::ClassExpr { .. }
            | Node::ClassDecl { .. } => false,
            _ => evaluated_children(ast, id)
                .into_iter()
                .any(|c| self.has_doc_type_ref(c, seen)),
        }
    }

    fn doc_ref(&self, id: NodeId) -> bool {
        self.has_doc_type_ref(id, &mut Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_and_replacements() {
        let p: Vec<String> = DEFAULT_PREFIXES.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(boolean_prefix("isOpen", &p), Some("is"));
        assert_eq!(boolean_prefix("_IS_OPEN", &p), Some("is"));
        assert_eq!(boolean_prefix("island", &p), None);
        assert_eq!(boolean_prefix("is", &p), None);
        assert_eq!(replacement_name("open", "is"), "isOpen");
        assert_eq!(replacement_name("__OPEN", "has"), "__HAS_OPEN");
        assert_eq!(hook_replacement_name("useOpen", "open", "is"), "useIsOpen");
        assert_eq!(combine(&[St::Bool, St::Non]), St::Non);
        assert_eq!(combine(&[]), St::Unk);
    }
}
