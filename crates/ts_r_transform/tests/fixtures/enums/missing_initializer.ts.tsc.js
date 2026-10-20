"use strict";
// @target: es2022
// TS1061: a member after a string member needs an initializer; tsc still emits it.
var E;
(function (E) {
    E["A"] = "x";
    E[E["B"] = void 0] = "B";
})(E || (E = {}));
console.log(E.B);
