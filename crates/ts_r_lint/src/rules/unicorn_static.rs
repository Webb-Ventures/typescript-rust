//! Static evaluation for the eslint-plugin-unicorn rules: the value an expression is known
//! to have without running the program (ESLint's `getStaticValue` from
//! `@eslint-community/eslint-utils`), the plugin's stricter variants that refuse values
//! whose evaluation could have side effects or depend on mutable state, and the side-effect
//! test (`hasSideEffect`) they use.
//!
//! What a value can be: the primitives, regular expressions, arrays, plain objects and a
//! handful of built-in globals (`Math`, `String`, `Object.freeze`, ...) that only matter as
//! callees or receivers. Calls are evaluated for a fixed list of pure built-ins on static
//! arguments; anything else is "not static".

#![allow(dead_code)] // A shared toolkit: each rule uses part of it.

use super::prelude::*;
use ts_r_ast::Ast;

/// A statically known JavaScript value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Static {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    BigInt(i128),
    RegExp {
        source: String,
        flags: String,
    },
    Array(Vec<Static>),
    Object(Vec<(String, Static)>),
    /// A built-in global or one of its functions, by dotted path (`Math`, `Math.max`,
    /// `String.prototype.trim`).
    Builtin(String),
}

/// Globals ESLint's evaluator knows (its `builtinNames`).
const BUILTIN_NAMES: &[&str] = &[
    "Array",
    "ArrayBuffer",
    "BigInt",
    "BigInt64Array",
    "BigUint64Array",
    "Boolean",
    "DataView",
    "Date",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "escape",
    "Float32Array",
    "Float64Array",
    "Function",
    "Infinity",
    "Int16Array",
    "Int32Array",
    "Int8Array",
    "isFinite",
    "isNaN",
    "isPrototypeOf",
    "JSON",
    "Map",
    "Math",
    "NaN",
    "Number",
    "Object",
    "parseFloat",
    "parseInt",
    "Promise",
    "Proxy",
    "Reflect",
    "RegExp",
    "Set",
    "String",
    "Symbol",
    "Uint16Array",
    "Uint32Array",
    "Uint8Array",
    "Uint8ClampedArray",
    "undefined",
    "unescape",
    "WeakMap",
    "WeakSet",
];

const MATH_CONSTANTS: &[(&str, f64)] = &[
    ("E", std::f64::consts::E),
    ("LN2", std::f64::consts::LN_2),
    ("LN10", std::f64::consts::LN_10),
    ("LOG2E", std::f64::consts::LOG2_E),
    ("LOG10E", std::f64::consts::LOG10_E),
    ("PI", std::f64::consts::PI),
    ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ("SQRT2", std::f64::consts::SQRT_2),
];

const NUMBER_CONSTANTS: &[(&str, f64)] = &[
    ("EPSILON", f64::EPSILON),
    ("MAX_SAFE_INTEGER", 9_007_199_254_740_991.0),
    ("MAX_VALUE", f64::MAX),
    ("MIN_SAFE_INTEGER", -9_007_199_254_740_991.0),
    ("MIN_VALUE", 5e-324),
    ("NaN", f64::NAN),
    ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
    ("POSITIVE_INFINITY", f64::INFINITY),
];

impl Static {
    /// JavaScript's ToBoolean.
    pub(crate) fn truthy(&self) -> bool {
        match self {
            Static::Undefined | Static::Null => false,
            Static::Bool(b) => *b,
            Static::Num(n) => *n != 0.0 && !n.is_nan(),
            Static::Str(s) => !s.is_empty(),
            Static::BigInt(b) => *b != 0,
            _ => true,
        }
    }

    pub(crate) fn is_nullish(&self) -> bool {
        matches!(self, Static::Undefined | Static::Null)
    }

    /// The `typeof` result.
    pub(crate) fn type_of(&self) -> &'static str {
        match self {
            Static::Undefined => "undefined",
            Static::Null => "object",
            Static::Bool(_) => "boolean",
            Static::Num(_) => "number",
            Static::Str(_) => "string",
            Static::BigInt(_) => "bigint",
            Static::RegExp { .. } | Static::Array(_) | Static::Object(_) => "object",
            Static::Builtin(p) => match p.as_str() {
                "Math" | "JSON" | "Reflect" => "object",
                _ => "function",
            },
        }
    }

    /// JavaScript's ToString, when it is defined without user code.
    pub(crate) fn to_js_string(&self) -> Option<String> {
        Some(match self {
            Static::Undefined => "undefined".into(),
            Static::Null => "null".into(),
            Static::Bool(b) => b.to_string(),
            Static::Num(n) => number_to_string(*n),
            Static::Str(s) => s.clone(),
            Static::BigInt(b) => b.to_string(),
            Static::RegExp { source, flags } => format!("/{source}/{flags}"),
            Static::Array(items) => {
                let mut out = String::new();
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if !v.is_nullish() {
                        out.push_str(&v.to_js_string()?);
                    }
                }
                out
            }
            Static::Object(_) => "[object Object]".into(),
            Static::Builtin(_) => return None,
        })
    }

    /// JavaScript's ToNumber (`None` for BigInt, which throws).
    pub(crate) fn to_number(&self) -> Option<f64> {
        Some(match self {
            Static::Undefined => f64::NAN,
            Static::Null => 0.0,
            Static::Bool(b) => f64::from(u8::from(*b)),
            Static::Num(n) => *n,
            Static::Str(s) => string_to_number(s),
            Static::BigInt(_) | Static::Builtin(_) => return None,
            Static::Array(_) | Static::RegExp { .. } | Static::Object(_) => {
                string_to_number(&self.to_js_string()?)
            }
        })
    }

    fn is_primitive(&self) -> bool {
        matches!(
            self,
            Static::Undefined
                | Static::Null
                | Static::Bool(_)
                | Static::Num(_)
                | Static::Str(_)
                | Static::BigInt(_)
        )
    }
}

/// JavaScript's StringToNumber.
pub(crate) fn string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if t.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(d) = t.strip_prefix(prefix) {
            if d.is_empty() || !d.chars().all(|c| c.is_digit(radix)) {
                return f64::NAN;
            }
            return u128::from_str_radix(d, radix).map_or(f64::NAN, |v| v as f64);
        }
    }
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    if body == "Infinity" {
        return if t.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let ok = !body.is_empty()
        && body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
        && body
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() || c == '.');
    if !ok {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

fn to_int32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let m = n.trunc() % 4_294_967_296.0;
    let m = if m < 0.0 { m + 4_294_967_296.0 } else { m };
    m as u32 as i32
}

fn to_uint32(n: f64) -> u32 {
    to_int32(n) as u32
}

/// JavaScript's IsLooselyEqual on two static values (`None` when objects are involved).
fn loose_eq(a: &Static, b: &Static) -> Option<bool> {
    use Static::*;
    Some(match (a, b) {
        (Undefined | Null, Undefined | Null) => true,
        (Undefined | Null, _) | (_, Undefined | Null) => false,
        (Num(x), Num(y)) => x == y,
        (Str(x), Str(y)) => x == y,
        (Bool(x), Bool(y)) => x == y,
        (BigInt(x), BigInt(y)) => x == y,
        (Num(_) | Str(_) | Bool(_), Num(_) | Str(_) | Bool(_)) => {
            a.to_number()? == b.to_number()?
        }
        (BigInt(x), Num(y)) | (Num(y), BigInt(x)) => (*x as f64) == *y,
        _ => return None,
    })
}

fn strict_eq(a: &Static, b: &Static) -> Option<bool> {
    use Static::*;
    Some(match (a, b) {
        (Undefined, Undefined) | (Null, Null) => true,
        (Num(x), Num(y)) => x == y,
        (Str(x), Str(y)) => x == y,
        (Bool(x), Bool(y)) => x == y,
        (BigInt(x), BigInt(y)) => x == y,
        (Builtin(x), Builtin(y)) => x == y,
        _ if a.is_primitive() != b.is_primitive() || a.type_of() != b.type_of() => false,
        _ if a.is_primitive() => false,
        // Two object values: identity unknown.
        _ => return None,
    })
}

