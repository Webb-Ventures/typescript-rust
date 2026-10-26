//! `unicorn/no-nonstandard-builtin-properties`: a property read on a built-in that the
//! standard (the latest ECMAScript edition, stage 4 proposals and a few web built-ins such as
//! `URL`) does not define, or a standard data property that is called or constructed.
//!
//! The receiver must be recognisable without types: a global built-in (`Math`, also through
//! `globalThis`/`window`/`self`/`global`), its `.prototype`, `new X()` of one, or a literal
//! (string, number, `+1`/`-1n`, boolean, regex, template, array). Indexes into arrays,
//! strings and typed arrays are fine. The property name is the plain name or a literal key;
//! constants are not followed.

use super::prelude::*;
use super::unicorn_static::is_global_ident;
use crate::ast_util::number_to_string;
use ts_r_ast::{Ast, BinaryOp, HeritageKind, UnaryOp};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/no-nonstandard-builtin-properties",
        "Disallow non-standard properties on built-in objects.",
        NO_PRESET,
        Category::Suspicious,
    ),
    create: create_default::<NoNonstandardBuiltinProperties>,
};

#[derive(Debug, Default)]
struct NoNonstandardBuiltinProperties;

/// The properties of one kind of object: its own data properties and methods, plus those
/// of the object it inherits from.
struct Shape {
    base: Option<&'static Shape>,
    props: &'static [&'static str],
    methods: &'static [&'static str],
}

impl Shape {
    fn has(&self, name: &str) -> bool {
        self.props.contains(&name)
            || self.methods.contains(&name)
            || self.base.is_some_and(|b| b.has(name))
    }

    fn callable(&self, name: &str) -> bool {
        self.methods.contains(&name) || self.base.is_some_and(|b| b.callable(name))
    }
}

const fn shape(
    base: &'static Shape,
    props: &'static [&'static str],
    methods: &'static [&'static str],
) -> Shape {
    Shape {
        base: Some(base),
        props,
        methods,
    }
}

static OBJECT: Shape = Shape {
    base: None,
    props: &["__proto__", "constructor"],
    methods: &[
        "__defineGetter__",
        "__defineSetter__",
        "__lookupGetter__",
        "__lookupSetter__",
        "hasOwnProperty",
        "isPrototypeOf",
        "propertyIsEnumerable",
        "toLocaleString",
        "toString",
        "valueOf",
    ],
};
static FUNCTION: Shape = shape(
    &OBJECT,
    &["arguments", "caller", "length", "name"],
    &["apply", "bind", "call"],
);
/// A constructor with no static members of its own (also what a function instance has).
static CONSTRUCTOR: Shape = shape(&FUNCTION, &["prototype"], &[]);

