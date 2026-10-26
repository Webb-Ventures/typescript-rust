//! Unit tests per construct. Each `ok_*` test asserts the snippet parses without diagnostics;
//! structure checks look for fragments of the dump.

// Real-world or generated input: abort instead of exhausting the machine's memory.
ts_r_testing::cap_memory!();

use ts_r_ast::{Modifiers, NodeKind, SourceKind};

use crate::parse;
use ts_r_diagnostics::Diagnostic;

#[track_caller]
fn ok_kind(src: &str, kind: SourceKind) -> String {
    let r = parse(src, kind);
    assert!(
        r.diagnostics.is_empty(),
        "unexpected diagnostics for {src:?}:\n{:#?}\n{}",
        r.diagnostics,
        r.ast.dump()
    );
    r.ast.dump()
}

#[track_caller]
fn ok(src: &str) -> String {
    ok_kind(src, SourceKind::Ts)
}

#[track_caller]
fn ok_tsx(src: &str) -> String {
    ok_kind(src, SourceKind::Tsx)
}

#[track_caller]
fn ok_js(src: &str) -> String {
    ok_kind(src, SourceKind::Js)
}

#[track_caller]
fn err_codes(src: &str, kind: SourceKind) -> Vec<u32> {
    parse(src, kind)
        .diagnostics
        .iter()
        .map(|d| d.code)
        .collect()
}

#[track_caller]
fn has(dump: &str, fragment: &str) {
    assert!(dump.contains(fragment), "missing {fragment:?} in:\n{dump}");
}

/// Kinds of the top-level statements.
fn stmt_kinds(src: &str, kind: SourceKind) -> Vec<NodeKind> {
    let r = parse(src, kind);
    let ts_r_ast::Node::Program { stmts } = *r.ast.node(r.ast.root) else {
        panic!()
    };
    r.ast.list(stmts).iter().map(|&s| r.ast.kind(s)).collect()
}

// ---- Statements ----

#[test]
fn statements() {
    ok("if (a) b(); else if (c) d; else {}");
    ok("for (;;) {} for (let i = 0; i < n; i++) {} for (x in y); for (const [a, b] of c) {}");
    ok("async function f() { for await (const x of y) {} }");
    ok("while (x) x--; do x++; while (x < 10) foo();");
    ok("switch (x) { case 1: case 2: f(); break; default: g() }");
    ok("try { a() } catch { } try {} catch (e) {} finally {} try {} catch ({ message }) {}");
    ok("try {} catch (e: unknown) {}");
    ok("label: for (;;) { continue label; break label; }");
    ok("with (obj) {} debugger; ;;");
    ok("throw new Error('x');");
    ok("function f() { return; } function g() { return 1 }");
    let d = ok("'use strict'; \"use client\"\nfoo();");
    has(&d, "stmts[0]: Directive 0..13 value=\"use strict\"");
    has(&d, "stmts[1]: Directive");
    let d = ok("'not directive'.length;");
    has(&d, "ExprStmt");
    // A newline doesn't end the string's expression when the next token continues it.
    for src in [
        "'hello'\n.toUpperCase();",
        "'hello'\n[0];",
        "function f() { 'a'\n('b') }",
        "'a'\n+ 1",
        "'a'\n`t`",
    ] {
        let d = ok(src);
        assert!(!d.contains("Directive"), "{src:?}:\n{d}");
    }
    let d = ok("function f() { 'use strict'\n'x'\nreturn }");
    assert_eq!(d.matches("Directive").count(), 2, "{d}");
}

/// A top-level `await` before a line break: a name in a script, an operator in a module
/// (topLevelAwait.1).
#[test]
fn top_level_await_before_newline() {
    let d = ok("let y = await\n 1;");
    has(&d, "init: Ident 8..13");
    let d = ok("export {};\nlet y = await\n 1;");
    has(&d, "init: Await 19..27");
}

#[test]
fn asi() {
    let d = ok("a\n++b");
    has(&d, "Update 2..5 prefix");
    let d = ok("function f() { return\n1 }");
    has(&d, "Return 15..21");
    ok("let a = 1\nlet b = 2\nconst c = a\n(b)");
    ok("x\n/re/g.test(y)");
    ok("do {} while (false) foo()");
    ok("throw err\n");
    ok("var x = y\n[1, 2].forEach(f)");
    // A lone name before a same-line token is TS1434 at the name (commonMissingSemicolons);
    // anything else is `';' expected` at the token.
    assert_eq!(err_codes("a b", SourceKind::Ts), [1434]);
    assert_eq!(err_codes("a.b c", SourceKind::Ts), [1005]);
}

#[test]
fn variables() {
    ok("var a; let b = 1, c; const d = 2;");
    ok("let { a, b: { c }, d = 1, ...rest } = o; let [x, , y = 2, ...z] = arr;");
    ok("let x!: number; let y: string = 'a';");
    ok("using res = open(); { await using r2 = open(); }");
    ok("let = 1; let.x; var let;");
    ok("for (using x of y) {} for (await using x of y) {}");
    let d = ok("using x = f();");
    has(&d, "VarDecl 0..14 kind=Using");
    ok("using\nx = 1;");
    let k = stmt_kinds("let\nx = 1", SourceKind::Ts);
    assert_eq!(k, [NodeKind::VarDecl]);
}

#[test]
fn functions() {
    ok("function f<T>(a: T, b?: number, ...rest: string[]): T { return a }");
    ok("function f(this: Window, { a, b }: Opts = {}, [c]: [number]) {}");
    ok("function* g() { yield; yield 1; yield* other(); const x = yield; }");
    ok("async function f() { await x; await (async () => {})(); }");
    ok("function f(a: string): void; function f(a: any) {}");
    ok("export default function () {} ");
    ok("export default async function* named() {}");
    let d = ok("function f(): asserts x is string {} function g(x): x is number { return true }");
    has(&d, "TypePredicate");
    has(&d, "asserts");
    ok("function f(): asserts this {} function g(): this is Foo { return true }");
}

#[test]
fn arrows() {
    ok("const a = () => {}; const b = x => x * 2; const c = async x => await x;");
    ok("const d = async (a, b) => a + b; const e = (a: number, b = 2, ...c: any[]): number => a;");
    ok("const f = ({ a, b }) => a; const g = ([a]) => a; const h = () => ({ a: 1 });");
    ok("const i = <T>(x: T) => x; const j = async <T>(x: T): Promise<T> => x;");
    ok("const k = (x): x is string => typeof x === 'string';");
    // A parameter named `async` (arrowFunctionWithParameterNameAsync_es6).
    ok("const l = async => async;");
    // An optional call after an instantiation is no property access
    // (optionalChainWithInstantiationExpression2).
    ok("a?.<b>();\na<b>?.();");
    ok("f(() => 1, (a) => a, async () => {});");
    let d = ok("x = a ? (b) : c => d;");
    has(&d, "cons: Paren");
    has(&d, "alt: Arrow");
    let d = ok("x = a ? (b): c => d : e;");
    has(&d, "cons: Arrow");
    has(&d, "return_type: TypeRef");
    ok("x = a ? b => c : d => e;");
    ok("x = (a, b);");
    ok("x = (a = 1);");
    ok("x = ((a));");
    ok("x = (a) + (b);");
    ok("x = async;");
    ok("x = async(1, 2);");
    ok("x = (a?: number) => a;");
}

#[test]
fn deeply_nested_parens_are_linear() {
    let mut src = String::from("x = ");
    for _ in 0..300 {
        src.push_str("(a = ");
    }
    src.push('1');
    for _ in 0..300 {
        src.push(')');
    }
    let t = std::time::Instant::now();
    ok(&src);
    assert!(t.elapsed().as_secs() < 5);
}

#[test]
fn classes() {
    let src = r#"
@Component({ selector: 'x' })
export class A<T> extends Base<T> implements I, J<T> {
    static #count = 0;
    #priv: string;
    declare readonly d: number;
    public static override x?: number = 1;
    protected abstract y: string;
    accessor z = 1;
    static accessor w: number;
    [key: string]: any;
    static [k: number]: string;
    constructor(private readonly a: string, @Inject(TOKEN) public b?: number) { super(); }
    get value(): T { return this.v }
    set value(v: T) {}
    static async *gen() {}
    async method<U>(this: A<T>, u: U): Promise<void> {}
    optional?(): void;
    definite!: string;
    ['computed']() {}
    'quoted'() {}
    42() {}
    #privMethod() { return #priv in this; }
    static { init(); }
    get() {}
    set() {}
    static() {}
    async() {}
    get get() { return 1 }
    declare: number;
    readonly;
    abstract;
    overload(a: string): void;
    overload(a: any) {}
}
"#;
    let d = ok(src);
    has(&d, "StaticBlock");
    has(&d, "kind=Constructor");
    has(&d, "modifiers=[private readonly]");
    has(&d, "modifiers=[accessor]");
    has(&d, "IndexSignature");
    ok("abstract class A { abstract m(): void; protected abstract p: string; }");
    ok("export default class {} ");
    ok("export default abstract class X {}");
    ok("const C = class Named<T> extends (foo as any) {};");
    ok("class A extends mixin(B, C) {}");
    ok("class A { m() {} ; ; n() {} }");
    ok("class A { 'constructor'() {} }");
    ok("class A<in out T, const U> {}");
}

/// Modifiers where none is allowed parse as modifiers, without syntax errors: the checker's
/// grammar pass reports them (TS1044, TS1090, TS1042, ...), as in tsc 7.
#[test]
fn misplaced_modifiers() {
    has(
        &ok("namespace Y { public class A {} }"),
        "ClassDecl 14..31 modifiers=[public]",
    );
    has(
        &ok("function g(static x) {}"),
        "Param 11..19 modifiers=[static]",
    );
    has(&ok_js("function g(public x) {}"), "modifiers=[public]");
    has(
        &ok("const o = { static m() {} };"),
        "MethodDef 12..25 modifiers=[static]",
    );
    has(
        &ok("const o = { public a: 1, readonly b, async c: 2 };"),
        "Property 12..23 modifiers=[public]",
    );
    has(
        &ok("const o = { public a: 1, readonly b, async c: 2 };"),
        "ShorthandProp 25..35 modifiers=[readonly]",
    );
    has(
        &ok("export static var v = 1;"),
        "VarDecl 0..24 modifiers=[export static]",
    );
    // `export default` after a modifier is an export, not `export` as a modifier
    // (autoAccessorDisallowedModifiers).
    has(
        &ok("var v;\naccessor export default v;"),
        "ExportDefaultExpr 7..33",
    );
    has(
        &ok("interface I { public m(): void; static x: number }"),
        "MethodSignature 14..31 modifiers=[public]",
    );
    has(
        &ok("public declare abstract class A {}"),
        "modifiers=[declare abstract public]",
    );
    for src in [
        "static var v = 1;",
        "static\nvar v = 1;",
        "export public class A {}",
        "async class A {}",
        "readonly interface I {}",
        "accessor var x;",
        "public import x = require('y');",
        "public export var x;",
        "public type T = 1;",
        "public namespace N {}",
        "function f() { public var x; }",
        "const enum E {} public const enum F {}",
        "class C { in y; out z; export w; async p = 1; }",
        "class C { async constructor() {} }",
        "class C { public static {} readonly private static {} async static {} }",
        "type T = { export x: 1; private m(): void };",
        "function f<public T, const U>() {}",
        "let f = (static x) => x; type F = (public x: number) => void;",
        // `export` on a parameter (request R7; tsc 7: TS1090 from the checker only).
        "function f(export e) {}",
        "function f(export\ne) {}",
        "function f(export public e, export ...r) {}",
        "function f(export [a], export async) {}",
        "class C { constructor(export e) {} }",
        "let a = (export e) => e; type F = (export e) => void;",
    ] {
        ok(src);
    }
    has(
        &ok("function f(export e) {}"),
        "Param 11..19 modifiers=[export]",
    );
    // `const` on a class member: TS1248 at the name, a grammar error tsc reports only in a
    // file without syntax errors (constInClassExpression).
    assert_eq!(
        err_codes("class C { const x = 1; }", SourceKind::Ts),
        [1248]
    );
    assert_eq!(
        err_codes("class C { const x = 1; } )", SourceKind::Ts),
        [1128]
    );
    // `export` before `{`, `as`, `default` or a lone `type` is a reserved word (TS1359), not
    // a modifier; the parameter list goes on.
    assert_eq!(
        err_codes("function f(export, b) {}", SourceKind::Ts),
        [1359]
    );
    assert_eq!(
        err_codes("function f(a, export {x}) {}", SourceKind::Ts),
        [1359, 1005]
    );
    assert_eq!(
        err_codes("function f(export as x) {}", SourceKind::Ts),
        [1359, 1005, 1005]
    );
    assert_eq!(
        err_codes("function f(export type) {}", SourceKind::Ts),
        [1359, 1005]
    );
    // `static static foo()` declares a property named `static`.
    has(
        &parse("class C { static static foo() {} }", SourceKind::Ts)
            .ast
            .dump(),
        "PropertyDef 10..23 modifiers=[static]",
    );
    // A modifier keyword followed by a line break, or by something that cannot follow a
    // modifier, is an identifier.
    assert_eq!(
        err_codes("public;\npublic.x;\nstatic [x];", SourceKind::Ts),
        [] as [u32; 0]
    );
    assert_eq!(
        stmt_kinds("public\nclass A {}", SourceKind::Ts),
        [NodeKind::ExprStmt, NodeKind::ClassDecl]
    );
}

/// Every keyword parsed as a modifier is recorded with its span; `export` and `default` only
/// when they modify a declaration.
#[test]
fn modifier_tokens() {
    fn tokens(src: &str) -> Vec<&str> {
        let r = parse(src, SourceKind::Ts);
        r.ast
            .modifier_tokens
            .iter()
            .map(|t| &src[t.span.start as usize..t.span.end as usize])
            .collect()
    }
    assert_eq!(
        tokens("export default async function f() {} export declare const enum E {}"),
        ["export", "default", "async", "export", "declare", "const"]
    );
    assert_eq!(
        tokens("export default x; export = y; export * from 'z'; export { a };"),
        [] as [&str; 0]
    );
    assert_eq!(
        tokens("class C { constructor(private readonly a) {} static async m() {} }"),
        ["private", "readonly", "static", "async"]
    );
    assert_eq!(
        tokens("let f = async () => 1, g = async x => x;"),
        ["async", "async"]
    );
    // Speculative arrow parses that fail leave no tokens behind.
    assert_eq!(tokens("let v = async(a, b);"), [] as [&str; 0]);

    let src = "@dec export abstract class A { @d public static m() {} }";
    let r = parse(src, SourceKind::Ts);
    let ts_r_ast::Node::Program { stmts } = *r.ast.node(r.ast.root) else {
        panic!()
    };
    let class = r.ast.list(stmts)[0];
    let flags: Vec<_> = r
        .ast
        .modifier_tokens_of(class)
        .iter()
        .map(|t| t.flag)
        .collect();
    assert_eq!(flags, [Modifiers::EXPORT, Modifiers::ABSTRACT]);
}

/// Misplaced modifiers that do not lead to a declaration: tsc reports TS1128 at the keyword
/// and skips it, or TS1146 where a declaration was required.
#[test]
fn stray_modifiers() {
    let cases: &[(&str, u32, u32)] = &[
        ("public x = 1;", 1128, 1),
        ("let x; public x;", 1128, 8),
        ("public declare\nvar x;", 1128, 1),
        ("static test;", 1128, 1),
        ("export foo;", 1128, 1),
        ("static static var x;", 1146, 7),
        ("@dec foo;", 1146, 5),
    ];
    for &(src, code, col) in cases {
        let r = parse(src, SourceKind::Ts);
        let d = r
            .diagnostics
            .first()
            .unwrap_or_else(|| panic!("no diagnostics for {src:?}"));
        assert_eq!(
            (d.code, d.span.start + 1, r.diagnostics.len()),
            (code, col, 1),
            "for {src:?}: {:?}",
            r.diagnostics
        );
    }
}

#[test]
fn decorators() {
    ok("@a @b.c @d() @e.f<T>() @(g) class A {}");
    ok("export @dec class A {} ");
    ok("@dec export class B {}");
    ok("@dec export default class {}");
    ok("class A { @dec() @other m(@param() p) {} @dec prop; @dec\n['computed'] = 1; }");
    ok("const x = @dec class {};");
}

#[test]
fn expressions() {
    ok("a = b ? c : d; a ||= b; a &&= b; a ??= b; a **= 2; a >>>= 1; a >>= 1; a <<= 1;");
    ok("x = a ?? b; y = (a || b) ?? c; z = a?.b?.[c]?.(d);");
    ok("x = a ** b ** c; y = (-a) ** b;");
    ok("x = typeof a === 'string' && !b || void 0; delete a.b; ~a;");
    ok("x = new A; y = new A.B(); z = new (f())(); w = new new A()();");
    ok(
        "x = new.target; y = import.meta.url; z = import('x'); w = import('x', { with: { type: 'json' } });",
    );
    ok("x = `a${b}c${`d${e}`}`; y = tag`x${y}`; z = String.raw`\\unicode`;");
    ok("x = /re/g.test(s) ? /[/]/ : a / b / c;");
    ok(
        "x = [1, , 2, ...rest]; y = {a, b: 1, [c]: 2, ...d, get e() { return 1 }, set e(v) {}, async f() {}, *g() {}, async *h() {}};",
    );
    ok("x = { get: 1, set: 2, async: 3, get() {}, set() {}, async() {}, new: 1, class: 2 };");
    ok("({ a, b } = c); [a, b] = [b, a]; ({ a = 1 } = {});");
    ok("x = a as any as T; y = a satisfies T; z = <T>a; w = a!; v = a!.b!; u = obj as const;");
    // Operators no tighter than the one under the `as` continue the expression.
    ok("a = 1 + 1 as number === 2; b = 1 as number * 2; c = (1 + 1 as number) * 2;");
    ok("d = 1 * 1 as number + 2; e = 1 + 1 as number + 2; f = 1 + 1 as number >> 2;");
    ok("x = 1_000_000n + 0x1Fn; y = .5 + 5. + 0b1010 + 0o17 + 1e-3;");
    ok("x = a in b; y = a instanceof B; for (const k in o) {}");
    ok("x = #a in this;");
    ok("a\n?.b");
    ok("x = a < b; y = a > b; z = a <= b; w = a >= b; v = a << b;");
    ok("if (a < b && c > d) {}");
    ok("x = f<T>(a); y = f<T, U>(a); z = new C<T>(); w = f<T>`t`;");
    ok("x = a < b > c;");
    ok("const v = obj?.a!.b;");
    ok("x = super.y;");
    ok("label: { break label; }");
}