/// `<` with JavaScript's coercions (`None` = undefined result).
fn less_than(a: &Static, b: &Static) -> Option<Option<bool>> {
    if let (Static::Str(x), Static::Str(y)) = (a, b) {
        let xs: Vec<u16> = x.encode_utf16().collect();
        let ys: Vec<u16> = y.encode_utf16().collect();
        return Some(Some(xs < ys));
    }
    if let (Static::BigInt(x), Static::BigInt(y)) = (a, b) {
        return Some(Some(x < y));
    }
    let x = a.to_number()?;
    let y = b.to_number()?;
    if x.is_nan() || y.is_nan() {
        return Some(None);
    }
    Some(Some(x < y))
}

/// The property key a computed or plain member names.
fn key_of(v: &Static) -> Option<String> {
    v.to_js_string()
}

const MAX_DEPTH: u32 = 24;

/// ESLint's `getStaticValue`: the value of `id`, if known.
pub(crate) fn static_value(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Static> {
    eval(cx, id, 0)
}

fn eval(cx: &Ctx<'_, '_>, id: NodeId, depth: u32) -> Option<Static> {
    if depth > MAX_DEPTH {
        return None;
    }
    let d = depth + 1;
    let ast = cx.ast();
    match *ast.node(id) {
        Node::Paren { expr }
        | Node::As { expr, .. }
        | Node::Satisfies { expr, .. }
        | Node::TypeAssertion { expr, .. }
        | Node::NonNull { expr }
        | Node::Instantiation { expr, .. } => eval(cx, expr, d),
        Node::NullLit {} => Some(Static::Null),
        Node::BoolLit { value } => Some(Static::Bool(value)),
        Node::NumberLit { value } => Some(Static::Num(value.get())),
        Node::StringLit { value } => Some(Static::Str(ast.atom(value).to_owned())),
        Node::BigIntLit { value } => bigint_value(ast.atom(value)).map(Static::BigInt),
        Node::RegexLit { pattern, flags } => Some(Static::RegExp {
            source: ast.atom(pattern).to_owned(),
            flags: ast.atom(flags).to_owned(),
        }),
        Node::TemplateLit { parts } => {
            let mut s = String::new();
            for &p in ast.list(parts) {
                match *ast.node(p) {
                    Node::TemplateElement {
                        cooked,
                        invalid_escape,
                    } => {
                        if invalid_escape {
                            return None;
                        }
                        s.push_str(ast.atom(cooked));
                    }
                    _ => s.push_str(&eval(cx, p, d)?.to_js_string()?),
                }
            }
            Some(Static::Str(s))
        }
        Node::ArrayLit { elements } => {
            let mut out = Vec::new();
            for &e in ast.list(elements) {
                match *ast.node(e) {
                    Node::Hole {} => out.push(Static::Undefined),
                    Node::Spread { arg } => match eval(cx, arg, d)? {
                        Static::Array(items) => out.extend(items),
                        Static::Str(s) => out.extend(s.chars().map(|c| Static::Str(c.into()))),
                        _ => return None,
                    },
                    _ => out.push(eval(cx, e, d)?),
                }
            }
            Some(Static::Array(out))
        }
        Node::ObjectLit { props } => {
            let mut out: Vec<(String, Static)> = Vec::new();
            let set = |k: String, v: Static, out: &mut Vec<(String, Static)>| {
                if let Some(slot) = out.iter_mut().find(|(n, _)| *n == k) {
                    slot.1 = v;
                } else {
                    out.push((k, v));
                }
            };
            for &p in ast.list(props) {
                match *ast.node(p) {
                    Node::Property { key, value, .. } => {
                        let k = property_key(cx, key, d)?;
                        let v = eval(cx, value, d)?;
                        set(k, v, &mut out);
                    }
                    Node::ShorthandProp {
                        name, init: None, ..
                    } => {
                        let k = ident_name(ast, name)?.to_owned();
                        let v = eval(cx, name, d)?;
                        set(k, v, &mut out);
                    }
                    Node::Spread { arg } => match eval(cx, arg, d)? {
                        Static::Object(entries) => {
                            for (k, v) in entries {
                                set(k, v, &mut out);
                            }
                        }
                        Static::Undefined | Static::Null => {}
                        _ => return None,
                    },
                    _ => return None,
                }
            }
            Some(Static::Object(out))
        }
        Node::Assign {
            op: AssignOp::Assign,
            right,
            ..
        } => eval(cx, right, d),
        Node::Seq { exprs } => eval(cx, *ast.list(exprs).last()?, d),
        Node::Conditional { test, cons, alt } => {
            let t = eval(cx, test, d)?;
            eval(cx, if t.truthy() { cons } else { alt }, d)
        }
        Node::Binary { left, op, right } => match op {
            BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::Coalesce => {
                let l = eval(cx, left, d)?;
                let short = match op {
                    BinaryOp::LogicalOr => l.truthy(),
                    BinaryOp::LogicalAnd => !l.truthy(),
                    _ => !l.is_nullish(),
                };
                if short { Some(l) } else { eval(cx, right, d) }
            }
            BinaryOp::In | BinaryOp::InstanceOf => None,
            _ => {
                let l = eval(cx, left, d)?;
                let r = eval(cx, right, d)?;
                binary(op, &l, &r)
            }
        },
        Node::Unary { op, arg } => match op {
            UnaryOp::Delete => None,
            UnaryOp::Void => Some(Static::Undefined),
            _ => {
                let v = eval(cx, arg, d)?;
                Some(match op {
                    UnaryOp::Not => Static::Bool(!v.truthy()),
                    UnaryOp::TypeOf => Static::Str(v.type_of().into()),
                    UnaryOp::Minus => match v {
                        Static::BigInt(b) => Static::BigInt(b.checked_neg()?),
                        _ => Static::Num(-v.to_number()?),
                    },
                    UnaryOp::Plus => Static::Num(v.to_number()?),
                    UnaryOp::BitNot => match v {
                        Static::BigInt(b) => Static::BigInt(!b),
                        _ => Static::Num(f64::from(!to_int32(v.to_number()?))),
                    },
                    _ => return None,
                })
            }
        },
        Node::Ident { name } => {
            let sem = cx.semantic();
            let name = ast.atom(name);
            let r = sem.reference_at(id)?;
            let reference = sem.reference(r);
            match reference.resolution {
                Resolution::Global => builtin_global(name),
                Resolution::Symbol(s) => {
                    let sym = sem.symbol(s);
                    if sym.flags.contains(SymbolFlags::COMMENT_GLOBAL) {
                        return builtin_global(name);
                    }
                    let init = considered_const_init(cx, s)?;
                    let v = eval(cx, init, d)?;
                    if matches!(v, Static::Array(_) | Static::Object(_))
                        && has_mutation_in_property(cx, s)
                    {
                        return None;
                    }
                    Some(v)
                }
                Resolution::Unresolved => None,
            }
        }
        Node::Member {
            obj,
            optional,
            prop,
        } => {
            if matches!(ast.node(prop), Node::PrivateName { .. }) {
                return None;
            }
            let o = eval(cx, obj, d)?;
            if o.is_nullish() && optional {
                return Some(Static::Undefined);
            }
            member(&o, ident_name(ast, prop)?)
        }
        Node::Index {
            obj,
            optional,
            index,
        } => {
            let o = eval(cx, obj, d)?;
            if o.is_nullish() && optional {
                return Some(Static::Undefined);
            }
            let k = key_of(&eval(cx, index, d)?)?;
            member(&o, &k)
        }
        Node::Call {
            callee,
            optional,
            args,
            ..
        } => {
            let args = arg_values(cx, ast.list(args), d)?;
            let callee = skip_parens(ast, callee);
            match *ast.node(callee) {
                Node::Member { obj, prop, .. }
                | Node::Index {
                    obj, index: prop, ..
                } => {
                    if matches!(ast.node(prop), Node::PrivateName { .. }) {
                        return None;
                    }
                    let o = eval(cx, obj, d)?;
                    if o.is_nullish() && (optional || chain_optional(ast, callee)) {
                        return Some(Static::Undefined);
                    }
                    let name = match *ast.node(callee) {
                        Node::Member { prop, .. } => ident_name(ast, prop)?.to_owned(),
                        _ => key_of(&eval(cx, prop, d)?)?,
                    };
                    call_method(&o, &name, &args)
                }
                _ => {
                    let f = eval(cx, callee, d)?;
                    if f.is_nullish() && optional {
                        return Some(Static::Undefined);
                    }
                    let Static::Builtin(path) = f else {
                        return None;
                    };
                    call_builtin(&path, &args)
                }
            }
        }
        Node::TaggedTemplate { tag, quasi, .. } => {
            let Static::Builtin(path) = eval(cx, tag, d)? else {
                return None;
            };
            if path != "String.raw" {
                return None;
            }
            let Node::TemplateLit { parts } = *ast.node(quasi) else {
                return None;
            };
            let mut s = String::new();
            for &p in ast.list(parts) {
                if matches!(ast.node(p), Node::TemplateElement { .. }) {
                    s.push_str(raw_template_text(cx, p));
                } else {
                    s.push_str(&eval(cx, p, d)?.to_js_string()?);
                }
            }
            Some(Static::Str(s))
        }
        Node::New { callee, args, .. } => {
            let Static::Builtin(path) = eval(cx, callee, d)? else {
                return None;
            };
            let args = arg_values(cx, ast.list(args), d)?;
            match path.as_str() {
                "RegExp" => regexp_from(&args),
                "Boolean" | "Number" | "String" | "Object" | "Map" | "Set" | "Date" => None,
                _ => None,
            }
        }
        _ => None,
    }
}

fn chain_optional(ast: &Ast, callee: NodeId) -> bool {
    matches!(
        ast.node(callee),
        Node::Member { optional: true, .. } | Node::Index { optional: true, .. }
    )
}

/// The raw text of a template element, without its delimiters.
fn raw_template_text<'a>(cx: &Ctx<'a, '_>, element: NodeId) -> &'a str {
    let text = cx.text(element);
    let text = text
        .strip_prefix('`')
        .or_else(|| text.strip_prefix('}'))
        .unwrap_or(text);
    text.strip_suffix('`')
        .or_else(|| text.strip_suffix("${"))
        .unwrap_or(text)
}

fn property_key(cx: &Ctx<'_, '_>, key: NodeId, depth: u32) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(key) {
        Node::ComputedKey { expr } => key_of(&eval(cx, expr, depth)?),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::PrivateName { .. } => None,
        _ => key_name(ast, cx.src(), key).map(|k| k.into_owned()),
    }
}

