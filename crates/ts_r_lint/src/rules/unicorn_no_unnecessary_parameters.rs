//! `unicorn/no-unnecessary-parameters`: a parameter of a local function that receives the
//! same value at every call (or always its default, or always `undefined`) is not needed.
//!
//! Only functions whose callers are all known are checked: a function, arrow, function or
//! class expression bound once (not in the global scope, never reassigned, not exported or
//! decorated) whose every runtime reference is a direct call (`new` for classes, which
//! check their explicit constructor), and `#private` methods called through any object. A
//! file with `with`, a direct `eval` or a non-strict block-level function declaration is
//! skipped, and so is a function using its own `arguments`, with a decorated parameter,
//! with type parameters, or called from fewer than `minimumCallCount` places outside
//! itself.
//!
//! Each plain parameter (and each shallow property of an object-pattern parameter, with an
//! optional empty `{}` default) is compared across the calls. A value is a primitive
//! literal (`-1`, `` `x` ``, `void 0`, an omitted argument), or a stable binding: a single
//! non-`var` declaration never written after it, or a read-only global. A recursive call
//! forwarding the parameter unchanged doesn't count; then a default only counts when it is
//! a primitive or a binding outside the function. Parameters named by a type predicate
//! (`x is T`, `asserts x`) are skipped.
//!
//! The first fixable parameter of a function gets a fix (a suggestion in TypeScript or in
//! files with JSDoc types): the parameter and its arguments are removed and its references
//! replaced by the value, or a final default naming an earlier parameter becomes a local
//! `const`/`let`. No fix when comments are in the way, the value is a global, a parameter
//! or an uninitialized binding, or a reference would be captured by another binding.

use super::prelude::*;
use super::unicorn_ast::{linebreak, remove_argument};
use super::unicorn_text::needs_semicolon;
use super::unicorn_util::with_edits;
use super::unicorn_util::{indent_unit, line_indent};
use crate::env::{GlobalAccess, SourceType};
use crate::semantic::Decl;
use ts_r_ast::Ast;

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-unnecessary-parameters",
        "Disallow parameters that receive the same value at every call.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create::<NoUnnecessaryParameters>,
};

#[derive(Debug)]
struct NoUnnecessaryParameters {
    minimum_call_count: usize,
}

impl FromOptions for NoUnnecessaryParameters {
    fn from_options(opts: &[Value]) -> Result<Self, String> {
        let o = obj_opt(opts, 0);
        Ok(NoUnnecessaryParameters {
            minimum_call_count: o
                .get("minimumCallCount")
                .and_then(Value::as_u64)
                .map_or(2, |n| n.max(1) as usize),
        })
    }
}

const RESERVED: &[&str] = &[
    "arguments",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const OBJECT_PROTOTYPE_KEYS: &[&str] = &[
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
    "__proto__",
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
];

#[derive(Clone, Debug, PartialEq)]
enum Prim {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    BigInt(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bind {
    Sym(SymbolId),
    /// A read-only global, by name.
    Global(ts_r_ast::Atom),
}

#[derive(Clone, Debug)]
enum Val {
    Prim(Prim, Option<NodeId>),
    Binding(Bind),
}

impl Val {
    fn is_undefined(&self) -> bool {
        matches!(self, Val::Prim(Prim::Undefined, _))
    }

    fn same(&self, other: &Val) -> bool {
        match (self, other) {
            (Val::Binding(a), Val::Binding(b)) => a == b,
            (Val::Prim(a, _), Val::Prim(b, _)) => match (a, b) {
                (Prim::Num(x), Prim::Num(y)) => {
                    (x.is_nan() && y.is_nan()) || x.to_bits() == y.to_bits()
                }
                _ => a == b,
            },
            _ => false,
        }
    }
}

/// One checked parameter: a plain one, or a property of an object-pattern parameter.
#[derive(Clone, Copy, Debug)]
struct Param {
    /// The `Param`, or the `BindingProperty` for a pattern property.
    node: NodeId,
    ident: NodeId,
    sym: SymbolId,
    index: usize,
    default: Option<NodeId>,
    /// The `Param` holding a pattern property.
    pattern_param: Option<NodeId>,
}

#[derive(Clone, Debug)]
struct Arg {
    node: Option<NodeId>,
    value: Option<Val>,
    forwarding: bool,
}

struct Outcome {
    id: &'static str,
    value: Option<Val>,
    args: Vec<Arg>,
}

/// A local function whose callers are all known (`local_call_targets`).
#[derive(Debug)]
pub(super) struct Target {
    pub(super) func: NodeId,
    /// The calls (or `new`s) of the function, in source order.
    pub(super) calls: Vec<NodeId>,
    excluded: bool,
}

fn unwrap_ts(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

struct FnParts<'a> {
    params: &'a [NodeId],
    body: Option<NodeId>,
    flags: FnFlags,
    type_params: Option<NodeId>,
    return_type: Option<NodeId>,
    arrow: bool,
}

fn fn_parts(ast: &Ast, f: NodeId) -> Option<FnParts<'_>> {
    Some(match *ast.node(f) {
        Node::FunctionDecl {
            flags,
            type_params,
            params,
            return_type,
            body,
            ..
        }
        | Node::FunctionExpr {
            flags,
            type_params,
            params,
            return_type,
            body,
            ..
        } => FnParts {
            params: ast.list(params),
            body,
            flags,
            type_params,
            return_type,
            arrow: false,
        },
        Node::Arrow {
            flags,
            type_params,
            params,
            return_type,
            body,
        } => FnParts {
            params: ast.list(params),
            body: Some(body),
            flags,
            type_params,
            return_type,
            arrow: true,
        },
        _ => return None,
    })
}

fn constructor_of(ast: &Ast, class: NodeId) -> Option<NodeId> {
    let members = match *ast.node(class) {
        Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } => members,
        _ => return None,
    };
    ast.list(members).iter().find_map(|&m| match *ast.node(m) {
        Node::MethodDef {
            kind: MethodKind::Constructor,
            value,
            ..
        } => Some(value),
        _ => None,
    })
}

fn has_decorators(ast: &Ast, id: NodeId) -> bool {
    match *ast.node(id) {
        Node::ClassDecl { decorators, .. }
        | Node::ClassExpr { decorators, .. }
        | Node::MethodDef { decorators, .. }
        | Node::Param { decorators, .. } => !ast.list(decorators).is_empty(),
        _ => false,
    }
}

fn inside(outer: Span, inner: Span) -> bool {
    inner.start >= outer.start && inner.end <= outer.end
}

/// The static name of a property key (`a`, `"a"`, `1`, `["a"]`, `` [`a`] ``).
fn key_name(ast: &Ast, key: NodeId) -> Option<String> {
    let lit = |k: NodeId| match *ast.node(k) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::TemplateLit { parts } => match ast.list(parts) {
            [e] => match *ast.node(*e) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    };
    match *ast.node(key) {
        Node::Ident { name } => Some(ast.atom(name).to_owned()),
        Node::ComputedKey { expr } => lit(skip_parens(ast, expr)),
        _ => lit(key),
    }
}

/// `(name, property)` of an object literal or pattern, `None` when any property is not a
/// plain static one (or repeats a name).
fn properties(ast: &Ast, obj: NodeId) -> Option<Vec<(String, NodeId)>> {
    let (list, pattern) = match *ast.node(obj) {
        Node::ObjectLit { props } => (props, false),
        Node::ObjectPattern { props } => (props, true),
        _ => return None,
    };
    let mut out: Vec<(String, NodeId)> = Vec::new();
    for &p in ast.list(list) {
        let name = match *ast.node(p) {
            Node::Rest { .. } if pattern => continue,
            Node::Property { key, .. } | Node::BindingProperty { key, .. } => key_name(ast, key)?,
            Node::ShorthandProp { name, .. } => ident_name(ast, name)?.to_owned(),
            _ => return None,
        };
        if name == "__proto__" || out.iter().any(|(n, _)| *n == name) {
            return None;
        }
        out.push((name, p));
    }
    Some(out)
}

fn number_value(ast: &Ast, id: NodeId) -> Option<f64> {
    match *ast.node(id) {
        Node::NumberLit { value } => Some(value.get()),
        _ => None,
    }
}

fn bigint_value(ast: &Ast, src: &str, id: NodeId) -> Option<String> {
    let Node::BigIntLit { .. } = *ast.node(id) else {
        return None;
    };
    let span = ast.span(id);
    let text = src
        .get(span.start as usize..span.end as usize)?
        .trim_end_matches('n')
        .replace('_', "");
    let lower = text.to_ascii_lowercase();
    let (digits, radix) = if let Some(d) = lower.strip_prefix("0x") {
        (d, 16)
    } else if let Some(d) = lower.strip_prefix("0o") {
        (d, 8)
    } else if let Some(d) = lower.strip_prefix("0b") {
        (d, 2)
    } else {
        (lower.as_str(), 10)
    };
    Some(match u128::from_str_radix(digits, radix) {
        Ok(v) => v.to_string(),
        Err(_) => lower,
    })
}

struct An<'c, 'x, 'f> {
    cx: &'c Ctx<'x, 'f>,
}

