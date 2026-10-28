//! Side-effect analysis.
//!
//! "Pure" means: evaluating the expression (or running the statement) can be skipped when
//! its value is unused without changing observable behaviour, assuming standard globals are
//! not monkey-patched and getters on plain objects don't exist. The rules follow the
//! documented behaviour of esbuild and Rollup:
//!
//! - literals, functions, arrows, and identifiers bound in the file are pure; reading an
//!   unbound global is pure only for well-known globals (others may throw ReferenceError);
//! - calls and `new` are impure unless annotated `/* @__PURE__ */` / `/* #__PURE__ */` (and
//!   their arguments are pure), are known constructors like `new Map()`, or are known
//!   built-ins whose arguments cannot run code when converted (`Number(1)`, not
//!   `Number(obj)`, which calls `obj.valueOf`);
//! - classes are pure when their `extends`, computed keys, decorators, static fields and
//!   static blocks are;
//! - property reads are impure (getters), except reads of well-known global namespaces
//!   (`Math.PI`, `Object.keys`, `Symbol.iterator`).

use ts_r_ast::{Ast, BinaryOp, Modifiers, Node, NodeId, UnaryOp, VarKind};

use crate::fold::Const;
use crate::scope::{Scopes, SymbolKind};

/// Context for [`expr_is_pure`] and friends.
#[derive(Clone, Copy)]
pub struct Purity<'a> {
    pub ast: &'a Ast,
    pub src: &'a str,
    /// Resolves identifiers; without it every identifier is treated as a possibly-unbound
    /// global (only well-known ones are pure).
    pub scopes: Option<&'a Scopes>,
    /// Honour `/* @__PURE__ */` annotations (esbuild's `--ignore-annotations` turns this off).
    pub annotations: bool,
    /// Global functions whose calls are pure like annotated ones (esbuild's `--pure:name`,
    /// `a.b` for a member of a global; honoured only with `annotations`).
    pub pure_calls: &'a [String],
}

impl std::fmt::Debug for Purity<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Purity")
            .field("annotations", &self.annotations)
            .finish()
    }
}

const MAX_DEPTH: u32 = 200;

