function f1(a = b) { var b; }
function f2(a = () => c) { let c = 1; }
function f3(a = g()) { function g() { return 1; } }
function f4(a = C) { class C { } }
function f5(a = x) { }
var x = 1;
function f6(a = { m() { var q; return q; } }) { }
const f7 = (a = d) => { var d; };
class K { m(a = e) { var e; } }
function f8(a = typeof h) { var h = 1; }
function f9(a: typeof i = 1) { var i = 1; }
function f10(a = j) { var j; { var j2 = j; } }
function f11(a = [k]) { enum k { A } }
function f12({ p = l } = {}) { var l = 1; }
function f13(a = arguments) { }
function f14(a = m) { namespace m { } }
function f15(a = n, n = 1) { }
function h1(p: string = z) { var zz: string; }
function h2(p: string = q) { var q: string; }
