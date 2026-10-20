// @target: es2015
// @module: esnext
declare const a: any, b: any;
function g() {
    const x = b ?? 1;
    const y = a?.b;
    const z = b.c ?? 2;
    const w = a?.c?.d;
    function inner() {
        return a?.q ?? b?.r;
    }
    return () => a?.k?.j;
}
function h(_a: any) {
    return _a?.b?.c;
}
try { } catch { }
try { } catch { const _a = 1; }
declare let a: any, b: any, c: any, k: any, x: any;
a.b?.c.d?.e;
x.y.b?.();
(a?.b)();
(a?.b).c;
(a?.b.c)();
this?.x;
a?.b?.[c]?.(k);
f(x.y?.z, a ?? b ?? c);
a?.b ?? c;
(a ?? b) || c;
1 ?? a;
"s" ?? a;
this ?? a;
null ?? a;
a.b ?? c;
a[0] ?? c;
a() ?? c;
a.b.c **= 2;
a[k] **= 2;
a[0] **= 2;
a[k.j] **= 2;
x **= 2;
a.b ||= c;
a[k] &&= c;
a[k.j] ??= c;
a.b.c ||= 1;
x ??= 1;
let n1 = 1_000_000, n2 = 0xFF_FF, n3 = 0b1010_0001, n4 = 1_0.5e1_0, n5 = 0o7_7;
let p = (-2) ** 2;
let q = 2 ** -2;
let r = a ** b ** c;
super_?.x;
function f(...args: any[]) { }
function g() { return arguments?.length; }
class D { m() { return super.toString?.(); } }
/x/ ?? a;
`foo` ?? a;
true ?? a;
1n ?? a;
undefined ?? a;
(void 0) ?? a;
-1 ?? a;
