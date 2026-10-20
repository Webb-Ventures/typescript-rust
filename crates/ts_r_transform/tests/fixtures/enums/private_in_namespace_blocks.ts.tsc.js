// @target: es2022
// @module: esnext
// Enums that are not exported are local to their namespace block: two blocks declaring a
// private `E` have two enums, and `N.a` inlines the first block's `E.A`.
export var N;
(function (N) {
    N.a = 1 /* E.A */;
})(N || (N = {}));
(function (N) {
    N.b = 2 /* E.A */;
})(N || (N = {}));
export var M;
(function (M) {
    M.a = E.A;
    let E;
    (function (E) {
        E[E["A"] = 1] = "A";
    })(E || (E = {}));
})(M || (M = {}));
(function (M) {
    let E;
    (function (E) {
        E[E["A"] = 2] = "A";
    })(E || (E = {}));
    M.b = E.A;
})(M || (M = {}));
