function f2() {
    function f(x: T): T {
        interface T { }
        return undefined!;
    }
    function g<U extends Q>(a: (b: Q) => R): void {
        type Q = number;
        class R { }
    }
    function h(): Inner.Z {
        namespace Inner { export type Z = 1; }
        return 1;
    }
    const k = (p: K) => { type K = string; };
    class C {
        m(p: M): M { interface M { } return p; }
    }
}
interface Outer { }
function o(x: Outer) { interface Outer { a: 1 } }
function inner() { interface T2 { } let v: T2; function nest(q: T2) { } }
type X1<T> = T extends { a: infer U extends string, b: infer U extends number } ? U : never;
type X2<T> = T extends { a: infer U, b: infer V extends U } ? [U, V] : never;
type X3<T> = T extends { a: infer U, b: U } ? U : never;
type X4<T> = T extends [infer H, ...infer R extends H[]] ? R : never;
type X5<T> = T extends (infer U extends string ? U : never) ? 1 : 0;
type X6<T> = T extends { a: infer U } ? (T extends { b: U } ? U : 0) : 0;
namespace x { export let y = 123; export const enum_ok = 1; }
enum x { z = y }
enum e1 { A = 1 }
enum e1 { B = A }
namespace e1 { export const C = 1; }
enum e1 { D = C }
namespace e1 { const q = B; }
class K { }
namespace K { export const w = 1; }
namespace K { const v = w; }