static ARRAY_PROTO: Shape = shape(
    &OBJECT,
    &["length"],
    &[
        "at",
        "concat",
        "copyWithin",
        "entries",
        "every",
        "fill",
        "filter",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "forEach",
        "includes",
        "indexOf",
        "join",
        "keys",
        "lastIndexOf",
        "map",
        "pop",
        "push",
        "reduce",
        "reduceRight",
        "reverse",
        "shift",
        "slice",
        "some",
        "sort",
        "splice",
        "toReversed",
        "toSorted",
        "toSpliced",
        "unshift",
        "values",
        "with",
    ],
);
static ARRAY_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["from", "fromAsync", "isArray", "of"]);
static ARRAY_BUFFER_PROTO: Shape = shape(
    &OBJECT,
    &["byteLength", "detached", "maxByteLength", "resizable"],
    &["resize", "slice", "transfer", "transferToFixedLength"],
);
static ARRAY_BUFFER_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["isView"]);
static BIGINT_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["asIntN", "asUintN"]);
static DATA_VIEW_PROTO: Shape = shape(
    &OBJECT,
    &["buffer", "byteLength", "byteOffset"],
    &[
        "getBigInt64",
        "getBigUint64",
        "getFloat16",
        "getFloat32",
        "getFloat64",
        "getInt16",
        "getInt32",
        "getInt8",
        "getUint16",
        "getUint32",
        "getUint8",
        "setBigInt64",
        "setBigUint64",
        "setFloat16",
        "setFloat32",
        "setFloat64",
        "setInt16",
        "setInt32",
        "setInt8",
        "setUint16",
        "setUint32",
        "setUint8",
    ],
);
static DATE_PROTO: Shape = shape(
    &OBJECT,
    &[],
    &[
        "getDate",
        "getDay",
        "getFullYear",
        "getHours",
        "getMilliseconds",
        "getMinutes",
        "getMonth",
        "getSeconds",
        "getTime",
        "getTimezoneOffset",
        "getUTCDate",
        "getUTCDay",
        "getUTCFullYear",
        "getUTCHours",
        "getUTCMilliseconds",
        "getUTCMinutes",
        "getUTCMonth",
        "getUTCSeconds",
        "getYear",
        "setDate",
        "setFullYear",
        "setHours",
        "setMilliseconds",
        "setMinutes",
        "setMonth",
        "setSeconds",
        "setTime",
        "setUTCDate",
        "setUTCFullYear",
        "setUTCHours",
        "setUTCMilliseconds",
        "setUTCMinutes",
        "setUTCMonth",
        "setUTCSeconds",
        "setYear",
        "toDateString",
        "toGMTString",
        "toISOString",
        "toJSON",
        "toLocaleDateString",
        "toLocaleTimeString",
        "toTemporalInstant",
        "toTimeString",
        "toUTCString",
    ],
);
static DATE_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["now", "parse", "UTC"]);
// `stack` is not in ECMAScript yet, but every engine has it.
static ERROR_PROTO: Shape = shape(&OBJECT, &["message", "name", "stack"], &["toString"]);
static ERROR_INSTANCE: Shape = shape(&ERROR_PROTO, &["cause", "message"], &[]);
static AGGREGATE_ERROR_INSTANCE: Shape = shape(&ERROR_INSTANCE, &["errors"], &[]);
static ERROR_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["isError"]);
static FINALIZATION_REGISTRY_PROTO: Shape = shape(&OBJECT, &[], &["register", "unregister"]);
static ITERATOR_PROTO: Shape = shape(
    &OBJECT,
    &[],
    &[
        "drop", "every", "filter", "find", "flatMap", "forEach", "map", "reduce", "some", "take",
        "toArray",
    ],
);
static ITERATOR_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["concat", "from", "zip", "zipKeyed"]);
static MAP_PROTO: Shape = shape(
    &OBJECT,
    &["size"],
    &[
        "clear",
        "delete",
        "entries",
        "forEach",
        "get",
        "getOrInsert",
        "getOrInsertComputed",
        "has",
        "keys",
        "set",
        "values",
    ],
);
static MAP_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["groupBy"]);
static NUMBER_PROTO: Shape = shape(&OBJECT, &[], &["toExponential", "toFixed", "toPrecision"]);
static NUMBER_STATIC: Shape = shape(
    &CONSTRUCTOR,
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
    &[
        "isFinite",
        "isInteger",
        "isNaN",
        "isSafeInteger",
        "parseFloat",
        "parseInt",
    ],
);
static OBJECT_STATIC: Shape = shape(
    &CONSTRUCTOR,
    &[],
    &[
        "assign",
        "create",
        "defineProperties",
        "defineProperty",
        "entries",
        "freeze",
        "fromEntries",
        "getOwnPropertyDescriptor",
        "getOwnPropertyDescriptors",
        "getOwnPropertyNames",
        "getOwnPropertySymbols",
        "getPrototypeOf",
        "groupBy",
        "hasOwn",
        "is",
        "isExtensible",
        "isFrozen",
        "isSealed",
        "keys",
        "preventExtensions",
        "seal",
        "setPrototypeOf",
        "values",
    ],
);
static PROMISE_PROTO: Shape = shape(&OBJECT, &[], &["catch", "finally", "then"]);
static PROMISE_STATIC: Shape = shape(
    &CONSTRUCTOR,
    &[],
    &[
        "all",
        "allSettled",
        "any",
        "race",
        "reject",
        "resolve",
        "try",
        "withResolvers",
    ],
);
static PROXY_STATIC: Shape = shape(&FUNCTION, &[], &["revocable"]);
static REGEXP_PROTO: Shape = shape(
    &OBJECT,
    &[
        "dotAll",
        "flags",
        "global",
        "hasIndices",
        "ignoreCase",
        "multiline",
        "source",
        "sticky",
        "unicode",
        "unicodeSets",
    ],
    &["compile", "exec", "test"],
);
static REGEXP_INSTANCE: Shape = shape(&REGEXP_PROTO, &["lastIndex"], &[]);
static REGEXP_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["escape"]);
static SET_PROTO: Shape = shape(
    &OBJECT,
    &["size"],
    &[
        "add",
        "clear",
        "delete",
        "difference",
        "entries",
        "forEach",
        "has",
        "intersection",
        "isDisjointFrom",
        "isSubsetOf",
        "isSupersetOf",
        "keys",
        "symmetricDifference",
        "union",
        "values",
    ],
);
static SHARED_ARRAY_BUFFER_PROTO: Shape = shape(
    &OBJECT,
    &["byteLength", "growable", "maxByteLength"],
    &["grow", "slice"],
);
static STRING_PROTO: Shape = shape(
    &OBJECT,
    &["length"],
    &[
        "at",
        "anchor",
        "big",
        "blink",
        "bold",
        "charAt",
        "charCodeAt",
        "codePointAt",
        "concat",
        "endsWith",
        "fixed",
        "fontcolor",
        "fontsize",
        "includes",
        "indexOf",
        "isWellFormed",
        "italics",
        "lastIndexOf",
        "link",
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
        "small",
        "split",
        "startsWith",
        "strike",
        "sub",
        "substr",
        "substring",
        "sup",
        "toLocaleLowerCase",
        "toLocaleUpperCase",
        "toLowerCase",
        "toUpperCase",
        "toWellFormed",
        "trim",
        "trimEnd",
        "trimLeft",
        "trimRight",
        "trimStart",
    ],
);
static STRING_STATIC: Shape = shape(&CONSTRUCTOR, &[], &["fromCharCode", "fromCodePoint", "raw"]);
static SYMBOL_PROTO: Shape = shape(&OBJECT, &["description"], &[]);
static SYMBOL_STATIC: Shape = shape(
    &CONSTRUCTOR,
    &[
        "asyncDispose",
        "asyncIterator",
        "dispose",
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
    &["for", "keyFor"],
);
static TYPED_ARRAY_PROTO: Shape = shape(
    &OBJECT,
    &[
        "BYTES_PER_ELEMENT",
        "buffer",
        "byteLength",
        "byteOffset",
        "length",
    ],
    &[
        "at",
        "copyWithin",
        "entries",
        "every",
        "fill",
        "filter",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "forEach",
        "includes",
        "indexOf",
        "join",
        "keys",
        "lastIndexOf",
        "map",
        "reduce",
        "reduceRight",
        "reverse",
        "set",
        "slice",
        "some",
        "sort",
        "subarray",
        "toReversed",
        "toSorted",
        "values",
        "with",
    ],
);
static TYPED_ARRAY_STATIC: Shape = shape(&CONSTRUCTOR, &["BYTES_PER_ELEMENT"], &["from", "of"]);
static UINT8_ARRAY_PROTO: Shape = shape(
    &TYPED_ARRAY_PROTO,
    &[],
    &["setFromBase64", "setFromHex", "toBase64", "toHex"],
);
static UINT8_ARRAY_STATIC: Shape = shape(&TYPED_ARRAY_STATIC, &[], &["fromBase64", "fromHex"]);
static URL_PROTO: Shape = shape(
    &OBJECT,
    &[
        "hash",
        "host",
        "hostname",
        "href",
        "origin",
        "password",
        "pathname",
        "port",
        "protocol",
        "search",
        "searchParams",
        "username",
    ],
    &["toJSON"],
);
static URL_STATIC: Shape = shape(
    &CONSTRUCTOR,
    &[],
    &["canParse", "createObjectURL", "parse", "revokeObjectURL"],
);
static URL_SEARCH_PARAMS_PROTO: Shape = shape(
    &OBJECT,
    &["size"],
    &[
        "append", "delete", "entries", "forEach", "get", "getAll", "has", "keys", "set", "sort",
        "values",
    ],
);
static WEAK_MAP_PROTO: Shape = shape(
    &OBJECT,
    &[],
    &[
        "delete",
        "get",
        "getOrInsert",
        "getOrInsertComputed",
        "has",
        "set",
    ],
);
static WEAK_SET_PROTO: Shape = shape(&OBJECT, &[], &["add", "delete", "has"]);
static WEAK_REF_PROTO: Shape = shape(&OBJECT, &[], &["deref"]);
static ATOMICS: Shape = shape(
    &OBJECT,
    &[],
    &[
        "add",
        "and",
        "compareExchange",
        "exchange",
        "isLockFree",
        "load",
        "notify",
        "or",
        "pause",
        "store",
        "sub",
        "wait",
        "waitAsync",
        "xor",
    ],
);
static JSON: Shape = shape(
    &OBJECT,
    &[],
    &["isRawJSON", "parse", "rawJSON", "stringify"],
);
static MATH: Shape = shape(
    &OBJECT,
    &[
        "E", "LN10", "LN2", "LOG10E", "LOG2E", "PI", "SQRT1_2", "SQRT2",
    ],
    &[
        "abs",
        "acos",
        "acosh",
        "asin",
        "asinh",
        "atan",
        "atan2",
        "atanh",
        "cbrt",
        "ceil",
        "clz32",
        "cos",
        "cosh",
        "exp",
        "expm1",
        "f16round",
        "floor",
        "fround",
        "hypot",
        "imul",
        "log",
        "log10",
        "log1p",
        "log2",
        "max",
        "min",
        "pow",
        "random",
        "round",
        "sign",
        "sin",
        "sinh",
        "sqrt",
        "sumPrecise",
        "tan",
        "tanh",
        "trunc",
    ],
);
static REFLECT: Shape = shape(
    &OBJECT,
    &[],
    &[
        "apply",
        "construct",
        "defineProperty",
        "deleteProperty",
        "get",
        "getOwnPropertyDescriptor",
        "getPrototypeOf",
        "has",
        "isExtensible",
        "ownKeys",
        "preventExtensions",
        "set",
        "setPrototypeOf",
    ],
);

/// `(instance, prototype, static)` shapes of a global built-in.
type Builtin = (
    Option<&'static Shape>,
    Option<&'static Shape>,
    &'static Shape,
);

fn builtin(name: &str) -> Option<Builtin> {
    let both = |s: &'static Shape, st: &'static Shape| Some((Some(s), Some(s), st));
    match name {
        "AggregateError" => Some((
            Some(&AGGREGATE_ERROR_INSTANCE),
            Some(&ERROR_PROTO),
            &ERROR_STATIC,
        )),
        "Error" | "EvalError" | "RangeError" | "ReferenceError" | "SyntaxError" | "TypeError"
        | "URIError" => Some((Some(&ERROR_INSTANCE), Some(&ERROR_PROTO), &ERROR_STATIC)),
        "Array" => both(&ARRAY_PROTO, &ARRAY_STATIC),
        "ArrayBuffer" => both(&ARRAY_BUFFER_PROTO, &ARRAY_BUFFER_STATIC),
        "BigInt" => both(&OBJECT, &BIGINT_STATIC),
        "Boolean" => both(&OBJECT, &CONSTRUCTOR),
        "DataView" => both(&DATA_VIEW_PROTO, &CONSTRUCTOR),
        "Date" => both(&DATE_PROTO, &DATE_STATIC),
        "FinalizationRegistry" => both(&FINALIZATION_REGISTRY_PROTO, &CONSTRUCTOR),
        "Function" => Some((Some(&CONSTRUCTOR), Some(&FUNCTION), &CONSTRUCTOR)),
        "Iterator" => both(&ITERATOR_PROTO, &ITERATOR_STATIC),
        "Map" => both(&MAP_PROTO, &MAP_STATIC),
        "Number" => both(&NUMBER_PROTO, &NUMBER_STATIC),
        "Object" => both(&OBJECT, &OBJECT_STATIC),
        "Promise" => both(&PROMISE_PROTO, &PROMISE_STATIC),
        "Proxy" => Some((None, None, &PROXY_STATIC)),
        "RegExp" => Some((Some(&REGEXP_INSTANCE), Some(&REGEXP_PROTO), &REGEXP_STATIC)),
        "Set" => both(&SET_PROTO, &CONSTRUCTOR),
        "SharedArrayBuffer" => both(&SHARED_ARRAY_BUFFER_PROTO, &CONSTRUCTOR),
        "String" => both(&STRING_PROTO, &STRING_STATIC),
        "Symbol" => both(&SYMBOL_PROTO, &SYMBOL_STATIC),
        "URL" => both(&URL_PROTO, &URL_STATIC),
        "URLSearchParams" => both(&URL_SEARCH_PARAMS_PROTO, &CONSTRUCTOR),
        "WeakMap" => both(&WEAK_MAP_PROTO, &CONSTRUCTOR),
        "WeakSet" => both(&WEAK_SET_PROTO, &CONSTRUCTOR),
        "WeakRef" => both(&WEAK_REF_PROTO, &CONSTRUCTOR),
        "Uint8Array" => both(&UINT8_ARRAY_PROTO, &UINT8_ARRAY_STATIC),
        _ if is_typed_array(name) => both(&TYPED_ARRAY_PROTO, &TYPED_ARRAY_STATIC),
        "Atomics" => Some((None, None, &ATOMICS)),
        "JSON" => Some((None, None, &JSON)),
        "Math" => Some((None, None, &MATH)),
        "Reflect" => Some((None, None, &REFLECT)),
        _ => None,
    }
}

fn is_typed_array(name: &str) -> bool {
    matches!(
        name,
        "Int8Array"
            | "Uint8Array"
            | "Uint8ClampedArray"
            | "Int16Array"
            | "Uint16Array"
            | "Int32Array"
            | "Uint32Array"
            | "Float16Array"
            | "Float32Array"
            | "Float64Array"
            | "BigInt64Array"
            | "BigUint64Array"
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Usage {
    Static,
    Prototype,
    Instance,
}

/// Skips parentheses and TS-only wrappers.
fn unwrap(ast: &Ast, mut id: NodeId) -> NodeId {
    for _ in 0..1000 {
        match *ast.node(id) {
            Node::Paren { expr }
            | Node::As { expr, .. }
            | Node::Satisfies { expr, .. }
            | Node::NonNull { expr }
            | Node::TypeAssertion { expr, .. }
            | Node::Instantiation { expr, .. } => id = expr,
            _ => break,
        }
    }
    id
}

/// The name of a literal key, as `String(value)` gives it; names held in variables are not
/// followed.
fn literal_key(ast: &Ast, id: NodeId, depth: u32) -> Option<String> {
    if depth > 50 {
        return None;
    }
    match *ast.node(id) {
        Node::Paren { expr } => literal_key(ast, expr, depth + 1),
        Node::StringLit { value } => Some(ast.atom(value).to_owned()),
        Node::NumberLit { value } => Some(number_to_string(value.get())),
        Node::BigIntLit { value } => {
            let text = ast.atom(value);
            Some(match text.parse::<i128>() {
                Ok(n) => n.to_string(),
                Err(_) => text.to_owned(),
            })
        }
        Node::BoolLit { value } => Some(value.to_string()),
        Node::NullLit {} => Some("null".into()),
        Node::TemplateLit { parts } => match ast.list(parts) {
            &[only] => match *ast.node(only) {
                Node::TemplateElement {
                    cooked,
                    invalid_escape: false,
                } => Some(ast.atom(cooked).to_owned()),
                _ => None,
            },
            _ => None,
        },
        Node::Unary {
            op: UnaryOp::Minus,
            arg,
        } => match *ast.node(skip_parens(ast, arg)) {
            Node::NumberLit { value } => Some(number_to_string(-value.get())),
            _ => None,
        },
        Node::Binary {
            left,
            op: BinaryOp::Add,
            right,
        } if is_string_lit(ast, left) || is_string_lit(ast, right) => {
            Some(literal_key(ast, left, depth + 1)? + &literal_key(ast, right, depth + 1)?)
        }
        _ => None,
    }
}

fn is_string_lit(ast: &Ast, id: NodeId) -> bool {
    matches!(
        ast.node(skip_parens(ast, id)),
        Node::StringLit { .. } | Node::TemplateLit { .. } | Node::Binary { .. }
    )
}

fn property_name(cx: &Ctx<'_, '_>, member: NodeId) -> Option<String> {
    let ast = cx.ast();
    match *ast.node(member) {
        Node::Member { prop, .. } => match *ast.node(prop) {
            Node::Ident { name } => Some(ast.atom(name).to_owned()),
            _ => None,
        },
        Node::Index { index, .. } => literal_key(ast, index, 0),
        _ => None,
    }
}

fn global_object(cx: &Ctx<'_, '_>, id: NodeId) -> bool {
    let id = unwrap(cx.ast(), id);
    ["globalThis", "global", "self", "window"]
        .iter()
        .any(|n| is_global_ident(cx, id, n))
}

/// `Array`, `globalThis.Array`, ...: the name of the built-in `id` refers to.
fn builtin_reference(cx: &Ctx<'_, '_>, id: NodeId) -> Option<String> {
    let ast = cx.ast();
    let id = unwrap(ast, id);
    match *ast.node(id) {
        Node::Ident { name } => {
            let name = ast.atom(name);
            (builtin(name).is_some() && is_global_ident(cx, id, name)).then(|| name.to_owned())
        }
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            let name = property_name(cx, id)?;
            (builtin(&name).is_some() && global_object(cx, obj)).then_some(name)
        }
        _ => None,
    }
}

fn literal_type(ast: &Ast, id: NodeId) -> Option<&'static str> {
    Some(match ast.node(id) {
        Node::RegexLit { .. } => "RegExp",
        Node::BoolLit { .. } => "Boolean",
        Node::NumberLit { .. } => "Number",
        Node::StringLit { .. } => "String",
        Node::BigIntLit { .. } => "BigInt",
        _ => return None,
    })
}

fn receiver(cx: &Ctx<'_, '_>, id: NodeId) -> Option<(String, Usage)> {
    let ast = cx.ast();
    let id = unwrap(ast, id);
    match *ast.node(id) {
        Node::Member { obj, .. } | Node::Index { obj, .. } => {
            if property_name(cx, id).as_deref() == Some("prototype")
                && let Some(name) = builtin_reference(cx, obj)
            {
                return Some((name, Usage::Prototype));
            }
            builtin_reference(cx, id).map(|n| (n, Usage::Static))
        }
        Node::Ident { .. } => builtin_reference(cx, id).map(|n| (n, Usage::Static)),
        Node::New { callee, .. } => builtin_reference(cx, callee).map(|n| (n, Usage::Instance)),
        Node::Unary {
            op: UnaryOp::Minus | UnaryOp::Plus,
            arg,
        } => match literal_type(ast, unwrap(ast, arg)) {
            Some(t @ ("Number" | "BigInt")) => Some((t.to_owned(), Usage::Instance)),
            _ => None,
        },
        Node::ArrayLit { .. } => Some(("Array".into(), Usage::Instance)),
        Node::TemplateLit { .. } => Some(("String".into(), Usage::Instance)),
        _ => literal_type(ast, id).map(|t| (t.to_owned(), Usage::Instance)),
    }
}

fn is_array_index(s: &str) -> bool {
    let canonical = s == "0" || (!s.starts_with('0') && !s.is_empty());
    canonical
        && s.bytes().all(|b| b.is_ascii_digit())
        && s.parse::<u64>().is_ok_and(|n| n <= (1 << 32) - 2)
}

fn is_canonical_numeric(s: &str) -> bool {
    s == "-0" || number_to_string(super::unicorn_static::string_to_number(s)) == s
}

fn is_indexed_access(type_name: &str, usage: Usage, name: &str) -> bool {
    if usage != Usage::Instance {
        return false;
    }
    if matches!(type_name, "Array" | "String") {
        return is_array_index(name);
    }
    is_typed_array(type_name) && is_canonical_numeric(name)
}

#[derive(Clone, Copy)]
enum CallKind {
    Call,
    Construct,
}

fn call_kind(cx: &Ctx<'_, '_>, member: NodeId) -> Option<CallKind> {
    let ast = cx.ast();
    let mut cur = member;
    let mut parent = cx.parent(cur)?;
    for _ in 0..1000 {
        match *ast.node(parent) {
            Node::Paren { .. }
            | Node::As { .. }
            | Node::Satisfies { .. }
            | Node::NonNull { .. }
            | Node::TypeAssertion { .. }
            | Node::Instantiation { .. } => {
                cur = parent;
                parent = cx.parent(cur)?;
            }
            _ => break,
        }
    }
    match *ast.node(parent) {
        Node::Call { callee, .. } if callee == cur => Some(CallKind::Call),
        Node::TaggedTemplate { tag, .. } if tag == cur => Some(CallKind::Call),
        Node::New { callee, .. } if callee == cur => Some(CallKind::Construct),
        Node::ExprWithTypeArgs { expr, .. } if expr == cur => {
            let clause = cx.parent(parent)?;
            matches!(
                *ast.node(clause),
                Node::HeritageClause {
                    kind: HeritageKind::Extends,
                    ..
                }
            )
            .then_some(CallKind::Construct)
            .filter(|_| {
                cx.parent(clause).is_some_and(|c| {
                    matches!(ast.node(c), Node::ClassDecl { .. } | Node::ClassExpr { .. })
                })
            })
        }
        _ => None,
    }
}

impl Rule for NoNonstandardBuiltinProperties {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Member, NodeKind::Index]
    }

    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::Member { obj, .. } | Node::Index { obj, .. }) = *ast.node(node) else {
            return;
        };
        let Some(name) = property_name(cx, node) else {
            return;
        };
        let Some((type_name, usage)) = receiver(cx, obj) else {
            return;
        };
        let Some((instance, prototype, statics)) = builtin(&type_name) else {
            return;
        };
        let shape = match usage {
            Usage::Static => statics,
            Usage::Prototype => match prototype {
                Some(s) => s,
                None => return,
            },
            Usage::Instance => match instance {
                Some(s) => s,
                None => return,
            },
        };
        if is_indexed_access(&type_name, usage, &name) {
            return;
        }
        let receiver = match usage {
            Usage::Static => type_name.clone(),
            Usage::Prototype => format!("{type_name}.prototype"),
            Usage::Instance => format!("{type_name} instances"),
        };
        let (id, what) = if !shape.has(&name) {
            ("no-nonstandard-builtin-properties/nonstandard", "standard")
        } else if name == "constructor" {
            return;
        } else {
            match call_kind(cx, node) {
                None => return,
                Some(CallKind::Call) if shape.callable(&name) => return,
                Some(CallKind::Call) => {
                    ("no-nonstandard-builtin-properties/noncallable", "callable")
                }
                Some(CallKind::Construct) => (
                    "no-nonstandard-builtin-properties/nonconstructible",
                    "constructible",
                ),
            }
        };
        let msg = format!("`{name}` is not a {what} property of `{receiver}`.");
        cx.report_node(node, msg).id(id);
    }
}