fn arg_values(cx: &Ctx<'_, '_>, args: &[NodeId], depth: u32) -> Option<Vec<Static>> {
    let ast = cx.ast();
    let mut out = Vec::new();
    for &a in args {
        if let Node::Spread { arg } = *ast.node(a) {
            match eval(cx, arg, depth)? {
                Static::Array(items) => out.extend(items),
                _ => return None,
            }
        } else {
            out.push(eval(cx, a, depth)?);
        }
    }
    Some(out)
}

fn bigint_value(text: &str) -> Option<i128> {
    let t = text.strip_suffix('n').unwrap_or(text);
    parse_bigint(t)?.parse::<i128>().ok()
}

fn builtin_global(name: &str) -> Option<Static> {
    if !BUILTIN_NAMES.contains(&name) {
        return None;
    }
    Some(match name {
        "undefined" => Static::Undefined,
        "NaN" => Static::Num(f64::NAN),
        "Infinity" => Static::Num(f64::INFINITY),
        _ => Static::Builtin(name.to_owned()),
    })
}

/// The initializer of a variable that ESLint's evaluator treats as a constant: one
/// plain-name `var`/`let`/`const` declaration, `const` or never written after its
/// initialization.
pub(crate) fn considered_const_init(cx: &Ctx<'_, '_>, sym: SymbolId) -> Option<NodeId> {
    let ast = cx.ast();
    let sem = cx.semantic();
    let s = sem.symbol(sym);
    let [decl] = s.decls.as_slice() else {
        return None;
    };
    if !matches!(decl.kind, DeclKind::Var | DeclKind::Let | DeclKind::Const) {
        return None;
    }
    let Node::VarDeclarator { name, init, .. } = *ast.node(decl.node) else {
        return None;
    };
    if name != decl.name {
        return None;
    }
    if decl.kind != DeclKind::Const {
        let mut inits = 0;
        for &r in &s.refs {
            let r = sem.reference(r);
            if r.flags.contains(RefFlags::INIT) {
                inits += 1;
            } else if r.is_write() {
                return None;
            }
        }
        if inits != 1 {
            return None;
        }
    }
    init
}

const MUTATING_ARRAY_METHODS: &[&str] = &[
    "copyWithin",
    "fill",
    "pop",
    "push",
    "reverse",
    "shift",
    "sort",
    "splice",
    "unshift",
];

