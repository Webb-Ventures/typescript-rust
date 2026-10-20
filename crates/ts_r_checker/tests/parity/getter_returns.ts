// TS2378 for a getter without any `return` whose end is reachable (an annotation does not
// matter), and TS2355 for an `unknown` return type with no `return` at all.
declare const c: boolean;
class C {
    get a() { }
    get b() { return; }
    get c() { if (c) return 1; }
    get d() { throw new Error(); }
    get e(): number { }
    get f(): any { }
    get g(): void { }
    get h(): undefined { }
    get i() { if (c) { return; } return 1; }
    get j() { while (true) {} }
    get k(): number | undefined { if (c) return 1; }
    get l(): unknown { }
    get [Symbol.iterator]() { }
    get m() { return undefined; }
    get o() { const f = () => 1; }
    set p(v: number) { }
}
abstract class E {
    abstract get a(): number;
}
const o = {
    get a() { },
    get b() { return 1; },
};
interface I { get x(): number }
function f1(): unknown { }
function f2(): unknown { if (c) return 1; }
function f3(): unknown | number { }
const f4 = (): unknown => { };
class D {
    m(): unknown { }
    get g(): unknown { if (c) return 1; }
}
async function f5(): Promise<unknown> { }