#[test]
fn generic_call_disambiguation() {
    let d = ok("a < b > c;");
    has(&d, "op=Gt");
    let d = ok("f<T>(x);");
    has(&d, "type_args: TypeArgs");
    let d = ok("const g = f<string>;");
    has(&d, "Instantiation");
    let d = ok("x = a < b >> c;");
    has(&d, "op=Shr");
    let d = ok("if (a < b || c > (d)) {}");
    assert!(!d.contains("TypeArgs"));
}

#[test]
fn failed_type_arguments_restore_jsdoc_syntax() {
    let r = parse("let a=1,b=2; a < b!;", SourceKind::Ts);
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    assert!(r.ast.jsdoc_syntax.is_empty());

    let src = "type T = number!; let a=1,b=2; a < b!;";
    let r = parse(src, SourceKind::Ts);
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    assert_eq!(r.ast.jsdoc_syntax.len(), 1);
    let syntax = r.ast.jsdoc_syntax[0];
    assert_eq!(
        &src[syntax.span.start as usize..syntax.span.end as usize],
        "number!"
    );
}

/// R4: `extends A<T>` with a line break before `{` gives the heritage type the type
/// arguments, as on one line (tsc 7), instead of an instantiation expression.
#[test]
fn heritage_type_args_across_newline() {
    let one_line = ok("class A<T> {}\nclass B<T> extends A<T> {\n}\n");
    // Same length as `one_line`, so the dumps (spans included) are identical.
    assert_eq!(
        ok("class A<T> {}\nclass B<T> extends A<T>\n{\n}\n"),
        one_line
    );
    for src in [
        "class A<T> {}\nclass B<T> extends A<T>\r\n{\n}\n",
        "class A<T> {}\nclass B<T> extends A<T> // c\n{\n}\n",
    ] {
        let d = ok(src);
        assert!(!d.contains("Instantiation"), "{src:?}:\n{d}");
        has(&d, "ExprWithTypeArgs 33..37\n        expr: Ident 33..34");
        has(&d, "type_args: TypeArgs 34..37");
    }
    let d = ok("interface I<T> {}\ninterface J extends I<number>\n{}\n");
    assert!(!d.contains("Instantiation"), "{d}");
    let d = ok("class A<T> {}\nclass D implements A<number>,\n  A<string>\n{}\n");
    assert!(!d.contains("Instantiation"), "{d}");
    let d = ok("class A<T> {}\nconst X = class extends A<number>\n{};\n");
    assert!(!d.contains("Instantiation"), "{d}");
    // A call or tagged template after the line break still belongs to the expression.
    let d = ok("declare function A<T>(x: number): any;\nclass E<T> extends A<T>\n(1) {}\n");
    has(&d, "expr: Call");
    // Member access after an instantiation is TS1477 at the type arguments (tsc 7).
    let r = parse(
        "class A<T> {}\nclass C<T> extends A<T>\n.foo {}\n",
        SourceKind::Ts,
    );
    let codes: Vec<_> = r
        .diagnostics
        .iter()
        .map(|d| (d.code, d.span.start))
        .collect();
    assert_eq!(codes, [(1477, 34)]);
    // A second `<...>` after the unwrapped arguments is `',' expected` (tsc 7).
    let r = parse(
        "class A<T> {}\nclass F<T> extends A<T>\n<T> {}\n",
        SourceKind::Ts,
    );
    let codes: Vec<_> = r
        .diagnostics
        .iter()
        .map(|d| (d.code, d.span.start))
        .collect();
    assert_eq!(codes, [(1005, 38)]);
    // No node is left orphaned: every node but the root has a parent.
    let r = parse(
        "class A<T> {}\nclass B<T> extends A<T>\n{}\n",
        SourceKind::Ts,
    );
    let parents = ts_r_ast::ParentMap::build(&r.ast);
    for i in 1..r.ast.node_count() {
        let id = ts_r_ast::NodeId::from_raw(i as u32).unwrap();
        assert!(
            id == r.ast.root || parents.parent(id).is_some(),
            "orphan {i}"
        );
    }
}

/// Heritage lists recover like tsc's: `',' expected`, then the list goes on.
#[test]
fn heritage_list_recovery() {
    let cases: &[(&str, &[(u32, u32)])] = &[
        ("class A {}\nclass B extends A x {}\n", &[(1005, 29)]),
        (
            "interface I {}\ninterface J extends I x {}\n",
            &[(1005, 37)],
        ),
        ("class A {}\nclass B implements A x {}\n", &[(1005, 32)]),
        (
            "class A {}\nclass B extends A ; {}\nlet z = 1;\n",
            &[(1005, 29)],
        ),
        (
            "class A {}\nfunction f() { class B extends A }\nlet z = 1;\n",
            &[(1005, 44)],
        ),
        (
            "declare function foo(): void;\nclass A {}\nclass B extends A\nfoo();\n",
            &[(1005, 59), (1005, 64), (1005, 66)],
        ),
    ];
    for &(src, want) in cases {
        let r = parse(src, SourceKind::Ts);
        let got: Vec<_> = r
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.start))
            .collect();
        assert_eq!(got, want, "for {src:?}");
    }
}

/// R5: invalid syntax that tsc 7 rejects, with tsc's codes and positions (byte offsets).
/// TS1209, TS17006/7, TS1005 and TS1109 are syntax errors in tsc; TS5076, TS1106, TS1497
/// and TS18007 come from its grammar check, but are reported here too.
#[test]
fn invalid_syntax_reported_like_tsc() {
    type Case = (&'static str, SourceKind, &'static [(u32, u32)]);
    let cases: &[Case] = &[
        ("a ?? b || c;", SourceKind::Ts, &[(5076, 0)]),
        ("a || b ?? c;", SourceKind::Ts, &[(5076, 0)]),
        ("a ?? b && c;", SourceKind::Ts, &[(5076, 5)]),
        ("a && b ?? c;", SourceKind::Ts, &[(5076, 0)]),
        ("f((undefined && foo ?? {}));", SourceKind::Ts, &[(5076, 3)]),
        // `a + b as T * c` stops before `*` (microsoft/TypeScript#63527).
        (
            "const x = 1 + 1 as number * 2;",
            SourceKind::Ts,
            &[(1005, 26)],
        ),
        (
            "const x = 1 + 1 as any as number * 2;",
            SourceKind::Ts,
            &[(1005, 33)],
        ),
        (
            "const x = 1 >> 1 satisfies number + 2;",
            SourceKind::Ts,
            &[(1005, 34)],
        ),
        ("const x = a > b as T >> c;", SourceKind::Ts, &[(1005, 21)]),
        ("for (async of x) {}", SourceKind::Ts, &[(1106, 5)]),
        ("for (\\u0061sync of x) {}", SourceKind::Js, &[(1106, 5)]),
        ("new a?.b();", SourceKind::Ts, &[(1209, 5)]),
        ("new a.b?.c;", SourceKind::Ts, &[(1209, 7)]),
        ("@a?.b class C {}", SourceKind::Ts, &[(1497, 1)]),
        ("@a.b?.() class C {}", SourceKind::Ts, &[(1497, 1)]),
        ("@a()() class C {}", SourceKind::Ts, &[(1497, 1)]),
        ("@a().b class C {}", SourceKind::Ts, &[(1497, 1)]),
        ("@(a).b class C {}", SourceKind::Ts, &[(1497, 1)]),
        ("@a`x` class C {}", SourceKind::Ts, &[(1497, 1)]),
        ("class K { @this.x m() {} }", SourceKind::Ts, &[(1497, 11)]),
        (
            "await using [a] = null; export {};",
            SourceKind::Ts,
            &[(1005, 16)],
        ),
        ("++(a as any)++;", SourceKind::Ts, &[(1005, 12), (1109, 14)]),
        ("++a++;", SourceKind::Ts, &[(1005, 3), (1109, 5)]),
        ("--a.b--;", SourceKind::Ts, &[(1005, 5), (1109, 7)]),
        ("++-a;", SourceKind::Ts, &[(1109, 2)]),
        ("++++a;", SourceKind::Ts, &[(1109, 2)]),
        (
            "async function f() { ++await 42; }",
            SourceKind::Ts,
            &[(1109, 23)],
        ),
        ("await x = 1; export {};", SourceKind::Ts, &[(1005, 8)]),
        ("-x = 1;", SourceKind::Ts, &[(1005, 3)]),
        ("typeof x = 1;", SourceKind::Ts, &[(1005, 9)]),
        ("x++ = 1;", SourceKind::Ts, &[(1005, 4)]),
        ("a + b = c;", SourceKind::Ts, &[(1005, 6)]),
        ("a || b = c;", SourceKind::Ts, &[(1005, 7)]),
        ("<any>x = 1;", SourceKind::Ts, &[(1005, 7)]),
        ("x as any = 1;", SourceKind::Ts, &[(1005, 9)]),
        ("x satisfies any += 1;", SourceKind::Ts, &[(1005, 16)]),
        (
            "class C { #x = 1; m(v) { #x in v = 10; } }",
            SourceKind::Ts,
            &[(1005, 33)],
        ),
        ("-a ** 2;", SourceKind::Ts, &[(17006, 0)]),
        ("x = 1 + typeof a ** 2;", SourceKind::Ts, &[(17006, 8)]),
        ("<number>a ** 2;", SourceKind::Ts, &[(17007, 0)]),
        (
            "async function f() { await a ** 2; }",
            SourceKind::Ts,
            &[(17006, 21)],
        ),
        (
            "const e = <div className={a, b}/>;",
            SourceKind::Tsx,
            &[(18007, 26)],
        ),
        (
            "const e = <div>{a, b}</div>;",
            SourceKind::Tsx,
            &[(18007, 16)],
        ),
    ];
    for &(src, kind, want) in cases {
        let r = parse(src, kind);
        let got: Vec<_> = r
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.start))
            .collect();
        assert_eq!(got, want, "for {src:?}: {:#?}", r.diagnostics);
    }
    // Messages carry tsc's arguments.
    let msg = |src: &str| parse(src, SourceKind::Ts).diagnostics[0].message.clone();
    assert_eq!(
        msg("a || b ?? c;"),
        "'||' and '??' operations cannot be mixed without parentheses."
    );
    assert_eq!(
        msg("a ?? b && c;"),
        "'??' and '&&' operations cannot be mixed without parentheses."
    );
    assert_eq!(
        msg("new a.b?.c();"),
        "Invalid optional chain from new expression. Did you mean to call 'a.b()'?"
    );
    assert!(msg("void a ** 2;").contains("with the 'void' operator"));

    // The tree stays tsc's: `??` binds like `||`, left to right.
    let d = ok("x = (a ?? b) || c; y = a ?? (b && c); z = a ?? b ?? c;");
    has(&d, "op=LogicalOr\n        left: Paren");
    let d = dump_of("a ?? b || c;");
    has(
        &d,
        "Binary 0..11 op=LogicalOr\n      left: Binary 0..6 op=Coalesce",
    );
    let d = dump_of("a ?? b && c;");
    has(&d, "Binary 0..11 op=Coalesce\n      left: Ident");
    // `new a?.b()` is `(new a)?.b()`; `++a++` is `++a` then `++` with a missing operand.
    let d = dump_of("new a?.b();");
    has(&d, "callee: Member 0..8 optional\n        obj: New 0..5");
    let d = dump_of("++a++;");
    has(&d, "Update 0..3 prefix\n      arg: Ident");
    // `new tag<T> `x` (args)`: the type arguments belong to the tagged template (tsc 7).
    let d = ok("new tag<number> `a${1}`(100);");
    has(&d, "New 0..28 has_args\n      callee: TaggedTemplate 4..23");
    let d = ok("new tag `a`<string>(1);");
    has(&d, "callee: TaggedTemplate 4..11");

    // Valid neighbours stay valid.
    ok("for ((async) of x) {} for (async in x) {} for (async.x of x) {}");
    ok("async function f() { for await (async of x) {} }");
    ok("new a()?.b();");
    ok("@a.b.c() class C {} @(a?.b) class D {} @a.b<number>() class E {} @a!.b class F {}");
    ok("class K { @a! m1() {} @a.b!() m2() {} @a!() m3() {} @a.b.c!.d() m4() {} }");
    ok("x! = 1; (x as any) = 1; a ? b : c = 1; [a] = b; ({ a } = b); a.b = 1; a[0] += 1;");
    ok("(-a) ** 2; -(a ** 2); ++a ** 2; a++ ** 2; 2 ** -a; (<number>a) ** 2;");
    ok("-a++; typeof a++; ++a.b; ++a[0]; ++(a); ++this.x; ++new A().x;");
    ok("++a\n++a;");
    ok("async function f() { await x++; await -x; }");
    ok_tsx("const e = <div className={(a, b)}>{[a, b]}</div>;");
}

/// R6: the generated `map_children` agrees with `for_each_child` on every node of every
/// fixture, and a deep clone through it dumps the same tree.
#[test]
fn map_children_matches_for_each_child() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    let mut kinds = std::collections::BTreeSet::new();
    for e in entries {
        let path = e.path();
        let src = std::fs::read_to_string(&path).unwrap();
        let mut ast = crate::parse_path(path.to_str().unwrap(), &src).ast;
        let lists = ast.list_ranges.len();
        for i in 1..=ast.node_count() {
            let id = ts_r_ast::NodeId::from_raw(i as u32).unwrap();
            let node = *ast.node(id);
            kinds.insert(node.kind());
            let mut seen = Vec::new();
            let mapped = ast.map_children(id, &mut |_, c| {
                seen.push(c);
                c
            });
            assert_eq!(mapped, node);
            assert_eq!(seen, ast.children(id), "{:?} in {path:?}", node.kind());
        }
        assert_eq!(ast.list_ranges.len(), lists);
        fn deep_clone(ast: &mut ts_r_ast::Ast, id: ts_r_ast::NodeId) -> ts_r_ast::NodeId {
            let node = ast.map_children(id, &mut deep_clone);
            let span = ast.span(id);
            ast.add(node, span)
        }
        let before = ast.dump();
        let root = ast.root;
        ast.root = deep_clone(&mut ast, root);
        assert_eq!(ast.dump(), before, "{path:?}");
    }
    assert!(kinds.len() > 100, "fixtures cover {} kinds", kinds.len());
}

fn dump_of(src: &str) -> String {
    parse(src, SourceKind::Ts).ast.dump()
}

/// `.json` files parse as one JSON value (an object literal, not a block).
#[test]
fn json_values() {
    for src in [
        "{\"a\": 1, \"b\": [true, null, \"x\"], \"c\": {}}",
        "[1, 2]",
        "\"s\"",
        "",
        "  \n",
    ] {
        let r = crate::parse_json(src);
        assert!(r.diagnostics.is_empty(), "{src:?}: {:?}", r.diagnostics);
    }
    let d = crate::parse_json("{\"a\": 1}").ast.dump();
    assert!(d.contains("ObjectLit"), "{d}");
    assert!(!crate::parse_json("{\"a\": 1} }").diagnostics.is_empty());
}

/// tsc's JSON checks (`convertToJson`): (code, byte offset) of each diagnostic, checked
/// against tsc 7 with `resolveJsonModule`.
#[test]
fn json_rejects_non_json_values() {
    let cases: &[(&str, &[(u32, u32)])] = &[
        ("{\"a\": undefined}", &[(1328, 6)]),
        ("{a: 1, 'b': 2, 3: 4}", &[(1327, 1), (1327, 7), (1327, 15)]),
        (
            "{\"a\": 'x', \"b\": -1, \"c\": -x, \"d\": (1)}",
            &[(1327, 6), (1328, 25), (1328, 34)],
        ),
        (
            "[1, foo, `t`, 1n, , ...a]",
            &[(1328, 4), (1328, 9), (1328, 14), (1328, 18), (1328, 20)],
        ),
        ("{...x, \"a\"() {}, b}", &[(1136, 1), (1136, 7), (1136, 17)]),
        ("{\"a\": {\"b\": [1, {\"c\": d}]}}", &[(1328, 22)]),
    ];
    for (src, want) in cases {
        let got: Vec<(u32, u32)> = crate::parse_json(src)
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.start))
            .collect();
        assert_eq!(got, *want, "{src}");
    }
}

/// `readonly` after `(` in a type: a parenthesized type operator, or a parameter modifier
/// (tsc 7 parses both; the parameter property is the checker's TS2369). typeorm's
/// `ForeignKey.ts` hit the first.
#[test]
fn parenthesized_readonly_type_operator() {
    let d = ok("type A = (readonly string[]); type B = (readonly [number, string]) | number;");
    assert!(!d.contains("FunctionType"), "{d}");
    ok("type H = (readonly x);");
    for src in [
        "type C = (readonly x: string) => void;",
        "type D = (readonly [a, b]: [number, number]) => void;",
        "type E = (readonly { a }: { a: 1 }) => void;",
        "type G = (readonly x) => void;",
        "type I = (readonly) => void;",
        "type J = (readonly?: number) => void;",
        "type K = (readonly ...r: number[]) => void;",
    ] {
        let d = ok(src);
        assert!(d.contains("FunctionType"), "{src}: {d}");
    }
}

