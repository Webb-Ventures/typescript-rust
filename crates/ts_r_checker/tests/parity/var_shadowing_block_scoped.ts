function f1() { { let a; { var a; } } }
function f2() { { let b; { var b = 1; } } }
function f3() { { const c = 1; { var c = 1; } } }
function f4() { { let d; { var [d] = [1]; } } }
function f5() { { let e; { var { e } = { e: 1 }; } } }
function f6() { { let g; function inner() { var g = 1; } } }
function f7() { { let h; { for (var h in {}) {} } } }
function f8() { { let i; { for (var i of []) {} } } }
function f9() { let j; { var j = 1; } }
function f10() { { class k {} { var k = 1; } } }
function f11() { { let l; } { var l = 1; } }
function f12() { { let m; { var { m: mm } = { m: 1 }; var mm2 = m; } } }
function f13() { { let n; { var [, n] = [1, 2]; } } }
function f14() { { let o; { var { p: o } = { p: 1 }; } } }
function f15(q: number) { { let q; { var q = 1; } } }
namespace NS { { let r; { var r = 1; } } }
{ let s; { var s; } }
{ let t; { for (var t; ;) {} } }
for (let u; ;) { var u; }
for (let w; ;) { var w = 1; }
export {};