impl An<'_, '_, '_> {
    fn ast(&self) -> &Ast {
        self.cx.ast()
    }

    fn resolution(&self, ident: NodeId) -> Option<Resolution> {
        let sem = self.cx.semantic();
        sem.reference_at(ident).map(|r| sem.reference(r).resolution)
    }

    fn has_writes(&self, sym: SymbolId) -> bool {
        let sem = self.cx.semantic();
        sem.symbol(sym).refs.iter().any(|&r| {
            let r = sem.reference(r);
            r.is_write() && !r.flags.contains(RefFlags::INIT)
        })
    }

    fn global_has_writes(&self, name: ts_r_ast::Atom) -> bool {
        self.cx.semantic().references().iter().any(|r| {
            r.name == name
                && !matches!(r.resolution, Resolution::Symbol(_))
                && r.is_write()
                && !r.flags.contains(RefFlags::INIT)
        })
    }

    fn primitive(&self, id: NodeId) -> Option<Prim> {
        let ast = self.ast();
        let src = self.cx.src();
        Some(match *ast.node(id) {
            Node::StringLit { value } => Prim::Str(ast.atom(value).to_owned()),
            Node::NumberLit { value } => Prim::Num(value.get()),
            Node::BoolLit { value } => Prim::Bool(value),
            Node::NullLit {} => Prim::Null,
            Node::BigIntLit { .. } => Prim::BigInt(bigint_value(ast, src, id)?),
            Node::TemplateLit { .. } => {
                Prim::Str(super::unicorn_text::static_string_value(ast, id)?)
            }
            Node::Unary { op, arg } => {
                let arg = unwrap_ts(ast, arg);
                match op {
                    UnaryOp::Minus => {
                        if let Some(n) = number_value(ast, arg) {
                            Prim::Num(-n)
                        } else {
                            Prim::BigInt(format!("-{}", bigint_value(ast, src, arg)?))
                        }
                    }
                    UnaryOp::Plus => Prim::Num(number_value(ast, arg)?),
                    UnaryOp::Void if number_value(ast, arg) == Some(0.0) => Prim::Undefined,
                    _ => return None,
                }
            }
            _ => return None,
        })
    }

    fn value(&self, node: Option<NodeId>) -> Option<Val> {
        let Some(node) = node else {
            return Some(Val::Prim(Prim::Undefined, None));
        };
        let ast = self.ast();
        let node = unwrap_ts(ast, node);
        if let Some(p) = self.primitive(node) {
            return Some(Val::Prim(p, Some(node)));
        }
        let Node::Ident { name } = *ast.node(node) else {
            return None;
        };
        let sem = self.cx.semantic();
        let global = |this: &Self, writable: bool| {
            if this.global_has_writes(name) {
                return None;
            }
            if ast.atom(name) == "undefined" {
                return Some(Val::Prim(Prim::Undefined, Some(node)));
            }
            (!writable).then_some(Val::Binding(Bind::Global(name)))
        };
        match self.resolution(node)? {
            Resolution::Global => {
                let access = self.cx.env().globals.get(ast.atom(name))?;
                global(self, access == GlobalAccess::Writable)
            }
            Resolution::Unresolved => None,
            Resolution::Symbol(s) => {
                let sym = sem.symbol(s);
                if sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
                    let g = self
                        .cx
                        .file
                        .comment_globals
                        .iter()
                        .rev()
                        .find(|g| g.name == ast.atom(name))?;
                    return global(self, g.writable);
                }
                if self.has_writes(s) {
                    return None;
                }
                let [d] = sym.decls[..] else {
                    return None;
                };
                let ok = match d.kind {
                    DeclKind::Let | DeclKind::Const | DeclKind::Using => !self.is_declare(d),
                    DeclKind::Function => {
                        !self.is_declare(d)
                            && !matches!(*ast.node(d.node), Node::FunctionDecl { body: None, .. })
                    }
                    DeclKind::Class => !self.is_declare(d),
                    DeclKind::FunctionExprName
                    | DeclKind::ClassExprName
                    | DeclKind::Param
                    | DeclKind::CatchParam => true,
                    _ => false,
                };
                ok.then_some(Val::Binding(Bind::Sym(s)))
            }
        }
    }

    fn is_declare(&self, d: Decl) -> bool {
        let ast = self.ast();
        let declare = |id: NodeId| match *ast.node(id) {
            Node::VarDecl { modifiers, .. }
            | Node::FunctionDecl { modifiers, .. }
            | Node::ClassDecl { modifiers, .. } => modifiers.contains(Modifiers::DECLARE),
            _ => false,
        };
        declare(d.node) || declare(d.parent)
    }

    fn bind_name(&self, b: Bind) -> String {
        match b {
            Bind::Sym(s) => self.cx.semantic().name(s).to_owned(),
            Bind::Global(a) => self.ast().atom(a).to_owned(),
        }
    }

    /// The binding is the one the name means just outside the function.
    fn is_outer_binding(&self, b: Bind, func: NodeId) -> bool {
        let sem = self.cx.semantic();
        let Some(scope) = sem.scope_of_node(func) else {
            return false;
        };
        let Some(upper) = sem.scope(scope).parent else {
            return false;
        };
        let found = sem.lookup(upper, &self.bind_name(b));
        match b {
            Bind::Sym(s) => found == Some(s),
            Bind::Global(_) => {
                found.is_none_or(|f| sem.symbol(f).flags.contains(SymbolFlags::COMMENT_GLOBAL))
            }
        }
    }

    fn is_strict(&self, node: NodeId) -> bool {
        let cx = self.cx;
        if cx.env().source_type == SourceType::Module || cx.env().implied_strict {
            return true;
        }
        let ast = self.ast();
        let has_use_strict = |stmts: &[NodeId]| {
            stmts.iter().any(|&s| {
                matches!(*ast.node(s), Node::Directive { .. }) && {
                    // The raw text, so an escaped `"use\x20strict"` doesn't count.
                    let t = cx.text(s);
                    t.starts_with("'use strict'") || t.starts_with("\"use strict\"")
                }
            })
        };
        let mut cur = Some(node);
        for _ in 0..100_000 {
            let Some(c) = cur else { break };
            match *ast.node(c) {
                Node::ClassDecl { .. } | Node::ClassExpr { .. } => return true,
                Node::Program { stmts, .. } => return has_use_strict(ast.list(stmts)),
                Node::FunctionDecl { body: Some(b), .. }
                | Node::FunctionExpr { body: Some(b), .. } => {
                    if let Node::Block { stmts } = *ast.node(b)
                        && has_use_strict(ast.list(stmts))
                    {
                        return true;
                    }
                }
                _ => {}
            }
            cur = cx.parent(c);
        }
        false
    }
}