impl Purity<'_> {
    /// True when the expression has no side effects.
    pub fn expr(&self, id: NodeId) -> bool {
        self.expr_d(id, 0)
    }

    /// True when the statement has no side effects (declarations with pure initializers,
    /// functions, pure classes, pure expression statements, empty statements).
    pub fn stmt(&self, id: NodeId) -> bool {
        let ast = self.ast;
        match *ast.node(id) {
            Node::Empty {} | Node::Directive { .. } | Node::Debugger {} => {
                matches!(ast.node(id), Node::Empty {} | Node::Directive { .. })
            }
            Node::FunctionDecl { .. } => true,
            Node::ClassDecl {
                decorators,
                heritage,
                members,
                modifiers,
                ..
            } => {
                modifiers.contains(Modifiers::DECLARE)
                    || self.class(decorators, heritage, members, 0)
            }
            Node::VarDecl {
                kind,
                decls,
                modifiers,
            } => {
                if modifiers.contains(Modifiers::DECLARE) {
                    return true;
                }
                if matches!(kind, VarKind::Using | VarKind::AwaitUsing) {
                    return false;
                }
                ast.list(decls).iter().all(|&d| match *ast.node(d) {
                    Node::VarDeclarator { name, init, .. } => {
                        matches!(ast.node(name), Node::Ident { .. })
                            && init.is_none_or(|i| self.expr(i))
                    }
                    _ => false,
                })
            }
            Node::ExprStmt { expr } | Node::ExportDefaultExpr { expr } => self.expr(expr),
            Node::Block { stmts } => ast.list(stmts).iter().all(|&s| self.stmt(s)),
            Node::ExportNamed { .. }
            | Node::Interface { .. }
            | Node::TypeAlias { .. }
            | Node::ImportDecl { .. } => true,
            Node::If { test, cons, alt } => {
                self.expr(test) && self.stmt(cons) && alt.is_none_or(|a| self.stmt(a))
            }
            _ => false,
        }
    }

    fn expr_d(&self, id: NodeId, d: u32) -> bool {
        if d > MAX_DEPTH {
            return false;
        }
        let d = d + 1;
        let ast = self.ast;
        match *ast.node(id) {
            Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. }
            | Node::This {}
            | Node::FunctionExpr { .. }
            | Node::Arrow { .. }
            | Node::MetaProperty { .. }
            | Node::Missing {} => true,
            // A regex literal creates a fresh object; pure.
            Node::RegexLit { .. } => true,
            Node::Ident { name } => {
                let bound = self.scopes.is_some_and(|s| s.symbol_of(id).is_some());
                bound || is_known_global(ast.atom(name))
            }
            Node::TemplateLit { parts } => ast.list(parts).iter().all(|&p| {
                matches!(ast.node(p), Node::TemplateElement { .. }) || self.primitive_pure(p, d)
            }),
            Node::ArrayLit { elements } => ast.list(elements).iter().all(|&e| match *ast.node(e) {
                Node::Hole {} => true,
                Node::Spread { arg } => {
                    matches!(ast.node(arg), Node::ArrayLit { .. }) && self.expr_d(arg, d)
                }
                _ => self.expr_d(e, d),
            }),
            Node::ObjectLit { props } => ast.list(props).iter().all(|&p| match *ast.node(p) {
                Node::Property { key, value, .. } => self.key(key, d) && self.expr_d(value, d),
                Node::ShorthandProp { name, init, .. } => init.is_none() && self.expr_d(name, d),
                Node::MethodDef { key, .. } => self.key(key, d),
                Node::Spread { arg } => {
                    matches!(ast.node(arg), Node::ObjectLit { .. }) && self.expr_d(arg, d)
                }
                _ => false,
            }),
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. } => self.expr_d(expr, d),
            Node::Seq { exprs } => ast.list(exprs).iter().all(|&e| self.expr_d(e, d)),
            Node::Unary { op, arg } => match op {
                UnaryOp::Delete => false,
                UnaryOp::TypeOf => {
                    matches!(ast.node(arg), Node::Ident { .. }) || self.expr_d(arg, d)
                }
                // `-x`/`+x`/`~x` call valueOf on objects; only primitives are pure.
                UnaryOp::Minus | UnaryOp::Plus | UnaryOp::BitNot => self.primitive_pure(arg, d),
                UnaryOp::Not | UnaryOp::Void => self.expr_d(arg, d),
            },
            Node::Binary { left, op, right } => match op {
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce => {
                    self.expr_d(left, d) && self.expr_d(right, d)
                }
                BinaryOp::StrictEq | BinaryOp::StrictNotEq => {
                    self.expr_d(left, d) && self.expr_d(right, d)
                }
                BinaryOp::In | BinaryOp::InstanceOf => false,
                // Other operators may call valueOf/toString on objects.
                _ => self.primitive_pure(left, d) && self.primitive_pure(right, d),
            },
            Node::Conditional { test, cons, alt } => {
                self.expr_d(test, d) && self.expr_d(cons, d) && self.expr_d(alt, d)
            }
            Node::ClassExpr {
                decorators,
                heritage,
                members,
                ..
            } => self.class(decorators, heritage, members, d),
            Node::Call { callee, args, .. } => {
                let annotated = self.annotations
                    && (has_pure_comment(ast, self.src, id)
                        || self.no_side_effects_callee(callee)
                        || self.pure_callee(callee));
                (annotated || self.known_pure_call(callee, args))
                    && ast.list(args).iter().all(|&a| self.arg(a, d))
            }
            Node::New { callee, args, .. } => {
                let annotated = self.annotations && has_pure_comment(ast, self.src, id);
                let known = ast.list(args).is_empty()
                    && matches!(*ast.node(callee), Node::Ident { name }
                        if self.is_global(callee) && matches!(ast.atom(name),
                            "Map" | "Set" | "WeakMap" | "WeakSet" | "Object" | "Array" | "Error"
                            | "Date" | "AbortController" | "WeakRef" | "TextEncoder" | "TextDecoder"));
                (annotated || known) && ast.list(args).iter().all(|&a| self.arg(a, d))
            }
            Node::Member { obj, prop, .. } => self.known_global_member(obj, prop),
            _ => false,
        }
    }

    /// Pure and certainly a primitive (so operators can't call user code).
    fn primitive_pure(&self, id: NodeId, d: u32) -> bool {
        let ast = self.ast;
        match *ast.node(id) {
            Node::NullLit {}
            | Node::BoolLit { .. }
            | Node::NumberLit { .. }
            | Node::BigIntLit { .. }
            | Node::StringLit { .. } => true,
            Node::Paren { expr } => self.primitive_pure(expr, d),
            Node::Unary { op, arg } => match op {
                UnaryOp::Void | UnaryOp::Not | UnaryOp::TypeOf => self.expr_d(id, d),
                UnaryOp::Minus | UnaryOp::Plus | UnaryOp::BitNot => self.primitive_pure(arg, d),
                UnaryOp::Delete => false,
            },
            Node::TemplateLit { parts } => ast.list(parts).iter().all(|&p| {
                matches!(ast.node(p), Node::TemplateElement { .. }) || self.primitive_pure(p, d)
            }),
            Node::Binary { left, op, right } => match op {
                BinaryOp::In | BinaryOp::InstanceOf => false,
                BinaryOp::StrictEq
                | BinaryOp::StrictNotEq
                | BinaryOp::LogicalAnd
                | BinaryOp::LogicalOr
                | BinaryOp::Coalesce => {
                    self.primitive_pure(left, d) && self.primitive_pure(right, d)
                }
                _ => self.primitive_pure(left, d) && self.primitive_pure(right, d),
            },
            Node::Ident { name } => {
                // `undefined`, `NaN` and `Infinity` are primitive globals.
                matches!(ast.atom(name), "undefined" | "NaN" | "Infinity") && self.is_global(id)
            }
            _ => false,
        }
    }

    fn arg(&self, a: NodeId, d: u32) -> bool {
        match *self.ast.node(a) {
            Node::Spread { arg } => {
                matches!(self.ast.node(arg), Node::ArrayLit { .. }) && self.expr_d(arg, d)
            }
            _ => self.expr_d(a, d),
        }
    }

    fn key(&self, key: NodeId, d: u32) -> bool {
        match *self.ast.node(key) {
            Node::ComputedKey { expr } => self.primitive_pure(expr, d),
            _ => true,
        }
    }

    fn class(
        &self,
        decorators: ts_r_ast::NodeList,
        heritage: ts_r_ast::NodeList,
        members: ts_r_ast::NodeList,
        d: u32,
    ) -> bool {
        let ast = self.ast;
        if !decorators.is_empty() {
            return false;
        }
        for &h in ast.list(heritage) {
            if let Node::HeritageClause {
                kind: ts_r_ast::HeritageKind::Extends,
                types,
            } = *ast.node(h)
            {
                for &t in ast.list(types) {
                    let e = match *ast.node(t) {
                        Node::ExprWithTypeArgs { expr, .. } => expr,
                        _ => t,
                    };
                    if !self.expr_d(e, d) {
                        return false;
                    }
                }
            }
        }
        ast.list(members).iter().all(|&m| match *ast.node(m) {
            Node::MethodDef {
                decorators, key, ..
            } => decorators.is_empty() && self.key(key, d),
            Node::PropertyDef {
                decorators,
                modifiers,
                key,
                value,
                ..
            } => {
                decorators.is_empty()
                    && self.key(key, d)
                    && (!modifiers.contains(Modifiers::STATIC)
                        || value.is_none_or(|v| self.expr_d(v, d)))
            }
            Node::StaticBlock { body } => ast.list(body).iter().all(|&s| self.stmt(s)),
            _ => true,
        })
    }

    /// `callee` is a global name or member chain listed in [`Purity::pure_calls`].
    fn pure_callee(&self, callee: NodeId) -> bool {
        if self.pure_calls.is_empty() {
            return false;
        }
        let ast = self.ast;
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = callee;
        // Member chains are bounded by the parser's nesting limit; this caps them anyway.
        for _ in 0..64 {
            match *ast.node(cur) {
                Node::Ident { name } => {
                    if !self.is_global(cur) {
                        return false;
                    }
                    parts.push(ast.atom(name));
                    parts.reverse();
                    let dotted = parts.join(".");
                    return self.pure_calls.contains(&dotted);
                }
                Node::Member { obj, prop, .. } => match *ast.node(prop) {
                    Node::Ident { name } => {
                        parts.push(ast.atom(name));
                        cur = obj;
                    }
                    _ => return false,
                },
                Node::Paren { expr } => cur = expr,
                _ => return false,
            }
        }
        false
    }

    fn is_global(&self, id: NodeId) -> bool {
        self.scopes.is_none_or(|s| s.symbol_of(id).is_none())
    }

    /// A call of a local function annotated `/* @__NO_SIDE_EFFECTS__ */` (a function
    /// declaration, or a never-reassigned `const` whose initializer is a function or arrow,
    /// with the comment before the statement or before the initializer), which esbuild
    /// treats like a `/* @__PURE__ */` call.
    pub fn no_side_effects_callee(&self, callee: NodeId) -> bool {
        let (ast, src) = (self.ast, self.src);
        let Some(scopes) = self.scopes else {
            return false;
        };
        if ast.comments.is_empty() || !src.contains("__NO_SIDE_EFFECTS__") {
            return false;
        }
        let Node::Ident { .. } = *ast.node(callee) else {
            return false;
        };
        let Some(sym) = scopes.symbol_of(callee) else {
            return false;
        };
        let sym = scopes.symbol(sym);
        if sym.writes > 0
            || sym.decls != 1
            || !matches!(sym.kind, SymbolKind::Function | SymbolKind::Const)
        {
            return false;
        }
        let Some(decl) = sym.decl else {
            return false;
        };
        let span = ast.span(decl);
        let comment_text = |c: &ts_r_ast::Comment| {
            src.get(c.span.start as usize..c.span.end as usize)
                .unwrap_or("")
        };
        // Before the statement: `/* @__NO_SIDE_EFFECTS__ */ export default async function* f`.
        let idx = ast.comments.partition_point(|c| c.span.end <= span.start);
        if let Some(c) = idx.checked_sub(1).map(|i| &ast.comments[i])
            && is_no_side_effects(comment_text(c))
        {
            let between = src
                .get(c.span.end as usize..span.start as usize)
                .unwrap_or("x");
            let keywords_only = between
                .split(|ch: char| ch.is_ascii_whitespace() || ch == '*')
                .all(|w| {
                    matches!(
                        w,
                        "" | "export" | "default" | "async" | "function" | "const"
                    )
                });
            let fn_init = || {
                let rest = src.get(span.end as usize..).unwrap_or("").trim_start();
                rest.strip_prefix('=').is_some_and(|r| {
                    let r = r.trim_start();
                    r.starts_with("function") || r.starts_with("async") || r.starts_with('(')
                })
            };
            if keywords_only && (sym.kind == SymbolKind::Function || fn_init()) {
                return true;
            }
        }
        // Before the initializer: `const f = /* @__NO_SIDE_EFFECTS__ */ function () {}`.
        if sym.kind == SymbolKind::Const
            && let Some(c) = ast.comments.get(idx)
            && is_no_side_effects(comment_text(c))
            && src
                .get(span.end as usize..c.span.start as usize)
                .is_some_and(|b| b.trim() == "=")
        {
            let after = src.get(c.span.end as usize..).unwrap_or("").trim_start();
            return after.starts_with("function")
                || after.starts_with("async")
                || after.starts_with('(');
        }
        false
    }

    fn known_pure_call(&self, callee: NodeId, args: ts_r_ast::NodeList) -> bool {
        let ast = self.ast;
        let args = ast.list(args);
        // Converting an object to a primitive runs its `valueOf` / `toString` /
        // `Symbol.toPrimitive`, and converting a symbol throws, so conversions are only
        // known pure on constant primitives.
        let constants = || args.iter().all(|&a| self.constant(a).is_some());
        match *ast.node(callee) {
            Node::Ident { name } if self.is_global(callee) => match ast.atom(name) {
                // ToBoolean never runs user code.
                "Boolean" => args.len() <= 1,
                "Symbol" | "String" | "Number" => args.len() <= 1 && constants(),
                // `BigInt()` and non-integral numbers throw.
                "BigInt" => match args {
                    [a] => match self.constant(*a) {
                        Some(Const::Num(n)) => n.is_finite() && n.fract() == 0.0,
                        Some(Const::Bool(_)) => true,
                        _ => matches!(ast.node(*a), Node::BigIntLit { .. }),
                    },
                    _ => false,
                },
                _ => false,
            },
            Node::Member { obj, prop, .. } => {
                if let (Node::Ident { name: o }, Node::Ident { name: p }) =
                    (*ast.node(obj), *ast.node(prop))
                {
                    self.is_global(obj)
                        && match (ast.atom(o), ast.atom(p)) {
                            ("Symbol", "for")
                            | ("Math", "max" | "min" | "abs" | "floor" | "ceil" | "round") => {
                                constants()
                            }
                            ("Object", "freeze") => {
                                args.iter().all(|&a| self.primitive_or_fresh(a))
                            }
                            // A prototype that is not an object or `null` throws, and so can
                            // reading property descriptors.
                            ("Object", "create") => matches!(
                                args,
                                [a] if matches!(
                                    self.ast.node(*a),
                                    Node::NullLit {} | Node::ObjectLit { .. } | Node::ArrayLit { .. }
                                )
                            ),
                            _ => false,
                        }
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    fn constant(&self, a: NodeId) -> Option<Const> {
        crate::fold::eval(self.ast, a, self.scopes)
    }

    fn primitive_or_fresh(&self, a: NodeId) -> bool {
        matches!(
            self.ast.node(a),
            Node::StringLit { .. }
                | Node::NumberLit { .. }
                | Node::NullLit {}
                | Node::ObjectLit { .. }
                | Node::ArrayLit { .. }
        ) && self.expr(a)
    }

    fn known_global_member(&self, obj: NodeId, prop: NodeId) -> bool {
        let ast = self.ast;
        let (Node::Ident { name: o }, Node::Ident { name: p }) = (*ast.node(obj), *ast.node(prop))
        else {
            return false;
        };
        if !self.is_global(obj) {
            return false;
        }
        let o = ast.atom(o);
        let p = ast.atom(p);
        match o {
            "Math" | "Object" | "Symbol" | "Reflect" | "JSON" | "Number" | "String" | "Array"
            | "Promise" | "Atomics" | "Intl" => {
                // Static properties and methods of built-in namespaces/constructors.
                !p.is_empty() && p != "prototype"
            }
            _ => false,
        }
    }
}

/// Globals whose read never throws in browsers and Node.
pub fn is_known_global(name: &str) -> bool {
    matches!(
        name,
        "undefined"
            | "NaN"
            | "Infinity"
            | "globalThis"
            | "Object"
            | "Function"
            | "Array"
            | "String"
            | "Number"
            | "Boolean"
            | "Symbol"
            | "BigInt"
            | "Math"
            | "JSON"
            | "Date"
            | "RegExp"
            | "Error"
            | "TypeError"
            | "RangeError"
            | "SyntaxError"
            | "ReferenceError"
            | "EvalError"
            | "URIError"
            | "AggregateError"
            | "Map"
            | "Set"
            | "WeakMap"
            | "WeakSet"
            | "WeakRef"
            | "FinalizationRegistry"
            | "Promise"
            | "Proxy"
            | "Reflect"
            | "Intl"
            | "ArrayBuffer"
            | "SharedArrayBuffer"
            | "DataView"
            | "Int8Array"
            | "Uint8Array"
            | "Uint8ClampedArray"
            | "Int16Array"
            | "Uint16Array"
            | "Int32Array"
            | "Uint32Array"
            | "Float32Array"
            | "Float64Array"
            | "BigInt64Array"
            | "BigUint64Array"
            | "Atomics"
            | "parseInt"
            | "parseFloat"
            | "isNaN"
            | "isFinite"
            | "encodeURI"
            | "encodeURIComponent"
            | "decodeURI"
            | "decodeURIComponent"
            | "escape"
            | "unescape"
            | "console"
            | "queueMicrotask"
            | "setTimeout"
            | "clearTimeout"
            | "setInterval"
            | "clearInterval"
    )
}

/// True when a `/* @__PURE__ */` or `/* #__PURE__ */` comment sits directly before the node
/// (only whitespace and opening parentheses in between).
pub fn has_pure_comment(ast: &Ast, src: &str, id: NodeId) -> bool {
    let span = ast.span(id);
    if span.start == span.end {
        return false;
    }
    let start = span.start;
    let idx = ast.comments.partition_point(|c| c.span.end <= start);
    if idx == 0 {
        return false;
    }
    // Look at the last few comments ending before the node.
    for c in ast.comments[..idx].iter().rev().take(3) {
        let between = src.get(c.span.end as usize..start as usize).unwrap_or("x");
        if !between
            .bytes()
            .all(|b| b.is_ascii_whitespace() || b == b'(')
        {
            return false;
        }
        let text = src
            .get(c.span.start as usize..c.span.end as usize)
            .unwrap_or("");
        if is_pure_annotation(text) {
            return true;
        }
    }
    false
}

/// `/* @__NO_SIDE_EFFECTS__ */` or `/* #__NO_SIDE_EFFECTS__ */` (also as a line comment).
fn is_no_side_effects(text: &str) -> bool {
    let inner = text
        .strip_prefix("/*")
        .and_then(|t| t.strip_suffix("*/"))
        .or_else(|| text.strip_prefix("//"))
        .unwrap_or("")
        .trim();
    inner == "@__NO_SIDE_EFFECTS__" || inner == "#__NO_SIDE_EFFECTS__"
}

/// `/* @__PURE__ */` or `/* #__PURE__ */` (also as a line comment).
pub fn is_pure_annotation(text: &str) -> bool {
    let inner = text
        .strip_prefix("/*")
        .and_then(|t| t.strip_suffix("*/"))
        .or_else(|| text.strip_prefix("//"))
        .unwrap_or("")
        .trim();
    inner == "@__PURE__" || inner == "#__PURE__"
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_r_ast::SourceKind;

    fn pure_stmts(src: &str) -> Vec<bool> {
        let ast = ts_r_parser::parse(src, SourceKind::Js).ast;
        let scopes = Scopes::analyze(&ast, true);
        let p = Purity {
            ast: &ast,
            src,
            scopes: Some(&scopes),
            annotations: true,
            pure_calls: &[],
        };
        crate::build::program_stmts(&ast)
            .into_iter()
            .map(|s| p.stmt(s))
            .collect()
    }

    #[test]
    fn statements() {
        assert_eq!(
            pure_stmts(
                "const a = 1, b = 'x' + 2; function f() { g(); } let c = f(); let d = /*#__PURE__*/ f(); \
                 class C { static x = 1; m() {} } class D { static y = g(); } let e = unknownGlobal; let o = { a, [1]: 2 };"
            ),
            [true, true, false, true, true, false, false, true]
        );
    }

    #[test]
    fn annotations_need_adjacency() {
        let src =
            "let a = /* @__PURE__ */ (f()); let b = /* #__PURE__ */ x + f(); let c = new Map();";
        assert_eq!(pure_stmts(src), [true, false, true]);
    }

    #[test]
    fn conversions_of_objects_are_not_pure() {
        let src = "Number({ valueOf() { log(); return 1; } }); String({}); Symbol([]); \
                   Math.max({ valueOf() { log(); } }); Symbol.for({}); BigInt(); BigInt(1.5); \
                   BigInt('x'); Object.create(1); Object.create(null, { p: 1 }); Number(Symbol());";
        assert!(pure_stmts(src).iter().all(|&p| !p), "{:?}", pure_stmts(src));
        let src = "Number('1'); String(1); Symbol(`a`); Boolean({ valueOf() { log(); } }); \
                   Math.max(1, '2', null); Symbol.for('k'); BigInt(2); BigInt(10n); \
                   Object.create(null); Object.create({}); Object.freeze([]); Number(); String();";
        assert!(pure_stmts(src).iter().all(|&p| p), "{:?}", pure_stmts(src));
    }
}