#[test]
fn types() {
    ok(
        "type A = string | number | boolean | null | undefined | void | never | unknown | any | object | symbol | bigint;",
    );
    ok(
        "type B = { a: string; b?: number; readonly c: boolean, [k: string]: any; (x: number): string; new (x: string): B; m<T>(a: T): void; get p(): number; set p(v: number) }",
    );
    ok(
        "type C = (a: string, b?: number) => void; type D = new (...args: any[]) => object; type E = abstract new () => object;",
    );
    ok("type F<T> = T extends string ? 'str' : T extends number ? 'num' : never;");
    ok(
        "type G<T> = T extends [infer H, ...infer R] ? H : never; type H<T> = T extends Array<infer U extends string> ? U : never;",
    );
    ok(
        "type I<T> = { [K in keyof T]?: T[K] }; type J<T> = { readonly [K in keyof T]-?: T[K] }; type K<T> = { -readonly [P in keyof T as `get${Capitalize<P & string>}`]+?: () => T[P] };",
    );
    ok(
        "type L = `prefix-${string}-suffix`; type M = typeof import('./x'); type N = import('./y').Z<number>;",
    );
    ok(
        "type O = [a: string, b?: number, ...rest: boolean[]]; type P = [string, number?, ...boolean[]];",
    );
    ok(
        "type Q = keyof T; type R = unique symbol; type S = readonly string[]; type T2 = T[number]['a'];",
    );
    ok("type U = typeof x.y.z; type V = typeof x<number>; type W = A.B.C<D>;");
    ok(
        "type X = (() => void) | null; type Y = Array<Array<number>>; type Z = Map<string, Array<Set<number>>>;",
    );
    ok("type AA = | 'a' | 'b'; type BB = & A & B;");
    ok("type CC = -1 | 1n | -1n | true | false | 'x' | `y`;");
    ok("type DD = this; type EE = { [K in 'a' | 'b']: K };");
    ok("let a: Array<number>= []; let f: <T>(x: T) => T;");
    ok("type FF = import('x', { with: { 'resolution-mode': 'import' } }).Y;");
    ok("type GG<T> = T extends (infer U)[] ? U : never;");
    ok(
        "type HH = (<T>() => T extends L ? 1 : 2) extends <T>() => T extends R ? 1 : 2 ? true : false;",
    );
    ok("function f<const T extends readonly unknown[]>(x: T): T { return x }");
    ok("type II = intrinsic; type Up<S extends string> = intrinsic;");
    ok("type JJ = A extends B\n? C : D;");
    ok("let x: typeof globalThis; let y: typeof this.z;");
    ok("type KK<in out T> = T; interface LL<out T> {}");
    ok("type MM = asserts;");
    assert_eq!(
        err_codes(
            "type T = A extends B ? C : D extends infer U extends string ? 1 : 2;",
            SourceKind::Ts
        ),
        Vec::<u32>::new()
    );
    assert_eq!(
        err_codes(
            "type A<T> = T extends (...args: any[]) => infer R extends string ? R : never;",
            SourceKind::Ts
        ),
        [1005]
    );
}

#[test]
fn declarations() {
    ok("interface A<T> extends B, C<T> { a: T; b(): void }");
    ok(
        "enum E { A, B = 2, 'c' = 3, D = A | B, } const enum F { X } declare enum G {} declare const enum H {}",
    );
    ok(
        "namespace N { export const x = 1; } namespace A.B.C { } declare module 'm' { export = x; } declare module 'short';",
    );
    // `module M {}` parses as a namespace, with tsc's TS1540 for the keyword.
    assert_eq!(err_codes("module M {}", SourceKind::Ts), [1540]);
    ok(
        "declare global { interface Window { x: number } } declare namespace NS { function f(): void; let v: number; }",
    );
    ok(
        "declare function f(): void; declare const c: number; declare class D { m(): void; } declare let l: string; declare var v: any;",
    );
    ok(
        "export declare function f(): void; export declare abstract class X {} export abstract class Y {}",
    );
    ok("type T<A = string, B extends A = A> = [A, B];");
    ok(
        "export type { A, B as C } from './x'; export type * from './y'; export type * as ns from './z';",
    );
    let k = stmt_kinds("type\nFoo = 1", SourceKind::Ts);
    assert_eq!(k, [NodeKind::ExprStmt, NodeKind::ExprStmt]);
    let k = stmt_kinds("declare\nfunction f() {}", SourceKind::Ts);
    assert_eq!(k, [NodeKind::ExprStmt, NodeKind::FunctionDecl]);
    let k = stmt_kinds("namespace\nfoo", SourceKind::Ts);
    assert_eq!(k, [NodeKind::ExprStmt, NodeKind::ExprStmt]);
    let k = stmt_kinds("abstract class A {}", SourceKind::Ts);
    assert_eq!(k, [NodeKind::ClassDecl]);
    ok("interface A { get x(): number; set x(v: number); readonly [k: string]: unknown; }");
}

#[test]
fn modules() {
    let src = r#"
import a from 'a';
import * as b from 'b';
import { c, d as e, type f, type g as h, 'string name' as i, default as j } from 'c';
import k, { l } from 'k';
import m, * as n from 'm';
import type T from 't';
import type { U } from 'u';
import type * as V from 'v';
import 'side-effect';
import json from './x.json' with { type: 'json' };
import legacy from './y.json' assert { type: 'json' };
import fs = require('fs');
import type P = require('p');
import Q = A.B.C;
export import R = S.T;
import defer * as deferred from 'd';
import source wasm from './x.wasm';
import type from 'type';
import { type } from 'x';
import { type as } from 'x';
import { type as as } from 'x';
import { type as as as } from 'x';
export { a, b as c2, type T as T2, 'x' as 'y' };
export { z } from 'z';
export * from 'all';
export * as ns from 'ns';
export const x1 = 1, x2 = 2;
export function fn() {}
export class Cls {}
export enum En {}
export interface If {}
export type Ty = string;
export namespace Na {}
export default 1 + 2;
export = foo;
export as namespace MyLib;
"#;
    let d = ok(src);
    has(&d, "ImportEquals");
    has(&d, "ExternalModuleRef");
    has(&d, "ImportAttributes");
    has(&d, "phase=Defer");
    has(&d, "phase=Source");
    has(&d, "NamespaceExport");
    has(&d, "ExportAssignment");
    let d = ok("import 'x';");
    assert!(!d.contains("has_clause"));
    let d = ok("import {} from 'x';");
    has(&d, "has_clause");
    let d = ok("import { type as } from 'x';");
    has(&d, "ImportSpecifier 9..16 type_only");
    let d = ok("import { type as x } from 'x';");
    assert!(!d.contains("type_only"), "{d}");
    let d = ok("import type from 'x';");
    assert!(!d.contains("type_only"), "{d}");
}

#[test]
fn jsx() {
    let d = ok_tsx(
        r#"const x = <div className="a" {...props} data-id={1} aria-label='l' disabled>text {expr} <Child /> &nbsp; {/* comment */}</div>;"#,
    );
    has(&d, "JsxElement");
    has(&d, "JsxSpreadAttr");
    has(&d, "JsxText");
    has(&d, "JsxExprContainer");
    ok_tsx("const f = <></>; const g = <><a /><b /></>;");
    ok_tsx(
        "const h = <Foo<string> bar={1} />; const i = <a.b.c />; const j = <ns:tag ns:attr='x' />; const k = <this.Comp />;",
    );
    ok_tsx("const l = <div attr=<span /> />;");
    ok_tsx("const m = <T,>(x: T) => x; const n = <T extends unknown>(x: T) => x;");
    ok_tsx("const o = cond ? <a /> : <b>{list.map(i => <li key={i}>{i}</li>)}</b>;");
    ok_tsx("const p = <div>\n  multi\n  line\n</div>;");
    ok_tsx("function C() { return (<div>{...children}</div>); }");
    ok_tsx("const q = <input value=\"a\\b\" />;");
    ok_tsx("const r = <a>{'}'}{'>'}</a>;");
    ok_js("const s = <div className='x'>{a}</div>;");
    ok_kind("const t = <div />;", SourceKind::Jsx);
    assert!(!err_codes("<a></b>", SourceKind::Tsx).is_empty());
    for (src, msg) in [
        (
            "<a.b></c>",
            "Expected corresponding JSX closing tag for 'a.b'.",
        ),
        ("<d>", "JSX element 'd' has no corresponding closing tag."),
    ] {
        let r = parse(src, SourceKind::Tsx);
        assert!(
            r.diagnostics.iter().any(|d| d.message == msg),
            "{src}: {:?}",
            r.diagnostics
        );
    }
    assert!(!err_codes("<a>", SourceKind::Tsx).is_empty());
    assert!(!err_codes("<a>}</a>", SourceKind::Tsx).is_empty());
}

/// JSX recovery observed in tsc 7 (errorSpanForUnclosedJsxTag, jsxAttributeInitializer,
/// parseUnaryExpressionNoTypeAssertionInJsx*, jsxAttributeWithoutExpressionReact).
#[test]
fn jsx_recovery_matches_tsc() {
    let cases: &[(&str, &[(u32, u32)])] = &[
        // Unclosed elements at the end of the file: one `'</' expected.` there.
        ("let y = <Baz>Hello\n", &[(17008, 9), (1005, 19)]),
        (
            "let x = <A>a\nlet y = <B>b\n",
            &[(17008, 9), (17008, 22), (1005, 26)],
        ),
        // An unclosed fragment: TS17014 from the end of the previous token to `<>`.
        ("let f = <>Hello\n", &[(17014, 7), (1005, 16)]),
        // `</a>` closing the parent: the child is the unclosed one.
        ("let a = <a><b>x</a>;", &[(17008, 12)]),
        (
            "let a = <a><b><c>x</a>;\n",
            &[(17008, 9), (17008, 12), (17002, 20), (1005, 24)],
        ),
        // A fragment closed by a named tag: what follows `</` is the parent's text again.
        (
            "let a = <a><>x</a>;\n",
            &[(17008, 9), (17015, 16), (1382, 17), (1005, 20)],
        ),
        // `<:` starts no tag (jsxNamespacePrefixInName); `</` is a closing tag, never a
        // `<` operator (tsxErrorRecovery1), and starts no statement (jsxInvalidEsprimaTestSuite).
        (
            "x = <:a />;",
            &[(1109, 4), (1109, 5), (1109, 9), (1109, 10)],
        ),
        ("x = <div>{ </div>;", &[(1109, 11)]),
        ("</>;", &[(1128, 0), (1109, 2), (1109, 3)]),
        // Attribute values.
        ("let a = <div foo= ></div>;", &[(1145, 18)]),
        ("let a = <div x={} y={} />;", &[(17000, 15)]),
        // Adjacent elements outside children: TS2657 (not under a prefix operator).
        ("let x = <a/><b/>;", &[(2657, 8)]),
        ("x = ~<a/> <\n", &[(1109, 11)]),
        // A tag that ends with neither `>` nor `/>`: `'/' expected.`; a token an enclosing
        // list can use ends the attributes, any other is skipped.
        ("x = <div ;\n", &[(1003, 9), (1005, 11)]),
        ("x = <b><1 >y</b>;", &[(1003, 8), (1382, 10)]),
        ("x = <this.#p />;", &[(1003, 12)]),
        // A conflict marker in children.
        ("x = <div>\n<<<<<<< HEAD\n", &[(1005, 9), (1185, 10)]),
    ];
    for &(src, want) in cases {
        let r = parse(src, SourceKind::Tsx);
        let mut got: Vec<(u32, u32)> = r
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.start))
            .collect();
        got.sort_by_key(|&(_, start)| start);
        assert_eq!(got, want, "for {src:?}");
    }
}

#[test]
fn js_files_disable_ts_syntax() {
    let d = ok_js("x = a < b > (c);");
    assert!(!d.contains("TypeArgs"));
    ok_js("let async = 1; async = 2;");
    ok_js("var yield = 1, await = 2;");
    ok_js("function f() { var await; }");
}

#[test]
fn dts_files() {
    ok_kind(
        "declare module 'x' { export function f(): void; export default class C { private x; } }\nexport declare const y: number;\nexport {};",
        SourceKind::Dts,
    );
    ok_kind(
        "export declare type A = string; declare function g<T>(x: T): T;",
        SourceKind::Dts,
    );
}

#[test]
fn await_and_yield_contexts() {
    ok("const x = await fetch(url);");
    ok("function f() { const await = 1; return await; }");
    ok("function* g() { yield\n1; }");
    ok("async function f() { await\nx; }");
    ok("const yieldValue = yield_ + 1;");
}

/// Outside a generator, `yield` followed on its line by a name, keyword, private name or
/// literal is a yield expression (the checker reports TS1163), as in tsc 7; otherwise it is
/// an identifier.
#[test]
fn yield_outside_generator() {
    for src in [
        "\"use strict\";\nfunction f() { yield 1; }",
        "function f() { yield 1; }",
        "class C { constructor() { yield foo } }",
        "class C { x = yield 1; static { yield 'a' } }",
        "function* g() { function f() { yield this } }",
        "function f() { x = yield 1n; g(yield null, yield #x in o); }",
        "function f() { yield of; yield async; yield class {}; }",
    ] {
        let d = ok(src);
        has(&d, "Yield ");
        assert!(!d.contains("name=\"yield\""), "{src:?}:\n{d}");
    }
    has(&ok_js("function f() { yield 1; }"), "Yield ");
    for src in [
        "yield;",
        "var x = yield;",
        "yield++;",
        "yield * 2;",
        "yield(x);",
        "yield[0];",
        "yield `t`;",
        "var f = yield => 1;",
        "yield\n1;",
        "yield: 1;",
        "a.yield; var o = { yield: 1, yield };",
        "function f() { return typeof yield; }",
        "var x = 1 + yield;",
    ] {
        assert!(
            !ok(src).contains("Yield "),
            "{src:?} should not parse a yield expression"
        );
    }
    // `yield in o`: the argument is parsed anyway, with a missing left operand.
    assert_eq!(
        err_codes("function f() { yield in o; }", SourceKind::Ts),
        [1109]
    );
}

#[test]
fn comments_are_collected() {
    let r = parse(
        "#!/usr/bin/env node\n// a\nlet x = /* b */ 1; /** doc */ function f() {}",
        SourceKind::Ts,
    );
    assert!(r.diagnostics.is_empty());
    assert_eq!(r.ast.comments.len(), 4);
    let span = r.ast.span(r.ast.root);
    assert_eq!(r.ast.comments_in(span).len(), 4);
}

/// Where tsc's recovery invents a declaration with a missing name, so do we (the binder
/// declares it as `(Missing)`, and two of them in one scope are duplicates).
#[test]
fn recovery_keeps_missing_names() {
    fn missing_count(src: &str) -> usize {
        parse(src, SourceKind::Ts)
            .ast
            .dump()
            .matches("Missing")
            .count()
    }
    // Object binding pattern: a keyword or literal key without `:` has a missing target, and
    // the list goes on (`{ return 'thing'; }` is two elements; the `;` is skipped).
    let src = "export default function getThing( { return 'thing'; }";
    let d = parse(src, SourceKind::Ts).ast.dump();
    assert_eq!(d.matches("BindingProperty").count(), 2, "{d}");
    assert_eq!(missing_count(src), 2);
    assert_eq!(err_codes(src, SourceKind::Ts), [1005, 1005, 1005]);
    assert_eq!(missing_count("var {while, return} = o;"), 2);
    // Parameters: a token that starts a type but not a binding is a parameter with a missing
    // name, and is skipped; `:` `;` `.` `=>` `}` are skipped with TS1138.
    let params = |src: &str| {
        let r = parse(src, SourceKind::Ts);
        let ts_r_ast::Node::Program { stmts } = *r.ast.node(r.ast.root) else {
            panic!()
        };
        let ts_r_ast::Node::FunctionDecl { params, .. } = *r.ast.node(r.ast.list(stmts)[0]) else {
            panic!()
        };
        r.ast.list(params).len()
    };
    assert_eq!(params("function f(a!: B) {}"), 3);
    assert_eq!(params("function f(a!, b) {}"), 3);
    assert_eq!(params("function f(a ! ! b) {}"), 4);
    assert_eq!(params("function f(a<b) {}"), 3);
    assert_eq!(params("function f(a 1) {}"), 2);
    assert_eq!(params("function f(a ; b) {}"), 2);
    assert_eq!(params("function f(a => b) {}"), 2);
    assert_eq!(params("function f(a - b) {}"), 1);
    assert_eq!(params("function f(a (b)) {}"), 1);
    assert_eq!(params("function f(null, b) {}"), 2);
    assert_eq!(params("function f(enum) {}"), 0);
    assert_eq!(missing_count("function f(a!: B) {}"), 1);
    assert_eq!(
        err_codes("function f(a!: B) {}", SourceKind::Ts),
        [1005, 1138]
    );
    assert_eq!(err_codes("function f(! b) {}", SourceKind::Ts), [1003]);
    assert_eq!(missing_count("function f(null) {}"), 1);
    // `yield` as a generator's parameter name stays an identifier (the binder reports it).
    assert_eq!(missing_count("function* g(yield) {}"), 0);
    // Declarations named with a reserved word: the name is missing and the word starts the
    // next statement.
    assert_eq!(
        stmt_kinds("enum void {}", SourceKind::Ts),
        [NodeKind::Enum, NodeKind::ExprStmt]
    );
    assert_eq!(missing_count("enum void {}"), 1);
    assert_eq!(
        stmt_kinds("function f1(enum) {}", SourceKind::Ts),
        [NodeKind::FunctionDecl, NodeKind::Enum, NodeKind::Block]
    );
    assert_eq!(
        stmt_kinds("function throw() {}", SourceKind::Ts)[0],
        NodeKind::FunctionDecl
    );
    assert_eq!(missing_count("function () {}"), 1);
    assert_eq!(missing_count("export default function () {}"), 0);
    // `export import { a } from "m"` is an import declaration with a misplaced modifier.
    assert_eq!(
        stmt_kinds("export import { a } from 'm';", SourceKind::Ts),
        [NodeKind::ImportDecl]
    );
    assert_eq!(
        stmt_kinds("export import a = require('m');", SourceKind::Ts),
        [NodeKind::ImportEquals]
    );
    // An arrow after a line break still parses (TS1200).
    assert_eq!(
        stmt_kinds("var f = ()\n=> 1, g = x\n=> x;", SourceKind::Ts),
        [NodeKind::VarDecl]
    );
    assert_eq!(
        err_codes("var f = ()\n=> 1, g = x\n=> x;", SourceKind::Ts),
        [1200, 1200]
    );
}

