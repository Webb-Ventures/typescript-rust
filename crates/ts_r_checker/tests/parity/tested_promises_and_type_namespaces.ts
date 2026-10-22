// @strict: true
declare const p: Promise<number>;
declare const q: null | Promise<number>;
declare function pf(): Promise<boolean>;
declare const o: { p: Promise<string> };

async function f() {
    if (p) {}
    if (q) {}
    if (!!p) {}
    if (p) { p; }
    if (o.p && await o.p) {}
    if (pf()) { pf().then(); }
    const x = o.p ? 1 : 2;
    return x;
}

namespace N { export interface Foo { a: string } }
type T = N.Foo.a;
