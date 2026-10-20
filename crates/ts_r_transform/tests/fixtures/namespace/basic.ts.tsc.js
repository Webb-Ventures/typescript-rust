// @target: es2022
var N;
(function (N) {
    var _a;
    N.a = 1, N.b2 = 2;
    function f() { return N.a + b; }
    N.f = f;
    const b = N.a;
    class C {
        m() { return N.a; }
    }
    N.C = C;
    let M;
    (function (M) {
        M.c = b;
    })(M = N.M || (N.M = {}));
    let Inner;
    (function (Inner) {
        Inner.d = N.a;
    })(Inner || (Inner = {}));
    let E;
    (function (E) {
        E[E["X"] = 0] = "X";
    })(E = N.E || (N.E = {}));
    _a = { p: 1, q: [2] }, N.p = _a.p, N.r = _a.q[0];
    N.Al = M.c;
    N.a;
    function g() { return N.unset; }
})(N || (N = {}));
(function (N) {
    N.late = N.a + N.f();
})(N || (N = {}));
var A;
(function (A) {
    var B;
    (function (B) {
        var C;
        (function (C) {
            C.deep = 1;
        })(C = B.C || (B.C = {}));
    })(B = A.B || (A.B = {}));
})(A || (A = {}));
function Merged() { }
(function (Merged) {
    Merged.extra = 1;
})(Merged || (Merged = {}));
class MC {
}
(function (MC) {
    MC.s = 2;
})(MC || (MC = {}));
export var Exp;
(function (Exp) {
    Exp.e = 1;
})(Exp || (Exp = {}));