#[test]
fn errors_produce_tree() {
    for (src, code) in [
        ("let x = ;", 1109),
        ("f(1;", 1005),
        ("if (x { }", 1005),
        // A grammar error tsc reports in a clean file (exportClassWithoutName).
        ("class { }", 1211),
        ("type T = ;", 1110),
        ("}", 1128),
        ("try {}", 1472),
        ("x = 'abc", 1002),
        ("/* open", 1010),
        ("let a = 08;", 1489),
        ("let a = 017;", 1121),
        ("for (;;", 1109),
    ] {
        let r = parse(src, SourceKind::Ts);
        assert!(
            r.diagnostics.iter().any(|d| d.code == code),
            "{src}: {:?}",
            r.diagnostics
        );
        assert_eq!(r.ast.kind(r.ast.root), NodeKind::Program);
    }
}

#[test]
fn never_panics_on_garbage() {
    let samples = [
        "",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "<",
        ">",
        "=>",
        "...",
        "@",
        "#",
        "`",
        "${",
        "/",
        "\\",
        "class",
        "class A {",
        "class A extends",
        "function",
        "function (",
        "import",
        "import {",
        "export",
        "export {",
        "enum",
        "enum E {",
        "interface",
        "type",
        "type X<",
        "let x: {",
        "a ? b",
        "a ? b :",
        "new",
        "new.",
        "import.",
        "async",
        "async (",
        "<div",
        "<div>",
        "</div>",
        "x = <a b=",
        "x = <a b={",
        "for (",
        "for (let",
        "switch (",
        "switch (x) {",
        "try",
        "catch",
        "if (",
        "do",
        "while",
        "@dec",
        "abstract",
        "declare",
        "namespace",
        "module",
        "global",
        "let [",
        "let {",
        "f<",
        "f<T",
        "a!.",
        "a?.",
        "`${a",
        "`${a}",
        "x as",
        "x satisfies",
        "(a, b) =>",
        "(a: ) =>",
        "<T>(",
        "type A = { [K in",
        "type A = [",
        "type A = (",
        "\u{0}",
        "\u{FEFF}",
        "'\\u{",
        "0x",
        "1e",
        "#!/x\n#!",
        "a\u{2028}b",
        "😀",
        // Found by mutation fuzzing: a lone `#` after an element used to loop forever.
        "[\"\"#",
        "f(\"\" #",
    ];
    for s in samples {
        for kind in [
            SourceKind::Ts,
            SourceKind::Tsx,
            SourceKind::Js,
            SourceKind::Dts,
        ] {
            let r = parse(s, kind);
            assert_eq!(r.ast.kind(r.ast.root), NodeKind::Program);
            // Every child id is in range and walks terminate.
            let pm = ts_r_ast::ParentMap::build(&r.ast);
            let _ = pm;
        }
    }
    // Truncations of a realistic file.
    let src = include_str!("../tests/fixtures/kitchen_sink.ts");
    for cut in (0..src.len()).step_by(7) {
        if src.is_char_boundary(cut) {
            let _ = parse(&src[..cut], SourceKind::Ts);
            let _ = parse(&src[..cut], SourceKind::Tsx);
        }
    }
}

#[test]
fn deep_nesting_is_bounded() {
    let src = "(".repeat(100_000);
    let r = parse(&src, SourceKind::Ts);
    assert!(!r.diagnostics.is_empty());
    let src = "[".repeat(50_000) + &"]".repeat(50_000);
    let _ = parse(&src, SourceKind::Ts);
    let src = "a".to_owned() + &" + a".repeat(200_000);
    let r = parse(&src, SourceKind::Ts);
    assert!(r.diagnostics.is_empty());
    let src = "type T = ".to_owned() + &"Array<".repeat(5000) + "x" + &">".repeat(5000);
    let _ = parse(&src, SourceKind::Ts);
    let src = "x = ".to_owned() + &"{a:".repeat(5000);
    let _ = parse(&src, SourceKind::Ts);
    let src = "<a>".repeat(5000);
    let _ = parse(&src, SourceKind::Tsx);
    // Deep nesting reports TS99001 instead of overflowing the stack.
    let r = parse(&"(".repeat(100_000), SourceKind::Ts);
    assert!(r.diagnostics.iter().any(|d| d.code == 99001));
}

#[test]
fn long_private_member_chain_is_linear() {
    let src = format!(
        "class C {{ #x = this; f() {{ return this{}; }} }}",
        ".#x".repeat(100_000)
    );
    let start = std::time::Instant::now();
    let r = parse(&src, SourceKind::Ts);
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    assert!(start.elapsed().as_secs() < 10, "slow private member chain");
}

#[test]
fn private_member_optional_chain_membership() {
    for (expr, in_chain) in [
        ("this.#x.#x", false),
        ("this?.#x", true),
        ("this?.a.#x", true),
        ("this?.a().#x", true),
        ("this?.[0].#x", true),
        ("this?.().#x", true),
        ("this?.a!.#x", true),
        ("(this?.a).#x", false),
        ("(this?.a)!.#x", false),
    ] {
        let src = format!("class C {{ #x = this; f() {{ return {expr}; }} }}");
        let r = parse(&src, SourceKind::Ts);
        let codes: Vec<_> = r.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, if in_chain { vec![18030] } else { vec![] }, "{expr}");
    }
}

#[test]
fn deep_binding_patterns_report_nesting_depth() {
    let n = 100_000;
    for src in [
        format!("let {}x{} = y;", "[".repeat(n), "]".repeat(n)),
        format!("let {}x{} = y;", "{a:".repeat(n), "}".repeat(n)),
        format!("function f({}x{}) {{}}", "[...".repeat(n), "]".repeat(n)),
        format!("try {{}} catch ({}x{}) {{}}", "[".repeat(n), "]".repeat(n)),
    ] {
        let r = parse(&src, SourceKind::Ts);
        assert!(
            r.diagnostics.iter().any(|d| d.code == 99001),
            "{}",
            &src[..20]
        );
    }
}

/// Repetitive garbage, lone `#`, and unterminated constructs at scale: each parse must
/// finish quickly with diagnostics and stay within the node budget (no runaway loop).
#[test]
fn pathological_inputs_terminate() {
    let units: &[&str] = &[
        "#",
        "@",
        "#!",
        "[\"\"#",
        "f(\"\" #",
        "{",
        "}",
        "(",
        ")",
        "[",
        "]",
        "<",
        ">",
        "`${",
        "${",
        "/*",
        "*/",
        "'",
        "\"",
        "`",
        "=>",
        "?.",
        "...",
        ",",
        ";",
        "a<",
        "<a ",
        "</",
        "class {",
        "enum {",
        "import {",
        "export {",
        "type T<",
        "x as ",
        "@d ",
        "async ",
        "abstract ",
        "declare ",
        "{a:",
        "[a,",
        "f(a,",
        "<T,>",
        "\\u",
        "0x",
        "1e",
        "#a",
        "new ",
        "case ",
        "class C extends A ",
        "class C extends A<T>\n",
        "interface I extends ; ",
    ];
    for unit in units {
        for kind in [
            SourceKind::Ts,
            SourceKind::Tsx,
            SourceKind::Js,
            SourceKind::Dts,
        ] {
            let src = unit.repeat(20_000 / unit.len().max(1));
            let start = std::time::Instant::now();
            let r = parse(&src, kind);
            assert!(
                start.elapsed().as_secs() < 10,
                "slow on {unit:?} x N ({kind:?})"
            );
            assert!(
                r.ast.node_count() <= src.len() * 4 + 1024,
                "node blow-up on {unit:?}"
            );
        }
    }
}

#[test]
fn spans_nest() {
    let src = include_str!("../tests/fixtures/kitchen_sink.ts");
    let r = parse(src, SourceKind::Ts);
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    let ast = &r.ast;
    for i in 1..ast.nodes.len() {
        let id = ts_r_ast::NodeId::from_raw(i as u32).unwrap();
        let span = ast.span(id);
        assert!(span.start <= span.end && span.end as usize <= src.len());
        if ast.kind(id) == NodeKind::Missing {
            continue;
        }
        let mut prev_end = span.start;
        ast.for_each_child(id, |c| {
            let cs = ast.span(c);
            assert!(
                span.contains_span(cs),
                "{:?} {:?} not in {:?} {:?}",
                ast.kind(c),
                cs,
                ast.kind(id),
                span
            );
            assert!(
                cs.start >= prev_end,
                "children out of order in {:?}",
                ast.kind(id)
            );
            prev_end = cs.end;
        });
    }
}

/// First diagnostic (code, 1-based column) for one-line inputs, checked against tsc 7.
#[test]
fn first_error_matches_tsc() {
    let cases: &[(&str, u32, u32)] = &[
        ("let x = 1 +;", 1109, 12),
        ("import { a as } from \"x\";", 1003, 15),
        ("let r = a ? b;", 1005, 14),
        ("for (let i = 0; i < 3) {}", 1005, 22),
        ("let a = 1 let b = 2;", 1005, 11),
        ("switch (x) { case: break; }", 1109, 18),
        ("let x: Array<number = [];", 1005, 21),
        ("let y = a?.b?.;", 1003, 15),
        ("const obj = { a: 1, , b: 2 };", 1136, 21),
        ("type U = | ;", 1110, 12),
        ("type V = keyof;", 1110, 15),
        ("import x, from \"y\";", 1005, 11),
        ("class K extends {}", 1097, 16),
        ("let u = 0x;", 1125, 11),
        ("var v = 08;", 1489, 9),
        ("let n = 1.e;", 1124, 12),
        ("let m = \"unterminated", 1002, 22),
        (r#"let s = "ab\u12";"#, 1125, 16),
        (r#"let t = `\x4`;"#, 1125, 13),
        ("class C { get x(): number; }", 1005, 26),
        ("function f(...a,) {}", 1013, 16),
        ("let {...a,} = b;", 1013, 10),
        ("let [...a,] = b;", 1013, 10),
        ("let f = (...a,) => a;", 1013, 14),
        ("let a = /a/gx;", 1499, 13),
        ("let a = /a/gg;", 1500, 13),
        ("let a = /a/uv;", 1502, 13),
        ("function f() { yield in o; }", 1109, 22),
        ("function* f() { <number> yield 0; }", 1109, 26),
        ("function* f() { var x = 1 + yield 2; }", 1109, 29),
        ("function* f() { typeof yield; }", 1109, 24),
        ("var [debugger, if] = [1, 2];", 1181, 6),
        ("var f = ()\n=> 1;", 1200, 12),
        ("function f(! b) {}", 1003, 12),
        ("const x = A<T>.foo;", 1477, 12),
        ("const x = A<T>?.foo;", 1477, 12),
        ("class B extends A x {}", 1005, 19),
        ("function f(null) {}", 1359, 12),
        ("function f(export) {}", 1359, 12),
        ("function f(a, export {x}) {}", 1359, 15),
    ];
    for &(src, code, col) in cases {
        let r = parse(src, SourceKind::Ts);
        let d = r
            .diagnostics
            .first()
            .unwrap_or_else(|| panic!("no diagnostics for {src:?}"));
        assert_eq!(
            (d.code, d.span.start + 1),
            (code, col),
            "for {src:?}: {:?}",
            r.diagnostics
        );
    }
    // Accessors without bodies are fine when ambient or abstract; tagged templates allow
    // invalid escapes.
    ok("abstract class A { abstract get x(): number; }");
    ok("declare class B { get x(): number; set x(v); }");
    ok(r"let u = tag`\x4`;");
    ok("class K extends {} {}");
    ok("let q = { readonly: 1 }; type T = [readonly: string];");
}

/// In a `.js` file the TS8xxx errors are not parse errors: the checker's grammar errors
/// (deferred by the parser) are kept next to them, also at one position, and dropped only
/// for a real syntax error (tsc 7 baseline plainJSGrammarErrors; tsc's command line stops
/// after any syntactic error, TS8xxx included, so it shows only the TS8xxx there).
#[test]
fn js_grammar_errors_stay_next_to_ts8xxx() {
    use crate::{ParseOptions, TsInJs, parse_with};
    let report = ParseOptions {
        ts_in_js: TsInJs::Report,
        ..Default::default()
    };
    let cases: &[(&str, &[(u32, u32)])] = &[
        ("class C { const x = 1 }", &[(8009, 10), (1248, 16)]),
        ("let o = { m?() { return 1 } };", &[(8009, 11), (1162, 11)]),
        ("let o = { p?: 1, q!: 2 };", &[(1162, 11), (1255, 18)]),
        ("class C { get g(); }", &[(8017, 10), (1005, 17)]),
        // A syntax error: only the TS8xxx and the syntax error stay.
        (
            "let o = { m?() { return 1 } };\nlet x = ;",
            &[(8009, 11), (1109, 39)],
        ),
    ];
    for &(src, want) in cases {
        let r = parse_with(src, SourceKind::Js, report);
        let got: Vec<(u32, u32)> = r
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.start))
            .collect();
        assert_eq!(got, want, "for {src:?}");
    }
}

