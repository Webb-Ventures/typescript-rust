// @target: es2017
// @module: esnext
declare let it: any, o1: any, k: string;
declare function f(): any;
async function g() {
    for await (const x of f()) { }
    for await (const x of o1.items) { }
    lbl: for await (const x of it) { continue lbl; }
    for await (const x of it) f();
    for await (const { a, ...r } of it) { }
}
const s1 = {
    a: 1,
    ...o1,
    b: 2,
};
const s2 = { ...o1, get x() { return 1; }, m() { } };
const { a, ...r1 } = f();
let { b, ...r2 } = o1.p;
const { [k]: c, ...r3 } = o1;
const { d: { e, ...r4 }, ...r5 } = o1;
const [{ ...r6 }] = o1;
const fa = ({ a, ...r }: any) => r;
function fb(x: number, { a, ...r }: any = {}) { return r; }
let q1: any, q2: any;
({ q1, ...q2 } = f());
const v = ({ q1, ...q2 } = o1);
for ({ q1, ...q2 } of [o1]) { }
try { } catch ({ a, ...r }) { }
const { "s-t": st, 1: one, ...r7 } = o1;
