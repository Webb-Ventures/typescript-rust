//! Built-in ECMAScript globals, by `ecmaVersion`, as ESLint defines them for flat config
//! (observed from ESLint 10's configuration data; all read-only).

use crate::env::{GlobalAccess, Globals};

/// `ecmaVersion: "latest"`.
pub const LATEST_ECMA_VERSION: u32 = 2026;

const ES3: &[&str] = &[
    "Array",
    "Boolean",
    "constructor",
    "Date",
    "decodeURI",
    "decodeURIComponent",
    "encodeURI",
    "encodeURIComponent",
    "Error",
    "escape",
    "eval",
    "EvalError",
    "Function",
    "hasOwnProperty",
    "Infinity",
    "isFinite",
    "isNaN",
    "isPrototypeOf",
    "Math",
    "NaN",
    "Number",
    "Object",
    "parseFloat",
    "parseInt",
    "propertyIsEnumerable",
    "RangeError",
    "ReferenceError",
    "RegExp",
    "String",
    "SyntaxError",
    "toLocaleString",
    "toString",
    "TypeError",
    "undefined",
    "unescape",
    "URIError",
    "valueOf",
];

/// (first ecmaVersion, names added).
const BY_VERSION: &[(u32, &[&str])] = &[
    (5, &["JSON"]),
    (
        2015,
        &[
            "ArrayBuffer",
            "DataView",
            "Float32Array",
            "Float64Array",
            "Int16Array",
            "Int32Array",
            "Int8Array",
            "Intl",
            "Map",
            "Promise",
            "Proxy",
            "Reflect",
            "Set",
            "Symbol",
            "Uint16Array",
            "Uint32Array",
            "Uint8Array",
            "Uint8ClampedArray",
            "WeakMap",
            "WeakSet",
        ],
    ),
    (2017, &["Atomics", "SharedArrayBuffer"]),
    (
        2020,
        &["BigInt", "BigInt64Array", "BigUint64Array", "globalThis"],
    ),
    (2021, &["AggregateError", "FinalizationRegistry", "WeakRef"]),
    (2025, &["Float16Array", "Iterator"]),
    (
        2026,
        &[
            "AsyncDisposableStack",
            "DisposableStack",
            "SuppressedError",
            "Temporal",
        ],
    ),
];

/// CommonJS module globals (`sourceType: "commonjs"`).
pub const COMMONJS: &[&str] = &["exports", "global", "module", "require"];

/// Normalises `ecmaVersion` (3, 5, 6..=17, 2015..; `latest`) to a year-style version.
pub fn normalize_ecma_version(v: u32) -> u32 {
    match v {
        3 | 5 => v,
        6..=17 => v + 2009,
        _ if v >= 2015 => v.min(LATEST_ECMA_VERSION),
        _ => LATEST_ECMA_VERSION,
    }
}

/// The built-in globals for an ecmaVersion.
pub fn builtin_globals(ecma_version: u32) -> Globals {
    let v = normalize_ecma_version(ecma_version);
    let mut g = Globals::new();
    for name in ES3 {
        g.insert(name, GlobalAccess::Readonly);
    }
    for (since, names) in BY_VERSION {
        if v >= *since {
            for name in *names {
                g.insert(name, GlobalAccess::Readonly);
            }
        }
    }
    g
}

/// True for the names of built-in ES globals at the latest version.
pub fn is_builtin(name: &str) -> bool {
    ES3.contains(&name) || BY_VERSION.iter().any(|(_, names)| names.contains(&name))
}