/// TypeScript syntax in `.js` files (`TsInJs::Report`): every diagnostic as
/// (code, line, column), checked against tsc 7.0.2 `--allowJs --checkJs` (one per position;
/// where tsc reports two at one position, the first). The checker's grammar errors the
/// parser defers (TS1015, TS1264, TS1540) are kept next to them: tsc's test baselines show
/// both, its command line stops after the TS8xxx.
#[test]
fn ts_in_js_matches_tsc() {
    use crate::{ParseOptions, TsInJs, parse_with};
    type Row = (&'static str, &'static [(u32, u32, u32)]);
    let cases: &[Row] = &[
        ("new Array<Foo>(1, 2, 3);", &[]),
        ("Array<Foo>(1, 2, 3);", &[]),
        ("const x = <div/> as any;", &[(8016, 1, 21)]),
        (
            "declare const foo: number | undefined;\nfoo as any;",
            &[(8009, 1, 1), (8010, 1, 20), (8016, 2, 8)],
        ),
        ("let x: number = 1;", &[(8010, 1, 8)]),
        (
            "function f<T>(a: T): T { return a; }",
            &[(8004, 1, 12), (8010, 1, 18), (8010, 1, 22)],
        ),
        (
            "interface I { a: number }\ntype T = string;",
            &[(8006, 1, 11), (8008, 2, 6)],
        ),
        ("enum E { A }", &[(8006, 1, 6)]),
        (
            "class C { private x = 1; constructor(public y) {} }",
            &[(8009, 1, 11), (8012, 1, 38)],
        ),
        (
            "let y = x!;\nlet z = x satisfies any;",
            &[(8013, 1, 9), (8037, 2, 21)],
        ),
        (
            "import type { A } from \"./a\";\nexport type { B };",
            &[(8006, 1, 1), (8006, 2, 1)],
        ),
        // The deferred TS1540 stays next to TS8006 (the checker's error; tsc's command
        // line stops after the TS8xxx).
        (
            "namespace N {}\nmodule M {}",
            &[(8006, 1, 11), (8006, 2, 8), (1540, 2, 8)],
        ),
        (
            "class D implements I {}\nabstract class E {}",
            &[(8005, 1, 9), (8009, 2, 1)],
        ),
        (
            "function g(a?) {}\nlet h = (a: number): void => {};",
            &[(8009, 1, 13), (8010, 2, 13), (8010, 2, 22)],
        ),
        (
            "import x = require(\"x\");\nexport = x;",
            &[(8002, 1, 1), (8003, 2, 1)],
        ),
        (
            "class F { x!: number; declare y; readonly z = 1; m?() {} }",
            &[(8010, 1, 15), (8009, 1, 23), (8009, 1, 34), (8009, 1, 51)],
        ),
        (
            "function k(this: any) {}\nlet o = f<string>;",
            &[(8010, 1, 18), (1109, 2, 18)],
        ),
        ("class A extends B<T> {}", &[(8011, 1, 19)]),
        ("tag<T>`x`;\na?.b<T>();", &[(1109, 2, 9)]),
        ("class C<T> { m<U>() {} }", &[(8004, 1, 9), (8004, 1, 16)]),
        ("function f(): void;\nfunction f() {}", &[(8017, 1, 1)]),
        ("class D { [key: string]: any; }", &[(8017, 1, 11)]),
        ("const enum E { A }", &[(8006, 1, 12)]),
        ("declare global { var x; }\ndeclare module \"m\" {}", &[]),
        (
            "import x, { type A } from \"a\";\nexport { type B } from \"b\";",
            &[(8006, 1, 13), (8006, 2, 10)],
        ),
        ("export type * from \"a\";", &[(8006, 1, 1)]),
        (
            "let o = { m(): void {} };\ntry {} catch (e: unknown) {}",
            &[(8010, 1, 16), (8010, 2, 18)],
        ),
        // TS1264 is the checker's grammar error for `y!` without a type (not a TS8xxx).
        (
            "let x!: number;\nclass F { y!; override m() {} }",
            &[(8010, 1, 9), (1264, 2, 12), (8009, 2, 15)],
        ),
        (
            "abstract class G { abstract m(); protected n() {} public static s = 1; }",
            &[(8009, 1, 1), (8009, 1, 20), (8009, 1, 34), (8009, 1, 51)],
        ),
        (
            "function h(x): x is string { return true; }\nfunction j(x): asserts x {}",
            &[(8010, 1, 16), (8010, 2, 16)],
        ),
        (
            "export declare const q = 1;\nexport abstract class K {}\nexport default interface L {}",
            &[(8009, 1, 8), (8009, 2, 8), (8006, 3, 26)],
        ),
        ("const f2 = async <T,>(x) => x;", &[(8004, 1, 19)]),
        (
            "class M { constructor(private readonly a, b?) {} }",
            &[(8012, 1, 23), (8009, 1, 44)],
        ),
        (
            "function o(a?: number = 1) {}\nfor (let i: number = 0;;) {}",
            &[(1015, 1, 12), (8009, 1, 13), (8010, 1, 16), (8010, 2, 13)],
        ),
        (
            "class N { get a(): number { return 1 } set a(v: number) {} }",
            &[(8010, 1, 20), (8010, 1, 49)],
        ),
        (
            "export import Z = N.Z;\nimport type D from \"d\";",
            &[(8002, 1, 1), (8006, 2, 1)],
        ),
        (
            "let fn = function <T>(a) {};\nlet ar = (a?) => a;",
            &[(8004, 1, 20), (8009, 2, 12)],
        ),
        (
            "let u: unique symbol;\nlet r = (a): a is number => true;",
            &[(8010, 1, 8), (8010, 2, 14)],
        ),
        (
            "class S { declare readonly x: number; accessor y = 1; }",
            &[(8009, 1, 11), (8009, 1, 19), (8010, 1, 31)],
        ),
        (
            "namespace A.B.C {}",
            &[(8006, 1, 11), (8006, 1, 13), (8006, 1, 15)],
        ),
        ("type X<T> = T;", &[(8008, 1, 6)]),
        (
            "namespace N { let x: number; function f<T>() {} }",
            &[(8006, 1, 11), (8010, 1, 22), (8004, 1, 41)],
        ),
        (
            "declare function f(): void;\ndeclare class C { m(): void }",
            &[(8009, 1, 1), (8009, 2, 1), (8017, 2, 19)],
        ),
        (
            "declare namespace Q { let y: number; }",
            &[(8006, 1, 19), (8010, 1, 30)],
        ),
        (
            "export declare function k(a: number): void;",
            &[(8017, 1, 1), (8009, 1, 8), (8010, 1, 30)],
        ),
        (
            "function f(this: any, a: number) {}",
            &[(8010, 1, 18), (8010, 1, 26)],
        ),
        (
            "declare enum E { A }\ndeclare interface I {}",
            &[(8006, 1, 14), (8006, 2, 19)],
        ),
        (
            "declare function f<T>(a: T): void;",
            &[(8009, 1, 1), (8004, 1, 20), (8010, 1, 26)],
        ),
        (
            "declare namespace N { function g(a: number): void; }",
            &[(8006, 1, 19), (8017, 1, 23), (8010, 1, 37)],
        ),
        (
            "class C { constructor(a: number); constructor(a) {} }",
            &[(8017, 1, 11), (8010, 1, 26)],
        ),
        ("export default abstract class {}", &[(8009, 1, 16)]),
        (
            "class D { abstract x: number; private static readonly y; }",
            &[(8009, 1, 11), (8010, 1, 23), (8009, 1, 31), (8009, 1, 46)],
        ),
        (
            "declare module M { let x: number; }",
            &[(8006, 1, 16), (1540, 1, 16), (8010, 1, 27)],
        ),
        ("const f = <T,>(x) => x;", &[(8004, 1, 12)]),
        ("const g = <T extends U>(x) => x;", &[(8004, 1, 12)]),
        ("new Foo<T>();", &[(1109, 1, 12)]),
        (
            "let x = y as unknown as T;",
            &[(8016, 1, 14), (8016, 1, 25)],
        ),
        (
            "class E { m?(): void; n<T>(): void; }",
            &[(8017, 1, 11), (8009, 1, 12), (8017, 1, 23), (8004, 1, 25)],
        ),
        (
            "export declare namespace Z {}\nexport declare enum Y {}",
            &[(8006, 1, 26), (8006, 2, 21)],
        ),
        ("function f(a /* c */ ?) {}", &[(8009, 1, 22)]),
        ("class F<in out T> {}", &[(8004, 1, 9)]),
        (
            "let o = { a: 1 }!;\nlet p = (a)!.b;",
            &[(8013, 1, 9), (8013, 2, 9)],
        ),
        (
            "export type T = 1;\nexport interface J {}",
            &[(8008, 1, 13), (8006, 2, 18)],
        ),
        (
            "import type * as ns from \"x\";\nexport type { default } from \"y\";",
            &[(8006, 1, 1), (8006, 2, 1)],
        ),
        // JavaScript has no namespaces: the module-level codes (importDeclarationInModuleDeclaration2).
        (
            "function f() {\n    import \"fs\";\n    export { x };\n}",
            &[(1473, 2, 5), (1474, 3, 5)],
        ),
    ];
    let report = ParseOptions {
        ts_in_js: TsInJs::Report,
        ..Default::default()
    };
    let allow = ParseOptions {
        ts_in_js: TsInJs::Allow,
        ..Default::default()
    };
    for &(src, want) in cases {
        let r = parse_with(src, SourceKind::Js, report);
        let got: Vec<(u32, u32, u32)> = r
            .diagnostics
            .iter()
            .map(|d| {
                let before = &src[..d.span.start as usize];
                let line = before.matches('\n').count() as u32 + 1;
                let col = (before.len() - before.rfind('\n').map_or(0, |i| i + 1)) as u32 + 1;
                (d.code, line, col)
            })
            .collect();
        assert_eq!(got, want, "for {src:?}");
        // `Allow` parses the same tree without the TS8xxx errors.
        let a = parse_with(src, SourceKind::Js, allow);
        assert_eq!(a.ast.dump(), r.ast.dump(), "for {src:?}");
        assert!(
            a.diagnostics
                .iter()
                .all(|d| !(8000..9000).contains(&d.code)),
            "for {src:?}"
        );
        // TypeScript kinds ignore the option.
        assert_eq!(
            parse(src, SourceKind::Tsx).diagnostics,
            parse_with(src, SourceKind::Tsx, report).diagnostics
        );
    }
    // Expressions take no type arguments: `new Array<Foo>(1)` is `(new Array < Foo) > (1)`.
    let d = parse_with("new Array<Foo>(1, 2, 3);", SourceKind::Js, allow)
        .ast
        .dump();
    has(&d, "expr: Binary");
    has(&d, "left: Binary");
    has(&d, "left: New");
    let d = parse_with("<div/> as any;", SourceKind::Js, allow)
        .ast
        .dump();
    has(&d, "expr: As");
    has(&d, "expr: JsxElement");
    // Without the option a `.js` file stays JavaScript-only.
    assert!(!err_codes("let x: number = 1;", SourceKind::Js).is_empty());
}

