"use strict";
var _a, _b, _c;
// Removed fields still evaluate computed keys that may have effects, with the
// captured keys of the other fields, in one statement.
class A {
    constructor() {
        this[_a] = 1;
    }
}
o.k, _a = o.p, o.r, _b = o.s, o.t;
A[_b] = 2;
class B {
    constructor() {
        this.x = 1;
    }
}
o.k;
const C = (_c = class {
    },
    o.k,
    _c);
