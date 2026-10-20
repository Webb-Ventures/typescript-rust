//! Printer tests: round trips, precedence and ASI on synthesized trees, comments, layouts and
//! source maps.

use ts_r_ast::{
    Ast, BinaryOp, F64Bits, FnFlags, Node, NodeId, NodeList, SourceKind, Span, UnaryOp,
};
use ts_r_codegen::shape::shape;
use ts_r_codegen::{
    CodegenOptions, Layout, PrintHints, SourceMapOptions, Syntax, print, print_as, print_with,
};

fn parse(src: &str, kind: SourceKind) -> Ast {
    let r = ts_r_parser::parse(src, kind);
    // The checker's grammar errors (TS1248 `const` on a class member) leave a complete tree,
    // kept for the misplaced-modifier cases.
    assert!(
        !r.syntax_errors().iter().any(|d| d.is_error()),
        "syntax error in {src:?}: {:?}",
        r.diagnostics
    );
    r.ast
}

fn opts(layout: Layout) -> CodegenOptions {
    CodegenOptions {
        layout,
        ..CodegenOptions::default()
    }
}

/// parse → print → parse gives the same tree, in both layouts, and printing is idempotent.
fn round_trip(src: &str, kind: SourceKind) {
    let ast = parse(src, kind);
    for layout in [Layout::Readable, Layout::Compact] {
        let out = print(&ast, src, &opts(layout)).code;
        let again = ts_r_parser::parse(&out, kind);
        assert!(
            !again.syntax_errors().iter().any(|d| d.is_error()),
            "{layout:?} output does not parse:\n{out}\n{:?}",
            again.diagnostics
        );
        assert_eq!(
            shape(&ast),
            shape(&again.ast),
            "{layout:?} changed the tree:\n{src}\n---\n{out}"
        );
        let out2 = print(&again.ast, &out, &opts(layout)).code;
        assert_eq!(out, out2, "{layout:?} printing is not idempotent");
    }
}

#[test]
fn round_trips_tricky_syntax() {
    let cases = [
        "a = b ? c : d ? e : f;\nx = (a, b);\ny = (1, 2), 3;\n",
        "(function () {})();\n(class {});\n({}).x;\n(async () => {})();\n",
        "let [a] = b;\n(let)[0] = 1;\n",
        "x = a ** -b;\ny = (-a) ** b;\nz = (a ?? b) || c;\nw = a && (b || c);\n",
        "new (a.b())();\nnew (a())();\nnew a.b.c();\n(new a).b;\nnew (foo().bar)();\n",
        "x = a-- - --b + +c - -d + ++e;\ny = a - -1;\nz = - -a;\n",
        "label: for (const x of y) { if (x) continue label; else break; }\n",
        "for (let i = 0, j = (1 in o); i < 3; i++) {}\nfor (const k in (a in b)) {}\n",
        "async function* g() { yield* x; await (yield 1); for await (const v of g()) {} }\n",
        "class A extends (B, C) { static #p = 1; #m() { return #p in this; } static { this.x = 1; } accessor y = 2; }\n",
        "x = `a${b}c${`d${e}`}`;\ntag`x${y}`;\nr = /[/]+/gu.test(s);\n",
        "o = { a, b: 1, [c]: 2, ...d, get e() { return 1; }, set e(v) {}, async *f() {}, 'g-h': 3, 1e3: 4 };\n",
        "if (a) b(); else if (c) d(); else { e(); }\ndo x(); while (y);\n",
        "switch (x) { case 1: case 2: y(); break; default: z(); }\ntry { a(); } catch { b(); } finally { c(); }\n",
        "x = a?.b?.[c]?.(d);\n(a?.b).c;\n(a?.b)();\n",
        "export default (function () {});\nexport default class {}\n",
        "x = () => ({});\ny = () => () => 1;\nz = (a = 1, { b }, [c], ...d) => a;\n",
        "x = 0.5.toFixed();\ny = 1..toString();\nz = 0x10.toString();\n",
        "a = b => c => d;\nx = typeof (() => {});\nvoid (a, b);\n",
    ];
    for c in cases {
        round_trip(c, SourceKind::Ts);
    }
}

