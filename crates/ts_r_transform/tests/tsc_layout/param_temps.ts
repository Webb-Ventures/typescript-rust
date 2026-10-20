// @target: es2015
// @module: esnext
declare let a: any;
function f1(x = 1, b = a.x?.b, c = 2, { d } = a) { return b; }
function f2(x, { [a.x?.b]: y }, z = 3) { }
const f3 = (b = a.x ?? 1) => b;
function f4(b = a.x?.b) { "use strict"; return b; }
class C { m(b = a.x?.c) { } constructor(public p = a.x?.d) { } }
