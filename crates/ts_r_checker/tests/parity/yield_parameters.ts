function* g1(a = yield) { }
function* g2() {
    function* h(b = yield) { }
    function k(c = yield 1) { }
    const f = (d = yield 1) => d;
    const o = { *m(e = yield) { } };
    class C { *m(f = yield) { } constructor(z = yield 2) { } }
}
function* g3(a = (yield 1, 2)) { }
function* g4({ x = yield 1 }) { }
function* g5(a = function* () { yield 1; }) { }