#[test]
fn round_trips_typescript_and_jsx() {
    round_trip(
        "abstract class A<T extends object = {}> implements I { private readonly x?: T; constructor(public y: number) { super(); } abstract m(): void; }\n\
         type F = <T>(a: T) => T extends string ? 'a' : `b${number}`;\n\
         let v = <const T,>(x: T) => x satisfies unknown as never;\n\
         enum E { A = 1, B = A << 1 }\n\
         declare module 'm' { export function f(): void; }\n\
         namespace N.M { export const x = 1; }\n",
        SourceKind::Ts,
    );
    round_trip(
        "const a = <div className=\"x\" {...p} key={k}>text {v} <b>&amp;</b><></></div>;\nconst f = <T,>(x: T) => <A<T> x={x} />;\n",
        SourceKind::Tsx,
    );
}

// ---- misplaced modifiers ----

/// Prints `src` (readable layout) as `syntax`.
fn printed(src: &str, syntax: Syntax) -> String {
    let ast = parse(src, SourceKind::Ts);
    print_as(
        &ast,
        src,
        &opts(Layout::Readable),
        &PrintHints::default(),
        syntax,
    )
    .code
}

#[test]
fn misplaced_modifiers_print_in_typescript_output() {
    let cases = [
        "class A {\n    static const H = 1;\n}\n",
        "let C = class {\n    const a = 4;\n};\n",
        "let o = { public a: 1, private b, abstract c: 2, async d: 3, static e, in f: 4, readonly [g]: 5, public m() {} };\n",
        "async class C {}\n",
        "export async class C {}\n",
        "async enum E {\n    A\n}\n",
        "export const enum E {\n    A\n}\n",
        "async interface I {}\n",
        "async namespace M {}\n",
        "interface D {\n    override bar(): void;\n    public x: number;\n}\n",
        "function f(async this: C, in a, out b, static c) {}\n",
        "type T<public T, const in out U> = T;\n",
        "class K {\n    async x = 1;\n    readonly async z;\n}\n",
        "static const z = 3;\nasync var w = 4;\npublic let y = 1;\n",
    ];
    for src in cases {
        round_trip(src, SourceKind::Ts);
        assert_eq!(printed(src, Syntax::TypeScript), src);
        // `print` is TypeScript output.
        let ast = parse(src, SourceKind::Ts);
        assert_eq!(print(&ast, src, &opts(Layout::Readable)).code, src);
    }
}

#[test]
fn javascript_output_drops_typescript_only_modifiers_like_tsc() {
    // The expected texts are tsc 7.0.2's JavaScript emit for these (erroneous) inputs, up to
    // formatting. Classes and parameters are printed as they are in the tree here (the
    // transform rewrites their modifiers).
    let cases = [
        (
            "let o = { public a: 1, private b, async c: 3, static e, in f: 4, public m() {}, async static s() {} };\n",
            "let o = { a: 1, b, c: 3, e, f: 4, m() {}, static async s() {} };\n",
        ),
        (
            "public var x = 1;\nprivate function f() {}\nabstract let y = 2;\nstatic const z = 3;\nasync var w = 4;\nexport public const e = 1;\n",
            "var x = 1;\nfunction f() {}\nlet y = 2;\nstatic const z = 3;\nasync var w = 4;\nexport const e = 1;\n",
        ),
        (
            "class K {\n    public static const a = 1;\n    async x = 1;\n    static async y;\n    readonly async z;\n}\n",
            "class K {\n    static a = 1;\n    async x = 1;\n    static async y;\n    async z;\n}\n",
        ),
        ("async class C {}\n", "async class C {}\n"),
        (
            "function f(async r, static s, accessor b, declare d, public e) {}\n",
            "function f(async r, static s, accessor b, d, e) {}\n",
        ),
    ];
    for (src, want) in cases {
        assert_eq!(printed(src, Syntax::JavaScript), want, "{src}");
        // `print_with` (the entry point after a transform) is JavaScript output.
        let ast = parse(src, SourceKind::Ts);
        let hints = PrintHints::default();
        let out = print_with(&ast, src, &opts(Layout::Readable), &hints);
        assert_eq!(out.code, want);
    }
}