/// Every syntax diagnostic as (code, line, column), as tsc 7 reports them (from the upstream
/// conformance baselines named in the comments). Pins recovery: where each error lands and
/// which follow-on errors appear.
#[test]
fn recovery_matches_tsc() {
    type Row = (&'static str, &'static [(u32, u32, u32)]);
    let cases: &[Row] = &[
        // parserSkippedTokens*: the scanner's TS1127 wins over the parser's error at the
        // same position; the bad character is skipped.
        ("\\", &[(1127, 1, 1)]),
        ("\\  /regexp/;", &[(1127, 1, 1)]),
        ("/regexp/ \\ ;", &[(1127, 1, 10)]),
        (";\n/*foo*/ \\ /*bar*/", &[(1127, 2, 9)]),
        // List recovery: a token an enclosing list can use ends the inner list (class2,
        // parserErrorRecovery_ClassElement2); a stray `catch`/`finally` starts a `try`
        // (invalidTryStatements).
        (
            "class foo { constructor() { static f = 3; } }",
            &[(1128, 1, 29), (1128, 1, 45)],
        ),
        (
            "namespace M {\n  class C {\n\n  enum E {\n  }\n}",
            &[(1068, 4, 3)],
        ),
        (
            "function fn() {\n    catch(x) { }\n\n    finally { }\n\n    try { };\n}",
            &[(1005, 2, 5), (1472, 6, 12)],
        ),
        (
            "function f() {\n    finally { }\n    catch (x) { }\n}",
            &[(1005, 2, 5), (1005, 3, 5)],
        ),
        // A missing `;` after a lone name (commonMissingSemicolons, anonymousModules,
        // staticModifierAlreadySeen) or a typed property
        // (derivedClassSuperCallsInNonConstructorMembers, privateInstanceMemberAccessibility).
        ("clasd MyClass2 {}", &[(1435, 1, 1), (1434, 1, 7)]),
        ("interface { }", &[(1438, 1, 11)]),
        ("module {\n}", &[(1437, 1, 8)]),
        ("declare constd x: 1;", &[(1435, 1, 9)]),
        ("class C {\n  static static Foo;\n}", &[(1434, 2, 10)]),
        (
            "class C {\n    a: super();\n}",
            &[(1441, 2, 13), (1068, 2, 14)],
        ),
        ("class C {\n    a: this.foo;\n}", &[(1442, 2, 12)]),
        // A missing `,` in an argument list goes on with the list (staticsInAFunction).
        (
            "function boo() {\n   test(name:string)\n}",
            &[(1005, 2, 13)],
        ),
        // Variable declaration lists (parserVariableDeclaration*, ClassDeclaration26,
        // parserInvalidIdentifiersInVariableStatements1): TS1123/TS1009 only in a clean file.
        ("var;", &[(1123, 1, 4)]),
        ("var a,;", &[(1009, 1, 6)]),
        ("for (var of X) {}", &[(1123, 1, 9)]),
        ("var a,; )", &[(1128, 1, 9)]),
        (
            "var export;\nvar class;",
            &[(1389, 1, 5), (1389, 2, 5), (1005, 2, 10)],
        ),
        ("var typeof = 10;", &[(1389, 1, 5), (1109, 1, 12)]),
        // `import` that is not a call or `import.meta` is left for the import declaration
        // after it (importCallExpressionIncorrect2); a bad specifier ends the named import
        // list (es6ImportNamedImportParsingError).
        ("var x = import { foo } from \"m\";", &[(1109, 1, 9)]),
        (
            "import { * } from \"m\";",
            &[(1003, 1, 10), (1141, 1, 10), (1109, 1, 12), (1434, 1, 14)],
        ),
        // A binary operator starts an expression statement with a missing left operand.
        ("let a = 1;\n* 1;\n>= 2;", &[(1109, 2, 1), (1109, 3, 1)]),
        (
            "<<<<<<<<\n",
            &[
                (1109, 1, 1),
                (1109, 1, 3),
                (1109, 1, 5),
                (1109, 1, 7),
                (1109, 1, 9),
            ],
        ),
        // A missing expression or type at the end of the file is reported just after the
        // last token; `'}' expected` is at the end.
        ("let a = 1 +\n  // c\n", &[(1109, 1, 12)]),
        ("class C {\n  x = \n\n", &[(1109, 2, 6), (1005, 4, 1)]),
        ("let a: \n\n", &[(1110, 1, 7)]),
        // Merge conflict markers are trivia (TS1185); the other side is skipped.
        (
            "class C {\n<<<<<<< HEAD\n  v = 1;\n=======\n  v = 2;\n>>>>>>> b\n}",
            &[(1185, 2, 1), (1185, 4, 1), (1185, 6, 1)],
        ),
        (
            "a();\n  ||||||| base\nb c;\n=======\nd e;\n>>>>>>> x\nf g;",
            &[(1185, 2, 3), (1185, 4, 1), (1185, 6, 1), (1434, 7, 1)],
        ),
        (
            "x;\n<<<<<<<\n",
            &[
                (1109, 2, 1),
                (1109, 2, 3),
                (1109, 2, 5),
                (1110, 2, 8),
                (1109, 2, 8),
                (1005, 3, 1),
            ],
        ),
        // An empty type argument is `Type expected.` and the list stays type arguments.
        ("Foo<a,,b>();", &[(1110, 1, 7)]),
        // The name `constructor` always starts a constructor.
        ("class C {\n  prop = 42;\n  constructor\n}", &[(1005, 4, 1)]),
        ("class C { constructor! }", &[(1005, 1, 22), (1109, 1, 24)]),
        // A keyword key without `:` still takes the value after it.
        ("x = {return 0;};", &[(1005, 1, 13), (1005, 1, 14)]),
        // An imported binding that is a reserved word or a string.
        (
            "import { default } from \"m\";\nimport { a as if } from \"m\";",
            &[(1003, 1, 10), (1003, 2, 15)],
        ),
        // A module specifier that is not a string is an expression, then TS1141 on it.
        (
            "import { a }, from \"m\";",
            &[(1005, 1, 13), (1141, 1, 13), (1005, 1, 20)],
        ),
        ("import x from 1 + 2;", &[(1141, 1, 15)]),
        ("import x from;", &[(1109, 1, 14)]),
        // A type member must be a name followed by `(`, `<`, `?`, `:`, `,`, `;`, `}` or a
        // line break; otherwise TS1131 ends the list where a statement can start.
        (
            "interface I {\n    var x: T<>;\n}",
            &[(1131, 2, 5), (1128, 3, 1)],
        ),
        (
            "interface I { x y z }\nlet q;",
            &[(1131, 1, 15), (1434, 1, 17), (1128, 1, 21)],
        ),
        ("interface I { async?: boolean; readonly x }", &[]),
        // A token that cannot start an array binding element is TS1181 and ends the list.
        ("let [1] = x;", &[(1181, 1, 6), (1005, 1, 7), (1128, 1, 9)]),
        // U+FFFD outside a string or comment: the file is binary, the rest is skipped.
        (
            "let a = 1; \u{FFFD} foo bar;",
            &[(1490, 1, 1), (1128, 1, 12)],
        ),
        ("var y = x:number => x*x;", &[(1005, 1, 10), (1005, 1, 18)]),
        ("class A { static const H = 1; }", &[(1248, 1, 24)]),
        (
            "class C {\n    public const var export foo = 10;\n\n    var constructor() { }\n}",
            &[
                (1440, 2, 18),
                (1068, 4, 5),
                (1005, 4, 20),
                (1005, 4, 23),
                (1128, 5, 1),
            ],
        ),
        // Arrows and bodies (arrowFunctionsMissingTokens, parserErrantEqualsGreaterThan*,
        // parseErrorIncorrectReturnToken, asyncArrowFunction5_es6).
        ("var a = () { };", &[(1005, 1, 12)]),
        ("var b = (): void;", &[(1005, 1, 17)]),
        ("var c = (x);", &[]),
        (
            "namespace N {\n    var a = () => var k = 10;};\n}",
            &[(1005, 2, 19)],
        ),
        ("function f(p: A) => p;", &[(1144, 1, 18)]),
        (
            "type F1 = {\n    (n: number) => string;\n}",
            &[(1005, 2, 17)],
        ),
        (
            "var foo = async (await): Promise<void> => {\n}",
            &[(1359, 1, 18)],
        ),
        (
            "let o = {\n    m(n: number) => string {\n        return n.toString();\n    }\n};",
            &[(1005, 2, 18), (1434, 2, 21), (1128, 5, 1)],
        ),
        // Object and array literals (parserErrorRecovery_ObjectLiteral*,
        // missingCloseBraceInObjectLiteral, templateStringInPropertyName2).
        (
            "var v = { a: \nreturn;",
            &[(1109, 2, 1), (1005, 2, 7), (1005, 2, 8)],
        ),
        (
            "var v = { a\nreturn;",
            &[(1005, 2, 1), (1005, 2, 7), (1005, 2, 8)],
        ),
        ("var foo = {\n    a: 'a'\n\n", &[(1005, 4, 1)]),
        ("var a = [\"1\",\"2\"", &[(1005, 1, 17)]),
        (
            "var x = {\n    `abc${ 123 }def`: 321\n}",
            &[(1136, 2, 5), (1005, 2, 21), (1134, 2, 23), (1128, 3, 1)],
        ),
        // Accessor and constructor grammar, kept only in files without syntax errors
        // (accessorWithoutBody1, abstractPropertyNegative, parserConstructorDeclaration12,
        // accessorWithRestParam, optionalSetterParam, computedPropertyNames49).
        ("var v = { get foo() }", &[(1005, 1, 19)]),
        ("class C { get x(): string; }", &[(1005, 1, 26)]),
        ("var v = { get foo() } +", &[(1109, 1, 24)]),
        ("class C<> {}", &[(1098, 1, 8)]),
        ("foo<>();", &[(1099, 1, 4)]),
        (
            "class C { constructor<>() { } }",
            &[(1098, 1, 22), (1092, 1, 23)],
        ),
        ("class C { constructor(): number { } }", &[(1093, 1, 26)]),
        ("class C { set foo<T>(v) { } }", &[(1094, 1, 15)]),
        ("class C { get foo(a) { return 1; } }", &[(1054, 1, 15)]),
        ("class C { set [1 + 1]() { } }", &[(1049, 1, 15)]),
        ("class C { set X(v): number { } }", &[(1095, 1, 15)]),
        ("class C { set X(...v) { } }", &[(1053, 1, 17)]),
        ("class C { set X(v?: any) { } }", &[(1051, 1, 18)]),
        ("class C { set X(v = 0) { } }", &[(1052, 1, 15)]),
        (
            "class C { set X(this: C, v) { } get g(this: C) { return 1; } }",
            &[],
        ),
        ("declare class D { get x(): number; set x(v); }", &[]),
        // A declaration keyword before a token that is no name (interfacesWithPredefinedTypesAsNames,
        // reservedNamesInAliases, parseInvalidNames, ambientModuleDeclarationWith...DottedPath).
        ("interface void {}", &[(2427, 1, 11)]),
        ("type void = I;", &[(2457, 1, 6), (1109, 1, 11)]),
        ("namespace 100 {}", &[(2819, 1, 11), (1005, 1, 15)]),
        (
            "declare namespace debugger {}",
            &[(2819, 1, 19), (1005, 1, 28)],
        ),
        // `using` with a pattern (usingDeclarations.5/6, usingDeclarationsInForOf.3).
        ("{ using {a} = null; }", &[(1492, 1, 9)]),
        ("for (using {} of []) {}", &[(1492, 1, 12)]),
        ("{ using a = null, [b] = null; }", &[(1492, 1, 19)]),
        // Unicode escapes (unicodeExtendedEscapesInStrings*, invalidUnicodeEscapeSequance*):
        // every bad escape of a string is reported, at the digits or just after them.
        ("var x = \"\\u{110000}\";", &[(1198, 1, 13)]),
        ("var x = \"\\u{}\";", &[(1125, 1, 13)]),
        ("var x = \"\\u{67\";", &[(1199, 1, 15)]),
        ("var x = \"\\u{00000000000067", &[(1199, 1, 27)]),
        (
            "var x = \"\\u{r}\\u{n}\\u{t}\";",
            &[(1125, 1, 13), (1125, 1, 18), (1125, 1, 23)],
        ),
        ("var arg\\u003", &[(1127, 1, 8)]),
        ("a\\u", &[(1127, 1, 2)]),
        // A reserved word as a name is TS1359 (parserEnumDeclaration4,
        // objectBindingPatternKeywordIdentifiers02); after `class` it is '{' expected
        // (classWithPredefinedTypesAsNames2).
        ("enum void {}", &[(1359, 1, 6)]),
        ("class void {}", &[(1005, 1, 7)]),
        ("function function() { }", &[(1359, 1, 10), (1003, 1, 18)]),
        (
            "var { while: while } = { while: 1 }",
            &[(1359, 1, 14), (1005, 1, 20)],
        ),
        // Decorators after modifiers or after a property (decoratorOnClassMethod3/17).
        (
            "class C {\n    public @dec method() {}\n}",
            &[(1436, 2, 12)],
        ),
        ("class C {\n    public @dec prop;\n}", &[(1436, 2, 12)]),
        (
            "class C {\n    private prop @dec\n    foo() { return 0; }\n}",
            &[(1436, 2, 18)],
        ),
        // A missing name after `.` at the end of the file or before a declaration on the
        // next line is TS1003 just after the `.` (enumMemberResolution).
        ("var x = A. // e\nvar y = 1;", &[(1003, 1, 11)]),
        ("var p2 = window. ", &[(1003, 1, 17)]),
        ("var x: M.\nnamespace N {\n}", &[(1003, 1, 10)]),
        ("a.\n  b(c);", &[]),
        // `public {};` (classMemberWithMissingIdentifier).
        (
            "class C {\n    public {};\n}",
            &[(1146, 2, 11), (1005, 2, 12), (1128, 3, 1)],
        ),
        ("function sum (...) {}", &[(1003, 1, 18)]),
        ("(...) => 105;", &[(1003, 1, 5)]),
        ("(readonly as number);", &[]),
        ("(async bar => bar);", &[]),
        ("interface I implements A {\n}", &[(1176, 1, 13)]),
        ("default function () {\n}", &[(1005, 1, 1), (1003, 1, 18)]),
        ("default abstract class C {}", &[(1005, 1, 1)]),
        // Checker grammar errors, kept in clean files.
        ("class D extends C, {\n}", &[(1009, 1, 18)]),
        ("@dec\ndefault class {}", &[(1029, 2, 1)]),
        ("interface F {\n    bar: number = 5;\n}", &[(1246, 2, 19)]),
        ("var F: {\n    bar: number = 5;\n};", &[(1247, 2, 19)]),
        ("try {\n}\ncatch (e = 1) {\n}", &[(1197, 3, 12)]),
        (
            "switch (x) {\n  default:\n  default:\n  default:\n}",
            &[(1113, 3, 3)],
        ),
        // Private names as keys outside a class: TS18016, deferred; member access is the
        // checker's (privateNameBadDeclaration, privateNameAccessorsAccess).
        ("const o = {\n    #foo: 1\n};", &[(18016, 2, 5)]),
        (
            "o = { #m() {}, get #p() { return 1 } };",
            &[(18016, 1, 7), (18016, 1, 20)],
        ),
        ("interface A {\n    #foo: string;\n}", &[(18016, 2, 5)]),
        ("new A().#x;", &[]),
        ("class C { #x = 1; m() { return { y: this.#x }; } }", &[]),
        // A private name in an optional chain: TS18030 (privateIdentifierChain.1).
        (
            "class C { #x; m() { this?.#x; this?.a!.#x; this?.f().#x; (this?.a).#x; } }",
            &[(18030, 1, 27), (18030, 1, 40), (18030, 1, 54)],
        ),
        // `await` is a type name in an async function (asyncArrowFunction10_es2017).
        ("async function f() { var v: await; }", &[]),
        // Jump targets and `return` (grammar.rs; invalidDoWhileBreakStatements,
        // classStaticBlock8, invalidContinueInDownlevelAsync, parserWithStatement2).
        ("break;", &[(1105, 1, 1)]),
        ("continue;", &[(1104, 1, 1)]),
        ("switch (1) { case 1: continue; }", &[(1104, 1, 22)]),
        ("ONE:\ndo break TWO; while (true)", &[(1116, 2, 4)]),
        ("L: if (1) { continue L; }", &[(1115, 1, 13)]),
        (
            "T: do { var x = () => { break T; } } while (1)",
            &[(1107, 1, 25)],
        ),
        (
            "async function f() { if (1) { continue; } }",
            &[(1107, 1, 31)],
        ),
        (
            "while (1) { class C { static { break; } } }",
            &[(1107, 1, 32)],
        ),
        (
            "outer: break outer;\nl: while (1) { if (1) break l; continue l; }\nswitch (1) { default: break; }",
            &[],
        ),
        ("a: b: while (1) { continue a; }", &[]),
        ("return;", &[(1108, 1, 1)]),
        (
            "function f() { return 1; }\nclass C { static { return; } }",
            &[(18041, 2, 20)],
        ),
        ("with (1)\n  return;", &[]),
        // Ambient blocks and labelled declarations (ambientWithStatements, letDeclarations-scopes).
        (
            "declare namespace M {\n    break;\n    continue;\n    debugger;\n    L: var y;\n}",
            &[(1036, 2, 5), (1104, 3, 5), (1344, 5, 5)],
        ),
        (
            "declare module \"x\" {\n  export interface i1 { };\n  namespace N { x = 1; }\n}",
            &[(1036, 2, 26), (1036, 3, 17)],
        ),
        ("namespace Q { var x = 1; ; }", &[]),
        ("label2: label3: label4: const c9 = 0;", &[(1344, 1, 17)]),
        // In a file with a syntax error none of these are reported.
        ("break; (", &[(1109, 1, 9)]),
        // Declarator initializers (constDeclarations-errors, declarationWithNoInitializer,
        // parserForInStatement3/4, parserForOfStatement3/4, usingDeclarations.8).
        (
            "const c3, c4 = 1, c5: string;",
            &[(1155, 1, 7), (1155, 1, 19)],
        ),
        ("for (const c9; c9 < 1;) { }", &[(1155, 1, 12)]),
        ("declare const x: number;\nfor (const a of [1]) {}", &[]),
        (
            "var [a, b];\nvar {c}: { c: any }, d: any;",
            &[(1182, 1, 5), (1182, 2, 5)],
        ),
        ("{ using x; }", &[(1155, 1, 9)]),
        ("for (var a, b of X) { }", &[(1188, 1, 13)]),
        ("for (var a = 1, b = 2 in X) { }", &[(1091, 1, 17)]),
        ("for (var a = 1 of X) { }", &[(1190, 1, 10)]),
        ("for (var a = 1 in X) { }", &[(1189, 1, 10)]),
        // Rest parameters (restParameterNotLast).
        ("function f(...x, y) { }", &[(1014, 1, 12)]),
        (
            "type F = (...a: any[], ...b: any[]) => void;",
            &[(1014, 1, 11)],
        ),
        // Ambient contexts (ambientErrors, ambientGetters, constDeclarations-ambient-errors,
        // generatorInAmbientContext*, accessorBodyInTypeContext,
        // esDecorators-classDeclaration-fields-staticAmbient).
        (
            "declare var x = 4;\ndeclare const c1: boolean = true;\ndeclare const c3 = null, ok = true, ok2 = -1, ok3 = E.A;",
            &[(1039, 1, 17), (1039, 2, 29), (1254, 3, 20)],
        ),
        (
            "declare class C {\n    get X() { return 1; }\n    *g(): any;\n    static readonly r = \"a\";\n    x = 1;\n}",
            &[(1183, 2, 13), (1221, 3, 5), (1039, 5, 9)],
        ),
        (
            "declare function *g(): any;\ndeclare function f() { }",
            &[(1221, 1, 18), (1183, 2, 22)],
        ),
        (
            "type A = {\n    get foo() { return 0 }\n};",
            &[(1183, 2, 15)],
        ),
        ("class D { declare foo = 1; }", &[(1039, 1, 25)]),
        // A `declare` method body (not an accessor's or a constructor's).
        (
            "class D {\n    declare m() { }\n    declare *g() { }\n    declare get x() { return 1 }\n    declare constructor() { }\n}",
            &[(1183, 2, 17), (1183, 3, 18)],
        ),
        ("class D { @dec static declare foo = 1; }", &[]),
        // Parameter lists, one error each (optionalArgsWithDefaultValues,
        // fatarrowfunctionsOptionalArgsErrors1, restParamAsOptional, restParamModifier2).
        (
            "function foo(x: number, y?:boolean=false, z?=0) {}",
            &[(1015, 1, 25)],
        ),
        ("(arg1?, arg2) => 101;", &[(1016, 1, 9)]),
        ("(arg1 = 1, arg2) => 1;", &[]),
        ("(...arg?) => 102;", &[(1047, 1, 8)]),
        ("function f2(...x = []) { }", &[(1048, 1, 16)]),
        (
            "class C {\n    constructor(public ...rest: string[]) {}\n}",
            &[(1317, 2, 17)],
        ),
        // Index signatures (indexSignatureTypeCheck, parserIndexSignature6/9/11,
        // indexSignatureWithInitializer1, arraySigChecking).
        (
            "interface I {\n    [p2?: string];\n    [...p3: any[]];\n    [p4: string, p5?: string];\n    []: number;\n    [p1: string];\n    [a:boolean]\n    [x: number = 1]: number;\n}",
            &[
                (1019, 2, 8),
                (1017, 3, 6),
                (1096, 4, 6),
                (1096, 5, 5),
                (1021, 6, 5),
                (1268, 7, 6),
                (1020, 8, 6),
            ],
        ),
        ("var foo: { [index: T]; };", &[]),
        // Heritage clauses (extendsClauseAlreadySeen2, parserClassDeclaration4,
        // implementClausePrecedingExtends, classExtendsMultipleBaseClasses).
        (
            "class D<T> extends C<number> extends C<string> { }",
            &[(1172, 1, 30)],
        ),
        (
            "class C extends A implements B extends C { }",
            &[(1172, 1, 32)],
        ),
        ("class D implements C extends C { }", &[(1173, 1, 22)]),
        ("class C extends A,B { }", &[(1174, 1, 19)]),
        ("class C implements A implements B { }", &[(1175, 1, 22)]),
        // Declarations as substatements (constDeclarations-invalidContexts,
        // typeInterfaceDeclarationsInBlockStatements1, usingDeclarations.10).
        (
            "if (true)\n    const c1 = 0;\nwhile (false)\n    l: const c9 = 0;\nif (x) type s = string;\n{ l2: const c = 0; }\nwith (o)\n    let y = 0;",
            &[
                (1156, 2, 5),
                (1344, 4, 5),
                (1156, 4, 8),
                (1156, 5, 13),
                (1344, 6, 3),
            ],
        ),
        ("if (true)\nexport const c = 1;", &[]),
        // Enum computed names: reported next to syntax errors (parserComputedPropertyName30).
        (
            "enum E {\n  [e] = 1, [\"4\"] = 4, [2] = 2\n}",
            &[(1164, 2, 3), (2452, 2, 12), (2452, 2, 23)],
        ),
        // Import types (importTypeGeneric, importTypeNonString).
        (
            "type A = import(T).Foo;\ntype B = import(\"./a\").X;",
            &[(1141, 1, 17)],
        ),
        // Module elements in the wrong place (moduleElementsInWrongContext,
        // labeledStatementWithLabel, quotedModuleNameMustBeAmbient,
        // exportDefaultClassInNamespace, es6ModuleInternalNamedImports2,
        // exportDeclarationsInAmbientNamespaces2, importInsideModule).
        (
            "{\n    export namespace N { }\n    declare module \"a\" { }\n    export = M;\n    export { foo };\n    export default v;\n    import I = M;\n}\nlabel: namespace L { }",
            &[
                (1235, 2, 5),
                (1234, 3, 5),
                (1231, 4, 5),
                (1233, 5, 5),
                (1258, 6, 5),
                (1232, 7, 5),
                (1235, 9, 8),
            ],
        ),
        ("module 'M' {}\ndeclare module 'M2' {}", &[(1035, 1, 8)]),
        // A misplaced `module N` is not TS1540 too; `declare global` is an ambient module, and
        // a namespace's re-export is reported at its specifier (misplaced_module_elements,
        // namespace_module_statements parity fixtures).
        (
            "{\n    module C { }\n    declare global { }\n}\nnamespace N {\n    export { v } from \"m\";\n}",
            &[(1235, 2, 5), (1234, 3, 5), (1194, 6, 23)],
        ),
        (
            "namespace A {\n    export default class {}\n    export { x };\n    import f = require(\"f\");\n    import g = B.C;\n}\ndeclare namespace D {\n    export { y };\n    export { y } from \"m\";\n}\ndeclare module \"m\" { export { z } from \"n\"; }",
            &[(1319, 2, 12), (1194, 3, 5), (1147, 4, 24), (1194, 9, 23)],
        ),
        ("var x;\nexport export = x;", &[(1120, 2, 1)]),
        ("module not.ok { }", &[(1540, 1, 8), (1540, 1, 12)]),
        // Statements (emptyThenWarning, awaitInNonAsyncFunction, classStaticBlock23,
        // dynamicImportTrailingComma).
        ("if (1);\nif (2); else {}", &[(1313, 1, 7), (1313, 2, 7)]),
        (
            "function f() { for await (const x of []); }\nclass C { static { for await (const x of []); } }\nasync function g() { for await (const x of []); }",
            &[(1103, 1, 20), (18038, 2, 24)],
        ),
        // A trailing comma in an import call is the checker's (it depends on `module`).
        ("import(p,);\nimport(p, {},);", &[]),
        // Import calls take an argument list (importAssertion1, importCallExpressionSpread).
        (
            "import(...a);\nimport();\nimport('./0', {}, {});",
            &[(1325, 1, 8)],
        ),
        // Members (generatorOverloads1/3, definiteAssignmentAssertions, classAbstractAccessor,
        // indexSignatureWithTrailingComma).
        (
            "function* f(): any;\nclass C {\n    *m(): any;\n    a! = 1;\n    static c!: number;\n    d!;\n    e!: number;\n}\nlet v!;\ndeclare let w!: number;",
            &[
                (1222, 1, 9),
                (1222, 3, 5),
                (1263, 4, 6),
                (1255, 5, 13),
                (1264, 6, 6),
                (1264, 9, 6),
                (1255, 10, 14),
            ],
        ),
        (
            "abstract class A {\n    abstract get a();\n    abstract get b() { return 1; }\n}",
            &[(1318, 3, 18)],
        ),
        ("type A = { [key: string,]: string };", &[(1025, 1, 24)]),
        // `<<` opens type arguments whose first type is generic (`F<<T>(x: T) => T>`).
        ("type R = ReturnType<<T>(x: T) => number>;", &[]),
        // `export type` not followed by a name: a stray `export` (parseInvalidNames).
        (
            "export type 100 {}",
            &[(1128, 1, 1), (2457, 1, 13), (1005, 1, 17)],
        ),
        // Rest elements in binding patterns: checker grammar errors, kept in clean files.
        ("const { ...a: b } = {};", &[(2566, 1, 15)]),
        ("var [...x = a] = a;", &[(1186, 1, 11)]),
        ("var { ...x = a } = a;", &[(1186, 1, 12)]),
        // A rest element followed by more elements (restElementMustBeLast).
        ("var [...a, x] = [1];", &[(2462, 1, 9)]),
        ("var { ...a, x, ...b } = { x: 1 };", &[(2462, 1, 10)]),
        ("var [...a,] = [1];", &[(1013, 1, 10)]),
        // Numeric literals followed by a name (parseErrorIncorrectReturnToken,
        // identifierStartAfterNumericLiteral).
        ("1.toString();", &[(1351, 1, 3)]),
        ("let c = 3xy + 1;", &[(1351, 1, 10), (1005, 1, 13)]),
        // Octal literals after `-` (literals, es5-oldStyleOctalLiteralInEnums).
        ("var n = -003;", &[(1121, 1, 9)]),
        ("var n = 1 - 01;", &[(1121, 1, 12)]),
        // A generator member without a name (FunctionPropertyAssignments*_es6).
        ("var v = { *() { } }", &[(1003, 1, 12)]),
        ("var v = { * }", &[(1003, 1, 13)]),
        // A tagged template statement without `;` (templateStringInModuleName).
        ("declare module `M1` {\n}", &[(1443, 1, 16)]),
        ("foo `y` {}", &[(1443, 1, 5)]),
        // `Identifier expected.` at the end of the file is reported after the last token.
        ("function\n", &[(1003, 1, 9), (1005, 2, 1)]),
        ("x = a?.\n", &[(1003, 2, 1)]),
        // A line break after `throw`: TS1142 at the end of `throw`.
        ("throw\na;", &[(1142, 1, 6)]),
        // Meta-property names: the checker's grammar errors (misspelledNewMetaProperty).
        ("function foo(){new.targ}", &[(17012, 1, 20)]),
        ("import.foo();", &[(18061, 1, 8)]),
        ("function g(){new.}", &[(1003, 1, 18)]),
        // A tagged template in an optional chain (taggedTemplateChain).
        ("a?.`b`;\na?.b`c${1}`;", &[(1358, 1, 4), (1358, 2, 5)]),
        // A decorator on an expression other than a class (decoratorOnArrowFunction).
        ("var F = @dec () => {\n}", &[(1109, 1, 16), (1005, 1, 17)]),
        // A lone `#` is an invalid character but still a private name.
        ("class D { # m() {} }", &[(1127, 1, 11), (1005, 1, 13)]),
        // `import defer` takes only a namespace import; a name not followed by `,` or
        // `from` makes an `import x =` (classAbstractManyKeywords).
        ("import defer x from \"a\";", &[(18058, 1, 8)]),
        ("import defer { a } from \"a\";", &[(18059, 1, 8)]),
        ("import abstract class D {}", &[(1005, 1, 17)]),
        ("import y\nlet z = 1;", &[(1005, 2, 1), (1005, 2, 5)]),
        // `import` followed by a keyword: no clause, so no `from`; the specifier is an
        // expression (importCallExpressionIncorrect1).
        ("import\nimport { foo } from './0';", &[(1109, 2, 1)]),
        ("import class C {}", &[(1141, 1, 14)]),
        // TS1011 just after `[` (badArraySyntax).
        ("var a = new Z[   ];", &[(1011, 1, 15)]),
        // `import q = null;`: TS1359, the word is left for the next statement.
        ("import t = void;", &[(1359, 1, 12), (1109, 1, 16)]),
        // `using` grammar errors (usingDeclarations.13/16/17, awaitUsingDeclarations.14).
        (
            "export using x = null;\ndeclare using y: null;",
            &[(1491, 1, 1), (1491, 2, 1)],
        ),
        (
            "declare namespace N {\n    using x: null;\n}",
            &[(1545, 2, 5)],
        ),
        (
            "switch (0) {\n  case 0:\n    using d = null;\n}",
            &[(1547, 3, 5)],
        ),
        ("for (using x in {}) {}", &[(1493, 1, 6)]),
        (
            "class C { static { await using x = null; } }",
            &[(18054, 1, 20)],
        ),
        // `for (using of of [])` iterates `using` (usingDeclarationsInForOf.2).
        ("for (using of of []) {}", &[(1011, 1, 19)]),
        // Constructor accessors and generators (constructorNameInAccessor/Generator).
        (
            "class C { get constructor() { return 1 } *constructor() {} }",
            &[(1341, 1, 15), (1368, 1, 43)],
        ),
        ("class C { get \"constructor\"() { return 1 } }", &[]),
        // A required tuple element after an optional one (optionalTupleElements1).
        (
            "type A = [string?, ...number[], string, number];",
            &[(1257, 1, 33)],
        ),
        // Assignment patterns (trailingCommasInBindingPatterns, restElementWithInitializer2).
        (
            "let c, d;\n([...c,] = []);\n({...d,} = {});",
            &[(1013, 2, 7), (1013, 3, 7)],
        ),
        ("[...x = a] = a;", &[(1186, 1, 7)]),
        ("[...a, x] = [1];", &[(2462, 1, 2)]),
        ("({ ...a, x } = { x: 1 });", &[(2462, 1, 4)]),
        // No TS1013 in an ambient context.
        ("declare function f25(...args,): void;", &[]),
        // Type predicates outside a function's return type (assertionTypePredicates1).
        ("var x: this is string;", &[(1228, 1, 8)]),
        (
            "declare let Q: new (x: unknown) => x is string;",
            &[(1228, 1, 36)],
        ),
        (
            "declare function f(x: asserts this): void;",
            &[(1228, 1, 23)],
        ),
        (
            "declare class W { get p(): asserts this is string; }",
            &[(1228, 1, 28)],
        ),
        ("type A = asserts;\ntype B = asserts[];", &[]),
        // `super` needs `(`, `.` or `[`; type arguments on it are TS2754 (superErrors).
        (
            "class A extends B { m() { var x = super; var y = super?.x; } }",
            &[(1034, 1, 40), (1034, 1, 55)],
        ),
        (
            "class A extends B { m() { super<T>(0); } }",
            &[(2754, 1, 32)],
        ),
        // `=` right after a `}` (assignmentLHSIsValue, destructionAssignmentError).
        ("{ a: 0} = value;", &[(2809, 1, 9)]),
        ("() => { } = value;", &[(2809, 1, 11), (1005, 1, 13)]),
        ("class A { m() {} = 1 }", &[(2809, 1, 18)]),
        ("x;\n= value;", &[(1128, 2, 1)]),
        // Only a block's `}` makes TS2809 (`var {a, %} = x;` is TS1128 at the `=`).
        (
            "var {a, %} = x;",
            &[(1180, 1, 9), (1109, 1, 10), (1128, 1, 12)],
        ),
        // Object binding patterns skip tokens no enclosing list uses
        // (destructuringObjectBindingPatternAndAssignment3).
        ("var {h?} = { h?: 1 };", &[(1005, 1, 7)]),
        (
            "var {a ) = x;",
            &[(1005, 1, 8), (1180, 1, 10), (1005, 1, 13)],
        ),
        ("function f({a ?}) {}", &[(1005, 1, 15), (1005, 1, 16)]),
        ("function f({ a; b }) {}", &[(1005, 1, 15)]),
        // Checker grammar errors in object literals.
        ("var q = { h?: 1 };", &[(1162, 1, 12)]),
        ("const foo = { a! }", &[(1255, 1, 16)]),
        // tsc takes one `?` or `!` after any object member's name (plainJSGrammarErrors).
        (
            "const o = { m!() { return 1 }, p!: 2 }",
            &[(1255, 1, 14), (1255, 1, 33)],
        ),
        // `await` bindings directly in a class static block (classStaticBlock22).
        ("class C { static { let await: any; } }", &[(1359, 1, 24)]),
        (
            "class C { static { let { await } = x; let { await: o } = x; } }",
            &[(1359, 1, 26)],
        ),
        (
            "class C { static { (function await() {}); (() => await); } }",
            &[(1359, 1, 30)],
        ),
        // `import<T>(...)` (importCallExpressionWithTypeArgument).
        (
            "var p1 = import<Promise<any>>(\"./0\");\nvar p2 = import<>(\"./0\");",
            &[(1326, 1, 10), (1326, 2, 10)],
        ),
        // TS1013 is the checker's: not with syntax errors.
        (
            "const [...a,] = [];\nlet c;\n([...c,] = []);",
            &[(1013, 1, 12), (1013, 3, 7)],
        ),
        ("const [...a,] = [];\nlet = ;", &[(1109, 2, 7)]),
        // `{ s = 5 }` only in a destructuring target (shorthandPropertyAssignmentsIn-
        // Destructuring).
        (
            "let a = { s = 5 };\nlet g = { h: { k = 1 } };",
            &[(1312, 1, 13), (1312, 2, 18)],
        ),
        (
            "({ z = 1 } = {});\n({ q: { r = 2 } = {} } = {});\nfor ({ w = 3 } of []) {}\nlet f = ({ u = 4 }) => u;",
            &[],
        ),
        // `async (a = await)` stays an arrow function (asyncArrowFunction6).
        (
            "var foo = async (a = await): Promise<void> => {\n}",
            &[(1109, 1, 27)],
        ),
        // Object literals in heritage clauses (classExtendsEveryObjectType).
        ("class C2 extends { foo: string; } { }", &[(1005, 1, 31)]),
        ("interface I extends { a: number }", &[(1005, 1, 34)]),
        // Private names as parameters (privateNamesNotAllowedAsParameters).
        (
            "class A { setFoo(#foo: string) {} }\nfunction f(#a?) {}",
            &[(18009, 1, 18), (18009, 2, 12)],
        ),
        // `C[)]`: an array type missing `]` (privateNamesAndIndexedAccess).
        ("type T = C[)];", &[(1005, 1, 12), (1128, 1, 13)]),
        // `*` cannot follow `get`/`set` (canFollowGetSetKeyword).
        (
            "class A {\n    get\n    *x() {}\n}\nconst c = {\n    get\n    *x() {}\n};",
            &[(1005, 7, 5)],
        ),
        // A decorated `using` is a declaration (decoratorOnUsing).
        (
            "declare function dec<T>(target: T): T;\n@dec\nusing 1\n\n@dec\nusing x",
            &[(1134, 3, 7)],
        ),
        // Enum member recovery (decoratorOnEnum2, parserEnum5).
        (
            "enum E {\n    @dec A\n}",
            &[(1132, 2, 5), (1146, 2, 9), (1128, 3, 1)],
        ),
        (
            "enum E1 { a, b: 1, c, d: 2 = 3 }",
            &[(1357, 1, 15), (2452, 1, 17), (1357, 1, 24), (2452, 1, 26)],
        ),
        (
            "enum X { 1 = 1, [2] = 2, \"3\" = 3, \"01\" = 4, 5n = 5, \"NaN\" = 6 }",
            &[(2452, 1, 10), (2452, 1, 17), (2452, 1, 26), (2452, 1, 45)],
        ),
        // A misplaced `#!` skips only the `#`; regex flag errors are the checker's
        // (shebangError).
        (
            "var foo = 1;\n#!/usr/bin/env node",
            &[(18026, 2, 1), (1005, 2, 16)],
        ),
        ("var x = /usr/bin;", &[(1499, 1, 14), (1499, 1, 16)]),
        // Regular expression patterns (unicodeExtendedEscapesInRegularExpressions*,
        // regularExpressionCharacterClassRangeOrder); kept only without syntax errors.
        ("var x = /\\u{110000}/gu;", &[(1198, 1, 13)]),
        (
            "var x = /\\u{r}\\u{n}/gu;",
            &[(1125, 1, 13), (1508, 1, 14), (1125, 1, 18), (1508, 1, 19)],
        ),
        ("var x = /\\u{}/gu;", &[(1125, 1, 13)]),
        (
            "/[\\u{1D621}-\\u{1D608}]/;",
            &[(1538, 1, 3), (1517, 1, 3), (1538, 1, 13)],
        ),
        ("/[b-a]/; /[\\d-a]/; /a{2}\\p{L}/u;", &[(1517, 1, 3)]),
        ("/[b-a]/;\nlet = ;", &[(1109, 2, 7)]),
        // An unterminated regex ends at a closer it did not open (parserRegularExpression-
        // DivideAmbiguity4) and is dropped at the position of the error before it
        // (parser645086_1).
        ("foo(/a)b);", &[(1161, 1, 5), (1005, 1, 8), (1128, 1, 9)]),
        ("foo(/notregexp);", &[(1161, 1, 5)]),
        ("var v = /[]/]/", &[(1005, 1, 13), (1134, 1, 14)]),
        // Every invalid escape of an untagged template (unicodeExtendedEscapesInTemplates17).
        ("var x = `\\u{r}\\u{n}`;", &[(1125, 1, 13), (1125, 1, 18)]),
        // Unbalanced groups and `v`-mode classes in a regex (parser579071,
        // regExpWithOpenBracketInCharClass).
        (
            "var x = /fo(o/;\nvar y = /a)/;",
            &[(1005, 1, 14), (1508, 2, 11)],
        ),
        ("/[[]/; /[[]/u; /[[]/v;", &[(1005, 1, 20)]),
        // A substatement at a token that ends its construct is `Expression expected`
        // (parserErrorRecoveryIfStatement4).
        ("function f() {\n  if (a)\n}", &[(1109, 3, 1)]),
        ("if (a) else b;", &[(1109, 1, 8)]),
        (
            "switch (x) { case 1: if (a) case 2: break; }",
            &[(1109, 1, 29)],
        ),
        // An invalid character in a parameter list is skipped (parserErrorRecovery_
        // ParameterList4).
        ("function f(a,\u{ac} b) {\n}", &[(1127, 1, 14)]),
        // `.foo` as a type: a name missing its first part (typeofImportDefer).
        (
            "export type X = typeof import.defer(\"./a\").Foo;\nlet x: .foo;",
            &[(1005, 1, 30), (1005, 1, 36), (1110, 2, 8)],
        ),
        // Escaped keywords are keywords: TS1260 where used as one, not as a name
        // (scannerUnicodeEscapeInKeyword1, switchStatementsWithMultipleDefaults).
        (
            "\\u{0076}ar x = 1;\nvar \\u0061wait = 1;\nvar o = { def\\u0061ult: 1 };",
            &[(1260, 1, 1)],
        ),
        (
            "switch (x) { def\\u0061ult: break; }\nclass C { \\u0073tatic y = 1 }",
            &[(1260, 1, 14), (1260, 2, 11)],
        ),
        // `from` and a string end an export list (unclosedExportClause01).
        ("export { x, from \"./a\"", &[(1005, 1, 13)]),
        ("export { x as a from \"./a\"", &[(1005, 1, 17)]),
        // A misplaced modifier before `export as namespace` (umd-errors).
        ("static export as namespace N;", &[]),
        // A reserved word as a parameter name (reservedWords3,
        // destructuringParameterDeclaration6).
        ("function f(enum) {}", &[(1390, 1, 12), (1003, 1, 16)]),
        ("function f(...while) {}", &[(1359, 1, 15), (1005, 1, 20)]),
        // A non-name after `break` on its line.
        ("while (1) { break 1; }", &[(1003, 1, 19)]),
        // `await` is an operator in a class static block: TS18037 even next to syntax
        // errors (classStaticBlock26); likewise where it is one in an async function.
        (
            "class C { static { await; } }",
            &[(18037, 1, 20), (1109, 1, 25)],
        ),
        (
            "class C { static { ({ await }); class await {} } }",
            &[(1005, 1, 29)],
        ),
        (
            "async function f() { const ff = (await) => {}; }",
            &[(1109, 1, 39), (1005, 1, 41)],
        ),
        ("0_1;", &[(6188, 1, 2)]),
        // `[a?]` is an index signature (parserIndexSignature3).
        ("interface I {\n  [a?]\n}", &[(1019, 2, 5)]),
        // `<in in>`: the second `in` is a missing name (varianceAnnotationsWith-
        // CircularlyReferencesError).
        (
            "type T1<in in> = 1;\ntype T2<in> = 1;",
            &[(1359, 1, 12), (1359, 2, 9)],
        ),
        // Heritage clauses (parserErrorRecovery_ExtendsOrImplementsClause3/5,
        // parserInterfaceDeclaration1).
        ("class C extends implements A {\n}", &[(1097, 1, 16)]),
        (
            "class C extends A, implements B, {\n}",
            &[(1009, 1, 18), (1009, 1, 32)],
        ),
        ("interface I extends A extends B {\n}", &[(1172, 1, 23)]),
        // The false branch of a conditional in a true branch keeps the `:` rule for
        // arrows with a return type (parserArrowFunctionExpression11).
        ("a ? b ? c : (d) : e => f;", &[]),
        // A comma expression as a computed name (parserComputedPropertyName35).
        ("var x = { [0, 1]: {} };", &[(1171, 1, 12)]),
        // `global x`: a global augmentation without a body (nestedGlobalNamespaceInClass).
        ("global x\nglobal\nx", &[(1005, 1, 8)]),
        // `(this: T)` starts an arrow head (jsdocDisallowedInTypescript).
        (
            "function g(f: x(this: number, string): string) {}",
            &[(1005, 1, 16), (1005, 1, 46)],
        ),
        // Labelled tuple members (namedTupleMembersErrors).
        (
            "type A = [a: string?];\ntype B = [b: ...string[]];\ntype C = [...c?: string[]];",
            &[(5086, 1, 14), (5087, 2, 14), (5085, 3, 11)],
        ),
        // Numeric separators before the first digit, and an escape after a number
        // (parser.numericSeparators.hexNegative, decmialNegative).
        ("0x___1;", &[(6188, 1, 3), (6188, 1, 4), (6188, 1, 5)]),
        // An object-literal method without a body before `;` is the checker's `'{' expected`
        // (objectLiteralMemberWithoutBlock1, parser.asyncGenerators.objectLiteralMethods).
        ("var v = { foo(): number; };", &[(1005, 1, 24)]),
        ("var v = { foo(); bar() {} };", &[(1005, 1, 18)]),
        ("var v = { async * x: 1; };", &[(1005, 1, 20)]),
        // Function and constructor types after `|` or `&` need parentheses, from the end of
        // the operator (unparenthesizedFunctionTypeInUnionOrIntersection).
        ("type U = string | () => void;", &[(1385, 1, 18)]),
        ("type I = & new () => any;", &[(1388, 1, 11)]),
        ("type F = () => void | string;", &[]),
        // `in` in a `for await` head (parser.forAwait.es2018).
        (
            "async function f() { for await (const x in y) {} }",
            &[(1005, 1, 41)],
        ),
        // `var` and `implements` as member and class names (convertKeywordsYes); the
        // checker reports the reserved word.
        (
            "class C {\n  public var = 0;\n  public var x = 0;\n}",
            &[(1440, 3, 10)],
        ),
        ("class implements { }", &[]),
        // A field named `constructor` (propertyNamedConstructor) and an initialized
        // abstract field (abstractPropertyInitializer).
        (
            "abstract class C {\n  \"constructor\" = 3;\n  abstract x = 1;\n  abstract z: number;\n}",
            &[(18006, 2, 3), (1267, 3, 12)],
        ),
        // `return` in a class static block (classStaticBlock7).
        (
            "class C { static { return; } }\nclass D { static { () => { return; }; } }",
            &[(18041, 1, 20)],
        ),
    ];
    for &(src, want) in cases {
        let r = parse(src, SourceKind::Ts);
        let got: Vec<(u32, u32, u32)> = r
            .diagnostics
            .iter()
            .map(|d| {
                let before = &src[..d.span.start as usize];
                let line = before.matches('\n').count() as u32 + 1;
                let col = (before.len() - before.rfind('\n').map_or(0, |i| i + 1)) as u32 + 1;
                (d.code, line, col)
            })
            .collect();
        assert_eq!(got, want, "for {src:?}");
    }
}

