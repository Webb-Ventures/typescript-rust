// @target: es2015
// @module: esnext
// @useDefineForClassFields: false
// @noEmitHelpers: true
declare const o: any, k: any;
// Removed fields still evaluate computed keys that may have effects, with the
// captured keys of the other fields, in one statement.
class A {
    [o.k]: number;
    [o.p] = 1;
    [o.r]: number;
    static [o.s] = 2;
    static [o.t]: number;
    [k]: number;
    [true]: number;
    ["s"]: number;
}
class B {
    [o.k]: number;
    x = 1;
    constructor() {}
}
const C = class {
    static [o.k]: number;
};