// ---- valid inputs that need no parentheses ----

/// `src` prints as `want` and round-trips.
fn prints_as(src: &str, want: &str) {
    round_trip(src, SourceKind::Ts);
    assert_eq!(printed(src, Syntax::TypeScript), want);
}

#[test]
fn for_of_update_target_is_not_parenthesized() {
    prints_as("for (v++ of []) {}\n", "for (v++ of []) {}\n");
}

#[test]
fn for_in_assignment_target_is_not_parenthesized() {
    prints_as("for (a = 1 in b) {}\n", "for (a = 1 in b) {}\n");
    // `in` inside the target still needs them.
    prints_as("for ((a in b) in c) {}\n", "for ((a in b) in c) {}\n");
}

#[test]
fn for_head_in_rule_stops_at_function_and_class_bodies() {
    for src in [
        "for (function () { return a in b; }.x of xs) {}\n",
        "for (function (p = a in b) {}.x in xs) {}\n",
        "for (var q = function () { return a in b; }; ;) {}\n",
        "for (class { m() { return a in b; } }.x of xs) {}\n",
        "for (class { x = a in b; }.y of xs) {}\n",
    ] {
        round_trip(src, SourceKind::Ts);
        assert!(
            !printed(src, Syntax::TypeScript).contains("(a in b)"),
            "{src}"
        );
    }
}

#[test]
fn for_await_of_async_is_not_parenthesized() {
    prints_as(
        "async function f() { for await (async of x) {} }\n",
        "async function f() {\n    for await (async of x) {}\n}\n",
    );
    // Without `await`, `for (async of x)` starts like an arrow function: keep them.
    let mut b = B::new();
    let left = b.id("async");
    let right = b.id("x");
    let body = b.add(Node::Block {
        stmts: NodeList::EMPTY,
    });
    let stmt = b.add(Node::ForOf {
        is_await: false,
        left,
        right,
        body,
    });
    assert_eq!(b.print(&[stmt]), "for ((async) of x) {}\n");
}

#[test]
fn non_null_decorator_is_not_parenthesized() {
    prints_as(
        "@x! class C {}\n@x.y! class D {}\n@x!.y class E {}\nclass F { @x! m() {} }\n",
        "@x!\nclass C {}\n@x.y!\nclass D {}\n@x!.y\nclass E {}\nclass F {\n    @x!\n    m() {}\n}\n",
    );
    // A member of a call result still needs them.
    prints_as("@(x()!.y) class C {}\n", "@(x()!.y)\nclass C {}\n");
}

// ---- synthesized trees: precedence and statement-start ambiguities ----

struct B {
    ast: Ast,
}

impl B {
    fn new() -> B {
        B {
            ast: Ast::new(SourceKind::Js, 0),
        }
    }
    fn add(&mut self, n: Node) -> NodeId {
        self.ast.add(n, Span::EMPTY)
    }
    fn id(&mut self, name: &str) -> NodeId {
        let name = self.ast.intern(name);
        self.add(Node::Ident { name })
    }
    fn num(&mut self, v: f64) -> NodeId {
        self.add(Node::NumberLit {
            value: F64Bits::new(v),
        })
    }
    fn bin(&mut self, left: NodeId, op: BinaryOp, right: NodeId) -> NodeId {
        self.add(Node::Binary { left, op, right })
    }
    fn list(&mut self, items: &[NodeId]) -> NodeList {
        self.ast.add_list(items)
    }
    fn print(mut self, stmts: &[NodeId]) -> String {
        let stmts = self.list(stmts);
        let root = self.add(Node::Program { stmts });
        self.ast.root = root;
        print(&self.ast, "", &opts(Layout::Readable)).code
    }
    fn expr_stmt(mut self, expr: impl FnOnce(&mut B) -> NodeId) -> String {
        let e = expr(&mut self);
        let s = self.add(Node::ExprStmt { expr: e });
        self.print(&[s])
    }
}

