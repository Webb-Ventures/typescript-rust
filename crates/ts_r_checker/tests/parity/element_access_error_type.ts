// An element access with no applicable index (TS7053, TS7015) is the error type: calling
// it with type arguments reports no TS2347. Any element of `never` is `never`, quietly.
declare const k: any;
declare const s: string;
declare const o: { a: number };
o[k]<string>(1);
declare const u: any;
u<string>(1);
declare const n: never;
n[k]<string>(1);
(o as any).x<string>(1);
o["zz"]<string>(1);
o[s]<string>(1);
const a1 = n["x"];
const a2 = n[0];
const a3 = n[s];
const a4: string = o[k];
const a5: string = o["zz"];
const a6 = o[k].foo.bar;
const a7 = [1][s];
a7<string>();