/// A reference writes to a property of the variable (`a.b = 1`, `a.x++`, `a.push(1)`).
fn has_mutation_in_property(cx: &Ctx<'_, '_>, sym: SymbolId) -> bool {
    let ast = cx.ast();
    let sem = cx.semantic();
    for &r in &sem.symbol(sym).refs {
        let mut node = sem.reference(r).node;
        while let Some(p) = parent_np(cx, node) {
            if matches!(ast.node(p), Node::Member { .. } | Node::Index { .. }) {
                node = p;
            } else {
                break;
            }
        }
        let Some(p) = parent_np(cx, node) else {
            continue;
        };
        match *ast.node(p) {
            Node::Assign { left, .. } if skip_parens(ast, left) == node => return true,
            Node::Update { arg, .. } if skip_parens(ast, arg) == node => return true,
            Node::Call { callee, .. } if skip_parens(ast, callee) == node => {
                if let Some(name) = member_name(ast, cx.src(), node)
                    && MUTATING_ARRAY_METHODS.contains(&name.as_ref())
                {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// The parent of `id`, looking through parentheses (ESTree has no parenthesis nodes).
pub(crate) fn parent_np(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let mut p = cx.parent(id)?;
    while matches!(cx.node(p), Node::Paren { .. }) {
        p = cx.parent(p)?;
    }
    Some(p)
}

fn binary(op: BinaryOp, l: &Static, r: &Static) -> Option<Static> {
    use BinaryOp::*;
    Some(match op {
        Eq => Static::Bool(loose_eq(l, r)?),
        NotEq => Static::Bool(!loose_eq(l, r)?),
        StrictEq => Static::Bool(strict_eq(l, r)?),
        StrictNotEq => Static::Bool(!strict_eq(l, r)?),
        Lt => Static::Bool(less_than(l, r)?.unwrap_or(false)),
        Gt => Static::Bool(less_than(r, l)?.unwrap_or(false)),
        LtEq => Static::Bool(less_than(r, l)?.is_some_and(|v| !v)),
        GtEq => Static::Bool(less_than(l, r)?.is_some_and(|v| !v)),
        Add => {
            let lp = to_primitive(l)?;
            let rp = to_primitive(r)?;
            if matches!(lp, Static::Str(_)) || matches!(rp, Static::Str(_)) {
                let mut s = lp.to_js_string()?;
                s.push_str(&rp.to_js_string()?);
                Static::Str(s)
            } else if let (Static::BigInt(a), Static::BigInt(b)) = (&lp, &rp) {
                Static::BigInt(a.checked_add(*b)?)
            } else {
                Static::Num(lp.to_number()? + rp.to_number()?)
            }
        }
        _ => {
            if let (Static::BigInt(a), Static::BigInt(b)) = (l, r) {
                let (a, b) = (*a, *b);
                return Some(Static::BigInt(match op {
                    Sub => a.checked_sub(b)?,
                    Mul => a.checked_mul(b)?,
                    Div => a.checked_div(b)?,
                    Mod => a.checked_rem(b)?,
                    Exp => a.checked_pow(u32::try_from(b).ok()?)?,
                    BitOr => a | b,
                    BitXor => a ^ b,
                    BitAnd => a & b,
                    Shl => a.checked_shl(u32::try_from(b).ok()?)?,
                    Shr => a.checked_shr(u32::try_from(b).ok()?)?,
                    _ => return None,
                }));
            }
            let a = l.to_number()?;
            let b = r.to_number()?;
            Static::Num(match op {
                Sub => a - b,
                Mul => a * b,
                Div => a / b,
                Mod => {
                    if b.is_infinite() && a.is_finite() {
                        a
                    } else {
                        a % b
                    }
                }
                Exp => {
                    if b.is_nan() || (a.abs() == 1.0 && b.is_infinite()) {
                        f64::NAN
                    } else {
                        a.powf(b)
                    }
                }
                BitOr => f64::from(to_int32(a) | to_int32(b)),
                BitXor => f64::from(to_int32(a) ^ to_int32(b)),
                BitAnd => f64::from(to_int32(a) & to_int32(b)),
                Shl => f64::from(to_int32(a).wrapping_shl(to_uint32(b) & 31)),
                Shr => f64::from(to_int32(a) >> (to_uint32(b) & 31)),
                UShr => f64::from(to_uint32(a) >> (to_uint32(b) & 31)),
                _ => return None,
            })
        }
    })
}

fn to_primitive(v: &Static) -> Option<Static> {
    if v.is_primitive() {
        Some(v.clone())
    } else {
        Some(Static::Str(v.to_js_string()?))
    }
}

fn member(o: &Static, key: &str) -> Option<Static> {
    match o {
        Static::Undefined | Static::Null => None,
        Static::Str(s) => {
            if key == "length" {
                return Some(Static::Num(s.encode_utf16().count() as f64));
            }
            if let Ok(i) = key.parse::<usize>()
                && i.to_string() == key
            {
                let units: Vec<u16> = s.encode_utf16().collect();
                return Some(match units.get(i) {
                    Some(&u) => Static::Str(String::from_utf16_lossy(&[u])),
                    None => Static::Undefined,
                });
            }
            Some(prototype_member("String", key))
        }
        Static::Array(items) => {
            if key == "length" {
                return Some(Static::Num(items.len() as f64));
            }
            if let Ok(i) = key.parse::<usize>()
                && i.to_string() == key
            {
                return Some(items.get(i).cloned().unwrap_or(Static::Undefined));
            }
            Some(prototype_member("Array", key))
        }
        Static::Object(entries) => Some(
            entries
                .iter()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| prototype_member("Object", key)),
        ),
        Static::RegExp { source, flags } => Some(match key {
            "source" => Static::Str(source.clone()),
            "flags" => Static::Str(flags.clone()),
            "global" => Static::Bool(flags.contains('g')),
            "ignoreCase" => Static::Bool(flags.contains('i')),
            "multiline" => Static::Bool(flags.contains('m')),
            "dotAll" => Static::Bool(flags.contains('s')),
            "sticky" => Static::Bool(flags.contains('y')),
            "unicode" => Static::Bool(flags.contains('u')),
            "hasIndices" => Static::Bool(flags.contains('d')),
            "lastIndex" => Static::Num(0.0),
            _ => prototype_member("RegExp", key),
        }),
        Static::Num(_) => Some(prototype_member("Number", key)),
        Static::Bool(_) => Some(prototype_member("Boolean", key)),
        Static::BigInt(_) => Some(prototype_member("BigInt", key)),
        Static::Builtin(path) => builtin_member(path, key),
    }
}

/// A method found on a built-in prototype (`String.prototype.trim`), or `undefined`.
fn prototype_member(class: &str, key: &str) -> Static {
    let known: &[&str] = match class {
        "String" => &[
            "at",
            "charAt",
            "charCodeAt",
            "codePointAt",
            "concat",
            "endsWith",
            "includes",
            "indexOf",
            "lastIndexOf",
            "localeCompare",
            "match",
            "matchAll",
            "normalize",
            "padEnd",
            "padStart",
            "repeat",
            "replace",
            "replaceAll",
            "search",
            "slice",
            "split",
            "startsWith",
            "substr",
            "substring",
            "toLowerCase",
            "toString",
            "toUpperCase",
            "trim",
            "trimEnd",
            "trimLeft",
            "trimRight",
            "trimStart",
            "valueOf",
        ],
        "Array" => &[
            "at",
            "concat",
            "entries",
            "every",
            "filter",
            "find",
            "findIndex",
            "flat",
            "includes",
            "indexOf",
            "join",
            "keys",
            "lastIndexOf",
            "map",
            "push",
            "slice",
            "some",
            "toString",
            "values",
        ],
        "Number" => &["toExponential", "toFixed", "toPrecision", "toString"],
        _ => &["toString", "valueOf", "hasOwnProperty"],
    };
    if known.contains(&key) {
        Static::Builtin(format!("{class}.prototype.{key}"))
    } else {
        Static::Undefined
    }
}

fn builtin_member(path: &str, key: &str) -> Option<Static> {
    match path {
        "Math" => {
            if let Some(&(_, v)) = MATH_CONSTANTS.iter().find(|(n, _)| *n == key) {
                return Some(Static::Num(v));
            }
            Some(Static::Builtin(format!("Math.{key}")))
        }
        "Number" => {
            if let Some(&(_, v)) = NUMBER_CONSTANTS.iter().find(|(n, _)| *n == key) {
                return Some(Static::Num(v));
            }
            Some(Static::Builtin(format!("Number.{key}")))
        }
        _ if path.contains('.') => None,
        _ => Some(Static::Builtin(format!("{path}.{key}"))),
    }
}

fn regexp_from(args: &[Static]) -> Option<Static> {
    let source = match args.first() {
        None | Some(Static::Undefined) => "(?:)".to_owned(),
        Some(Static::RegExp { source, .. }) => source.clone(),
        Some(v) => {
            let s = v.to_js_string()?;
            if s.is_empty() { "(?:)".to_owned() } else { s }
        }
    };
    let flags = match (args.get(1), args.first()) {
        (None | Some(Static::Undefined), Some(Static::RegExp { flags, .. })) => flags.clone(),
        (None | Some(Static::Undefined), _) => String::new(),
        (Some(v), _) => v.to_js_string()?,
    };
    Some(Static::RegExp { source, flags })
}

fn num_arg(args: &[Static], i: usize) -> Option<f64> {
    args.get(i).unwrap_or(&Static::Undefined).to_number()
}

fn int_arg(args: &[Static], i: usize, default: f64) -> Option<f64> {
    match args.get(i) {
        None | Some(Static::Undefined) => Some(default),
        Some(v) => {
            let n = v.to_number()?;
            Some(if n.is_nan() { 0.0 } else { n.trunc() })
        }
    }
}

/// Calls of a built-in function on static arguments (the pure ones ESLint allows).
fn call_builtin(path: &str, args: &[Static]) -> Option<Static> {
    let first = args.first().cloned().unwrap_or(Static::Undefined);
    Some(match path {
        "String" => match args.first() {
            None => Static::Str(String::new()),
            Some(v) => Static::Str(v.to_js_string()?),
        },
        "Number" => match args.first() {
            None => Static::Num(0.0),
            Some(Static::BigInt(b)) => Static::Num(*b as f64),
            Some(v) => Static::Num(v.to_number()?),
        },
        "Boolean" => Static::Bool(first.truthy()),
        "isNaN" => Static::Bool(first.to_number()?.is_nan()),
        "isFinite" => Static::Bool(first.to_number()?.is_finite()),
        "Number.isNaN" => Static::Bool(matches!(first, Static::Num(n) if n.is_nan())),
        "Number.isFinite" => Static::Bool(matches!(first, Static::Num(n) if n.is_finite())),
        "parseInt" | "Number.parseInt" => {
            let s = first.to_js_string()?;
            let radix = int_arg(args, 1, 0.0)?;
            Static::Num(parse_int(&s, radix as i64))
        }
        "parseFloat" | "Number.parseFloat" => Static::Num(parse_float(&first.to_js_string()?)),
        "Object.freeze" | "Object.preventExtensions" | "Object.seal" => first,
        "Array.isArray" => Static::Bool(matches!(first, Static::Array(_))),
        "Array.of" => Static::Array(args.to_vec()),
        "RegExp" => return regexp_from(args),
        "Object.keys" => match first {
            Static::Object(e) => {
                Static::Array(e.into_iter().map(|(k, _)| Static::Str(k)).collect())
            }
            _ => return None,
        },
        "Object.values" => match first {
            Static::Object(e) => Static::Array(e.into_iter().map(|(_, v)| v).collect()),
            _ => return None,
        },
        "String.fromCharCode" => {
            let mut units = Vec::new();
            for i in 0..args.len() {
                units.push(to_uint32(num_arg(args, i)?) as u16);
            }
            Static::Str(String::from_utf16_lossy(&units))
        }
        _ => {
            let name = path.strip_prefix("Math.")?;
            let a = num_arg(args, 0)?;
            Static::Num(match name {
                "abs" => a.abs(),
                "ceil" => a.ceil(),
                "floor" => a.floor(),
                "round" => (a + 0.5).floor(),
                "trunc" => a.trunc(),
                "sign" => {
                    if a.is_nan() || a == 0.0 {
                        a
                    } else {
                        a.signum()
                    }
                }
                "sqrt" => a.sqrt(),
                "cbrt" => a.cbrt(),
                "max" | "min" => {
                    let mut acc = if name == "max" {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    };
                    for i in 0..args.len() {
                        let v = num_arg(args, i)?;
                        if v.is_nan() {
                            return Some(Static::Num(f64::NAN));
                        }
                        acc = if name == "max" {
                            acc.max(v)
                        } else {
                            acc.min(v)
                        };
                    }
                    acc
                }
                "pow" => a.powf(num_arg(args, 1)?),
                "log" => a.ln(),
                "log2" => a.log2(),
                "log10" => a.log10(),
                "exp" => a.exp(),
                _ => return None,
            })
        }
    })
}

fn parse_int(s: &str, radix: i64) -> f64 {
    let t = s.trim_start();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mut radix, mut t) = (radix, t);
    if (radix == 0 || radix == 16)
        && let Some(r) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))
    {
        t = r;
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let digits: String = t.chars().take_while(|c| c.is_digit(radix as u32)).collect();
    if digits.is_empty() {
        return f64::NAN;
    }
    let mut v = 0.0f64;
    for c in digits.chars() {
        v = v * radix as f64 + f64::from(c.to_digit(radix as u32).unwrap_or(0));
    }
    if neg { -v } else { v }
}

fn parse_float(s: &str) -> f64 {
    let t = s.trim_start();
    let body = t.strip_prefix(['+', '-']).unwrap_or(t);
    if body.starts_with("Infinity") {
        return if t.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let mut end = 0;
    let bytes = t.as_bytes();
    let mut seen_dot = false;
    let mut seen_e = false;
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut digits = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            digits = true;
            end = i + 1;
        } else if c == b'.' && !seen_dot && !seen_e {
            seen_dot = true;
        } else if (c == b'e' || c == b'E') && digits && !seen_e {
            seen_e = true;
            if i + 1 < bytes.len() && (bytes[i + 1] == b'+' || bytes[i + 1] == b'-') {
                i += 1;
            }
        } else {
            break;
        }
        i += 1;
    }
    if !digits {
        return f64::NAN;
    }
    t[..end].parse::<f64>().unwrap_or(f64::NAN)
}

fn call_method(o: &Static, name: &str, args: &[Static]) -> Option<Static> {
    match o {
        Static::Builtin(path) => call_builtin(&format!("{path}.{name}"), args),
        Static::Str(s) => string_method(s, name, args),
        Static::Array(items) => array_method(items, name, args),
        Static::Num(n) => match name {
            "toString" if args.is_empty() => Some(Static::Str(number_to_string(*n))),
            "toFixed" => {
                let digits = int_arg(args, 0, 0.0)? as usize;
                if digits > 100 || !n.is_finite() || n.abs() >= 1e21 {
                    return None;
                }
                Some(Static::Str(format!("{n:.digits$}")))
            }
            _ => None,
        },
        _ => None,
    }
}

fn string_method(s: &str, name: &str, args: &[Static]) -> Option<Static> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let len = units.len() as f64;
    let rel = |v: f64| -> usize {
        if v < 0.0 {
            (len + v).max(0.0) as usize
        } else {
            v.min(len) as usize
        }
    };
    let str_arg =
        |i: usize| -> Option<String> { args.get(i).unwrap_or(&Static::Undefined).to_js_string() };
    let slice = |a: usize, b: usize| String::from_utf16_lossy(&units[a.min(b)..b.max(a.min(b))]);
    Some(match name {
        "toString" | "valueOf" => Static::Str(s.to_owned()),
        "toLowerCase" => Static::Str(s.to_lowercase()),
        "toUpperCase" => Static::Str(s.to_uppercase()),
        "trim" => Static::Str(s.trim().to_owned()),
        "trimStart" | "trimLeft" => Static::Str(s.trim_start().to_owned()),
        "trimEnd" | "trimRight" => Static::Str(s.trim_end().to_owned()),
        "slice" => {
            let a = rel(int_arg(args, 0, 0.0)?);
            let b = rel(int_arg(args, 1, len)?);
            if a >= b {
                Static::Str(String::new())
            } else {
                Static::Str(slice(a, b))
            }
        }
        "substring" => {
            let clamp = |v: f64| v.clamp(0.0, len) as usize;
            let a = clamp(int_arg(args, 0, 0.0)?);
            let b = clamp(int_arg(args, 1, len)?);
            Static::Str(slice(a.min(b), a.max(b)))
        }
        "charAt" => {
            let i = int_arg(args, 0, 0.0)?;
            Static::Str(if i >= 0.0 && i < len {
                String::from_utf16_lossy(&units[i as usize..i as usize + 1])
            } else {
                String::new()
            })
        }
        "charCodeAt" => {
            let i = int_arg(args, 0, 0.0)?;
            Static::Num(if i >= 0.0 && i < len {
                f64::from(units[i as usize])
            } else {
                f64::NAN
            })
        }
        "at" => {
            let i = int_arg(args, 0, 0.0)?;
            let i = if i < 0.0 { len + i } else { i };
            if i >= 0.0 && i < len {
                Static::Str(String::from_utf16_lossy(&units[i as usize..i as usize + 1]))
            } else {
                Static::Undefined
            }
        }
        "concat" => {
            let mut out = s.to_owned();
            for a in args {
                out.push_str(&a.to_js_string()?);
            }
            Static::Str(out)
        }
        "includes" => Static::Bool(s.contains(&str_arg(0)?)),
        "startsWith" if args.len() <= 1 => Static::Bool(s.starts_with(&str_arg(0)?)),
        "endsWith" if args.len() <= 1 => Static::Bool(s.ends_with(&str_arg(0)?)),
        "indexOf" if args.len() <= 1 => {
            let needle: Vec<u16> = str_arg(0)?.encode_utf16().collect();
            Static::Num(find_units(&units, &needle).map_or(-1.0, |i| i as f64))
        }
        "repeat" => {
            let n = int_arg(args, 0, 0.0)?;
            if !(0.0..=10_000.0).contains(&n) || s.len() * n as usize > 1 << 20 {
                return None;
            }
            Static::Str(s.repeat(n as usize))
        }
        _ => return None,
    })
}