#[test]
fn synthesized_precedence_gets_parentheses() {
    let out = B::new().expr_stmt(|b| {
        let (a, c, d) = (b.id("a"), b.id("c"), b.id("d"));
        let sum = b.bin(a, BinaryOp::Add, c);
        b.bin(sum, BinaryOp::Mul, d)
    });
    assert_eq!(out, "(a + c) * d;\n");
    let out = B::new().expr_stmt(|b| {
        let (a, c, d) = (b.id("a"), b.id("c"), b.id("d"));
        let sub = b.bin(c, BinaryOp::Sub, d);
        b.bin(a, BinaryOp::Sub, sub)
    });
    assert_eq!(out, "a - (c - d);\n");
    // `**` is right-associative; a unary operand on its left needs parentheses.
    let out = B::new().expr_stmt(|b| {
        let a = b.id("a");
        let neg = b.add(Node::Unary {
            op: UnaryOp::Minus,
            arg: a,
        });
        let two = b.num(2.0);
        b.bin(neg, BinaryOp::Exp, two)
    });
    assert_eq!(out, "(-a) ** 2;\n");
    // `??` can't mix with `||` without parentheses.
    let out = B::new().expr_stmt(|b| {
        let (a, c, d) = (b.id("a"), b.id("c"), b.id("d"));
        let or = b.bin(a, BinaryOp::LogicalOr, c);
        b.bin(or, BinaryOp::Coalesce, d)
    });
    assert_eq!(out, "(a || c) ?? d;\n");
    // Calling the result of a call-less `new` / an arrow.
    let out = B::new().expr_stmt(|b| {
        let f = b.id("f");
        let body = b.num(1.0);
        let params = NodeList::EMPTY;
        let arrow = b.add(Node::Arrow {
            flags: FnFlags::EXPRESSION_BODY,
            type_params: None,
            params,
            return_type: None,
            body,
        });
        let args = b.list(&[f]);
        b.add(Node::Call {
            callee: arrow,
            optional: false,
            type_args: None,
            args,
        })
    });
    assert_eq!(out, "(() => 1)(f);\n");
    // Negative numbers as member objects and in subtraction.
    let out = B::new().expr_stmt(|b| {
        let a = b.id("a");
        let neg = b.num(-1.0);
        b.bin(a, BinaryOp::Sub, neg)
    });
    assert_eq!(out, "a - -1;\n");
}

#[test]
fn synthesized_statement_starts_are_unambiguous() {
    // An object literal or function expression at statement start must be wrapped.
    let out = B::new().expr_stmt(|b| {
        let props = NodeList::EMPTY;
        let obj = b.add(Node::ObjectLit { props });
        let x = b.id("x");
        b.add(Node::Member {
            obj,
            optional: false,
            prop: x,
        })
    });
    assert_eq!(out, "({}.x);\n");
    let out = B::new().expr_stmt(|b| {
        let props = NodeList::EMPTY;
        let obj = b.add(Node::ObjectLit { props });
        let one = b.num(1.0);
        let exprs = b.list(&[obj, one]);
        b.add(Node::Seq { exprs })
    });
    let reparsed = ts_r_parser::parse(&out, SourceKind::Js);
    assert!(reparsed.diagnostics.is_empty(), "{out}");
    assert!(out.starts_with('('), "{out}");
}

#[test]
fn synthesized_return_argument_keeps_its_line() {
    // `return` followed by a comment-free synthesized expression must not break the line.
    let mut b = B::new();
    let a = b.id("a");
    let c = b.id("c");
    let e = b.bin(a, BinaryOp::LogicalAnd, c);
    let r = b.add(Node::Return { arg: Some(e) });
    let stmts = b.list(&[r]);
    let body = b.add(Node::Block { stmts });
    let params = NodeList::EMPTY;
    let arrow = b.add(Node::Arrow {
        flags: FnFlags::NONE,
        type_params: None,
        params,
        return_type: None,
        body,
    });
    let s = b.add(Node::ExprStmt { expr: arrow });
    let out = b.print(&[s]);
    assert!(out.contains("return a && c;"), "{out}");
}

// ---- comments and layouts ----

