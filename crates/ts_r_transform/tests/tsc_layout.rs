//! `Layout::Tsc` after a transform: each `tests/tsc_layout/<name>.ts` transpiles (options from
//! its `// @option: value` header) byte-for-byte to the `.js` next to it, which is tsc 7's
//! output for the same file.

use std::path::Path;

use ts_r_codegen::{CodegenOptions, Layout};
use ts_r_transform::{TransformOptions, transpile};

mod support;

fn check(name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tsc_layout");
    let path = dir.join(format!("{name}.ts"));
    let src = std::fs::read_to_string(&path).unwrap();
    let want = std::fs::read_to_string(dir.join(format!("{name}.js"))).unwrap();
    let path = path.to_string_lossy().to_string();
    let opts = TransformOptions::from_json(&support::options_json(&src), &path);
    let codegen = CodegenOptions {
        layout: Layout::Tsc,
        ..CodegenOptions::default()
    };
    let got = transpile(&path, &src, &opts, &codegen).code;
    assert_eq!(got, want, "{name}: Layout::Tsc output differs from tsc 7");
}

/// Comments in front of erased code, and after it on its last line, are dropped.
#[test]
fn erased_comments() {
    check("erased_comments");
}

/// A statement a transform wrote in place of an original one (requires, export assignments,
/// enum IIFEs) is led by the original's comments; moved field initializers take none.
#[test]
fn replaced_comments() {
    check("replaced_comments");
}

/// ES2020 lowering below ES2020: optional chains (calls keep `this`, `delete`, parenthesized
/// chains) and `??`, with tsc's hoisted, print-order temporaries.
#[test]
fn es2020_lowering() {
    check("es2020_lowering");
}

/// ES2021 logical assignment, numeric separators, ES2019 optional catch bindings and ES2016
/// `**` below their years.
#[test]
fn operators_lowering() {
    check("operators_lowering");
}

/// ES2017 async functions, arrows and methods below ES2017: `__awaiter`, `arguments`
/// capture, `this` for arrows, parameters moved to the generator.
#[test]
fn async_lowering() {
    check("async_lowering");
}

/// `super` in lowered async methods: `_super` and `_superIndex`, with setters when assigned.
#[test]
fn async_super() {
    check("async_super");
}

/// ES2018 async generators, `for await` and object spread below ES2018.
#[test]
fn async_generators() {
    check("async_generators");
}

/// Async parameter lists, one-line bodies, directives and comments in lowered bodies.
#[test]
fn async_params() {
    check("async_params");
}

/// ES2018 object rest in declarations, parameters, `catch`, `for`-`of` and assignments,
/// `for await` forms and object spread, at ES2017.
#[test]
fn rest_lowering() {
    check("rest_lowering");
}

/// Parameters whose initializers or patterns need temporaries move into the body.
#[test]
fn param_temps() {
    check("param_temps");
}

/// `var`s of a lowered async body that collide with a parameter name are hoisted.
#[test]
fn async_vars() {
    check("async_vars");
}

/// Below ES2022 static initializers and static blocks move after the class, with a class
/// alias where static code uses `this`, `super` or the class name.
#[test]
fn class_statics() {
    check("class_statics");
}

/// `useDefineForClassFields` below ES2022: fields become `Object.defineProperty` calls.
#[test]
fn class_define_fields() {
    check("class_define_fields");
}

/// The TC39 decorator transform's class below ES2022: `var C = _classThis = class {}`.
#[test]
fn class_decorators_es2015() {
    check("class_decorators_es2015");
}

/// `using` below ESNext: the `env`/`try` wrapping, constructors (field initializers stay
/// in front), loops, and the numbering of `env_N`/`e_N` with a `for await`'s names.
#[test]
fn using_lowering() {
    check("using_lowering");
}

/// A module's top-level `using`: declarations hoisted out of the `try`, exports regrouped.
#[test]
fn using_top_level() {
    check("using_top_level");
}

/// At ESNext every `using` stays as written, a top-level one followed by exports too.
#[test]
fn using_esnext() {
    check("using_esnext");
}

/// A decorated anonymous default class under CommonJS takes the module's `default_1`.
#[test]
fn legacy_decorated_default_cjs() {
    check("legacy_decorated_default_cjs");
}

/// Private names below ES2022: `WeakMap`/`WeakSet` storage, method functions, the helper
/// calls for reads, writes, compound and update operators, calls and `#x in o`, aliases.
#[test]
fn private_lowering() {
    check("private_lowering");
}

/// Without useDefineForClassFields, removed fields still evaluate computed keys that may
/// have effects, with the other fields' key captures, in one statement (one sequence item
/// of a class expression).
#[test]
fn removed_field_keys() {
    check("removed_field_keys");
}

#[test]
fn multiline_comments_keep_return_and_throw_operands_parenthesized() {
    ts_r_testing::cap_memory!();
    let codegen = CodegenOptions {
        layout: Layout::Tsc,
        ..CodegenOptions::default()
    };
    for newline in ["\n", "\r", "\r\n", "\u{2028}", "\u{2029}"] {
        for expr in ["1 as number", "1 satisfies number", "<number>1", "value!"] {
            for keyword in ["return", "throw"] {
                let src =
                    format!("function f(value: number) {{ {keyword} (/*{newline}*/ {expr}); }}");
                let out = transpile("a.ts", &src, &TransformOptions::default(), &codegen);
                assert!(!out.has_errors(), "{src:?}: {:?}", out.diagnostics);
                assert!(out.code.contains(&format!("{keyword} (")), "{}", out.code);
                let parsed = ts_r_parser::parse(&out.code, ts_r_ast::SourceKind::Js);
                assert!(parsed.diagnostics.is_empty(), "{}", out.code);
            }
        }
    }
}

/// Static fields and blocks of a class-decorated derived class read `super` through
/// `Reflect.get(_classSuper, k, _classThis)` (calls through `.call`, tags through `.bind`);
/// an anonymous heritage is `(0, e)`, and static blocks alone don't name the class.
#[test]
fn es_decorated_statics() {
    check("es_decorated_statics");
}