fn find_units(hay: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn array_method(items: &[Static], name: &str, args: &[Static]) -> Option<Static> {
    Some(match name {
        "join" => {
            let sep = match args.first() {
                None | Some(Static::Undefined) => ",".to_owned(),
                Some(v) => v.to_js_string()?,
            };
            let mut out = String::new();
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(&sep);
                }
                if !v.is_nullish() {
                    out.push_str(&v.to_js_string()?);
                }
            }
            Static::Str(out)
        }
        "toString" => Static::Str(Static::Array(items.to_vec()).to_js_string()?),
        "includes" => Static::Bool(items.iter().any(|v| {
            let a = args.first().unwrap_or(&Static::Undefined);
            strict_eq(v, a) == Some(true)
                || matches!((v, a), (Static::Num(x), Static::Num(y)) if x.is_nan() && y.is_nan())
        })),
        "indexOf" => {
            let a = args.first().unwrap_or(&Static::Undefined);
            Static::Num(
                items
                    .iter()
                    .position(|v| strict_eq(v, a) == Some(true))
                    .map_or(-1.0, |i| i as f64),
            )
        }
        "at" => {
            let len = items.len() as f64;
            let i = int_arg(args, 0, 0.0)?;
            let i = if i < 0.0 { len + i } else { i };
            if i >= 0.0 && i < len {
                items[i as usize].clone()
            } else {
                Static::Undefined
            }
        }
        "concat" => {
            let mut out = items.to_vec();
            for a in args {
                match a {
                    Static::Array(more) => out.extend(more.iter().cloned()),
                    v => out.push(v.clone()),
                }
            }
            Static::Array(out)
        }
        _ => return None,
    })
}

