//! The minifier must not change behaviour: every case is minified with everything on, the
//! output must re-parse cleanly, and (when Node is installed) the original and the minified
//! program must print the same thing under `scripts/limit.sh`. Function and class `.name`
//! values may change (identifier minification renames them, as esbuild does without
//! `--keep-names`), so no case prints them.

ts_r_testing::cap_memory!();

use std::path::{Path, PathBuf};
use std::process::Command;

use ts_r_ast::SourceKind;
use ts_r_minify::{MinifyOptions, minify};

const CASES: &[(&str, &str)] = &[
    (
        // `const` becomes `let` only when nothing assigns to it; arrow bodies that only
        // return become expressions (object literals and sequences stay parenthesized).
        "const-and-arrow-bodies",
        "const fixed = 1;\nconst assigned = 2;\ntry { assigned = 3; } catch (e) { console.log(e.constructor.name); }\nconst obj = () => { return { k: fixed }; };\nconst seq = (a) => { console.log(a); return a + 1; };\nconst fns = [];\nfor (const i of [1, 2]) { const j = i * 10; fns.push(() => { return j; }); }\nconsole.log(obj().k, seq(4), fns.map((f) => f()), typeof obj === 'function', typeof fixed !== 'number', assigned);\n",
    ),
    (
        // Ecosystem (@radix-ui/*, prettier, tailwindcss): an unused `var` in a block must not
        // take the name of a `let` of that block.
        "block-var-beside-let",
        "function f(trapped, log) { return () => { if (trapped) { let in2 = function () {}, out2 = function () {}; var handleIn = in2, handleOut = out2; log(typeof in2, typeof out2); } }; }\nf(true, console.log)();\n",
    ),
    (
        "shadowing",
        "var longName = 1;\nfunction f(longName) { { let longName = 3; console.log(longName); } return longName; }\nconsole.log(f(2), longName);\n",
    ),
    (
        "closures",
        "function counter() { let count = 0; return { inc() { return ++count; }, get value() { return count; } }; }\nconst c = counter(); c.inc(); c.inc(); console.log(c.value);\n",
    ),
    (
        "hoisting",
        "console.log(typeof hoisted, typeof later);\nfunction hoisted() { return 1; }\nvar later = 2;\nconsole.log(hoisted(), later);\n",
    ),
    (
        "arguments",
        "function f(a, b) { arguments[0] = 9; return [a, b, arguments.length]; }\nconsole.log(f(1, 2, 3));\n",
    ),
    (
        "direct-eval",
        "function f() { var secret = 42; return eval('secret'); }\nconsole.log(f());\n",
    ),
    (
        "with-statement",
        "var obj = { value: 5 };\nfunction f() { var value = 1; with (obj) { return value; } }\nconsole.log(f());\n",
    ),
    (
        "labels",
        "outer: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { if (j === 1) continue outer; if (i === 2) break outer; console.log(i, j); } }\n",
    ),
    (
        "class-names",
        "class Animal { constructor(name) { this.name = name; } speak() { return this.name + ' (' + (this instanceof Dog) + ')'; } }\nconst Dog = class Puppy extends Animal { static kind() { return Puppy === Dog; } };\nconsole.log(new Dog('rex').speak(), Dog.kind());\n",
    ),
    (
        "function-name",
        "const fn = function named() { return typeof named; };\nconsole.log(fn(), fn.length, (() => {}).name);\n",
    ),
    (
        "tdz",
        "try { console.log(x); } catch (e) { console.log(e.constructor.name); }\nlet x = 1;\nconsole.log(x);\n",
    ),
    (
        "dead-branches",
        "if (false) { console.log('no'); } else { console.log('yes'); }\nconst v = true ? 'a' : 'b';\nconsole.log(v, 0 && 'x', 1 || 'y', null ?? 'z');\nwhile (false) console.log('never');\n",
    ),
    (
        "if-to-expression",
        "function f(n) { if (n > 2) { return 'big'; } else if (n > 1) { return 'mid'; } return 'small'; }\nfor (const n of [0, 2, 3]) { if (n) console.log(f(n)); else console.log('zero'); }\n",
    ),
    (
        "undefined-and-booleans",
        "let u = undefined;\nconsole.log(u === undefined, true, false, !true, typeof undefined);\nfunction g(undefined) { return undefined; }\nconsole.log(g(3));\n",
    ),
    (
        "asi-and-regex",
        "let a = 4\nlet b = a / 2 / 1\nconst re = /ab+c/g\nconsole.log(b, 'abbc abc'.match(re).length)\nlet c = a\n;[1, 2].forEach(n => console.log(n + c))\n",
    ),
    (
        "in-operator-for-init",
        "const o = { k: 1 };\nfor (var i = ('k' in o) ? 1 : 0; i < 2; i++) console.log(i);\nfor (const key in o) console.log(key);\n",
    ),
    (
        "numbers-and-strings",
        "console.log(-1 - -1, 1 / 0, -(0), 0.1 + 0.2, 1e21, 0x10, 'a' + 1 + 2, `t${1 + 1}t`, \"q'uote\", 'd\"q');\n",
    ),
    (
        "optional-chaining",
        "const o = { a: { b() { return this === o.a; } } };\nconsole.log(o?.a?.b(), o.x?.y, o.x?.y.z, (o.a).b());\n",
    ),
    (
        "comma-and-sequence",
        "let i = 0;\nconst r = (i++, i++, i);\nconsole.log(r, [1, 2, 3].map((x, j) => (x * j, x + j)));\n",
    ),
    (
        "getters-setters",
        "const o = { _v: 1, get v() { return this._v; }, set v(n) { this._v = n * 2; } };\no.v = 3;\nconsole.log(o.v, Object.keys(o));\n",
    ),
    (
        "destructuring-defaults",
        "function f({ alpha = 1, beta: { gamma = 2 } = {} } = {}, ...rest) { return [alpha, gamma, rest.length]; }\nconst [first, , third = 'd'] = ['a', 'b'];\nconsole.log(f(), f({ alpha: 5, beta: { gamma: 6 } }, 7, 8), first, third);\n",
    ),
    (
        "generators-async",
        "function* gen() { const got = yield 1; yield got * 2; }\nconst it = gen(); it.next();\nconsole.log(it.next(21).value);\nasync function af(value) { const awaited = await value; return awaited + 1; }\naf(1).then(v => console.log('async', v));\n",
    ),
    (
        "catch-scope",
        "var err = 'outer';\ntry { throw new Error('inner'); } catch (err) { var err = 'reassigned'; console.log(err); }\nconsole.log(err);\n",
    ),
    (
        // The `var` initializer assigns the catch parameter, which nothing else references:
        // the binding must stay.
        "catch-binding-assigned-by-var",
        "var err = 'outer';\ntry { throw 1; } catch (err) { var err = 'reassigned'; }\nconsole.log(err);\nfunction f() { try { throw 1; } catch (e) { var e = 2; } return e; }\nconsole.log(f());\n",
    ),
    (
        // An unused binding whose initializer reads a binding before its declaration runs
        // throws; dropping it would not. Reads after the declaration may go.
        "unused-binding-reads-tdz",
        "function f() { let unused = later; let later = 1; return later; }\nfunction g() { let early = 1; let unused = early; return early; }\nfunction h(k) { switch (k) { case 0: let x = 1; case 1: let unused = x; return 'reached'; } }\nfunction i() { let unused = cls; class cls {} return typeof cls; }\nfor (const fn of [f, g, () => h(1), i]) { try { console.log(fn()); } catch (e) { console.log(e.constructor.name); } }\n",
    ),
    (
        "switch",
        "function f(k) { switch (k) { case 1: return 'one'; case 2: case 3: { let local = 'few'; return local; } default: return 'many'; } }\nconsole.log(f(1), f(3), f(9));\n",
    ),
    (
        "global-references",
        "globalThis.shared = 7;\nfunction readGlobal() { return shared; }\nvar topLevel = 3;\nconsole.log(readGlobal(), globalThis.topLevel);\n",
    ),
    (
        "property-names",
        "const o = { 'valid': 1, 'not-valid': 2, 123: 3, ['comp' + 'uted']: 4 };\nconsole.log(o['valid'], o['not-valid'], o[123], o.computed, Object.keys(o).join());\n",
    ),
    (
        "unreachable-code",
        "function f() { return 1; console.log('dead'); function hoistedInside() {} }\nfunction g() { return typeof inner; function inner() {} }\nconsole.log(f(), g());\n",
    ),
    (
        "nested-functions-many-names",
        "function outer(alpha, beta, gamma) { function inner(delta) { const epsilon = alpha + delta; return function deepest(zeta) { return epsilon + zeta + beta + gamma; }; } return inner(1)(2); }\nconsole.log(outer(10, 20, 30));\n",
    ),
    (
        // `if (a) return x; return y` chains, nested ifs, implicit returns and continues.
        "if-return-chains",
        "function pick(a, b) { if (a) return 'a'; if (b) { console.log('b'); return 'b'; } return; }\nfunction nested(a, b) { if (a) { if (b) return 1; } return 0; }\nfunction early(x) { if (!x) return; console.log('x', x); console.log('again'); }\nfunction late(x) { console.log('start'); if (x > 1) return; for (var i = 0; i < x; i++) console.log(i); }\nfunction thrower(x) { if (x) throw new Error('t'); else throw new Error('f'); }\nfunction loop(xs) { for (const x of xs) { if (x % 2) continue; console.log(x); } let i = 0; while (i < 4) { i++; if (i === 2) continue; console.log('w', i); } }\nconsole.log(pick(1, 0), pick(0, 1), pick(0, 0), nested(1, 1), nested(1, 0), nested(0, 1), early(0), early(2), late(0), late(2), late(3));\ntry { thrower(1); } catch (e) { console.log(e.message); }\ntry { thrower(0); } catch (e) { console.log(e.message); }\nloop([1, 2, 3, 4]);\nconst arrow = (x) => { if (x) return; console.log('arrow', x); };\narrow(0); arrow(1);\nfunction withLet(x) { if (x) return 'early'; let y = x + 1; return () => y; }\nconsole.log(withLet(1), withLet(0)());\nfunction seqTest(x) { console.log('s'); if (x) return 1; return 2; }\nconsole.log(seqTest(0), seqTest(1));\n",
    ),
    (
        // `== null`, `??`, `?.` and `typeof x > "u"` rewrites.
        "nullish-rewrites",
        "function n(x) { return x === null || x === undefined ? 'nullish' : 'set'; }\nfunction m(x) { return x !== undefined && x !== null ? x : 'default'; }\nfunction c(o) { if (o != null) o.f(); if (o == null) return 'none'; o.g?.(); if (null != o) o.h(1); return 'done'; }\nfunction u(x) { return [typeof x === 'undefined', typeof x !== 'undefined', 'undefined' == typeof x, 'undefined' !== typeof x]; }\nconst obj = { f() { console.log('f'); }, h(v) { console.log('h', v); } };\nconsole.log(n(null), n(undefined), n(0), n(''), m(null), m(undefined), m(0), m(false), c(obj), c(null), c(undefined), u(1), u(undefined), u(), typeof undeclaredThing === 'undefined');\nlet calls = 0; const get = () => (calls++, null);\nfunction pick(v) { return v != null ? v : (calls++, 'fallback'); }\nconsole.log(pick(get()), pick(0), calls);\nlet f = null; f != null && f(); f = (v) => console.log('called', v); f != null && f(5);\nconst arr = [1]; arr != null && arr[0]; let s = 'x'; s != null && s.length && console.log('len', s.length);\nfunction keepsAll(x) { return x === null || x === undefined || x === 0; }\nconsole.log(keepsAll(0), keepsAll(null), keepsAll(1));\n",
    ),
    (
        // Optional catch bindings, `var` merged into `for` heads, `a && (b && c)`.
        "statements-and-chains",
        "function t() { try { JSON.parse('{'); } catch (unused) { return 'caught'; } }\nfunction u() { try { JSON.parse('{'); } catch (e) { return e.name; } }\nconsole.log(t(), u());\nfunction sum(xs) { var total = 0; for (var i = 0; i < xs.length; i++) total += xs[i]; return total; }\nfunction sum2(xs) { let n = 0; console.log('pre'); for (let i = 0; i < xs.length; i++) n += xs[i]; return n; }\nfunction g(a, b, c) { if (a) { if (b && c) console.log('all'); } return a && (b && c); }\nconsole.log(sum([1, 2, 3]), sum2([4, 5]), g(1, 1, 1), g(1, 0, 1), g(0));\nlet k = 0; k++; for (; k < 3; k++) console.log('k', k);\nvar inKey = 'a' in { a: 1 }; for (var j = 0; j < 2; j++) console.log(inKey, j);\n",
    ),
    (
        // Private names are renamed per class; nested classes and heritage resolve to the
        // right declaration; `#x in o` brand checks keep working.
        "private-names",
        "class Counter { #count = 0; static #instances = 0; constructor() { Counter.#instances++; } get #double() { return this.#count * 2; } inc() { this.#count++; return this; } read() { return [this.#count, this.#double]; } static total() { return Counter.#instances; } static is(o) { return #count in o; }\n  nested() { const self = this; return new (class Inner { #count = 100; get() { return [this.#count, self.#count]; } })().get(); }\n  heritage() { const v = this.#count; return new (class extends (this.#count, Object) { #count = v + 1; read() { return this.#count; } })().read(); } }\nconst c = new Counter().inc().inc(); new Counter();\nconsole.log(c.read(), Counter.total(), Counter.is(c), Counter.is({}), c.nested(), c.heritage());\nclass A { #secret = 'a'; static peek(o) { return o.#secret; } } class B { #secret = 'b'; static peek(o) { return o.#secret; } }\nconsole.log(A.peek(new A()), B.peek(new B()));\ntry { A.peek(new B()); } catch (e) { console.log(e.constructor.name); }\n",
    ),
    (
        // Unused function-local `let`/`const` with pure initializers go; others stay.
        "unused-locals",
        "function f() { let unused = Symbol('u'), used = 2; const effect = console.log('effect'); let later; const fn = () => used; return fn() + (typeof later); }\nconsole.log(f());\nfunction g() { let tdz = 1; { const shadow = (() => { try { return tdz; } catch (e) { return 'tdz'; } })(); return shadow; } }\nconsole.log(g());\n",
    ),
    (
        // Parentheses the printer must keep or add back.
        "parentheses",
        "const o = { a: { b() { return this === o.a; } }, m() { return this; } };\nconsole.log((o.a.b)(), (0, o.m)() === o, (o?.a).b(), (o.missing)?.x, typeof (o));\nfunction F() { return function G() { this.v = 1; }; }\nconsole.log(new (F())().v, new (F()) instanceof Function);\nconsole.log((-2) ** 2, (2 ** 3) ** 2, 2 ** (3 ** 2), (1, 2), [(1, 2)], ((a) => ({ a }))(3).a);\nconst x = null; console.log((x || null) ?? 'n', x ?? (0 || 'z'), (x && 1) ?? 2);\n(function () { console.log('iife'); })();\n(() => console.log('arrow iife'))();\n({}).constructor === Object && console.log('obj');\n('not a directive');\nlet n = 1; (n) = 2; console.log(n, (n += 1), `${(n, 'tpl')}`, (async function () {}) instanceof Function);\nconst tag = (s) => s[0]; console.log((tag)`t`, (o.a.b)``);\nconsole.log(!(1 in {}), !('a' instanceof Object), -(-1), +(+'2'), void (0), delete (o.zz));\nfor (const k in ({ a: 1 })) console.log(k);\nfunction* gen() { const v = (yield 1); console.log('got', v); }\nconst it = gen(); it.next(); it.next('val');\n",
    ),
];

