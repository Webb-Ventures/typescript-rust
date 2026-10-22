// Names a signature cannot see: a return type reads what the parameters read (not the
// body's variables or types), `typeof arguments` needs a function other than an arrow, and
// a default export is no name in its module.
function bar(): typeof b {
    var b = 1;
    return undefined!;
}
function p(a: number): typeof a { return a; }
function q(): I { interface I {} return {}; }
function r(): typeof g { function g() {} return g; }
interface Sig {
    method(args: typeof arguments): void;
}
function g(a: typeof arguments) {}
type T = (a: typeof arguments) => void;
function h() { type U = typeof arguments; }
export {};
// @filename: d.ts
export default function () {
    return true;
}
export type X = typeof default;