// ---- Side effects ------------------------------------------------------------------------

/// `hasSideEffect` of eslint-utils (default options): calls, `new`, assignments, updates,
/// `await`, `yield`, `import()` and `delete` anywhere outside nested functions.
pub(crate) fn has_side_effect(ast: &Ast, id: NodeId) -> bool {
    side_effect(ast, id, 0)
}

fn side_effect(ast: &Ast, id: NodeId, depth: u32) -> bool {
    if depth > 200 {
        return true;
    }
    match *ast.node(id) {
        Node::Call { .. }
        | Node::New { .. }
        | Node::Assign { .. }
        | Node::Update { .. }
        | Node::Await { .. }
        | Node::Yield { .. }
        | Node::ImportCall { .. }
        | Node::Unary {
            op: UnaryOp::Delete,
            ..
        } => true,
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::FunctionDecl { .. } => false,
        _ => {
            let mut found = false;
            ast.for_each_child(id, |c| {
                if !found && is_value_child(ast, c) && side_effect(ast, c, depth + 1) {
                    found = true;
                }
            });
            found
        }
    }
}

/// Type nodes are not evaluated.
fn is_value_child(ast: &Ast, id: NodeId) -> bool {
    !matches!(
        ast.node(id),
        Node::KeywordType { .. }
            | Node::TypeRef { .. }
            | Node::TypeArgs { .. }
            | Node::TypeParams { .. }
            | Node::UnionType { .. }
            | Node::IntersectionType { .. }
            | Node::TypeLiteral { .. }
            | Node::ArrayType { .. }
            | Node::TupleType { .. }
            | Node::FunctionType { .. }
            | Node::TypeQuery { .. }
            | Node::TypeOperator { .. }
            | Node::ParenType { .. }
            | Node::IndexedAccessType { .. }
            | Node::ConditionalType { .. }
            | Node::MappedType { .. }
            | Node::ImportType { .. }
            | Node::TemplateLitType { .. }
    )
}

/// The value-level children ESLint's evaluator walks: no functions or classes, no type
/// nodes, no non-computed member names or property keys.
pub(crate) fn evaluated_children(ast: &Ast, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    match *ast.node(id) {
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => {}
        Node::Member { obj, .. } => out.push(obj),
        Node::Property { key, value, .. } => {
            if let Node::ComputedKey { expr } = *ast.node(key) {
                out.push(expr);
            }
            out.push(value);
        }
        _ => ast.for_each_child(id, |c| {
            if is_value_child(ast, c) {
                out.push(c);
            }
        }),
    }
    out
}

/// The initializer of a plain-name `const` the identifier `id` refers to.
pub(crate) fn const_definition(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(SymbolId, NodeId)> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let s = sem.symbol_of_ident(id)?;
    let sym = sem.symbol(s);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    if decl.kind != DeclKind::Const {
        return None;
    }
    match *ast.node(decl.node) {
        Node::VarDeclarator {
            name,
            init: Some(init),
            ..
        } if name == decl.name => Some((s, init)),
        _ => None,
    }
}

/// `Object.freeze(x)` / `preventExtensions` / `seal` of a static value: a call that only
/// returns its argument.
pub(crate) fn is_pass_through_call(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    visited: &mut Vec<SymbolId>,
) -> bool {
    let ast = cx.ast();
    let id = skip_ts_only(ast, id);
    let Node::Call {
        callee,
        optional: false,
        args,
        ..
    } = *ast.node(id)
    else {
        return false;
    };
    let args = ast.list(args);
    let [arg] = args else {
        return false;
    };
    if matches!(ast.node(*arg), Node::Spread { .. }) {
        return false;
    }
    let callee = skip_ts_only(ast, callee);
    let Node::Member {
        obj,
        optional: false,
        prop,
    } = *ast.node(callee)
    else {
        return false;
    };
    let obj = skip_ts_only(ast, obj);
    if !is_global_ident(cx, obj, "Object") {
        return false;
    }
    if !matches!(
        ident_name(ast, prop),
        Some("freeze" | "preventExtensions" | "seal")
    ) {
        return false;
    }
    no_side_effects_value(cx, *arg, visited).is_some()
}

/// Strips the TypeScript wrappers (not parentheses).
pub(crate) fn skip_ts_only(ast: &Ast, mut id: NodeId) -> NodeId {
    loop {
        match *ast.node(id) {
            Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. }
            | Node::Paren { expr } => id = expr,
            _ => return id,
        }
    }
}

/// An identifier that names the unshadowed global `name` (configured or implicit).
pub(crate) fn is_global_ident(cx: &Ctx<'_, '_>, id: NodeId, name: &str) -> bool {
    let ast = cx.ast();
    if ident_name(ast, id) != Some(name) {
        return false;
    }
    let sem = cx.semantic();
    match sem.reference_at(id).map(|r| sem.reference(r).resolution) {
        Some(Resolution::Global | Resolution::Unresolved) => true,
        Some(Resolution::Symbol(s)) => sem.symbol(s).flags.contains(SymbolFlags::COMMENT_GLOBAL),
        None => false,
    }
}

fn side_effectful_const_reference(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    visited: &mut Vec<SymbolId>,
) -> bool {
    let ast = cx.ast();
    if matches!(ast.node(id), Node::Ident { .. }) {
        return side_effectful_const_init(cx, id, visited);
    }
    if matches!(
        ast.node(id),
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. }
    ) {
        return false;
    }
    evaluated_children(ast, id)
        .into_iter()
        .any(|c| side_effectful_const_reference(cx, c, visited))
}

/// `hasSideEffectfulConstInitializer`: `id` is (or reads) a `const` whose initializer has
/// side effects.
pub(crate) fn has_side_effectful_const_initializer(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    side_effectful_const_init(cx, id, &mut Vec::new())
}