#[test]
fn readable_layout_keeps_comments() {
    let src = "/** doc */\nfunction f(/* p */ a) {\n    // inside\n    return a; // trailing\n}\n/*! legal */\nconst x = /*#__PURE__*/ g();\n";
    let ast = parse(src, SourceKind::Ts);
    let out = print(&ast, src, &opts(Layout::Readable)).code;
    for c in ["/** doc */", "// inside", "/*! legal */", "/*#__PURE__*/"] {
        assert!(out.contains(c), "missing {c}:\n{out}");
    }
    let compact = print(&ast, src, &opts(Layout::Compact)).code;
    assert!(compact.contains("/*! legal */"), "{compact}");
    assert!(compact.contains("/*#__PURE__*/"), "{compact}");
    assert!(
        !compact.contains("// inside") && !compact.contains("/** doc */"),
        "{compact}"
    );
    let none = print(
        &ast,
        src,
        &CodegenOptions {
            comments: false,
            ..CodegenOptions::default()
        },
    )
    .code;
    assert!(!none.contains("doc") && !none.contains("inside"), "{none}");
    assert!(none.contains("/*! legal */"), "{none}");
}

/// Comments attached by transforms (`Ast::synthetic_comments`) print next to their node, and
/// are dropped without `comments` and in the compact layout (tsc's `removeComments`).
#[test]
fn synthetic_comments_print_around_their_node() {
    let src = "f(a, b);\n";
    let mut ast = parse(src, SourceKind::Ts);
    let ident = |ast: &Ast, name: &str| {
        (1..=ast.node_count() as u32)
            .filter_map(NodeId::from_raw)
            .find(|&id| ast.name_of(id) == Some(name))
            .unwrap()
    };
    let a = ident(&ast, "a");
    let b = ident(&ast, "b");
    ast.add_synthetic_comment(a, "/* after a */", true);
    ast.add_synthetic_comment(b, "/* before b */", false);
    ast.add_synthetic_comment(b, "/* after b */", true);
    let out = print(&ast, src, &opts(Layout::Readable)).code;
    assert_eq!(out, "f(a /* after a */, /* before b */ b /* after b */);\n");
    let compact = print(&ast, src, &opts(Layout::Compact)).code;
    assert!(!compact.contains("/*"), "{compact}");
    let none = print(
        &ast,
        src,
        &CodegenOptions {
            comments: false,
            ..CodegenOptions::default()
        },
    )
    .code;
    assert_eq!(none, "f(a, b);\n");
}

#[test]
fn compact_layout_has_no_optional_whitespace() {
    let src =
        "function f(a, b) {\n    if (a) {\n        return a + b;\n    }\n    return typeof b;\n}\n";
    let ast = parse(src, SourceKind::Js);
    let out = print(&ast, src, &opts(Layout::Compact)).code;
    assert_eq!(
        out.trim_end(),
        "function f(a,b){if(a){return a+b}return typeof b}"
    );
}

#[test]
fn compact_layout_drops_keyword_spaces_and_last_semicolons() {
    let cases = [
        (
            "function f() { return (a, b); }",
            "function f(){return(a,b)}",
        ),
        ("function f() { return !0; }", "function f(){return!0}"),
        (
            "function f() { return \"x\" + y; }",
            "function f(){return\"x\"+y}",
        ),
        ("function f() { return [1]; }", "function f(){return[1]}"),
        ("function f() { return x; }", "function f(){return x}"),
        (
            "function f() { return /re/.test(x); }",
            "function f(){return/re/.test(x)}",
        ),
        ("let { a } = b; let [c] = d;", "let{a}=b;let[c]=d;"),
        ("for (const [k, v] of m) f(k);", "for(const[k,v]of m)f(k);"),
        (
            "class A { #x; m() { return #x in this; } }",
            "class A{#x;m(){return #x in this}}",
        ),
        (
            "const f = (x) => x; const g = async (y) => y;",
            "const f=x=>x;const g=async y=>y;",
        ),
        (
            "const h = (x = 1) => x, i = ([x]) => x, j = (...x) => x;",
            "const h=(x=1)=>x,i=([x])=>x,j=(...x)=>x;",
        ),
        ("if (a) b(); else c();", "if(a)b();else c();"),
        ("do x(); while (y); z();", "do x();while(y);z();"),
        ("a: ; if (b) ; else ;", "a:;if(b);else;"),
        ("{ a(); ; }", "{a();;}"),
        ("throw new Error(); ", "throw new Error();"),
        (
            "typeof (a); void (0); delete (a.b);",
            "typeof(a);void(0);delete(a.b);",
        ),
        ("x = a - -b + +c - --d;", "x=a- -b+ +c- --d;"),
    ];
    for (src, want) in cases {
        let ast = parse(src, SourceKind::Js);
        let out = print(&ast, src, &opts(Layout::Compact)).code;
        assert_eq!(out.trim_end(), want, "{src}");
        // The compact output parses back to the same tree.
        let back = parse(&out, SourceKind::Js);
        assert_eq!(
            ts_r_codegen::shape::shape(&back),
            ts_r_codegen::shape::shape(&ast),
            "{src}"
        );
    }
}

