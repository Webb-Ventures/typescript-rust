// Parameters cannot see the body's locals, only its hoisted functions and namespaces (tsc
// reports TS2373 for those). `x`, `y`, `w`, `K`, `E`, `I` and `T` read the outer scope.
const x = 1;
function f(a = x) { var x = "s"; return a; }
function g(b = y) { let y = 2; return b; }
function h(c = k()) { function k() { return 1; } return c; }
function t(d: T) { type T = string; return d; }
function h1(c = new K()) { class K {} return c; }
function h2(c = E.A) { enum E { A } return c; }
function h3(c = N.v) { namespace N { export const v = 1; } return c; }
function h4(c: I) { interface I {} return c; }
function h7(c = w) { var w = 1; return c; }
function h8<U>(c: U, d = c) { return d; }
function n1(a = function named() { return x; }) { var x = 1; return a; }
function n2(a = () => { var x = 1; return () => x; }) { var x = 1; return a; }