fn side_effectful_const_init(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> bool {
    let Some((sym, init)) = const_definition(cx, id) else {
        return !matches!(cx.node(id), Node::Ident { .. })
            && side_effectful_const_reference(cx, id, visited);
    };
    if visited.contains(&sym) {
        return true;
    }
    visited.push(sym);
    let ast = cx.ast();
    let result = (!is_pass_through_call(cx, init, visited) && has_side_effect(ast, init))
        || side_effectful_const_reference(cx, init, visited);
    visited.retain(|s| *s != sym);
    result
}

const SAFE_GLOBAL_MEMBERS: &[(&str, &[&str])] = &[
    (
        "Math",
        &[
            "E", "LN2", "LN10", "LOG2E", "LOG10E", "PI", "SQRT1_2", "SQRT2",
        ],
    ),
    (
        "Number",
        &[
            "EPSILON",
            "MAX_SAFE_INTEGER",
            "MAX_VALUE",
            "MIN_SAFE_INTEGER",
            "MIN_VALUE",
            "NaN",
            "NEGATIVE_INFINITY",
            "POSITIVE_INFINITY",
        ],
    ),
    ("String", &["raw"]),
    (
        "Symbol",
        &[
            "asyncIterator",
            "hasInstance",
            "isConcatSpreadable",
            "iterator",
            "match",
            "matchAll",
            "replace",
            "search",
            "species",
            "split",
            "toPrimitive",
            "toStringTag",
            "unscopables",
        ],
    ),
];

fn is_safe_global_member(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let ast = cx.ast();
    let (obj, name) = match *ast.node(id) {
        Node::Member {
            obj,
            optional: false,
            prop,
        } => (obj, ident_name(ast, prop).map(str::to_owned)),
        Node::Index {
            obj,
            optional: false,
            index,
        } => (obj, static_string_literal(ast, index)),
        _ => return false,
    };
    let Some(name) = name else {
        return false;
    };
    let obj_name = match ast.node(obj) {
        Node::Ident { name } => ast.atom(*name),
        _ => return false,
    };
    SAFE_GLOBAL_MEMBERS
        .iter()
        .any(|(o, props)| *o == obj_name && props.contains(&name.as_str()))
        && is_global_ident(cx, obj, obj_name)
}

/// A string literal's or substitution-free template's value (`getStaticStringValue`).
pub(crate) fn static_string_literal(ast: &Ast, id: NodeId) -> Option<String> {
    match *ast.node(id) {
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::TemplateLit { parts } => {
            let parts = ast.list(parts);
            match parts {
                [only] => match *ast.node(*only) {
                    Node::TemplateElement {
                        cooked,
                        invalid_escape: false,
                    } => Some(ast.atom(cooked).to_owned()),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

fn static_property_name(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(member) {
        Node::Member { prop, .. } => ident_name(ast, prop).map(str::to_owned),
        Node::Index { index, .. } => {
            let index = skip_parens(ast, index);
            match *ast.node(index) {
                Node::StringLit { value } => Some(ast.atom(value).to_owned()),
                Node::NumberLit { value } => Some(number_to_string(value.get())),
                Node::Ident { .. } if const_definition(cx, index).is_none() => None,
                _ => match static_value(cx, index)? {
                    v @ (Static::Str(_) | Static::Num(_)) => v.to_js_string(),
                    _ => None,
                },
            }
        }
        _ => None,
    }
}

fn is_safe_static_member(cx: &Ctx<'_, '_>, member: NodeId, visited: &mut Vec<SymbolId>) -> bool {
    let ast = cx.ast();
    let (obj, computed) = match *ast.node(member) {
        Node::Member { obj, .. } => (obj, None),
        Node::Index { obj, index, .. } => (obj, Some(index)),
        _ => return false,
    };
    if let Some(index) = computed
        && mutable_member_access(cx, index, visited)
    {
        return false;
    }
    let Some(name) = static_property_name(cx, member) else {
        return false;
    };
    let obj = skip_parens(ast, obj);
    let definition = const_definition(cx, obj);
    let primitive = static_string_literal(ast, obj).or_else(|| {
        definition.and_then(|(_, init)| static_string_literal(ast, skip_parens(ast, init)))
    });
    if let Some(s) = primitive {
        if name == "length" {
            return true;
        }
        return name
            .parse::<usize>()
            .is_ok_and(|i| i.to_string() == name && i < s.encode_utf16().count());
    }
    match *ast.node(obj) {
        Node::ArrayLit { elements } => {
            let elements = ast.list(elements);
            if elements.iter().any(|&e| {
                matches!(ast.node(e), Node::Spread { .. })
                    || (!matches!(ast.node(e), Node::Hole {})
                        && mutable_member_access(cx, e, visited))
            }) {
                return false;
            }
            if name == "length" {
                return true;
            }
            name.parse::<usize>().is_ok_and(|i| {
                i.to_string() == name
                    && i < elements.len()
                    && !matches!(ast.node(elements[i]), Node::Hole {})
            })
        }
        Node::ObjectLit { props } => {
            let props = ast.list(props);
            let mut found = None;
            for &p in props {
                match *ast.node(p) {
                    Node::Property { key, value, .. } => {
                        if matches!(ast.node(key), Node::ComputedKey { .. }) {
                            return false;
                        }
                        let k = key_name(ast, cx.src(), key).map(|k| k.into_owned());
                        if k.as_deref() == Some("__proto__")
                            || mutable_member_access(cx, value, visited)
                        {
                            return false;
                        }
                        if k.as_deref() == Some(name.as_str()) {
                            found = Some(true);
                        }
                    }
                    Node::ShorthandProp {
                        name: n,
                        init: None,
                        ..
                    } => {
                        if mutable_member_access(cx, n, visited) {
                            return false;
                        }
                        if ident_name(ast, n) == Some(name.as_str()) {
                            found = Some(true);
                        }
                    }
                    Node::MethodDef { key, .. } => {
                        if matches!(ast.node(key), Node::ComputedKey { .. }) {
                            return false;
                        }
                        if key_name(ast, cx.src(), key).as_deref() == Some(name.as_str()) {
                            found = Some(false);
                        }
                    }
                    _ => return false,
                }
            }
            found == Some(true)
        }
        _ => false,
    }
}

/// `hasPotentiallyMutableMemberAccess`: the value reads a property that could have been
/// changed (anything but literal members of literals and a few frozen globals).
pub(crate) fn mutable_member_access(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    visited: &mut Vec<SymbolId>,
) -> bool {
    let ast = cx.ast();
    let id = skip_ts_only(ast, id);
    if is_pass_through_call(cx, id, visited) {
        return false;
    }
    if let Some((sym, init)) = const_definition(cx, id) {
        if visited.contains(&sym) {
            return true;
        }
        visited.push(sym);
        let r = mutable_member_access(cx, init, visited);
        visited.retain(|s| *s != sym);
        return r;
    }
    if matches!(ast.node(id), Node::Member { .. } | Node::Index { .. }) {
        return !is_safe_global_member(cx, id) && !is_safe_static_member(cx, id, visited);
    }
    named_children_skipped(ast, id)
        .into_iter()
        .any(|c| mutable_member_access(cx, c, visited))
}

/// Value children without names (non-computed member properties and keys); unlike
/// [`evaluated_children`] this walks into functions and classes too.
fn named_children_skipped(ast: &Ast, id: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut key_and_value = |key: NodeId, value: Option<NodeId>| {
        if let Node::ComputedKey { expr } = *ast.node(key) {
            out.push(expr);
        }
        out.extend(value);
    };
    match *ast.node(id) {
        Node::Property { key, value, .. } => key_and_value(key, Some(value)),
        Node::MethodDef { key, value, .. } => key_and_value(key, Some(value)),
        Node::PropertyDef { key, value, .. } => key_and_value(key, value),
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => ast
            .for_each_child(id, |c| {
                if is_value_child(ast, c) {
                    out.push(c);
                }
            }),
        _ => return evaluated_children(ast, id),
    }
    out
}

fn no_side_effects_value(
    cx: &Ctx<'_, '_>,
    id: NodeId,
    visited: &mut Vec<SymbolId>,
) -> Option<Static> {
    let ast = cx.ast();
    let id = skip_ts_only(ast, id);
    if (has_side_effect(ast, id) && !is_pass_through_call(cx, id, visited))
        || side_effectful_const_init(cx, id, visited)
    {
        return None;
    }
    let v = static_value(cx, id)?;
    if mutable_member_access(cx, id, visited) {
        return None;
    }
    Some(v)
}

/// `getStaticValueIfNoSideEffects`: the static value, unless evaluating the expression (or a
/// constant it reads) has side effects or reads a property that may have changed.
pub(crate) fn static_value_no_side_effects(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Static> {
    no_side_effects_value(cx, id, &mut Vec::new())
}

/// `getStaticValueForControlFlow`: as [`static_value_no_side_effects`], and every binding on
/// a path that can be evaluated is a `const` declared before the expression.
pub(crate) fn static_value_control_flow(cx: &Ctx<'_, '_>, id: NodeId) -> Option<Static> {
    let v = static_value_no_side_effects(cx, id)?;
    if unsafe_binding_in_path(cx, id, &mut Vec::new()) {
        return None;
    }
    Some(v)
}

fn unsafe_binding_in_path(cx: &Ctx<'_, '_>, id: NodeId, visited: &mut Vec<SymbolId>) -> bool {
    let ast = cx.ast();
    let id = skip_ts_only(ast, id);
    match *ast.node(id) {
        Node::Ident { .. } => {
            let sem = cx.semantic();
            let Some(s) = sem.symbol_of_ident(id) else {
                return false;
            };
            let sym = sem.symbol(s);
            let [decl] = sym.decls.as_slice() else {
                return false;
            };
            if !matches!(
                decl.kind,
                DeclKind::Var | DeclKind::Let | DeclKind::Const | DeclKind::Using
            ) {
                return false;
            }
            if decl.kind != DeclKind::Const {
                return true;
            }
            if cx.span(decl.node).end > cx.span(id).start {
                return true;
            }
            let Node::VarDeclarator {
                init: Some(init), ..
            } = *ast.node(decl.node)
            else {
                return false;
            };
            if visited.contains(&s) {
                return false;
            }
            visited.push(s);
            let r = unsafe_binding_in_path(cx, init, visited);
            visited.retain(|x| *x != s);
            r
        }
        Node::Conditional { test, cons, alt } => {
            if unsafe_binding_in_path(cx, test, visited) {
                return true;
            }
            match no_side_effects_value(cx, test, visited) {
                Some(v) => unsafe_binding_in_path(cx, if v.truthy() { cons } else { alt }, visited),
                None => {
                    unsafe_binding_in_path(cx, cons, visited)
                        || unsafe_binding_in_path(cx, alt, visited)
                }
            }
        }
        Node::Binary {
            left,
            op: op @ (BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Coalesce),
            right,
        } => {
            if unsafe_binding_in_path(cx, left, visited) {
                return true;
            }
            match no_side_effects_value(cx, left, visited) {
                Some(v) => {
                    let evaluates_right = match op {
                        BinaryOp::LogicalAnd => v.truthy(),
                        BinaryOp::LogicalOr => !v.truthy(),
                        _ => v.is_nullish(),
                    };
                    evaluates_right && unsafe_binding_in_path(cx, right, visited)
                }
                None => unsafe_binding_in_path(cx, right, visited),
            }
        }
        Node::Seq { exprs } => ast
            .list(exprs)
            .iter()
            .any(|&e| unsafe_binding_in_path(cx, e, visited)),
        Node::FunctionExpr { .. } | Node::Arrow { .. } | Node::ClassExpr { .. } => false,
        _ => evaluated_children(ast, id)
            .into_iter()
            .any(|c| unsafe_binding_in_path(cx, c, visited)),
    }
}

fn is_safe_constructor_argument(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    !matches!(cx.node(id), Node::Spread { .. })
        && matches!(
            static_value_no_side_effects(cx, id),
            Some(
                Static::RegExp { .. }
                    | Static::Null
                    | Static::Undefined
                    | Static::BigInt(_)
                    | Static::Bool(_)
                    | Static::Num(_)
                    | Static::Str(_)
            )
        )
}

fn regexp_constructor_args(ast: &Ast, id: NodeId) -> Option<&[NodeId]> {
    match *ast.node(skip_parens(ast, id)) {
        Node::New { callee, args, .. } if ident_name(ast, callee) == Some("RegExp") => {
            Some(ast.list(args))
        }
        _ => None,
    }
}

/// The initializer of a single-definition plain-name variable that is never reassigned.
fn unwritten_variable_initializer(cx: &Ctx<'_, '_>, id: NodeId) -> Option<NodeId> {
    let ast = cx.ast();
    if !matches!(ast.node(id), Node::Ident { .. }) {
        return None;
    }
    let sem = cx.semantic();
    let sym = sem.symbol(sem.symbol_of_ident(id)?);
    let [decl] = sym.decls.as_slice() else {
        return None;
    };
    let Node::VarDeclarator {
        name,
        init: Some(init),
        ..
    } = *ast.node(decl.node)
    else {
        return None;
    };
    let written = sym.refs.iter().any(|&r| {
        let r = sem.reference(r);
        r.is_write() && !r.flags.contains(RefFlags::INIT)
    });
    (name == decl.name && !written).then_some(init)
}

/// `getStaticRegExp`: the regex (`source`, `flags`) a node statically is: a regex value
/// without side effects, or a regex literal / `new RegExp(...)` of static arguments, directly
/// or through a never-reassigned variable.
pub(crate) fn static_regexp(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(String, String)> {
    let ast = cx.ast();
    let as_regexp = |v: Option<Static>| match v {
        Some(Static::RegExp { source, flags }) => Some((source, flags)),
        _ => None,
    };
    if let Some(r) = as_regexp(static_value_no_side_effects(cx, id)) {
        return Some(r);
    }
    let inner = skip_parens(ast, id);
    let init = unwritten_variable_initializer(cx, inner);
    let init_literal =
        init.is_some_and(|i| matches!(ast.node(skip_parens(ast, i)), Node::RegexLit { .. }));
    let ctor_args = regexp_constructor_args(ast, inner)
        .or_else(|| init.and_then(|i| regexp_constructor_args(ast, i)));
    if !init_literal
        && ctor_args.is_none_or(|args| args.iter().any(|&a| !is_safe_constructor_argument(cx, a)))
    {
        return None;
    }
    as_regexp(static_value(cx, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_to_number_follows_js() {
        assert_eq!(string_to_number(""), 0.0);
        assert_eq!(string_to_number(" 12 "), 12.0);
        assert_eq!(string_to_number("0x10"), 16.0);
        assert!(string_to_number("12px").is_nan());
        assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
        assert_eq!(string_to_number(".5"), 0.5);
    }

    #[test]
    fn parse_int_and_float() {
        assert_eq!(parse_int("10", 2), 2.0);
        assert_eq!(parse_int("0x1f", 0), 31.0);
        assert_eq!(parse_int("12px", 10), 12.0);
        assert!(parse_int("px", 10).is_nan());
        assert_eq!(parse_float("3.5e2x"), 350.0);
        assert!(parse_float("x").is_nan());
    }

    #[test]
    fn binary_ops() {
        let add = binary(BinaryOp::Add, &Static::Str("a".into()), &Static::Num(1.0));
        assert_eq!(add, Some(Static::Str("a1".into())));
        let ushr = binary(BinaryOp::UShr, &Static::Num(-1.0), &Static::Num(0.0));
        assert_eq!(ushr, Some(Static::Num(4_294_967_295.0)));
        assert_eq!(
            binary(BinaryOp::Eq, &Static::Null, &Static::Undefined),
            Some(Static::Bool(true))
        );
    }
}
