namespace M {
    export interface I { a: number }
}
namespace N {
    export interface I { b: number }
}
declare let x: M.I;
declare let y: N.I;
if (x === y) {}
x = y;
let n: M.I = { a: 1 };
if (n === 1) {}
export {};
