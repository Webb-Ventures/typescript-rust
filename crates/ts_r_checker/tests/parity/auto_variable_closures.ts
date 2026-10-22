// Auto-typed variables read from closures: undeterminable unless the closure is
// created after every assignment and `undefined` cannot reach it.
function a1() { var y; function f() { y; } }
function a2() { let y; function f() { y; } }
function a3() { let y; const f = () => y; }
function a4() { for (let x; false;) { () => x; } }
function a5() { let y; y = 1; const f = () => y; }
function a6() { let y; const f = () => y; y = 1; }
function a7() { var y; const f = () => y; }
function a8() { let y; { const f = () => y; } }
function a9() { for (var x; false;) { () => x; } }
function b1(c: boolean) { let y; if (c) y = 1; const f = () => y; }
function b2(c: boolean) { let y; if (c) y = 1; else y = "a"; const f = () => y; }
function b3() { let y; y = 1; function f() { return y; } }
export {};