fn have_node() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn run_node(path: &Path) -> String {
    let out = ts_r_testing::limited("node", 512, 30)
        .arg(path)
        .output()
        .expect("run node");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        if out.status.success() {
            String::new()
        } else {
            format!("[exit {:?}]", out.status.code())
        }
    )
}

#[test]
fn minified_programs_behave_the_same() {
    let node = have_node();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("minify-semantics");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let mut failures = Vec::new();
    for (name, src) in CASES {
        let out = minify("case.js", src, &MinifyOptions::all(false), None);
        assert!(
            out.errors.is_empty(),
            "{name}: input errors {:?}",
            out.errors
        );
        let reparsed = ts_r_parser::parse(&out.code, SourceKind::Js);
        if reparsed.diagnostics.iter().any(|d| d.is_error()) {
            failures.push(format!("{name}: output does not parse\n{}", out.code));
            continue;
        }
        if out.code.len() >= src.len() {
            failures.push(format!("{name}: output is not smaller\n{}", out.code));
        }
        if !node {
            continue;
        }
        let original = dir.join(format!("{name}.orig.js"));
        let minified = dir.join(format!("{name}.min.js"));
        std::fs::write(&original, src).expect("write");
        std::fs::write(&minified, &out.code).expect("write");
        let (want, got) = (run_node(&original), run_node(&minified));
        if want != got {
            failures.push(format!(
                "{name}: behaviour changed\n--- original\n{want}--- minified\n{got}--- code\n{}",
                out.code
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
