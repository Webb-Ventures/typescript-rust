"use strict";
// @target: es2022
// Numeric, bigint and private member names (errors) and a missing member.
var Nums;
(function (Nums) {
    Nums[Nums[1] = 0] = 1;
    Nums[Nums[1.1] = 1] = 1.1;
    Nums[Nums["13e-1"] = 2] = "13e-1";
    Nums[Nums[61453] = 3] = 61453;
})(Nums || (Nums = {}));
var E;
(function (E) {
    E[E[0n] = 0] = 0n;
})(E || (E = {}));
var P;
(function (P) {
    P[P[] = 0] = ;
})(P || (P = {}));
var S;
(function (S) {
})(S || (S = {}));
var C;
(function (C) {
    C[C[1] = 2] = 1;
})(C || (C = {}));
