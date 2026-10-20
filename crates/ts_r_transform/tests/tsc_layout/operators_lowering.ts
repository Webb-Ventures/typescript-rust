// @target: es2015
// @module: esnext
declare const a: any, b: any;
function f(o: any) {
    const r = o?.x?.y ?? o?.z;
    return () => { const q = o?.w; return q ?? r; };
}
let t = a;
t ||= b;
t.x ??= 3;
t[a.b] &&= 4;
(t.y) ||= (b);
let e = 2 ** 3;
e **= 2;
a.b.c **= 2;
a[b()] **= a ** 2;
a?.b();
a.b?.();
a?.[0]?.(1);
delete a?.b;
const n1 = 1_000_000, n2 = 0xf_f ?? 0, n3 = 1_0n, n4 = 0.000_1;
try { f(1); } catch { }
const s = () => t ??= 1;
