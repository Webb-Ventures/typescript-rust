if (a) b(); else c();
if (a) { b(); } else { c(); }
while (x) { y(); }
do { y(); } while (x);
for (const k in o) { }
for (const v of o) { }
switch (x) {
    case 1: a(); break;
    default: b();
}
try { a(); } catch (e) { b(); } finally { c(); }
try { a(); } catch { b(); }
label: for (;;) { continue label; }
throw new Error("x");
var u = typeof x, w = void 0, q = !x;
let arrow = (a, b) => a + b;
let arrow2 = async x => { await x; };
let t = `a${x}b${y}c`;
let cond = a ? b : c;
let idx = o[k];
let o2 = { m() { return 1; }, get g() { return 2; }, [k]: 3, ...s };
let [d1, , d2 = 1, ...rest] = arr;
let { p1, p2: q2, ...others } = obj;
x += 1;
debugger;
