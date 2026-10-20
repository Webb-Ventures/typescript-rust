// @target: es2017
// @module: esnext
// @noEmitHelpers: true
export {};
function f() { using b = null; }
{ using d = null; }
async function g() {
    { using c = null; }
    for await (const v of []) { }
    const h = () => { using q = null; };
    using r = null;
}
class A { constructor(public z: number) {} }
class B extends A {
    a = 1;
    constructor(public p: number) {
        super(p);
        using d = null;
        console.log(d);
    }
}
class C {
    a = () => 1;
    constructor(public p: number) {
        "use strict";
        using d = null;
    }
}
class D {
    b = 2;
    constructor() {
        using d = null;
    }
}
export {};
function f() {
    using a = null;
    { using b = null; }
    const g = () => { using c = null; };
    function h() { using d = null; }
    { using e = null; }
}
function k() { using z = null; }
{ using y = null; const q = () => { using x = null; }; }