#[test]
fn literals_keep_their_source_form() {
    let src = "x = 0xFF + 1_000 + 1e3 + .5;\ny = 'it\\'s' + \"q\";\nz = /a\\/b/g;\n";
    let ast = parse(src, SourceKind::Js);
    let out = print(&ast, src, &opts(Layout::Readable)).code;
    assert_eq!(out, src);
}

// ---- source maps ----

#[test]
fn source_map_maps_tokens_back() {
    let src = "const a: number = 1;\n\nfunction f(x: string): string {\n    return x;\n}\n";
    let ast = parse(src, SourceKind::Ts);
    // Print the TypeScript as-is (no transform); the map must still point at the source.
    let out = print(
        &ast,
        src,
        &CodegenOptions {
            source_map: Some(SourceMapOptions {
                file: Some("a.js".into()),
                source: "a.ts".into(),
                source_root: None,
                include_content: true,
            }),
            ..CodegenOptions::default()
        },
    );
    let map = out.map.expect("map");
    assert_eq!(map.sources, ["a.ts"]);
    let json = map.to_json();
    assert!(
        json.starts_with("{\"version\":3,\"file\":\"a.js\""),
        "{json}"
    );
    assert!(json.contains("\"sourcesContent\":["), "{json}");
    let segs = map.decode();
    // `return` is on source line 3 (0-based), column 4.
    let (ret_line, ret_col) = out
        .code
        .lines()
        .enumerate()
        .find_map(|(i, l)| l.find("return").map(|c| (i as u32, c as u32)))
        .expect("return line");
    let seg = segs
        .iter()
        .find(|m| m.gen_line == ret_line && m.gen_col == ret_col)
        .expect("mapping for return");
    let (_, line, col, _) = seg.source.expect("source position");
    assert_eq!((line, col), (3, 4));
    // Every mapping points inside the source.
    let lines = src.lines().count() as u32;
    assert!(
        segs.iter()
            .all(|m| m.source.is_none_or(|(_, l, _, _)| l < lines))
    );
}

#[test]
fn data_url_round_trips() {
    let src = "let x = 1;\n";
    let ast = parse(src, SourceKind::Js);
    let out = print(
        &ast,
        src,
        &CodegenOptions {
            source_map: Some(SourceMapOptions {
                source: "x.js".into(),
                ..SourceMapOptions::default()
            }),
            ..CodegenOptions::default()
        },
    );
    let url = out.map.expect("map").to_data_url();
    assert!(
        url.starts_with("data:application/json;base64,eyJ2ZXJzaW9uIjoz"),
        "{url}"
    );
}

// ---- corpus: the repo's parser and transform fixtures ----

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["ts", "tsx", "mts", "cts", "js", "jsx"].contains(&e))
        {
            out.push(p);
        }
    }
}

#[test]
fn round_trips_repo_fixtures() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    walk(&root.join("ts_r_parser/tests/fixtures"), &mut files);
    walk(&root.join("ts_r_transform/tests/fixtures"), &mut files);
    let mut checked = 0;
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        let kind = SourceKind::from_path(&f.to_string_lossy());
        let parsed = ts_r_parser::parse(&src, kind);
        if parsed.diagnostics.iter().any(|d| d.is_error()) {
            continue;
        }
        round_trip(&src, kind);
        checked += 1;
    }
    assert!(checked >= 40, "only {checked} fixtures checked");
}
