"use strict";
// @target: esnext
const base = 4;
const label = "x";
const neg = -base;
var E;
(function (E) {
    E[E["A"] = 4] = "A";
    E[E["B"] = 8] = "B";
    E["C"] = "x";
    E[E["D"] = -4] = "D";
    E[E["F"] = later] = "F";
})(E || (E = {}));
const later = 9;