/// The call or `new` whose callee is `id` (through parentheses and TS wrappers).
fn call_of(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    let mut cur = id;
    for _ in 0..1000 {
        let p = cx.parent(cur)?;
        match *ast.node(p) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. } => cur = p,
            Node::Call { callee, .. } | Node::New { callee, .. } if callee == cur => {
                return Some(p);
            }
            _ => return None,
        }
    }
    None
}

fn is_exported(cx: &Ctx<'_, '_>, d: Decl) -> bool {
    let ast = cx.ast();
    let check = |id: NodeId| match *ast.node(id) {
        Node::VarDecl { modifiers, .. }
        | Node::FunctionDecl { modifiers, .. }
        | Node::ClassDecl { modifiers, .. } => {
            modifiers.intersects(Modifiers::EXPORT | Modifiers::DEFAULT)
        }
        _ => false,
    };
    check(d.node)
        || check(d.parent)
        || cx
            .parent(d.parent)
            .is_some_and(|p| matches!(ast.node(p), Node::ExportDefaultExpr { .. }))
}

impl NoUnnecessaryParameters {
    fn file_is_dynamic(cx: &Ctx<'_, '_>, an: &An<'_, '_, '_>) -> bool {
        let ast = cx.ast();
        for i in 1..=ast.node_count() as u32 {
            let Some(id) = NodeId::from_raw(i) else {
                continue;
            };
            match *ast.node(id) {
                Node::With { .. } => return true,
                Node::Call {
                    callee,
                    optional: false,
                    ..
                } if ident_name(ast, unwrap_ts(ast, callee)) == Some("eval") => return true,
                Node::FunctionDecl { .. } => {
                    let in_block = cx.parent(id).is_some_and(|p| {
                        !matches!(ast.node(p), Node::Program { .. })
                            && !cx.parent(p).is_some_and(|g| {
                                fn_parts(ast, g).is_some_and(|f| f.body == Some(p))
                            })
                            && !matches!(
                                ast.node(p),
                                Node::ExportDefaultExpr { .. } | Node::ExportNamed { .. }
                            )
                    });
                    if in_block && !an.is_strict(id) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn targets(cx: &Ctx<'_, '_>) -> Vec<Target> {
        let ast = cx.ast();
        let sem = cx.semantic();
        let mut targets: Vec<Target> = Vec::new();
        let mut index: rustc_hash::FxHashMap<NodeId, usize> = Default::default();
        fn get<'t>(
            targets: &'t mut Vec<Target>,
            index: &mut rustc_hash::FxHashMap<NodeId, usize>,
            func: NodeId,
        ) -> &'t mut Target {
            let i = *index.entry(func).or_insert_with(|| {
                targets.push(Target {
                    func,
                    calls: Vec::new(),
                    excluded: false,
                });
                targets.len() - 1
            });
            &mut targets[i]
        }
        for (i, sym) in sem.symbols().iter().enumerate() {
            let [d] = sym.decls[..] else { continue };
            let class_def = matches!(d.kind, DeclKind::Class | DeclKind::ClassExprName);
            let func = match d.kind {
                DeclKind::Function | DeclKind::FunctionExprName => match *ast.node(d.node) {
                    Node::FunctionDecl { .. } | Node::FunctionExpr { .. } => Some(d.node),
                    _ => None,
                },
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using => {
                    match *ast.node(d.node) {
                        Node::VarDeclarator {
                            name,
                            init: Some(init),
                            ..
                        } if name == d.name => {
                            let init = unwrap_ts(ast, init);
                            match *ast.node(init) {
                                Node::FunctionExpr { .. } | Node::Arrow { .. } => Some(init),
                                Node::ClassExpr { decorators, .. }
                                    if ast.list(decorators).is_empty() =>
                                {
                                    constructor_of(ast, init)
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                }
                DeclKind::Class | DeclKind::ClassExprName => match *ast.node(d.node) {
                    Node::ClassDecl { .. } | Node::ClassExpr { .. } => constructor_of(ast, d.node),
                    _ => None,
                },
                _ => None,
            };
            let Some(func) = func else { continue };
            if fn_parts(ast, func).is_none_or(|f| f.body.is_none()) {
                continue;
            }
            let sym_id = SymbolId(i as u32);
            let excluded = sem.scope(sym.scope).kind == ScopeKind::Global
                || sym.refs.iter().any(|&r| {
                    let r = sem.reference(r);
                    r.is_write() && !r.flags.contains(RefFlags::INIT)
                })
                || is_exported(cx, d)
                || (class_def && has_decorators(ast, d.node))
                || cx.parent(func).is_some_and(|p| has_decorators(ast, p));
            let mut calls = Vec::new();
            let mut bad = excluded;
            for &r in &sem.symbol(sym_id).refs {
                let r = sem.reference(r);
                if r.flags.contains(RefFlags::INIT)
                    || r.is_type()
                    || r.flags.contains(RefFlags::TYPEOF_TYPE)
                    || in_type_only_export(cx, r.node)
                {
                    continue;
                }
                match call_of(cx, r.node) {
                    Some(c) if !class_def || matches!(ast.node(c), Node::New { .. }) => {
                        calls.push(c)
                    }
                    _ => bad = true,
                }
            }
            let t = get(&mut targets, &mut index, func);
            t.excluded |= bad;
            t.calls.extend(calls);
        }
        // `#private` methods.
        for i in 1..=ast.node_count() as u32 {
            let Some(id) = NodeId::from_raw(i) else {
                continue;
            };
            let Node::Member { prop, .. } = *ast.node(id) else {
                continue;
            };
            let Node::PrivateName { name } = *ast.node(prop) else {
                continue;
            };
            let Some((method, func)) = private_method(cx, id, name) else {
                continue;
            };
            let call = call_of(cx, id);
            let t = get(&mut targets, &mut index, func);
            match call {
                Some(c) if !has_decorators(ast, method) => t.calls.push(c),
                _ => t.excluded = true,
            }
        }
        for t in &mut targets {
            t.calls.sort_unstable();
            t.calls.dedup();
        }
        targets.retain(|t| !t.excluded);
        targets
    }
}

/// `x` in `export type { x }` or `export { type x }`: not a runtime reference.
fn in_type_only_export(cx: &Ctx<'_, '_>, ident: NodeId) -> bool {
    let ast = cx.ast();
    let Some(spec) = cx.parent(ident) else {
        return false;
    };
    let Node::ExportSpecifier { type_only, .. } = *ast.node(spec) else {
        return false;
    };
    type_only
        || cx.parent(spec).is_some_and(|e| {
            matches!(
                *ast.node(e),
                Node::ExportNamed {
                    type_only: true,
                    ..
                }
            )
        })
}

/// The method a `#name` member names: `(MethodDef, function)` of the nearest class defining
/// the name, when that definition is a plain method with a body.
fn private_method(
    cx: &Ctx<'_, '_>,
    member: NodeId,
    name: ts_r_ast::Atom,
) -> Option<(NodeId, NodeId)> {
    let ast = cx.ast();
    let mut cur = cx.parent(member);
    for _ in 0..100_000 {
        let c = cur?;
        if let Node::ClassDecl { members, .. } | Node::ClassExpr { members, .. } = *ast.node(c) {
            let def = ast.list(members).iter().copied().find(|&m| {
                let key = match *ast.node(m) {
                    Node::MethodDef { key, .. } | Node::PropertyDef { key, .. } => key,
                    _ => return false,
                };
                matches!(*ast.node(key), Node::PrivateName { name: n } if n == name)
            });
            if let Some(def) = def {
                return match *ast.node(def) {
                    Node::MethodDef {
                        kind: MethodKind::Method,
                        value,
                        ..
                    } if fn_parts(ast, value).is_some_and(|f| f.body.is_some()) => {
                        Some((def, value))
                    }
                    _ => None,
                };
            }
        }
        cur = cx.parent(c);
    }
    None
}

/// Local functions whose every runtime reference is a direct call (the plugin's
/// `trackLocalFunctionCalls`): `None` when the file has dynamic scope (`with`, a direct
/// `eval`, a non-strict block-level function declaration).
pub(super) fn local_call_targets(cx: &Ctx<'_, '_>) -> Option<Vec<Target>> {
    let an = An { cx };
    if NoUnnecessaryParameters::file_is_dynamic(cx, &an) {
        return None;
    }
    Some(NoUnnecessaryParameters::targets(cx))
}

impl Rule for NoUnnecessaryParameters {
    fn has_run_file(&self) -> bool {
        true
    }

    fn run_file(&self, cx: &mut Ctx<'_, '_>) {
        let Some(targets) = local_call_targets(cx) else {
            return;
        };
        let needs_review =
            cx.env().typescript || is_typescript_path(cx.file.path) || has_js_type_annotations(cx);
        let mut reports: Vec<(Span, String, Option<Fix>, String)> = Vec::new();
        for t in &targets {
            self.check_target(cx, t, needs_review, &mut reports);
        }
        for (span, msg, fix, name) in reports {
            let r = cx.report(span, msg);
            match fix {
                Some(f) if needs_review => {
                    r.suggest(
                        "remove-parameter",
                        format!(
                            "Remove parameter `{name}` and its arguments (requires type review)."
                        ),
                        f,
                    );
                }
                f => {
                    r.fix_opt(f);
                }
            }
        }
    }
}

fn is_typescript_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".ts", ".mts", ".cts", ".tsx"]
        .iter()
        .any(|e| lower.ends_with(e))
}

/// JSDoc type tags or a `@ts-check` pragma anywhere in the file.
fn has_js_type_annotations(cx: &Ctx<'_, '_>) -> bool {
    const TAGS: &[&str] = &[
        "param",
        "arg",
        "argument",
        "type",
        "returns",
        "return",
        "template",
        "this",
        "overload",
        "satisfies",
        "constructor",
        "class",
        "implements",
        "extends",
        "augments",
    ];
    let src = cx.src();
    cx.ast().comments.iter().any(|c| {
        let text = c.span.text(src);
        let value = text
            .strip_prefix("//")
            .or_else(|| {
                text.strip_prefix("/*")
                    .map(|t| t.strip_suffix("*/").unwrap_or(t))
            })
            .unwrap_or(text);
        let pragma = {
            let v = value.trim_start();
            let v = v.strip_prefix(['*', '/']).map_or(v, str::trim_start);
            v.to_ascii_lowercase().starts_with("@ts-check")
        };
        let block_doc = matches!(c.kind, ts_r_ast::CommentKind::Block)
            && value.trim_start().starts_with('*')
            && value.match_indices('@').any(|(i, _)| {
                let rest = &value[i + 1..];
                TAGS.iter().any(|t| {
                    rest.starts_with(t)
                        && !rest[t.len()..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                })
            });
        pragma || block_doc
    })
}

impl NoUnnecessaryParameters {
    fn params(&self, cx: &Ctx<'_, '_>, func: NodeId) -> Vec<Param> {
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(parts) = fn_parts(ast, func) else {
            return Vec::new();
        };
        let mut predicate: Option<&str> = None;
        if let Some(rt) = parts.return_type
            && let Node::TypePredicate { param, .. } = *ast.node(rt)
        {
            predicate = match *ast.node(param) {
                Node::Ident { name } => Some(ast.atom(name)),
                _ => Some("this"),
            };
        }
        let mut out = Vec::new();
        let mut index = 0;
        for &p in parts.params {
            let Node::Param {
                modifiers,
                rest,
                pattern,
                init,
                ..
            } = *ast.node(p)
            else {
                continue;
            };
            if ident_name(ast, pattern) == Some("this") {
                continue;
            }
            let position = index;
            index += 1;
            if rest
                || modifiers.intersects(
                    Modifiers::PUBLIC
                        | Modifiers::PRIVATE
                        | Modifiers::PROTECTED
                        | Modifiers::READONLY
                        | Modifiers::OVERRIDE,
                )
            {
                continue;
            }
            match *ast.node(pattern) {
                Node::Ident { name } => {
                    if predicate == Some(ast.atom(name)) {
                        continue;
                    }
                    if let Some(sym) = sem.symbol_of_decl(pattern) {
                        out.push(Param {
                            node: p,
                            ident: pattern,
                            sym,
                            index: position,
                            default: init,
                            pattern_param: None,
                        });
                    }
                }
                Node::ObjectPattern { .. } => {
                    if let Some(d) = init
                        && !matches!(*ast.node(unwrap_ts(ast, d)), Node::ObjectLit { props } if ast.list(props).is_empty())
                    {
                        continue;
                    }
                    let Some(props) = properties(ast, pattern) else {
                        continue;
                    };
                    for (_, prop) in props {
                        let Node::BindingProperty { key, value, init } = *ast.node(prop) else {
                            continue;
                        };
                        let ident = value.unwrap_or(key);
                        if !matches!(ast.node(ident), Node::Ident { .. }) {
                            continue;
                        }
                        if let Some(sym) = sem.symbol_of_decl(ident) {
                            out.push(Param {
                                node: prop,
                                ident,
                                sym,
                                index: position,
                                default: init,
                                pattern_param: Some(p),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn arguments(&self, an: &An<'_, '_, '_>, t: &Target, p: &Param) -> Option<Vec<Arg>> {
        let cx = an.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let func_span = cx.span(t.func);
        let mut out = Vec::new();
        for &call in &t.calls {
            let args = match *ast.node(call) {
                Node::Call { args, .. } | Node::New { args, .. } => ast.list(args),
                _ => return None,
            };
            if args
                .iter()
                .take(p.index + 1)
                .any(|&a| matches!(ast.node(a), Node::Spread { .. }))
            {
                return None;
            }
            let arg = args.get(p.index).copied();
            if let Some(pp) = p.pattern_param {
                let name = match *ast.node(p.node) {
                    Node::BindingProperty { key, .. } => key_name(ast, key)?,
                    _ => return None,
                };
                let mut object = arg.map(|a| unwrap_ts(ast, a));
                let param_default = match *ast.node(pp) {
                    Node::Param { init, .. } => init,
                    _ => None,
                };
                if param_default.is_some()
                    && object.is_none_or(|o| an.value(Some(o)).is_some_and(|v| v.is_undefined()))
                {
                    object = param_default.map(|d| unwrap_ts(ast, d));
                }
                let object = object?;
                if !matches!(ast.node(object), Node::ObjectLit { .. }) {
                    return None;
                }
                let props = properties(ast, object)?;
                let prop = props.iter().find(|(n, _)| *n == name).map(|(_, p)| *p);
                if prop.is_none() && OBJECT_PROTOTYPE_KEYS.contains(&name.as_str()) {
                    return None;
                }
                let value_node = prop.map(|pr| match *ast.node(pr) {
                    Node::Property { value, .. } => value,
                    Node::ShorthandProp { name, .. } => name,
                    _ => pr,
                });
                out.push(Arg {
                    node: prop,
                    value: an.value(value_node),
                    forwarding: false,
                });
                continue;
            }
            if let Some(a) = arg {
                let u = unwrap_ts(ast, a);
                if matches!(ast.node(u), Node::Ident { .. })
                    && inside(func_span, cx.span(call))
                    && sem.reference_at(u).and_then(|r| sem.reference(r).symbol()) == Some(p.sym)
                {
                    if an.has_writes(p.sym) {
                        return None;
                    }
                    out.push(Arg {
                        node: Some(a),
                        value: None,
                        forwarding: true,
                    });
                    continue;
                }
            }
            out.push(Arg {
                node: arg,
                value: an.value(arg),
                forwarding: false,
            });
        }
        Some(out)
    }

    fn outcome(
        &self,
        an: &An<'_, '_, '_>,
        t: &Target,
        p: &Param,
        args: Vec<Arg>,
    ) -> Option<Outcome> {
        let incoming: Vec<&Arg> = args.iter().filter(|a| !a.forwarding).collect();
        if incoming.iter().any(|a| a.value.is_none()) {
            return None;
        }
        let default = match p.default {
            Some(d) => an.value(Some(d)),
            None => Some(Val::Prim(Prim::Undefined, None)),
        };
        let forwarding = args.iter().any(|a| a.forwarding);
        if incoming
            .iter()
            .all(|a| a.value.as_ref().is_some_and(Val::is_undefined))
        {
            if forwarding
                && match &default {
                    None => true,
                    Some(Val::Binding(b)) => !an.is_outer_binding(*b, t.func),
                    _ => false,
                }
            {
                return None;
            }
            return Some(Outcome {
                id: if p.default.is_some() {
                    "always-default"
                } else {
                    "always-undefined"
                },
                value: default,
                args,
            });
        }
        let values: Vec<Option<Val>> = incoming
            .iter()
            .map(|a| match &a.value {
                Some(v) if v.is_undefined() => default.clone(),
                v => v.clone(),
            })
            .collect();
        let first = values.first()?.clone()?;
        if values
            .iter()
            .any(|v| v.as_ref().is_none_or(|v| !first.same(v)))
        {
            return None;
        }
        if let Val::Binding(b) = first
            && !an.is_outer_binding(b, t.func)
        {
            return None;
        }
        let id = if p.default.is_some() && default.as_ref().is_some_and(|d| first.same(d)) {
            "always-default"
        } else {
            "same-value"
        };
        Some(Outcome {
            id,
            value: Some(first),
            args,
        })
    }

    fn check_target(
        &self,
        cx: &Ctx<'_, '_>,
        t: &Target,
        needs_review: bool,
        reports: &mut Vec<(Span, String, Option<Fix>, String)>,
    ) {
        let ast = cx.ast();
        let sem = cx.semantic();
        let an = An { cx };
        let Some(parts) = fn_parts(ast, t.func) else {
            return;
        };
        if parts.type_params.is_some() || parts.params.iter().any(|&p| has_decorators(ast, p)) {
            return;
        }
        if let Some(scope) = sem.scope_of_node(t.func)
            && sem.symbols().iter().any(|sym| {
                sym.scope == scope
                    && sym.flags.contains(SymbolFlags::ARGUMENTS)
                    && !sym.refs.is_empty()
            })
        {
            return;
        }
        let func_span = cx.span(t.func);
        let external = t
            .calls
            .iter()
            .filter(|&&c| !inside(func_span, cx.span(c)))
            .count();
        if external < self.minimum_call_count {
            return;
        }
        let mut has_fix = false;
        for p in self.params(cx, t.func) {
            let Some(args) = self.arguments(&an, t, &p) else {
                continue;
            };
            let Some(outcome) = self.outcome(&an, t, &p, args) else {
                continue;
            };
            let fix = if has_fix {
                None
            } else {
                self.fix(&an, t, &p, &outcome)
            };
            has_fix |= fix.is_some() && !needs_review;
            let name = cx.text(p.ident).to_owned();
            let msg = match outcome.id {
                "same-value" => {
                    format!("Parameter `{name}` receives the same value at every call.")
                }
                "always-default" => {
                    format!("Parameter `{name}` always receives its default value.")
                }
                _ => format!("Parameter `{name}` is always `undefined`."),
            };
            let mut span = cx.span(p.ident);
            if p.pattern_param.is_none()
                && let Node::Param {
                    type_ann: Some(ta), ..
                } = *ast.node(p.node)
            {
                span = Span::new(span.start, cx.span(ta).end);
            }
            reports.push((span, msg, fix, name));
        }
    }

    fn fix(&self, an: &An<'_, '_, '_>, t: &Target, p: &Param, o: &Outcome) -> Option<Fix> {
        let cx = an.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let src = cx.src();
        let parts = fn_parts(ast, t.func)?;
        let body = parts.body?;
        let sym = sem.symbol(p.sym);
        if sym.decls.len() != 1 {
            return None;
        }
        let refs: Vec<_> = sym.refs.iter().map(|&r| *sem.reference(r)).collect();
        if refs
            .iter()
            .any(|r| r.is_type() || r.flags.contains(RefFlags::TYPEOF_TYPE))
        {
            return None;
        }
        let body_span = cx.span(body);
        let param_span = cx.span(p.node);
        if refs.iter().any(|r| {
            let s = cx.span(r.node);
            !r.flags.contains(RefFlags::INIT) && !inside(body_span, s) && !inside(param_span, s)
        }) {
            return None;
        }
        let local_default = if o.id == "always-default" {
            self.local_default(an, t, p, o)
        } else {
            None
        };
        if local_default.is_none() && !self.can_inline(an, p, o) {
            return None;
        }
        let removals: Vec<NodeId> = o
            .args
            .iter()
            .filter_map(|a| a.node)
            .filter(|&n| !inside(param_span, cx.span(n)))
            .collect();
        let header = Span::new(func_span_start(cx, t.func), body_span.start);
        if !ast.comments_in(header).is_empty()
            || (local_default.is_some() && !ast.comments_in(body_span).is_empty())
            || t.calls
                .iter()
                .any(|&c| !ast.comments_in(cx.span(c)).is_empty())
        {
            return None;
        }
        if local_default.is_none()
            && refs.iter().any(|r| {
                let mut cur = r.node;
                while let Some(par) = cx.parent(cur)
                    && matches!(
                        ast.node(par),
                        Node::Paren { .. }
                            | Node::As { .. }
                            | Node::Satisfies { .. }
                            | Node::NonNull { .. }
                            | Node::TypeAssertion { .. }
                    )
                {
                    cur = par;
                }
                cx.parent(cur).is_some_and(|par| {
                    matches!(
                        *ast.node(par),
                        Node::Unary {
                            op: UnaryOp::Delete,
                            ..
                        }
                    )
                })
            })
        {
            return None;
        }
        let mut edits: Vec<(Span, String)> = Vec::new();
        if p.pattern_param.is_none() {
            edits.push(self.param_removal(cx, t.func, p.node)?);
            for &n in &removals {
                let call = cx.parent(n)?;
                let index = match *ast.node(call) {
                    Node::Call { args, .. } | Node::New { args, .. } => {
                        ast.list(args).iter().position(|&a| a == n)?
                    }
                    _ => return None,
                };
                let f = remove_argument(cx, call, index)?;
                edits.extend(f.edits.iter().map(|e| (e.span, e.text.clone())));
            }
        } else {
            edits.extend(remove_object_property(cx, p.node)?);
            for &n in &removals {
                edits.extend(remove_object_property(cx, n)?);
            }
        }
        if let Some(local) = local_default {
            let lb = linebreak(src);
            let indent = line_indent(src, cx.span(t.func).start as usize).to_owned();
            let body_indent = format!("{indent}{}", indent_unit(cx));
            if let Node::Block { stmts } = *ast.node(body) {
                let stmts = ast.list(stmts);
                let last_directive = stmts
                    .iter()
                    .take_while(|&&s| matches!(ast.node(s), Node::Directive { .. }))
                    .last()
                    .copied();
                let anchor_end = match last_directive {
                    Some(d) => cx.span(d).end,
                    None => body_span.start + 1,
                };
                let next = skip_trivia(ast, src, anchor_end as usize);
                let gap = &src[anchor_end as usize..next];
                let suffix = if gap.contains(['\n', '\r']) {
                    String::new()
                } else if src.as_bytes().get(next) == Some(&b'}') {
                    format!("{lb}{indent}")
                } else {
                    format!("{lb}{body_indent}")
                };
                edits.push((
                    Span::new(anchor_end, anchor_end),
                    format!("{lb}{body_indent}{local}{suffix}"),
                ));
            } else {
                let range = super::unicorn_text::paren_span(cx, body);
                let expr = range.text(src);
                edits.push((
                    range,
                    format!(
                        "{{{lb}{body_indent}{local}{lb}{body_indent}return {expr};{lb}{indent}}}"
                    ),
                ));
            }
        } else {
            let value = o.value.as_ref()?;
            for r in &refs {
                let s = cx.span(r.node);
                if r.flags.contains(RefFlags::INIT)
                    || inside(param_span, s)
                    || removals.iter().any(|&n| inside(cx.span(n), s))
                {
                    continue;
                }
                let text = self.replacement_text(an, r.node, value);
                let parent = cx.parent(r.node);
                let shorthand = parent.is_some_and(|par| {
                    matches!(*ast.node(par), Node::ShorthandProp { name, .. } if name == r.node)
                        || matches!(*ast.node(par), Node::BindingProperty { key, value: None, .. } if key == r.node)
                });
                let name = cx.text(r.node);
                edits.push((
                    s,
                    if shorthand {
                        format!("{name}: {text}")
                    } else {
                        text
                    },
                ));
            }
        }
        edits.sort_by_key(|(s, _)| (s.start, s.end));
        let mut it = edits.into_iter();
        let (s0, t0) = it.next()?;
        Some(with_edits(Fix::replace(s0, t0), it.collect()))
    }

    fn param_removal(
        &self,
        cx: &Ctx<'_, '_>,
        func: NodeId,
        param: NodeId,
    ) -> Option<(Span, String)> {
        let ast = cx.ast();
        let src = cx.src();
        let parts = fn_parts(ast, func)?;
        let span = cx.span(param);
        let after = skip_trivia(ast, src, span.end as usize);
        if parts.arrow && parts.params.len() == 1 && src[after..].starts_with("=>") {
            return Some((span, "()".into()));
        }
        let index = parts.params.iter().position(|&p| p == param)?;
        let (mut start, mut end) = (span.start, span.end);
        if index + 1 < parts.params.len() {
            if src.as_bytes().get(after) != Some(&b',') {
                return None;
            }
            end = skip_spaces_only(src, after + 1) as u32;
        } else if index > 0 {
            let before = skip_trivia_back(ast, src, span.start as usize);
            start = before.checked_sub(1)? as u32;
        } else if src.as_bytes().get(after) == Some(&b',') {
            end = after as u32 + 1;
        }
        Some((Span::new(start, end), String::new()))
    }

    fn local_default(
        &self,
        an: &An<'_, '_, '_>,
        t: &Target,
        p: &Param,
        o: &Outcome,
    ) -> Option<String> {
        let cx = an.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let parts = fn_parts(ast, t.func)?;
        let body = parts.body?;
        let default = unwrap_ts(ast, p.default?);
        let name = cx.text(p.ident);
        if !matches!(ast.node(default), Node::Ident { .. })
            || p.pattern_param.is_some()
            || RESERVED.contains(&name)
            || parts.flags.intersects(FnFlags::ASYNC | FnFlags::GENERATOR)
            || parts.params.last() != Some(&p.node)
            || o.args.iter().any(|a| a.forwarding)
            || (!matches!(ast.node(body), Node::Block { .. })
                && o.args
                    .iter()
                    .any(|a| a.node.is_some_and(|n| inside(cx.span(body), cx.span(n)))))
            || sem.symbol(p.sym).decls.len() != 1
        {
            return None;
        }
        let var = sem
            .reference_at(default)
            .and_then(|r| sem.reference(r).symbol())?;
        let vs = sem.symbol(var);
        let scope = sem.scope_of_node(t.func)?;
        if var == p.sym
            || vs.decls.len() != 1
            || vs.decls[0].kind != DeclKind::Param
            || !parts.params.contains(&vs.decls[0].node)
            || sem.lookup(scope, sem.name(var)) != Some(var)
        {
            return None;
        }
        let kw = if an.has_writes(p.sym) { "let" } else { "const" };
        Some(format!("{kw} {name} = {};", cx.text(p.default?)))
    }

    fn can_inline(&self, an: &An<'_, '_, '_>, p: &Param, o: &Outcome) -> bool {
        let cx = an.cx;
        let ast = cx.ast();
        let sem = cx.semantic();
        let Some(value) = &o.value else {
            return false;
        };
        if an.has_writes(p.sym) {
            return false;
        }
        let refs: Vec<_> = sem
            .symbol(p.sym)
            .refs
            .iter()
            .map(|&r| *sem.reference(r))
            .collect();
        if refs.iter().any(|r| {
            r.flags.contains(RefFlags::JSX)
                || (cx.text(r.node) == "__proto__"
                    && cx.parent(r.node).is_some_and(|par| {
                        matches!(*ast.node(par), Node::ShorthandProp { name, .. } if name == r.node)
                    }))
        }) {
            return false;
        }
        let b = match value {
            Val::Prim(_, node) => {
                return node.is_none_or(|n| an.is_strict(n))
                    || refs.iter().all(|r| !an.is_strict(r.node));
            }
            Val::Binding(b) => *b,
        };
        let Bind::Sym(vsym) = b else {
            return false;
        };
        let vs = sem.symbol(vsym);
        if sem.scope(vs.scope).kind == ScopeKind::Global
            || RESERVED.contains(&sem.name(vsym))
            || o.id == "always-default"
        {
            return false;
        }
        let Some(d) = vs.decls.first().copied() else {
            return false;
        };
        if matches!(d.kind, DeclKind::Param | DeclKind::CatchParam) {
            return false;
        }
        let variable = matches!(d.kind, DeclKind::Let | DeclKind::Const | DeclKind::Using);
        if variable && !matches!(*ast.node(d.node), Node::VarDeclarator { init: Some(_), .. }) {
            return false;
        }
        if variable || d.kind == DeclKind::Class {
            let var_scope = |mut s: ScopeId| {
                for _ in 0..10_000 {
                    if sem.scope(s).kind.is_var_scope() {
                        return s;
                    }
                    match sem.scope(s).parent {
                        Some(p) => s = p,
                        None => break,
                    }
                }
                s
            };
            let def_end = cx.span(d.node).end;
            if sem.scope(vs.scope).kind == ScopeKind::Switch
                || o.args.iter().any(|a| {
                    a.node.is_some_and(|n| {
                        var_scope(sem.enclosing_scope(n)) != var_scope(vs.scope)
                            || def_end > cx.span(n).start
                    })
                })
            {
                return false;
            }
        }
        let name = sem.name(vsym);
        refs.iter().all(|r| {
            let found = sem.lookup(r.scope, name);
            found == Some(vsym) || found == Some(p.sym)
        })
    }

    fn replacement_text(&self, an: &An<'_, '_, '_>, ident: NodeId, value: &Val) -> String {
        let cx = an.cx;
        let ast = cx.ast();
        let mut text = match value {
            Val::Prim(Prim::Undefined, _) => "(void 0)".to_owned(),
            Val::Binding(b) => an.bind_name(*b),
            Val::Prim(_, Some(n)) => cx.text(*n).to_owned(),
            Val::Prim(_, None) => return "(void 0)".into(),
        };
        let mut outer = ident;
        while let Some(par) = cx.parent(outer)
            && matches!(
                ast.node(par),
                Node::As { .. }
                    | Node::Satisfies { .. }
                    | Node::NonNull { .. }
                    | Node::TypeAssertion { .. }
            )
        {
            outer = par;
        }
        if let Val::Prim(prim, Some(n)) = value {
            let parent = cx.parent(outer);
            let wrap = (matches!(ast.node(*n), Node::Unary { .. }) && !value.is_undefined())
                || parent.is_some_and(|par| match *ast.node(par) {
                    Node::Member { obj, .. } | Node::Index { obj, .. } => {
                        obj == outer && member_object_needs_parens(cx, *n)
                    }
                    Node::New { callee, .. } => callee == outer,
                    Node::ExprStmt { .. } => matches!(prim, Prim::Str(_)),
                    _ => false,
                });
            if wrap {
                text = format!("({text})");
            }
        }
        // At the start of a statement, a `(` or `[` may join the previous line.
        let mut stmt = None;
        let mut cur = ident;
        for _ in 0..100_000 {
            let Some(par) = cx.parent(cur) else { break };
            if matches!(ast.node(par), Node::ExprStmt { .. }) {
                stmt = Some(par);
            }
            cur = par;
        }
        if let Some(s) = stmt
            && cx.span(s).start == cx.span(ident).start
            && needs_semicolon(cx, cx.span(ident).start, &text)
        {
            text = format!(";{text}");
        }
        text
    }
}

fn member_object_needs_parens(cx: &Ctx<'_, '_>, n: NodeId) -> bool {
    let ast = cx.ast();
    match *ast.node(n) {
        Node::NumberLit { .. } => {
            let t = cx.text(n);
            t.bytes().all(|b| b.is_ascii_digit() || b == b'_')
        }
        _ => false,
    }
}

fn func_span_start(cx: &Ctx<'_, '_>, func: NodeId) -> u32 {
    cx.span(func).start
}

fn skip_trivia(ast: &Ast, src: &str, mut pos: usize) -> usize {
    let bytes = src.as_bytes();
    for _ in 0..1_000_000 {
        while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() {
            pos += 1;
        }
        match ast.comments.iter().find(|c| c.span.start as usize == pos) {
            Some(c) => pos = c.span.end as usize,
            None => break,
        }
    }
    pos
}

fn skip_trivia_back(ast: &Ast, src: &str, mut pos: usize) -> usize {
    let bytes = src.as_bytes();
    for _ in 0..1_000_000 {
        while pos > 0 && (bytes[pos - 1] as char).is_ascii_whitespace() {
            pos -= 1;
        }
        match ast.comments.iter().find(|c| c.span.end as usize == pos) {
            Some(c) => pos = c.span.start as usize,
            None => break,
        }
    }
    pos
}

fn skip_spaces_only(src: &str, mut pos: usize) -> usize {
    let bytes = src.as_bytes();
    while pos < bytes.len() && (bytes[pos] as char).is_ascii_whitespace() {
        pos += 1;
    }
    pos
}

/// Removes every token of a property (the whitespace between them stays) and its comma:
/// the one after it, or the one before it when it is the last of several.
fn remove_object_property(cx: &Ctx<'_, '_>, prop: NodeId) -> Option<Vec<(Span, String)>> {
    let ast = cx.ast();
    let src = cx.src();
    let span = cx.span(prop);
    let text = span.text(src);
    let mut kept = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in text.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            c if c.is_whitespace() => kept.push(c),
            _ => {}
        }
    }
    let mut edits = vec![(span, kept)];
    let after = skip_trivia(ast, src, span.end as usize);
    if src.as_bytes().get(after) == Some(&b',') {
        edits.push((Span::new(after as u32, after as u32 + 1), String::new()));
    } else {
        let parent = cx.parent(prop)?;
        let list = match *ast.node(parent) {
            Node::ObjectLit { props } | Node::ObjectPattern { props } => ast.list(props),
            _ => return None,
        };
        if list.len() > 1 && list.last() == Some(&prop) {
            let before = skip_trivia_back(ast, src, span.start as usize);
            let at = before.checked_sub(1)?;
            edits.push((Span::new(at as u32, at as u32 + 1), String::new()));
        }
    }
    Some(edits)
}