/// `?` followed by `|` or `&` starts a conditional type's branch, not a JSDoc postfix `T?`
/// (`@types/node` 26 writes `S extends T ? | A | B : never`).
#[test]
fn conditional_branch_with_leading_bar() {
    let dump = ok("type A<S> = S extends string ? | string | number : & { a: 1 };\n");
    has(&dump, "ConditionalType");
    let dump = ok("type B<S> = S extends\n  Array<infer I> ?\n    | string\n    | I\n  : never;\n");
    has(&dump, "ConditionalType");
}

/// Octal and `\8` escapes: TS1487 / TS1488 from the backslash to the end of the escape, in
/// strings (`lenient`: also in `grammar`, which ESLint's parser and build tools accept) and
/// untagged templates (syntax only). Tagged templates allow them; `\0` alone is fine.
#[test]
fn octal_escapes() {
    let errs = |src: &str| {
        let r = parse(src, SourceKind::Ts);
        let show = |ds: &[Diagnostic]| -> Vec<String> {
            ds.iter()
                .map(|d| format!("{}@{}..{} {}", d.code, d.span.start, d.span.end, d.message))
                .collect()
        };
        assert_eq!(r.lenient, r.grammar);
        assert_eq!(
            show(&r.syntax_errors()).len() + r.grammar.len(),
            r.diagnostics.len()
        );
        (show(&r.diagnostics), show(&r.grammar))
    };
    let oct = |at: u32, end: u32, arg: &str| {
        format!("1487@{at}..{end} Octal escape sequences are not allowed. Use the syntax '{arg}'.")
    };
    let (all, grammar) = errs(r#""\05"; '\0'; "\08"; '\477';"#);
    let want = vec![
        oct(1, 4, r"\x05"),
        oct(14, 16, r"\x00"),
        oct(21, 24, r"\x27"),
    ];
    assert_eq!(all, want);
    assert_eq!(grammar, want);
    let (all, grammar) = errs(r#""\8"; "\9x";"#);
    let want = vec![
        r"1488@1..3 Escape sequence '\8' is not allowed.".to_string(),
        r"1488@7..9 Escape sequence '\9' is not allowed.".to_string(),
    ];
    assert_eq!(all, want);
    assert_eq!(grammar, want);
    let (all, grammar) = errs(r"`\05${0}\8`; tag`\05${0}\8`; `\0`;");
    assert_eq!(
        all,
        vec![
            oct(1, 4, r"\x05"),
            r"1488@8..10 Escape sequence '\8' is not allowed.".to_string()
        ]
    );
    assert!(grammar.is_empty());
}

/// Regex flags and named groups newer than some target (TS1501, TS1503) are in `grammar`
/// only: the checker keeps them for older targets. Other regex errors are in both.
#[test]
fn target_regex_errors_are_grammar_only() {
    let r = parse("/(?<a>x)/s; /a/gg;", SourceKind::Ts);
    let codes = |ds: &[Diagnostic]| ds.iter().map(|d| d.code).collect::<Vec<_>>();
    assert_eq!(codes(&r.diagnostics), vec![1500]);
    assert_eq!(codes(&r.grammar), vec![1503, 1501, 1500]);
    assert_eq!(codes(&r.syntax_errors()), Vec::<u32>::new());
}

/// Members after a mapping in a mapped type, and `[P in K]` keys in an interface, type
/// literal or class, are TS7061 (deferred) at the first member's name, once per list. A `,`
/// after the mapping is `';' expected` (tsc 7, observed).
#[test]
fn mapped_type_members_are_ts7061() {
    let all = |src: &str| {
        let r = parse(src, SourceKind::Ts);
        let f = |ds: &[Diagnostic]| {
            ds.iter()
                .map(|d| (d.code, d.span.start))
                .collect::<Vec<_>>()
        };
        (f(&r.diagnostics), f(&r.grammar))
    };
    // 0         1         2         3         4
    // 0123456789012345678901234567890123456789012345
    let (d, g) = all("type A = { [K in X]: 1; b: 2; c(): void };");
    assert_eq!(d, vec![(7061, 24)]);
    assert_eq!(g, d);
    let (d, _) = all("type B = { [K in X]: 1, };");
    assert_eq!(d, vec![(1005, 22)]);
    let (d, g) = all("interface I { a: 1; [P in X]: 1; [Q in X]: 2 }");
    assert_eq!(d, vec![(7061, 14)]);
    assert_eq!(g, d);
    let (d, g) = all("class C { [P in X]: 1; }");
    assert_eq!(d, vec![(7061, 10)]);
    assert_eq!(g, d);
    // Methods with `in` keys are not mapped-type members.
    let (d, _) = all("interface J { [P in X](): void }");
    assert!(d.iter().all(|&(c, _)| c != 7061), "{d:?}");
    ok("type M = { [K in X]: 1; };");
    ok("type N = { readonly [K in X]?: 1 }");
}

/// `tsc_syntax_errors` keeps syntax errors and lenient ones (octal escapes in strings), which
/// `without_grammar` drops, in position order. (A deferred grammar error is kept only
/// without lexer or syntax errors, so it never meets a lenient one.)
#[test]
fn tsc_syntax_errors_keep_lenient() {
    let codes = |ds: &[Diagnostic]| ds.iter().map(|d| d.code).collect::<Vec<_>>();
    let check = |src: &str, syntax: &[u32], tsc: &[u32]| {
        let r = parse(src, SourceKind::Ts);
        assert_eq!(codes(&r.syntax_errors()), syntax, "{src}");
        let got = crate::tsc_syntax_errors(&r.diagnostics, &r.grammar, &r.lenient);
        assert_eq!(codes(&got), tsc, "{src}");
    };
    check("const a; let s = \"\\01\";", &[], &[1487]);
    check(
        "let s = \"\\01\"; let x = 1 +; \"\\8\"",
        &[1109],
        &[1487, 1109, 1488],
    );
    check("let t = `\\01`; const a;", &[1487], &[1487]);
    check("const a;", &[], &[]);
}

/// Unicode escapes in JSX tag and attribute names: decoded (so `<\u0061></a>` closes) and
/// TS17021 on the whole name, a syntax error (tsc 7, unicodeEscapesInJsxtags).
#[test]
fn jsx_name_unicode_escapes_are_ts17021() {
    let errs = |src: &str| {
        let r = parse(src, SourceKind::Tsx);
        r.diagnostics
            .iter()
            .map(|d| (d.code, d.span.start, d.span.end))
            .collect::<Vec<_>>()
    };
    // 0         1         2
    // 012345678901234567890123456
    assert_eq!(errs(r"<\u0061></a>;"), vec![(17021, 1, 7)]);
    assert_eq!(errs(r"<a-\u0063></a-c>;"), vec![(17021, 1, 9)]);
    assert_eq!(errs(r"<x.\u0076ideo />;"), vec![(17021, 3, 13)]);
    assert_eq!(
        errs(r"<v data-\u0076 \u0073rc='' />;"),
        vec![(17021, 3, 14), (17021, 15, 23)]
    );
    assert_eq!(errs(r"<a></\u{61}>;"), vec![(17021, 5, 11)]);
    assert_eq!(errs(r"<a></b>;")[0].0, 17002);
    assert!(errs("<a-b c-d='' />;").is_empty());
}

/// TypeScript-only grammar rules on valid JavaScript (TS1313 `if (x);`, TS1108 a top-level
/// `return`, TS1052 `set x(v = 0)`) are in `grammar` only in a JavaScript file: tsc 7 reports
/// them there only with `checkJs`. A TypeScript file has them in both. Other grammar errors
/// (TS1105 `break` outside a loop) stay in both. Found in @babel/parser, which, ora (ecosystem).
#[test]
fn checked_js_grammar_is_grammar_only_in_javascript() {
    let src = "if (!m) ;else f();\nreturn;\nclass C { set indent(indent = 0) {} }\nvar o = { set c(c = []) {} };";
    let codes = |ds: &[Diagnostic]| ds.iter().map(|d| d.code).collect::<Vec<_>>();
    for kind in [SourceKind::Js, SourceKind::Jsx] {
        let r = parse(src, kind);
        assert_eq!(codes(&r.diagnostics), Vec::<u32>::new(), "{kind:?}");
        assert_eq!(codes(&r.grammar), vec![1313, 1108, 1052, 1052], "{kind:?}");
        let r = crate::parse_with(
            src,
            kind,
            crate::ParseOptions {
                ts_in_js: crate::TsInJs::Report,
                ..Default::default()
            },
        );
        assert_eq!(codes(&r.diagnostics), Vec::<u32>::new(), "{kind:?} checker");
        assert_eq!(codes(&r.grammar), vec![1313, 1108, 1052, 1052], "{kind:?}");
    }
    for kind in [SourceKind::Ts, SourceKind::Tsx] {
        let r = parse(src, kind);
        assert_eq!(
            codes(&r.diagnostics),
            vec![1313, 1108, 1052, 1052],
            "{kind:?}"
        );
        assert_eq!(r.grammar, r.diagnostics, "{kind:?}");
    }
    let r = parse("if (x);\nbreak;", SourceKind::Js);
    assert_eq!(codes(&r.diagnostics), vec![1105]);
    assert_eq!(codes(&r.grammar), vec![1313, 1105]);
    assert!(r.syntax_errors().is_empty());
    // With a syntax error there are no grammar errors at all.
    let r = parse("if (x);\nreturn;\nlet = ;", SourceKind::Js);
    assert_eq!(codes(&r.diagnostics), vec![1109]);
    assert!(r.grammar.is_empty());
}

/// TS1156 for a `let`, `const`, `using` or `await using` statement as an `if` or loop body
/// is reported next to syntax errors (tsc 7's checker reports it with `error`, not as a
/// grammar error that waits for a clean file); for `type` and `interface` it waits
/// (disallowedBlockScopedInPresenceOfParseErrors1).
#[test]
fn embedded_variable_statement_is_reported_next_to_syntax_errors() {
    let codes = |ds: &[Diagnostic]| ds.iter().map(|d| d.code).collect::<Vec<_>>();
    let src = "function f() {\n  if (1 > 0)\n    const e = 3;\n  while (x) l: let y = 1;\n  if (x) type T = 1;\n  }\n}\n";
    let r = parse(src, SourceKind::Ts);
    // (TS1344 for the label is the binder's: `ts_r_binder` reports it either way.)
    assert_eq!(codes(&r.diagnostics), vec![1156, 1156, 1128]);
    assert_eq!(codes(&r.grammar), vec![1156, 1156]);
    assert_eq!(codes(&r.syntax_errors()), vec![1128]);
    // In a clean file all three are reported.
    let r = parse(&src[..src.len() - 2], SourceKind::Ts);
    assert_eq!(codes(&r.diagnostics), vec![1156, 1344, 1156, 1156]);
    assert_eq!(r.grammar, r.diagnostics);
}

/// In a module `await` is an operator at the top level even where only a name fits, as
/// tsc reparses such statements (topLevelAwaitErrors.1): `@await` decorators (also on
/// parameters, which are read in the context outside the function) and `extends await<T>`
/// are TS1109. A declaration named `await` is left to the checker (TS1262,
/// topLevelAwaitErrors.12). In a script nothing changes.
#[test]
fn top_level_await_is_an_operator_in_a_module() {
    let codes_at = |r: &crate::ParseResult, src: &str| {
        let lines = ts_r_span::LineIndex::new(src);
        r.diagnostics
            .iter()
            .map(|d| {
                let at = lines.line_col(d.span.start);
                (d.code, at.line + 1, at.col + 1)
            })
            .collect::<Vec<_>>()
    };
    let body = "@await\nclass C1 {}\n@(await)\nclass C2 {}\nclass C3 extends await<string> {}\nclass C4 {\n  m(@await(1) [x]) {}\n}\nfunction f(@await [x]) {}\n";
    let src = format!("export {{}};\n{body}");
    let r = parse(&src, SourceKind::Ts);
    assert_eq!(
        codes_at(&r, &src),
        vec![
            (1109, 2, 2),
            (1109, 4, 8),
            (1109, 6, 18),
            (1109, 6, 23),
            (1005, 6, 30),
            (1109, 8, 6),
            (1109, 10, 13),
        ]
    );
    let src = "export {};\nimport await = foo.await;\ndeclare namespace foo { const await: any; }\ndeclare class C extends await {}\nlet t: await;\n";
    assert!(parse(src, SourceKind::Ts).diagnostics.is_empty());
    // A script: `await` is a name.
    let r = parse(body, SourceKind::Ts);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
}

/// After `?.` only a name makes a property access, so an instantiation expression followed
/// by an optional call (`f<T>?.()`, `f<T>?.<U>()`) or element access is not TS1477
/// (instantiationExpressionErrors).
#[test]
fn instantiation_before_optional_call_is_not_a_property_access() {
    for src in [
        "f<number>?.();",
        "f<number>?.<number>();",
        "f<number>?.[0];",
    ] {
        let r = parse(src, SourceKind::Ts);
        assert!(r.diagnostics.is_empty(), "{src}: {:?}", r.diagnostics);
    }
}

/// An import type takes no `,` after its attributes' `}` (tsc 7): the comma is `')'
/// expected` and is left for the statement list (importAttributes10).
#[test]
fn import_type_has_no_comma_after_attributes() {
    let at = |src: &str| {
        parse(src, SourceKind::Ts)
            .diagnostics
            .iter()
            .map(|d| (d.code, d.span.start))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        at("type A = typeof import(\"a\", { with: {} },);"),
        [(1005, 40), (1128, 41)]
    );
    assert_eq!(
        at("type A = typeof import(\"a\", { with: {} ,,\n});"),
        [(1005, 40), (1128, 42), (1128, 43)]
    );
    assert!(at("type A = typeof import(\"a\", { with: {}, });").is_empty());
}

/// `import<T>(x)` and `import<T>` keep their type arguments on the `ImportCall` (the
/// checker resolves them: importWithTypeArguments, TS2304 on `T`); `import<>` has none.
#[test]
fn import_call_keeps_type_arguments() {
    for (src, expect) in [
        ("import<T>(\"m\");", Some(1)),
        ("import<T>\nconst a = import<string, number>", Some(1)),
        ("const a = import<string, number>(\"m\");", Some(2)),
        ("import<>(\"m\");", None),
        ("import(\"m\");", None),
    ] {
        let r = parse(src, SourceKind::Ts);
        let ast = &r.ast;
        let call = (1..ast.nodes.len() as u32)
            .filter_map(ts_r_ast::NodeId::from_raw)
            .find(|&id| matches!(ast.node(id), ts_r_ast::Node::ImportCall { .. }))
            .expect("an import call");
        let ts_r_ast::Node::ImportCall { type_args, .. } = *ast.node(call) else {
            unreachable!()
        };
        let n = type_args.map(|ta| match *ast.node(ta) {
            ts_r_ast::Node::TypeArgs { args } => ast.list(args).len(),
            _ => panic!("{src}: type_args is {:?}", ast.kind(ta)),
        });
        assert_eq!(n, expect, "{src}");
        // Children before parents, in source order.
        if let Some(ta) = type_args {
            assert!(ta < call);
        }
    }
}

/// `ParseOptions::module` parses with the module goal without `import`/`export`: top-level
/// `await` before a line break is the operator (espree `sourceType: "module"`).
#[test]
fn module_option_forces_top_level_await() {
    let src = "Promise.all([\n\tawait\n\t\tpromise,\n]);";
    let script = parse(src, SourceKind::Js);
    assert!(!script.diagnostics.is_empty(), "the default stays tsc's");
    let opts = crate::ParseOptions {
        module: true,
        ..Default::default()
    };
    let r = crate::parse_with(src, SourceKind::Js, opts);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let awaits = (1..r.ast.nodes.len() as u32)
        .filter_map(ts_r_ast::NodeId::from_raw)
        .filter(|&id| r.ast.kind(id) == NodeKind::Await)
        .count();
    assert_eq!(awaits, 1);
}

/// TS1228 (a type predicate outside a return type) is tsc's checker's, reported while
/// checking the predicate: it stays next to syntax errors (a kept grammar error).
#[test]
fn misplaced_type_predicate_is_kept_next_to_syntax_errors() {
    let r = parse("var x: this is string;\nlet = ;", SourceKind::Ts);
    let codes: Vec<u32> = r.diagnostics.iter().map(|d| d.code).collect();
    assert!(codes.contains(&1228), "{codes:?}");
    assert!(codes.contains(&1109), "{codes:?}");
    assert!(r.grammar.iter().any(|d| d.code == 1228));
    assert!(r.syntax_errors().iter().all(|d| d.code != 1228));
}
