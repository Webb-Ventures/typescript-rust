//! `unicorn/new-for-builtins`: construct built-ins with `new` (`new Map()`, `new
//! Intl.Collator()`, `new Error()`), and call the primitive wrappers without it
//! (`String(x)`, `BigInt(1)`, `Symbol()`). `Temporal.Now`, `WebAssembly` and
//! `WebAssembly.JSTag` are neither functions nor constructors.
//!
//! Globals are followed as ESLint's reference tracker does (see
//! [`super::unicorn_util::global_path`]): through `globalThis`/`global`/`self`/`window`,
//! destructuring and aliases; a global the file declares or writes is not a built-in.
//! Optional calls (`Array?.()`) are left alone. `Object(x) === x` is allowed. `Date()` is
//! `String(new Date())` (a fix without arguments or comments, a suggestion otherwise).
//! `new String/Number/Boolean` are reported without a fix (dropping `new` changes the
//! value's type).

use super::prelude::*;
use super::unicorn_util::{
    call_to_new, global_path, has_optional_chain, new_to_call, space_around_keyword, with_edits,
};

pub static RULE: RuleDef = RuleDef {
    meta: meta(
        "unicorn/new-for-builtins",
        "Enforce the use of `new` for all builtins, except `String`, `Number`, `Boolean`, `Symbol` and `BigInt`.",
        NO_PRESET,
        Category::Style,
    )
    .fixable()
    .suggestions(),
    create: create_default::<NewForBuiltins>,
};

const ENFORCE_NEW: &[&str] = &[
    "Object",
    "Array",
    "ArrayBuffer",
    "DataView",
    "Date",
    "Function",
    "Map",
    "WeakMap",
    "Set",
    "WeakSet",
    "Promise",
    "RegExp",
    "SharedArrayBuffer",
    "Proxy",
    "WeakRef",
    "FinalizationRegistry",
    "DisposableStack",
    "AsyncDisposableStack",
    "Error",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "AggregateError",
    "SuppressedError",
    "Intl.Collator",
    "Intl.DateTimeFormat",
    "Intl.DisplayNames",
    "Intl.DurationFormat",
    "Intl.ListFormat",
    "Intl.Locale",
    "Intl.NumberFormat",
    "Intl.PluralRules",
    "Intl.RelativeTimeFormat",
    "Intl.Segmenter",
    "Temporal.Duration",
    "Temporal.Instant",
    "Temporal.PlainDate",
    "Temporal.PlainDateTime",
    "Temporal.PlainMonthDay",
    "Temporal.PlainTime",
    "Temporal.PlainYearMonth",
    "Temporal.ZonedDateTime",
    "WebAssembly.Module",
    "WebAssembly.Instance",
    "WebAssembly.Memory",
    "WebAssembly.Table",
    "WebAssembly.Global",
    "WebAssembly.Tag",
    "WebAssembly.Exception",
    "WebAssembly.CompileError",
    "WebAssembly.LinkError",
    "WebAssembly.RuntimeError",
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

const DISALLOW_NEW: &[&str] = &["BigInt", "Boolean", "Number", "String", "Symbol"];

const NEITHER: &[&str] = &["Temporal.Now", "WebAssembly", "WebAssembly.JSTag"];

#[derive(Debug, Default)]
struct NewForBuiltins;

impl Rule for NewForBuiltins {
    fn kinds(&self) -> &'static [NodeKind] {
        &[NodeKind::Call, NodeKind::New]
    }
    fn enter(&self, node: NodeId, cx: &mut Ctx<'_, '_>) {
        let ast = cx.ast();
        let (Node::Call { callee, .. } | Node::New { callee, .. }) = *cx.node(node) else {
            return;
        };
        let Some(name) = global_path(cx, callee) else {
            return;
        };
        let is_new = matches!(cx.node(node), Node::New { .. });
        if NEITHER.contains(&name.as_str()) {
            cx.report_node(node, format!("`{name}` is not a function or constructor."))
                .id("disallowCallOrNew");
            return;
        }
        if is_new {
            if !DISALLOW_NEW.contains(&name.as_str()) {
                return;
            }
            let fix = (!matches!(name.as_str(), "String" | "Boolean" | "Number"))
                .then(|| new_to_call(cx, node));
            cx.report_node(node, format!("Use `{name}()` instead of `new {name}()`."))
                .id("disallow")
                .fix_opt(fix);
            return;
        }
        if !ENFORCE_NEW.contains(&name.as_str()) || has_optional_chain(ast, node) {
            return;
        }
        match name.as_str() {
            "Object" => {
                if let Some(p) = parent_skip_parens(ast, cx.file.parents, node)
                    && let Node::Binary {
                        op: BinaryOp::StrictEq | BinaryOp::StrictNotEq,
                        ..
                    } = *ast.node(p)
                {
                    return;
                }
            }
            "Date" => {
                let span = cx.span(node);
                let fix = with_edits(
                    Fix::replace(span, "String(new Date())"),
                    space_around_keyword(cx, node),
                );
                let Node::Call { args, .. } = *ast.node(node) else {
                    return;
                };
                let plain = ast.comments_in(span).is_empty() && ast.list(args).is_empty();
                let report = cx
                    .report(span, "Use `String(new Date())` instead of `Date()`.")
                    .id("error-date");
                if plain {
                    report.fix(fix);
                } else {
                    report.suggest("suggestion-date", "Switch to `String(new Date())`.", fix);
                }
                return;
            }
            _ => {}
        }
        let fix = call_to_new(cx, node);
        cx.report_node(node, format!("Use `new {name}()` instead of `{name}()`."))
            .id("enforce")
            .fix(fix);
    }
}
